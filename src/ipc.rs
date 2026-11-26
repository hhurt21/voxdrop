use crate::config::{ActivationMode, AppConfig, InputMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtteranceDiagnosticDto {
    pub id: u64,
    pub timestamp: String,
    pub duration_sec: f32,
    pub peak_dbfs: f32,
    pub infer_ms: u64,
    pub rtf: f32,
    pub device_name: String,
    pub engine_label: String,
    pub raw_text: String,
    pub final_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedChannelDto {
    pub server_name: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoicevoxSpeakerDto {
    pub id: u32,
    pub name: String,
    pub style_name: String,
}

/// Messages sent from WebView JS (`window.ipc.postMessage(JSON.stringify(...))`) to Rust.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientCommand {
    GetConfig,
    SaveConfig { config: AppConfig },
    ToggleListening,
    SetInputMode { mode: InputMode },
    ClearDraft,
    ForceSendDraft,
    UpdateDraft { draft: String },
    ListAudioDevices,
    ListSttModels,
    CalibrateVad,
    ReplayUtterance { id: u64 },
    AbCompareUtterance { id: u64, engine: String, model_file: String },
    ClearDiagnostics,
    QuickLearnCorrection { from: String, to: String },
    CancelSendCountdown,
    DownloadModel {
        model_id: String,
        custom_url: Option<String>,
        custom_filename: Option<String>,
    },
    TestHttpAsr { base_url: Option<String> },
    LaunchHttpAsr,
    ToggleOverlay,
    SetOverlayClickThrough { click_through: bool },
    DragOverlay,
    // Discord target switching, confirmation, sync & remote control
    SelectDiscordTarget { target_id: Option<String>, navigate: bool },
    NavigateDiscordTarget { target_id: String },
    ConfirmTargetReply { reply: String },
    ScanDiscordUi,
    SyncDiscordBot { bot_token: String },
    DiscordRemoteAction { action: String, delay_ms: Option<u64> },
    // Phase 2+ test & diagnostic commands
    GetActiveWindow,
    TestPaste { text: String, press_enter: bool, delay_ms: Option<u64> },
    TestKeyAction { action: String, delay_ms: Option<u64> },
    // Phase 4+ simulation / manual transcript testing
    SimulateTranscript { text: String },
    // Phase 5 sound preview
    PlayTestSound { kind: String },
    // Discord DM auto-reading (VOICEVOX TTS)
    #[serde(alias = "voicevox_check_health")]
    VoicevoxCheckHealth { base_url: Option<String> },
    #[serde(alias = "voicevox_get_speakers")]
    VoicevoxGetSpeakers { base_url: Option<String> },
    #[serde(alias = "voicevox_test_speak")]
    VoicevoxTestSpeak { text: Option<String> },
    #[serde(alias = "voicevox_detect_path")]
    VoicevoxDetectPath,
    #[serde(alias = "tts_stop")]
    TtsStop,
}

/// Events pushed from Rust to WebView JS (`window.onBackendEvent(...)`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum BackendEvent {
    ConfigLoaded {
        config: AppConfig,
        config_path: String,
    },
    ConfigSaved {
        ok: bool,
        message: Option<String>,
    },
    AudioLevel {
        rms: f32,
        is_speaking: bool,
        vad_prob: f32,
        rnnoise_active: bool,
        noise_floor: f32,
        effective_threshold: f32,
    },
    TranscriptUpdate {
        text: String,
        draft: String,
        is_final: bool,
    },
    CommandExecuted {
        action: String,
        detail: String,
    },
    ConfirmationPrompt {
        active: bool,
        has_next: bool,
        title: String,
        subtitle: String,
    },
    DiscordScanResult {
        ok: bool,
        active_server: String,
        servers: Vec<String>,
        channels: Vec<ScannedChannelDto>,
        friends: Vec<String>,
        message: String,
    },
    DiscordBotSyncResult {
        ok: bool,
        added_channels: usize,
        added_members: usize,
        message: String,
    },
    DiscordUrlDetected {
        url: String,
        is_dm: bool,
        guild_id: String,
        channel_id: String,
    },
    StatusChanged {
        listening: bool,
        sleeping: bool,
        recognizing: bool,
        target_active: bool,
        active_process: String,
        input_mode: InputMode,
        activation_mode: ActivationMode,
        overlay_visible: bool,
        stt_ready: bool,
        stt_engine: String,
        stt_device: String,
        last_infer_ms: u64,
        last_rtf: f32,
        rnnoise_enabled: bool,
        active_discord_target_id: Option<String>,
        active_discord_target_label: String,
        active_profile: Option<String>,
    },
    AudioDevices {
        devices: Vec<String>,
        selected: Option<String>,
    },
    SttModels {
        models: Vec<String>,
        selected: String,
        qwen3_installed: bool,
    },
    DiagnosticsUpdated {
        items: Vec<UtteranceDiagnosticDto>,
    },
    AbCompareResult {
        id: u64,
        ok: bool,
        engine_label: String,
        device_name: String,
        infer_ms: u64,
        rtf: f32,
        text: String,
        error: Option<String>,
    },
    SendCountdown {
        active: bool,
        remaining_ms: u64,
        total_ms: u64,
        preview_text: String,
    },
    ModelDownloadProgress {
        model_id: String,
        progress: u8,
        message: String,
        done: bool,
        ok: bool,
    },
    HttpAsrHealthResult {
        ok: bool,
        message: String,
    },
    OverlayStyle {
        opacity: f32,
    },
    Toast {
        level: String,
        message: String,
    },
    VoicevoxHealthResult {
        ok: bool,
        version: Option<String>,
        message: String,
    },
    VoicevoxSpeakersResult {
        ok: bool,
        speakers: Vec<VoicevoxSpeakerDto>,
        message: String,
    },
    VoicevoxDetectResult {
        found: bool,
        path: String,
        headless: bool,
        message: String,
    },
    TtsStatusChanged {
        connected: bool,
        speaking: bool,
        queued: usize,
        last_error: Option<String>,
    },
    ToastDebugLog {
        text: String,
    },
}

impl ClientCommand {
    pub fn parse(raw: &str) -> Result<Self, String> {
        serde_json::from_str::<Self>(raw)
            .map_err(|e| format!("IPCコマンドの解析に失敗しました: {} (raw={})", e, raw))
    }
}

impl BackendEvent {
    pub fn to_eval_script(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!(
            "if (typeof window.onBackendEvent === 'function') {{ window.onBackendEvent({}); }}",
            json
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_commands() {
        let cmd = ClientCommand::parse(r#"{"type":"GetConfig"}"#).unwrap();
        assert!(matches!(cmd, ClientCommand::GetConfig));

        let cmd = ClientCommand::parse(r#"{"type":"SetInputMode","mode":"direct"}"#).unwrap();
        assert!(matches!(
            cmd,
            ClientCommand::SetInputMode {
                mode: InputMode::Direct
            }
        ));

        let cmd = ClientCommand::parse(
            r#"{"type":"SelectDiscordTarget","target_id":"t1","navigate":true}"#,
        )
        .unwrap();
        assert!(matches!(
            cmd,
            ClientCommand::SelectDiscordTarget {
                target_id: Some(_),
                navigate: true
            }
        ));

        let cmd = ClientCommand::parse(r#"{"type":"voicevox_check_health"}"#).unwrap();
        assert!(matches!(
            cmd,
            ClientCommand::VoicevoxCheckHealth { base_url: None }
        ));

        let cmd = ClientCommand::parse(r#"{"type":"tts_stop"}"#).unwrap();
        assert!(matches!(cmd, ClientCommand::TtsStop));
    }

    #[test]
    fn serialize_backend_event_script() {
        let ev = BackendEvent::AudioLevel {
            rms: 0.125,
            is_speaking: true,
            vad_prob: 0.87,
            rnnoise_active: true,
            noise_floor: 0.012,
            effective_threshold: 0.038,
        };
        let script = ev.to_eval_script();
        assert!(script.contains("window.onBackendEvent"));
        assert!(script.contains("\"type\":\"AudioLevel\""));
        assert!(script.contains("\"rnnoise_active\":true"));
    }
}
