// VoxDrop エントリポイント:
// - tao + wry メインウィンドウ & 透過ミニHUDオーバーレイ & システムトレイ常駐
// - Windows 起動時自動起動（HKCU\...\Run、デフォルトOFF）
// - 純Rust RNNoise 高性能ノイズキャンセリング + 85Hzハイパス + VAD
// - sherpa-onnx-c-api.dll メモリ常駐による超高速オフライン日本語STT（単体EXE自己展開対応）
// - Discord 宛先指定（チャンネルID/DM/ユーザーID/Webhook）＆ 合言葉・フレンド名呼び出し複数候補確認フロー
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod automation;
mod autostart;
mod config;
mod hotkey;
mod ipc;
mod notification;
mod processor;
mod runtime;
mod sound;
mod stt;
mod tray;
mod tts;

use audio::capture::{list_input_devices, AudioController};
use audio::AudioEvent;
use automation::discord;
use automation::input::KeyAction;
use automation::uia;
use config::{AppConfig, DiscordTarget, DiscordTargetKind, GuardConfig, InputMode};
use hotkey::{HotkeyAction, HotkeyManager};
use ipc::{BackendEvent, ClientCommand, ScannedChannelDto, VoicevoxSpeakerDto};
use notification::{NotificationController, NotificationEvent};
use processor::commands::{
    parse_confirmation_reply, ConfirmationReply, TargetSwitchAction, VoiceCommand,
};
use sound::{SoundKind, SoundPlayer};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::Duration;
use stt::SttEngine;
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::WindowBuilderExtWindows,
    window::WindowBuilder,
};
use tray::{TrayAction, TrayManager, TrayState};
use tts::queue::{TtsQueueController, TtsStatusUpdate};
use tts::voicevox::{VoicevoxClient, VoicevoxSpeaker};
use wry::{WebView, WebViewBuilder};

const INDEX_HTML_TEMPLATE: &str = include_str!("../ui/index.html");
const STYLE_CSS: &str = include_str!("../ui/style.css");
const APP_JS: &str = include_str!("../ui/app.js");
const OVERLAY_HTML: &str = include_str!("../ui/overlay.html");

struct BootLoadingEngine;
impl SttEngine for BootLoadingEngine {
    fn name(&self) -> String {
        "モデル読み込み中...".into()
    }
    fn is_ready(&self) -> bool {
        false
    }
    fn transcribe(&self, _samples_16k: &[f32]) -> Result<String, String> {
        Err("STTモデルを初期化中です。数秒お待ちください。".into())
    }
}

enum UserEvent {
    Ipc(String),
    Tray(TrayAction),
    Hotkey(HotkeyAction),
    Audio(AudioEvent),
    SttEngineLoaded {
        engine: Arc<dyn SttEngine>,
        runtime_dir: Option<PathBuf>,
        error: Option<String>,
    },
    TranscriptReady(Result<String, String>),
    ActiveWindowChanged {
        process: String,
        target_active: bool,
    },
    DiscordUrlCopied {
        url: String,
        is_dm: bool,
        guild_id: String,
        channel_id: String,
    },
    DiscordUiScanDone(Result<uia::DiscordUiScan, String>),
    DiscordBotSyncDone(Result<discord::BotSyncData, String>),
    AutomationResult {
        ok: bool,
        action: String,
        message: String,
    },
    TtsStatus(TtsStatusUpdate),
    Notification(NotificationEvent),
    VoicevoxHealthDone(Result<String, String>),
    VoicevoxSpeakersDone(Result<Vec<VoicevoxSpeaker>, String>),
}

enum SttWorkerMsg {
    Transcribe {
        samples: Vec<f32>,
        radio_clarity: bool,
    },
    ReloadEngine(Arc<dyn SttEngine>),
    Shutdown,
}

#[derive(Debug, Clone)]
enum PendingConfirmation {
    RegisteredCandidates {
        query: String,
        candidates: Vec<DiscordTarget>,
        index: usize,
        deferred_text: String,
        deferred_cmd: Option<VoiceCommand>,
    },
    QuickSwitcher {
        query: String,
        index: usize,
        deferred_text: String,
        deferred_cmd: Option<VoiceCommand>,
    },
}

impl PendingConfirmation {
    fn prompt_event(&self) -> BackendEvent {
        match self {
            Self::RegisteredCandidates {
                query,
                candidates,
                index,
                ..
            } => {
                let current = &candidates[*index];
                let label = discord::target_display_label(Some(current));
                let has_next = candidates.len() > 1;
                BackendEvent::ConfirmationPrompt {
                    active: true,
                    has_next,
                    title: format!(
                        "確認 ({}/{}): 「{}」で合っていますか？",
                        index + 1,
                        candidates.len(),
                        label
                    ),
                    subtitle: if has_next {
                        format!(
                            "検索「{}」 — 「はい」で確定 ・ 「違う/次」で別の候補 ・ 「いいえ」でキャンセル",
                            query
                        )
                    } else {
                        format!(
                            "検索「{}」 — 「はい」で確定 ・ 「いいえ」でキャンセル（このまま通常の会話・操作も可能です）",
                            query
                        )
                    },
                }
            }
            Self::QuickSwitcher { query, index, .. } => BackendEvent::ConfirmationPrompt {
                active: true,
                has_next: true,
                title: format!(
                    "Discord検索「{}」({}番目) この人で合っていますか？",
                    query, index
                ),
                subtitle: "「はい」でDM入力欄へ ・ 「違う/次」で別の似た名前へ ・ 「いいえ」でキャンセル"
                    .into(),
            },
        }
    }
}

struct AppState {
    config: AppConfig,
    config_path: PathBuf,
    runtime_dir: Option<PathBuf>,
    shared_guard: Arc<RwLock<GuardConfig>>,
    clipboard_watch_flag: Arc<AtomicBool>,
    stt_ready: bool,
    stt_engine_name: String,
    listening: bool,
    sleeping: bool,
    recognizing: bool,
    ptt_active: bool,
    listening_before_ptt: bool,
    target_active: bool,
    active_process: String,
    overlay_visible: bool,
    draft_segments: Vec<String>,
    direct_history_lengths: Vec<usize>,
    last_direct_utterance: String,
    pending_confirm: Option<PendingConfirmation>,
}

impl AppState {
    fn new(mut config: AppConfig, config_path: PathBuf) -> Self {
        // Keep config auto_start synchronized with actual Windows Registry state
        config.auto_start = autostart::is_auto_start_enabled();
        let overlay_visible = config.overlay.enabled;
        let shared_guard = Arc::new(RwLock::new(config.guard.clone()));
        let clipboard_watch_flag =
            Arc::new(AtomicBool::new(config.discord_integration.clipboard_url_watch));
        let (target_active, active_process) = automation::check_target(&config.guard);
        Self {
            config,
            config_path,
            runtime_dir: None,
            shared_guard,
            clipboard_watch_flag,
            stt_ready: false,
            stt_engine_name: "モデル読み込み中...".into(),
            listening: false,
            sleeping: false,
            recognizing: false,
            ptt_active: false,
            listening_before_ptt: false,
            target_active,
            active_process,
            overlay_visible,
            draft_segments: Vec::new(),
            direct_history_lengths: Vec::new(),
            last_direct_utterance: String::new(),
            pending_confirm: None,
        }
    }

    fn sync_guard(&self) {
        if let Ok(mut g) = self.shared_guard.write() {
            *g = self.config.guard.clone();
        }
    }

    fn active_discord_target(&self) -> Option<DiscordTarget> {
        self.config.discord_targets.active_target().cloned()
    }

    fn append_draft(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.draft_segments.push(text.to_string());
    }

    fn draft_text(&self) -> String {
        processor::japanese_fix::join_utterances_smart(
            &self.draft_segments,
            self.config.filter.auto_punctuation,
            self.config.filter.auto_newline,
            self.config.filter.auto_space,
            self.config.filter.strip_punctuation,
        )
    }

    fn tray_state(&self) -> TrayState {
        if !self.listening {
            TrayState::Muted
        } else if self.sleeping {
            TrayState::Sleeping
        } else if self.recognizing {
            TrayState::Speaking
        } else {
            TrayState::Listening
        }
    }

    fn status_event(&self) -> BackendEvent {
        let active_target = self.config.discord_targets.active_target();
        BackendEvent::StatusChanged {
            listening: self.listening,
            sleeping: self.sleeping,
            recognizing: self.recognizing,
            target_active: self.target_active,
            active_process: self.active_process.clone(),
            input_mode: self.config.input_mode,
            overlay_visible: self.overlay_visible,
            stt_ready: self.stt_ready,
            stt_engine: self.stt_engine_name.clone(),
            rnnoise_enabled: self.config.noise_cancel.enabled,
            active_discord_target_id: active_target.map(|t| t.id.clone()),
            active_discord_target_label: discord::target_display_label(active_target),
        }
    }
}

fn build_index_html() -> String {
    INDEX_HTML_TEMPLATE
        .replace("/*__STYLE_CSS__*/", STYLE_CSS)
        .replace("/*__APP_JS__*/", APP_JS)
}

fn emit(webview: &WebView, event: &BackendEvent) {
    let script = event.to_eval_script();
    if let Err(err) = webview.evaluate_script(&script) {
        eprintln!("[ipc->js error] {}", err);
    }
}

fn emit_all(main_wv: &WebView, overlay_wv: &WebView, event: &BackendEvent) {
    let script = event.to_eval_script();
    let _ = main_wv.evaluate_script(&script);
    let _ = overlay_wv.evaluate_script(&script);
}

fn clear_confirmation_ui(main_wv: &WebView, overlay_wv: &WebView) {
    emit_all(
        main_wv,
        overlay_wv,
        &BackendEvent::ConfirmationPrompt {
            active: false,
            has_next: false,
            title: String::new(),
            subtitle: String::new(),
        },
    );
}

fn spawn_voicevox_autostart(
    base_url: String,
    exe_path: String,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        match tts::launcher::ensure_running(&base_url, &exe_path) {
            Ok(_) => {
                let client = VoicevoxClient::new();
                if let Ok(ver) = client.check_health(&base_url) {
                    let _ = proxy.send_event(UserEvent::VoicevoxHealthDone(Ok(ver)));
                }
                if let Ok(speakers) = client.get_speakers(&base_url) {
                    let _ = proxy.send_event(UserEvent::VoicevoxSpeakersDone(Ok(speakers)));
                }
            }
            Err(err) => {
                let _ = proxy.send_event(UserEvent::VoicevoxHealthDone(Err(err)));
            }
        }
    });
}

fn spawn_stt_worker(
    initial_engine: Arc<dyn SttEngine>,
    proxy: EventLoopProxy<UserEvent>,
) -> mpsc::Sender<SttWorkerMsg> {
    let (tx, rx) = mpsc::channel::<SttWorkerMsg>();
    thread::spawn(move || {
        let mut engine = initial_engine;
        while let Ok(msg) = rx.recv() {
            match msg {
                SttWorkerMsg::Transcribe {
                    samples,
                    radio_clarity,
                } => {
                    let result = engine.transcribe_enhanced(&samples, radio_clarity);
                    if proxy
                        .send_event(UserEvent::TranscriptReady(result))
                        .is_err()
                    {
                        break;
                    }
                }
                SttWorkerMsg::ReloadEngine(new_engine) => {
                    engine = new_engine;
                }
                SttWorkerMsg::Shutdown => break,
            }
        }
    });
    tx
}

fn spawn_stt_loader(
    stt_cfg: config::SttConfig,
    existing_runtime: Option<PathBuf>,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let (runtime_dir, extract_err) = match existing_runtime {
            Some(dir) => (Some(dir), None),
            None => match runtime::ensure_embedded_runtime_extracted() {
                Ok(dir) => (dir, None),
                Err(err) => (None, Some(err)),
            },
        };
        let engine = stt::create_engine(&stt_cfg, runtime_dir.as_deref());
        let _ = proxy.send_event(UserEvent::SttEngineLoaded {
            engine,
            runtime_dir,
            error: extract_err,
        });
    });
}

fn spawn_active_window_monitor(
    shared_guard: Arc<RwLock<GuardConfig>>,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let mut last_proc = String::new();
        let mut last_allowed = false;
        let mut initialized = false;
        loop {
            let guard = shared_guard
                .read()
                .map(|g| g.clone())
                .unwrap_or_default();
            let (allowed, proc_name) = automation::check_target(&guard);
            if !initialized || proc_name != last_proc || allowed != last_allowed {
                initialized = true;
                last_proc = proc_name.clone();
                last_allowed = allowed;
                if proxy
                    .send_event(UserEvent::ActiveWindowChanged {
                        process: proc_name,
                        target_active: allowed,
                    })
                    .is_err()
                {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
}

fn spawn_clipboard_url_watcher(
    enabled: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let mut last_seen = String::new();
        loop {
            thread::sleep(Duration::from_millis(750));
            if !enabled.load(Ordering::Relaxed) {
                continue;
            }
            if automation::input::was_clipboard_touched_by_self_recently(1500) {
                continue;
            }
            let Ok(mut cb) = arboard::Clipboard::new() else {
                continue;
            };
            let Ok(text) = cb.get_text() else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed == last_seen {
                continue;
            }
            last_seen = trimmed.to_string();
            if let Some((is_dm, guild_id, channel_id)) = discord::parse_discord_channel_url(trimmed)
            {
                if proxy
                    .send_event(UserEvent::DiscordUrlCopied {
                        url: trimmed.to_string(),
                        is_dm,
                        guild_id,
                        channel_id,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}

fn spawn_send_for_state(
    text: String,
    press_enter: bool,
    action: &'static str,
    state: &AppState,
    proxy: EventLoopProxy<UserEvent>,
) {
    let target = state.active_discord_target();
    let auto_nav = state.config.discord_targets.auto_navigate_on_send;
    let nav_delay = state.config.discord_targets.navigate_delay_ms;
    let guard = state.config.guard.clone();
    let integration = state.config.discord_integration.clone();
    thread::spawn(move || {
        let res = automation::send_to_discord_target(
            &text,
            press_enter,
            target.as_ref(),
            auto_nav,
            nav_delay,
            &guard,
            &integration,
        );
        let _ = proxy.send_event(match res {
            Ok(msg) => UserEvent::AutomationResult {
                ok: true,
                action: action.into(),
                message: msg,
            },
            Err(err) => UserEvent::AutomationResult {
                ok: false,
                action: action.into(),
                message: err,
            },
        });
    });
}

fn spawn_navigate_target(
    target: DiscordTarget,
    guard: GuardConfig,
    navigate_delay_ms: u64,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        match automation::navigate_and_focus_discord_target(
            &target,
            &guard,
            navigate_delay_ms,
            false,
        ) {
            Ok(msg) => {
                let _ = proxy.send_event(UserEvent::AutomationResult {
                    ok: true,
                    action: "navigate_target".into(),
                    message: msg,
                });
            }
            Err(err) => {
                let _ = proxy.send_event(UserEvent::AutomationResult {
                    ok: false,
                    action: "navigate_target".into(),
                    message: err,
                });
            }
        }
    });
}

fn spawn_quick_switcher_open(
    query: String,
    guard: GuardConfig,
    nav_delay: u64,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        if let Err(err) = discord::open_quick_switcher_search(&query, &guard, nav_delay) {
            let _ = proxy.send_event(UserEvent::AutomationResult {
                ok: false,
                action: "quick_switcher".into(),
                message: err,
            });
        }
    });
}

fn spawn_key_async(
    key_action: KeyAction,
    action: &'static str,
    guard: GuardConfig,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let res = automation::send_action_guarded(key_action, &guard);
        let _ = proxy.send_event(match res {
            Ok(msg) => UserEvent::AutomationResult {
                ok: true,
                action: action.into(),
                message: msg,
            },
            Err(err) => UserEvent::AutomationResult {
                ok: false,
                action: action.into(),
                message: err,
            },
        });
    });
}

fn set_listening_state(
    enabled: bool,
    play_sound: bool,
    state: &mut AppState,
    audio_ctrl: &AudioController,
    sound: &SoundPlayer,
    main_wv: &WebView,
    overlay_wv: &WebView,
    tray: &Option<TrayManager>,
) {
    state.listening = enabled;
    if enabled {
        state.sleeping = false;
    } else {
        state.recognizing = false;
    }
    audio_ctrl.set_listening(enabled);
    if play_sound {
        sound.play(
            if enabled {
                SoundKind::ToggleOn
            } else {
                SoundKind::ToggleOff
            },
            &state.config.sound,
        );
    }
    if let Some(ref t) = tray {
        t.update_state(state.tray_state(), state.overlay_visible);
    }
    emit_all(main_wv, overlay_wv, &state.status_event());
}

fn activate_registered_target(
    target: DiscordTarget,
    navigate: bool,
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    sound: &SoundPlayer,
    proxy: &EventLoopProxy<UserEvent>,
) {
    state.config.discord_targets.active_target_id = Some(target.id.clone());
    let _ = state.config.save_to(&state.config_path);
    let label = discord::target_display_label(Some(&target));
    sound.play(SoundKind::ToggleOn, &state.config.sound);
    if navigate
        && state.config.discord_targets.auto_navigate_on_switch
        && target.webhook_url.trim().is_empty()
    {
        spawn_navigate_target(
            target,
            state.config.guard.clone(),
            state.config.discord_targets.navigate_delay_ms,
            proxy.clone(),
        );
    }
    emit_all(main_wv, overlay_wv, &state.status_event());
    emit_all(
        main_wv,
        overlay_wv,
        &BackendEvent::CommandExecuted {
            action: "target_switch".into(),
            detail: format!("宛先を {} に確定しました（そのまま入力できます）", label),
        },
    );
}

fn handle_confirmation_reply(
    reply: ConfirmationReply,
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    tray: &Option<TrayManager>,
    sound: &SoundPlayer,
    proxy: &EventLoopProxy<UserEvent>,
) {
    let Some(pending) = state.pending_confirm.take() else {
        return;
    };

    match (pending, reply) {
        (
            PendingConfirmation::RegisteredCandidates {
                candidates,
                index,
                deferred_text,
                deferred_cmd,
                ..
            },
            ConfirmationReply::Yes,
        ) => {
            clear_confirmation_ui(main_wv, overlay_wv);
            let chosen = candidates[index].clone();
            let has_send = deferred_cmd == Some(VoiceCommand::Send);
            activate_registered_target(
                chosen,
                !has_send,
                state,
                main_wv,
                overlay_wv,
                sound,
                proxy,
            );
            execute_text_and_command(
                deferred_text,
                deferred_cmd,
                state,
                main_wv,
                overlay_wv,
                tray,
                sound,
                proxy,
            );
        }
        (
            PendingConfirmation::RegisteredCandidates {
                query,
                candidates,
                index,
                deferred_text,
                deferred_cmd,
            },
            ConfirmationReply::Next,
        ) => {
            if candidates.len() <= 1 {
                clear_confirmation_ui(main_wv, overlay_wv);
                sound.play(SoundKind::Cancel, &state.config.sound);
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::CommandExecuted {
                        action: "confirm_cancel".into(),
                        detail: "宛先の確認をキャンセルしました".into(),
                    },
                );
                return;
            }
            let next_idx = (index + 1) % candidates.len();
            let next_target = candidates[next_idx].clone();
            if next_target.webhook_url.trim().is_empty() {
                spawn_navigate_target(
                    next_target,
                    state.config.guard.clone(),
                    state.config.discord_targets.navigate_delay_ms,
                    proxy.clone(),
                );
            }
            let updated = PendingConfirmation::RegisteredCandidates {
                query,
                candidates,
                index: next_idx,
                deferred_text,
                deferred_cmd,
            };
            sound.play(SoundKind::ToggleOn, &state.config.sound);
            emit_all(main_wv, overlay_wv, &updated.prompt_event());
            state.pending_confirm = Some(updated);
        }
        (
            PendingConfirmation::RegisteredCandidates {
                query,
                candidates,
                index,
                deferred_text,
                deferred_cmd,
            },
            ConfirmationReply::Prev,
        ) => {
            if candidates.len() <= 1 {
                clear_confirmation_ui(main_wv, overlay_wv);
                sound.play(SoundKind::Cancel, &state.config.sound);
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::CommandExecuted {
                        action: "confirm_cancel".into(),
                        detail: "宛先の確認をキャンセルしました".into(),
                    },
                );
                return;
            }
            let prev_idx = if index == 0 {
                candidates.len() - 1
            } else {
                index - 1
            };
            let prev_target = candidates[prev_idx].clone();
            if prev_target.webhook_url.trim().is_empty() {
                spawn_navigate_target(
                    prev_target,
                    state.config.guard.clone(),
                    state.config.discord_targets.navigate_delay_ms,
                    proxy.clone(),
                );
            }
            let updated = PendingConfirmation::RegisteredCandidates {
                query,
                candidates,
                index: prev_idx,
                deferred_text,
                deferred_cmd,
            };
            sound.play(SoundKind::ToggleOn, &state.config.sound);
            emit_all(main_wv, overlay_wv, &updated.prompt_event());
            state.pending_confirm = Some(updated);
        }
        (PendingConfirmation::RegisteredCandidates { .. }, ConfirmationReply::Cancel) => {
            clear_confirmation_ui(main_wv, overlay_wv);
            sound.play(SoundKind::Cancel, &state.config.sound);
            emit_all(
                main_wv,
                overlay_wv,
                &BackendEvent::CommandExecuted {
                    action: "confirm_cancel".into(),
                    detail: "宛先の確認をキャンセルしました".into(),
                },
            );
        }
        (
            PendingConfirmation::QuickSwitcher {
                query,
                deferred_text,
                deferred_cmd,
                ..
            },
            ConfirmationReply::Yes,
        ) => {
            clear_confirmation_ui(main_wv, overlay_wv);
            // Clear fixed target ID so speech goes straight into the newly opened Discord DM!
            state.config.discord_targets.active_target_id = None;
            let _ = state.config.save_to(&state.config_path);
            sound.play(SoundKind::Send, &state.config.sound);
            emit_all(main_wv, overlay_wv, &state.status_event());

            let guard = state.config.guard.clone();
            let nav_delay = state.config.discord_targets.navigate_delay_ms;
            let p = proxy.clone();
            let text_to_send = deferred_text.clone();
            let press_enter = deferred_cmd == Some(VoiceCommand::Send);

            thread::spawn(move || {
                // Press Enter in Discord Quick Switcher -> opens DM & focuses message input box!
                if let Err(err) = automation::send_action_guarded(KeyAction::Enter, &guard) {
                    let _ = p.send_event(UserEvent::AutomationResult {
                        ok: false,
                        action: "dm_open".into(),
                        message: err,
                    });
                    return;
                }
                if !text_to_send.is_empty() || press_enter {
                    thread::sleep(Duration::from_millis(nav_delay.clamp(250, 1500)));
                    let res = automation::paste_text_guarded(&text_to_send, press_enter, &guard);
                    let _ = p.send_event(match res {
                        Ok(msg) => UserEvent::AutomationResult {
                            ok: true,
                            action: "send".into(),
                            message: format!("DM「{}」を開いて入力しました: {}", query, msg),
                        },
                        Err(err) => UserEvent::AutomationResult {
                            ok: false,
                            action: "send".into(),
                            message: err,
                        },
                    });
                } else {
                    let _ = p.send_event(UserEvent::AutomationResult {
                        ok: true,
                        action: "dm_open".into(),
                        message: format!("「{}」のDMを開きました（そのまま入力できます）", query),
                    });
                }
            });
        }
        (
            PendingConfirmation::QuickSwitcher {
                query,
                index,
                deferred_text,
                deferred_cmd,
            },
            ConfirmationReply::Next,
        ) => {
            spawn_key_async(
                KeyAction::DownArrow,
                "quick_switcher_next",
                state.config.guard.clone(),
                proxy.clone(),
            );
            let updated = PendingConfirmation::QuickSwitcher {
                query,
                index: index + 1,
                deferred_text,
                deferred_cmd,
            };
            sound.play(SoundKind::ToggleOn, &state.config.sound);
            emit_all(main_wv, overlay_wv, &updated.prompt_event());
            state.pending_confirm = Some(updated);
        }
        (
            PendingConfirmation::QuickSwitcher {
                query,
                index,
                deferred_text,
                deferred_cmd,
            },
            ConfirmationReply::Prev,
        ) => {
            spawn_key_async(
                KeyAction::UpArrow,
                "quick_switcher_prev",
                state.config.guard.clone(),
                proxy.clone(),
            );
            let updated = PendingConfirmation::QuickSwitcher {
                query,
                index: index.saturating_sub(1).max(1),
                deferred_text,
                deferred_cmd,
            };
            sound.play(SoundKind::ToggleOn, &state.config.sound);
            emit_all(main_wv, overlay_wv, &updated.prompt_event());
            state.pending_confirm = Some(updated);
        }
        (PendingConfirmation::QuickSwitcher { .. }, ConfirmationReply::Cancel) => {
            clear_confirmation_ui(main_wv, overlay_wv);
            sound.play(SoundKind::Cancel, &state.config.sound);
            spawn_key_async(
                KeyAction::Escape,
                "confirm_cancel",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
    }
}

fn execute_text_and_command(
    text: String,
    command: Option<VoiceCommand>,
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    tray: &Option<TrayManager>,
    sound: &SoundPlayer,
    proxy: &EventLoopProxy<UserEvent>,
) {
    match command {
        Some(VoiceCommand::Mute) => {
            state.sleeping = true;
            sound.play(SoundKind::Sleep, &state.config.sound);
            if let Some(ref t) = tray {
                t.update_state(state.tray_state(), state.overlay_visible);
            }
            emit_all(main_wv, overlay_wv, &state.status_event());
            emit_all(
                main_wv,
                overlay_wv,
                &BackendEvent::CommandExecuted {
                    action: VoiceCommand::Mute.as_str().into(),
                    detail: "音声スリープに移行（「認識再開」で復帰）".into(),
                },
            );
        }
        Some(VoiceCommand::Resume) => {
            emit_all(
                main_wv,
                overlay_wv,
                &BackendEvent::CommandExecuted {
                    action: VoiceCommand::Resume.as_str().into(),
                    detail: "認識待機中です".into(),
                },
            );
        }
        Some(VoiceCommand::ClearAll) => {
            sound.play(SoundKind::Cancel, &state.config.sound);
            match state.config.input_mode {
                InputMode::Draft => {
                    state.draft_segments.clear();
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::TranscriptUpdate {
                            text: String::new(),
                            draft: String::new(),
                            is_final: true,
                        },
                    );
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::CommandExecuted {
                            action: VoiceCommand::ClearAll.as_str().into(),
                            detail: "ドラフトを全消去しました".into(),
                        },
                    );
                }
                InputMode::Direct => {
                    state.direct_history_lengths.clear();
                    state.last_direct_utterance.clear();
                    spawn_key_async(
                        KeyAction::ClearAll,
                        VoiceCommand::ClearAll.as_str(),
                        state.config.guard.clone(),
                        proxy.clone(),
                    );
                }
            }
        }
        Some(VoiceCommand::Undo) => {
            sound.play(SoundKind::Cancel, &state.config.sound);
            match state.config.input_mode {
                InputMode::Draft => {
                    let removed = state.draft_segments.pop().unwrap_or_default();
                    let draft = state.draft_text();
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::TranscriptUpdate {
                            text: String::new(),
                            draft,
                            is_final: true,
                        },
                    );
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::CommandExecuted {
                            action: VoiceCommand::Undo.as_str().into(),
                            detail: if removed.is_empty() {
                                "取り消すフレーズがありません".into()
                            } else {
                                format!("「{}」を取り消しました", removed)
                            },
                        },
                    );
                }
                InputMode::Direct => {
                    state.last_direct_utterance.clear();
                    let key_action = match state.direct_history_lengths.pop() {
                        Some(n) if n > 0 => KeyAction::BackSpace(n),
                        _ => KeyAction::CtrlZ,
                    };
                    spawn_key_async(
                        key_action,
                        VoiceCommand::Undo.as_str(),
                        state.config.guard.clone(),
                        proxy.clone(),
                    );
                }
            }
        }
        Some(VoiceCommand::Newline) => match state.config.input_mode {
            InputMode::Draft => {
                if !text.is_empty() {
                    state.append_draft(&text);
                }
                state.append_draft("\n");
                let draft = state.draft_text();
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::TranscriptUpdate {
                        text,
                        draft,
                        is_final: true,
                    },
                );
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::CommandExecuted {
                        action: VoiceCommand::Newline.as_str().into(),
                        detail: "改行を挿入しました".into(),
                    },
                );
            }
            InputMode::Direct => {
                let guard = state.config.guard.clone();
                let paste_text = if text.is_empty() {
                    String::new()
                } else {
                    let joiner = processor::japanese_fix::compute_utterance_joiner(
                        &state.last_direct_utterance,
                        &text,
                        state.config.filter.auto_punctuation,
                        state.config.filter.auto_newline,
                        state.config.filter.auto_space,
                        state.config.filter.strip_punctuation,
                    );
                    format!("{}{}", joiner, text)
                };
                if !paste_text.is_empty() {
                    state.direct_history_lengths.push(paste_text.chars().count());
                }
                state.last_direct_utterance = "\n".into();
                let text_bg = paste_text;
                let p = proxy.clone();
                thread::spawn(move || {
                    if !text_bg.is_empty() {
                        if let Err(err) = automation::paste_text_guarded(&text_bg, false, &guard) {
                            let _ = p.send_event(UserEvent::AutomationResult {
                                ok: false,
                                action: VoiceCommand::Newline.as_str().into(),
                                message: err,
                            });
                            return;
                        }
                        thread::sleep(Duration::from_millis(30));
                    }
                    let res = automation::send_action_guarded(KeyAction::ShiftEnter, &guard);
                    let _ = p.send_event(match res {
                        Ok(msg) => UserEvent::AutomationResult {
                            ok: true,
                            action: VoiceCommand::Newline.as_str().into(),
                            message: msg,
                        },
                        Err(err) => UserEvent::AutomationResult {
                            ok: false,
                            action: VoiceCommand::Newline.as_str().into(),
                            message: err,
                        },
                    });
                });
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::TranscriptUpdate {
                        text,
                        draft: state.draft_text(),
                        is_final: true,
                    },
                );
            }
        },
        Some(VoiceCommand::Send) => match state.config.input_mode {
            InputMode::Draft => {
                if !text.is_empty() {
                    state.append_draft(&text);
                }
                let full_text = state.draft_text();
                state.draft_segments.clear();
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::TranscriptUpdate {
                        text: full_text.clone(),
                        draft: String::new(),
                        is_final: true,
                    },
                );
                if full_text.is_empty() {
                    emit(
                        main_wv,
                        &BackendEvent::Toast {
                            level: "info".into(),
                            message: "送信するテキストがありません".into(),
                        },
                    );
                } else {
                    spawn_send_for_state(
                        full_text,
                        true,
                        VoiceCommand::Send.as_str(),
                        state,
                        proxy.clone(),
                    );
                }
            }
            InputMode::Direct => {
                let paste_text = if text.is_empty() {
                    String::new()
                } else {
                    let joiner = processor::japanese_fix::compute_utterance_joiner(
                        &state.last_direct_utterance,
                        &text,
                        state.config.filter.auto_punctuation,
                        state.config.filter.auto_newline,
                        state.config.filter.auto_space,
                        state.config.filter.strip_punctuation,
                    );
                    format!("{}{}", joiner, text)
                };
                state.direct_history_lengths.clear();
                state.last_direct_utterance.clear();
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::TranscriptUpdate {
                        text: text.clone(),
                        draft: String::new(),
                        is_final: true,
                    },
                );
                spawn_send_for_state(
                    paste_text,
                    true,
                    VoiceCommand::Send.as_str(),
                    state,
                    proxy.clone(),
                );
            }
        },
        Some(VoiceCommand::DiscordMuteMic) => {
            spawn_key_async(
                KeyAction::DiscordMuteMic,
                "discord_mute_mic",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordDeafen) => {
            spawn_key_async(
                KeyAction::DiscordDeafen,
                "discord_deafen",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordNextChannel) => {
            spawn_key_async(
                KeyAction::DiscordNextChannel,
                "discord_next_channel",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordPrevChannel) => {
            spawn_key_async(
                KeyAction::DiscordPrevChannel,
                "discord_prev_channel",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordNextUnread) => {
            spawn_key_async(
                KeyAction::DiscordNextUnread,
                "discord_next_unread",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordNextServer) => {
            spawn_key_async(
                KeyAction::DiscordNextServer,
                "discord_next_server",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordPrevServer) => {
            spawn_key_async(
                KeyAction::DiscordPrevServer,
                "discord_prev_server",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordScrollUp) => {
            spawn_key_async(
                KeyAction::DiscordScrollUp,
                "discord_scroll_up",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordScrollDown) => {
            spawn_key_async(
                KeyAction::DiscordScrollDown,
                "discord_scroll_down",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordEditLast) => {
            spawn_key_async(
                KeyAction::DiscordEditLast,
                "discord_edit_last",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordMarkRead) => {
            spawn_key_async(
                KeyAction::DiscordMarkRead,
                "discord_mark_read",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordToggleMembers) => {
            spawn_key_async(
                KeyAction::DiscordToggleMembers,
                "discord_toggle_members",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        None => {
            if text.is_empty() {
                return;
            }
            match state.config.input_mode {
                InputMode::Draft => {
                    state.append_draft(&text);
                    let draft = state.draft_text();
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::TranscriptUpdate {
                            text,
                            draft,
                            is_final: true,
                        },
                    );
                }
                InputMode::Direct => {
                    let joiner = processor::japanese_fix::compute_utterance_joiner(
                        &state.last_direct_utterance,
                        &text,
                        state.config.filter.auto_punctuation,
                        state.config.filter.auto_newline,
                        state.config.filter.auto_space,
                        state.config.filter.strip_punctuation,
                    );
                    let paste_text = format!("{}{}", joiner, text);
                    state.last_direct_utterance = text.clone();
                    state.direct_history_lengths.push(paste_text.chars().count());
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::TranscriptUpdate {
                            text,
                            draft: String::new(),
                            is_final: true,
                        },
                    );
                    spawn_send_for_state(paste_text, false, "direct_paste", state, proxy.clone());
                }
            }
        }
    }
}

fn handle_transcript(
    raw: &str,
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    tray: &Option<TrayManager>,
    sound: &SoundPlayer,
    proxy: &EventLoopProxy<UserEvent>,
) {
    // 1. If awaiting friend confirmation ("この人で合っていますか？"), handle Yes / No / Next / Prev
    //    if the user spoke a short confirmation reply; otherwise let normal dictation & commands work!
    if state.pending_confirm.is_some() {
        if let Some(reply) = parse_confirmation_reply(raw) {
            handle_confirmation_reply(reply, state, main_wv, overlay_wv, tray, sound, proxy);
            return;
        }
    }

    let processed = processor::process_transcript(
        raw,
        &state.config.filter,
        &state.config.commands,
        &state.config.discord_targets,
    );

    if processed.target_switch.is_some() || processed.command == Some(VoiceCommand::ClearAll) {
        if state.pending_confirm.take().is_some() {
            clear_confirmation_ui(main_wv, overlay_wv);
        }
    }

    if state.sleeping {
        if processed.command == Some(VoiceCommand::Resume) {
            state.sleeping = false;
            sound.play(SoundKind::ToggleOn, &state.config.sound);
            if let Some(ref t) = tray {
                t.update_state(state.tray_state(), state.overlay_visible);
            }
            emit_all(main_wv, overlay_wv, &state.status_event());
            emit_all(
                main_wv,
                overlay_wv,
                &BackendEvent::CommandExecuted {
                    action: VoiceCommand::Resume.as_str().into(),
                    detail: "音声認識を再開しました".into(),
                },
            );
        }
        return;
    }

    // 2. Handle friend name / passphrase target switching
    if let Some(switch_action) = processed.target_switch {
        match switch_action {
            TargetSwitchAction::Reset => {
                state.config.discord_targets.active_target_id = None;
                state.last_direct_utterance.clear();
                let _ = state.config.save_to(&state.config_path);
                sound.play(SoundKind::ToggleOff, &state.config.sound);
                emit_all(main_wv, overlay_wv, &state.status_event());
                emit_all(
                    main_wv,
                    overlay_wv,
                    &BackendEvent::CommandExecuted {
                        action: "target_reset".into(),
                        detail: "宛先指定を解除しました（現在の画面へ送信）".into(),
                    },
                );
            }
            TargetSwitchAction::SwitchExact(target_id) => {
                if let Some(target) = state.config.discord_targets.find_by_id(&target_id).cloned() {
                    let has_send = processed.command == Some(VoiceCommand::Send);
                    activate_registered_target(
                        target,
                        !has_send,
                        state,
                        main_wv,
                        overlay_wv,
                        sound,
                        proxy,
                    );
                }
            }
            TargetSwitchAction::QueryFriend(query) => {
                let (candidates, is_single_exact) =
                    discord::find_matching_targets(&query, &state.config.discord_targets.targets);

                if is_single_exact && candidates.len() == 1 {
                    let has_send = processed.command == Some(VoiceCommand::Send);
                    activate_registered_target(
                        candidates[0].clone(),
                        !has_send,
                        state,
                        main_wv,
                        overlay_wv,
                        sound,
                        proxy,
                    );
                } else if !candidates.is_empty() {
                    // Multiple (or fuzzy) registered candidates -> show 1st candidate's DM & ask confirmation!
                    let first = candidates[0].clone();
                    if first.webhook_url.trim().is_empty() {
                        spawn_navigate_target(
                            first,
                            state.config.guard.clone(),
                            state.config.discord_targets.navigate_delay_ms,
                            proxy.clone(),
                        );
                    }
                    let pending = PendingConfirmation::RegisteredCandidates {
                        query,
                        candidates,
                        index: 0,
                        deferred_text: processed.text,
                        deferred_cmd: processed.command,
                    };
                    sound.play(SoundKind::ToggleOn, &state.config.sound);
                    emit_all(main_wv, overlay_wv, &pending.prompt_event());
                    state.pending_confirm = Some(pending);
                    return;
                } else if state.config.discord_targets.use_quick_switcher_for_unknown {
                    // Unregistered friend -> open Discord Quick Switcher (Ctrl+K) with @<name> and ask confirmation!
                    spawn_quick_switcher_open(
                        query.clone(),
                        state.config.guard.clone(),
                        state.config.discord_targets.navigate_delay_ms,
                        proxy.clone(),
                    );
                    let pending = PendingConfirmation::QuickSwitcher {
                        query,
                        index: 1,
                        deferred_text: processed.text,
                        deferred_cmd: processed.command,
                    };
                    sound.play(SoundKind::ToggleOn, &state.config.sound);
                    emit_all(main_wv, overlay_wv, &pending.prompt_event());
                    state.pending_confirm = Some(pending);
                    return;
                } else {
                    emit(
                        main_wv,
                        &BackendEvent::Toast {
                            level: "info".into(),
                            message: format!("「{}」に一致する宛先が見つかりませんでした", query),
                        },
                    );
                    return;
                }
            }
        }
    }

    execute_text_and_command(
        processed.text,
        processed.command,
        state,
        main_wv,
        overlay_wv,
        tray,
        sound,
        proxy,
    );
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let (boot_cfg, boot_cfg_path) = AppConfig::load_or_default();
    let mut state = AppState::new(boot_cfg, boot_cfg_path);

    let sound_player = SoundPlayer::new();
    // Start STT worker immediately with lightweight placeholder, then warm-load resident C API DLL in background!
    let stt_tx = spawn_stt_worker(Arc::new(BootLoadingEngine), proxy.clone());
    spawn_stt_loader(state.config.stt.clone(), None, proxy.clone());
    spawn_active_window_monitor(state.shared_guard.clone(), proxy.clone());
    spawn_clipboard_url_watcher(state.clipboard_watch_flag.clone(), proxy.clone());

    let audio_proxy = proxy.clone();
    let audio_ctrl = AudioController::spawn(
        state.config.audio_device.clone(),
        state.config.vad.clone(),
        state.config.noise_cancel.clone(),
        move |ev| {
            let _ = audio_proxy.send_event(UserEvent::Audio(ev));
        },
    );

    let tts_proxy = proxy.clone();
    let tts_ctrl = TtsQueueController::spawn(state.config.discord_tts.clone(), move |status| {
        let _ = tts_proxy.send_event(UserEvent::TtsStatus(status));
    });

    let notif_proxy = proxy.clone();
    let notif_ctrl = NotificationController::spawn(state.config.discord_tts.clone(), move |ev| {
        let _ = notif_proxy.send_event(UserEvent::Notification(ev));
    });

    if state.config.discord_tts.enabled && state.config.discord_tts.voicevox.auto_launch {
        spawn_voicevox_autostart(
            state.config.discord_tts.voicevox.base_url.clone(),
            state.config.discord_tts.voicevox.exe_path.clone(),
            proxy.clone(),
        );
    }

    let hk_proxy = proxy.clone();
    let (mut hotkey_mgr, initial_hk_warnings) =
        match HotkeyManager::new(&state.config.hotkeys, move |action| {
            let _ = hk_proxy.send_event(UserEvent::Hotkey(action));
        }) {
            Ok((mgr, warns)) => (Some(mgr), warns),
            Err(e) => {
                eprintln!("[hotkey warning] {}", e);
                (None, vec![e])
            }
        };

    // Main Settings Window
    let window = WindowBuilder::new()
        .with_title("VoxDrop — ハンズフリー音声チャット支援")
        .with_inner_size(LogicalSize::new(980.0, 740.0))
        .with_min_inner_size(LogicalSize::new(680.0, 500.0))
        .with_visible(!state.config.start_minimized)
        .build(&event_loop)
        .expect("メインウィンドウの作成に失敗しました");

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(build_index_html())
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(request.body().to_string()));
        })
        .build(&window)?;

    // Mini HUD Overlay Window (Frameless, Transparent, Always-on-Top, Click-through toggle)
    let overlay_window = WindowBuilder::new()
        .with_title("VoxDrop HUD")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top(true)
        .with_resizable(false)
        .with_skip_taskbar(true)
        .with_undecorated_shadow(false)
        .with_inner_size(LogicalSize::new(400.0, 168.0))
        .with_position(LogicalPosition::new(
            state.config.overlay.x as f64,
            state.config.overlay.y as f64,
        ))
        .with_visible(state.overlay_visible)
        .build(&event_loop)
        .expect("ミニHUDウィンドウの作成に失敗しました");
    let _ = overlay_window.set_ignore_cursor_events(state.config.overlay.click_through);

    let overlay_ipc_proxy = proxy.clone();
    let overlay_webview = WebViewBuilder::new()
        .with_transparent(true)
        .with_html(OVERLAY_HTML)
        .with_ipc_handler(move |request| {
            let _ = overlay_ipc_proxy.send_event(UserEvent::Ipc(request.body().to_string()));
        })
        .build(&overlay_window)?;

    let tray_proxy = proxy.clone();
    let tray = match TrayManager::new(move |action| {
        let _ = tray_proxy.send_event(UserEvent::Tray(action));
    }) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("[tray warning] {}", e);
            None
        }
    };

    if let Some(ref t) = tray {
        t.update_state(state.tray_state(), state.overlay_visible);
    }

    let main_window_id = window.id();
    let overlay_window_id = overlay_window.id();
    let mut pending_boot_warnings = initial_hk_warnings;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if window_id == main_window_id => {
                window.set_visible(false);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Moved(pos),
                ..
            } if window_id == overlay_window_id => {
                state.config.overlay.x = pos.x;
                state.config.overlay.y = pos.y;
            }
            Event::UserEvent(UserEvent::SttEngineLoaded {
                engine,
                runtime_dir,
                error,
            }) => {
                if runtime_dir.is_some() {
                    state.runtime_dir = runtime_dir;
                }
                state.stt_ready = engine.is_ready();
                state.stt_engine_name = engine.name();
                let _ = stt_tx.send(SttWorkerMsg::ReloadEngine(engine));
                emit_all(&webview, &overlay_webview, &state.status_event());
                if let Some(err) = error {
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message: err,
                        },
                    );
                } else if state.stt_ready {
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "ok".into(),
                            message: format!("STT準備完了: {}", state.stt_engine_name),
                        },
                    );
                }
            }
            Event::UserEvent(UserEvent::Audio(audio_ev)) => match audio_ev {
                AudioEvent::Level {
                    rms,
                    is_speaking,
                    vad_prob,
                    rnnoise_active,
                    noise_floor,
                    effective_threshold,
                } => {
                    emit_all(
                        &webview,
                        &overlay_webview,
                        &BackendEvent::AudioLevel {
                            rms,
                            is_speaking,
                            vad_prob,
                            rnnoise_active,
                            noise_floor,
                            effective_threshold,
                        },
                    );
                }
                AudioEvent::CalibrationDone {
                    noise_floor,
                    threshold,
                } => {
                    state.config.vad.threshold = (threshold * 1000.0).round() / 1000.0;
                    state.config.vad.auto_adjust = true;
                    let _ = state.config.save_to(&state.config_path);
                    emit(
                        &webview,
                        &BackendEvent::ConfigLoaded {
                            config: state.config.clone(),
                            config_path: state.config_path.display().to_string(),
                        },
                    );
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "ok".into(),
                            message: format!(
                                "ノイズ測定完了！環境ノイズ: {:.3} → 自動しきい値: {:.3}",
                                noise_floor, threshold
                            ),
                        },
                    );
                }
                AudioEvent::SpeechStart => {
                    state.recognizing = true;
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
                AudioEvent::SpeechCancelled => {
                    state.recognizing = false;
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
                AudioEvent::Utterance(samples) => {
                    let _ = stt_tx.send(SttWorkerMsg::Transcribe {
                        samples,
                        radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                    });
                }
                AudioEvent::Error(err) => {
                    state.listening = false;
                    state.recognizing = false;
                    state.ptt_active = false;
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::TranscriptReady(result)) => {
                state.recognizing = false;
                if let Some(ref t) = tray {
                    t.update_state(state.tray_state(), state.overlay_visible);
                }
                emit_all(&webview, &overlay_webview, &state.status_event());

                match result {
                    Ok(raw) => {
                        if !raw.trim().is_empty() {
                            handle_transcript(
                                &raw,
                                &mut state,
                                &webview,
                                &overlay_webview,
                                &tray,
                                &sound_player,
                                &proxy,
                            );
                        }
                    }
                    Err(err) => {
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "error".into(),
                                message: err,
                            },
                        );
                    }
                }
            }
            Event::UserEvent(UserEvent::ActiveWindowChanged {
                process,
                target_active,
            }) => {
                state.active_process = process;
                state.target_active = target_active;
                emit_all(&webview, &overlay_webview, &state.status_event());
            }
            Event::UserEvent(UserEvent::DiscordUrlCopied {
                url,
                is_dm,
                guild_id,
                channel_id,
            }) => {
                emit(
                    &webview,
                    &BackendEvent::DiscordUrlDetected {
                        url,
                        is_dm,
                        guild_id,
                        channel_id,
                    },
                );
            }
            Event::UserEvent(UserEvent::DiscordUiScanDone(res)) => match res {
                Ok(scan) => {
                    let channels_dto: Vec<ScannedChannelDto> = scan
                        .channels
                        .into_iter()
                        .map(|c| ScannedChannelDto {
                            server_name: c.server_name,
                            name: c.name,
                        })
                        .collect();
                    let msg = format!(
                        "スキャン完了: サーバー{}件 / チャンネル{}件 / フレンド{}人",
                        scan.servers.len(),
                        channels_dto.len(),
                        scan.friends.len()
                    );
                    emit(
                        &webview,
                        &BackendEvent::DiscordScanResult {
                            ok: true,
                            active_server: scan.active_server,
                            servers: scan.servers,
                            channels: channels_dto,
                            friends: scan.friends,
                            message: msg,
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::DiscordScanResult {
                            ok: false,
                            active_server: String::new(),
                            servers: Vec::new(),
                            channels: Vec::new(),
                            friends: Vec::new(),
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::DiscordBotSyncDone(res)) => match res {
                Ok(data) => {
                    let mut added_ch = 0usize;
                    let mut added_mem = 0usize;
                    let targets = &mut state.config.discord_targets.targets;

                    for ch in data.channels {
                        let exists = targets.iter().any(|t| {
                            t.kind == DiscordTargetKind::Channel && t.channel_id == ch.channel_id
                        });
                        if !exists {
                            targets.push(DiscordTarget {
                                id: format!("bot_ch_{}", ch.channel_id),
                                name: ch.name.clone(),
                                server_name: ch.server_name,
                                passphrases: vec![ch.name],
                                kind: DiscordTargetKind::Channel,
                                guild_id: ch.guild_id,
                                channel_id: ch.channel_id,
                                user_id: String::new(),
                                prepend_mention: false,
                                webhook_url: String::new(),
                            });
                            added_ch += 1;
                        }
                    }

                    for m in data.members {
                        let exists = targets.iter().any(|t| t.user_id == m.user_id);
                        if !exists {
                            targets.push(DiscordTarget {
                                id: format!("bot_u_{}", m.user_id),
                                name: m.name.clone(),
                                server_name: String::new(),
                                passphrases: vec![m.name],
                                kind: DiscordTargetKind::Mention,
                                guild_id: String::new(),
                                channel_id: String::new(),
                                user_id: m.user_id,
                                prepend_mention: true,
                                webhook_url: String::new(),
                            });
                            added_mem += 1;
                        }
                    }

                    let _ = state.config.save_to(&state.config_path);
                    let mut msg = format!(
                        "公式Bot同期完了: 新規チャンネル {}件 / メンバー {}人を追加しました",
                        added_ch, added_mem
                    );
                    if let Some(note) = data.member_note {
                        msg.push_str(" ");
                        msg.push_str(&note);
                    }
                    emit(
                        &webview,
                        &BackendEvent::ConfigLoaded {
                            config: state.config.clone(),
                            config_path: state.config_path.display().to_string(),
                        },
                    );
                    emit(
                        &webview,
                        &BackendEvent::DiscordBotSyncResult {
                            ok: true,
                            added_channels: added_ch,
                            added_members: added_mem,
                            message: msg,
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::DiscordBotSyncResult {
                            ok: false,
                            added_channels: 0,
                            added_members: 0,
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::AutomationResult {
                ok,
                action,
                message,
            }) => {
                if ok {
                    if action == "send" || action == "send_draft" {
                        sound_player.play(SoundKind::Send, &state.config.sound);
                    }
                    emit_all(
                        &webview,
                        &overlay_webview,
                        &BackendEvent::CommandExecuted {
                            action,
                            detail: message,
                        },
                    );
                } else {
                    sound_player.play(SoundKind::Cancel, &state.config.sound);
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message,
                        },
                    );
                }
            }
            Event::UserEvent(UserEvent::TtsStatus(status)) => {
                emit(
                    &webview,
                    &BackendEvent::TtsStatusChanged {
                        connected: status.connected,
                        speaking: status.speaking,
                        queued: status.queued,
                        last_error: status.last_error,
                    },
                );
            }
            Event::UserEvent(UserEvent::Notification(ev)) => match ev {
                NotificationEvent::SpeakText {
                    sender,
                    body,
                    speech_text,
                } => {
                    tts_ctrl.enqueue(speech_text, false);
                    if state.config.discord_tts.debug_log_toasts {
                        emit(
                            &webview,
                            &BackendEvent::ToastDebugLog {
                                text: format!("[読み上げキュー追加] {}: {}", sender, body),
                            },
                        );
                    }
                }
                NotificationEvent::DebugToast(line) => {
                    emit(&webview, &BackendEvent::ToastDebugLog { text: line });
                }
            },
            Event::UserEvent(UserEvent::VoicevoxHealthDone(res)) => match res {
                Ok(ver) => {
                    emit(
                        &webview,
                        &BackendEvent::VoicevoxHealthResult {
                            ok: true,
                            version: Some(ver.clone()),
                            message: format!("VOICEVOX 接続OK (v{})", ver),
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::VoicevoxHealthResult {
                            ok: false,
                            version: None,
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::VoicevoxSpeakersDone(res)) => match res {
                Ok(speakers) => {
                    let count = speakers.len();
                    let dto: Vec<VoicevoxSpeakerDto> = speakers
                        .into_iter()
                        .map(|s| VoicevoxSpeakerDto {
                            id: s.id,
                            name: s.name,
                            style_name: s.style_name,
                        })
                        .collect();
                    emit(
                        &webview,
                        &BackendEvent::VoicevoxSpeakersResult {
                            ok: true,
                            speakers: dto,
                            message: format!("VOICEVOX 話者リストを {} 件取得しました", count),
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::VoicevoxSpeakersResult {
                            ok: false,
                            speakers: Vec::new(),
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::Hotkey(hk_action)) => match hk_action {
                HotkeyAction::ToggleListening => {
                    let next = !state.listening;
                    state.ptt_active = false;
                    set_listening_state(
                        next,
                        true,
                        &mut state,
                        &audio_ctrl,
                        &sound_player,
                        &webview,
                        &overlay_webview,
                        &tray,
                    );
                }
                HotkeyAction::PushToTalkDown => {
                    if !state.ptt_active {
                        state.listening_before_ptt = state.listening;
                        state.ptt_active = true;
                        set_listening_state(
                            true,
                            true,
                            &mut state,
                            &audio_ctrl,
                            &sound_player,
                            &webview,
                            &overlay_webview,
                            &tray,
                        );
                    }
                }
                HotkeyAction::PushToTalkUp => {
                    if state.ptt_active {
                        state.ptt_active = false;
                        audio_ctrl.flush();
                        let restore = state.listening_before_ptt;
                        set_listening_state(
                            restore,
                            !restore,
                            &mut state,
                            &audio_ctrl,
                            &sound_player,
                            &webview,
                            &overlay_webview,
                            &tray,
                        );
                    }
                }
                HotkeyAction::ClearDraft => {
                    state.draft_segments.clear();
                    if state.pending_confirm.take().is_some() {
                        clear_confirmation_ui(&webview, &overlay_webview);
                    }
                    sound_player.play(SoundKind::Cancel, &state.config.sound);
                    emit_all(
                        &webview,
                        &overlay_webview,
                        &BackendEvent::TranscriptUpdate {
                            text: String::new(),
                            draft: String::new(),
                            is_final: true,
                        },
                    );
                    emit_all(
                        &webview,
                        &overlay_webview,
                        &BackendEvent::CommandExecuted {
                            action: "clear_draft".into(),
                            detail: "ホットキーでドラフトを消去しました".into(),
                        },
                    );
                }
                HotkeyAction::ToggleOverlay => {
                    state.overlay_visible = !state.overlay_visible;
                    state.config.overlay.enabled = state.overlay_visible;
                    overlay_window.set_visible(state.overlay_visible);
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
            },
            Event::UserEvent(UserEvent::Tray(action)) => match action {
                TrayAction::ToggleListening => {
                    let next = !state.listening;
                    state.ptt_active = false;
                    set_listening_state(
                        next,
                        true,
                        &mut state,
                        &audio_ctrl,
                        &sound_player,
                        &webview,
                        &overlay_webview,
                        &tray,
                    );
                }
                TrayAction::ToggleOverlay => {
                    state.overlay_visible = !state.overlay_visible;
                    state.config.overlay.enabled = state.overlay_visible;
                    overlay_window.set_visible(state.overlay_visible);
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
                TrayAction::ShowMainWindow => {
                    window.set_visible(true);
                    window.set_focus();
                }
                TrayAction::Quit => {
                    let _ = state.config.save_to(&state.config_path);
                    notif_ctrl.shutdown();
                    tts_ctrl.shutdown();
                    if state.config.discord_tts.voicevox.stop_on_exit {
                        tts::launcher::stop_if_started_by_us();
                    }
                    audio_ctrl.shutdown();
                    let _ = stt_tx.send(SttWorkerMsg::Shutdown);
                    *control_flow = ControlFlow::Exit;
                }
            },
            Event::UserEvent(UserEvent::Ipc(raw)) => match ClientCommand::parse(&raw) {
                Ok(cmd) => match cmd {
                    ClientCommand::GetConfig => {
                        state.config.auto_start = autostart::is_auto_start_enabled();
                        emit(
                            &webview,
                            &BackendEvent::ConfigLoaded {
                                config: state.config.clone(),
                                config_path: state.config_path.display().to_string(),
                            },
                        );
                        emit_all(&webview, &overlay_webview, &state.status_event());
                        if let Some(ref pending) = state.pending_confirm {
                            emit_all(&webview, &overlay_webview, &pending.prompt_event());
                        }
                        for w in pending_boot_warnings.drain(..) {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "error".into(),
                                    message: w,
                                },
                            );
                        }
                    }
                    ClientCommand::SaveConfig { config } => {
                        let stt_changed = state.config.stt != config.stt;
                        let hotkeys_changed = state.config.hotkeys != config.hotkeys;
                        let autostart_changed = state.config.auto_start != config.auto_start;
                        let should_launch_voicevox = config.discord_tts.enabled
                            && config.discord_tts.voicevox.auto_launch
                            && (!state.config.discord_tts.enabled
                                || !state.config.discord_tts.voicevox.auto_launch
                                || state.config.discord_tts.voicevox.exe_path
                                    != config.discord_tts.voicevox.exe_path
                                || state.config.discord_tts.voicevox.base_url
                                    != config.discord_tts.voicevox.base_url);
                        state.config = config;
                        state.overlay_visible = state.config.overlay.enabled;
                        overlay_window.set_visible(state.overlay_visible);
                        let _ = overlay_window
                            .set_ignore_cursor_events(state.config.overlay.click_through);
                        state.sync_guard();
                        state.clipboard_watch_flag.store(
                            state.config.discord_integration.clipboard_url_watch,
                            Ordering::Relaxed,
                        );
                        audio_ctrl.set_device(state.config.audio_device.clone());
                        audio_ctrl.update_vad(state.config.vad.clone());
                        audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                        tts_ctrl.update_config(state.config.discord_tts.clone());
                        notif_ctrl.update_config(state.config.discord_tts.clone());
                        if should_launch_voicevox {
                            spawn_voicevox_autostart(
                                state.config.discord_tts.voicevox.base_url.clone(),
                                state.config.discord_tts.voicevox.exe_path.clone(),
                                proxy.clone(),
                            );
                        }

                        if autostart_changed {
                            if let Err(err) = autostart::set_auto_start(state.config.auto_start) {
                                state.config.auto_start = autostart::is_auto_start_enabled();
                                emit(
                                    &webview,
                                    &BackendEvent::Toast {
                                        level: "error".into(),
                                        message: err,
                                    },
                                );
                            }
                        }

                        if stt_changed {
                            state.stt_ready = false;
                            state.stt_engine_name = "モデル再読み込み中...".into();
                            spawn_stt_loader(
                                state.config.stt.clone(),
                                state.runtime_dir.clone(),
                                proxy.clone(),
                            );
                        }

                        if hotkeys_changed {
                            if let Some(ref mut mgr) = hotkey_mgr {
                                let warns = mgr.reload(&state.config.hotkeys);
                                for w in warns {
                                    emit(
                                        &webview,
                                        &BackendEvent::Toast {
                                            level: "error".into(),
                                            message: w,
                                        },
                                    );
                                }
                            }
                        }

                        match state.config.save_to(&state.config_path) {
                            Ok(()) => {
                                emit(
                                    &webview,
                                    &BackendEvent::ConfigLoaded {
                                        config: state.config.clone(),
                                        config_path: state.config_path.display().to_string(),
                                    },
                                );
                                emit(
                                    &webview,
                                    &BackendEvent::ConfigSaved {
                                        ok: true,
                                        message: Some("設定を保存しました".into()),
                                    },
                                );
                            }
                            Err(err) => {
                                emit(
                                    &webview,
                                    &BackendEvent::ConfigSaved {
                                        ok: false,
                                        message: Some(err),
                                    },
                                );
                            }
                        }
                        if let Some(ref t) = tray {
                            t.update_state(state.tray_state(), state.overlay_visible);
                        }
                        emit_all(&webview, &overlay_webview, &state.status_event());
                    }
                    ClientCommand::ConfirmTargetReply { reply } => {
                        let parsed = match reply.as_str() {
                            "yes" => Some(ConfirmationReply::Yes),
                            "next" => Some(ConfirmationReply::Next),
                            "prev" => Some(ConfirmationReply::Prev),
                            "cancel" => Some(ConfirmationReply::Cancel),
                            other => parse_confirmation_reply(other),
                        };
                        if let Some(r) = parsed {
                            handle_confirmation_reply(
                                r,
                                &mut state,
                                &webview,
                                &overlay_webview,
                                &tray,
                                &sound_player,
                                &proxy,
                            );
                        }
                    }
                    ClientCommand::SelectDiscordTarget {
                        target_id,
                        navigate,
                    } => match target_id {
                        Some(id) if !id.trim().is_empty() => {
                            if let Some(t) = state.config.discord_targets.find_by_id(&id).cloned() {
                                activate_registered_target(
                                    t,
                                    navigate,
                                    &mut state,
                                    &webview,
                                    &overlay_webview,
                                    &sound_player,
                                    &proxy,
                                );
                            }
                        }
                        _ => {
                            state.config.discord_targets.active_target_id = None;
                            let _ = state.config.save_to(&state.config_path);
                            sound_player.play(SoundKind::ToggleOff, &state.config.sound);
                            emit_all(&webview, &overlay_webview, &state.status_event());
                        }
                    },
                    ClientCommand::NavigateDiscordTarget { target_id } => {
                        if let Some(t) = state.config.discord_targets.find_by_id(&target_id).cloned()
                        {
                            spawn_navigate_target(
                                t,
                                state.config.guard.clone(),
                                state.config.discord_targets.navigate_delay_ms,
                                proxy.clone(),
                            );
                        }
                    }
                    ClientCommand::ScanDiscordUi => {
                        let allowed = state.config.guard.allowed_processes.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = uia::scan_discord_window(&allowed);
                            let _ = p.send_event(UserEvent::DiscordUiScanDone(res));
                        });
                    }
                    ClientCommand::SyncDiscordBot { bot_token } => {
                        state.config.discord_integration.bot_token = bot_token.clone();
                        let _ = state.config.save_to(&state.config_path);
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = discord::sync_with_bot_token(&bot_token);
                            let _ = p.send_event(UserEvent::DiscordBotSyncDone(res));
                        });
                    }
                    ClientCommand::DiscordRemoteAction { action, delay_ms } => {
                        let key_action = match action.as_str() {
                            "mute_mic" => KeyAction::DiscordMuteMic,
                            "deafen" => KeyAction::DiscordDeafen,
                            "next_channel" => KeyAction::DiscordNextChannel,
                            "prev_channel" => KeyAction::DiscordPrevChannel,
                            "next_unread" => KeyAction::DiscordNextUnread,
                            "next_server" => KeyAction::DiscordNextServer,
                            "prev_server" => KeyAction::DiscordPrevServer,
                            "scroll_up" => KeyAction::DiscordScrollUp,
                            "scroll_down" => KeyAction::DiscordScrollDown,
                            "edit_last" => KeyAction::DiscordEditLast,
                            "mark_read" => KeyAction::DiscordMarkRead,
                            "toggle_members" => KeyAction::DiscordToggleMembers,
                            "search" => KeyAction::DiscordSearch,
                            _ => KeyAction::DiscordMuteMic,
                        };
                        let delay = delay_ms.unwrap_or(0).min(10_000);
                        let guard = state.config.guard.clone();
                        let p = proxy.clone();
                        if delay > 0 {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "info".into(),
                                    message: format!(
                                        "{:.1}秒後にDiscord操作を実行します（Discord画面をフォーカスしてください）",
                                        delay as f64 / 1000.0
                                    ),
                                },
                            );
                        }
                        thread::spawn(move || {
                            if delay > 0 {
                                thread::sleep(Duration::from_millis(delay));
                            }
                            let res = automation::send_action_guarded(key_action, &guard);
                            let _ = p.send_event(match res {
                                Ok(msg) => UserEvent::AutomationResult {
                                    ok: true,
                                    action,
                                    message: msg,
                                },
                                Err(err) => UserEvent::AutomationResult {
                                    ok: false,
                                    action,
                                    message: err,
                                },
                            });
                        });
                    }
                    ClientCommand::ToggleListening => {
                        let next = !state.listening;
                        state.ptt_active = false;
                        set_listening_state(
                            next,
                            true,
                            &mut state,
                            &audio_ctrl,
                            &sound_player,
                            &webview,
                            &overlay_webview,
                            &tray,
                        );
                    }
                    ClientCommand::SetInputMode { mode } => {
                        state.config.input_mode = mode;
                        state.last_direct_utterance.clear();
                        let _ = state.config.save_to(&state.config_path);
                        emit_all(&webview, &overlay_webview, &state.status_event());
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "info".into(),
                                message: match mode {
                                    InputMode::Draft => "ドラフトモードに切り替えました".into(),
                                    InputMode::Direct => "ダイレクト入力モードに切り替えました".into(),
                                },
                            },
                        );
                    }
                    ClientCommand::ClearDraft => {
                        state.draft_segments.clear();
                        sound_player.play(SoundKind::Cancel, &state.config.sound);
                        emit_all(
                            &webview,
                            &overlay_webview,
                            &BackendEvent::TranscriptUpdate {
                                text: String::new(),
                                draft: String::new(),
                                is_final: true,
                            },
                        );
                        emit_all(
                            &webview,
                            &overlay_webview,
                            &BackendEvent::CommandExecuted {
                                action: "clear_draft".into(),
                                detail: "ドラフトをクリアしました".into(),
                            },
                        );
                    }
                    ClientCommand::UpdateDraft { draft } => {
                        state.draft_segments.clear();
                        if !draft.is_empty() {
                            state.draft_segments.push(draft.clone());
                        }
                        emit(
                            &overlay_webview,
                            &BackendEvent::TranscriptUpdate {
                                text: String::new(),
                                draft,
                                is_final: false,
                            },
                        );
                    }
                    ClientCommand::ForceSendDraft => {
                        let text = state.draft_text();
                        if text.is_empty() {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "info".into(),
                                    message: "送信するドラフトがありません".into(),
                                },
                            );
                        } else {
                            state.draft_segments.clear();
                            emit_all(
                                &webview,
                                &overlay_webview,
                                &BackendEvent::TranscriptUpdate {
                                    text: text.clone(),
                                    draft: String::new(),
                                    is_final: true,
                                },
                            );
                            spawn_send_for_state(text, true, "send_draft", &state, proxy.clone());
                        }
                    }
                    ClientCommand::CalibrateVad => {
                        if !state.listening {
                            set_listening_state(
                                true,
                                false,
                                &mut state,
                                &audio_ctrl,
                                &sound_player,
                                &webview,
                                &overlay_webview,
                                &tray,
                            );
                        }
                        audio_ctrl.calibrate_noise(2000);
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "info".into(),
                                message: "2秒間、話さずにそのままお待ちください（環境ノイズを測定中...）".into(),
                            },
                        );
                    }
                    ClientCommand::SimulateTranscript { text } => {
                        handle_transcript(
                            &text,
                            &mut state,
                            &webview,
                            &overlay_webview,
                            &tray,
                            &sound_player,
                            &proxy,
                        );
                    }
                    ClientCommand::GetActiveWindow => {
                        let (allowed, proc_name) = automation::check_target(&state.config.guard);
                        state.active_process = proc_name.clone();
                        state.target_active = allowed;
                        emit_all(&webview, &overlay_webview, &state.status_event());
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: if allowed { "ok" } else { "info" }.into(),
                                message: format!(
                                    "最前面プロセス: {} ({})",
                                    if proc_name.is_empty() { "不明" } else { &proc_name },
                                    if allowed { "許可対象" } else { "ガード対象外" }
                                ),
                            },
                        );
                    }
                    ClientCommand::TestPaste {
                        text,
                        press_enter,
                        delay_ms,
                    } => {
                        let delay = delay_ms.unwrap_or(0).min(15_000);
                        let guard = state.config.guard.clone();
                        let integration = state.config.discord_integration.clone();
                        let target = state.active_discord_target();
                        let auto_nav = state.config.discord_targets.auto_navigate_on_send;
                        let nav_delay = state.config.discord_targets.navigate_delay_ms;
                        let p = proxy.clone();
                        if delay > 0 {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "info".into(),
                                    message: format!(
                                        "{:.1}秒後にテスト入力を実行します（対象ウィンドウをフォーカスしてください）",
                                        delay as f64 / 1000.0
                                    ),
                                },
                            );
                        }
                        thread::spawn(move || {
                            if delay > 0 {
                                thread::sleep(Duration::from_millis(delay));
                            }
                            let res = automation::send_to_discord_target(
                                &text,
                                press_enter,
                                target.as_ref(),
                                auto_nav,
                                nav_delay,
                                &guard,
                                &integration,
                            );
                            let _ = p.send_event(match res {
                                Ok(msg) => UserEvent::AutomationResult {
                                    ok: true,
                                    action: "test_paste".into(),
                                    message: msg,
                                },
                                Err(err) => UserEvent::AutomationResult {
                                    ok: false,
                                    action: "test_paste".into(),
                                    message: err,
                                },
                            });
                        });
                    }
                    ClientCommand::TestKeyAction { action, delay_ms } => {
                        let delay = delay_ms.unwrap_or(0).min(15_000);
                        let key_action = match action.as_str() {
                            "newline" => KeyAction::ShiftEnter,
                            "undo" => KeyAction::CtrlZ,
                            "clear_all" => KeyAction::ClearAll,
                            "enter" => KeyAction::Enter,
                            _ => KeyAction::ShiftEnter,
                        };
                        let guard = state.config.guard.clone();
                        let p = proxy.clone();
                        if delay > 0 {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "info".into(),
                                    message: format!(
                                        "{:.1}秒後にキー送信テスト({})を実行します",
                                        delay as f64 / 1000.0,
                                        action
                                    ),
                                },
                            );
                        }
                        thread::spawn(move || {
                            if delay > 0 {
                                thread::sleep(Duration::from_millis(delay));
                            }
                            let res = automation::send_action_guarded(key_action, &guard);
                            let _ = p.send_event(match res {
                                Ok(msg) => UserEvent::AutomationResult {
                                    ok: true,
                                    action,
                                    message: msg,
                                },
                                Err(err) => UserEvent::AutomationResult {
                                    ok: false,
                                    action,
                                    message: err,
                                },
                            });
                        });
                    }
                    ClientCommand::ListAudioDevices => {
                        let devices = list_input_devices();
                        emit(
                            &webview,
                            &BackendEvent::AudioDevices {
                                devices,
                                selected: state.config.audio_device.clone(),
                            },
                        );
                    }
                    ClientCommand::ToggleOverlay => {
                        state.overlay_visible = !state.overlay_visible;
                        state.config.overlay.enabled = state.overlay_visible;
                        overlay_window.set_visible(state.overlay_visible);
                        let _ = state.config.save_to(&state.config_path);
                        if let Some(ref t) = tray {
                            t.update_state(state.tray_state(), state.overlay_visible);
                        }
                        emit_all(&webview, &overlay_webview, &state.status_event());
                    }
                    ClientCommand::SetOverlayClickThrough { click_through } => {
                        state.config.overlay.click_through = click_through;
                        let _ = overlay_window.set_ignore_cursor_events(click_through);
                        let _ = state.config.save_to(&state.config_path);
                    }
                    ClientCommand::DragOverlay => {
                        if !state.config.overlay.click_through {
                            let _ = overlay_window.drag_window();
                        }
                    }
                    ClientCommand::PlayTestSound { kind } => {
                        let mut preview_cfg = state.config.sound.clone();
                        preview_cfg.enabled = true;
                        sound_player.play(SoundKind::from_name(&kind), &preview_cfg);
                    }
                    ClientCommand::VoicevoxCheckHealth { base_url } => {
                        let url = base_url
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| state.config.discord_tts.voicevox.base_url.clone());
                        let auto_launch = state.config.discord_tts.voicevox.auto_launch;
                        let exe_path = state.config.discord_tts.voicevox.exe_path.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let client = VoicevoxClient::new();
                            let res = match client.check_health(&url) {
                                Ok(v) => Ok(v),
                                Err(err) => {
                                    if auto_launch
                                        && tts::launcher::ensure_running(&url, &exe_path).is_ok()
                                    {
                                        client.check_health(&url)
                                    } else {
                                        Err(err)
                                    }
                                }
                            };
                            let _ = p.send_event(UserEvent::VoicevoxHealthDone(res));
                        });
                    }
                    ClientCommand::VoicevoxGetSpeakers { base_url } => {
                        let url = base_url
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| state.config.discord_tts.voicevox.base_url.clone());
                        let auto_launch = state.config.discord_tts.voicevox.auto_launch;
                        let exe_path = state.config.discord_tts.voicevox.exe_path.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let client = VoicevoxClient::new();
                            let res = match client.get_speakers(&url) {
                                Ok(list) => Ok(list),
                                Err(err) => {
                                    if auto_launch
                                        && tts::launcher::ensure_running(&url, &exe_path).is_ok()
                                    {
                                        client.get_speakers(&url)
                                    } else {
                                        Err(err)
                                    }
                                }
                            };
                            let _ = p.send_event(UserEvent::VoicevoxSpeakersDone(res));
                        });
                    }
                    ClientCommand::VoicevoxDetectPath => {
                        match tts::launcher::detect_voicevox_path() {
                            Some((path, headless)) => {
                                let path_str = path.to_string_lossy().into_owned();
                                state.config.discord_tts.voicevox.exe_path = path_str.clone();
                                let _ = state.config.save_to(&state.config_path);
                                emit(
                                    &webview,
                                    &BackendEvent::VoicevoxDetectResult {
                                        found: true,
                                        path: path_str,
                                        headless,
                                        message: if headless {
                                            "VOICEVOX エンジン（窓なしバックグラウンド起動対応）を検出しました".into()
                                        } else {
                                            "VOICEVOX 本体を検出しました".into()
                                        },
                                    },
                                );
                            }
                            None => {
                                emit(
                                    &webview,
                                    &BackendEvent::VoicevoxDetectResult {
                                        found: false,
                                        path: String::new(),
                                        headless: false,
                                        message: "VOICEVOX が見つかりませんでした（インストール済みなら実行ファイルパスを手動入力してください）".into(),
                                    },
                                );
                            }
                        }
                    }
                    ClientCommand::VoicevoxTestSpeak { text } => {
                        let msg = text
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "テスト読み上げです".to_string());
                        tts_ctrl.enqueue(msg, true);
                    }
                    ClientCommand::TtsStop => {
                        tts_ctrl.stop_and_clear();
                        emit(
                            &webview,
                            &BackendEvent::TtsStatusChanged {
                                connected: true,
                                speaking: false,
                                queued: 0,
                                last_error: None,
                            },
                        );
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "info".into(),
                                message: "読み上げを停止し、キューをクリアしました".into(),
                            },
                        );
                    }
                },
                Err(err) => {
                    eprintln!("[ipc parse error] {}", err);
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message: err,
                        },
                    );
                }
            },
            _ => {}
        }
    });
}
