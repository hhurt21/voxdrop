pub mod discord_filter;
pub mod discord_uia_watcher;
pub mod toast_watcher;

use crate::config::DiscordTtsConfig;
use discord_filter::{
    filter_and_format_toast, sanitize_message_for_tts, NotificationDeduplicator,
};
use discord_uia_watcher::{DiscordUiaWatcher, UiaDmEvent};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use toast_watcher::{RawToastNotification, ToastWatcher};

#[derive(Debug, Clone)]
pub enum NotificationEvent {
    SpeakText {
        sender: String,
        body: String,
        speech_text: String,
    },
    DebugToast(String),
}

/// Manages lifecycle of BOTH the Windows UIA toast watcher AND the Discord UIA DM watcher
/// (which works even when Windows notifications are turned off), sharing one deduplicator.
pub struct NotificationController {
    watcher: ToastWatcher,
    uia_watcher: DiscordUiaWatcher,
    config: Arc<Mutex<DiscordTtsConfig>>,
}

impl NotificationController {
    pub fn spawn<F>(initial_cfg: DiscordTtsConfig, on_event: F) -> Self
    where
        F: Fn(NotificationEvent) + Send + Sync + 'static,
    {
        let toast_enabled = initial_cfg.enabled || initial_cfg.debug_log_toasts;
        let uia_enabled = initial_cfg.enabled && initial_cfg.read_without_windows_notifications;
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

        let cfg_uia = config.clone();
        let dedup_uia = dedup.clone();
        let cb_uia = cb;
        let uia_watcher = DiscordUiaWatcher::spawn(uia_enabled, move |ev: UiaDmEvent| {
            let cfg = match cfg_uia.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };
            if !cfg.enabled || !cfg.read_without_windows_notifications {
                return;
            }
            let Ok(mut dedup_guard) = dedup_uia.lock() else {
                return;
            };

            match ev {
                UiaDmEvent::OpenDmMessage { sender, body } => {
                    if !discord_filter::sender_permitted(&sender, &cfg) {
                        return;
                    }
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
                    let speech_text = discord_filter::format_speech_text(&sender, &sanitized, &cfg);
                    cb_uia(NotificationEvent::SpeakText {
                        sender,
                        body: sanitized,
                        speech_text,
                    });
                }
                UiaDmEvent::UnreadBadge { sender, count } => {
                    if !discord_filter::sender_permitted(&sender, &cfg) {
                        return;
                    }
                    // If a toast for this sender was just processed in the last 5 seconds, skip duplicate
                    if dedup_guard.seen_sender_recently(&sender, 5000, Instant::now()) {
                        return;
                    }
                    let badge_key = format!("__badge_{}", count);
                    if dedup_guard.is_duplicate_and_record(
                        &sender,
                        &badge_key,
                        cfg.dedup_window_ms.max(5000),
                        Instant::now(),
                    ) {
                        return;
                    }
                    let body = if count <= 1 {
                        "新しいDMが届きました".to_string()
                    } else {
                        format!("新しいDMが{}件届きました", count)
                    };
                    let speech_text = if cfg.read_sender_name {
                        format!("{}さんから。{}", sender, body)
                    } else {
                        body.clone()
                    };
                    cb_uia(NotificationEvent::SpeakText {
                        sender,
                        body,
                        speech_text,
                    });
                }
            }
        });

        Self {
            watcher,
            uia_watcher,
            config,
        }
    }

    pub fn update_config(&self, cfg: DiscordTtsConfig) {
        self.watcher.set_enabled(cfg.enabled || cfg.debug_log_toasts);
        self.uia_watcher
            .set_enabled(cfg.enabled && cfg.read_without_windows_notifications);
        if let Ok(mut guard) = self.config.lock() {
            *guard = cfg;
        }
    }

    pub fn shutdown(&self) {
        self.watcher.shutdown();
        self.uia_watcher.shutdown();
    }
}
