pub mod catalog;
pub mod downloader;
pub mod ensemble;
pub mod http_asr;
pub mod llm_corrector;
pub mod local;
pub mod sherpa_ffi;
pub mod whisper_ffi;

use crate::config::{ConversationPreset, SttConfig};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscribedToken {
    pub text: String,
    pub prob: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptWithConfidence {
    pub text: String,
    pub tokens: Vec<TranscribedToken>,
    pub avg_prob: f32,
}

impl TranscriptWithConfidence {
    pub fn from_text(text: String) -> Self {
        Self {
            text,
            tokens: Vec::new(),
            avg_prob: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscribeContext {
    pub language: String,
    pub hotwords_prompt: Option<String>,
    #[allow(dead_code)]
    pub preset: ConversationPreset,
    pub radio_clarity: bool,
    pub eq_strength: f32,
    pub room_debox_strength: f32,
    pub dereverb_enabled: bool,
    pub head_pad_ms: u64,
    pub tail_pad_ms: u64,
    pub beam_size: i32,
    pub temperature_fallback: bool,
}

impl Default for TranscribeContext {
    fn default() -> Self {
        Self {
            language: "ja".into(),
            hotwords_prompt: None,
            preset: ConversationPreset::Normal,
            radio_clarity: true,
            eq_strength: 0.55,
            room_debox_strength: 0.35,
            dereverb_enabled: false,
            head_pad_ms: 200,
            tail_pad_ms: 300,
            beam_size: 5,
            temperature_fallback: true,
        }
    }
}

/// Abstraction over local/external Speech-to-Text engines (Whisper.cpp Vulkan, sherpa-onnx Qwen3-ASR/SenseVoice, HTTP ASR, CLI).
pub trait SttEngine: Send + Sync {
    /// Stable engine identifier (`"whisper-cpp"`, `"sherpa-qwen3"`, `"sherpa-onnx"`, `"local-http"`, etc.).
    #[allow(dead_code)]
    fn id(&self) -> &'static str {
        "unknown"
    }

    /// Human-readable engine label shown in the UI status pill.
    fn name(&self) -> String;

    /// Active execution device label (`"Vulkan: AMD Radeon RX 6700 XT"`, `"DirectML (GPU)"`, `"CPU"`, etc.).
    fn device_name(&self) -> String {
        "CPU".into()
    }

    /// Returns `true` when the engine and its local model are ready for offline inference.
    fn is_ready(&self) -> bool;

    /// Transcribes a 16 kHz mono `f32` PCM buffer into recognized text.
    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String>;

    /// Transcribes with optional radio/slurred-speech enhancement.
    fn transcribe_enhanced(
        &self,
        samples_16k: &[f32],
        _radio_clarity: bool,
    ) -> Result<String, String> {
        self.transcribe(samples_16k)
    }

    /// Context-aware transcription entrypoint used by VoxDrop v1.5.0+.
    fn transcribe_with_context(
        &self,
        samples_16k: &[f32],
        ctx: &TranscribeContext,
    ) -> Result<String, String> {
        self.transcribe_enhanced(samples_16k, ctx.radio_clarity)
    }

    /// Context-aware transcription returning per-token probabilities when supported (#2).
    fn transcribe_with_confidence(
        &self,
        samples_16k: &[f32],
        ctx: &TranscribeContext,
    ) -> Result<TranscriptWithConfidence, String> {
        let text = self.transcribe_with_context(samples_16k, ctx)?;
        Ok(TranscriptWithConfidence::from_text(text))
    }

    /// Updates dynamic prompt hints (voice commands, custom dictionary, friend/channel names).
    fn set_prompt_hints(&self, _hints: &str) {}
}

pub fn create_engine(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Arc<dyn SttEngine> {
    local::create_local_engine(cfg, runtime_dir)
}
