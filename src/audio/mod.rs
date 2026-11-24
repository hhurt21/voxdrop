pub mod capture;
pub mod denoise;
pub mod enhance;
pub mod vad;

#[derive(Debug)]
pub enum AudioEvent {
    Level {
        rms: f32,
        is_speaking: bool,
        vad_prob: f32,
        rnnoise_active: bool,
        noise_floor: f32,
        effective_threshold: f32,
    },
    CalibrationDone {
        noise_floor: f32,
        threshold: f32,
    },
    SpeechStart,
    SpeechCancelled,
    Utterance(Vec<f32>),
    Error(String),
}
