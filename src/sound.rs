use crate::config::SoundConfig;
use rodio::{Decoder, OutputStream, Sink};
use std::io::Cursor;
use std::sync::mpsc;
use std::thread;

const SOUND_SEND: &[u8] = include_bytes!("../assets/sounds/send.wav");
const SOUND_CANCEL: &[u8] = include_bytes!("../assets/sounds/cancel.wav");
const SOUND_TOGGLE: &[u8] = include_bytes!("../assets/sounds/toggle.wav");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    Send,
    Cancel,
    ToggleOn,
    ToggleOff,
    Sleep,
}

impl SoundKind {
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "send" => Self::Send,
            "cancel" => Self::Cancel,
            "toggle_off" => Self::ToggleOff,
            "sleep" => Self::Sleep,
            _ => Self::ToggleOn,
        }
    }

    fn wav_bytes_and_speed(&self) -> (&'static [u8], f32) {
        match self {
            Self::Send => (SOUND_SEND, 1.0),
            Self::Cancel => (SOUND_CANCEL, 1.0),
            Self::ToggleOn => (SOUND_TOGGLE, 1.15),
            Self::ToggleOff => (SOUND_TOGGLE, 0.85),
            Self::Sleep => (SOUND_CANCEL, 0.9),
        }
    }
}

struct PlayRequest {
    kind: SoundKind,
    volume: f32,
}

/// Non-blocking audio feedback player backed by a dedicated `rodio` worker thread.
pub struct SoundPlayer {
    tx: mpsc::Sender<PlayRequest>,
}

impl SoundPlayer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<PlayRequest>();
        thread::spawn(move || {
            let mut output: Option<(OutputStream, rodio::OutputStreamHandle)> = None;
            while let Ok(req) = rx.recv() {
                if output.is_none() {
                    if let Ok(pair) = OutputStream::try_default() {
                        output = Some(pair);
                    }
                }
                if let Some((_, ref handle)) = output {
                    let (bytes, speed) = req.kind.wav_bytes_and_speed();
                    if let (Ok(sink), Ok(source)) =
                        (Sink::try_new(handle), Decoder::new(Cursor::new(bytes)))
                    {
                        sink.set_volume(req.volume.clamp(0.0, 1.0));
                        sink.set_speed(speed);
                        sink.append(source);
                        sink.detach();
                    }
                }
            }
        });
        Self { tx }
    }

    pub fn play(&self, kind: SoundKind, cfg: &SoundConfig) {
        if !cfg.enabled || cfg.volume <= 0.001 {
            return;
        }
        let _ = self.tx.send(PlayRequest {
            kind,
            volume: cfg.volume,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_wav_assets_decode_cleanly() {
        for kind in [
            SoundKind::Send,
            SoundKind::Cancel,
            SoundKind::ToggleOn,
            SoundKind::ToggleOff,
            SoundKind::Sleep,
        ] {
            let (bytes, _) = kind.wav_bytes_and_speed();
            assert!(bytes.len() > 44);
            let decoded = Decoder::new(Cursor::new(bytes));
            assert!(decoded.is_ok(), "failed to decode {:?}", kind);
        }
    }
}
