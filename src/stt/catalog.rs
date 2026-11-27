//! Unified External Model Catalog (#15), Dynamic PC Fit Badges (#29),
//! Safe Individual Deletion (#31), and SHA-256 Integrity Checksum Support.
//! Every UI-visible model MUST carry explicit benefits, best_for, and caution metadata.

use crate::config::AppConfig;
use crate::gpu_diag::HardwareProfile;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCategory {
    Stt,
    NoiseSuppression,
    LlmCorrection,
    Vad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScores {
    pub accuracy: u8,
    pub noise_resilience: u8,
    pub speed: u8,
    pub lightness: u8,
}

#[derive(Debug, Clone)]
pub struct ModelCatalogEntry {
    pub id: &'static str,
    pub category: ModelCategory,
    pub display_name: &'static str,
    pub runtime: &'static str,
    pub benefits: &'static [&'static str],
    pub best_for: &'static str,
    pub caution: &'static str,
    pub min_vram_gb: f32,
    pub download_size_mb: u32,
    pub scores: ModelScores,
    pub bundled_default: bool,
    pub download_url: Option<&'static str>,
    pub local_match: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogItemDto {
    pub id: String,
    pub category: ModelCategory,
    pub display_name: String,
    pub runtime: String,
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
}

pub const MODEL_CATALOG: &[ModelCatalogEntry] = &[
    // --- Noise Suppression Models (Section 3.1) ---
    ModelCatalogEntry {
        id: "dpdfnet-16k",
        category: ModelCategory::NoiseSuppression,
        display_name: "DPDFNet (16kHz Dual-Path)",
        runtime: "sherpa-onnx (DirectML / CPU)",
        benefits: &[
            "DeepFilterNet2 を Dual-Path RNN で拡張し、打鍵音・生活音除去力が極めて高い",
            "過抑制リミット (attenuation_limit dB) でノイズを消しつつ滑舌・子音の削れを防ぐ",
            "既存 sherpa-onnx-c-api.dll で動作するため追加DLL・CUDA不要",
        ],
        best_for: "滑舌重視＋キーボード打鍵音・生活音がある環境（12 dB 推奨）",
        caution: "RNNoise より CPU 負荷がやや増加します",
        min_vram_gb: 0.0,
        download_size_mb: 10,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 4, lightness: 3 },
        bundled_default: false,
        download_url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/dpdfnet_baseline_16k.onnx"),
        local_match: &["dpdfnet2.onnx", "dpdfnet_baseline_16k.onnx"],
    },
    ModelCatalogEntry {
        id: "dpdfnet-48k",
        category: ModelCategory::NoiseSuppression,
        display_name: "DPDFNet (48kHz フルバンド)",
        runtime: "sherpa-onnx (DirectML / CPU)",
        benefits: &[
            "48kHz フルバンド帯域のまま高精度な Dual-Path ノイズ抑制が可能",
            "サ行・タ行など高周波子音の自然な明瞭度を維持",
            "attenuation_limit (dB) 調整で過抑制による声質劣化を回避",
        ],
        best_for: "高音質マイク（48kHz）＋騒音環境",
        caution: "16kHz 版より推論負荷が高く、STT 直前では16kへリサンプリングされます",
        min_vram_gb: 0.0,
        download_size_mb: 12,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 3, lightness: 3 },
        bundled_default: false,
        download_url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/dpdfnet_48k.onnx"),
        local_match: &["dpdfnet_48k.onnx"],
    },
    ModelCatalogEntry {
        id: "gtcrn-16k",
        category: ModelCategory::NoiseSuppression,
        display_name: "GTCRN (超軽量 16kHz)",
        runtime: "sherpa-onnx (CPU / DirectML)",
        benefits: &[
            "16kHz 音声強調に特化した超軽量モデルで CPU 消費が極めて少ない",
            "Silero VAD / STT の 16kHz パイプラインとリサンプリング無しで直結",
            "重量級ゲーム同時起動でもフレームレートへの影響がほぼゼロ",
        ],
        best_for: "重量級ゲーム同時起動・ノートPC・低遅延重視",
        caution: "極端な爆音ノイズ環境では DPDFNet に一歩譲ります",
        min_vram_gb: 0.0,
        download_size_mb: 1,
        scores: ModelScores { accuracy: 4, noise_resilience: 4, speed: 5, lightness: 5 },
        bundled_default: false,
        download_url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/gtcrn_simple.onnx"),
        local_match: &["gtcrn_simple.onnx", "gtcrn.onnx"],
    },
    ModelCatalogEntry {
        id: "deepfilternet3",
        category: ModelCategory::NoiseSuppression,
        display_name: "DeepFilterNet 3 (48kHz 高品位)",
        runtime: "ONNX Runtime (CPU / DirectML)",
        benefits: &[
            "RNNoise の正当な上位互換で、突発音・複雑な非定常ノイズ除去に強い",
            "48kHz フルバンド処理により自然な声質（PESQ/STOI）を保ちやすい",
            "Apache-2.0 / MIT ライセンスで配布にも安心",
        ],
        best_for: "48kHz マイク入力の高品位ノイズ除去",
        caution: "約40msのフレーム遅延があり、現在はオプションモデル扱いです",
        min_vram_gb: 0.0,
        download_size_mb: 14,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 3, lightness: 3 },
        bundled_default: false,
        download_url: None,
        local_match: &["deepfilternet3.onnx"],
    },
    ModelCatalogEntry {
        id: "rnnoise",
        category: ModelCategory::NoiseSuppression,
        display_name: "RNNoise (内蔵ベースライン)",
        runtime: "Rust nnnoiseless (CPU)",
        benefits: &[
            "遅延約10msの超低遅延・超軽量ベースラインで即起動",
            "追加ダウンロード不要で単体バイナリに完全内蔵済み",
            "エアコン・PCファン等の定常ノイズ除去に安定",
        ],
        best_for: "定常ノイズのみの軽量用途・設定不要での即利用",
        caution: "突発的な打鍵音に弱く、強設定だと小声や子音を削りやすい",
        min_vram_gb: 0.0,
        download_size_mb: 0,
        scores: ModelScores { accuracy: 3, noise_resilience: 3, speed: 5, lightness: 5 },
        bundled_default: true,
        download_url: None,
        local_match: &[],
    },
    ModelCatalogEntry {
        id: "off-hpf",
        category: ModelCategory::NoiseSuppression,
        display_name: "OFF + 85Hz ハイパスフィルタ",
        runtime: "Rust DSP (CPU ゼロ遅延)",
        benefits: &[
            "AI加工による声質劣化・子音（サ行/タ行）の削れがゼロ",
            "遅延ゼロ・CPU負荷ゼロで STT へ元の音声を渡せる",
            "静かな部屋では STT 認識精度が最も高くなるケースが多い",
        ],
        best_for: "静かな室内・単一指向性マイク（STT経路の推奨設定）",
        caution: "周囲の生活音・キーボード打鍵音はそのまま通過します",
        min_vram_gb: 0.0,
        download_size_mb: 0,
        scores: ModelScores { accuracy: 5, noise_resilience: 1, speed: 5, lightness: 5 },
        bundled_default: true,
        download_url: None,
        local_match: &[],
    },
    // --- STT Models (Section 3.2) ---
    ModelCatalogEntry {
        id: "whisper-large-v3-turbo-q8",
        category: ModelCategory::Stt,
        display_name: "Whisper Large-v3-Turbo (Q8_0・同梱標準)",
        runtime: "whisper.cpp (Vulkan GPU / CPU)",
        benefits: &[
            "日本語精度と応答速度のバランスが優秀な標準搭載モデル",
            "追加ダウンロード不要で起動直後から即利用でき、Vulkanで高速推論",
            "ゲーム同時起動でも VRAM 消費が約1.1GBと控えめで安定",
        ],
        best_for: "日常の高速チャット・ゲーム同時起動（同梱デフォルト）",
        caution: "極端な早口・長文では Full Large-v3 や Qwen3-ASR との併用がより有利",
        min_vram_gb: 2.0,
        download_size_mb: 834,
        scores: ModelScores { accuracy: 4, noise_resilience: 4, speed: 5, lightness: 4 },
        bundled_default: true,
        download_url: Some("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin"),
        local_match: &["ggml-large-v3-turbo-q8_0.bin"],
    },
    ModelCatalogEntry {
        id: "whisper-large-v3-q8",
        category: ModelCategory::Stt,
        display_name: "Whisper Large-v3 Full (Q8_0・32層フル版)",
        runtime: "whisper.cpp (Vulkan GPU / CPU)",
        benefits: &[
            "デコーダ32層のフル版で Turbo (4層) より早口・ノイズ混在・長文での崩れが少ない",
            "initial_prompt による辞書・会話履歴ヒントが極めてよく効く",
            "CUDA不要で Radeon / GeForce / Intel Arc 共通の Vulkan 経路でそのまま高精度化",
        ],
        best_for: "早口・雑音あり・高精度重視の通常会話",
        caution: "Turbo より推論時間と VRAM（約2GB）がやや増えます",
        min_vram_gb: 4.0,
        download_size_mb: 1660,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 3, lightness: 3 },
        bundled_default: false,
        download_url: Some("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q8_0.bin"),
        local_match: &["ggml-large-v3-q8_0.bin"],
    },
    ModelCatalogEntry {
        id: "whisper-large-v3-turbo-ja-sft",
        category: ModelCategory::Stt,
        display_name: "Whisper Large-v3-Turbo 日本語SFT版",
        runtime: "whisper.cpp (Vulkan GPU / CPU)",
        benefits: &[
            "日本語の口語表現・音声入力発話へ LoRA SFT 適応済み",
            "普段の話し言葉や実務・チャット用語の日本語変換が自然になりやすい",
            "軽量・高速な Turbo アーキテクチャのまま利用可能",
        ],
        best_for: "日本語オンリーの日常会話・チャット入力",
        caution: "英語固有名詞が多めの会話では標準 Large-v3 と A/B 比較推奨",
        min_vram_gb: 2.0,
        download_size_mb: 834,
        scores: ModelScores { accuracy: 5, noise_resilience: 4, speed: 5, lightness: 4 },
        bundled_default: false,
        download_url: Some("https://huggingface.co/Holy-fox/whisper-large-v3-turbo-ja-personal-sft-v2-ggml/resolve/main/ggml-model-q8_0.bin"),
        local_match: &["ggml-whisper-turbo-ja-sft-q8_0.bin", "ggml-model-q8_0.bin"],
    },
    ModelCatalogEntry {
        id: "qwen3-asr-0.6b-int8",
        category: ModelCategory::Stt,
        display_name: "Qwen3-ASR-0.6B int8 (Audio-LLM)",
        runtime: "sherpa-onnx (DirectML GPU / CPU)",
        benefits: &[
            "外部サーバー不要・単体完結で文脈補完が強い次世代 Audio-LLM モデル",
            "Whisper とは異なる認識傾向を持ち、2エンジン合議アンサンブル (#34) に最適",
            "DirectML / CPU 両対応でノートPCや VRAM 少なめ環境でも動作",
        ],
        best_for: "単体完結の文脈補完・Whisperとの2エンジン合議・A/B比較",
        caution: "1.7B 版に比べると長文の難語補完力はやや控えめです",
        min_vram_gb: 2.0,
        download_size_mb: 680,
        scores: ModelScores { accuracy: 4, noise_resilience: 4, speed: 4, lightness: 4 },
        bundled_default: false,
        download_url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2"),
        local_match: &["sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25", "sherpa-onnx-qwen3-asr-0.6B-int8"],
    },
    ModelCatalogEntry {
        id: "qwen3-asr-1.7b-gguf",
        category: ModelCategory::Stt,
        display_name: "Qwen3-ASR-1.7B (llama-server / HTTP ASR)",
        runtime: "llama-server (Vulkan GPU / CPU)",
        benefits: &[
            "2026年オープンソース日本語最高精度帯（自然会話 WER 0.185 / CER 0.140）",
            "文脈補完と句読点付与が極めて自然で、滑舌が曖昧でも意味の通る文に復元",
            "Vulkan 対応のため全 GPU ベンダーで CUDA 不要で高速動作",
        ],
        best_for: "滑舌不安・長文自然会話・最高精度を求める環境",
        caution: "推奨 VRAM 6GB 以上。ローカル HTTP ASR サーバー (llama-server) 連携が必要です",
        min_vram_gb: 6.0,
        download_size_mb: 1650,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 4, lightness: 2 },
        bundled_default: false,
        download_url: Some("https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main/qwen3-asr-1.7b-q8_0.gguf"),
        local_match: &["qwen3-asr-1.7b-q8_0.gguf", "qwen3-asr-1.7b.gguf"],
    },
    ModelCatalogEntry {
        id: "parakeet-ja-int8",
        category: ModelCategory::Stt,
        display_name: "Parakeet-JA 0.6B int8 (超高速退避)",
        runtime: "sherpa-onnx (CPU / DirectML)",
        benefits: &[
            "圧倒的な超高速・超低負荷で GPU をほぼ使わず瞬時に文字化",
            "VRAM 消費が極めて小さく、重量級ゲーム中の VRAM 退避モデルに最適",
            "日本語特化 FastConformer 設計で短いコールアウトに強い",
        ],
        best_for: "重量級ゲーム中の VRAM 退避・短いコールアウト・ノートPC",
        caution: "句読点が少なく長い口語文は省略傾向があります",
        min_vram_gb: 0.0,
        download_size_mb: 620,
        scores: ModelScores { accuracy: 3, noise_resilience: 3, speed: 5, lightness: 5 },
        bundled_default: false,
        download_url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8.tar.bz2"),
        local_match: &["sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8"],
    },
    // --- Local LLM Post-Correction Models (Section 3.3) ---
    ModelCatalogEntry {
        id: "qwen3-1.7b-llm",
        category: ModelCategory::LlmCorrection,
        display_name: "ローカルLLM 軽量クラス (Qwen3-1.7B Q4_K_M)",
        runtime: "llama-server (Vulkan GPU / CPU)",
        benefits: &[
            "約1.2GBの省メモリでゲーム起動中の裏でも軽く動作",
            "100〜200ms 程度の超低遅延で誤字・空耳を自然な日本語へ補正",
            "VRAM 4GB〜 や CPU のみ環境でも実用的",
        ],
        best_for: "軽量な空耳補正・VRAM 4〜6GB 環境",
        caution: "複雑な文体変換よりも単純な誤変換の最小限修正向きです",
        min_vram_gb: 2.0,
        download_size_mb: 1200,
        scores: ModelScores { accuracy: 4, noise_resilience: 4, speed: 5, lightness: 4 },
        bundled_default: false,
        download_url: Some("https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf"),
        local_match: &["Qwen3-1.7B-Q4_K_M.gguf", "qwen3-1.7b-q4_k_m.gguf"],
    },
    ModelCatalogEntry {
        id: "qwen3-4b-llm",
        category: ModelCategory::LlmCorrection,
        display_name: "ローカルLLM バランスクラス (Qwen3-4B Q4_K_M)",
        runtime: "llama-server (Vulkan GPU)",
        benefits: &[
            "日本語文脈理解が高く、滑舌が崩れた空耳や2エンジン合議を高精度に統合復元",
            "直前会話文脈やチャンネル名・ゲーム辞書を踏まえた自然な補正が可能",
            "タメ口／丁寧語／ゲーム報告短縮などの口調変換 (#11) も高品質",
        ],
        best_for: "滑舌補正の決定版・2エンジン合議統合・口調変換 (VRAM 8GB+ 推奨)",
        caution: "VRAM 8GB 未満では軽量クラス (1.7B) を推奨します",
        min_vram_gb: 6.0,
        download_size_mb: 2500,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 4, lightness: 2 },
        bundled_default: false,
        download_url: Some("https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf"),
        local_match: &["Qwen3-4B-Q4_K_M.gguf", "qwen3-4b-q4_k_m.gguf"],
    },
    // --- VAD Model (Bundled Default Protection) ---
    ModelCatalogEntry {
        id: "silero-vad",
        category: ModelCategory::Vad,
        display_name: "Silero VAD (音声区間検出・同梱)",
        runtime: "sherpa-onnx (CPU / DirectML)",
        benefits: &[
            "ニューラル音声区間検出により話し始めの小声とノイズを高精度分離",
            "適応型ノイズ床と連動して息継ぎ途切れを防止",
            "単体バイナリに完全同梱済み（削除保護対象）",
        ],
        best_for: "全環境必須の標準音声区間検出",
        caution: "同梱コアコンポーネントのため個別削除不可です",
        min_vram_gb: 0.0,
        download_size_mb: 1,
        scores: ModelScores { accuracy: 5, noise_resilience: 5, speed: 5, lightness: 5 },
        bundled_default: true,
        download_url: None,
        local_match: &["silero_vad.onnx"],
    },
];

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

fn find_matched_path(models_dir: &Path, entry: &ModelCatalogEntry) -> Option<PathBuf> {
    if !models_dir.exists() {
        return None;
    }
    for name in entry.local_match {
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

pub fn compute_pc_fit(entry: &ModelCatalogEntry, hw: &HardwareProfile) -> String {
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

pub fn is_entry_in_use(entry: &ModelCatalogEntry, cfg: &AppConfig) -> bool {
    let eng = cfg.stt.engine.to_ascii_lowercase();
    let mf = cfg.stt.model_file.to_ascii_lowercase();
    let llm_mf = cfg.stt.accuracy.llm.model_file.to_ascii_lowercase();
    match entry.id {
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

pub fn build_catalog_inventory(
    models_dir: &Path,
    cfg: &AppConfig,
    hw: &HardwareProfile,
) -> Vec<ModelCatalogItemDto> {
    let mut out = Vec::new();
    let mut covered_paths: Vec<PathBuf> = Vec::new();

    for entry in MODEL_CATALOG {
        let matched = find_matched_path(models_dir, entry);
        let installed = entry.bundled_default || matched.is_some();
        let disk_size_bytes = matched
            .as_ref()
            .map(|p| dir_size_bytes(p))
            .unwrap_or(0);
        if let Some(p) = matched {
            covered_paths.push(p);
        }
        let in_use = is_entry_in_use(entry, cfg);
        out.push(ModelCatalogItemDto {
            id: entry.id.into(),
            category: entry.category,
            display_name: entry.display_name.into(),
            runtime: entry.runtime.into(),
            benefits: entry.benefits.iter().map(|s| (*s).into()).collect(),
            best_for: entry.best_for.into(),
            caution: entry.caution.into(),
            min_vram_gb: entry.min_vram_gb,
            download_size_mb: entry.download_size_mb,
            scores: entry.scores,
            bundled_default: entry.bundled_default,
            downloadable: entry.download_url.is_some(),
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
        });
    }

    // Also surface any user-downloaded/custom .bin/.gguf/.onnx models inside models_dir for individual delete!
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
                benefits: vec![
                    "ユーザーが追加・カスタムURL取得したローカルモデルファイル".into(),
                    "外部モデル管理UIから不要時にワンクリック個別削除が可能".into(),
                ],
                best_for: "特定話者・専門用語向けカスタムモデル".into(),
                caution: "動作要件は配置した量子化形式に依存します".into(),
                min_vram_gb: 2.0,
                download_size_mb: (sz / (1024 * 1024)) as u32,
                scores: ModelScores { accuracy: 4, noise_resilience: 4, speed: 4, lightness: 3 },
                bundled_default: false,
                downloadable: false,
                installed: true,
                in_use,
                can_delete: true,
                disk_size_bytes: sz,
                disk_size_label: format_bytes(sz),
                pc_fit_badge: "カスタム".into(),
            });
        }
    }
    out
}

/// Safely removes a specific model file/directory and leftover `.part`/`.tmp` fragments.
/// Bundled core defaults are strictly protected.
pub fn delete_model_from_disk(models_dir: &Path, model_id: &str) -> Result<u64, String> {
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

    let entry = MODEL_CATALOG
        .iter()
        .find(|e| e.id == model_id)
        .ok_or_else(|| format!("未知のモデルIDです: {}", model_id))?;

    if entry.bundled_default {
        return Err(format!(
            "「{}」は同梱標準モデルのため削除できません（保護対象）",
            entry.display_name
        ));
    }

    let matched = find_matched_path(models_dir, entry)
        .ok_or_else(|| format!("「{}」のローカルファイルは既に存在しません", entry.display_name))?;
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
        assert!(!MODEL_CATALOG.is_empty());
        for entry in MODEL_CATALOG {
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
    fn deletes_external_model_safely_while_protecting_bundled_default() {
        let dir = std::env::temp_dir().join(format!("voxdrop_del_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Bundled default refusal
        assert!(delete_model_from_disk(&dir, "whisper-large-v3-turbo-q8").is_err());
        assert!(delete_model_from_disk(&dir, "silero-vad").is_err());

        // Single file + .part cleanup
        let f = dir.join("ggml-large-v3-q8_0.bin");
        let f_part = dir.join("ggml-large-v3-q8_0.bin.part");
        fs::write(&f, vec![1u8; 4096]).unwrap();
        fs::write(&f_part, b"partial").unwrap();
        let freed = delete_model_from_disk(&dir, "whisper-large-v3-q8").expect("delete file");
        assert_eq!(freed, 4096);
        assert!(!f.exists());
        assert!(!f_part.exists());

        // Directory model delete (Qwen3-ASR)
        let qdir = dir.join("sherpa-onnx-qwen3-asr-0.6B-int8");
        fs::create_dir_all(qdir.join("tokenizer")).unwrap();
        fs::write(qdir.join("conv_frontend.onnx"), vec![2u8; 2048]).unwrap();
        let qfreed = delete_model_from_disk(&dir, "qwen3-asr-0.6b-int8").expect("delete dir");
        assert_eq!(qfreed, 2048);
        assert!(!qdir.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
