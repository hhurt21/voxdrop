use crate::audio::vad::TARGET_SAMPLE_RATE;
use crate::config::SttConfig;
use crate::stt::http_asr::LocalHttpAsrEngine;
use crate::stt::sherpa_ffi::{ResidentModelSpec, ResidentSherpaRecognizer, SpeechDenoiserSpec};
use crate::stt::whisper_ffi::ResidentWhisperRecognizer;
use crate::stt::{SttEngine, TranscribeContext};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static TEMP_WAV_SEQ: AtomicU64 = AtomicU64::new(1);

/// Encodes 16 kHz mono `f32` PCM samples (-1.0..=1.0) into a standard 16-bit PCM WAV byte buffer.
pub fn encode_wav_16k_mono(samples: &[f32]) -> Vec<u8> {
    let num_channels: u16 = 1;
    let sample_rate: u32 = TARGET_SAMPLE_RATE;
    let bits_per_sample: u16 = 16;
    let byte_rate: u32 = sample_rate * num_channels as u32 * (bits_per_sample as u32 / 8);
    let block_align: u16 = num_channels * (bits_per_sample / 8);
    let data_bytes: u32 = (samples.len() * 2) as u32;
    let riff_size: u32 = 36 + data_bytes;

    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&num_channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());

    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let pcm = (clamped * 32767.0).round() as i16;
        out.extend_from_slice(&pcm.to_le_bytes());
    }
    out
}

fn write_temp_wav(samples: &[f32]) -> Result<PathBuf, String> {
    let seq = TEMP_WAV_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("voxdrop_utt_{}_{}.wav", std::process::id(), seq));
    let bytes = encode_wav_16k_mono(samples);
    fs::write(&path, bytes).map_err(|e| format!("一時WAVの書き込みに失敗しました: {}", e))?;
    Ok(path)
}

#[derive(Debug, Clone)]
enum SherpaModelKind {
    Whisper {
        encoder: PathBuf,
        decoder: PathBuf,
        tokens: PathBuf,
    },
    NemoCtc {
        model: PathBuf,
        tokens: PathBuf,
    },
    SenseVoice {
        model: PathBuf,
        tokens: PathBuf,
    },
    Moonshine {
        encoder: PathBuf,
        merged_decoder: PathBuf,
        tokens: PathBuf,
    },
    Transducer {
        encoder: PathBuf,
        decoder: PathBuf,
        joiner: PathBuf,
        tokens: PathBuf,
    },
    Paraformer {
        model: PathBuf,
        tokens: PathBuf,
    },
    Qwen3Asr {
        conv_frontend: PathBuf,
        encoder: PathBuf,
        decoder: PathBuf,
        tokenizer: PathBuf,
    },
}

/// Returns user-writable external models directory (`%LOCALAPPDATA%\VoxDrop\models`).
pub fn user_models_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let trimmed = local.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("VoxDrop").join("models");
        }
    }
    std::env::temp_dir().join("VoxDrop").join("models")
}

/// Checks whether a sherpa-onnx Qwen3-ASR model directory is installed under any model search dir.
pub fn has_qwen3_asr_model(cfg: &SttConfig, runtime_dir: Option<&Path>) -> bool {
    resolve_model_search_dirs(&cfg.model_dir, runtime_dir)
        .into_iter()
        .any(|dir| discover_sherpa_qwen3_model(&dir).is_some())
}

/// Resolves all existing model directories to search across (ordered by priority, deduplicated).
pub fn resolve_model_search_dirs(configured: &str, runtime_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut raw_candidates: Vec<PathBuf> = Vec::new();
    let trimmed = configured.trim();
    if let Some(p_dir) = crate::config::AppConfig::portable_dir() {
        let sub = if trimmed.is_empty() {
            p_dir.join("models")
        } else if Path::new(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else {
            p_dir.join(trimmed)
        };
        raw_candidates.push(sub);
    }
    if !trimmed.is_empty() {
        let raw = PathBuf::from(trimmed);
        raw_candidates.push(raw.clone());
        if raw.is_relative() {
            if let Some(dir) = exe_dir() {
                raw_candidates.push(dir.join(&raw));
                if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
                    raw_candidates.push(parent.join(&raw));
                }
            }
        }
    } else {
        raw_candidates.push(PathBuf::from("models"));
        if let Some(dir) = exe_dir() {
            raw_candidates.push(dir.join("models"));
        }
    }
    raw_candidates.push(user_models_dir());
    if let Some(rt) = runtime_dir {
        raw_candidates.push(rt.join("models"));
    }
    raw_candidates.push(crate::runtime::runtime_cache_dir().join("models"));

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cand in raw_candidates {
        if !cand.exists() || !cand.is_dir() {
            continue;
        }
        let norm = fs::canonicalize(&cand)
            .unwrap_or_else(|_| cand.clone())
            .to_string_lossy()
            .to_ascii_lowercase();
        if seen.insert(norm) {
            out.push(cand);
        }
    }
    out
}

/// Resolves the writable target directory for downloading external models.
pub fn resolve_download_model_dir(configured: &str, runtime_dir: Option<&Path>) -> PathBuf {
    let trimmed = configured.trim();
    if let Some(p_dir) = crate::config::AppConfig::portable_dir() {
        return if trimmed.is_empty() {
            p_dir.join("models")
        } else if Path::new(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else {
            p_dir.join(trimmed)
        };
    }
    let raw = PathBuf::from(trimmed);
    if !trimmed.is_empty() && raw.is_absolute() {
        return raw;
    }
    if runtime_dir.is_some() {
        return user_models_dir();
    }
    if !trimmed.is_empty() && raw.exists() {
        return raw;
    }
    if let Some(dir) = exe_dir() {
        let candidate = dir.join(if trimmed.is_empty() { "models" } else { trimmed });
        if candidate.exists() {
            return candidate;
        }
    }
    user_models_dir()
}

pub fn list_available_llm_gguf_models(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in resolve_model_search_dirs(&cfg.model_dir, runtime_dir) {
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                        let lower = name.to_ascii_lowercase();
                        if lower.ends_with(".gguf") && !lower.contains("asr") && seen.insert(lower) {
                            out.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out
}

pub fn find_llm_model_path(
    model_file: &str,
    cfg: &SttConfig,
    runtime_dir: Option<&Path>,
) -> Option<PathBuf> {
    let requested = model_file.trim();
    if !requested.is_empty() {
        let direct = Path::new(requested);
        if direct.is_file() {
            return Some(direct.to_path_buf());
        }
    }
    let dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
    if !requested.is_empty() {
        for d in &dirs {
            let candidate = d.join(requested);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for d in &dirs {
        if let Ok(entries) = fs::read_dir(d) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                        let lower = name.to_ascii_lowercase();
                        if lower.ends_with(".gguf") && !lower.contains("asr") {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn discover_llama_server_exe(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
    let dl_dir = resolve_download_model_dir(&cfg.model_dir, runtime_dir);
    if !dirs.iter().any(|d| d == &dl_dir) {
        dirs.insert(0, dl_dir);
    }
    for d in &dirs {
        for rel in [
            "llama-server/llama-server.exe",
            "llama-server.exe",
            "llama-b12811-bin-win-vulkan-x64/llama-server.exe",
        ] {
            let p = d.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
        // Scan 1-level subdirectories in case archive unpacked under a versioned folder name
        if let Ok(entries) = fs::read_dir(d) {
            for e in entries.flatten() {
                let sub = e.path();
                if sub.is_dir() {
                    let cand = sub.join("llama-server.exe");
                    if cand.is_file() {
                        return Some(cand);
                    }
                    let nested = sub.join("build").join("bin").join("llama-server.exe");
                    if nested.is_file() {
                        return Some(nested);
                    }
                }
            }
        }
    }
    find_file_in_search_dirs("", &["llama-server.exe", "llama-server"], runtime_dir)
}

pub fn discover_stt_denoiser(
    kind: crate::config::NoiseEngineKind,
    cfg: &SttConfig,
    runtime_dir: Option<&Path>,
    attenuation_limit_db: f32,
) -> Option<Arc<crate::stt::sherpa_ffi::ResidentSherpaDenoiser>> {
    use crate::config::NoiseEngineKind;
    let names: &[&str] = match kind {
        NoiseEngineKind::DpdfNet16k => &[
            "dpdfnet2.onnx",
            "dpdfnet_baseline.onnx",
            "dpdfnet_baseline_16k.onnx",
        ],
        NoiseEngineKind::DpdfNet48k => &["dpdfnet2_48khz_hr.onnx", "dpdfnet_48k.onnx"],
        NoiseEngineKind::Gtcrn16k => &["gtcrn_simple.onnx", "gtcrn.onnx"],
        NoiseEngineKind::DeepFilterNet3 => &["deepfilternet3.onnx"],
        NoiseEngineKind::Rnnoise | NoiseEngineKind::OffHpf => return None,
    };
    let dll = find_file_in_search_dirs("", &["sherpa-onnx-c-api.dll"], runtime_dir)?;
    let search_dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
    let model_path = search_dirs
        .iter()
        .find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))?;
    crate::stt::sherpa_ffi::ResidentSherpaDenoiser::load(
        &dll,
        kind.as_str(),
        &model_path,
        attenuation_limit_db,
        cfg.threads.clamp(1, 4),
    )
    .ok()
    .map(Arc::new)
}

/// Lists available GGML Whisper `.bin` model filenames ordered by quality priority.
pub fn list_available_ggml_models(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for model_dir in resolve_model_search_dirs(&cfg.model_dir, runtime_dir) {
        for p in collect_ggml_models(&model_dir) {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if seen.insert(name.to_ascii_lowercase()) {
                    candidates.push(p);
                }
            }
        }
    }
    candidates.sort_by(|a, b| score_ggml_model(b).cmp(&score_ggml_model(a)));
    candidates
        .into_iter()
        .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()))
        .collect()
}

fn collect_ggml_models(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                let l = name.to_ascii_lowercase();
                if l.ends_with(".bin") {
                    found.push(path);
                }
            }
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }
    for sub in subdirs {
        if let Ok(sub_entries) = fs::read_dir(sub) {
            for entry in sub_entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                        let l = name.to_ascii_lowercase();
                        if l.ends_with(".bin") {
                            found.push(path);
                        }
                    }
                }
            }
        }
    }
    found
}

fn score_ggml_model(path: &Path) -> i32 {
    let l = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let base = if l.contains("large-v3-turbo") || l.contains("turbo") {
        1000
    } else if l.contains("large-v3") {
        800
    } else if l.contains("kotoba") {
        600
    } else if l.contains("medium") {
        400
    } else if l.contains("small") {
        300
    } else {
        200
    };
    let quant = if l.contains("f16") {
        40
    } else if l.contains("q8_0") || l.contains("q8") {
        35
    } else if l.contains("q5_0") || l.contains("q5") {
        20
    } else if l.contains("q4") {
        10
    } else {
        0
    };
    base + quant
}

/// Ultra-fast Vulkan-GPU / AVX2-CPU resident `whisper.dll` engine (`Whisper Large-v3-Turbo` / `Kotoba-Whisper v2.0`).
pub struct WhisperResidentEngine {
    label: String,
    recognizer: ResidentWhisperRecognizer,
}

impl WhisperResidentEngine {
    pub fn discover(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Result<Self, String> {
        let dll_path = find_file_in_search_dirs("", &["whisper.dll"], runtime_dir)
            .ok_or_else(|| "whisper.dll が実行ディレクトリまたはキャッシュに見つかりません".to_string())?;
        let dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
        let mut candidates = Vec::new();
        for model_dir in &dirs {
            candidates.extend(collect_ggml_models(model_dir));
        }
        if candidates.is_empty() {
            let searched = dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "GGML .bin モデルが見つかりません (探索先: {})",
                if searched.is_empty() { cfg.model_dir.as_str() } else { &searched }
            ));
        }
        candidates.sort_by(|a, b| score_ggml_model(b).cmp(&score_ggml_model(a)));

        let requested = cfg.model_file.trim();
        let model_path = if !requested.is_empty() {
            candidates
                .iter()
                .find(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.eq_ignore_ascii_case(requested))
                        .unwrap_or(false)
                })
                .cloned()
                .unwrap_or_else(|| candidates[0].clone())
        } else {
            candidates[0].clone()
        };

        let language = if cfg.language.trim().is_empty() {
            "ja"
        } else {
            cfg.language.trim()
        };

        let model_short = model_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper")
            .to_ascii_lowercase();
        let model_title = if model_short.contains("turbo") {
            if model_short.contains("q8") {
                "Whisper Large-v3-Turbo (Q8)"
            } else if model_short.contains("q5") {
                "Whisper Large-v3-Turbo (Q5)"
            } else {
                "Whisper Large-v3-Turbo"
            }
        } else if model_short.contains("kotoba") {
            "Kotoba-Whisper v2.0 日本語特化"
        } else if model_short.contains("large-v3") {
            "Whisper Large-v3"
        } else {
            "Whisper GGML"
        };

        let recognizer = ResidentWhisperRecognizer::load(&dll_path, &model_path, language, cfg.threads)?;
        let accel = if recognizer.is_gpu_accelerated() {
            "Vulkan GPU高速"
        } else {
            "AVX2 CPU"
        };
        Ok(Self {
            label: format!("{} ({})", model_title, accel),
            recognizer,
        })
    }
}

impl SttEngine for WhisperResidentEngine {
    fn id(&self) -> &'static str {
        "whisper-cpp"
    }

    fn name(&self) -> String {
        self.label.clone()
    }

    fn device_name(&self) -> String {
        self.recognizer.device_name().to_string()
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        self.recognizer.transcribe(samples_16k)
    }

    fn transcribe_enhanced(
        &self,
        samples_16k: &[f32],
        radio_clarity: bool,
    ) -> Result<String, String> {
        self.recognizer
            .transcribe_with_clarity(samples_16k, radio_clarity)
    }

    fn transcribe_with_context(
        &self,
        samples_16k: &[f32],
        ctx: &TranscribeContext,
    ) -> Result<String, String> {
        if let Some(ref prompt) = ctx.hotwords_prompt {
            self.recognizer.set_prompt_hints(prompt);
        }
        self.recognizer.transcribe_with_options(
            samples_16k,
            ctx.radio_clarity,
            ctx.eq_strength,
            ctx.head_pad_ms,
            ctx.tail_pad_ms,
            ctx.beam_size,
            ctx.temperature_fallback,
        )
    }

    fn set_prompt_hints(&self, hints: &str) {
        self.recognizer.set_prompt_hints(hints);
    }
}

/// Ultra-fast resident in-memory `sherpa-onnx-c-api.dll` STT engine (zero reload latency).
pub struct SherpaResidentEngine {
    label: String,
    #[allow(dead_code)]
    is_qwen3: bool,
    recognizer: ResidentSherpaRecognizer,
}

impl SherpaResidentEngine {
    pub fn discover_qwen3(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Result<Self, String> {
        let dll_path = find_file_in_search_dirs(
            "",
            &["sherpa-onnx-c-api.dll"],
            runtime_dir,
        )
        .ok_or_else(|| "sherpa-onnx-c-api.dll が見つかりません".to_string())?;
        let dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
        let SherpaModelKind::Qwen3Asr {
            conv_frontend,
            encoder,
            decoder,
            tokenizer,
        } = dirs.iter().find_map(|d| discover_sherpa_qwen3_model(d))
            .ok_or_else(|| {
                "Qwen3-ASR モデル（conv_frontend.onnx / encoder / decoder / tokenizer）が見つかりません（詳細設定からダウンロードしてください）".to_string()
            })?
        else {
            return Err("Qwen3-ASR モデル種別が一致しません".into());
        };

        let spec = ResidentModelSpec::Qwen3Asr {
            conv_frontend: &conv_frontend,
            encoder: &encoder,
            decoder: &decoder,
            tokenizer: &tokenizer,
            hotwords: "",
        };
        let recognizer = ResidentSherpaRecognizer::load(&dll_path, spec, None, cfg.threads)?;
        let label = format!("sherpa-onnx Qwen3-ASR 0.6B ({})", recognizer.device_name());
        Ok(Self {
            label,
            is_qwen3: true,
            recognizer,
        })
    }

    pub fn discover(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Result<Self, String> {
        let dll_path = find_file_in_search_dirs(
            "",
            &["sherpa-onnx-c-api.dll"],
            runtime_dir,
        )
        .ok_or_else(|| "sherpa-onnx-c-api.dll が見つかりません".to_string())?;
        let dirs = resolve_model_search_dirs(&cfg.model_dir, runtime_dir);
        let model_kind = dirs
            .iter()
            .find_map(|d| discover_sherpa_model(d))
            .ok_or_else(|| {
                format!(
                    "sherpa-onnx 対応モデルが見つかりません (探索先: {})",
                    dirs.iter()
                        .map(|d| d.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let language = if cfg.language.trim().is_empty() {
            "ja"
        } else {
            cfg.language.trim()
        };

        let is_qwen3 = matches!(model_kind, SherpaModelKind::Qwen3Asr { .. });
        let (label, spec) = match &model_kind {
            SherpaModelKind::Qwen3Asr {
                conv_frontend,
                encoder,
                decoder,
                tokenizer,
            } => (
                "sherpa-onnx Qwen3-ASR 0.6B".to_string(),
                ResidentModelSpec::Qwen3Asr {
                    conv_frontend,
                    encoder,
                    decoder,
                    tokenizer,
                    hotwords: "",
                },
            ),
            SherpaModelKind::Whisper {
                encoder,
                decoder,
                tokens,
            } => (
                "sherpa-onnx 常駐高精度 (Whisper Large-v3-Turbo)".to_string(),
                ResidentModelSpec::Whisper {
                    encoder,
                    decoder,
                    tokens,
                    language,
                },
            ),
            SherpaModelKind::NemoCtc { model, tokens } => (
                "sherpa-onnx 常駐高速 (Parakeet-Ja)".to_string(),
                ResidentModelSpec::NemoCtc { model, tokens },
            ),
            SherpaModelKind::SenseVoice { model, tokens } => (
                "sherpa-onnx 常駐高速 (SenseVoice-Ja)".to_string(),
                ResidentModelSpec::SenseVoice {
                    model,
                    tokens,
                    language,
                },
            ),
            SherpaModelKind::Moonshine {
                encoder,
                merged_decoder,
                tokens,
            } => (
                "sherpa-onnx 常駐高速 (Moonshine-Ja)".to_string(),
                ResidentModelSpec::Moonshine {
                    encoder,
                    merged_decoder,
                    tokens,
                },
            ),
            SherpaModelKind::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            } => (
                "sherpa-onnx 常駐高速 (ReazonSpeech)".to_string(),
                ResidentModelSpec::Transducer {
                    encoder,
                    decoder,
                    joiner,
                    tokens,
                },
            ),
            SherpaModelKind::Paraformer { model, tokens } => (
                "sherpa-onnx 常駐高速 (Paraformer)".to_string(),
                ResidentModelSpec::Paraformer { model, tokens },
            ),
        };

        // Prefer DPDFNet2 (high-fidelity neural speech enhancement with safe attenuation limit) over GTCRN
        let dpdfnet_path = dirs.iter().find_map(|d| {
            find_file_matching(d, |n| {
                let l = n.to_ascii_lowercase();
                l.starts_with("dpdfnet") && l.ends_with(".onnx")
            })
        });
        let gtcrn_path = dirs.iter().find_map(|d| {
            find_file_matching(d, |n| {
                n.eq_ignore_ascii_case("gtcrn_simple.onnx")
            })
        });
        let denoiser_spec = if let Some(ref p) = dpdfnet_path {
            Some(SpeechDenoiserSpec::DpdfNet {
                model: p.as_path(),
                attenuation_limit_db: 12.0,
            })
        } else if let Some(ref p) = gtcrn_path {
            Some(SpeechDenoiserSpec::Gtcrn { model: p.as_path() })
        } else {
            None
        };

        let recognizer = ResidentSherpaRecognizer::load(&dll_path, spec, denoiser_spec, cfg.threads)?;
        let full_label = if is_qwen3 {
            format!("{} ({})", label, recognizer.device_name())
        } else if let Some(dn) = recognizer.denoiser_name() {
            format!("{} + {}", label, dn)
        } else {
            label
        };
        Ok(Self {
            label: full_label,
            is_qwen3,
            recognizer,
        })
    }
}

impl SttEngine for SherpaResidentEngine {
    fn id(&self) -> &'static str {
        if self.is_qwen3 {
            "sherpa-qwen3"
        } else {
            "sherpa-onnx"
        }
    }

    fn name(&self) -> String {
        self.label.clone()
    }

    fn device_name(&self) -> String {
        self.recognizer.device_name().to_string()
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        let json = self.recognizer.transcribe_json(samples_16k)?;
        Ok(parse_sherpa_output(&json))
    }

    fn transcribe_enhanced(
        &self,
        samples_16k: &[f32],
        radio_clarity: bool,
    ) -> Result<String, String> {
        let json = self
            .recognizer
            .transcribe_json_enhanced(samples_16k, radio_clarity)?;
        Ok(parse_sherpa_output(&json))
    }
}

/// Fallback CLI engine when `sherpa-onnx-offline.exe` is used instead of the C API DLL.
pub struct SherpaOnnxEngine {
    exe_path: PathBuf,
    model_kind: SherpaModelKind,
    language: String,
    threads: u32,
}

impl SherpaOnnxEngine {
    pub fn discover(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Option<Self> {
        let exe_path = find_file_in_search_dirs(
            &cfg.cli_path,
            &["sherpa-onnx-offline.exe", "sherpa-onnx-offline"],
            runtime_dir,
        )?;
        let model_kind = resolve_model_search_dirs(&cfg.model_dir, runtime_dir)
            .into_iter()
            .find_map(|d| discover_sherpa_model(&d))?;
        Some(Self {
            exe_path,
            model_kind,
            language: if cfg.language.trim().is_empty() {
                "ja".into()
            } else {
                cfg.language.clone()
            },
            threads: cfg.threads.clamp(1, 16),
        })
    }
}

impl SttEngine for SherpaOnnxEngine {
    fn name(&self) -> String {
        match &self.model_kind {
            SherpaModelKind::Whisper { .. } => "sherpa-onnx CLI (Whisper Large-v3-Turbo)".into(),
            SherpaModelKind::NemoCtc { .. } => "sherpa-onnx CLI (Parakeet-Ja)".into(),
            SherpaModelKind::SenseVoice { .. } => "sherpa-onnx CLI (SenseVoice-Ja)".into(),
            SherpaModelKind::Moonshine { .. } => "sherpa-onnx CLI (Moonshine-Ja)".into(),
            SherpaModelKind::Transducer { .. } => "sherpa-onnx CLI (ReazonSpeech)".into(),
            SherpaModelKind::Paraformer { .. } => "sherpa-onnx CLI (Paraformer)".into(),
            SherpaModelKind::Qwen3Asr { .. } => "sherpa-onnx CLI (Qwen3-ASR)".into(),
        }
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        let wav_path = write_temp_wav(samples_16k)?;
        let mut cmd = Command::new(&self.exe_path);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.arg(format!("--num-threads={}", self.threads));
        match &self.model_kind {
            SherpaModelKind::Whisper {
                encoder,
                decoder,
                tokens,
            } => {
                cmd.arg(format!("--whisper-encoder={}", encoder.display()));
                cmd.arg(format!("--whisper-decoder={}", decoder.display()));
                cmd.arg(format!("--whisper-language={}", self.language));
                cmd.arg("--whisper-task=transcribe");
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::NemoCtc { model, tokens } => {
                cmd.arg(format!("--nemo-ctc-model={}", model.display()));
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::SenseVoice { model, tokens } => {
                cmd.arg(format!("--sense-voice-model={}", model.display()));
                cmd.arg(format!("--sense-voice-language={}", self.language));
                cmd.arg("--sense-voice-use-itn=1");
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::Moonshine {
                encoder,
                merged_decoder,
                tokens,
            } => {
                cmd.arg(format!("--moonshine-encoder={}", encoder.display()));
                cmd.arg(format!(
                    "--moonshine-merged-decoder={}",
                    merged_decoder.display()
                ));
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            } => {
                cmd.arg(format!("--encoder={}", encoder.display()));
                cmd.arg(format!("--decoder={}", decoder.display()));
                cmd.arg(format!("--joiner={}", joiner.display()));
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::Paraformer { model, tokens } => {
                cmd.arg(format!("--paraformer={}", model.display()));
                cmd.arg(format!("--tokens={}", tokens.display()));
            }
            SherpaModelKind::Qwen3Asr {
                conv_frontend,
                encoder,
                decoder,
                tokenizer,
            } => {
                cmd.arg(format!("--qwen3-asr-conv-frontend={}", conv_frontend.display()));
                cmd.arg(format!("--qwen3-asr-encoder={}", encoder.display()));
                cmd.arg(format!("--qwen3-asr-decoder={}", decoder.display()));
                cmd.arg(format!("--qwen3-asr-tokenizer={}", tokenizer.display()));
            }
        }
        cmd.arg(&wav_path);

        let output = cmd.output();
        let _ = fs::remove_file(&wav_path);

        let output = output.map_err(|e| format!("sherpa-onnx 実行エラー: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "sherpa-onnx がエラー終了しました ({}): {}",
                output.status,
                stderr.trim()
            ));
        }

        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(parse_sherpa_output(&combined))
    }
}

pub struct WhisperCliEngine {
    exe_path: PathBuf,
    model_path: PathBuf,
    language: String,
    threads: u32,
}

impl WhisperCliEngine {
    pub fn discover(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Option<Self> {
        let exe_path = find_file_in_search_dirs(
            &cfg.cli_path,
            &["whisper-cli.exe", "whisper-cli", "main.exe"],
            runtime_dir,
        )?;
        let model_path = resolve_model_search_dirs(&cfg.model_dir, runtime_dir)
            .into_iter()
            .find_map(|d| {
                find_file_matching(&d, |name| {
                    let lower = name.to_ascii_lowercase();
                    lower.ends_with(".bin") && (lower.contains("ggml") || lower.contains("whisper"))
                })
            })?;
        Some(Self {
            exe_path,
            model_path,
            language: if cfg.language.trim().is_empty() {
                "ja".into()
            } else {
                cfg.language.clone()
            },
            threads: cfg.threads.clamp(1, 16),
        })
    }
}

impl SttEngine for WhisperCliEngine {
    fn name(&self) -> String {
        "whisper-cli (local)".into()
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        let wav_path = write_temp_wav(samples_16k)?;
        let mut cmd = Command::new(&self.exe_path);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.arg("-m")
            .arg(&self.model_path)
            .arg("-f")
            .arg(&wav_path)
            .arg("-l")
            .arg(&self.language)
            .arg("-t")
            .arg(self.threads.to_string())
            .arg("-nt")
            .arg("-np");

        let output = cmd.output();
        let _ = fs::remove_file(&wav_path);

        let output = output.map_err(|e| format!("whisper-cli 実行エラー: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "whisper-cli がエラー終了しました ({}): {}",
                output.status,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_whisper_output(&stdout))
    }
}

pub struct MockSttEngine {
    explicit_mock: bool,
    reason: String,
}

impl SttEngine for MockSttEngine {
    fn name(&self) -> String {
        if self.explicit_mock {
            "mock (テストモード)".into()
        } else {
            "未設定 (モデル未検出)".into()
        }
    }

    fn is_ready(&self) -> bool {
        self.explicit_mock
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        if self.explicit_mock {
            let sec = samples_16k.len() as f32 / TARGET_SAMPLE_RATE as f32;
            Ok(format!("テスト発話({:.1}秒)", sec))
        } else {
            Err(self.reason.clone())
        }
    }
}

pub fn create_local_engine(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Arc<dyn SttEngine> {
    let mode = cfg.engine.trim().to_ascii_lowercase();
    match mode.as_str() {
        "mock" => Arc::new(MockSttEngine {
            explicit_mock: true,
            reason: String::new(),
        }),
        "whisper-cpp" | "whisper" => {
            match WhisperResidentEngine::discover(cfg, runtime_dir) {
                Ok(e) => Arc::new(e),
                Err(res_err) => {
                    if let Some(e) = WhisperCliEngine::discover(cfg, runtime_dir) {
                        Arc::new(e)
                    } else {
                        Arc::new(MockSttEngine {
                            explicit_mock: false,
                            reason: res_err,
                        })
                    }
                }
            }
        }
        "sherpa-qwen3" | "qwen3-asr" => {
            match SherpaResidentEngine::discover_qwen3(cfg, runtime_dir) {
                Ok(e) => Arc::new(e),
                Err(err) => Arc::new(MockSttEngine {
                    explicit_mock: false,
                    reason: err,
                }),
            }
        }
        "local-http" | "http-asr" => {
            Arc::new(LocalHttpAsrEngine::new(cfg.http_asr.clone(), &cfg.language))
        }
        "sherpa-onnx" => {
            match SherpaResidentEngine::discover(cfg, runtime_dir) {
                Ok(e) => Arc::new(e),
                Err(res_err) => {
                    if let Some(e) = SherpaOnnxEngine::discover(cfg, runtime_dir) {
                        Arc::new(e)
                    } else {
                        Arc::new(MockSttEngine {
                            explicit_mock: false,
                            reason: res_err,
                        })
                    }
                }
            }
        }
        "whisper-cli" => {
            if let Some(e) = WhisperCliEngine::discover(cfg, runtime_dir) {
                Arc::new(e)
            } else {
                Arc::new(MockSttEngine {
                    explicit_mock: false,
                    reason: format!(
                        "whisper-cli または ggml-*.bin モデルが '{}' に見つかりません",
                        cfg.model_dir
                    ),
                })
            }
        }
        _ => {
            // Prefer Vulkan-GPU Resident Whisper first, then Resident sherpa-onnx DLL, then CLI fallbacks.
            match WhisperResidentEngine::discover(cfg, runtime_dir) {
                Ok(e) => Arc::new(e),
                Err(w_err) => match SherpaResidentEngine::discover(cfg, runtime_dir) {
                    Ok(e) => Arc::new(e),
                    Err(s_err) => {
                        if let Some(e) = SherpaOnnxEngine::discover(cfg, runtime_dir) {
                            Arc::new(e)
                        } else if let Some(e) = WhisperCliEngine::discover(cfg, runtime_dir) {
                            Arc::new(e)
                        } else {
                            Arc::new(MockSttEngine {
                                explicit_mock: false,
                                reason: format!("{} / {}", w_err, s_err),
                            })
                        }
                    }
                },
            }
        }
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn find_file_in_search_dirs(
    explicit_path: &str,
    candidates: &[&str],
    runtime_dir: Option<&Path>,
) -> Option<PathBuf> {
    let trimmed = explicit_path.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut search_dirs: Vec<PathBuf> = vec![PathBuf::from("bin"), PathBuf::from(".")];
    if let Some(dir) = exe_dir() {
        search_dirs.push(dir.join("bin"));
        search_dirs.push(dir.clone());
        if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
            search_dirs.push(parent.join("bin"));
        }
    }
    if let Some(rt) = runtime_dir {
        search_dirs.push(rt.join("bin"));
        search_dirs.push(rt.to_path_buf());
    }
    let shared_bin = crate::runtime::runtime_cache_dir().join("bin");
    search_dirs.push(shared_bin);
    for dir in &search_dirs {
        for name in candidates {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_env) {
            for name in candidates {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn discover_sherpa_qwen3_model(dir: &Path) -> Option<SherpaModelKind> {
    if !dir.exists() {
        return None;
    }
    let mut subdirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                subdirs.push(e.path());
            }
        }
    }
    subdirs.sort();
    let mut search_roots = vec![dir.to_path_buf()];
    search_roots.extend(subdirs);

    for root in &search_roots {
        let conv_frontend = root.join("conv_frontend.onnx");
        if !conv_frontend.is_file() {
            continue;
        }
        let encoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("encoder") && l.ends_with(".int8.onnx")
        })
        .or_else(|| {
            find_direct_file(root, |n| {
                let l = n.to_ascii_lowercase();
                l.starts_with("encoder") && l.ends_with(".onnx")
            })
        });
        let decoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("decoder") && l.ends_with(".int8.onnx")
        })
        .or_else(|| {
            find_direct_file(root, |n| {
                let l = n.to_ascii_lowercase();
                l.starts_with("decoder") && l.ends_with(".onnx")
            })
        });
        let tokenizer = root.join("tokenizer");
        if tokenizer.is_dir() {
            if let (Some(encoder), Some(decoder)) = (encoder, decoder) {
                return Some(SherpaModelKind::Qwen3Asr {
                    conv_frontend,
                    encoder,
                    decoder,
                    tokenizer,
                });
            }
        }
    }
    None
}

fn discover_sherpa_model(dir: &Path) -> Option<SherpaModelKind> {
    if !dir.exists() {
        return None;
    }

    let mut subdirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                subdirs.push(e.path());
            }
        }
    }
    // Sort ascending so 2024-07-17 comes before 2025-09-09
    subdirs.sort();
    let mut search_roots = vec![dir.to_path_buf()];
    search_roots.extend(subdirs);

    // 0. Highest priority: Whisper Large-v3-Turbo / Whisper ONNX (best Japanese accuracy on slurred/noisy/radio audio!)
    for root in &search_roots {
        let encoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.contains("encoder") && l.ends_with(".int8.onnx")
        })
        .or_else(|| {
            find_direct_file(root, |n| {
                let l = n.to_ascii_lowercase();
                l.contains("encoder") && l.ends_with(".onnx")
            })
        });
        let decoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.contains("decoder") && l.ends_with(".int8.onnx")
        })
        .or_else(|| {
            find_direct_file(root, |n| {
                let l = n.to_ascii_lowercase();
                l.contains("decoder") && l.ends_with(".onnx")
            })
        });
        let tokens = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.ends_with("tokens.txt")
        });
        let has_joiner = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("joiner") && l.ends_with(".onnx")
        })
        .is_some();
        let has_conv = root.join("conv_frontend.onnx").is_file();
        if !has_joiner && !has_conv {
            if let (Some(encoder), Some(decoder), Some(tokens)) = (encoder, decoder, tokens) {
                return Some(SherpaModelKind::Whisper {
                    encoder,
                    decoder,
                    tokens,
                });
            }
        }
    }

    // 1. Prefer SenseVoice-2024-07-17 (fast fallback; avoid 2025-09-09 which has upstream <|yue|> ja regression)
    //    Also support NeMo Parakeet CTC if explicitly placed by user.

    for root in &search_roots {
        let tokens = root.join("tokens.txt");
        if !tokens.is_file() {
            continue;
        }
        let m_int8 = root.join("model.int8.onnx");
        let m_fp = root.join("model.onnx");
        let root_lower = root.to_string_lossy().to_ascii_lowercase();
        // Skip broken upstream 2025-09-09 SenseVoice for Japanese if another model exists
        if root_lower.contains("sense-voice") && root_lower.contains("2025-09-09") {
            continue;
        }
        if m_int8.is_file() || m_fp.is_file() {
            let model = if m_int8.is_file() { m_int8 } else { m_fp };
            if root_lower.contains("nemo") || root_lower.contains("parakeet") {
                return Some(SherpaModelKind::NemoCtc { model, tokens });
            } else if root_lower.contains("paraformer") {
                return Some(SherpaModelKind::Paraformer { model, tokens });
            } else {
                return Some(SherpaModelKind::SenseVoice { model, tokens });
            }
        }
    }

    // 2. Transducer (Zipformer / ReazonSpeech)
    for root in &search_roots {
        let tokens = root.join("tokens.txt");
        if !tokens.is_file() {
            continue;
        }
        let encoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("encoder") && l.ends_with(".onnx")
        });
        let decoder = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("decoder") && l.ends_with(".onnx")
        });
        let joiner = find_direct_file(root, |n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("joiner") && l.ends_with(".onnx")
        });
        if let (Some(encoder), Some(decoder), Some(joiner)) = (encoder, decoder, joiner) {
            return Some(SherpaModelKind::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            });
        }
    }

    // 3. Fallback to Moonshine merged-decoder model if present
    for root in &search_roots {
        let tokens = root.join("tokens.txt");
        let enc = root.join("encoder_model.ort");
        let dec = root.join("decoder_model_merged.ort");
        if tokens.is_file() && enc.is_file() && dec.is_file() {
            return Some(SherpaModelKind::Moonshine {
                encoder: enc,
                merged_decoder: dec,
                tokens,
            });
        }
    }

    None
}

fn find_direct_file<F>(dir: &Path, predicate: F) -> Option<PathBuf>
where
    F: Fn(&str) -> bool,
{
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if predicate(name) {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn find_file_matching<F>(dir: &Path, predicate: F) -> Option<PathBuf>
where
    F: Fn(&str) -> bool + Copy,
{
    let entries = fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if predicate(name) {
                    return Some(path);
                }
            }
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }
    for sub in subdirs {
        if let Ok(sub_entries) = fs::read_dir(sub) {
            for entry in sub_entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                        if predicate(name) {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Parses `sherpa-onnx` JSON output (`{"text": "..."}`) or CLI text and removes special tags.
pub fn parse_sherpa_output(output: &str) -> String {
    let mut extracted = String::new();
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
                    extracted = t.to_string();
                    break;
                }
            }
        }
    }

    if extracted.is_empty() {
        for line in output.lines().rev() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("----")
                || trimmed.starts_with("Elapsed")
                || trimmed.starts_with("Real time")
                || trimmed.starts_with("Wave duration")
                || trimmed.ends_with(".wav")
                || trimmed.contains("OfflineRecognizerConfig")
            {
                continue;
            }
            extracted = trimmed.to_string();
            break;
        }
    }

    collapse_japanese_spaces(&strip_sensevoice_tags(&extracted))
}

/// Removes spurious ASCII spaces inserted by SenseVoice/BPE between Japanese characters
/// (e.g. `"聞こえ ま すか 送信 して。"` -> `"聞こえますか送信して。"`),
/// while preserving spaces between ASCII/English words (`"hello world"`).
pub fn collapse_japanese_spaces(input: &str) -> String {
    let chars: Vec<char> = input.trim().chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' {
            let prev_ascii_word = out
                .chars()
                .last()
                .map(|p| p.is_ascii_alphanumeric())
                .unwrap_or(false);
            let next_ascii_word = chars
                .get(i + 1)
                .map(|n| n.is_ascii_alphanumeric())
                .unwrap_or(false);
            if prev_ascii_word && next_ascii_word {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn strip_sensevoice_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("|>") {
            rest = &rest[start + end + 2..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

pub fn parse_whisper_output(stdout: &str) -> String {
    let cleaned: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("[BLANK_AUDIO]") && !l.starts_with('('))
        .collect();
    cleaned.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_encoding_has_valid_header_and_length() {
        let samples = vec![0.0, 0.5, -0.5, 1.0];
        let wav = encode_wav_16k_mono(&samples);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + samples.len() * 2);
    }

    #[test]
    fn collapses_japanese_spaces_while_keeping_english_spaces() {
        assert_eq!(
            collapse_japanese_spaces("聞こえ ま すか 送信 して。"),
            "聞こえますか送信して。"
        );
        assert_eq!(
            collapse_japanese_spaces("今 ログイン しま した 送 って。"),
            "今ログインしました送って。"
        );
        assert_eq!(
            collapse_japanese_spaces("hello world 送信 して"),
            "hello world送信して"
        );
    }

    #[test]
    fn parses_sherpa_json_and_strips_sensevoice_tags() {
        let sample_out = r#"
----
C:\Temp\voxdrop_utt.wav
{"text": "<|ja|><|NEUTRAL|><|Speech|><|withitn|>こんにちは送信して", "timestamps": []}
----
Elapsed seconds: 0.12
"#;
        assert_eq!(parse_sherpa_output(sample_out), "こんにちは送信して");
    }

    #[test]
    fn parses_whisper_output_ignoring_blank_audio() {
        let sample = "  [BLANK_AUDIO]\n  お疲れ様です \n";
        assert_eq!(parse_whisper_output(sample), "お疲れ様です");
    }

    #[test]
    fn mock_engine_reports_ready_when_explicitly_selected() {
        let mut cfg = SttConfig::default();
        cfg.engine = "mock".into();
        let eng = create_local_engine(&cfg, None);
        assert!(eng.is_ready());
        let text = eng.transcribe(&vec![0.0; 16_000]).unwrap();
        assert!(text.contains("1.0秒"));
    }

    #[test]
    fn discovers_sherpa_qwen3_model_directory() {
        let dir = std::env::temp_dir().join(format!(
            "voxdrop_qwen3_disc_{}",
            std::process::id()
        ));
        let model_sub = dir.join("sherpa-onnx-qwen3-asr-0.6B-int8");
        let tok_dir = model_sub.join("tokenizer");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&tok_dir).unwrap();
        fs::write(model_sub.join("conv_frontend.onnx"), b"stub").unwrap();
        fs::write(model_sub.join("encoder.int8.onnx"), b"stub").unwrap();
        fs::write(model_sub.join("decoder.int8.onnx"), b"stub").unwrap();

        let found = discover_sherpa_qwen3_model(&dir);
        match found {
            Some(SherpaModelKind::Qwen3Asr {
                conv_frontend,
                encoder,
                decoder,
                tokenizer,
            }) => {
                assert!(conv_frontend.ends_with("conv_frontend.onnx"));
                assert!(encoder.ends_with("encoder.int8.onnx"));
                assert!(decoder.ends_with("decoder.int8.onnx"));
                assert_eq!(tokenizer, tok_dir);
            }
            other => panic!("expected Qwen3Asr variant, got {:?}", other),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_local_http_engine_from_config() {
        let mut cfg = SttConfig::default();
        cfg.engine = "local-http".into();
        let eng = create_local_engine(&cfg, None);
        assert_eq!(eng.id(), "local-http");
        assert!(eng.device_name().contains("HTTP"));
    }

    #[test]
    fn resolve_model_search_dirs_includes_runtime_when_configured_empty_dir_exists() {
        let base = std::env::temp_dir().join(format!("voxdrop_multidir_{}", std::process::id()));
        let empty_cfg_dir = base.join("empty_models");
        let rt_dir = base.join("runtime");
        let rt_models = rt_dir.join("models");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&empty_cfg_dir).unwrap();
        fs::create_dir_all(&rt_models).unwrap();
        fs::write(rt_models.join("ggml-large-v3-turbo-q8_0.bin"), b"stub").unwrap();

        let mut cfg = SttConfig::default();
        cfg.model_dir = empty_cfg_dir.to_string_lossy().into_owned();
        let dirs = resolve_model_search_dirs(&cfg.model_dir, Some(&rt_dir));
        assert!(dirs.iter().any(|d| d == &empty_cfg_dir));
        assert!(dirs.iter().any(|d| d == &rt_models));
        let listed = list_available_ggml_models(&cfg, Some(&rt_dir));
        assert!(listed.contains(&"ggml-large-v3-turbo-q8_0.bin".to_string()));

        let dl = resolve_download_model_dir("models", Some(&rt_dir));
        assert_eq!(dl, user_models_dir());

        let _ = fs::remove_dir_all(&base);
    }
}
