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
mod gpu_diag;
mod hotkey;
mod ipc;
mod notification;
mod processor;
mod runtime;
mod sound;
mod stats;
mod stt;
mod tray;
mod tts;

use audio::capture::{list_input_devices, AudioController};
use audio::AudioEvent;
use automation::discord;
use automation::input::KeyAction;
use automation::uia;
use config::{
    ActivationMode, AppConfig, DictionaryEntry, DiscordTarget, DiscordTargetKind, GameProfile,
    GuardConfig, InputMode, NoiseEngineKind, SpeechStyle, TextFilterConfig,
};
use hotkey::{HotkeyAction, HotkeyManager};
use ipc::{
    BackendEvent, ClientCommand, ScannedChannelDto, UtteranceDiagnosticDto, VoicevoxSpeakerDto,
};
use notification::{NotificationController, NotificationEvent};
use processor::commands::{
    parse_confirmation_reply, parse_countdown_cancel, parse_special_voice_action,
    ConfirmationReply, SpecialVoiceAction, TargetSwitchAction, VoiceCommand,
};
use sound::{SoundKind, SoundPlayer};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use stt::{SttEngine, TranscribeContext};
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
    fn device_name(&self) -> String {
        "初期化中...".into()
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
    TranscriptReady {
        samples_16k: Vec<f32>,
        duration_sec: f32,
        peak_dbfs: f32,
        infer_ms: u64,
        rtf: f32,
        engine_label: String,
        device_name: String,
        result: Result<String, String>,
    },
    AbCompareDone {
        id: u64,
        engine_label: String,
        device_name: String,
        infer_ms: u64,
        rtf: f32,
        result: Result<String, String>,
    },
    SendCountdownTick {
        id: u64,
        remaining_ms: u64,
        total_ms: u64,
    },
    SendCountdownFire {
        id: u64,
    },
    SendCountdownCancelEsc {
        id: u64,
    },
    ModelDownloadProgress {
        model_id: String,
        progress: u8,
        message: String,
        done: bool,
        ok: bool,
    },
    UrlHealthCheckDone(Vec<stt::catalog::ModelUrlHealthDto>),
    OnlineCatalogSyncDone(Result<(usize, usize, String), String>),
    ModelsDiscoveredDone(Result<stt::catalog::ModelDiscoveryReport, String>),
    HttpAsrHealthDone(Result<String, String>),
    LlmHealthDone(Result<String, String>),
    LlmSampleDone {
        ok: bool,
        input: String,
        output: String,
        elapsed_ms: u64,
        error: Option<String>,
    },
    PhaseUpdate(String),
    StreamingPreviewReady(String),
    AppUpdateFound {
        latest_tag: String,
        url: String,
    },
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
        ctx: TranscribeContext,
        accuracy: config::AccuracyPipelineConfig,
        noise_stt: NoiseEngineKind,
        dpdfnet_limit_db: f32,
        stt_cfg: config::SttConfig,
        runtime_dir: Option<PathBuf>,
        recent_msgs: Vec<String>,
        target_label: String,
        game_name: Option<String>,
        dict: Vec<DictionaryEntry>,
        speech_style: SpeechStyle,
    },
    TranscribePreview {
        samples: Vec<f32>,
        ctx: TranscribeContext,
    },
    ReloadEngine(Arc<dyn SttEngine>),
    UpdatePromptHints(String),
    ClearCaches,
    UnloadAll,
    Shutdown,
}

static AB_ENGINE_CACHE: Mutex<Option<(String, Arc<dyn SttEngine>)>> = Mutex::new(None);

#[repr(C)]
struct WinSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

fn current_local_hms() -> String {
    let (_, _, st) = current_local_time_raw();
    format!("{:02}:{:02}:{:02}", st.hour, st.minute, st.second)
}

fn current_local_ymd_hour() -> (String, u8) {
    let (ymd, hour, _) = current_local_time_raw();
    (ymd, hour)
}

fn current_local_time_raw() -> (String, u8, WinSystemTime) {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(lp_system_time: *mut WinSystemTime);
    }
    let mut st = WinSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    unsafe {
        GetLocalTime(&mut st);
    }
    (
        format!("{:04}-{:02}-{:02}", st.year, st.month, st.day),
        st.hour as u8,
        st,
    )
}

pub fn compute_peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .fold(0.0f32, |acc, &s| acc.max(s.abs()));
    if peak < 1e-5 {
        -96.0
    } else {
        (20.0 * peak.log10()).clamp(-96.0, 0.0)
    }
}

pub fn find_matching_game_profile<'a>(
    process: &str,
    profiles: &'a [GameProfile],
) -> Option<&'a GameProfile> {
    let proc_trim = process.trim();
    if proc_trim.is_empty() {
        return None;
    }
    profiles.iter().find(|p| {
        p.enabled
            && p.process_names
                .iter()
                .any(|n| !n.trim().is_empty() && n.trim().eq_ignore_ascii_case(proc_trim))
    })
}

pub fn merge_profile_filter(
    base: &TextFilterConfig,
    profile: Option<&GameProfile>,
) -> TextFilterConfig {
    let mut merged = base.clone();
    if let Some(p) = profile {
        for entry in &p.dictionary {
            if !entry.from.trim().is_empty() {
                merged.dictionary.push(entry.clone());
            }
        }
    }
    merged
}

fn build_stt_prompt_hints(cfg: &AppConfig, active_profile: Option<&GameProfile>) -> String {
    build_stt_prompt_hints_with_ctx(cfg, active_profile, &[], None)
}

fn build_stt_prompt_hints_with_ctx(
    cfg: &AppConfig,
    active_profile: Option<&GameProfile>,
    recent_msgs: &[String],
    active_target_label: Option<&str>,
) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut push_term = |s: &str| {
        let t = s.trim();
        if t.is_empty() || (t.starts_with(':') && t.ends_with(':')) {
            return;
        }
        if !terms.iter().any(|existing| existing == t) {
            terms.push(t.to_string());
        }
    };

    for s in ["Discord", "DM", "通話", "ミュート", "テスト"] {
        push_term(s);
    }
    if let Some(lbl) = active_target_label {
        push_term(lbl);
    }
    if let Some(p) = active_profile {
        push_term(&p.name);
        for d in &p.dictionary {
            push_term(&d.to);
        }
    }
    for d in &cfg.speech_habits {
        push_term(&d.to);
    }
    for s in cfg
        .commands
        .send
        .iter()
        .chain(cfg.commands.clear_all.iter())
        .chain(cfg.commands.undo.iter())
        .chain(cfg.commands.newline.iter())
    {
        push_term(s);
    }
    for d in &cfg.filter.dictionary {
        push_term(&d.to);
    }
    for tgt in cfg.discord_targets.targets.iter().take(6) {
        push_term(&tgt.name);
        if let Some(p) = tgt.passphrases.first() {
            push_term(p);
        }
    }
    if cfg.stt.accuracy.context_prompt_injection {
        for msg in recent_msgs.iter().rev().take(2) {
            let short: String = msg.chars().take(22).collect();
            push_term(&short);
        }
    }

    let mut out = String::new();
    for term in terms {
        let add_len = term.chars().count() + if out.is_empty() { 0 } else { 1 };
        if out.chars().count() + add_len > 200 {
            break;
        }
        if !out.is_empty() {
            out.push('、');
        }
        out.push_str(&term);
    }
    out
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

#[derive(Clone)]
struct UtteranceDiagnostic {
    id: u64,
    timestamp: String,
    samples_16k: Vec<f32>,
    duration_sec: f32,
    peak_dbfs: f32,
    infer_ms: u64,
    rtf: f32,
    device_name: String,
    engine_label: String,
    raw_text: String,
    final_text: String,
}

impl UtteranceDiagnostic {
    fn to_dto(&self) -> UtteranceDiagnosticDto {
        UtteranceDiagnosticDto {
            id: self.id,
            timestamp: self.timestamp.clone(),
            duration_sec: self.duration_sec,
            peak_dbfs: self.peak_dbfs,
            infer_ms: self.infer_ms,
            rtf: self.rtf,
            device_name: self.device_name.clone(),
            engine_label: self.engine_label.clone(),
            raw_text: self.raw_text.clone(),
            final_text: self.final_text.clone(),
        }
    }
}

struct PendingSendCountdown {
    id: u64,
    text: String,
    press_enter: bool,
    action: &'static str,
    cancel_flag: Arc<AtomicBool>,
}

struct AppState {
    config: AppConfig,
    config_path: PathBuf,
    runtime_dir: Option<PathBuf>,
    shared_guard: Arc<RwLock<GuardConfig>>,
    clipboard_watch_flag: Arc<AtomicBool>,
    stt_ready: bool,
    stt_engine_name: String,
    stt_device_name: String,
    last_infer_ms: u64,
    last_rtf: f32,
    listening: bool,
    sleeping: bool,
    recognizing: bool,
    ptt_active: bool,
    listening_before_ptt: bool,
    target_active: bool,
    active_process: String,
    active_game_profile_id: Option<String>,
    overlay_visible: bool,
    draft_segments: Vec<String>,
    direct_history_lengths: Vec<usize>,
    last_direct_utterance: String,
    pending_confirm: Option<PendingConfirmation>,
    pending_countdown: Option<PendingSendCountdown>,
    next_countdown_id: u64,
    recent_utterances: VecDeque<UtteranceDiagnostic>,
    next_diag_id: u64,
    phase: String,
    panic_mode: bool,
    night_mode_active: bool,
    eco_mode_active: bool,
    vram_eco_offloaded: bool,
    recent_sent_texts: VecDeque<(String, Instant)>,
    last_dm_sender_info: Option<(String, Instant)>,
    last_cancelled_raw: Option<(String, Instant)>,
    calibration_wizard_active: bool,
    stats: stats::StatsStore,
    stats_path: PathBuf,
    bench_cases: Vec<stats::BenchmarkCase>,
    win_poll_ticks: u32,
    model_health_map: std::collections::BTreeMap<String, stt::catalog::ModelUrlHealthDto>,
    last_preset_snapshot: Option<stt::catalog::PresetRollbackSnapshot>,
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
        let active_game_profile_id =
            find_matching_game_profile(&active_process, &config.game_profiles)
                .map(|p| p.id.clone());
        let stats_path = stats::StatsStore::default_path();
        let stats = stats::StatsStore::load_from(&stats_path);
        let bench_cases = stats::load_benchmark_cases(&stats::bench_dir());
        Self {
            config,
            config_path,
            runtime_dir: None,
            shared_guard,
            clipboard_watch_flag,
            stt_ready: false,
            stt_engine_name: "モデル読み込み中...".into(),
            stt_device_name: "初期化中...".into(),
            last_infer_ms: 0,
            last_rtf: 0.0,
            listening: false,
            sleeping: false,
            recognizing: false,
            ptt_active: false,
            listening_before_ptt: false,
            target_active,
            active_process,
            active_game_profile_id,
            overlay_visible,
            draft_segments: Vec::new(),
            direct_history_lengths: Vec::new(),
            last_direct_utterance: String::new(),
            pending_confirm: None,
            pending_countdown: None,
            next_countdown_id: 1,
            recent_utterances: VecDeque::with_capacity(5),
            next_diag_id: 1,
            phase: "idle".into(),
            panic_mode: false,
            night_mode_active: false,
            eco_mode_active: false,
            vram_eco_offloaded: false,
            recent_sent_texts: VecDeque::with_capacity(10),
            last_dm_sender_info: None,
            last_cancelled_raw: None,
            calibration_wizard_active: false,
            stats,
            stats_path,
            bench_cases,
            win_poll_ticks: 0,
            model_health_map: std::collections::BTreeMap::new(),
            last_preset_snapshot: None,
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(AppConfig::app_data_dir)
    }

    fn recent_sent_strings(&self) -> Vec<String> {
        let now = Instant::now();
        self.recent_sent_texts
            .iter()
            .filter(|(_, ts)| now.duration_since(*ts) <= Duration::from_secs(30))
            .map(|(s, _)| s.clone())
            .collect()
    }

    fn record_sent_text(&mut self, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        if self.recent_sent_texts.len() >= 10 {
            self.recent_sent_texts.pop_front();
        }
        self.recent_sent_texts.push_back((t.to_string(), Instant::now()));
    }

    fn active_profile(&self) -> Option<&GameProfile> {
        let id = self.active_game_profile_id.as_deref()?;
        self.config
            .game_profiles
            .iter()
            .find(|p| p.enabled && p.id == id)
    }

    fn effective_guard(&self) -> GuardConfig {
        let mut g = self.config.guard.clone();
        if let Some(p) = self.active_profile() {
            if let Some(sm) = p.send_method {
                g.send_method = sm;
            }
        }
        g
    }

    fn effective_filter(&self) -> TextFilterConfig {
        let mut f = merge_profile_filter(&self.config.filter, self.active_profile());
        for h in &self.config.speech_habits {
            if !h.from.trim().is_empty() {
                f.dictionary.push(h.clone());
            }
        }
        f
    }

    fn effective_phase(&self) -> String {
        if self.pending_countdown.is_some() {
            "countdown".into()
        } else if self.phase == "rescuing" && self.recognizing {
            "rescuing".into()
        } else if self.recognizing {
            "recognizing".into()
        } else if self.listening && !self.sleeping {
            "listening".into()
        } else {
            "idle".into()
        }
    }

    fn sync_guard(&self) {
        let eff = self.effective_guard();
        if let Ok(mut g) = self.shared_guard.write() {
            *g = eff;
        }
    }

    fn push_diagnostic(
        &mut self,
        samples_16k: Vec<f32>,
        duration_sec: f32,
        peak_dbfs: f32,
        infer_ms: u64,
        rtf: f32,
        device_name: String,
        engine_label: String,
        raw_text: String,
        final_text: String,
    ) {
        let id = self.next_diag_id;
        self.next_diag_id += 1;
        if self.recent_utterances.len() >= 5 {
            self.recent_utterances.pop_back();
        }
        self.recent_utterances.push_front(UtteranceDiagnostic {
            id,
            timestamp: current_local_hms(),
            samples_16k,
            duration_sec,
            peak_dbfs,
            infer_ms,
            rtf,
            device_name,
            engine_label,
            raw_text,
            final_text,
        });
    }

    fn diagnostic_dtos(&self) -> Vec<UtteranceDiagnosticDto> {
        self.recent_utterances.iter().map(|d| d.to_dto()).collect()
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
            activation_mode: self.config.activation_mode,
            overlay_visible: self.overlay_visible,
            stt_ready: self.stt_ready,
            stt_engine: self.stt_engine_name.clone(),
            stt_device: self.stt_device_name.clone(),
            last_infer_ms: self.last_infer_ms,
            last_rtf: self.last_rtf,
            rnnoise_enabled: self.config.noise_cancel.enabled,
            active_discord_target_id: active_target.map(|t| t.id.clone()),
            active_discord_target_label: discord::target_display_label(active_target),
            active_profile: self.active_profile().map(|p| p.name.clone()),
            phase: self.effective_phase(),
            panic_mode: self.panic_mode,
            target_locked: self.config.guard.target_locked,
            noise_vad_engine: self.config.noise_cancel.vad_engine_name().into(),
            noise_stt_engine: self.config.noise_cancel.stt_engine_name().into(),
            night_mode_active: self.night_mode_active,
            eco_mode_active: self.eco_mode_active || self.vram_eco_offloaded,
            portable_mode: AppConfig::portable_dir().is_some(),
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

fn spawn_http_asr_autostart(
    http_cfg: config::HttpAsrConfig,
    proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let res = stt::http_asr::ensure_http_asr_running(&http_cfg);
        let _ = proxy.send_event(UserEvent::HttpAsrHealthDone(res));
    });
}

fn spawn_stt_worker(
    initial_engine: Arc<dyn SttEngine>,
    proxy: EventLoopProxy<UserEvent>,
) -> mpsc::Sender<SttWorkerMsg> {
    let (tx, rx) = mpsc::channel::<SttWorkerMsg>();
    thread::spawn(move || {
        let mut engine = initial_engine;
        let mut denoiser_cache: Option<(String, Option<Arc<stt::sherpa_ffi::ResidentSherpaDenoiser>>)> =
            None;
        let mut secondary_cache: Option<(String, Arc<dyn SttEngine>)> = None;

        while let Ok(msg) = rx.recv() {
            match msg {
                SttWorkerMsg::Transcribe {
                    samples,
                    ctx,
                    accuracy,
                    noise_stt,
                    dpdfnet_limit_db,
                    stt_cfg,
                    runtime_dir,
                    recent_msgs,
                    target_label,
                    game_name,
                    dict,
                    speech_style,
                } => {
                    let duration_sec = samples.len() as f32 / 16_000.0;
                    let peak_dbfs = compute_peak_dbfs(&samples);
                    let t0 = Instant::now();

                    // Layer 1 (#21): Standalone STT-path Neural Speech Enhancement (DPDFNet / GTCRN)
                    let denoised_samples = if noise_stt != NoiseEngineKind::Rnnoise
                        && noise_stt != NoiseEngineKind::OffHpf
                    {
                        let d_key = format!(
                            "{}|{}|{:.1}",
                            noise_stt.as_str(),
                            stt_cfg.model_dir,
                            dpdfnet_limit_db
                        );
                        let den_opt = match denoiser_cache {
                            Some((ref k, ref d)) if k == &d_key => d.clone(),
                            _ => {
                                let loaded = stt::local::discover_stt_denoiser(
                                    noise_stt,
                                    &stt_cfg,
                                    runtime_dir.as_deref(),
                                    dpdfnet_limit_db,
                                );
                                denoiser_cache = Some((d_key, loaded.clone()));
                                loaded
                            }
                        };
                        if let Some(ref den) = den_opt {
                            den.denoise(&samples, 16_000).unwrap_or_else(|_| samples.clone())
                        } else {
                            samples.clone()
                        }
                    } else {
                        samples.clone()
                    };

                    // Layer 2 (#1): WSOLA pitch-preserving slow stretch when speech is fast
                    let tempo = crate::audio::enhance::estimate_speech_tempo_score(&denoised_samples);
                    let use_wsola = accuracy.wsola_enabled
                        && (ctx.preset == config::ConversationPreset::Fast
                            || (accuracy.wsola_auto_fast && tempo >= 4.2));
                    let wsola_input = if use_wsola && (accuracy.wsola_rate - 1.0).abs() > 0.02 {
                        crate::audio::enhance::wsola_time_stretch(&denoised_samples, accuracy.wsola_rate)
                    } else {
                        denoised_samples.clone()
                    };

                    // Layer 2+4 (#35 + #38): Overlap chunking + Beam Search + Temperature Fallback
                    let primary_res = if accuracy.overlap_chunking_enabled {
                        stt::ensemble::transcribe_with_overlap(
                            engine.as_ref(),
                            &wsola_input,
                            &ctx,
                            accuracy.overlap_threshold_sec,
                            accuracy.overlap_sec,
                        )
                    } else {
                        engine.transcribe_with_context(&wsola_input, &ctx)
                    };

                    let mut final_res = primary_res;
                    let mut secondary_text: Option<String> = None;

                    // Layer 4 (#6, #34): 2-pass low-confidence rescue or Ultimate Dual-Engine Ensemble
                    if let Ok(ref p_txt) = final_res {
                        let needs_rescue =
                            accuracy.two_pass_rescue && stt::ensemble::looks_low_confidence(p_txt, duration_sec);
                        if accuracy.ultimate_ensemble || needs_rescue {
                            let _ = proxy.send_event(UserEvent::PhaseUpdate("rescuing".into()));
                            let sec_eng_id = accuracy.secondary_engine.trim();
                            let sec_key = format!("{}|{}|{}", sec_eng_id, stt_cfg.model_dir, stt_cfg.language);
                            let sec_ready = if !sec_eng_id.is_empty() && sec_eng_id != engine.id() {
                                let cached = match secondary_cache {
                                    Some((ref k, ref e)) if k == &sec_key => Some(e.clone()),
                                    _ => {
                                        let mut sec_cfg = stt_cfg.clone();
                                        sec_cfg.engine = sec_eng_id.to_string();
                                        sec_cfg.model_file.clear();
                                        let e = stt::create_engine(&sec_cfg, runtime_dir.as_deref());
                                        if e.is_ready() {
                                            secondary_cache = Some((sec_key, e.clone()));
                                            Some(e)
                                        } else {
                                            None
                                        }
                                    }
                                };
                                cached
                            } else {
                                None
                            };

                            if let Some(sec_eng) = sec_ready {
                                if let Some(ref hints) = ctx.hotwords_prompt {
                                    sec_eng.set_prompt_hints(hints);
                                }
                                if let Ok(s_txt) = sec_eng.transcribe_with_context(&wsola_input, &ctx) {
                                    secondary_text = Some(s_txt.clone());
                                    if !accuracy.llm.enabled {
                                        let picked = stt::ensemble::reconcile_dual_candidates(
                                            p_txt,
                                            &s_txt,
                                            duration_sec,
                                        );
                                        final_res = Ok(picked);
                                    }
                                }
                            } else if needs_rescue {
                                // Fallback rescue pass with stronger 0.85x WSOLA stretch
                                let slow_rescue =
                                    crate::audio::enhance::wsola_time_stretch(&denoised_samples, 0.85);
                                if let Ok(alt) = engine.transcribe_with_context(&slow_rescue, &ctx) {
                                    let picked = stt::ensemble::reconcile_dual_candidates(
                                        p_txt,
                                        &alt,
                                        duration_sec,
                                    );
                                    final_res = Ok(picked);
                                }
                            }
                        }
                    }

                    // Layer 5 (#3, #11): Local LLM Phonetic & Contextual Post-Correction
                    if accuracy.llm.enabled {
                        if let Ok(ref cur_txt) = final_res {
                            if !cur_txt.trim().is_empty() {
                                let _ = proxy.send_event(UserEvent::PhaseUpdate("rescuing".into()));
                                let req = stt::llm_corrector::LlmCorrectionRequest {
                                    primary_text: cur_txt,
                                    secondary_text: secondary_text.as_deref(),
                                    recent_messages: &recent_msgs,
                                    target_label: &target_label,
                                    game_name: game_name.as_deref(),
                                    dictionary: &dict,
                                    style: speech_style,
                                };
                                if let Ok(llm_out) =
                                    stt::llm_corrector::correct_transcript_with_llm(&accuracy.llm, &req)
                                {
                                    final_res = Ok(llm_out);
                                }
                            }
                        }
                    }

                    let infer_ms = t0.elapsed().as_millis() as u64;
                    let rtf = if duration_sec > 0.01 {
                        (infer_ms as f32 / 1000.0) / duration_sec
                    } else {
                        0.0
                    };
                    let engine_label = engine.name();
                    let device_name = engine.device_name();
                    if proxy
                        .send_event(UserEvent::TranscriptReady {
                            samples_16k: samples,
                            duration_sec,
                            peak_dbfs,
                            infer_ms,
                            rtf,
                            engine_label,
                            device_name,
                            result: final_res,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                SttWorkerMsg::TranscribePreview { samples, ctx } => {
                    if engine.is_ready() {
                        if let Ok(pv_txt) = engine.transcribe_with_context(&samples, &ctx) {
                            let clean = pv_txt.trim().to_string();
                            if !clean.is_empty() {
                                let _ = proxy.send_event(UserEvent::StreamingPreviewReady(clean));
                            }
                        }
                    }
                }
                SttWorkerMsg::ReloadEngine(new_engine) => {
                    engine = new_engine;
                }
                SttWorkerMsg::UpdatePromptHints(hints) => {
                    engine.set_prompt_hints(&hints);
                }
                SttWorkerMsg::ClearCaches => {
                    denoiser_cache = None;
                    secondary_cache = None;
                }
                SttWorkerMsg::UnloadAll => {
                    engine = Arc::new(BootLoadingEngine);
                    denoiser_cache = None;
                    secondary_cache = None;
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
    let guard = state.effective_guard();
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

fn start_send_countdown(
    text: String,
    press_enter: bool,
    action: &'static str,
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    proxy: EventLoopProxy<UserEvent>,
) {
    if let Some(prev) = state.pending_countdown.take() {
        prev.cancel_flag.store(true, Ordering::SeqCst);
    }
    let id = state.next_countdown_id;
    state.next_countdown_id += 1;
    let total_ms = state.config.send_countdown.duration_ms.clamp(300, 10_000);
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let preview: String = text.chars().take(60).collect();

    state.pending_countdown = Some(PendingSendCountdown {
        id,
        text,
        press_enter,
        action,
        cancel_flag: cancel_flag.clone(),
    });

    emit_all(
        main_wv,
        overlay_wv,
        &BackendEvent::SendCountdown {
            active: true,
            remaining_ms: total_ms,
            total_ms,
            preview_text: preview,
        },
    );

    thread::spawn(move || {
        let mut remaining = total_ms;
        while remaining > 0 {
            let step = remaining.min(100);
            thread::sleep(Duration::from_millis(step));
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }
            let esc_down = unsafe {
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
                (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0
            };
            if esc_down {
                let _ = proxy.send_event(UserEvent::SendCountdownCancelEsc { id });
                return;
            }
            remaining = remaining.saturating_sub(step);
            if remaining > 0 {
                let _ = proxy.send_event(UserEvent::SendCountdownTick {
                    id,
                    remaining_ms: remaining,
                    total_ms,
                });
            }
        }
        if !cancel_flag.load(Ordering::SeqCst) {
            let _ = proxy.send_event(UserEvent::SendCountdownFire { id });
        }
    });
}

fn cancel_send_countdown(
    state: &mut AppState,
    main_wv: &WebView,
    overlay_wv: &WebView,
    sound: &SoundPlayer,
    reason: &str,
) {
    let Some(pending) = state.pending_countdown.take() else {
        return;
    };
    pending.cancel_flag.store(true, Ordering::SeqCst);
    if state.config.input_mode == InputMode::Draft
        && !pending.text.is_empty()
        && state.draft_segments.is_empty()
    {
        state.draft_segments.push(pending.text);
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
    }
    sound.play(SoundKind::Cancel, &state.config.sound);
    emit_all(
        main_wv,
        overlay_wv,
        &BackendEvent::SendCountdown {
            active: false,
            remaining_ms: 0,
            total_ms: 0,
            preview_text: String::new(),
        },
    );
    emit_all(
        main_wv,
        overlay_wv,
        &BackendEvent::CommandExecuted {
            action: "countdown_cancelled".into(),
            detail: reason.to_string(),
        },
    );
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

fn toggle_panic_safe_mode(
    state: &mut AppState,
    audio_ctrl: &AudioController,
    tts_ctrl: &TtsQueueController,
    sound_player: &SoundPlayer,
    webview: &WebView,
    overlay_webview: &WebView,
    overlay_window: &tao::window::Window,
    tray: &Option<TrayManager>,
) {
    state.panic_mode = !state.panic_mode;
    if state.panic_mode {
        set_listening_state(
            false,
            false,
            state,
            audio_ctrl,
            sound_player,
            webview,
            overlay_webview,
            tray,
        );
        cancel_send_countdown(
            state,
            webview,
            overlay_webview,
            sound_player,
            "パニックセーフモード発動",
        );
        state.draft_segments.clear();
        tts_ctrl.stop_and_clear();
        state.overlay_visible = false;
        overlay_window.set_visible(false);
    } else {
        state.overlay_visible = state.config.overlay.enabled;
        overlay_window.set_visible(state.overlay_visible);
    }
    if let Some(ref t) = tray {
        t.set_panic_mode(state.panic_mode);
        t.update_state(state.tray_state(), state.overlay_visible);
    }
    emit_all(webview, overlay_webview, &state.status_event());
    emit(
        webview,
        &BackendEvent::Toast {
            level: if state.panic_mode { "warn" } else { "ok" }.into(),
            message: if state.panic_mode {
                "配信・画面共有セーフモード ON（マイク停止・HUD非表示・読み上げ停止・履歴マスク）"
            } else {
                "配信セーフモードを解除しました"
            }
            .into(),
        },
    );
}

fn emit_stt_models(state: &AppState, webview: &WebView) {
    let models = stt::local::list_available_ggml_models(
        &state.config.stt,
        state.runtime_dir.as_deref(),
    );
    let qwen3_installed = stt::local::has_qwen3_asr_model(
        &state.config.stt,
        state.runtime_dir.as_deref(),
    );
    let llm_models = stt::local::list_available_llm_gguf_models(
        &state.config.stt,
        state.runtime_dir.as_deref(),
    );
    let llama_server_path = stt::local::discover_llama_server_exe(
        &state.config.stt,
        state.runtime_dir.as_deref(),
    )
    .map(|p| p.display().to_string());
    emit(
        webview,
        &BackendEvent::SttModels {
            models,
            selected: state.config.stt.model_file.clone(),
            qwen3_installed,
            llm_models,
            selected_llm_model: state.config.stt.accuracy.llm.model_file.clone(),
            llama_server_path,
        },
    );
}

fn emit_catalog_inventory(state: &AppState, webview: &WebView) {
    let mut m_dirs = stt::local::resolve_model_search_dirs(
        &state.config.stt.model_dir,
        state.runtime_dir.as_deref(),
    );
    let dl_dir = stt::local::resolve_download_model_dir(
        &state.config.stt.model_dir,
        state.runtime_dir.as_deref(),
    );
    if !m_dirs.iter().any(|d| d == &dl_dir) {
        m_dirs.insert(0, dl_dir);
    }
    let data_dir = state.data_dir();
    let hw = gpu_diag::detect_hardware_profile(state.runtime_dir.as_deref());
    let merged = stt::catalog::load_merged_catalog(&data_dir);
    let bundles = stt::catalog::build_recommended_bundles_in_dirs(&hw, &m_dirs, &merged);
    let items = stt::catalog::build_catalog_inventory_in_dirs(
        &m_dirs,
        &data_dir,
        &state.config,
        &hw,
        &state.model_health_map,
    );
    emit(
        webview,
        &BackendEvent::CatalogInventory {
            hardware: hw,
            items,
            bundles,
            auto_apply_preset_on_download: state.config.auto_apply_preset_on_download,
            can_undo_preset: state.last_preset_snapshot.is_some(),
            manifest_url: state.config.catalog_manifest_url.clone(),
        },
    );
}

fn apply_model_auto_preset_for_state(
    state: &mut AppState,
    model_id: &str,
    audio_ctrl: &AudioController,
    webview: &WebView,
    overlay_webview: &WebView,
    proxy: &EventLoopProxy<UserEvent>,
) -> bool {
    let data_dir = state.data_dir();
    let mut m_dirs = stt::local::resolve_model_search_dirs(
        &state.config.stt.model_dir,
        state.runtime_dir.as_deref(),
    );
    let dl_dir = stt::local::resolve_download_model_dir(
        &state.config.stt.model_dir,
        state.runtime_dir.as_deref(),
    );
    if !m_dirs.iter().any(|d| d == &dl_dir) {
        m_dirs.insert(0, dl_dir);
    }
    let merged = stt::catalog::load_merged_catalog(&data_dir);
    let Some(resolved) = merged.iter().find(|r| r.entry.id == model_id) else {
        return false;
    };
    let (snap, summary) =
        stt::catalog::apply_preset_with_snapshot_in_dirs(&mut state.config, resolved, &m_dirs);
    state.last_preset_snapshot = Some(snap);
    let _ = state.config.save_to(&state.config_path);

    audio_ctrl.update_vad(state.config.vad.clone());
    audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
    spawn_stt_loader(
        state.config.stt.clone(),
        state.runtime_dir.clone(),
        proxy.clone(),
    );

    emit(
        webview,
        &BackendEvent::PresetAutoApplied {
            model_id: model_id.to_string(),
            summary: summary.clone(),
            can_undo: true,
        },
    );
    emit(
        webview,
        &BackendEvent::ConfigLoaded {
            config: state.config.clone(),
            config_path: state.config_path.display().to_string(),
        },
    );
    emit_all(webview, overlay_webview, &state.status_event());
    emit_catalog_inventory(state, webview);
    emit(
        webview,
        &BackendEvent::Toast {
            level: "ok".into(),
            message: summary,
        },
    );
    true
}

fn reload_stt_for_power_or_vram(
    state: &AppState,
    stt_tx: &mpsc::Sender<SttWorkerMsg>,
    proxy: &EventLoopProxy<UserEvent>,
) {
    let mut cfg = state.config.stt.clone();
    if state.vram_eco_offloaded {
        let _ = stt_tx.send(SttWorkerMsg::ClearCaches);
        let m_dir = stt::local::resolve_download_model_dir(&cfg.model_dir, state.runtime_dir.as_deref());
        let has_parakeet = m_dir
            .join("sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8")
            .is_dir();
        if has_parakeet {
            cfg.engine = "sherpa-onnx".into();
            cfg.model_file.clear();
        }
        cfg.threads = state.config.power_eco.eco_threads.clamp(1, 8);
    } else if state.eco_mode_active {
        cfg.threads = state.config.power_eco.eco_threads.clamp(1, 8);
    }
    spawn_stt_loader(cfg, state.runtime_dir.clone(), proxy.clone());
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
            let removed_all = state.draft_text();
            if !removed_all.trim().is_empty() {
                state.last_cancelled_raw = Some((removed_all, Instant::now()));
            }
            let (ymd, _) = current_local_ymd_hour();
            state.stats.record_cancel(&ymd, &state.stt_engine_name);
            state.stats.save_to(&state.stats_path);
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
            let (ymd, _) = current_local_ymd_hour();
            state.stats.record_cancel(&ymd, &state.stt_engine_name);
            state.stats.save_to(&state.stats_path);
            match state.config.input_mode {
                InputMode::Draft => {
                    let removed = state.draft_segments.pop().unwrap_or_default();
                    if !removed.trim().is_empty() {
                        state.last_cancelled_raw = Some((removed.clone(), Instant::now()));
                    }
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
                    state.record_sent_text(&full_text);
                    let active_id = state.config.discord_targets.active_target_id.clone();
                    let is_careful = active_id
                        .as_ref()
                        .map(|id| state.config.guard.careful_target_ids.iter().any(|c| c == id))
                        .unwrap_or(false);
                    if (state.config.send_countdown.enabled
                        && state.config.send_countdown.duration_ms >= 300)
                        || is_careful
                    {
                        let dur = if is_careful {
                            state.config.send_countdown.duration_ms.max(2500)
                        } else {
                            state.config.send_countdown.duration_ms
                        };
                        let orig = state.config.send_countdown.duration_ms;
                        state.config.send_countdown.duration_ms = dur;
                        start_send_countdown(
                            full_text,
                            true,
                            VoiceCommand::Send.as_str(),
                            state,
                            main_wv,
                            overlay_wv,
                            proxy.clone(),
                        );
                        state.config.send_countdown.duration_ms = orig;
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
                if state.config.send_countdown.enabled
                    && state.config.send_countdown.duration_ms >= 300
                    && !paste_text.trim().is_empty()
                {
                    start_send_countdown(
                        paste_text,
                        true,
                        VoiceCommand::Send.as_str(),
                        state,
                        main_wv,
                        overlay_wv,
                        proxy.clone(),
                    );
                } else {
                    spawn_send_for_state(
                        paste_text,
                        true,
                        VoiceCommand::Send.as_str(),
                        state,
                        proxy.clone(),
                    );
                }
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
        Some(VoiceCommand::DiscordUnmuteMic) => {
            spawn_key_async(
                KeyAction::DiscordUnmuteMic,
                "discord_unmute_mic",
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
        Some(VoiceCommand::DiscordUndeafen) => {
            spawn_key_async(
                KeyAction::DiscordUndeafen,
                "discord_undeafen",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordNavBack) => {
            spawn_key_async(
                KeyAction::DiscordNavBack,
                "discord_nav_back",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordNavForward) => {
            spawn_key_async(
                KeyAction::DiscordNavForward,
                "discord_nav_forward",
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
        Some(VoiceCommand::DiscordPrevUnread) => {
            spawn_key_async(
                KeyAction::DiscordPrevUnread,
                "discord_prev_unread",
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
        Some(VoiceCommand::DiscordJumpPresent) => {
            spawn_key_async(
                KeyAction::DiscordJumpPresent,
                "discord_jump_present",
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
        Some(VoiceCommand::DiscordCancelEscape) => {
            spawn_key_async(
                KeyAction::DiscordCancelEscape,
                "discord_cancel_escape",
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
        Some(VoiceCommand::DiscordTogglePins) => {
            spawn_key_async(
                KeyAction::DiscordTogglePins,
                "discord_toggle_pins",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordToggleInbox) => {
            spawn_key_async(
                KeyAction::DiscordToggleInbox,
                "discord_toggle_inbox",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordSearch) => {
            spawn_key_async(
                KeyAction::DiscordSearch,
                "discord_search",
                state.config.guard.clone(),
                proxy.clone(),
            );
        }
        Some(VoiceCommand::DiscordQuickSwitcher) => {
            spawn_key_async(
                KeyAction::DiscordQuickSwitcher,
                "discord_quick_switcher",
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
) -> String {
    // 0. If a pre-send countdown is active, allow spoken "待って" / "取り消し" / "キャンセル" to abort send!
    if state.pending_countdown.is_some() && parse_countdown_cancel(raw) {
        cancel_send_countdown(
            state,
            main_wv,
            overlay_wv,
            sound,
            "音声コマンドで送信をキャンセルしました",
        );
        return String::new();
    }

    // 1. If awaiting friend confirmation ("この人で合っていますか？"), handle Yes / No / Next / Prev
    //    if the user spoke a short confirmation reply; otherwise let normal dictation & commands work!
    if state.pending_confirm.is_some() {
        if let Some(reply) = parse_confirmation_reply(raw) {
            handle_confirmation_reply(reply, state, main_wv, overlay_wv, tray, sound, proxy);
            return String::new();
        }
    }

    // 1.5 Handle v1.6.0 special inline voice actions (#13 QuickReply, #14 PartialReplace, #25 Clipboard, #32 Target Lock)
    if !state.sleeping {
        if let Some(special) = parse_special_voice_action(raw) {
            match special {
                SpecialVoiceAction::LockTarget => {
                    state.config.guard.target_locked = true;
                    let _ = state.config.save_to(&state.config_path);
                    emit_all(main_wv, overlay_wv, &state.status_event());
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::CommandExecuted {
                            action: "target_lock".into(),
                            detail: "宛先をロックしました（音声での誤切替を防止）".into(),
                        },
                    );
                    return String::new();
                }
                SpecialVoiceAction::UnlockTarget => {
                    state.config.guard.target_locked = false;
                    let _ = state.config.save_to(&state.config_path);
                    emit_all(main_wv, overlay_wv, &state.status_event());
                    emit_all(
                        main_wv,
                        overlay_wv,
                        &BackendEvent::CommandExecuted {
                            action: "target_unlock".into(),
                            detail: "宛先ロックを解除しました".into(),
                        },
                    );
                    return String::new();
                }
                SpecialVoiceAction::PasteClipboard { press_enter } => {
                    spawn_send_for_state(
                        automation::input::copy_clipboard_read_fallback(),
                        press_enter,
                        "clipboard_paste",
                        state,
                        proxy.clone(),
                    );
                    return String::new();
                }
                SpecialVoiceAction::PartialReplace { from, to } => {
                    if let Some(last) = state.draft_segments.last_mut() {
                        if let Some(updated) = processor::phonetic::fuzzy_replace_segment(last, &from, &to) {
                            *last = updated.clone();
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
                                    action: "partial_replace".into(),
                                    detail: format!("部分修正: 「{}」→「{}」", from, to),
                                },
                            );
                            return updated;
                        }
                    }
                    emit(
                        main_wv,
                        &BackendEvent::Toast {
                            level: "info".into(),
                            message: format!("ドラフト内に「{}」に一致する部分が見つかりませんでした", from),
                        },
                    );
                    return String::new();
                }
                SpecialVoiceAction::QuickReplyDm(reply_body) => {
                    if let Some((sender, ts)) = state.last_dm_sender_info.clone() {
                        if ts.elapsed() <= Duration::from_secs(60) {
                            let (cands, _) =
                                discord::find_matching_targets(&sender, &state.config.discord_targets.targets);
                            if let Some(t) = cands.first().cloned() {
                                let eff_guard = state.effective_guard();
                                let integ = state.config.discord_integration.clone();
                                let delay = state.config.discord_targets.navigate_delay_ms;
                                let p = proxy.clone();
                                let msg_text = reply_body.clone();
                                thread::spawn(move || {
                                    let res = automation::send_to_discord_target(
                                        &msg_text,
                                        true,
                                        Some(&t),
                                        true,
                                        delay,
                                        &eff_guard,
                                        &integ,
                                    );
                                    let _ = p.send_event(match res {
                                        Ok(m) => UserEvent::AutomationResult {
                                            ok: true,
                                            action: "quick_reply".into(),
                                            message: m,
                                        },
                                        Err(e) => UserEvent::AutomationResult {
                                            ok: false,
                                            action: "quick_reply".into(),
                                            message: e,
                                        },
                                    });
                                });
                                return reply_body;
                            }
                        }
                    }
                    // Fallback: send as regular text if no recent DM target mapped
                    spawn_send_for_state(reply_body.clone(), true, "quick_reply", state, proxy.clone());
                    return reply_body;
                }
            }
        }
    }

    let eff_filter = state.effective_filter();
    let active_targets_cfg = if state.config.guard.target_locked {
        // When Target Lock is active (#32), do not allow passphrase target switching!
        let mut locked = state.config.discord_targets.clone();
        locked.switch_prefixes.clear();
        locked.switch_suffixes.clear();
        locked.reset_phrases.clear();
        locked.allow_standalone_passphrase = false;
        locked
    } else {
        state.config.discord_targets.clone()
    };
    let processed = processor::process_transcript_full(
        raw,
        &eff_filter,
        &state.config.commands,
        &active_targets_cfg,
        &state.config.snippets,
        &state.config.emoji_aliases,
        state.config.stt.accuracy.stutter_cleanup,
        state.config.stt.accuracy.phonetic_fuzzy_matching,
    );
    let final_text = processed.text.clone();

    // Auto re-speak habit learning (#5): if user cancelled within last 10s and now says a corrected phrase
    if state.config.stt.accuracy.auto_learn_respeak && !final_text.is_empty() && processed.command.is_none() {
        if let Some((cancelled, ts)) = state.last_cancelled_raw.take() {
            if ts.elapsed() <= Duration::from_secs(10) {
                if let Some(pair) = processor::phonetic::extract_respeak_habit_pair(&cancelled, &final_text) {
                    if !state.config.speech_habits.iter().any(|h| h.from == pair.from) {
                        state.stats.record_learning(&pair.from, &pair.to);
                        state.stats.save_to(&state.stats_path);
                        state.config.speech_habits.push(pair.clone());
                        let _ = state.config.save_to(&state.config_path);
                        emit(
                            main_wv,
                            &BackendEvent::Toast {
                                level: "ok".into(),
                                message: format!(
                                    "言い直しから発音クセを学習しました: 「{}」→「{}」",
                                    pair.from, pair.to
                                ),
                            },
                        );
                    }
                }
            }
        }
    }

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
        return String::new();
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
                            state.effective_guard(),
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
                    return final_text;
                } else if state.config.discord_targets.use_quick_switcher_for_unknown {
                    // Unregistered friend -> open Discord Quick Switcher (Ctrl+K) with @<name> and ask confirmation!
                    spawn_quick_switcher_open(
                        query.clone(),
                        state.effective_guard(),
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
                    return final_text;
                } else {
                    emit(
                        main_wv,
                        &BackendEvent::Toast {
                            level: "info".into(),
                            message: format!("「{}」に一致する宛先が見つかりませんでした", query),
                        },
                    );
                    return final_text;
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
    final_text
}

const MAIN_WINDOW_TITLE: &str = "VoxDrop — ハンズフリー音声チャット支援";

struct SingleInstanceGuard(windows::Win32::Foundation::HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            lp_mutex_attributes: *const core::ffi::c_void,
            b_initial_owner: i32,
            lp_name: *const u16,
        ) -> *mut core::ffi::c_void;
    }

    let mutex_name: Vec<u16> = "Local\\VoxDrop_SingleInstance_Mutex\0"
        .encode_utf16()
        .collect();
    unsafe {
        let raw_handle = CreateMutexW(core::ptr::null(), 0, mutex_name.as_ptr());
        if raw_handle.is_null() {
            return Some(SingleInstanceGuard(HANDLE::default()));
        }
        let handle = HANDLE(raw_handle);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            let title_w: Vec<u16> = format!("{MAIN_WINDOW_TITLE}\0").encode_utf16().collect();
            if let Ok(hwnd) = FindWindowW(None, PCWSTR(title_w.as_ptr())) {
                if !hwnd.0.is_null() {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            return None;
        }
        Some(SingleInstanceGuard(handle))
    }
}

fn main() -> wry::Result<()> {
    let Some(_instance_guard) = acquire_single_instance() else {
        return Ok(());
    };
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

    audio_ctrl.set_fallbacks(state.config.audio_device_fallbacks.clone());
    audio_ctrl.set_streaming_preview_ms(if state.config.stt.accuracy.streaming_preview_enabled {
        state.config.stt.accuracy.streaming_interval_ms
    } else {
        0
    });

    if state.config.stt.engine.eq_ignore_ascii_case("local-http")
        && state.config.stt.http_asr.auto_launch
    {
        spawn_http_asr_autostart(state.config.stt.http_asr.clone(), proxy.clone());
    }
    if state.config.stt.accuracy.llm.enabled && state.config.stt.accuracy.llm.auto_launch {
        let lcfg = state.config.stt.accuracy.llm.clone();
        let scfg = state.config.stt.clone();
        let rt_dir = state.runtime_dir.clone();
        let lp = proxy.clone();
        thread::spawn(move || {
            let res = stt::llm_corrector::ensure_llm_server_running_with_dirs(
                &lcfg,
                &scfg,
                rt_dir.as_deref(),
            );
            let _ = lp.send_event(UserEvent::LlmHealthDone(res));
        });
    }
    // Non-intrusive once-per-startup GitHub Releases update check (#33)
    {
        let up_proxy = proxy.clone();
        thread::spawn(move || {
            if let Some((latest_tag, url)) = stats::check_latest_release_non_blocking() {
                let _ = up_proxy.send_event(UserEvent::AppUpdateFound { latest_tag, url });
            }
        });
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
                    audio_ctrl.set_runtime_dir(state.runtime_dir.clone());
                }
                let hints = build_stt_prompt_hints(&state.config, state.active_profile());
                engine.set_prompt_hints(&hints);
                state.stt_ready = engine.is_ready();
                state.stt_engine_name = engine.name();
                state.stt_device_name = engine.device_name();
                let _ = stt_tx.send(SttWorkerMsg::ReloadEngine(engine));
                emit_stt_models(&state, &webview);
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
                            message: format!(
                                "STT準備完了: {} [{}]",
                                state.stt_engine_name, state.stt_device_name
                            ),
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
                    state.phase = "recognizing".into();
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
                AudioEvent::SpeechCancelled => {
                    state.recognizing = false;
                    state.phase = "idle".into();
                    if let Some(ref t) = tray {
                        t.update_state(state.tray_state(), state.overlay_visible);
                    }
                    emit_all(&webview, &overlay_webview, &state.status_event());
                }
                AudioEvent::UtterancePreview(samples) => {
                    if !state.panic_mode && !state.sleeping && state.config.stt.accuracy.streaming_preview_enabled {
                        let tgt_label = discord::target_display_label(
                            state.config.discord_targets.active_target(),
                        );
                        let hints = build_stt_prompt_hints_with_ctx(
                            &state.config,
                            state.active_profile(),
                            &state.recent_sent_strings(),
                            Some(&tgt_label),
                        );
                        let ctx = TranscribeContext {
                            language: state.config.stt.language.clone(),
                            hotwords_prompt: if hints.is_empty() { None } else { Some(hints) },
                            preset: state.config.stt.preset,
                            radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                            eq_strength: state.config.noise_cancel.presence_eq_strength,
                            head_pad_ms: state.config.stt.accuracy.head_silence_pad_ms,
                            tail_pad_ms: state.config.stt.accuracy.tail_silence_pad_ms,
                            beam_size: 2,
                            temperature_fallback: false,
                        };
                        let _ = stt_tx.send(SttWorkerMsg::TranscribePreview { samples, ctx });
                    }
                }
                AudioEvent::Utterance(samples) => {
                    if state.panic_mode {
                        state.recognizing = false;
                        return;
                    }
                    let recent_msgs = state.recent_sent_strings();
                    let tgt_label =
                        discord::target_display_label(state.config.discord_targets.active_target());
                    let hints = build_stt_prompt_hints_with_ctx(
                        &state.config,
                        state.active_profile(),
                        &recent_msgs,
                        Some(&tgt_label),
                    );
                    let eff_beam = if state.eco_mode_active || state.vram_eco_offloaded {
                        1
                    } else {
                        state.config.stt.accuracy.beam_size
                    };
                    let eff_preset = if state.night_mode_active {
                        config::ConversationPreset::Slow
                    } else {
                        state.config.stt.preset
                    };
                    let eff_eq = if state.night_mode_active {
                        state.config.noise_cancel.presence_eq_strength.max(0.65)
                    } else {
                        state.config.noise_cancel.presence_eq_strength
                    };
                    let ctx = TranscribeContext {
                        language: state.config.stt.language.clone(),
                        hotwords_prompt: if hints.is_empty() { None } else { Some(hints) },
                        preset: eff_preset,
                        radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                        eq_strength: eff_eq,
                        head_pad_ms: state.config.stt.accuracy.head_silence_pad_ms,
                        tail_pad_ms: state.config.stt.accuracy.tail_silence_pad_ms,
                        beam_size: eff_beam,
                        temperature_fallback: state.config.stt.accuracy.temperature_fallback,
                    };
                    let game_name = state.active_profile().map(|p| p.name.clone());
                    let eff_filter = state.effective_filter();
                    let style = state
                        .active_profile()
                        .and_then(|p| p.speech_style)
                        .unwrap_or(state.config.stt.accuracy.llm.style);
                    let _ = stt_tx.send(SttWorkerMsg::Transcribe {
                        samples,
                        ctx,
                        accuracy: state.config.stt.accuracy.clone(),
                        noise_stt: state.config.noise_cancel.stt_engine,
                        dpdfnet_limit_db: state.config.noise_cancel.dpdfnet_attenuation_limit_db,
                        stt_cfg: state.config.stt.clone(),
                        runtime_dir: state.runtime_dir.clone(),
                        recent_msgs,
                        target_label: tgt_label,
                        game_name,
                        dict: eff_filter.dictionary,
                        speech_style: style,
                    });
                }
                AudioEvent::DeviceSwitched { from, to } => {
                    state.config.audio_device = Some(to.clone());
                    let _ = state.config.save_to(&state.config_path);
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "info".into(),
                            message: format!(
                                "マイク自動切替 (#18): {} → {}",
                                from.as_deref().unwrap_or("既定"),
                                to
                            ),
                        },
                    );
                }
                AudioEvent::Error(err) => {
                    state.listening = false;
                    state.recognizing = false;
                    state.ptt_active = false;
                    state.phase = "idle".into();
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
            Event::UserEvent(UserEvent::PhaseUpdate(ph)) => {
                state.phase = ph;
                emit_all(&webview, &overlay_webview, &state.status_event());
            }
            Event::UserEvent(UserEvent::StreamingPreviewReady(pv_text)) => {
                if !state.panic_mode {
                    emit_all(
                        &webview,
                        &overlay_webview,
                        &BackendEvent::StreamingPreview { draft: pv_text },
                    );
                }
            }
            Event::UserEvent(UserEvent::LlmHealthDone(res)) => match res {
                Ok(msg) => {
                    emit(&webview, &BackendEvent::LlmHealthResult { ok: true, message: msg.clone() });
                    emit(&webview, &BackendEvent::Toast { level: "ok".into(), message: msg });
                }
                Err(err) => {
                    emit(&webview, &BackendEvent::LlmHealthResult { ok: false, message: err.clone() });
                    emit(&webview, &BackendEvent::Toast { level: "error".into(), message: err });
                }
            },
            Event::UserEvent(UserEvent::LlmSampleDone {
                ok,
                input,
                output,
                elapsed_ms,
                error,
            }) => {
                emit(
                    &webview,
                    &BackendEvent::LlmSampleResult {
                        ok,
                        input,
                        output,
                        elapsed_ms,
                        error,
                    },
                );
            }
            Event::UserEvent(UserEvent::AppUpdateFound { latest_tag, url }) => {
                emit(
                    &webview,
                    &BackendEvent::AppUpdateAvailable {
                        current_tag: stats::CURRENT_APP_TAG.into(),
                        latest_tag: latest_tag.clone(),
                        url,
                    },
                );
                emit(
                    &webview,
                    &BackendEvent::Toast {
                        level: "info".into(),
                        message: format!("新しいバージョン {} が公開されています", latest_tag),
                    },
                );
            }
            Event::UserEvent(UserEvent::TranscriptReady {
                samples_16k,
                duration_sec,
                peak_dbfs,
                infer_ms,
                rtf,
                engine_label,
                device_name,
                result,
            }) => {
                state.recognizing = false;
                state.phase = "idle".into();
                state.last_infer_ms = infer_ms;
                state.last_rtf = rtf;
                if !device_name.is_empty() {
                    state.stt_device_name = device_name.clone();
                }
                let (ymd, _) = current_local_ymd_hour();
                state
                    .stats
                    .record_utterance(&ymd, &engine_label, &device_name, infer_ms);
                state.stats.save_to(&state.stats_path);
                emit(&webview, &BackendEvent::StatsUpdated { summary: state.stats.summary() });

                if let Some(ref t) = tray {
                    t.update_state(state.tray_state(), state.overlay_visible);
                }
                emit_all(&webview, &overlay_webview, &state.status_event());

                match result {
                    Ok(raw) => {
                        let trimmed = raw.trim().to_string();
                        if state.calibration_wizard_active {
                            state.calibration_wizard_active = false;
                            let tempo = crate::audio::enhance::estimate_speech_tempo_score(&samples_16k);
                            let hw = gpu_diag::detect_hardware_profile(state.runtime_dir.as_deref());
                            let rec_vad = if peak_dbfs < -24.0 { 0.007 } else if peak_dbfs < -14.0 { 0.010 } else { 0.012 };
                            let rec_eq = if peak_dbfs < -18.0 || tempo >= 4.0 { 0.70 } else { 0.55 };
                            let rec_wsola = tempo >= 3.8;
                            let rec_noise = if peak_dbfs < -22.0 && !hw.is_on_battery {
                                "dpdfnet-16k"
                            } else if hw.is_on_battery {
                                "gtcrn-16k"
                            } else {
                                hw.recommended_stt_noise.as_str()
                            }
                            .to_string();
                            let note = format!(
                                "診断完了: ピーク {:.1} dBFS / 話速スコア {:.1} / 認識「{}」",
                                peak_dbfs, tempo, trimmed
                            );
                            emit(
                                &webview,
                                &BackendEvent::CalibrationWizardResult {
                                    score_note: note,
                                    recommended_vad: rec_vad,
                                    recommended_eq: rec_eq,
                                    recommended_wsola: rec_wsola,
                                    recommended_model: hw.recommended_stt_model,
                                    recommended_noise_engine: rec_noise,
                                },
                            );
                            return;
                        }
                        let final_text = if !trimmed.is_empty() {
                            handle_transcript(
                                &trimmed,
                                &mut state,
                                &webview,
                                &overlay_webview,
                                &tray,
                                &sound_player,
                                &proxy,
                            )
                        } else {
                            String::new()
                        };
                        state.push_diagnostic(
                            samples_16k,
                            duration_sec,
                            peak_dbfs,
                            infer_ms,
                            rtf,
                            device_name,
                            engine_label,
                            trimmed,
                            final_text,
                        );
                        emit(
                            &webview,
                            &BackendEvent::DiagnosticsUpdated {
                                items: state.diagnostic_dtos(),
                            },
                        );
                    }
                    Err(err) => {
                        state.push_diagnostic(
                            samples_16k,
                            duration_sec,
                            peak_dbfs,
                            infer_ms,
                            rtf,
                            device_name,
                            engine_label,
                            format!("[エラー] {}", err),
                            String::new(),
                        );
                        emit(
                            &webview,
                            &BackendEvent::DiagnosticsUpdated {
                                items: state.diagnostic_dtos(),
                            },
                        );
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
            Event::UserEvent(UserEvent::AbCompareDone {
                id,
                engine_label,
                device_name,
                infer_ms,
                rtf,
                result,
            }) => match result {
                Ok(text) => {
                    emit(
                        &webview,
                        &BackendEvent::AbCompareResult {
                            id,
                            ok: true,
                            engine_label,
                            device_name,
                            infer_ms,
                            rtf,
                            text,
                            error: None,
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::AbCompareResult {
                            id,
                            ok: false,
                            engine_label,
                            device_name,
                            infer_ms,
                            rtf,
                            text: String::new(),
                            error: Some(err),
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::SendCountdownTick {
                id,
                remaining_ms,
                total_ms,
            }) => {
                if let Some(ref pending) = state.pending_countdown {
                    if pending.id == id {
                        let preview: String = pending.text.chars().take(60).collect();
                        emit_all(
                            &webview,
                            &overlay_webview,
                            &BackendEvent::SendCountdown {
                                active: true,
                                remaining_ms,
                                total_ms,
                                preview_text: preview,
                            },
                        );
                    }
                }
            }
            Event::UserEvent(UserEvent::SendCountdownFire { id }) => {
                let matches_id = state
                    .pending_countdown
                    .as_ref()
                    .map(|p| p.id == id)
                    .unwrap_or(false);
                if matches_id {
                    if let Some(pending) = state.pending_countdown.take() {
                        emit_all(
                            &webview,
                            &overlay_webview,
                            &BackendEvent::SendCountdown {
                                active: false,
                                remaining_ms: 0,
                                total_ms: 0,
                                preview_text: String::new(),
                            },
                        );
                        spawn_send_for_state(
                            pending.text,
                            pending.press_enter,
                            pending.action,
                            &state,
                            proxy.clone(),
                        );
                    }
                }
            }
            Event::UserEvent(UserEvent::SendCountdownCancelEsc { id }) => {
                let matches_id = state
                    .pending_countdown
                    .as_ref()
                    .map(|p| p.id == id)
                    .unwrap_or(false);
                if matches_id {
                    cancel_send_countdown(
                        &mut state,
                        &webview,
                        &overlay_webview,
                        &sound_player,
                        "Escキーで送信をキャンセルしました",
                    );
                }
            }
            Event::UserEvent(UserEvent::ModelDownloadProgress {
                model_id,
                progress,
                message,
                done,
                ok,
            }) => {
                emit(
                    &webview,
                    &BackendEvent::ModelDownloadProgress {
                        model_id: model_id.clone(),
                        progress,
                        message: message.clone(),
                        done,
                        ok,
                    },
                );
                if done {
                    emit_stt_models(&state, &webview);
                    let err_msg = message.clone();
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: if ok { "ok" } else { "error" }.into(),
                            message,
                        },
                    );
                    if !ok
                        && (err_msg.contains("404")
                            || err_msg.contains("HTMLエラーページ")
                            || err_msg.contains("未知のモデルID")
                            || err_msg.contains("見つかりません"))
                    {
                        let data_dir = state.data_dir();
                        let broken_id = model_id.clone();
                        let p = proxy.clone();
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "info".into(),
                                message: format!(
                                    "「{}」が見つからないため、最新・代替モデルを自動探索しています...",
                                    broken_id
                                ),
                            },
                        );
                        thread::spawn(move || {
                            let res = stt::catalog::discover_latest_and_alternative_models_winhttp(
                                &data_dir,
                                Some(&broken_id),
                            );
                            let _ = p.send_event(UserEvent::ModelsDiscoveredDone(res));
                        });
                    }
                    if ok && state.config.auto_apply_preset_on_download {
                        let applied = apply_model_auto_preset_for_state(
                            &mut state,
                            &model_id,
                            &audio_ctrl,
                            &webview,
                            &overlay_webview,
                            &proxy,
                        );
                        if !applied {
                            emit_catalog_inventory(&state, &webview);
                        }
                    } else {
                        emit_catalog_inventory(&state, &webview);
                    }
                }
            }
            Event::UserEvent(UserEvent::UrlHealthCheckDone(results)) => {
                let mut ok_cnt = 0usize;
                let mut mir_cnt = 0usize;
                let mut ng_cnt = 0usize;
                for r in &results {
                    match r.status.as_str() {
                        "reachable" => ok_cnt += 1,
                        "mirror_reachable" => mir_cnt += 1,
                        "unreachable" => ng_cnt += 1,
                        _ => {}
                    }
                    state.model_health_map.insert(r.model_id.clone(), r.clone());
                }
                emit(&webview, &BackendEvent::ModelUrlHealthUpdated { results });
                emit_catalog_inventory(&state, &webview);
                emit(
                    &webview,
                    &BackendEvent::Toast {
                        level: if ng_cnt > 0 { "warn" } else { "ok" }.into(),
                        message: format!(
                            "URL疎通診断完了: 取得可 {}件 / ミラー {}件 / 不可 {}件",
                            ok_cnt, mir_cnt, ng_cnt
                        ),
                    },
                );
            }
            Event::UserEvent(UserEvent::OnlineCatalogSyncDone(res)) => match res {
                Ok((updated_urls, added_models, summary)) => {
                    emit(
                        &webview,
                        &BackendEvent::OnlineCatalogSynced {
                            updated_urls,
                            added_models,
                            summary: summary.clone(),
                        },
                    );
                    emit_catalog_inventory(&state, &webview);
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "ok".into(),
                            message: summary,
                        },
                    );
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
            },
            Event::UserEvent(UserEvent::ModelsDiscoveredDone(res)) => match res {
                Ok(report) => {
                    emit(
                        &webview,
                        &BackendEvent::ModelsDiscovered {
                            ok: true,
                            added: report.added_models.clone(),
                            updated: report.updated_models.clone(),
                            summary: report.summary.clone(),
                        },
                    );
                    emit_catalog_inventory(&state, &webview);
                    emit_stt_models(&state, &webview);
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "ok".into(),
                            message: report.summary,
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::ModelsDiscovered {
                            ok: false,
                            added: Vec::new(),
                            updated: Vec::new(),
                            summary: err.clone(),
                        },
                    );
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::HttpAsrHealthDone(res)) => match res {
                Ok(msg) => {
                    emit(
                        &webview,
                        &BackendEvent::HttpAsrHealthResult {
                            ok: true,
                            message: msg.clone(),
                        },
                    );
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "ok".into(),
                            message: msg,
                        },
                    );
                }
                Err(err) => {
                    emit(
                        &webview,
                        &BackendEvent::HttpAsrHealthResult {
                            ok: false,
                            message: err.clone(),
                        },
                    );
                    emit(
                        &webview,
                        &BackendEvent::Toast {
                            level: "error".into(),
                            message: err,
                        },
                    );
                }
            },
            Event::UserEvent(UserEvent::ActiveWindowChanged {
                process,
                target_active,
            }) => {
                state.active_process = process.clone();
                state.target_active = target_active;
                let prev_profile = state.active_game_profile_id.clone();
                if let Some(matched) =
                    find_matching_game_profile(&process, &state.config.game_profiles)
                {
                    state.active_game_profile_id = Some(matched.id.clone());
                } else if !target_active {
                    state.active_game_profile_id = None;
                }
                if state.active_game_profile_id != prev_profile {
                    state.sync_guard();
                    let hints = build_stt_prompt_hints_with_ctx(
                        &state.config,
                        state.active_profile(),
                        &state.recent_sent_strings(),
                        None,
                    );
                    let _ = stt_tx.send(SttWorkerMsg::UpdatePromptHints(hints));
                    if let Some(p) = state.active_profile().cloned() {
                        if let (Some(x), Some(y)) = (p.overlay_x, p.overlay_y) {
                            overlay_window.set_outer_position(LogicalPosition::new(x as f64, y as f64));
                        }
                        if let Some(op) = p.overlay_opacity {
                            emit(
                                &overlay_webview,
                                &BackendEvent::OverlayStyle {
                                    opacity: op.clamp(0.2, 1.0),
                                },
                            );
                        }
                    } else {
                        overlay_window.set_outer_position(LogicalPosition::new(
                            state.config.overlay.x as f64,
                            state.config.overlay.y as f64,
                        ));
                        emit(
                            &overlay_webview,
                            &BackendEvent::OverlayStyle {
                                opacity: state.config.overlay.opacity.clamp(0.2, 1.0),
                            },
                        );
                    }
                }
                // #26 Night Mode Auto-Schedule + #16/#30 Periodic VRAM Watchdog & Battery Eco
                state.win_poll_ticks = state.win_poll_ticks.wrapping_add(1);
                if state.config.night_mode.enabled {
                    let (_, hour) = current_local_ymd_hour();
                    let s = state.config.night_mode.start_hour;
                    let e = state.config.night_mode.end_hour;
                    let in_night = if s <= e {
                        hour >= s && hour < e
                    } else {
                        hour >= s || hour < e
                    };
                    if in_night != state.night_mode_active {
                        state.night_mode_active = in_night;
                        if in_night {
                            let mut vad = state.config.vad.clone();
                            vad.threshold = vad.threshold.min(0.008);
                            vad.silence_ms = vad.silence_ms.max(800);
                            audio_ctrl.update_vad(vad);
                            let mut night_tts = state.config.discord_tts.clone();
                            night_tts.voicevox.volume_scale = (night_tts.voicevox.volume_scale
                                * state.config.night_mode.tts_volume_scale)
                                .clamp(0.05, 2.0);
                            tts_ctrl.update_config(night_tts);
                        } else {
                            audio_ctrl.update_vad(state.config.vad.clone());
                            tts_ctrl.update_config(state.config.discord_tts.clone());
                        }
                    }
                } else {
                    if state.night_mode_active {
                        state.night_mode_active = false;
                        audio_ctrl.update_vad(state.config.vad.clone());
                        tts_ctrl.update_config(state.config.discord_tts.clone());
                    }
                }
                if state.win_poll_ticks % 20 == 1 {
                    let prev_eco = state.eco_mode_active;
                    let prev_vram = state.vram_eco_offloaded;
                    if state.config.power_eco.auto_battery_eco {
                        let (on_bat, _) = gpu_diag::query_power_status();
                        state.eco_mode_active = on_bat;
                    } else {
                        state.eco_mode_active = false;
                    }
                    if state.config.power_eco.vram_watchdog_enabled {
                        if let Some(adp) = gpu_diag::query_best_dxgi_adapter() {
                            if let Some(pct) = adp.vram_usage_pct {
                                let th = state.config.power_eco.vram_threshold_pct;
                                state.vram_eco_offloaded = if prev_vram {
                                    pct >= th.saturating_sub(10)
                                } else {
                                    pct >= th
                                };
                            }
                        }
                    } else {
                        state.vram_eco_offloaded = false;
                    }
                    if prev_eco != state.eco_mode_active || prev_vram != state.vram_eco_offloaded {
                        reload_stt_for_power_or_vram(&state, &stt_tx, &proxy);
                        let msg = if state.vram_eco_offloaded {
                            "VRAM高負荷を検知：STTを軽量/省メモリ構成へ退避しました"
                        } else if state.eco_mode_active {
                            "バッテリー駆動を検知：省電力Ecoスレッド設定へ切り替えました"
                        } else {
                            "電源/VRAM回復：通常STT構成へ復帰しました"
                        };
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: if state.vram_eco_offloaded || state.eco_mode_active {
                                    "warn"
                                } else {
                                    "ok"
                                }
                                .into(),
                                message: msg.into(),
                            },
                        );
                    }
                }
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
                    for code in [
                        "profile_modal_stuck",
                        "composer_not_found",
                        "focus_failed",
                        "text_not_inserted",
                        "enter_not_sent",
                    ] {
                        if message.contains(code) {
                            emit_all(
                                &webview,
                                &overlay_webview,
                                &BackendEvent::PostSendError {
                                    code: code.into(),
                                    detail: message.clone(),
                                },
                            );
                            break;
                        }
                    }
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
                if status.is_manual_test {
                    if let Some(ref err) = status.last_error {
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "error".into(),
                                message: format!("テスト読み上げ失敗: {}", err),
                            },
                        );
                    }
                }
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
                    state.last_dm_sender_info = Some((sender.clone(), Instant::now()));
                    if !state.panic_mode {
                        tts_ctrl.enqueue(speech_text, false);
                    }
                    if state.config.discord_tts.debug_log_toasts && !state.panic_mode {
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
                HotkeyAction::TogglePanicSafeMode => {
                    toggle_panic_safe_mode(
                        &mut state,
                        &audio_ctrl,
                        &tts_ctrl,
                        &sound_player,
                        &webview,
                        &overlay_webview,
                        &overlay_window,
                        &tray,
                    );
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
                TrayAction::TogglePanicSafeMode => {
                    toggle_panic_safe_mode(
                        &mut state,
                        &audio_ctrl,
                        &tts_ctrl,
                        &sound_player,
                        &webview,
                        &overlay_webview,
                        &overlay_window,
                        &tray,
                    );
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
                    if state.config.stt.http_asr.stop_on_exit {
                        stt::http_asr::stop_http_asr_if_started_by_us();
                    }
                    if state.config.stt.accuracy.llm.stop_on_exit {
                        stt::llm_corrector::stop_llm_server_if_started_by_us();
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
                        emit(
                            &webview,
                            &BackendEvent::DiagnosticsUpdated {
                                items: state.diagnostic_dtos(),
                            },
                        );
                        emit(
                            &webview,
                            &BackendEvent::StatsUpdated {
                                summary: state.stats.summary(),
                            },
                        );
                        emit(
                            &webview,
                            &BackendEvent::BenchmarkCasesUpdated {
                                cases: state.bench_cases.clone(),
                            },
                        );
                        emit_catalog_inventory(&state, &webview);
                        emit(
                            &overlay_webview,
                            &BackendEvent::OverlayStyle {
                                opacity: state.config.overlay.opacity.clamp(0.2, 1.0),
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
                        let stt_changed = state.config.stt.engine != config.stt.engine
                            || state.config.stt.model_dir != config.stt.model_dir
                            || state.config.stt.model_file != config.stt.model_file
                            || state.config.stt.cli_path != config.stt.cli_path
                            || state.config.stt.language != config.stt.language
                            || state.config.stt.threads != config.stt.threads
                            || state.config.stt.http_asr != config.stt.http_asr;
                        let hotkeys_changed = state.config.hotkeys != config.hotkeys;
                        let autostart_changed = state.config.auto_start != config.auto_start;
                        let should_launch_http_asr = config.stt.engine.eq_ignore_ascii_case("local-http")
                            && config.stt.http_asr.auto_launch
                            && (!state.config.stt.engine.eq_ignore_ascii_case("local-http")
                                || !state.config.stt.http_asr.auto_launch
                                || state.config.stt.http_asr.server_exe_path
                                    != config.stt.http_asr.server_exe_path
                                || state.config.stt.http_asr.base_url
                                    != config.stt.http_asr.base_url);
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
                        emit(
                            &overlay_webview,
                            &BackendEvent::OverlayStyle {
                                opacity: state.config.overlay.opacity.clamp(0.2, 1.0),
                            },
                        );
                        state.active_game_profile_id = find_matching_game_profile(
                            &state.active_process,
                            &state.config.game_profiles,
                        )
                        .map(|p| p.id.clone());
                        state.sync_guard();
                        state.clipboard_watch_flag.store(
                            state.config.discord_integration.clipboard_url_watch,
                            Ordering::Relaxed,
                        );
                        audio_ctrl.set_device(state.config.audio_device.clone());
                        audio_ctrl.set_fallbacks(state.config.audio_device_fallbacks.clone());
                        audio_ctrl.update_vad(state.config.vad.clone());
                        audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                        audio_ctrl.set_streaming_preview_ms(
                            if state.config.stt.accuracy.streaming_preview_enabled {
                                state.config.stt.accuracy.streaming_interval_ms
                            } else {
                                0
                            },
                        );
                        tts_ctrl.update_config(state.config.discord_tts.clone());
                        notif_ctrl.update_config(state.config.discord_tts.clone());
                        if should_launch_voicevox {
                            spawn_voicevox_autostart(
                                state.config.discord_tts.voicevox.base_url.clone(),
                                state.config.discord_tts.voicevox.exe_path.clone(),
                                proxy.clone(),
                            );
                        }
                        if should_launch_http_asr {
                            spawn_http_asr_autostart(state.config.stt.http_asr.clone(), proxy.clone());
                        }
                        if state.config.stt.accuracy.llm.enabled
                            && state.config.stt.accuracy.llm.auto_launch
                        {
                            let lcfg = state.config.stt.accuracy.llm.clone();
                            let scfg = state.config.stt.clone();
                            let rt_dir = state.runtime_dir.clone();
                            let lp = proxy.clone();
                            thread::spawn(move || {
                                let res = stt::llm_corrector::ensure_llm_server_running_with_dirs(
                                    &lcfg,
                                    &scfg,
                                    rt_dir.as_deref(),
                                );
                                let _ = lp.send_event(UserEvent::LlmHealthDone(res));
                            });
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
                        } else {
                            let hints =
                                build_stt_prompt_hints(&state.config, state.active_profile());
                            let _ = stt_tx.send(SttWorkerMsg::UpdatePromptHints(hints));
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
                        if state.config.activation_mode == ActivationMode::PushToTalk
                            && !state.ptt_active
                            && state.listening
                        {
                            set_listening_state(
                                false,
                                false,
                                &mut state,
                                &audio_ctrl,
                                &sound_player,
                                &webview,
                                &overlay_webview,
                                &tray,
                            );
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
                            "unmute_mic" => KeyAction::DiscordUnmuteMic,
                            "deafen" => KeyAction::DiscordDeafen,
                            "undeafen" => KeyAction::DiscordUndeafen,
                            "nav_back" => KeyAction::DiscordNavBack,
                            "nav_forward" => KeyAction::DiscordNavForward,
                            "next_channel" => KeyAction::DiscordNextChannel,
                            "prev_channel" => KeyAction::DiscordPrevChannel,
                            "prev_unread" => KeyAction::DiscordPrevUnread,
                            "next_unread" => KeyAction::DiscordNextUnread,
                            "next_server" => KeyAction::DiscordNextServer,
                            "prev_server" => KeyAction::DiscordPrevServer,
                            "scroll_up" => KeyAction::DiscordScrollUp,
                            "scroll_down" => KeyAction::DiscordScrollDown,
                            "jump_present" => KeyAction::DiscordJumpPresent,
                            "edit_last" => KeyAction::DiscordEditLast,
                            "enter" => KeyAction::Enter,
                            "cancel_escape" => KeyAction::DiscordCancelEscape,
                            "mark_read" => KeyAction::DiscordMarkRead,
                            "toggle_members" => KeyAction::DiscordToggleMembers,
                            "toggle_pins" => KeyAction::DiscordTogglePins,
                            "toggle_inbox" => KeyAction::DiscordToggleInbox,
                            "quick_switcher" => KeyAction::DiscordQuickSwitcher,
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
                            if state.config.send_countdown.enabled
                                && state.config.send_countdown.duration_ms >= 300
                            {
                                start_send_countdown(
                                    text,
                                    true,
                                    "send_draft",
                                    &mut state,
                                    &webview,
                                    &overlay_webview,
                                    proxy.clone(),
                                );
                            } else {
                                spawn_send_for_state(text, true, "send_draft", &state, proxy.clone());
                            }
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
                    ClientCommand::ListSttModels => {
                        emit_stt_models(&state, &webview);
                    }
                    ClientCommand::ReplayUtterance { id } => {
                        if let Some(entry) = state.recent_utterances.iter().find(|u| u.id == id) {
                            let wav = stt::local::encode_wav_16k_mono(&entry.samples_16k);
                            sound_player.play_wav_bytes(wav, 1.0);
                        } else {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "info".into(),
                                    message: "指定された音声データはメモリにありません".into(),
                                },
                            );
                        }
                    }
                    ClientCommand::AbCompareUtterance {
                        id,
                        engine,
                        model_file,
                    } => {
                        if let Some(entry) = state.recent_utterances.iter().find(|u| u.id == id) {
                            let samples = entry.samples_16k.clone();
                            let duration_sec = entry.duration_sec;
                            let mut ab_cfg = state.config.stt.clone();
                            ab_cfg.engine = engine.clone();
                            ab_cfg.model_file = model_file.clone();
                            let rt_dir = state.runtime_dir.clone();
                            let hints =
                                build_stt_prompt_hints(&state.config, state.active_profile());
                            let ctx = TranscribeContext {
                                language: ab_cfg.language.clone(),
                                hotwords_prompt: if hints.is_empty() {
                                    None
                                } else {
                                    Some(hints.clone())
                                },
                                preset: ab_cfg.preset,
                                radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                                ..Default::default()
                            };
                            let p = proxy.clone();
                            thread::spawn(move || {
                                let cache_key = format!(
                                    "{}|{}|{}|{}|{}",
                                    ab_cfg.engine,
                                    ab_cfg.model_file,
                                    ab_cfg.model_dir,
                                    ab_cfg.language,
                                    ab_cfg.http_asr.base_url
                                );
                                let eng = {
                                    let mut cache = AB_ENGINE_CACHE.lock().ok();
                                    let hit = cache
                                        .as_ref()
                                        .and_then(|g| g.as_ref())
                                        .filter(|(k, _)| k == &cache_key)
                                        .map(|(_, e)| e.clone());
                                    if let Some(e) = hit {
                                        e
                                    } else {
                                        let created = stt::create_engine(&ab_cfg, rt_dir.as_deref());
                                        if let Some(ref mut g) = cache {
                                            **g = Some((cache_key, created.clone()));
                                        }
                                        created
                                    }
                                };
                                eng.set_prompt_hints(&hints);
                                let engine_label = eng.name();
                                let device_name = eng.device_name();
                                let t0 = Instant::now();
                                let res = eng.transcribe_with_context(&samples, &ctx);
                                let infer_ms = t0.elapsed().as_millis() as u64;
                                let rtf = if duration_sec > 0.01 {
                                    (infer_ms as f32 / 1000.0) / duration_sec
                                } else {
                                    0.0
                                };
                                let _ = p.send_event(UserEvent::AbCompareDone {
                                    id,
                                    engine_label,
                                    device_name,
                                    infer_ms,
                                    rtf,
                                    result: res,
                                });
                            });
                        }
                    }
                    ClientCommand::ClearDiagnostics => {
                        state.recent_utterances.clear();
                        emit(
                            &webview,
                            &BackendEvent::DiagnosticsUpdated { items: Vec::new() },
                        );
                    }
                    ClientCommand::QuickLearnCorrection { from, to } => {
                        let f = from.trim().to_string();
                        let t = to.trim().to_string();
                        if f.is_empty() || t.is_empty() {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "error".into(),
                                    message: "誤認識語句と正しい表記の両方を入力してください".into(),
                                },
                            );
                        } else {
                            if let Some(existing) = state
                                .config
                                .filter
                                .dictionary
                                .iter_mut()
                                .find(|e| e.from.trim() == f)
                            {
                                existing.to = t.clone();
                            } else {
                                state.config.filter.dictionary.push(DictionaryEntry {
                                    from: f.clone(),
                                    to: t.clone(),
                                });
                            }
                            let _ = state.config.save_to(&state.config_path);
                            let hints =
                                build_stt_prompt_hints(&state.config, state.active_profile());
                            let _ = stt_tx.send(SttWorkerMsg::UpdatePromptHints(hints));
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
                                    message: format!("辞書＆ヒントに登録しました: 「{}」→「{}」", f, t),
                                },
                            );
                        }
                    }
                    ClientCommand::CancelSendCountdown => {
                        cancel_send_countdown(
                            &mut state,
                            &webview,
                            &overlay_webview,
                            &sound_player,
                            "送信前カウントダウンを中止しました",
                        );
                    }
                    ClientCommand::DownloadModel {
                        model_id,
                        custom_url,
                        custom_filename,
                    } => {
                        let target_dir = stt::local::resolve_download_model_dir(
                            &state.config.stt.model_dir,
                            state.runtime_dir.as_deref(),
                        );
                        let data_dir = state.data_dir();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::downloader::download_model_into_with_data(
                                &model_id,
                                custom_url.as_deref(),
                                custom_filename.as_deref(),
                                &target_dir,
                                &data_dir,
                                |mid, pct, msg| {
                                    let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                        model_id: mid.to_string(),
                                        progress: pct,
                                        message: msg.to_string(),
                                        done: false,
                                        ok: true,
                                    });
                                },
                            );
                            match res {
                                Ok(path) => {
                                    let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                        model_id,
                                        progress: 100,
                                        message: format!(
                                            "モデル保存完了: {}",
                                            path.display()
                                        ),
                                        done: true,
                                        ok: true,
                                    });
                                }
                                Err(err) => {
                                    let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                        model_id,
                                        progress: 0,
                                        message: err,
                                        done: true,
                                        ok: false,
                                    });
                                }
                            }
                        });
                    }
                    ClientCommand::TestHttpAsr { base_url } => {
                        let mut cfg = state.config.stt.http_asr.clone();
                        if let Some(u) = base_url {
                            if !u.trim().is_empty() {
                                cfg.base_url = u.trim().to_string();
                            }
                        }
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::http_asr::check_http_asr_health(&cfg);
                            let _ = p.send_event(UserEvent::HttpAsrHealthDone(res));
                        });
                    }
                    ClientCommand::LaunchHttpAsr => {
                        let cfg = state.config.stt.http_asr.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::http_asr::ensure_http_asr_running(&cfg);
                            let _ = p.send_event(UserEvent::HttpAsrHealthDone(res));
                        });
                    }
                    ClientCommand::TestLlmCorrector { base_url } => {
                        let mut u = state.config.stt.accuracy.llm.base_url.clone();
                        if let Some(ov) = base_url {
                            if !ov.trim().is_empty() {
                                u = ov.trim().to_string();
                            }
                        }
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::llm_corrector::check_llm_health(&u);
                            let _ = p.send_event(UserEvent::LlmHealthDone(res));
                        });
                    }
                    ClientCommand::LaunchLlmServer => {
                        let cfg = state.config.stt.accuracy.llm.clone();
                        let scfg = state.config.stt.clone();
                        let rt_dir = state.runtime_dir.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::llm_corrector::ensure_llm_server_running_with_dirs(
                                &cfg,
                                &scfg,
                                rt_dir.as_deref(),
                            );
                            let _ = p.send_event(UserEvent::LlmHealthDone(res));
                        });
                    }
                    ClientCommand::TestLlmSample { text } => {
                        let mut cfg = state.config.stt.accuracy.llm.clone();
                        cfg.enabled = true;
                        let dict = state.effective_filter().dictionary;
                        let tgt_label = discord::target_display_label(
                            state.config.discord_targets.active_target(),
                        );
                        let game_name = state.active_profile().map(|p| p.name.clone());
                        let recent = state.recent_sent_strings();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let t0 = Instant::now();
                            let req = stt::llm_corrector::LlmCorrectionRequest {
                                primary_text: &text,
                                secondary_text: None,
                                recent_messages: &recent,
                                target_label: &tgt_label,
                                game_name: game_name.as_deref(),
                                dictionary: &dict,
                                style: cfg.style,
                            };
                            match stt::llm_corrector::correct_transcript_with_llm(&cfg, &req) {
                                Ok(out) => {
                                    let _ = p.send_event(UserEvent::LlmSampleDone {
                                        ok: true,
                                        input: text,
                                        output: out,
                                        elapsed_ms: t0.elapsed().as_millis() as u64,
                                        error: None,
                                    });
                                }
                                Err(err) => {
                                    let _ = p.send_event(UserEvent::LlmSampleDone {
                                        ok: false,
                                        input: text,
                                        output: String::new(),
                                        elapsed_ms: t0.elapsed().as_millis() as u64,
                                        error: Some(err),
                                    });
                                }
                            }
                        });
                    }
                    ClientCommand::GetCatalogInventory => {
                        emit_catalog_inventory(&state, &webview);
                    }
                    ClientCommand::CheckModelUrl { model_id, test_url } => {
                        let data_dir = state.data_dir();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let merged = stt::catalog::load_merged_catalog(&data_dir);
                            let res = if let Some(u) = test_url.filter(|s| !s.trim().is_empty()) {
                                let probe = stt::downloader::probe_url_health_winhttp(&u);
                                vec![stt::catalog::ModelUrlHealthDto {
                                    model_id,
                                    status: if probe.ok { "reachable" } else { "unreachable" }.into(),
                                    http_code: probe.status_code,
                                    content_length: probe.content_length,
                                    active_url: u,
                                    message: probe.message,
                                }]
                            } else if let Some(resolved) = merged.iter().find(|r| r.entry.id == model_id) {
                                vec![stt::downloader::check_model_url_health(resolved)]
                            } else {
                                Vec::new()
                            };
                            let _ = p.send_event(UserEvent::UrlHealthCheckDone(res));
                        });
                    }
                    ClientCommand::CheckAllModelUrls => {
                        let data_dir = state.data_dir();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let merged = stt::catalog::load_merged_catalog(&data_dir);
                            let mut results = Vec::new();
                            for item in merged {
                                if !item.entry.primary_url.trim().is_empty() {
                                    results.push(stt::downloader::check_model_url_health(&item));
                                }
                            }
                            let _ = p.send_event(UserEvent::UrlHealthCheckDone(results));
                        });
                    }
                    ClientCommand::SaveModelUrlOverride {
                        model_id,
                        primary_url,
                        skip_sha256,
                    } => {
                        let data_dir = state.data_dir();
                        match stt::catalog::save_catalog_override(
                            &data_dir,
                            &model_id,
                            &primary_url,
                            skip_sha256,
                        ) {
                            Ok(()) => {
                                state.model_health_map.remove(&model_id);
                                emit_catalog_inventory(&state, &webview);
                                emit(
                                    &webview,
                                    &BackendEvent::Toast {
                                        level: "ok".into(),
                                        message: format!("{} のURL修正を保存しました", model_id),
                                    },
                                );
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
                    ClientCommand::ResetModelUrlOverride { model_id } => {
                        let data_dir = state.data_dir();
                        let _ = stt::catalog::reset_catalog_override(&data_dir, &model_id);
                        state.model_health_map.remove(&model_id);
                        emit_catalog_inventory(&state, &webview);
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "ok".into(),
                                message: format!("{} のURLをデフォルトへ戻しました", model_id),
                            },
                        );
                    }
                    ClientCommand::SyncOnlineCatalog => {
                        let data_dir = state.data_dir();
                        let url = state.config.catalog_manifest_url.clone();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::catalog::sync_online_catalog_winhttp(&url, &data_dir);
                            let _ = p.send_event(UserEvent::OnlineCatalogSyncDone(res));
                        });
                    }
                    ClientCommand::DiscoverLatestModels { broken_model_id } => {
                        let data_dir = state.data_dir();
                        let p = proxy.clone();
                        thread::spawn(move || {
                            let res = stt::catalog::discover_latest_and_alternative_models_winhttp(
                                &data_dir,
                                broken_model_id.as_deref(),
                            );
                            let _ = p.send_event(UserEvent::ModelsDiscoveredDone(res));
                        });
                    }
                    ClientCommand::DownloadBundle { bundle_id } => {
                        let m_dir = stt::local::resolve_download_model_dir(
                            &state.config.stt.model_dir,
                            state.runtime_dir.as_deref(),
                        );
                        let mut m_dirs = stt::local::resolve_model_search_dirs(
                            &state.config.stt.model_dir,
                            state.runtime_dir.as_deref(),
                        );
                        if !m_dirs.iter().any(|d| d == &m_dir) {
                            m_dirs.insert(0, m_dir.clone());
                        }
                        let data_dir = state.data_dir();
                        let hw = gpu_diag::detect_hardware_profile(state.runtime_dir.as_deref());
                        let merged = stt::catalog::load_merged_catalog(&data_dir);
                        let bundles = stt::catalog::build_recommended_bundles_in_dirs(&hw, &m_dirs, &merged);
                        if let Some(bundle) = bundles.into_iter().find(|b| b.id == bundle_id) {
                            let p = proxy.clone();
                            let to_download: Vec<String> = bundle
                                .model_ids
                                .into_iter()
                                .filter(|mid| {
                                    merged
                                        .iter()
                                        .find(|r| &r.entry.id == mid)
                                        .map(|r| {
                                            !r.entry.bundled_default
                                                && stt::catalog::find_matched_path_in_dirs(&m_dirs, &r.entry)
                                                    .is_none()
                                        })
                                        .unwrap_or(false)
                                })
                                .collect();
                            if to_download.is_empty() {
                                for mid in ["dpdfnet-16k", "gtcrn-16k", "whisper-large-v3-q8", "qwen3-asr-0.6b-int8", "parakeet-ja-int8"] {
                                    if bundle_id == "best_accuracy" && (mid == "whisper-large-v3-q8" || mid == "dpdfnet-16k") {
                                        let _ = apply_model_auto_preset_for_state(&mut state, mid, &audio_ctrl, &webview, &overlay_webview, &proxy);
                                    } else if bundle_id == "balanced_standard" && (mid == "qwen3-asr-0.6b-int8" || mid == "dpdfnet-16k") {
                                        let _ = apply_model_auto_preset_for_state(&mut state, mid, &audio_ctrl, &webview, &overlay_webview, &proxy);
                                    } else if bundle_id == "lightweight_game" && (mid == "parakeet-ja-int8" || mid == "gtcrn-16k") {
                                        let _ = apply_model_auto_preset_for_state(&mut state, mid, &audio_ctrl, &webview, &overlay_webview, &proxy);
                                    } else if bundle_id == "cpu_laptop" && (mid == "qwen3-asr-0.6b-int8" || mid == "gtcrn-16k") {
                                        let _ = apply_model_auto_preset_for_state(&mut state, mid, &audio_ctrl, &webview, &overlay_webview, &proxy);
                                    }
                                }
                            } else {
                                emit(
                                    &webview,
                                    &BackendEvent::Toast {
                                        level: "info".into(),
                                        message: format!(
                                            "「{}」のモデル {}件を順次ダウンロード開始します...",
                                            bundle.title,
                                            to_download.len()
                                        ),
                                    },
                                );
                                thread::spawn(move || {
                                    for mid in to_download {
                                        let res = stt::downloader::download_model_into_with_data(
                                            &mid,
                                            None,
                                            None,
                                            &m_dir,
                                            &data_dir,
                                            |id, pct, msg| {
                                                let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                                    model_id: id.to_string(),
                                                    progress: pct,
                                                    message: msg.to_string(),
                                                    done: false,
                                                    ok: true,
                                                });
                                            },
                                        );
                                        match res {
                                            Ok(path) => {
                                                let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                                    model_id: mid,
                                                    progress: 100,
                                                    message: format!("セット構成モデル完了: {}", path.display()),
                                                    done: true,
                                                    ok: true,
                                                });
                                            }
                                            Err(err) => {
                                                let _ = p.send_event(UserEvent::ModelDownloadProgress {
                                                    model_id: mid,
                                                    progress: 0,
                                                    message: err,
                                                    done: true,
                                                    ok: false,
                                                });
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                    ClientCommand::ApplyModelAutoPreset { model_id } => {
                        if !apply_model_auto_preset_for_state(
                            &mut state,
                            &model_id,
                            &audio_ctrl,
                            &webview,
                            &overlay_webview,
                            &proxy,
                        ) {
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "error".into(),
                                    message: format!("推奨プリセットが見つかりません: {}", model_id),
                                },
                            );
                        }
                    }
                    ClientCommand::UndoAutoPreset => {
                        if let Some(snap) = state.last_preset_snapshot.take() {
                            stt::catalog::restore_preset_snapshot(&mut state.config, snap);
                            let _ = state.config.save_to(&state.config_path);
                            audio_ctrl.update_vad(state.config.vad.clone());
                            audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                            spawn_stt_loader(
                                state.config.stt.clone(),
                                state.runtime_dir.clone(),
                                proxy.clone(),
                            );
                            emit(
                                &webview,
                                &BackendEvent::ConfigLoaded {
                                    config: state.config.clone(),
                                    config_path: state.config_path.display().to_string(),
                                },
                            );
                            emit_all(&webview, &overlay_webview, &state.status_event());
                            emit_catalog_inventory(&state, &webview);
                            emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "ok".into(),
                                    message: "プリセット自動適用前の設定へ戻しました".into(),
                                },
                            );
                        }
                    }
                    ClientCommand::CleanupUnusedModels => {
                        let m_dirs = stt::local::resolve_model_search_dirs(
                            &state.config.stt.model_dir,
                            state.runtime_dir.as_deref(),
                        );
                        let data_dir = state.data_dir();
                        let hw = gpu_diag::detect_hardware_profile(state.runtime_dir.as_deref());
                        let items = stt::catalog::build_catalog_inventory_in_dirs(
                            &m_dirs,
                            &data_dir,
                            &state.config,
                            &hw,
                            &state.model_health_map,
                        );
                        let mut freed_total = 0u64;
                        let mut del_count = 0usize;
                        for item in items {
                            if item.can_delete && !item.in_use {
                                if let Ok(f) = stt::catalog::delete_model_from_disk_in_dirs(&m_dirs, &data_dir, &item.id) {
                                    freed_total = freed_total.saturating_add(f);
                                    del_count += 1;
                                }
                            }
                        }
                        emit_catalog_inventory(&state, &webview);
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "ok".into(),
                                message: if del_count > 0 {
                                    format!(
                                        "未使用モデル {}件を削除し、{} を解放しました",
                                        del_count,
                                        stt::catalog::format_bytes(freed_total)
                                    )
                                } else {
                                    "削除対象の未使用モデルはありません".into()
                                },
                            },
                        );
                    }
                    ClientCommand::DeleteModel { model_id } => {
                        // Safe unload BEFORE deleting files on Windows (Section 3.0 & 6.2)!
                        let deleted_was_llm = model_id.contains("llm")
                            || state
                                .config
                                .stt
                                .accuracy
                                .llm
                                .model_file
                                .eq_ignore_ascii_case(model_id.trim_start_matches("custom:"));
                        let deleted_was_http = model_id == "qwen3-asr-1.7b-gguf";
                        let deleted_was_vad_nc = model_id
                            == state.config.noise_cancel.vad_engine_name()
                            || model_id == state.config.noise_cancel.stt_engine_name();

                        let _ = stt_tx.send(SttWorkerMsg::UnloadAll);
                        if let Ok(mut g) = AB_ENGINE_CACHE.lock() {
                            *g = None;
                        }
                        if deleted_was_http {
                            stt::http_asr::stop_http_asr_if_started_by_us();
                            if state.config.stt.engine == "local-http" {
                                state.config.stt.engine = "auto".into();
                            }
                        }
                        if deleted_was_llm {
                            stt::llm_corrector::stop_llm_server_if_started_by_us();
                            state.config.stt.accuracy.llm.enabled = false;
                            state.config.stt.accuracy.llm.model_file.clear();
                        }
                        if deleted_was_vad_nc {
                            if state.config.noise_cancel.vad_engine_name() == model_id {
                                state.config.noise_cancel.vad_engine = NoiseEngineKind::Rnnoise;
                            }
                            if state.config.noise_cancel.stt_engine_name() == model_id {
                                state.config.noise_cancel.stt_engine = NoiseEngineKind::OffHpf;
                            }
                            audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                        }
                        if !state.config.stt.model_file.is_empty()
                            && (model_id == state.config.stt.model_file
                                || model_id == format!("custom:{}", state.config.stt.model_file)
                                || (model_id == "whisper-large-v3-q8"
                                    && state.config.stt.model_file.contains("large-v3-q8")))
                        {
                            state.config.stt.model_file.clear();
                            state.config.stt.engine = "auto".into();
                        }
                        if model_id == "qwen3-asr-0.6b-int8" && state.config.stt.engine == "sherpa-qwen3" {
                            state.config.stt.engine = "auto".into();
                        }
                        if model_id == "parakeet-ja-int8" && state.config.stt.engine == "sherpa-onnx" {
                            state.config.stt.engine = "auto".into();
                        }

                        thread::sleep(Duration::from_millis(80));
                        let m_dirs = stt::local::resolve_model_search_dirs(
                            &state.config.stt.model_dir,
                            state.runtime_dir.as_deref(),
                        );
                        let data_dir = state.data_dir();
                        match stt::catalog::delete_model_from_disk_in_dirs(&m_dirs, &data_dir, &model_id) {
                            Ok(freed) => {
                                let _ = state.config.save_to(&state.config_path);
                                state.stt_ready = false;
                                state.stt_engine_name = "標準モデルへ再読込中...".into();
                                spawn_stt_loader(
                                    state.config.stt.clone(),
                                    state.runtime_dir.clone(),
                                    proxy.clone(),
                                );
                                emit_stt_models(&state, &webview);
                                emit_catalog_inventory(&state, &webview);
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
                                            "モデルを削除し、{} を解放しました",
                                            stt::catalog::format_bytes(freed)
                                        ),
                                    },
                                );
                            }
                            Err(err) => {
                                spawn_stt_loader(
                                    state.config.stt.clone(),
                                    state.runtime_dir.clone(),
                                    proxy.clone(),
                                );
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
                    ClientCommand::ApplyWizardRecommendation => {
                        let hw = gpu_diag::detect_hardware_profile(state.runtime_dir.as_deref());
                        state.config.stt.engine = hw.recommended_stt_engine.clone();
                        state.config.noise_cancel.vad_engine = match hw.recommended_vad_noise.as_str() {
                            "gtcrn-16k" => NoiseEngineKind::Gtcrn16k,
                            _ => NoiseEngineKind::Rnnoise,
                        };
                        state.config.noise_cancel.stt_engine = match hw.recommended_stt_noise.as_str() {
                            "dpdfnet-16k" => NoiseEngineKind::DpdfNet16k,
                            _ => NoiseEngineKind::OffHpf,
                        };
                        state.config.wizard_completed = true;
                        let _ = state.config.save_to(&state.config_path);
                        audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                        spawn_stt_loader(
                            state.config.stt.clone(),
                            state.runtime_dir.clone(),
                            proxy.clone(),
                        );
                        emit_catalog_inventory(&state, &webview);
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
                                message: format!("PC最適化構成を適用しました: {}", hw.summary_note),
                            },
                        );
                    }
                    ClientCommand::SaveBenchmarkCase {
                        diag_id,
                        expected_text,
                    } => {
                        if let Some(d) = state
                            .recent_utterances
                            .iter()
                            .find(|x| x.id == diag_id)
                            .cloned()
                        {
                            let id = format!("b_{}", d.id);
                            let dir = stats::bench_dir();
                            let wav_path = dir.join(format!("{}.wav", id));
                            match stats::write_f32_wav_16k(&wav_path, &d.samples_16k) {
                                Ok(()) => {
                                    let exp = if expected_text.trim().is_empty() {
                                        d.final_text.clone()
                                    } else {
                                        expected_text.trim().to_string()
                                    };
                                    state.bench_cases.retain(|c| c.id != id);
                                    state.bench_cases.push(stats::BenchmarkCase {
                                        id,
                                        label: format!("{} ({:.1}s)", d.timestamp, d.duration_sec),
                                        expected_text: exp,
                                        wav_path: wav_path.to_string_lossy().into_owned(),
                                        duration_sec: d.duration_sec,
                                    });
                                    stats::save_benchmark_cases(&dir, &state.bench_cases);
                                    emit(
                                        &webview,
                                        &BackendEvent::BenchmarkCasesUpdated {
                                            cases: state.bench_cases.clone(),
                                        },
                                    );
                                    emit(
                                        &webview,
                                        &BackendEvent::Toast {
                                            level: "ok".into(),
                                            message: "ベンチマークWAVとして保存しました".into(),
                                        },
                                    );
                                }
                                Err(err) => emit(
                                    &webview,
                                    &BackendEvent::Toast {
                                        level: "error".into(),
                                        message: err,
                                    },
                                ),
                            }
                        }
                    }
                    ClientCommand::DeleteBenchmarkCase { id } => {
                        if let Some(pos) = state.bench_cases.iter().position(|c| c.id == id) {
                            let c = state.bench_cases.remove(pos);
                            let _ = std::fs::remove_file(&c.wav_path);
                            stats::save_benchmark_cases(&stats::bench_dir(), &state.bench_cases);
                            emit(
                                &webview,
                                &BackendEvent::BenchmarkCasesUpdated {
                                    cases: state.bench_cases.clone(),
                                },
                            );
                        }
                    }
                    ClientCommand::RunBenchmarkSuite => {
                        let cases = state.bench_cases.clone();
                        let stt_cfg = state.config.stt.clone();
                        let rt = state.runtime_dir.clone();
                        let ctx = TranscribeContext {
                            language: stt_cfg.language.clone(),
                            hotwords_prompt: None,
                            preset: stt_cfg.preset,
                            radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                            eq_strength: state.config.noise_cancel.presence_eq_strength,
                            head_pad_ms: stt_cfg.accuracy.head_silence_pad_ms,
                            tail_pad_ms: stt_cfg.accuracy.tail_silence_pad_ms,
                            beam_size: stt_cfg.accuracy.beam_size,
                            temperature_fallback: stt_cfg.accuracy.temperature_fallback,
                        };
                        let mut results = Vec::new();
                        let eng = stt::create_engine(&stt_cfg, rt.as_deref());
                        let mut sum_sim = 0.0f32;
                        for c in &cases {
                            if let Ok(s) = stats::read_f32_wav_16k(std::path::Path::new(&c.wav_path)) {
                                let t0 = Instant::now();
                                let actual = eng.transcribe_with_context(&s, &ctx).unwrap_or_default();
                                let ms = t0.elapsed().as_millis() as u64;
                                let sim = processor::phonetic::phonetic_similarity(&actual, &c.expected_text);
                                sum_sim += sim;
                                results.push(stats::BenchmarkCaseResult {
                                    id: c.id.clone(),
                                    label: c.label.clone(),
                                    expected_text: c.expected_text.clone(),
                                    actual_text: actual,
                                    similarity: sim,
                                    infer_ms: ms,
                                });
                            }
                        }
                        let avg = if results.is_empty() { 0.0 } else { sum_sim / results.len() as f32 };
                        emit(
                            &webview,
                            &BackendEvent::BenchmarkSuiteResult {
                                results,
                                avg_similarity: avg,
                            },
                        );
                    }
                    ClientCommand::RunAutoTuner => {
                        let eng = stt::create_engine(&state.config.stt, state.runtime_dir.as_deref());
                        let ctx = TranscribeContext {
                            language: state.config.stt.language.clone(),
                            hotwords_prompt: None,
                            preset: state.config.stt.preset,
                            radio_clarity: state.config.noise_cancel.radio_clarity_mode,
                            eq_strength: state.config.noise_cancel.presence_eq_strength,
                            head_pad_ms: state.config.stt.accuracy.head_silence_pad_ms,
                            tail_pad_ms: state.config.stt.accuracy.tail_silence_pad_ms,
                            beam_size: state.config.stt.accuracy.beam_size,
                            temperature_fallback: state.config.stt.accuracy.temperature_fallback,
                        };
                        let den_opt = stt::local::discover_stt_denoiser(
                            state.config.noise_cancel.stt_engine,
                            &state.config.stt,
                            state.runtime_dir.as_deref(),
                            state.config.noise_cancel.dpdfnet_attenuation_limit_db,
                        );
                        match stats::run_auto_parameter_tuner(
                            &state.bench_cases,
                            eng.as_ref(),
                            &ctx,
                            den_opt.as_deref(),
                        ) {
                            Ok(proposal) => {
                                emit(&webview, &BackendEvent::AutoTunerResult { proposal });
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
                    ClientCommand::ApplyAutoTuner {
                        eq_strength,
                        wsola_rate,
                        beam_size,
                        dpdfnet_db,
                    } => {
                        state.config.noise_cancel.presence_eq_strength = eq_strength.clamp(0.0, 1.0);
                        state.config.stt.accuracy.wsola_rate = wsola_rate.clamp(0.75, 1.0);
                        state.config.stt.accuracy.beam_size = beam_size.clamp(1, 5);
                        if let Some(db) = dpdfnet_db {
                            state.config.noise_cancel.dpdfnet_attenuation_limit_db =
                                db.abs().clamp(0.0, 60.0);
                        }
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
                                message: "自動探索パラメータを適用しました".into(),
                            },
                        );
                    }
                    ClientCommand::ApplyCalibrationResult {
                        vad_threshold,
                        eq_strength,
                        wsola_enabled,
                        noise_engine,
                    } => {
                        state.config.vad.threshold = vad_threshold.clamp(0.002, 0.20);
                        state.config.noise_cancel.presence_eq_strength = eq_strength.clamp(0.0, 1.0);
                        state.config.stt.accuracy.wsola_enabled = wsola_enabled;
                        state.config.noise_cancel.stt_engine = match noise_engine.as_str() {
                            "dpdfnet-48k" => NoiseEngineKind::DpdfNet48k,
                            "gtcrn-16k" => NoiseEngineKind::Gtcrn16k,
                            "deepfilternet3" => NoiseEngineKind::DeepFilterNet3,
                            "rnnoise" => NoiseEngineKind::Rnnoise,
                            "off-hpf" => NoiseEngineKind::OffHpf,
                            _ => NoiseEngineKind::DpdfNet16k,
                        };
                        audio_ctrl.update_vad(state.config.vad.clone());
                        audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                        let _ = state.config.save_to(&state.config_path);
                        emit(
                            &webview,
                            &BackendEvent::ConfigLoaded {
                                config: state.config.clone(),
                                config_path: state.config_path.display().to_string(),
                            },
                        );
                        emit_all(&webview, &overlay_webview, &state.status_event());
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "ok".into(),
                                message: "15秒滑舌診断の推奨パラメータを適用しました".into(),
                            },
                        );
                    }
                    ClientCommand::StartCalibrationWizard => {
                        state.calibration_wizard_active = true;
                        if !state.listening {
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
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "info".into(),
                                message: "15秒滑舌診断開始：普段通りの速さで「今日は天気がいいのでDiscordで連絡します」と話してください".into(),
                            },
                        );
                    }
                    ClientCommand::TogglePanicSafeMode => {
                        toggle_panic_safe_mode(
                            &mut state,
                            &audio_ctrl,
                            &tts_ctrl,
                            &sound_player,
                            &webview,
                            &overlay_webview,
                            &overlay_window,
                            &tray,
                        );
                    }
                    ClientCommand::ToggleTargetLock => {
                        state.config.guard.target_locked = !state.config.guard.target_locked;
                        let _ = state.config.save_to(&state.config_path);
                        emit_all(&webview, &overlay_webview, &state.status_event());
                        emit(
                            &webview,
                            &BackendEvent::Toast {
                                level: "ok".into(),
                                message: if state.config.guard.target_locked {
                                    "宛先ロック ON（誤宛先切替を防止）"
                                } else {
                                    "宛先ロック OFF"
                                }
                                .into(),
                            },
                        );
                    }
                    ClientCommand::ExportBackup => {
                        match stats::export_backup_json(&state.config, &state.stats) {
                            Ok(json) => emit(&webview, &BackendEvent::BackupExported { json }),
                            Err(err) => emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "error".into(),
                                    message: err,
                                },
                            ),
                        }
                    }
                    ClientCommand::ImportBackup { json } => {
                        match stats::import_backup_json(&json) {
                            Ok((new_cfg, new_stats)) => {
                                state.config = new_cfg;
                                let _ = state.config.save_to(&state.config_path);
                                if let Some(ns) = new_stats {
                                    state.stats = ns;
                                    state.stats.save_to(&state.stats_path);
                                    emit(
                                        &webview,
                                        &BackendEvent::StatsUpdated {
                                            summary: state.stats.summary(),
                                        },
                                    );
                                }
                                audio_ctrl.update_vad(state.config.vad.clone());
                                audio_ctrl.update_noise_cancel(state.config.noise_cancel.clone());
                                state.sync_guard();
                                emit(
                                    &webview,
                                    &BackendEvent::ConfigLoaded {
                                        config: state.config.clone(),
                                        config_path: state.config_path.display().to_string(),
                                    },
                                );
                                emit_all(&webview, &overlay_webview, &state.status_event());
                                emit(
                                    &webview,
                                    &BackendEvent::Toast {
                                        level: "ok".into(),
                                        message: "バックアップから設定・辞書を復元しました".into(),
                                    },
                                );
                            }
                            Err(err) => emit(
                                &webview,
                                &BackendEvent::Toast {
                                    level: "error".into(),
                                    message: err,
                                },
                            ),
                        }
                    }
                    ClientCommand::GetStats => {
                        emit(
                            &webview,
                            &BackendEvent::StatsUpdated {
                                summary: state.stats.summary(),
                            },
                        );
                    }
                    ClientCommand::ResetStats => {
                        state.stats = stats::StatsStore::default();
                        state.stats.save_to(&state.stats_path);
                        emit(
                            &webview,
                            &BackendEvent::StatsUpdated {
                                summary: state.stats.summary(),
                            },
                        );
                    }
                    ClientCommand::CheckAppUpdate => {
                        let up_p = proxy.clone();
                        thread::spawn(move || {
                            if let Some((latest_tag, url)) = stats::check_latest_release_non_blocking() {
                                let _ = up_p.send_event(UserEvent::AppUpdateFound { latest_tag, url });
                            }
                        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_peak_dbfs_and_matches_game_profiles() {
        assert!((compute_peak_dbfs(&[0.0, 0.0]) - (-96.0)).abs() < 1e-4);
        assert!((compute_peak_dbfs(&[0.5, -0.25]) - (-6.0206)).abs() < 0.05);

        let profiles = vec![GameProfile {
            id: "ow2".into(),
            name: "Overwatch 2".into(),
            enabled: true,
            process_names: vec!["Overwatch.exe".into()],
            dictionary: vec![DictionaryEntry {
                from: "うると".into(),
                to: "ウルト".into(),
            }],
            send_method: Some(config::SendMethod::SafeWhenGameActive),
            ..Default::default()
        }];

        let matched = find_matching_game_profile("overwatch.exe", &profiles).unwrap();
        assert_eq!(matched.id, "ow2");
        assert!(find_matching_game_profile("Discord.exe", &profiles).is_none());

        let base_filter = TextFilterConfig::default();
        let merged = merge_profile_filter(&base_filter, Some(matched));
        assert!(merged.dictionary.iter().any(|d| d.to == "ウルト"));
    }
}
