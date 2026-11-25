pub mod launcher;
pub mod queue;
pub mod voicevox;

use crate::config::VoicevoxConfig;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global echo-prevention guard flag: `true` while TTS audio is playing (plus tail grace window).
/// When `DiscordTtsConfig.pause_stt_while_speaking` is enabled, microphone VAD/STT ignores input
/// while this flag is set so speaker output is never picked up and sent back to Discord.
static IS_TTS_PLAYING: AtomicBool = AtomicBool::new(false);

pub fn is_tts_playing() -> bool {
    IS_TTS_PLAYING.load(Ordering::SeqCst)
}

pub fn set_tts_playing(active: bool) {
    IS_TTS_PLAYING.store(active, Ordering::SeqCst);
}

/// Abstraction for TTS synthesis engines (VOICEVOX Engine now, extensible to other engines later).
pub trait TtsEngine: Send + Sync {
    fn synthesize(&self, text: &str, cfg: &VoicevoxConfig) -> Result<Vec<u8>, String>;
}
