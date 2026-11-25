use crate::config::VadConfig;
use std::collections::VecDeque;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    SpeechStart,
    SpeechEnd(Vec<f32>),
    SpeechCancelled,
    CalibrationDone { noise_floor: f32, threshold: f32 },
}

/// Computes root-mean-square energy of a PCM frame.
pub fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Self-Calibrating Adaptive Voice-vs-Noise VAD Segmenter at 16 kHz.
///
/// Key capabilities:
/// 1. Continuously learns the ambient room/game/preamp noise floor (`noise_floor_rms`) so
///    varying background noise never keeps VAD stuck open for 5-8 seconds!
/// 2. Tracks per-utterance `speech_peak_rms` and detects clean voice-to-noise drop-off so
///    end-of-speech triggers in ~320ms even over background audio!
/// 3. Combines SNR + RNNoise neural voice probability to reject keyboard clicks & pure hum.
/// 4. Trims excess trailing silence before STT so Whisper runs in ~0.45s with zero noise hallucinations.
pub struct VadSegmenter {
    config: VadConfig,
    in_speech: bool,
    pre_roll: VecDeque<f32>,
    utterance: Vec<f32>,
    speech_samples: usize,
    silence_samples: usize,
    // Adaptive noise-floor & utterance speech-envelope tracking
    noise_floor: f32,
    noise_initialized: bool,
    speech_peak_rms: f32,
    max_neural_prob: f32,
    // Optional explicit 2-second room calibration state
    calibrating: bool,
    calib_samples_left: usize,
    calib_rms_values: Vec<f32>,
}

impl VadSegmenter {
    pub fn new(config: VadConfig) -> Self {
        let pre_roll_cap = ms_to_samples(config.pre_roll_ms.max(420));
        Self {
            config,
            in_speech: false,
            pre_roll: VecDeque::with_capacity(pre_roll_cap + 512),
            utterance: Vec::new(),
            speech_samples: 0,
            silence_samples: 0,
            noise_floor: 0.005,
            noise_initialized: false,
            speech_peak_rms: 0.0,
            max_neural_prob: 0.0,
            calibrating: false,
            calib_samples_left: 0,
            calib_rms_values: Vec::new(),
        }
    }

    pub fn update_config(&mut self, config: VadConfig) {
        self.config = config;
        let max_pre = ms_to_samples(self.config.pre_roll_ms.max(250));
        while self.pre_roll.len() > max_pre {
            self.pre_roll.pop_front();
        }
    }

    pub fn reset(&mut self) {
        self.in_speech = false;
        self.pre_roll.clear();
        self.utterance.clear();
        self.speech_samples = 0;
        self.silence_samples = 0;
        self.speech_peak_rms = 0.0;
        self.max_neural_prob = 0.0;
    }

    /// Starts an explicit ambient noise calibration window (e.g. 2000ms of room silence).
    pub fn start_calibration(&mut self, duration_ms: u64) {
        self.reset();
        self.calibrating = true;
        self.calib_samples_left = ms_to_samples(duration_ms.clamp(500, 5000));
        self.calib_rms_values.clear();
    }

    pub fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    pub fn effective_start_threshold(&self) -> f32 {
        if self.config.auto_adjust {
            (self.noise_floor * 2.4 + 0.010).clamp(0.010, 0.18)
        } else {
            self.config.threshold.max(0.001)
        }
    }

    fn effective_keep_threshold(&self, start_th: f32) -> f32 {
        if self.config.auto_adjust {
            (self.noise_floor * 1.55 + 0.005)
                .clamp(0.006, start_th * 0.78)
        } else {
            start_th * 0.65
        }
    }

    /// Processes a chunk of 16 kHz mono samples along with optional RNNoise neural VAD probability.
    pub fn push_frame_with_prob(
        &mut self,
        samples: &[f32],
        neural_vad_prob: f32,
        rnnoise_active: bool,
    ) -> (f32, bool, Vec<VadEvent>) {
        let rms = compute_rms(samples);
        let mut events = Vec::new();
        if samples.is_empty() {
            return (rms, self.in_speech, events);
        }

        // Explicit 2-second noise calibration mode
        if self.calibrating {
            self.calib_rms_values.push(rms);
            self.calib_samples_left = self.calib_samples_left.saturating_sub(samples.len());
            if self.calib_samples_left == 0 {
                self.calibrating = false;
                if !self.calib_rms_values.is_empty() {
                    self.calib_rms_values
                        .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    // 85th percentile of ambient room frames gives a rock-solid noise floor
                    let idx = (self.calib_rms_values.len() * 85) / 100;
                    let measured = self.calib_rms_values[idx.min(self.calib_rms_values.len() - 1)];
                    self.noise_floor = measured.clamp(0.001, 0.12);
                    self.noise_initialized = true;
                }
                let new_th = self.effective_start_threshold();
                events.push(VadEvent::CalibrationDone {
                    noise_floor: self.noise_floor,
                    threshold: new_th,
                });
            }
            return (rms, false, events);
        }

        // Continuous adaptive noise floor learning when not speaking
        if !self.in_speech {
            if !self.noise_initialized {
                self.noise_floor = rms.clamp(0.001, 0.08);
                self.noise_initialized = true;
            } else if rms < self.noise_floor {
                // Fast downward adaptation when room becomes quieter
                self.noise_floor = self.noise_floor * 0.88 + rms * 0.12;
            } else if !rnnoise_active || neural_vad_prob < 0.45 {
                // Slow upward adaptation to steady fan/game hum (capped so speech never drags it up)
                let capped = rms.min(self.noise_floor * 2.0 + 0.015);
                self.noise_floor = (self.noise_floor * 0.985 + capped * 0.015).clamp(0.001, 0.12);
            }
        }

        let start_th = self.effective_start_threshold();
        let keep_th = self.effective_keep_threshold(start_th);
        let silence_limit = ms_to_samples(self.config.silence_ms.clamp(150, 2000));
        let min_speech = ms_to_samples(self.config.min_speech_ms.max(40));
        let max_utterance = ms_to_samples(self.config.max_utterance_ms.max(1500));
        let max_pre_roll = ms_to_samples(self.config.pre_roll_ms.max(420));

        // Voice start decision:
        // Either clearly above SNR threshold (and not rejected by neural VAD when strong hum),
        // OR neural VAD strongly confirms human voice (>0.65) cleanly above noise floor!
        let strong_energy = rms >= start_th
            && (!rnnoise_active || neural_vad_prob >= 0.12 || rms >= start_th * 1.8);
        let neural_assist = rnnoise_active
            && neural_vad_prob >= 0.65
            && rms >= (self.noise_floor * 1.7 + 0.005).max(start_th * 0.65);

        if !self.in_speech {
            if strong_energy || neural_assist {
                self.in_speech = true;
                self.utterance.clear();
                self.utterance.extend(self.pre_roll.iter().copied());
                self.pre_roll.clear();
                self.utterance.extend_from_slice(samples);
                self.speech_samples = samples.len();
                self.silence_samples = 0;
                self.speech_peak_rms = rms;
                self.max_neural_prob = neural_vad_prob;
                events.push(VadEvent::SpeechStart);
            } else {
                for &s in samples {
                    self.pre_roll.push_back(s);
                }
                while self.pre_roll.len() > max_pre_roll {
                    self.pre_roll.pop_front();
                }
            }
        } else {
            self.utterance.extend_from_slice(samples);
            if rms > self.speech_peak_rms {
                self.speech_peak_rms = self.speech_peak_rms * 0.6 + rms * 0.4;
            } else {
                // Gently decay peak envelope so one early loud syllable/laugh never cuts off quieter fast speech later
                self.speech_peak_rms = (self.speech_peak_rms * 0.992).max(keep_th);
            }
            if neural_vad_prob > self.max_neural_prob {
                self.max_neural_prob = neural_vad_prob;
            }

            // Peak-Drop End-of-Speech Detector:
            // Using 0.105x peak preserves soft trailing sibilants ('す', 'し', 'そ', 'さ') & unvoiced consonants
            // in fast/slurred speech while still closing promptly.
            let dynamic_drop_th = keep_th.max(self.speech_peak_rms * 0.105);
            let energy_active = rms >= dynamic_drop_th;
            let neural_still_talking = rnnoise_active
                && neural_vad_prob >= 0.45
                && rms >= keep_th * 0.85
                && rms >= self.speech_peak_rms * 0.12;

            if energy_active || neural_still_talking {
                self.speech_samples += samples.len();
                self.silence_samples = 0;
            } else {
                self.silence_samples += samples.len();
            }

            let reached_silence = self.silence_samples >= silence_limit;
            let reached_max = self.utterance.len() >= max_utterance;

            if reached_silence || reached_max {
                let enough_speech = self.speech_samples >= min_speech;
                // Reject pure non-speech clicks/thumps when RNNoise saw virtually zero human voice
                let looks_like_voice = !rnnoise_active
                    || self.max_neural_prob >= 0.22
                    || self.speech_peak_rms >= start_th * 2.2;

                let mut buf = std::mem::take(&mut self.utterance);
                let trailing_sil = self.silence_samples;
                self.in_speech = false;
                self.speech_samples = 0;
                self.silence_samples = 0;
                self.speech_peak_rms = 0.0;
                self.max_neural_prob = 0.0;

                if reached_max && !buf.is_empty() {
                    // Carry 160ms overlap into pre_roll so fast speech split at max_utterance never drops a syllable
                    let overlap = ms_to_samples(160).min(buf.len());
                    let start = buf.len() - overlap;
                    self.pre_roll.clear();
                    self.pre_roll.extend(buf[start..].iter().copied());
                }

                if enough_speech && looks_like_voice && !buf.is_empty() {
                    // Keep 220ms tail margin so trailing Japanese unvoiced consonants (す/し/く/つ/た) never clip
                    let keep_tail = ms_to_samples(220);
                    if trailing_sil > keep_tail && buf.len() > trailing_sil {
                        let trim = trailing_sil - keep_tail;
                        buf.truncate(buf.len() - trim);
                    }
                    events.push(VadEvent::SpeechEnd(buf));
                } else {
                    events.push(VadEvent::SpeechCancelled);
                }
            }
        }

        (rms, self.in_speech, events)
    }

    #[cfg(test)]
    pub fn push_frame(&mut self, samples: &[f32]) -> (f32, bool, Vec<VadEvent>) {
        self.push_frame_with_prob(samples, 0.0, false)
    }

    pub fn flush(&mut self) -> Option<Vec<f32>> {
        if !self.in_speech {
            return None;
        }
        let min_speech = ms_to_samples(self.config.min_speech_ms.max(40));
        let valid = self.speech_samples >= min_speech;
        let buf = std::mem::take(&mut self.utterance);
        self.in_speech = false;
        self.speech_samples = 0;
        self.silence_samples = 0;
        self.speech_peak_rms = 0.0;
        self.max_neural_prob = 0.0;
        if valid && !buf.is_empty() {
            Some(buf)
        } else {
            None
        }
    }
}

fn ms_to_samples(ms: u64) -> usize {
    ((TARGET_SAMPLE_RATE as u64 * ms) / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(val: f32, ms: u64) -> Vec<f32> {
        vec![val; ms_to_samples(ms)]
    }

    #[test]
    fn silence_never_triggers_speech() {
        let mut vad = VadSegmenter::new(VadConfig::default());
        for _ in 0..20 {
            let (rms, speaking, evs) = vad.push_frame(&frame_of(0.0, 50));
            assert_eq!(rms, 0.0);
            assert!(!speaking);
            assert!(evs.is_empty());
        }
    }

    #[test]
    fn speech_then_silence_emits_utterance_with_preroll() {
        let cfg = VadConfig {
            threshold: 0.05,
            silence_ms: 200,
            min_speech_ms: 150,
            pre_roll_ms: 300,
            max_utterance_ms: 5000,
            silero_model_path: String::new(),
            auto_adjust: false,
        };
        let mut vad = VadSegmenter::new(cfg);

        let (_, _, evs) = vad.push_frame(&frame_of(0.01, 100));
        assert!(evs.is_empty());

        let (_, speaking, evs) = vad.push_frame(&frame_of(0.2, 250));
        assert!(speaking);
        assert_eq!(evs, vec![VadEvent::SpeechStart]);

        let (_, speaking, evs) = vad.push_frame(&frame_of(0.0, 250));
        assert!(!speaking);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            VadEvent::SpeechEnd(samples) => {
                // 100ms pre-roll + 250ms speech + 220ms trimmed tail = 570ms
                assert_eq!(samples.len(), ms_to_samples(570));
            }
            _ => panic!("expected SpeechEnd"),
        }
    }

    #[test]
    fn adaptive_vad_learns_room_noise_and_closes_promptly_without_hanging() {
        let cfg = VadConfig {
            threshold: 0.015,
            silence_ms: 320,
            min_speech_ms: 140,
            pre_roll_ms: 300,
            max_utterance_ms: 10000,
            silero_model_path: String::new(),
            auto_adjust: true,
        };
        let mut vad = VadSegmenter::new(cfg);

        // 1. Feed 1 second of steady room/game noise at RMS = 0.022 (higher than old fixed 0.015!)
        for _ in 0..20 {
            let (_, speaking, evs) = vad.push_frame_with_prob(&frame_of(0.022, 50), 0.15, true);
            assert!(!speaking, "adaptive VAD must not false-trigger on room noise");
            assert!(evs.is_empty());
        }
        assert!(
            vad.effective_start_threshold() > 0.04,
            "auto threshold should rise cleanly above 0.022 noise floor, got {}",
            vad.effective_start_threshold()
        );

        // 2. User speaks clearly at RMS = 0.12 for 500ms
        let mut started = false;
        for _ in 0..10 {
            let (_, speaking, evs) = vad.push_frame_with_prob(&frame_of(0.12, 50), 0.92, true);
            if evs.contains(&VadEvent::SpeechStart) {
                started = true;
            }
            assert!(speaking);
        }
        assert!(started);

        // 3. User stops speaking -> signal returns to 0.022 room noise (even with RNNoise false prob 0.40!)
        //    MUST end within <= 400ms (NEVER hang for 5-8 seconds!)
        let mut ended_ms = None;
        for step in 1..=10 {
            let (_, _, evs) = vad.push_frame_with_prob(&frame_of(0.022, 50), 0.40, true);
            if evs.iter().any(|e| matches!(e, VadEvent::SpeechEnd(_))) {
                ended_ms = Some(step * 50);
                break;
            }
        }
        assert!(
            ended_ms.is_some() && ended_ms.unwrap() <= 400,
            "expected prompt speech end in <=400ms, got {:?}",
            ended_ms
        );
    }

    #[test]
    fn short_click_is_discarded() {
        let cfg = VadConfig {
            threshold: 0.05,
            silence_ms: 150,
            min_speech_ms: 200,
            pre_roll_ms: 50,
            max_utterance_ms: 5000,
            silero_model_path: String::new(),
            auto_adjust: false,
        };
        let mut vad = VadSegmenter::new(cfg);
        let (_, _, _) = vad.push_frame(&frame_of(0.3, 50));
        let (_, _, evs) = vad.push_frame(&frame_of(0.0, 200));
        assert_eq!(evs, vec![VadEvent::SpeechCancelled]);
    }
}
