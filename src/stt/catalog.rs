//! Unified External Model Catalog (#15), 3-Tier Manifest Resolution,
//! Recommended PC Setup Bundles, Auto-Preset + Snapshot Undo, and Safe Individual Deletion (#31).
//! Every UI-visible model and bundle MUST carry explicit benefits, best_for, and caution metadata.

use crate::config::{
    AppConfig, ConversationPreset, NoiseCancelConfig, NoiseEngineKind, SttConfig, VadConfig,
};
use crate::gpu_diag::HardwareProfile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

pub const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCategory {
    Stt,
    NoiseSuppression,
    LlmCorrection,
    Vad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendPreference {
    VulkanWithCpuFallback,
    #[serde(rename = "directml_with_cpu_fallback", alias = "direct_ml_with_cpu_fallback")]
    DirectMlWithCpuFallback,
    CpuOnly,
}

impl Default for BackendPreference {
    fn default() -> Self {
        Self::VulkanWithCpuFallback
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveType {
    RawFile,
    TarBz2,
    Zip,
}

impl Default for ArchiveType {
    fn default() -> Self {
        Self::RawFile
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelAutoPreset {
    pub backend: BackendPreference,
    pub language: Option<String>,
    pub beam_size: Option<u32>,
    pub temperature_fallback: Option<bool>,
    pub no_speech_thold: Option<f32>,
    pub attenuation_limit_db: Option<f32>,
    pub conversation_preset: Option<String>,
    pub recommended_vad_padding_ms: Option<(u32, u32)>,
}

impl ModelAutoPreset {
    pub fn summary_text(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.backend {
            BackendPreference::VulkanWithCpuFallback => "Vulkan/CPU".to_string(),
            BackendPreference::DirectMlWithCpuFallback => "DirectML/CPU".to_string(),
            BackendPreference::CpuOnly => "CPU".to_string(),
        });
        if let Some(b) = self.beam_size {
            parts.push(format!("beam={}", b));
        }
        if let Some(db) = self.attenuation_limit_db {
            parts.push(format!("DPDFNet={}dB", db.round() as i32));
        }
        if let Some((h, t)) = self.recommended_vad_padding_ms {
            parts.push(format!("無音余白={}/{}ms", h, t));
        }
        if let Some(ref p) = self.conversation_preset {
            parts.push(format!("会話={}", p));
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScores {
    pub accuracy: u8,
    pub noise_resilience: u8,
    pub speed: u8,
    pub lightness: u8,
}

impl Default for ModelScores {
    fn default() -> Self {
        Self {
            accuracy: 4,
            noise_resilience: 4,
            speed: 4,
            lightness: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogManifestEntry {
    pub id: String,
    pub category: ModelCategory,
    pub display_name: String,
    pub runtime: String,
    pub version_tag: String,
    pub primary_url: String,
    pub mirror_urls: Vec<String>,
    pub local_rel_path: String,
    pub local_match: Vec<String>,
    pub archive_type: ArchiveType,
    pub expected_size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub min_vram_gb: f32,
    pub download_size_mb: u32,
    pub scores: ModelScores,
    pub bundled_default: bool,
    pub benefits: Vec<String>,
    pub best_for: String,
    pub caution: String,
    pub recommended_preset: ModelAutoPreset,
}

impl Default for CatalogManifestEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            category: ModelCategory::Stt,
            display_name: String::new(),
            runtime: "Vulkan / CPU".into(),
            version_tag: String::new(),
            primary_url: String::new(),
            mirror_urls: Vec::new(),
            local_rel_path: String::new(),
            local_match: Vec::new(),
            archive_type: ArchiveType::RawFile,
            expected_size_bytes: None,
            sha256: None,
            min_vram_gb: 0.0,
            download_size_mb: 0,
            scores: ModelScores::default(),
            bundled_default: false,
            benefits: Vec::new(),
            best_for: String::new(),
            caution: String::new(),
            recommended_preset: ModelAutoPreset::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: String,
    pub models: Vec<CatalogManifestEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CatalogOverride {
    pub primary_url: Option<String>,
    pub skip_sha256: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverridesFile {
    pub overrides: BTreeMap<String, CatalogOverride>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCatalogModel {
    pub entry: CatalogManifestEntry,
    pub default_url: String,
    pub has_override: bool,
    pub skip_sha256: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUrlHealthDto {
    pub model_id: String,
    /// "unknown" | "reachable" | "mirror_reachable" | "unreachable"
    pub status: String,
    pub http_code: u32,
    pub content_length: Option<u64>,
    pub active_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogItemDto {
    pub id: String,
    pub category: ModelCategory,
    pub display_name: String,
    pub runtime: String,
    pub version_tag: String,
    pub benefits: Vec<String>,
    pub best_for: String,
    pub caution: String,
    pub min_vram_gb: f32,
    pub download_size_mb: u32,
    pub scores: ModelScores,
    pub bundled_default: bool,
    pub downloadable: bool,
    pub installed: bool,
    pub in_use: bool,
    pub can_delete: bool,
    pub disk_size_bytes: u64,
    pub disk_size_label: String,
    pub pc_fit_badge: String,
    pub primary_url: String,
    pub default_url: String,
    pub mirror_urls: Vec<String>,
    pub has_override: bool,
    pub skip_sha256: bool,
    pub sha256: Option<String>,
    pub preset_summary: String,
    pub health_status: String,
    pub health_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendedBundleDto {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub model_ids: Vec<String>,
    pub model_labels: Vec<String>,
    pub pending_download_mb: u32,
    pub all_installed: bool,
    pub is_recommended_for_pc: bool,
    pub pc_badge: String,
    pub benefits: Vec<String>,
    pub preset_summary: String,
}

#[derive(Debug, Clone)]
pub struct PresetRollbackSnapshot {
    pub stt: SttConfig,
    pub noise_cancel: NoiseCancelConfig,
    pub vad: VadConfig,
}

pub fn embedded_catalog_entries() -> Vec<CatalogManifestEntry> {
    let mut entries: Vec<CatalogManifestEntry> = serde_json::from_str::<CatalogManifest>(
        include_str!("../../models_catalog.json"),
    )
    .map(|m| m.models)
    .unwrap_or_default();

    // Also include CPU/built-in baseline noise/VAD items for unified UI display & protection
    entries.push(CatalogManifestEntry {
        id: "deepfilternet3".into(),
        category: ModelCategory::NoiseSuppression,
        display_name: "DeepFilterNet 3 (48kHz 高品位)".into(),
        runtime: "ONNX Runtime (CPU / DirectML)".into(),
        version_tag: "df3-onnx".into(),
        primary_url: String::new(),
        mirror_urls: Vec::new(),
        local_rel_path: "deepfilternet3.onnx".into(),
        local_match: vec!["deepfilternet3.onnx".into()],
        archive_type: ArchiveType::RawFile,
        min_vram_gb: 0.0,
        download_size_mb: 14,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 3, lightness: 3 },
        bundled_default: false,
        benefits: vec![
            "RNNoise の正当な上位互換で、突発音・複雑な非定常ノイズ除去に強い".into(),
            "48kHz フルバンド処理により自然な声質 (PESQ/STOI) を保ちやすい".into(),
            "Apache-2.0 / MIT ライセンスで配布にも安心".into(),
        ],
        best_for: "48kHz マイク入力の高品位ノイズ除去".into(),
        caution: "約40msのフレーム遅延があり、カスタムONNX配置向けオプションです".into(),
        recommended_preset: ModelAutoPreset {
            backend: BackendPreference::DirectMlWithCpuFallback,
            ..Default::default()
        },
        ..Default::default()
    });
    entries.push(CatalogManifestEntry {
        id: "rnnoise".into(),
        category: ModelCategory::NoiseSuppression,
        display_name: "RNNoise (内蔵ベースライン)".into(),
        runtime: "Rust nnnoiseless (CPU)".into(),
        version_tag: "builtin".into(),
        bundled_default: true,
        benefits: vec![
            "遅延約10msの超低遅延・超軽量ベースラインで即起動".into(),
            "追加ダウンロード不要で単体バイナリに完全内蔵済み".into(),
            "エアコン・PCファン等の定常ノイズ除去に安定".into(),
        ],
        best_for: "定常ノイズのみの軽量用途・設定不要での即利用".into(),
        caution: "突発的な打鍵音に弱く、強設定だと小声や子音を削りやすい".into(),
        scores: ModelScores { accuracy: 3, noise_resilience: 3, speed: 5, lightness: 5 },
        recommended_preset: ModelAutoPreset { backend: BackendPreference::CpuOnly, ..Default::default() },
        ..Default::default()
    });
    entries.push(CatalogManifestEntry {
        id: "off-hpf".into(),
        category: ModelCategory::NoiseSuppression,
        display_name: "OFF + 85Hz ハイパスフィルタ".into(),
        runtime: "Rust DSP (CPU ゼロ遅延)".into(),
        version_tag: "builtin".into(),
        bundled_default: true,
        benefits: vec![
            "AI加工による声質劣化・子音（サ行/タ行）の削れがゼロ".into(),
            "遅延ゼロ・CPU負荷ゼロで STT へ元の音声を渡せる".into(),
            "静かな部屋では STT 認識精度が最も高くなるケースが多い".into(),
        ],
        best_for: "静かな室内・単一指向性マイク（STT経路の推奨設定）".into(),
        caution: "周囲の生活音・キーボード打鍵音はそのまま通過します".into(),
        scores: ModelScores { accuracy: 5, noise_resilience: 1, speed: 5, lightness: 5 },
        recommended_preset: ModelAutoPreset { backend: BackendPreference::CpuOnly, ..Default::default() },
        ..Default::default()
    });
    entries.push(CatalogManifestEntry {
        id: "silero-vad".into(),
        category: ModelCategory::Vad,
        display_name: "Silero VAD (音声区間検出・同梱)".into(),
        runtime: "sherpa-onnx (CPU / DirectML)".into(),
        version_tag: "builtin".into(),
        local_rel_path: "silero_vad.onnx".into(),
        local_match: vec!["silero_vad.onnx".into()],
        download_size_mb: 1,
        bundled_default: true,
        benefits: vec![
            "ニューラル音声区間検出により話し始めの小声とノイズを高精度分離".into(),
            "適応型ノイズ床と連動して息継ぎ途切れを防止".into(),
            "単体バイナリに完全同梱済み（削除保護対象）".into(),
        ],
        best_for: "全環境必須の標準音声区間検出".into(),
        caution: "同梱コアコンポーネントのため個別削除不可です".into(),
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 5, lightness: 5 },
        recommended_preset: ModelAutoPreset { backend: BackendPreference::DirectMlWithCpuFallback, ..Default::default() },
        ..Default::default()
    });

    entries
}

fn overrides_path(data_dir: &Path) -> PathBuf {
    data_dir.join("model_catalog_overrides.json")
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("catalog_cache.json")
}

pub fn load_overrides(data_dir: &Path) -> OverridesFile {
    let p = overrides_path(data_dir);
    fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<OverridesFile>(&s).ok())
        .unwrap_or_default()
}

pub fn save_catalog_override(
    data_dir: &Path,
    model_id: &str,
    primary_url: &str,
    skip_sha256: bool,
) -> Result<(), String> {
    let u = primary_url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err("URL は https:// または http:// で指定してください".into());
    }
    fs::create_dir_all(data_dir).map_err(|e| format!("設定ディレクトリ作成失敗: {}", e))?;
    let mut ov = load_overrides(data_dir);
    ov.overrides.insert(
        model_id.to_string(),
        CatalogOverride {
            primary_url: Some(u.to_string()),
            skip_sha256,
            note: Some("ユーザー手動設定".into()),
        },
    );
    let json = serde_json::to_string_pretty(&ov).map_err(|e| e.to_string())?;
    atomic_write(&overrides_path(data_dir), json.as_bytes())
}

pub fn reset_catalog_override(data_dir: &Path, model_id: &str) -> Result<(), String> {
    let mut ov = load_overrides(data_dir);
    ov.overrides.remove(model_id);
    if ov.overrides.is_empty() {
        let p = overrides_path(data_dir);
        if p.exists() {
            let _ = fs::remove_file(p);
        }
        return Ok(());
    }
    fs::create_dir_all(data_dir).map_err(|e| format!("設定ディレクトリ作成失敗: {}", e))?;
    let json = serde_json::to_string_pretty(&ov).map_err(|e| e.to_string())?;
    atomic_write(&overrides_path(data_dir), json.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("書き込み失敗 ({}): {}", tmp.display(), e))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)
        .or_else(|_| fs::write(path, bytes))
        .map_err(|e| format!("置換保存失敗 ({}): {}", path.display(), e))
}

/// Loads 3-tier merged catalog: `model_catalog_overrides.json` > `catalog_cache.json` > `embedded`.
pub fn load_merged_catalog(data_dir: &Path) -> Vec<ResolvedCatalogModel> {
    let mut base = embedded_catalog_entries();

    // Tier 2: catalog_cache.json overrides matching IDs and appends newly announced models
    if let Ok(raw) = fs::read_to_string(cache_path(data_dir)) {
        if let Ok(manifest) = parse_and_validate_manifest(&raw) {
            for cached in manifest.models {
                if let Some(slot) = base.iter_mut().find(|e| e.id == cached.id) {
                    *slot = cached;
                } else {
                    base.push(cached);
                }
            }
        }
    }

    // Tier 1: User per-model URL overrides
    let ov = load_overrides(data_dir);
    base.into_iter()
        .map(|mut entry| {
            let default_url = entry.primary_url.clone();
            let mut has_override = false;
            let mut skip_sha256 = false;
            if let Some(user_ov) = ov.overrides.get(&entry.id) {
                if let Some(ref u) = user_ov.primary_url {
                    if !u.trim().is_empty() {
                        entry.primary_url = u.trim().to_string();
                        has_override = true;
                    }
                }
                skip_sha256 = user_ov.skip_sha256;
            }
            ResolvedCatalogModel {
                entry,
                default_url,
                has_override,
                skip_sha256,
            }
        })
        .collect()
}

pub fn parse_and_validate_manifest(raw: &str) -> Result<CatalogManifest, String> {
    let manifest: CatalogManifest =
        serde_json::from_str(raw).map_err(|e| format!("カタログJSON解析エラー: {}", e))?;
    if manifest.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(format!(
            "非対応の schema_version です: {} (対応: {})",
            manifest.schema_version, CATALOG_SCHEMA_VERSION
        ));
    }
    if manifest.models.is_empty() {
        return Err("カタログにモデル定義が1件も含まれていません".into());
    }
    for m in &manifest.models {
        if m.id.trim().is_empty() {
            return Err("id が空のモデル定義があります".into());
        }
        if m.primary_url.trim().is_empty() && !m.bundled_default {
            return Err(format!("モデル {} の primary_url が空です", m.id));
        }
        if m.benefits.is_empty() || m.benefits.iter().all(|b| b.trim().is_empty()) {
            return Err(format!("モデル {} に利点 (benefits) が未定義です", m.id));
        }
    }
    Ok(manifest)
}

/// Fetches latest official `models_catalog.json` via HTTPS-only WinHTTP and atomically saves `catalog_cache.json`.
pub fn sync_online_catalog_winhttp(
    manifest_url: &str,
    data_dir: &Path,
) -> Result<(usize, usize, String), String> {
    let trimmed = manifest_url.trim();
    if !trimmed.starts_with("https://") {
        return Err("セキュリティ保護のため、カタログ同期URLは https:// のみ許可されています".into());
    }
    let parsed = crate::stt::http_asr::parse_url(trimmed)?;
    let full_path = if parsed.base_path.is_empty() {
        "/".to_string()
    } else {
        parsed.base_path.clone()
    };

    let ua_w: Vec<u16> = "VoxDrop-CatalogSync/1.7\0".encode_utf16().collect();
    let host_w: Vec<u16> = parsed.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();

    let (status, bytes) = unsafe {
        let session = WinHttpOpen(
            PCWSTR(ua_w.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHTTP セッション初期化失敗".into());
        }
        let res = (|| -> Result<(u32, Vec<u8>), String> {
            let _ = WinHttpSetTimeouts(session, 4000, 5000, 8000, 8000);
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), parsed.port, 0);
            if connect.is_null() {
                return Err(format!("カタログサーバーへ接続できません: {}", parsed.host));
            }
            let req = WinHttpOpenRequest(
                connect,
                w!("GET"),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            );
            if req.is_null() {
                let _ = WinHttpCloseHandle(connect);
                return Err("HTTPS リクエスト作成に失敗しました".into());
            }
            WinHttpSendRequest(req, None, None, 0, 0, 0)
                .map_err(|e| format!("リクエスト送信失敗: {}", e))?;
            WinHttpReceiveResponse(req, std::ptr::null_mut())
                .map_err(|e| format!("応答受信失敗: {}", e))?;

            let mut status: u32 = 0;
            let mut slen = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut slen,
                std::ptr::null_mut(),
            );
            let mut out = Vec::new();
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let mut chunk = vec![0u8; avail as usize];
                let mut read: u32 = 0;
                if WinHttpReadData(req, chunk.as_mut_ptr() as *mut c_void, avail, &mut read).is_err()
                    || read == 0
                {
                    break;
                }
                out.extend_from_slice(&chunk[..read as usize]);
                if out.len() > 2 * 1024 * 1024 {
                    break;
                }
            }
            let _ = WinHttpCloseHandle(req);
            let _ = WinHttpCloseHandle(connect);
            Ok((status, out))
        })();
        let _ = WinHttpCloseHandle(session);
        res?
    };

    if status != 200 {
        return Err(format!("カタログ取得エラー (HTTP {})", status));
    }
    let raw = String::from_utf8(bytes).map_err(|_| "カタログはUTF-8ではありません".to_string())?;
    let manifest = parse_and_validate_manifest(&raw)?;

    let before = load_merged_catalog(data_dir);
    let mut updated_urls = 0usize;
    let mut added_models = 0usize;
    for m in &manifest.models {
        match before.iter().find(|b| b.entry.id == m.id) {
            Some(prev) if prev.default_url != m.primary_url => updated_urls += 1,
            None => added_models += 1,
            _ => {}
        }
    }

    fs::create_dir_all(data_dir).map_err(|e| format!("フォルダ作成失敗: {}", e))?;
    atomic_write(&cache_path(data_dir), raw.as_bytes())?;
    let summary = format!(
        "URL更新 {}件・新規モデル {}件を反映しました ({}件検証済)",
        updated_urls,
        added_models,
        manifest.models.len()
    );
    Ok((updated_urls, added_models, summary))
}

pub fn build_recommended_bundles(
    hw: &HardwareProfile,
    models_dir: &Path,
    merged: &[ResolvedCatalogModel],
) -> Vec<RecommendedBundleDto> {
    let best_bundle_id = if hw.is_on_battery || !hw.vulkan_available && hw.vram_gb < 1.5 {
        "cpu_laptop"
    } else if hw.vram_gb >= 7.5 {
        "best_accuracy"
    } else if hw.vram_gb >= 3.5 {
        "balanced_standard"
    } else {
        "lightweight_game"
    };

    let defs: &[(&str, &str, &str, &[&str], &[&str], &str)] = &[
        (
            "best_accuracy",
            "精度・滑舌最優先セット",
            "専用VRAM 8GB 以上（Radeon / NVIDIA / Intel Arc 共通）",
            &["whisper-large-v3-q8", "dpdfnet-16k"],
            &[
                "早口・長文・雑音下でも文脈復元と高精度文字起こしを最大化",
                "DPDFNet (12 dB) で打鍵音を消しつつ語頭の子音削れを防止",
                "CUDA不要で 8GB 以上の全 GPU (Vulkan) で高速動作",
            ],
            "Vulkan有効・beam_size=5・無音パディング(200/300ms)・DPDFNet 12dB",
        ),
        (
            "balanced_standard",
            "バランス標準セット（迷ったらこれ）",
            "専用VRAM 4〜8GB（ゲーム同時起動・日常会話）",
            &["whisper-large-v3-turbo-q8", "qwen3-asr-0.6b-int8", "dpdfnet-16k"],
            &[
                "認識精度と応答速度のバランスが良くゲーム裏でも安定",
                "追加容量を抑えつつ Whisper と Qwen3-ASR を切替・2パス比較可能",
                "DPDFNet 16kHz + 通常会話プリセットで導入直後から高精度",
            ],
            "Vulkan/DirectML優先・通常会話プリセット・動的 initial_prompt ON",
        ),
        (
            "lightweight_game",
            "重量級ゲーム・超軽量セット",
            "GPU負荷を抑えたい時 / VRAM 4GB未満",
            &["parakeet-ja-int8", "gtcrn-16k"],
            &[
                "ゲームのフレームレート低下・カクつきをほぼゼロに抑制",
                "16kHz直結の超軽量 GTCRN ノイズ抑制で遅延を最小化",
                "短い報告・コールアウトを GPU をほぼ使わず瞬時に文字化",
            ],
            "DirectML/CPU軽量設定・VRAM消費最小化・fastプリセット",
        ),
        (
            "cpu_laptop",
            "CPUオンリー・ノートPC快適セット",
            "GPU非搭載 / iGPU / バッテリー駆動",
            &["qwen3-asr-0.6b-int8", "gtcrn-16k"],
            &[
                "GPU無し・CUDA無しの環境でも実用速度で日本語認識",
                "メモリ・ディスク消費が小さくノートPCに優しい",
                "CPUスレッド自動制限（75%上限）でファン騒音と消費電力を抑制",
            ],
            "DirectML/CPU最適化・軽量VAD・低消費電力スレッド設定",
        ),
    ];

    defs.iter()
        .map(|&(id, title, subtitle, m_ids, benefits, preset_summary)| {
            let mut labels = Vec::new();
            let mut pending_mb = 0u32;
            let mut all_installed = true;
            for mid in m_ids {
                if let Some(rm) = merged.iter().find(|r| r.entry.id == *mid) {
                    labels.push(rm.entry.display_name.clone());
                    let inst = rm.entry.bundled_default || find_matched_path(models_dir, &rm.entry).is_some();
                    if !inst {
                        all_installed = false;
                        pending_mb = pending_mb.saturating_add(rm.entry.download_size_mb);
                    }
                }
            }
            let is_rec = id == best_bundle_id;
            RecommendedBundleDto {
                id: id.into(),
                title: title.into(),
                subtitle: subtitle.into(),
                model_ids: m_ids.iter().map(|s| (*s).into()).collect(),
                model_labels: labels,
                pending_download_mb: pending_mb,
                all_installed,
                is_recommended_for_pc: is_rec,
                pc_badge: if is_rec {
                    "★ あなたのPCに最適".into()
                } else {
                    "選択可能".into()
                },
                benefits: benefits.iter().map(|s| (*s).into()).collect(),
                preset_summary: preset_summary.into(),
            }
        })
        .collect()
}

pub fn apply_preset_with_snapshot(
    cfg: &mut AppConfig,
    model: &ResolvedCatalogModel,
    models_dir: &Path,
) -> (PresetRollbackSnapshot, String) {
    let snap = PresetRollbackSnapshot {
        stt: cfg.stt.clone(),
        noise_cancel: cfg.noise_cancel.clone(),
        vad: cfg.vad.clone(),
    };
    let e = &model.entry;
    let p = &e.recommended_preset;
    let mut changes = Vec::new();

    match e.category {
        ModelCategory::NoiseSuppression => {
            cfg.noise_cancel.enabled = true;
            cfg.noise_cancel.stt_engine = match e.id.as_str() {
                "dpdfnet-16k" => NoiseEngineKind::DpdfNet16k,
                "dpdfnet-48k" => NoiseEngineKind::DpdfNet48k,
                "gtcrn-16k" => NoiseEngineKind::Gtcrn16k,
                "deepfilternet3" => NoiseEngineKind::DeepFilterNet3,
                "off-hpf" => NoiseEngineKind::OffHpf,
                _ => NoiseEngineKind::Rnnoise,
            };
            if let Some(db) = p.attenuation_limit_db {
                cfg.noise_cancel.dpdfnet_attenuation_limit_db = db.abs().clamp(0.0, 60.0);
                changes.push(format!("過抑制リミット {:.0} dB", db.abs()));
            }
            changes.push(format!("STTノイズ抑制={}", e.display_name));
        }
        ModelCategory::Stt => {
            match e.id.as_str() {
                "qwen3-asr-0.6b-int8" => {
                    cfg.stt.engine = "sherpa-qwen3".into();
                    cfg.stt.model_file.clear();
                }
                "qwen3-asr-1.7b-gguf" => {
                    cfg.stt.engine = "local-http".into();
                }
                "parakeet-ja-int8" => {
                    cfg.stt.engine = "sherpa-onnx".into();
                    cfg.stt.model_file.clear();
                }
                _ => {
                    cfg.stt.engine = "whisper-cpp".into();
                    if let Some(matched) = find_matched_path(models_dir, e) {
                        if let Some(fname) = matched.file_name().and_then(|s| s.to_str()) {
                            cfg.stt.model_file = fname.to_string();
                        }
                    } else if !e.local_rel_path.is_empty() {
                        cfg.stt.model_file = e.local_rel_path.clone();
                    }
                }
            }
            if let Some(ref lang) = p.language {
                cfg.stt.language = lang.clone();
            }
            if let Some(b) = p.beam_size {
                cfg.stt.accuracy.beam_size = (b as i32).clamp(1, 5);
                changes.push(format!("beam_size={}", cfg.stt.accuracy.beam_size));
            }
            if let Some(tf) = p.temperature_fallback {
                cfg.stt.accuracy.temperature_fallback = tf;
            }
            if let Some(ref cp) = p.conversation_preset {
                cfg.stt.preset = match cp.as_str() {
                    "fast" => ConversationPreset::Fast,
                    "slow" | "quiet" => ConversationPreset::Slow,
                    _ => ConversationPreset::Normal,
                };
            }
            if let Some((head, tail)) = p.recommended_vad_padding_ms {
                cfg.stt.accuracy.head_silence_pad_ms = head as u64;
                cfg.stt.accuracy.tail_silence_pad_ms = tail as u64;
                changes.push(format!("無音パディング={}/{}ms", head, tail));
            }
            if matches!(p.backend, BackendPreference::CpuOnly) {
                let cores = std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(4);
                cfg.stt.threads = ((cores * 3) / 4).max(2).min(8);
            }
            changes.insert(0, format!("STT={}", e.display_name));
        }
        ModelCategory::LlmCorrection => {
            cfg.stt.accuracy.llm.enabled = true;
            if let Some(matched) = find_matched_path(models_dir, e) {
                if let Some(fname) = matched.file_name().and_then(|s| s.to_str()) {
                    cfg.stt.accuracy.llm.model_file = fname.to_string();
                }
            }
            changes.push(format!("ローカルLLM補正を有効化 ({})", e.display_name));
        }
        ModelCategory::Vad => {}
    }

    let summary = format!(
        "「{}」の推奨プリセットを自動適用しました（{}）",
        e.display_name,
        if changes.is_empty() {
            p.summary_text()
        } else {
            changes.join(" / ")
        }
    );
    (snap, summary)
}

pub fn restore_preset_snapshot(cfg: &mut AppConfig, snap: PresetRollbackSnapshot) {
    cfg.stt = snap.stt;
    cfg.noise_cancel = snap.noise_cancel;
    cfg.vad = snap.vad;
}

pub fn dir_size_bytes(path: &Path) -> u64 {
    if path.is_file() {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    if !path.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            total = total.saturating_add(dir_size_bytes(&e.path()));
        }
    }
    total
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 MB".into();
    }
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn find_matched_path(models_dir: &Path, entry: &CatalogManifestEntry) -> Option<PathBuf> {
    if !models_dir.exists() {
        return None;
    }
    if !entry.local_rel_path.trim().is_empty() {
        let rp = models_dir.join(entry.local_rel_path.trim());
        if rp.exists() {
            return Some(rp);
        }
    }
    for name in &entry.local_match {
        let p = models_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if entry.id == "qwen3-asr-0.6b-int8" {
        if let Ok(entries) = fs::read_dir(models_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir()
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.to_ascii_lowercase().contains("qwen3-asr"))
                        .unwrap_or(false)
                    && p.join("conv_frontend.onnx").is_file()
                {
                    return Some(p);
                }
            }
        }
    }
    if entry.id == "parakeet-ja-int8" {
        if let Ok(entries) = fs::read_dir(models_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir()
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.to_ascii_lowercase().contains("parakeet"))
                        .unwrap_or(false)
                {
                    return Some(p);
                }
            }
        }
    }
    None
}

pub fn compute_pc_fit(entry: &CatalogManifestEntry, hw: &HardwareProfile) -> String {
    if entry.min_vram_gb <= 0.0 {
        return "全PC快適".into();
    }
    if hw.vram_gb >= entry.min_vram_gb + 2.0 && !hw.is_on_battery {
        "最適 (高速GPU)".into()
    } else if hw.vram_gb >= entry.min_vram_gb {
        "推奨".into()
    } else if hw.vram_gb >= 1.5 {
        "動作可 (軽量設定推奨)".into()
    } else {
        "CPU動作 (低速注意)".into()
    }
}

pub fn is_entry_in_use(entry: &CatalogManifestEntry, cfg: &AppConfig) -> bool {
    let eng = cfg.stt.engine.to_ascii_lowercase();
    let mf = cfg.stt.model_file.to_ascii_lowercase();
    let llm_mf = cfg.stt.accuracy.llm.model_file.to_ascii_lowercase();
    match entry.id.as_str() {
        "whisper-large-v3-turbo-q8" => {
            (eng == "auto" || eng == "whisper-cpp")
                && (mf.is_empty() || mf.contains("large-v3-turbo"))
        }
        "whisper-large-v3-q8" => mf.contains("large-v3-q8"),
        "whisper-large-v3-turbo-ja-sft" => mf.contains("sft") || mf.contains("model-q8"),
        "qwen3-asr-0.6b-int8" => {
            eng == "sherpa-qwen3"
                || (cfg.stt.accuracy.ultimate_ensemble
                    && cfg.stt.accuracy.secondary_engine == "sherpa-qwen3")
        }
        "qwen3-asr-1.7b-gguf" => eng == "local-http",
        "parakeet-ja-int8" => eng == "sherpa-onnx" || mf.contains("parakeet"),
        "qwen3-1.7b-llm" => cfg.stt.accuracy.llm.enabled && llm_mf.contains("1.7b"),
        "qwen3-4b-llm" => cfg.stt.accuracy.llm.enabled && llm_mf.contains("4b"),
        "dpdfnet-16k" => {
            cfg.noise_cancel.vad_engine_name() == "dpdfnet-16k"
                || cfg.noise_cancel.stt_engine_name() == "dpdfnet-16k"
        }
        "dpdfnet-48k" => {
            cfg.noise_cancel.vad_engine_name() == "dpdfnet-48k"
                || cfg.noise_cancel.stt_engine_name() == "dpdfnet-48k"
        }
        "gtcrn-16k" => {
            cfg.noise_cancel.vad_engine_name() == "gtcrn-16k"
                || cfg.noise_cancel.stt_engine_name() == "gtcrn-16k"
        }
        "silero-vad" | "rnnoise" | "off-hpf" => true,
        _ => false,
    }
}

#[allow(dead_code)]
pub fn build_catalog_inventory(
    models_dir: &Path,
    cfg: &AppConfig,
    hw: &HardwareProfile,
) -> Vec<ModelCatalogItemDto> {
    build_catalog_inventory_ex(models_dir, Path::new("."), cfg, hw, &BTreeMap::new())
}

pub fn build_catalog_inventory_ex(
    models_dir: &Path,
    data_dir: &Path,
    cfg: &AppConfig,
    hw: &HardwareProfile,
    health_map: &BTreeMap<String, ModelUrlHealthDto>,
) -> Vec<ModelCatalogItemDto> {
    let mut out = Vec::new();
    let mut covered_paths: Vec<PathBuf> = Vec::new();
    let merged = load_merged_catalog(data_dir);

    for resolved in merged {
        let entry = &resolved.entry;
        let matched = find_matched_path(models_dir, entry);
        let installed = entry.bundled_default || matched.is_some();
        let disk_size_bytes = matched.as_ref().map(|p| dir_size_bytes(p)).unwrap_or(0);
        if let Some(p) = matched {
            covered_paths.push(p);
        }
        let in_use = is_entry_in_use(entry, cfg);
        let health = health_map.get(&entry.id);
        out.push(ModelCatalogItemDto {
            id: entry.id.clone(),
            category: entry.category,
            display_name: entry.display_name.clone(),
            runtime: entry.runtime.clone(),
            version_tag: entry.version_tag.clone(),
            benefits: entry.benefits.clone(),
            best_for: entry.best_for.clone(),
            caution: entry.caution.clone(),
            min_vram_gb: entry.min_vram_gb,
            download_size_mb: entry.download_size_mb,
            scores: entry.scores,
            bundled_default: entry.bundled_default,
            downloadable: !entry.primary_url.trim().is_empty(),
            installed,
            in_use,
            can_delete: installed && !entry.bundled_default,
            disk_size_bytes,
            disk_size_label: if disk_size_bytes > 0 {
                format_bytes(disk_size_bytes)
            } else if entry.bundled_default {
                "本体内蔵".into()
            } else {
                format!("約 {} MB", entry.download_size_mb)
            },
            pc_fit_badge: compute_pc_fit(entry, hw),
            primary_url: entry.primary_url.clone(),
            default_url: resolved.default_url,
            mirror_urls: entry.mirror_urls.clone(),
            has_override: resolved.has_override,
            skip_sha256: resolved.skip_sha256,
            sha256: entry.sha256.clone(),
            preset_summary: entry.recommended_preset.summary_text(),
            health_status: health
                .map(|h| h.status.clone())
                .unwrap_or_else(|| "unknown".into()),
            health_message: health.map(|h| h.message.clone()).unwrap_or_default(),
        });
    }

    if let Ok(entries) = fs::read_dir(models_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if covered_paths.iter().any(|cp| cp == &p) {
                continue;
            }
            let Some(name) = p.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let is_extra_model = (p.is_file()
                && (lower.ends_with(".bin") || lower.ends_with(".gguf") || lower.ends_with(".onnx")))
                || (p.is_dir() && lower.starts_with("sherpa-onnx-"));
            if !is_extra_model {
                continue;
            }
            let sz = dir_size_bytes(&p);
            let in_use = cfg.stt.model_file.eq_ignore_ascii_case(&name)
                || cfg.stt.accuracy.llm.model_file.eq_ignore_ascii_case(&name);
            let cat = if lower.ends_with(".gguf") && !lower.contains("asr") {
                ModelCategory::LlmCorrection
            } else if lower.contains("gtcrn") || lower.contains("dpdf") || lower.contains("deepfilter") {
                ModelCategory::NoiseSuppression
            } else {
                ModelCategory::Stt
            };
            out.push(ModelCatalogItemDto {
                id: format!("custom:{}", name),
                category: cat,
                display_name: format!("カスタム追加: {}", name),
                runtime: if lower.ends_with(".onnx") || p.is_dir() {
                    "sherpa-onnx (DirectML / CPU)".into()
                } else {
                    "Vulkan / CPU".into()
                },
                version_tag: "custom".into(),
                benefits: vec![
                    "ユーザーが追加・カスタムURL取得したローカルモデルファイル".into(),
                    "外部モデル管理UIから不要時にワンクリック個別削除が可能".into(),
                ],
                best_for: "特定話者・専門用語向けカスタムモデル".into(),
                caution: "動作要件は配置した量子化形式に依存します".into(),
                min_vram_gb: 2.0,
                download_size_mb: (sz / (1024 * 1024)) as u32,
                scores: ModelScores::default(),
                bundled_default: false,
                downloadable: false,
                installed: true,
                in_use,
                can_delete: true,
                disk_size_bytes: sz,
                disk_size_label: format_bytes(sz),
                pc_fit_badge: "カスタム".into(),
                primary_url: String::new(),
                default_url: String::new(),
                mirror_urls: Vec::new(),
                has_override: false,
                skip_sha256: false,
                sha256: None,
                preset_summary: String::new(),
                health_status: "unknown".into(),
                health_message: String::new(),
            });
        }
    }
    out
}

pub fn delete_model_from_disk(models_dir: &Path, model_id: &str) -> Result<u64, String> {
    delete_model_from_disk_ex(models_dir, Path::new("."), model_id)
}

pub fn delete_model_from_disk_ex(
    models_dir: &Path,
    data_dir: &Path,
    model_id: &str,
) -> Result<u64, String> {
    if let Some(custom_name) = model_id.strip_prefix("custom:") {
        let clean = crate::stt::downloader::sanitize_filename(custom_name)?;
        let p = models_dir.join(&clean);
        if !p.exists() {
            return Err("対象モデルが見つかりません".into());
        }
        let freed = dir_size_bytes(&p);
        remove_path_and_partials(&p)?;
        return Ok(freed);
    }

    let merged = load_merged_catalog(data_dir);
    let resolved = merged
        .iter()
        .find(|r| r.entry.id == model_id)
        .ok_or_else(|| format!("未知のモデルIDです: {}", model_id))?;

    if resolved.entry.bundled_default {
        return Err(format!(
            "「{}」は同梱標準モデルのため削除できません（保護対象）",
            resolved.entry.display_name
        ));
    }

    let matched = find_matched_path(models_dir, &resolved.entry).ok_or_else(|| {
        format!(
            "「{}」のローカルファイルは既に存在しません",
            resolved.entry.display_name
        )
    })?;
    let freed = dir_size_bytes(&matched);
    remove_path_and_partials(&matched)?;
    Ok(freed)
}

fn remove_path_and_partials(target: &Path) -> Result<(), String> {
    if target.is_dir() {
        fs::remove_dir_all(target).map_err(|e| {
            format!("モデルフォルダの削除に失敗しました ({}): {}", target.display(), e)
        })?;
    } else if target.is_file() {
        fs::remove_file(target).map_err(|e| {
            format!("モデルファイルの削除に失敗しました ({}): {}", target.display(), e)
        })?;
    }
    if let Some(name) = target.file_name().and_then(|s| s.to_str()) {
        if let Some(parent) = target.parent() {
            for ext in ["part", "tmp", "tar.bz2.part"] {
                let frag = parent.join(format!("{}.{}", name, ext));
                if frag.is_file() {
                    let _ = fs::remove_file(&frag);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_model_defines_non_empty_benefits_and_metadata() {
        let raw_manifest = include_str!("../../models_catalog.json");
        let parsed = parse_and_validate_manifest(raw_manifest).expect("valid models_catalog.json");
        assert!(!parsed.models.is_empty());

        let all = embedded_catalog_entries();
        assert!(all.len() >= parsed.models.len());
        for entry in all {
            assert!(
                !entry.benefits.is_empty(),
                "Model {} MUST declare non-empty benefits (Section 3 enforcement)",
                entry.id
            );
            assert!(
                entry.benefits.iter().all(|b| !b.trim().is_empty()),
                "Model {} contains blank benefit string",
                entry.id
            );
            assert!(!entry.best_for.trim().is_empty());
            assert!(!entry.caution.trim().is_empty());
            assert!(!entry.runtime.trim().is_empty());
        }
    }

    #[test]
    fn three_tier_catalog_merge_and_reset_works() {
        let dir = std::env::temp_dir().join(format!("voxdrop_tier_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Write Tier 2 cache overriding URL
        let mut manifest = parse_and_validate_manifest(include_str!("../../models_catalog.json")).unwrap();
        manifest.models[0].primary_url = "https://example.com/cached.onnx".into();
        fs::write(cache_path(&dir), serde_json::to_string(&manifest).unwrap()).unwrap();

        let merged1 = load_merged_catalog(&dir);
        assert_eq!(merged1[0].entry.primary_url, "https://example.com/cached.onnx");
        assert!(!merged1[0].has_override);

        // Tier 1 override wins over cache
        save_catalog_override(&dir, &manifest.models[0].id, "https://example.com/user_fix.onnx", true)
            .unwrap();
        let merged2 = load_merged_catalog(&dir);
        assert_eq!(merged2[0].entry.primary_url, "https://example.com/user_fix.onnx");
        assert_eq!(merged2[0].default_url, "https://example.com/cached.onnx");
        assert!(merged2[0].has_override);
        assert!(merged2[0].skip_sha256);

        // Reset restores Tier 2/3 URL
        reset_catalog_override(&dir, &manifest.models[0].id).unwrap();
        let merged3 = load_merged_catalog(&dir);
        assert_eq!(merged3[0].entry.primary_url, "https://example.com/cached.onnx");
        assert!(!merged3[0].has_override);

        // Invalid schema_version or blank benefits rejected
        assert!(parse_and_validate_manifest(r#"{"schema_version":99,"models":[]}"#).is_err());
        assert!(parse_and_validate_manifest(
            r#"{"schema_version":1,"models":[{"id":"x","primary_url":"https://a/b","benefits":[]}]}"#
        )
        .is_err());
        // Security: http:// manifest rejected on sync
        assert!(sync_online_catalog_winhttp("http://example.com/models_catalog.json", &dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_preset_applies_and_undo_restores_config_cleanly() {
        let dir = std::env::temp_dir().join(format!("voxdrop_preset_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let merged = load_merged_catalog(&dir);
        let dpdf = merged.iter().find(|r| r.entry.id == "dpdfnet-16k").unwrap();
        let mut cfg = AppConfig::default();
        cfg.noise_cancel.dpdfnet_attenuation_limit_db = 22.0;
        cfg.noise_cancel.stt_engine = NoiseEngineKind::Rnnoise;

        let (snap, summary) = apply_preset_with_snapshot(&mut cfg, dpdf, &dir);
        assert!(summary.contains("12 dB"));
        assert_eq!(cfg.noise_cancel.stt_engine, NoiseEngineKind::DpdfNet16k);
        assert_eq!(cfg.noise_cancel.dpdfnet_attenuation_limit_db, 12.0);

        restore_preset_snapshot(&mut cfg, snap);
        assert_eq!(cfg.noise_cancel.stt_engine, NoiseEngineKind::Rnnoise);
        assert_eq!(cfg.noise_cancel.dpdfnet_attenuation_limit_db, 22.0);

        let hw_8gb = HardwareProfile {
            gpu_name: "AMD Radeon RX 6700 XT".into(),
            gpu_vendor: "AMD".into(),
            vram_gb: 12.0,
            vram_used_gb: None,
            vram_usage_pct: None,
            is_on_battery: false,
            battery_pct: None,
            cpu_cores: 12,
            vulkan_available: true,
            directml_available: true,
            recommended_stt_engine: "whisper-cpp".into(),
            recommended_stt_model: "ggml-large-v3-q8_0.bin".into(),
            recommended_vad_noise: "gtcrn-16k".into(),
            recommended_stt_noise: "dpdfnet-16k".into(),
            recommended_llm_class: "Qwen3-4B".into(),
            summary_note: String::new(),
        };
        let bundles = build_recommended_bundles(&hw_8gb, &dir, &merged);
        assert_eq!(bundles.len(), 4);
        assert!(bundles.iter().all(|b| b.benefits.len() >= 2));
        assert!(bundles.iter().find(|b| b.id == "best_accuracy").unwrap().is_recommended_for_pc);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletes_external_model_safely_while_protecting_bundled_default() {
        let dir = std::env::temp_dir().join(format!("voxdrop_del_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(delete_model_from_disk(&dir, "whisper-large-v3-turbo-q8").is_err());
        assert!(delete_model_from_disk(&dir, "silero-vad").is_err());

        let f = dir.join("ggml-large-v3-q8_0.bin");
        let f_part = dir.join("ggml-large-v3-q8_0.bin.part");
        fs::write(&f, vec![1u8; 4096]).unwrap();
        fs::write(&f_part, b"partial").unwrap();
        let freed = delete_model_from_disk(&dir, "whisper-large-v3-q8").expect("delete file");
        assert_eq!(freed, 4096);
        assert!(!f.exists());
        assert!(!f_part.exists());

        let qdir = dir.join("sherpa-onnx-qwen3-asr-0.6B-int8");
        fs::create_dir_all(qdir.join("tokenizer")).unwrap();
        fs::write(qdir.join("conv_frontend.onnx"), vec![2u8; 2048]).unwrap();
        let qfreed = delete_model_from_disk(&dir, "qwen3-asr-0.6b-int8").expect("delete dir");
        assert_eq!(qfreed, 2048);
        assert!(!qdir.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
