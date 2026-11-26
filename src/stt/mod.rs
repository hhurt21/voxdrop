pub mod local;
pub mod sherpa_ffi;
pub mod whisper_ffi;

use crate::config::SttConfig;
use std::path::Path;
use std::sync::Arc;

/// Abstraction over local offline Speech-to-Text engines (resident sherpa-onnx C API, CLI, whisper).
pub trait SttEngine: Send + Sync {
    /// Human-readable engine label shown in the UI status pill.
    fn name(&self) -> String;

    /// Returns `true` when the engine and its local model are ready for offline inference.
    fn is_ready(&self) -> bool;

    /// Transcribes a 16 kHz mono `f32` PCM buffer into recognized text.
    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String>;

    /// Transcribes with optional radio/slurred-speech enhancement (GTCRN + presence EQ + AGC).
    fn transcribe_enhanced(
        &self,
        samples_16k: &[f32],
        _radio_clarity: bool,
    ) -> Result<String, String> {
        self.transcribe(samples_16k)
    }

    /// Updates dynamic prompt hints (voice commands, custom dictionary, friend/channel names).
    fn set_prompt_hints(&self, _hints: &str) {}
}

pub fn create_engine(cfg: &SttConfig, runtime_dir: Option<&Path>) -> Arc<dyn SttEngine> {
    local::create_local_engine(cfg, runtime_dir)
}
