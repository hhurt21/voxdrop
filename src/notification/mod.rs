pub mod discord_filter;
pub mod discord_uia_watcher;
pub mod toast_watcher;
pub mod wpn_watcher;

use crate::config::DiscordTtsConfig;
use discord_filter::{
    filter_and_format_toast, sanitize_message_for_tts, NotificationDeduplicator,
};
use discord_uia_watcher::{DiscordUiaWatcher, UiaDmEvent};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 未読バッジを見つけてから、本文つきの Windows 通知を待つ時間（ms）。
const BADGE_GRACE_MS: u64 = 2500;
use toast_watcher::{RawToastNotification, ToastWatcher};
use wpn_watcher::WpnWatcher;

#[derive(Debug, Clone)]
pub enum NotificationEvent {
    SpeakText {
        sender: String,
        body: String,
        speech_text: String,
    },
    DebugToast(String),
}

/// Manages lifecycle of:
/// 1. Windows UIA Toast Watcher (live popups),
/// 2. Windows Push Notification (`wpndatabase.db`) Watcher (works during Do Not Disturb / minimized / hidden),
/// 3. Discord UIA DM Watcher (works even when Windows notifications for Discord are OFF).
/// All three share one deduplicator.
pub struct NotificationController {
    watcher: ToastWatcher,
    wpn_watcher: WpnWatcher,
    uia_watcher: DiscordUiaWatcher,
    config: Arc<Mutex<DiscordTtsConfig>>,
}

impl NotificationController {
    pub fn spawn<F>(initial_cfg: DiscordTtsConfig, on_event: F) -> Self
    where
        F: Fn(NotificationEvent) + Send + Sync + 'static,
    {
        let toast_enabled = initial_cfg.enabled || initial_cfg.debug_log_toasts;
        let wpn_enabled = initial_cfg.enabled;
        let uia_enabled = initial_cfg.enabled && initial_cfg.read_without_windows_notifications;
        let uia_debug = initial_cfg.debug_log_toasts;
        let config = Arc::new(Mutex::new(initial_cfg));
        let dedup = Arc::new(Mutex::new(NotificationDeduplicator::new(24)));

        let cfg_bg = config.clone();
        let dedup_bg = dedup.clone();
        let cb = Arc::new(on_event);
        let cb_toast = cb.clone();

        let watcher = ToastWatcher::spawn(toast_enabled, move |raw: RawToastNotification| {
            let cfg = match cfg_bg.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };
            if cfg.debug_log_toasts {
                let debug_line = format!(
                    "[Toast] win='{}' cls='{}' id='{}' hint='{}' texts={:?}",
                    raw.window_name, raw.class_name, raw.automation_id, raw.app_name_hint, raw.texts
                );
                eprintln!("{}", debug_line);
                cb_toast(NotificationEvent::DebugToast(debug_line));
            }
            if !cfg.enabled {
                return;
            }
            let Ok(mut dedup_guard) = dedup_bg.lock() else {
                return;
            };
            if let Some(parsed) = filter_and_format_toast(&raw, &cfg, &mut dedup_guard) {
                cb_toast(NotificationEvent::SpeakText {
                    sender: parsed.sender,
                    body: parsed.sanitized_body,
                    speech_text: parsed.speech_text,
                });
            }
        });

        let cfg_wpn = config.clone();
        let dedup_wpn = dedup.clone();
        let cb_wpn = cb.clone();
        let wpn_watcher = WpnWatcher::spawn(wpn_enabled, move |raw: RawToastNotification| {
            let cfg = match cfg_wpn.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };
            if cfg.debug_log_toasts {
                let debug_line = format!(
                    "[WPN] hint='{}' id='{}' texts={:?}",
                    raw.app_name_hint, raw.automation_id, raw.texts
                );
                eprintln!("{}", debug_line);
                cb_wpn(NotificationEvent::DebugToast(debug_line));
            }
            if !cfg.enabled {
                return;
            }
            let Ok(mut dedup_guard) = dedup_wpn.lock() else {
                return;
            };
            if let Some(parsed) = filter_and_format_toast(&raw, &cfg, &mut dedup_guard) {
                cb_wpn(NotificationEvent::SpeakText {
                    sender: parsed.sender,
                    body: parsed.sanitized_body,
                    speech_text: parsed.speech_text,
                });
            }
        });

        let cfg_uia = config.clone();
        let dedup_uia = dedup.clone();
        let cb_uia = cb;
        let uia_watcher = DiscordUiaWatcher::spawn(uia_enabled, uia_debug, move |ev: UiaDmEvent| {
            let cfg = match cfg_uia.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };

            if let UiaDmEvent::Debug(line) = ev {
                if cfg.debug_log_toasts {
                    eprintln!("{}", line);
                    cb_uia(NotificationEvent::DebugToast(line));
                }
                return;
            }

            if !cfg.enabled || !cfg.read_without_windows_notifications {
                return;
            }

            match ev {
                UiaDmEvent::OpenDmMessage { sender, body } => {
                    let sender = discord_filter::strip_bidi_controls(&sender).trim().to_string();
                    if !discord_filter::sender_permitted(&sender, &cfg) {
                        return;
                    }
                    let Ok(mut dedup_guard) = dedup_uia.lock() else {
                        return;
                    };
                    if dedup_guard.is_duplicate_and_record(
                        &sender,
                        &body,
                        cfg.dedup_window_ms.max(4000),
                        Instant::now(),
                    ) {
                        return;
                    }
                    let sanitized =
                        sanitize_message_for_tts(&body, &cfg.url_replacement, cfg.max_chars);
                    if sanitized.is_empty() {
                        return;
                    }
                    if cfg.debug_log_toasts {
                        let line = format!("[UIA OpenDM] {}: {}", sender, sanitized);
                        eprintln!("{}", line);
                        cb_uia(NotificationEvent::DebugToast(line));
                    }
                    let speech_text = discord_filter::format_speech_text(&sender, &sanitized, &cfg);
                    cb_uia(NotificationEvent::SpeakText {
                        sender,
                        body: sanitized,
                        speech_text,
                    });
                }
                UiaDmEvent::UnreadBadge { sender, count } => {
                    let sender = discord_filter::strip_bidi_controls(&sender).trim().to_string();
                    if !discord_filter::sender_permitted(&sender, &cfg) {
                        return;
                    }
                    // バッジは本文を持たない。Windows 通知（本文つき）は少し遅れて届くことがあるので、
                    // 待ってから「その相手の本文をもう読んだか」を見て、読んでいなければ件数だけ伝える。
                    let dedup_later = dedup_uia.clone();
                    let cb_later = cb_uia.clone();
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(BADGE_GRACE_MS));
                        let Ok(mut dedup_guard) = dedup_later.lock() else {
                            return;
                        };
                        let now = Instant::now();
                        if dedup_guard.seen_sender_recently(&sender, BADGE_GRACE_MS + 5000, now) {
                            return;
                        }
                        let badge_key = format!("__badge_{}", count);
                        if dedup_guard.is_duplicate_and_record(
                            &sender,
                            &badge_key,
                            cfg.dedup_window_ms.max(5000),
                            now,
                        ) {
                            return;
                        }
                        drop(dedup_guard);
                        let body = if count <= 1 {
                            "新しいDMが届きました".to_string()
                        } else {
                            format!("新しいDMが{}件届きました", count)
                        };
                        if cfg.debug_log_toasts {
                            let line = format!(
                                "[UIA UnreadBadge] {}: {}（本文つきの Windows 通知が来なかったため件数のみ）",
                                sender, body
                            );
                            eprintln!("{}", line);
                            cb_later(NotificationEvent::DebugToast(line));
                        }
                        let speech_text = if cfg.read_sender_name {
                            format!("{}さんから。{}", sender, body)
                        } else {
                            body.clone()
                        };
                        cb_later(NotificationEvent::SpeakText {
                            sender,
                            body,
                            speech_text,
                        });
                    });
                }
                UiaDmEvent::Debug(_) => {}
            }
        });

        Self {
            watcher,
            wpn_watcher,
            uia_watcher,
            config,
        }
    }

    pub fn update_config(&self, cfg: DiscordTtsConfig) {
        self.watcher.set_enabled(cfg.enabled || cfg.debug_log_toasts);
        self.wpn_watcher.set_enabled(cfg.enabled);
        self.uia_watcher
            .set_enabled(cfg.enabled && cfg.read_without_windows_notifications);
        self.uia_watcher.set_debug_enabled(cfg.debug_log_toasts);
        if let Ok(mut guard) = self.config.lock() {
            *guard = cfg;
        }
    }

    pub fn shutdown(&self) {
        self.watcher.shutdown();
        self.wpn_watcher.shutdown();
        self.uia_watcher.shutdown();
    }
}
