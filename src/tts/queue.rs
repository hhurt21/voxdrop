//! Thread-based FIFO TTS playback queue with:
//! - Configurable max queue depth (drops oldest unplayed items to prioritize latest DMs)
//! - Instant playback interruption + queue clearing (`stop_and_clear`)
//! - Connection-failure backoff (10s suppression on repeated VOICEVOX errors; manual test bypasses backoff)
//! - Echo-prevention guard (`IS_TTS_PLAYING` + configurable post-playback tail delay)

use crate::config::DiscordTtsConfig;
use crate::sound;
use crate::tts::voicevox::VoicevoxClient;
use crate::tts::{set_tts_playing, TtsEngine};
use rodio::OutputStream;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TtsStatusUpdate {
    pub connected: bool,
    pub speaking: bool,
    pub queued: usize,
    pub last_error: Option<String>,
    pub is_manual_test: bool,
}

#[derive(Debug, Clone)]
pub struct TtsJob {
    pub text: String,
    pub is_manual_test: bool,
}

struct QueueInner {
    jobs: VecDeque<TtsJob>,
    config: DiscordTtsConfig,
    shutdown: bool,
}

/// Manages FIFO synthesis & playback on a dedicated background worker thread.
pub struct TtsQueueController {
    shared: Arc<(Mutex<QueueInner>, Condvar)>,
    stop_flag: Arc<AtomicBool>,
}

impl TtsQueueController {
    pub fn spawn<F>(initial_cfg: DiscordTtsConfig, on_status: F) -> Self
    where
        F: Fn(TtsStatusUpdate) + Send + Sync + 'static,
    {
        let shared = Arc::new((
            Mutex::new(QueueInner {
                jobs: VecDeque::new(),
                config: initial_cfg,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let shared_bg = shared.clone();
        let stop_bg = stop_flag.clone();
        let status_cb = Arc::new(on_status);

        thread::spawn(move || {
            let engine = VoicevoxClient::new();
            let mut audio_out: Option<(OutputStream, rodio::OutputStreamHandle)> = None;
            let mut consecutive_failures: u32 = 0;
            let mut backoff_until: Option<Instant> = None;

            loop {
                let (job, cfg, remaining_after_pop) = {
                    let (lock, cvar) = &*shared_bg;
                    let mut guard = match lock.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    while guard.jobs.is_empty() && !guard.shutdown {
                        guard = match cvar.wait(guard) {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                    if guard.shutdown {
                        break;
                    }
                    let job = guard.jobs.pop_front().unwrap();
                    let cfg = guard.config.clone();
                    let rem = guard.jobs.len();
                    (job, cfg, rem)
                };

                // Clear any stale stop signal from a previous cancellation before starting this job
                stop_bg.store(false, Ordering::SeqCst);

                // Apply 10s backoff for automatic DM jobs after consecutive connection failures
                if !job.is_manual_test {
                    if let Some(until) = backoff_until {
                        if Instant::now() < until {
                            continue;
                        }
                    }
                } else {
                    consecutive_failures = 0;
                    backoff_until = None;
                }

                // Synthesize via VOICEVOX Engine (auto-launch headlessly if enabled and not yet running)
                let wav_bytes = match engine.synthesize(&job.text, &cfg.voicevox) {
                    Ok(bytes) => {
                        consecutive_failures = 0;
                        backoff_until = None;
                        bytes
                    }
                    Err(first_err) => {
                        let retried = if cfg.voicevox.auto_launch {
                            status_cb(TtsStatusUpdate {
                                connected: false,
                                speaking: false,
                                queued: remaining_after_pop + 1,
                                last_error: Some("VOICEVOX をバックグラウンド起動中...".into()),
                                is_manual_test: job.is_manual_test,
                            });
                            if crate::tts::launcher::ensure_running(
                                &cfg.voicevox.base_url,
                                &cfg.voicevox.exe_path,
                            )
                            .is_ok()
                            {
                                engine.synthesize(&job.text, &cfg.voicevox)
                            } else {
                                Err(first_err)
                            }
                        } else {
                            Err(first_err)
                        };

                        match retried {
                            Ok(bytes) => {
                                consecutive_failures = 0;
                                backoff_until = None;
                                bytes
                            }
                            Err(err) => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                if consecutive_failures >= 2 {
                                    backoff_until = Some(Instant::now() + Duration::from_secs(10));
                                }
                                eprintln!("[voxdrop tts warning] {}", err);
                                status_cb(TtsStatusUpdate {
                                    connected: false,
                                    speaking: false,
                                    queued: remaining_after_pop,
                                    last_error: Some(err),
                                    is_manual_test: job.is_manual_test,
                                });
                                continue;
                            }
                        }
                    }
                };

                if stop_bg.load(Ordering::SeqCst) {
                    continue;
                }

                // Lazily initialize rodio default output stream on worker thread
                if audio_out.is_none() {
                    match OutputStream::try_default() {
                        Ok(pair) => audio_out = Some(pair),
                        Err(err) => {
                            let msg = format!("オーディオ出力デバイスの初期化に失敗しました: {}", err);
                            eprintln!("[voxdrop tts audio error] {}", msg);
                            status_cb(TtsStatusUpdate {
                                connected: true,
                                speaking: false,
                                queued: remaining_after_pop,
                                last_error: Some(msg),
                                is_manual_test: job.is_manual_test,
                            });
                            continue;
                        }
                    }
                }

                status_cb(TtsStatusUpdate {
                    connected: true,
                    speaking: true,
                    queued: remaining_after_pop,
                    last_error: None,
                    is_manual_test: job.is_manual_test,
                });

                if cfg.pause_stt_while_speaking {
                    set_tts_playing(true);
                }

                let mut playback_failed = false;
                if let Some((_, ref handle)) = audio_out {
                    if let Err(err) = sound::play_wav_bytes_blocking(handle, wav_bytes, 1.0, &stop_bg)
                    {
                        eprintln!("[voxdrop tts playback warning] {}", err);
                        playback_failed = true;
                    }
                }
                if playback_failed {
                    // Re-open default audio device on next job in case output device switched
                    audio_out = None;
                }

                // Wait echo-guard tail window when no next job is immediately waiting (or stopped)
                let has_more = shared_bg
                    .0
                    .lock()
                    .map(|g| !g.jobs.is_empty() && !g.shutdown)
                    .unwrap_or(false);

                if cfg.pause_stt_while_speaking && (!has_more || stop_bg.load(Ordering::SeqCst)) {
                    let tail_ms = cfg.echo_guard_tail_ms.min(2000);
                    if tail_ms > 0 {
                        let deadline = Instant::now() + Duration::from_millis(tail_ms);
                        while Instant::now() < deadline {
                            thread::sleep(Duration::from_millis(25));
                        }
                    }
                    set_tts_playing(false);
                } else if !cfg.pause_stt_while_speaking {
                    set_tts_playing(false);
                }

                let queued_now = shared_bg.0.lock().map(|g| g.jobs.len()).unwrap_or(0);
                status_cb(TtsStatusUpdate {
                    connected: true,
                    speaking: false,
                    queued: queued_now,
                    last_error: None,
                    is_manual_test: false,
                });
            }

            set_tts_playing(false);
        });

        Self { shared, stop_flag }
    }

    pub fn update_config(&self, cfg: DiscordTtsConfig) {
        if let Ok(mut guard) = self.shared.0.lock() {
            let max_q = cfg.max_queue_size.clamp(1, 50);
            while guard.jobs.len() > max_q {
                guard.jobs.pop_front();
            }
            guard.config = cfg;
        }
    }

    /// Enqueues a speech job; drops oldest queued items when `max_queue_size` is exceeded.
    pub fn enqueue(&self, text: String, is_manual_test: bool) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let (lock, cvar) = &*self.shared;
        if let Ok(mut guard) = lock.lock() {
            if guard.jobs.is_empty() {
                self.stop_flag.store(false, Ordering::SeqCst);
            }
            let max_q = guard.config.max_queue_size.clamp(1, 50);
            while guard.jobs.len() >= max_q {
                guard.jobs.pop_front();
            }
            guard.jobs.push_back(TtsJob {
                text: trimmed.to_string(),
                is_manual_test,
            });
            cvar.notify_one();
        }
    }

    /// Immediately interrupts current playback and clears all pending queued jobs.
    pub fn stop_and_clear(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.shared.0.lock() {
            guard.jobs.clear();
        }
    }

    pub fn shutdown(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let (lock, cvar) = &*self.shared;
        if let Ok(mut guard) = lock.lock() {
            guard.jobs.clear();
            guard.shutdown = true;
            cvar.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_bounded_fifo<T>(queue: &mut VecDeque<T>, item: T, max_size: usize) {
        let cap = max_size.max(1);
        while queue.len() >= cap {
            queue.pop_front();
        }
        queue.push_back(item);
    }

    #[test]
    fn fifo_queue_drops_oldest_when_exceeding_max_size() {
        let mut q = VecDeque::new();
        for i in 1..=7 {
            push_bounded_fifo(&mut q, i, 5);
        }
        let items: Vec<i32> = q.into_iter().collect();
        assert_eq!(items, vec![3, 4, 5, 6, 7]);
    }
}
