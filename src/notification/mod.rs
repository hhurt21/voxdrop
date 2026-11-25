pub mod discord_filter;
pub mod toast_watcher;

use crate::config::DiscordTtsConfig;
use discord_filter::{filter_and_format_toast, NotificationDeduplicator};
use std::sync::{Arc, Mutex};
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

/// Manages lifecycle of the Windows UIA toast watcher + Discord DM filter + deduplicator.
pub struct NotificationController {
    watcher: ToastWatcher,
    config: Arc<Mutex<DiscordTtsConfig>>,
}

impl NotificationController {
    pub fn spawn<F>(initial_cfg: DiscordTtsConfig, on_event: F) -> Self
    where
        F: Fn(NotificationEvent) + Send + Sync + 'static,
    {
        let enabled = initial_cfg.enabled || initial_cfg.debug_log_toasts;
        let config = Arc::new(Mutex::new(initial_cfg));
        let dedup = Arc::new(Mutex::new(NotificationDeduplicator::new(20)));
        let cfg_bg = config.clone();
        let dedup_bg = dedup.clone();
        let cb = Arc::new(on_event);

        let watcher = ToastWatcher::spawn(enabled, move |raw: RawToastNotification| {
            let cfg = match cfg_bg.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };
            if cfg.debug_log_toasts {
                let debug_line = format!(
                    "[Toast] win='{}' cls='{}' id='{}' texts={:?}",
                    raw.window_name, raw.class_name, raw.automation_id, raw.texts
                );
                eprintln!("{}", debug_line);
                cb(NotificationEvent::DebugToast(debug_line));
            }
            if !cfg.enabled {
                return;
            }
            let Ok(mut dedup_guard) = dedup_bg.lock() else {
                return;
            };
            if let Some(parsed) = filter_and_format_toast(&raw, &cfg, &mut dedup_guard) {
                cb(NotificationEvent::SpeakText {
                    sender: parsed.sender,
                    body: parsed.sanitized_body,
                    speech_text: parsed.speech_text,
                });
            }
        });

        Self { watcher, config }
    }

    pub fn update_config(&self, cfg: DiscordTtsConfig) {
        self.watcher.set_enabled(cfg.enabled || cfg.debug_log_toasts);
        if let Ok(mut guard) = self.config.lock() {
            *guard = cfg;
        }
    }

    pub fn shutdown(&self) {
        self.watcher.shutdown();
    }
}
