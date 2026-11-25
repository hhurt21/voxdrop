use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    Draft,
    Direct,
}

impl Default for InputMode {
    fn default() -> Self {
        Self::Draft
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceCommandsConfig {
    pub send: Vec<String>,
    pub clear_all: Vec<String>,
    pub undo: Vec<String>,
    pub newline: Vec<String>,
    pub mute: Vec<String>,
    pub resume: Vec<String>,
    // Discord remote control voice commands (reversible pairs)
    pub discord_mute_mic: Vec<String>,
    pub discord_unmute_mic: Vec<String>,
    pub discord_deafen: Vec<String>,
    pub discord_undeafen: Vec<String>,
    pub discord_nav_back: Vec<String>,
    pub discord_nav_forward: Vec<String>,
    pub discord_next_channel: Vec<String>,
    pub discord_prev_channel: Vec<String>,
    pub discord_prev_unread: Vec<String>,
    pub discord_next_unread: Vec<String>,
    pub discord_next_server: Vec<String>,
    pub discord_prev_server: Vec<String>,
    pub discord_scroll_up: Vec<String>,
    pub discord_scroll_down: Vec<String>,
    pub discord_jump_present: Vec<String>,
    pub discord_edit_last: Vec<String>,
    pub discord_cancel_escape: Vec<String>,
    pub discord_mark_read: Vec<String>,
    pub discord_toggle_members: Vec<String>,
    pub discord_toggle_pins: Vec<String>,
    pub discord_toggle_inbox: Vec<String>,
    pub discord_search: Vec<String>,
    pub discord_quick_switcher: Vec<String>,
}

impl Default for VoiceCommandsConfig {
    fn default() -> Self {
        Self {
            send: vec!["送信して".into(), "送って".into(), "エンター".into()],
            clear_all: vec!["今の消して".into(), "全部消して".into(), "キャンセル".into()],
            undo: vec!["一つ戻して".into(), "さっきの消して".into()],
            newline: vec!["改行して".into()],
            mute: vec!["ミュートして".into()],
            resume: vec!["認識再開".into()],
            discord_mute_mic: vec![
                "通話ミュート".into(),
                "ボイチャミュート".into(),
                "マイクミュート".into(),
                "マイクオフ".into(),
            ],
            discord_unmute_mic: vec![
                "ミュート解除".into(),
                "マイク解除".into(),
                "マイク戻して".into(),
                "マイクオン".into(),
                "通話復帰".into(),
            ],
            discord_deafen: vec![
                "スピーカーミュート".into(),
                "デフン".into(),
                "通話音消して".into(),
                "聞こえなくして".into(),
            ],
            discord_undeafen: vec![
                "スピーカー解除".into(),
                "デフン解除".into(),
                "スピーカー戻して".into(),
                "通話音戻して".into(),
                "聞こえるようにして".into(),
            ],
            discord_nav_back: vec![
                "前の画面".into(),
                "画面戻して".into(),
                "前に戻る".into(),
            ],
            discord_nav_forward: vec![
                "次の画面".into(),
                "画面進めて".into(),
                "次に進む".into(),
            ],
            discord_next_channel: vec!["次のチャンネル".into(), "下のチャンネル".into()],
            discord_prev_channel: vec!["前のチャンネル".into(), "上のチャンネル".into()],
            discord_prev_unread: vec![
                "前の未読".into(),
                "上の未読".into(),
            ],
            discord_next_unread: vec![
                "未読へ".into(),
                "次の未読".into(),
                "未読チャンネル".into(),
            ],
            discord_next_server: vec!["次のサーバー".into(), "下のサーバー".into()],
            discord_prev_server: vec!["前のサーバー".into(), "上のサーバー".into()],
            discord_scroll_up: vec![
                "上にスクロール".into(),
                "上スクロール".into(),
                "上に戻って".into(),
            ],
            discord_scroll_down: vec![
                "下にスクロール".into(),
                "下スクロール".into(),
            ],
            discord_jump_present: vec![
                "最新へ".into(),
                "一番下へ".into(),
                "最新に戻って".into(),
            ],
            discord_edit_last: vec![
                "直前を編集".into(),
                "さっきの編集".into(),
                "メッセージ編集".into(),
            ],
            discord_cancel_escape: vec![
                "編集キャンセル".into(),
                "検索を閉じる".into(),
                "画面を閉じて".into(),
                "操作取り消し".into(),
            ],
            discord_mark_read: vec!["既読にして".into(), "全部既読".into()],
            discord_toggle_members: vec![
                "メンバー表示".into(),
                "メンバー非表示".into(),
                "メンバーリスト".into(),
            ],
            discord_toggle_pins: vec!["ピン留め表示".into(), "ピン留め".into()],
            discord_toggle_inbox: vec!["受信トレイ".into(), "通知一覧".into()],
            discord_search: vec!["チャンネル検索".into(), "メッセージ検索".into()],
            discord_quick_switcher: vec!["クイックスイッチャー".into(), "クイック切替".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextFilterConfig {
    pub dictionary: Vec<DictionaryEntry>,
    pub fillers: Vec<String>,
    pub remove_fillers: bool,
    pub auto_space: bool,
    pub strip_punctuation: bool,
    pub append_period: bool,
    /// 文脈や接続詞・文節の区切りから自然な句読点（「、」「。」）を自動補完する（デフォルト: true）
    pub auto_punctuation: bool,
    /// 文の区切り・質問・話題転換などで自動的に改行を挿入する（デフォルト: true）
    pub auto_newline: bool,
}

impl Default for TextFilterConfig {
    fn default() -> Self {
        Self {
            dictionary: vec![
                DictionaryEntry {
                    from: "くさ".into(),
                    to: "草".into(),
                },
                DictionaryEntry {
                    from: "ナイス".into(),
                    to: ":nice:".into(),
                },
            ],
            fillers: vec![
                "えーと".into(),
                "えっと".into(),
                "あのー".into(),
                "うーん".into(),
                "えー".into(),
                "あー".into(),
            ],
            remove_fillers: true,
            auto_space: false,
            strip_punctuation: false,
            append_period: false,
            auto_punctuation: true,
            auto_newline: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VadConfig {
    pub threshold: f32,
    pub silence_ms: u64,
    pub min_speech_ms: u64,
    pub pre_roll_ms: u64,
    pub max_utterance_ms: u64,
    pub silero_model_path: String,
    /// 環境ノイズ床を自動学習して VAD しきい値と喋り終わり判定を常時最適化する（デフォルト: true）
    pub auto_adjust: bool,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.025,
            silence_ms: 420,
            min_speech_ms: 140,
            pre_roll_ms: 360,
            max_utterance_ms: 15000,
            silero_model_path: String::new(),
            auto_adjust: true,
        }
    }
}

/// beast-tools 相当の高性能ノイズキャンセリング（RNNoise + ハイパスフィルタ + ニューラルVADゲート）設定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseCancelConfig {
    /// RNNoise（ニューラルネットワークノイズ抑制）の有効/無効
    pub enabled: bool,
    /// RNNoise 抑制強度 (0.0 ..= 1.0) — 0.70前後が子音欠けを防ぎつつ雑音除去に最適
    pub strength: f32,
    /// 85Hz ハイパスフィルタ（空調・PCファン・机振動の低周波カット）
    pub highpass_enabled: bool,
    /// RNNoise の発話確率を使ったノイズゲート（語頭欠け防止のためデフォルトOFF）
    pub noise_gate_enabled: bool,
    /// 発話とみなすニューラルVAD確率しきい値 (0.05 ..= 0.95)
    pub vad_prob_threshold: f32,
    /// 無線・ラジオ・滑舌強化モード（GTCRN AI音声強調 + 2.5-4kHz子音明瞭化EQ + AGC自動音量正規化）
    pub radio_clarity_mode: bool,
}

impl Default for NoiseCancelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: 0.20,
            highpass_enabled: true,
            noise_gate_enabled: false,
            vad_prob_threshold: 0.30,
            radio_clarity_mode: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscordTargetKind {
    /// サーバー内のテキストチャンネル（guild_id + channel_id）
    Channel,
    /// フレンドとのダイレクトメッセージ（DM channel_id または user_id）
    Dm,
    /// 特定ユーザーへのメンション（<@user_id> を自動付与、任意で channel_id 移動も併用可）
    Mention,
}

impl Default for DiscordTargetKind {
    fn default() -> Self {
        Self::Channel
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordTarget {
    pub id: String,
    pub name: String,
    /// サーバー表示名（サーバー別グループ表示用。DM時は空欄）
    pub server_name: String,
    /// 合言葉（複数登録可、例: ["ざつだん", "雑談"]）
    pub passphrases: Vec<String>,
    pub kind: DiscordTargetKind,
    /// サーバーID（チャンネル指定時。空欄または "@me" でDM扱い）
    pub guild_id: String,
    /// Discord チャンネルID / DMチャンネルID
    pub channel_id: String,
    /// Discord ユーザーID（メンション付与やDM対象識別用）
    pub user_id: String,
    /// 送信文の先頭に `<@user_id> ` を自動付与するか
    pub prepend_mention: bool,
    /// 任意のDiscord Webhook URL（設定するとゲーム画面のままバックグラウンド直接送信）
    pub webhook_url: String,
}

impl Default for DiscordTarget {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            server_name: String::new(),
            passphrases: Vec::new(),
            kind: DiscordTargetKind::Channel,
            guild_id: String::new(),
            channel_id: String::new(),
            user_id: String::new(),
            prepend_mention: false,
            webhook_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordTargetsConfig {
    pub active_target_id: Option<String>,
    /// 合言葉プレフィックス（例: 「合言葉 雑談」「宛先 たろう」）
    pub switch_prefixes: Vec<String>,
    /// 合言葉サフィックス（例: 「雑談に切り替えて」「たろうに変更」）
    pub switch_suffixes: Vec<String>,
    /// 宛先解除コマンド（例: 「宛先解除」「通常に戻して」）
    pub reset_phrases: Vec<String>,
    /// プレフィックスなしで合言葉単独発話だけでも切り替えを許可するか
    pub allow_standalone_passphrase: bool,
    /// 未登録のフレンド名を呼んだとき Discord クイックスイッチャー(Ctrl+K)で検索＆確認するか
    pub use_quick_switcher_for_unknown: bool,
    /// 声で宛先を切り替えた瞬間に discord:// でチャンネル移動するか
    pub auto_navigate_on_switch: bool,
    /// 送信時に discord:// で対象チャンネルへ自動移動してから貼り付けるか
    pub auto_navigate_on_send: bool,
    /// discord:// 起動後にペーストするまでの待機時間 (ms)
    pub navigate_delay_ms: u64,
    pub targets: Vec<DiscordTarget>,
}

impl Default for DiscordTargetsConfig {
    fn default() -> Self {
        Self {
            active_target_id: None,
            switch_prefixes: vec![
                "合言葉".into(),
                "あいことば".into(),
                "宛先".into(),
                "あてさき".into(),
                "チャンネル変更".into(),
                "相手変更".into(),
                "フレンド".into(),
                "DM".into(),
            ],
            switch_suffixes: vec![
                "に切り替えて".into(),
                "に切り替え".into(),
                "に変更して".into(),
                "に変更".into(),
                "宛てにして".into(),
                "宛て".into(),
                "のDMを開いて".into(),
                "のDM".into(),
                "にDM".into(),
                "にディーエム".into(),
                "を開いて".into(),
                "と話す".into(),
                "を呼んで".into(),
            ],
            reset_phrases: vec![
                "宛先解除".into(),
                "宛先リセット".into(),
                "相手解除".into(),
                "通常に戻して".into(),
            ],
            allow_standalone_passphrase: false,
            use_quick_switcher_for_unknown: true,
            auto_navigate_on_switch: true,
            auto_navigate_on_send: true,
            navigate_delay_ms: 420,
            targets: Vec::new(),
        }
    }
}

impl DiscordTargetsConfig {
    pub fn active_target(&self) -> Option<&DiscordTarget> {
        let id = self.active_target_id.as_deref()?;
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn find_by_id(&self, id: &str) -> Option<&DiscordTarget> {
        self.targets.iter().find(|t| t.id == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMethod {
    /// 標準: Discord へ自動フォーカスし SendInput (Ctrl+V / Enter) で自動送信
    AutoPaste,
    /// SendInput 不使用: Win32 PostMessage (WM_CHAR / VK_RETURN) ＋ UIA SetFocus で送信（Webhook/Bot優先）
    WindowMessage,
    /// ゲーム中セーフ: Discord 以外が最前面のときは SendInput を使わず Webhook/Bot またはクリップボードコピーのみ
    SafeWhenGameActive,
    /// 完全セーフ: SendInput / PostMessage / マウス移動を一切使わず、Webhook/Bot 送信＋クリップボードコピーのみ
    NoSendInput,
}

impl Default for SendMethod {
    fn default() -> Self {
        Self::AutoPaste
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardConfig {
    pub enabled: bool,
    pub allowed_processes: Vec<String>,
    pub clipboard_restore_delay_ms: u64,
    /// Discordが非アクティブでも送信・操作時に自動で前面化して実行する（デフォルト: true）
    pub auto_focus_discord: bool,
    /// 自動フォーカスで送信した後、元のウィンドウ（ゲーム等）へフォーカスを戻す（デフォルト: true）
    pub restore_previous_window: bool,
    /// アンチチート干渉回避のための SendInput 制御方式
    pub send_method: SendMethod,
}

impl GuardConfig {
    pub fn should_avoid_send_input(&self, target_already_foreground: bool) -> bool {
        match self.send_method {
            SendMethod::AutoPaste => false,
            SendMethod::WindowMessage | SendMethod::NoSendInput => true,
            SendMethod::SafeWhenGameActive => !target_already_foreground,
        }
    }
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_processes: vec![
                "Discord.exe".into(),
                "DiscordPTB.exe".into(),
                "DiscordCanary.exe".into(),
            ],
            clipboard_restore_delay_ms: 80,
            auto_focus_discord: true,
            restore_previous_window: true,
            send_method: SendMethod::AutoPaste,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub toggle_listening: String,
    pub push_to_talk: String,
    pub clear_draft: String,
    pub toggle_overlay: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle_listening: "F8".into(),
            push_to_talk: String::new(),
            clear_draft: "Ctrl+Shift+X".into(),
            toggle_overlay: "Ctrl+Shift+O".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub click_through: bool,
    pub opacity: f32,
    pub x: i32,
    pub y: i32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            click_through: true,
            opacity: 0.92,
            x: 32,
            y: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    pub volume: f32,
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.45,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    /// "auto" | "sherpa-onnx" | "whisper-cli" | "mock"
    pub engine: String,
    pub model_dir: String,
    pub cli_path: String,
    pub language: String,
    pub threads: u32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            engine: "auto".into(),
            model_dir: "models".into(),
            cli_path: String::new(),
            language: "ja".into(),
            threads: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordIntegrationConfig {
    /// 公式 Discord Bot Token（任意・招待済みサーバーのチャンネル／メンバー一括同期用）
    pub bot_token: String,
    /// Bot API を使ってバックグラウンド送信する（デフォルト: false＝自分のアカウントで安全に操作）
    pub use_bot_for_send: bool,
    /// Discord で「リンクをコピー」した URL を自動検知して登録提案する
    pub clipboard_url_watch: bool,
}

impl Default for DiscordIntegrationConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            use_bot_for_send: false,
            clipboard_url_watch: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoicevoxConfig {
    pub base_url: String,
    pub speaker_id: u32,
    pub speed_scale: f32,
    pub pitch_scale: f32,
    pub intonation_scale: f32,
    pub volume_scale: f32,
    /// VOICEVOX ENGINE を窓なしで裏起動する（beast-tools 同等、デフォルト: true）
    pub auto_launch: bool,
    /// VOICEVOX 実行ファイルの場所（空欄で自動検出・run.exe 優先）
    pub exe_path: String,
    /// VoxDrop 終了時に、VoxDrop が起動した VOICEVOX を止める（デフォルト: true）
    pub stop_on_exit: bool,
}

impl Default for VoicevoxConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:50021".into(),
            speaker_id: 3,
            speed_scale: 1.15,
            pitch_scale: 0.0,
            intonation_scale: 1.0,
            volume_scale: 1.0,
            auto_launch: true,
            exe_path: String::new(),
            stop_on_exit: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordTtsConfig {
    pub enabled: bool,
    pub dm_only: bool,
    pub read_sender_name: bool,
    pub template: String,
    pub max_chars: usize,
    pub url_replacement: String,
    pub dedup_window_ms: u64,
    pub max_queue_size: usize,
    pub pause_stt_while_speaking: bool,
    pub echo_guard_tail_ms: u64,
    pub allow_senders: Vec<String>,
    pub block_senders: Vec<String>,
    pub debug_log_toasts: bool,
    /// Windows の Discord 通知を切っていても、UIA で未読・新着 DM を検知して読み上げる（デフォルト: true）
    pub read_without_windows_notifications: bool,
    pub voicevox: VoicevoxConfig,
}

impl Default for DiscordTtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dm_only: true,
            read_sender_name: true,
            template: "{sender}さんから。{message}".into(),
            max_chars: 80,
            url_replacement: "URL省略".into(),
            dedup_window_ms: 3000,
            max_queue_size: 5,
            pause_stt_while_speaking: true,
            echo_guard_tail_ms: 400,
            allow_senders: Vec::new(),
            block_senders: Vec::new(),
            debug_log_toasts: false,
            read_without_windows_notifications: true,
            voicevox: VoicevoxConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub input_mode: InputMode,
    pub audio_device: Option<String>,
    /// Windows起動時に自動起動する（デフォルト: false / OFF）
    pub auto_start: bool,
    pub start_minimized: bool,
    pub commands: VoiceCommandsConfig,
    pub filter: TextFilterConfig,
    pub vad: VadConfig,
    pub noise_cancel: NoiseCancelConfig,
    pub discord_targets: DiscordTargetsConfig,
    pub discord_integration: DiscordIntegrationConfig,
    pub discord_tts: DiscordTtsConfig,
    pub guard: GuardConfig,
    pub hotkeys: HotkeyConfig,
    pub overlay: OverlayConfig,
    pub sound: SoundConfig,
    pub stt: SttConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            input_mode: InputMode::Draft,
            audio_device: None,
            auto_start: false,
            start_minimized: false,
            commands: VoiceCommandsConfig::default(),
            filter: TextFilterConfig::default(),
            vad: VadConfig::default(),
            noise_cancel: NoiseCancelConfig::default(),
            discord_targets: DiscordTargetsConfig::default(),
            discord_integration: DiscordIntegrationConfig::default(),
            discord_tts: DiscordTtsConfig::default(),
            guard: GuardConfig::default(),
            hotkeys: HotkeyConfig::default(),
            overlay: OverlayConfig::default(),
            sound: SoundConfig::default(),
            stt: SttConfig::default(),
        }
    }
}

impl AppConfig {
    /// Returns the default configuration file path (`%APPDATA%\VoxDrop\config.json`).
    pub fn default_path() -> PathBuf {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let trimmed = appdata.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("VoxDrop").join("config.json");
            }
        }
        PathBuf::from("voxdrop.config.json")
    }

    pub fn load_or_default() -> (Self, PathBuf) {
        let path = Self::default_path();
        match Self::load_from(&path) {
            Ok(cfg) => (cfg, path),
            Err(_) => {
                let cfg = Self::default();
                let _ = cfg.save_to(&path);
                (cfg, path)
            }
        }
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path).map_err(|e| {
            format!("設定ファイルの読み込みに失敗しました ({}): {}", path.display(), e)
        })?;
        serde_json::from_str::<Self>(&content)
            .map_err(|e| format!("設定JSONの解析に失敗しました ({}): {}", path.display(), e))
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "設定ディレクトリの作成に失敗しました ({}): {}",
                        parent.display(),
                        e
                    )
                })?;
            }
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("設定のシリアライズに失敗しました: {}", e))?;

        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, json.as_bytes())
            .map_err(|e| format!("一時設定ファイルの書き込みに失敗しました: {}", e))?;

        if path.exists() {
            let _ = fs::remove_file(path);
        }
        fs::rename(&tmp_path, path)
            .or_else(|_| fs::write(path, json.as_bytes()))
            .map_err(|e| format!("設定ファイルの保存に失敗しました ({}): {}", path.display(), e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrip() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string_pretty(&cfg).expect("serialize");
        let parsed: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn partial_json_uses_defaults() {
        let partial = r#"{"input_mode": "direct"}"#;
        let parsed: AppConfig = serde_json::from_str(partial).expect("deserialize partial");
        assert_eq!(parsed.input_mode, InputMode::Direct);
        assert!(parsed.guard.enabled);
        assert!(parsed.noise_cancel.enabled);
        assert!(!parsed.discord_targets.switch_prefixes.is_empty());
        assert!(parsed
            .guard
            .allowed_processes
            .iter()
            .any(|p| p == "Discord.exe"));
        assert!(!parsed.commands.send.is_empty());
        assert!(!parsed.discord_tts.enabled);
        assert!(parsed.discord_tts.dm_only);
        assert!(parsed.discord_tts.read_without_windows_notifications);
        assert!(parsed.discord_tts.pause_stt_while_speaking);
        assert_eq!(parsed.discord_tts.voicevox.base_url, "http://127.0.0.1:50021");
        assert_eq!(parsed.discord_tts.voicevox.speaker_id, 3);
        assert_eq!(parsed.guard.send_method, SendMethod::AutoPaste);
        assert!(!parsed.guard.should_avoid_send_input(false));

        let mut g = GuardConfig::default();
        g.send_method = SendMethod::WindowMessage;
        assert!(g.should_avoid_send_input(false));
        assert!(g.should_avoid_send_input(true));

        g.send_method = SendMethod::SafeWhenGameActive;
        assert!(g.should_avoid_send_input(false));
        assert!(!g.should_avoid_send_input(true));

        g.send_method = SendMethod::NoSendInput;
        assert!(g.should_avoid_send_input(false));
        assert!(g.should_avoid_send_input(true));
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join(format!("voxdrop_cfg_test_{}", std::process::id()));
        let path = dir.join("config.json");
        let mut cfg = AppConfig::default();
        cfg.input_mode = InputMode::Direct;
        cfg.vad.threshold = 0.05;
        cfg.noise_cancel.strength = 0.85;
        cfg.save_to(&path).expect("save");
        let loaded = AppConfig::load_from(&path).expect("load");
        assert_eq!(cfg, loaded);
        let _ = fs::remove_dir_all(&dir);
    }
}
