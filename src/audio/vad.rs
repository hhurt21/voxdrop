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
/// Supports dual-path audio (`vad_samples` for Silero/SNR detection, `stt_samples` for clean
/// Whisper input) and fuses Silero neural VAD + RNNoise neural prob + adaptive SNR.
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
    silero_seen: bool,
    last_preview_samples: usize,
    // Optional explicit 2-second room calibration state
    calibrating: bool,
    calib_samples_left: usize,
    calib_rms_values: Vec<f32>,
}

impl VadSegmenter {
    pub fn new(config: VadConfig) -> Self {
        let pre_roll_cap = ms_to_samples(config.pre_roll_ms.max(480));
        Self {
            config,
            in_speech: false,
            pre_roll: VecDeque::with_capacity(pre_roll_cap + 512),
            utterance: Vec::new(),
            speech_samples: 0,
            silence_samples: 0,
            noise_floor: 0.004,
            noise_initialized: false,
            speech_peak_rms: 0.0,
            max_neural_prob: 0.0,
            silero_seen: false,
            last_preview_samples: 0,
            calibrating: false,
            calib_samples_left: 0,
            calib_rms_values: Vec::new(),
        }
    }

    pub fn update_config(&mut self, config: VadConfig) {
        self.config = config;
        let max_pre = ms_to_samples(self.config.pre_roll_ms.max(480));
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
        self.silero_seen = false;
        self.last_preview_samples = 0;
    }

    /// Returns a non-destructive 16kHz snapshot of the ongoing utterance every `interval_ms` (#7)
    /// once the utterance exceeds 2.0 seconds, so the mini HUD can show live interim text!
    pub fn take_streaming_preview_if_ready(&mut self, interval_ms: u64) -> Option<Vec<f32>> {
        if !self.in_speech || self.utterance.len() < ms_to_samples(2000) {
            return None;
        }
        let step = ms_to_samples(interval_ms.clamp(900, 5000));
        if self.utterance.len().saturating_sub(self.last_preview_samples) >= step {
            self.last_preview_samples = self.utterance.len();
            Some(self.utterance.clone())
        } else {
            None
        }
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
            (self.noise_floor * 2.0 + 0.0045).clamp(0.005, 0.12)
        } else {
            self.config.threshold.max(0.001)
        }
    }

    fn effective_keep_threshold(&self, start_th: f32) -> f32 {
        if self.config.auto_adjust {
            (self.noise_floor * 1.35 + 0.0025).clamp(0.003, start_th * 0.72)
        } else {
            start_th * 0.55
        }
    }

    /// Dual-path frame processing:
    /// - `vad_samples`: denoised 16 kHz audio used for energy/Silero VAD evaluation
    /// - `stt_samples`: clean un-gated 16 kHz audio buffered for STT inference
    pub fn push_frame_dual(
        &mut self,
        vad_samples: &[f32],
        stt_samples: &[f32],
        neural_vad_prob: f32,
        rnnoise_active: bool,
        silero_speech: Option<bool>,
    ) -> (f32, bool, Vec<VadEvent>) {
        let rms = compute_rms(vad_samples);
        let mut events = Vec::new();
        if vad_samples.is_empty() {
            return (rms, self.in_speech, events);
        }

        let stt_chunk = if stt_samples.len() == vad_samples.len() {
            stt_samples
        } else {
            vad_samples
        };

        // Explicit 2-second noise calibration mode
        if self.calibrating {
            self.calib_rms_values.push(rms);
            self.calib_samples_left = self.calib_samples_left.saturating_sub(vad_samples.len());
            if self.calib_samples_left == 0 {
                self.calibrating = false;
                if !self.calib_rms_values.is_empty() {
                    self.calib_rms_values
                        .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let idx = (self.calib_rms_values.len() * 85) / 100;
                    let measured = self.calib_rms_values[idx.min(self.calib_rms_values.len() - 1)];
                    self.noise_floor = measured.clamp(0.001, 0.08);
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

        // Continuous adaptive noise floor learning when not speaking.
        // Only adapt upward on clear silence (neither Silero nor RNNoise sees speech, and below start_th)
        // so slow/quiet speech onset never gets absorbed into the noise floor!
        if !self.in_speech {
            if !self.noise_initialized {
                self.noise_floor = rms.clamp(0.001, 0.06);
                self.noise_initialized = true;
            } else if rms < self.noise_floor {
                self.noise_floor = self.noise_floor * 0.88 + rms * 0.12;
            } else {
                let start_th_curr = self.effective_start_threshold();
                let clearly_not_speech = silero_speech != Some(true)
                    && (!rnnoise_active || neural_vad_prob < 0.22)
                    && rms < start_th_curr;
                if clearly_not_speech {
                    let capped = rms.min(self.noise_floor * 1.8 + 0.010);
                    self.noise_floor = (self.noise_floor * 0.985 + capped * 0.015).clamp(0.001, 0.08);
                }
            }
        }

        let start_th = self.effective_start_threshold();
        let keep_th = self.effective_keep_threshold(start_th);
        let silence_limit = ms_to_samples(self.config.silence_ms.clamp(200, 3000));
        let min_speech = ms_to_samples(self.config.min_speech_ms.max(40));
        let max_utterance = ms_to_samples(self.config.max_utterance_ms.max(1500));
        let max_pre_roll = ms_to_samples(self.config.pre_roll_ms.max(480));

        // Voice start decision:
        // Silero neural VAD OR strong SNR energy OR RNNoise neural voice confirm
        let silero_start =
            silero_speech == Some(true) && rms >= (self.noise_floor * 1.15).max(0.0020);
        let strong_energy = rms >= start_th
            && (!rnnoise_active || neural_vad_prob >= 0.10 || rms >= start_th * 1.6);
        let neural_assist = rnnoise_active
            && neural_vad_prob >= 0.45
            && rms >= (self.noise_floor * 1.35 + 0.0025).max(start_th * 0.55);

        if !self.in_speech {
            if silero_start || strong_energy || neural_assist {
                self.in_speech = true;
                self.utterance.clear();
                self.utterance.extend(self.pre_roll.iter().copied());
                self.pre_roll.clear();
                self.utterance.extend_from_slice(stt_chunk);
                self.speech_samples = vad_samples.len();
                self.silence_samples = 0;
                self.speech_peak_rms = rms;
                self.max_neural_prob = neural_vad_prob;
                self.silero_seen = silero_speech == Some(true);
                events.push(VadEvent::SpeechStart);
            } else {
                for &s in stt_chunk {
                    self.pre_roll.push_back(s);
                }
                while self.pre_roll.len() > max_pre_roll {
                    self.pre_roll.pop_front();
                }
            }
        } else {
            self.utterance.extend_from_slice(stt_chunk);
            if silero_speech == Some(true) {
                self.silero_seen = true;
            }
            if rms > self.speech_peak_rms {
                self.speech_peak_rms = self.speech_peak_rms * 0.6 + rms * 0.4;
            } else {
                self.speech_peak_rms = (self.speech_peak_rms * 0.992).max(keep_th);
            }
            if neural_vad_prob > self.max_neural_prob {
                self.max_neural_prob = neural_vad_prob;
            }

            // Gentle peak-drop detector (0.05x peak) + Silero neural keep so conversational
            // loudness variation and quiet trailing morae stay intact.
            let dynamic_drop_th = keep_th.max(self.speech_peak_rms * 0.05);
            let energy_active = rms >= dynamic_drop_th;
            let neural_still_talking =
                rnnoise_active && neural_vad_prob >= 0.28 && rms >= keep_th * 0.70;
            let silero_keep = silero_speech == Some(true);

            if silero_keep || energy_active || neural_still_talking {
                self.speech_samples += vad_samples.len();
                self.silence_samples = 0;
            } else {
                self.silence_samples += vad_samples.len();
            }

            let reached_silence = self.silence_samples >= silence_limit;
            let reached_max = self.utterance.len() >= max_utterance;

            if reached_silence || reached_max {
                let enough_speech = self.speech_samples >= min_speech;
                let looks_like_voice = self.silero_seen
                    || !rnnoise_active
                    || self.max_neural_prob >= 0.12
                    || self.speech_samples >= ms_to_samples(240)
                    || self.speech_peak_rms >= start_th * 1.6;

                let mut buf = std::mem::take(&mut self.utterance);
                let trailing_sil = self.silence_samples;
                self.in_speech = false;
                self.speech_samples = 0;
                self.silence_samples = 0;
                self.speech_peak_rms = 0.0;
                self.max_neural_prob = 0.0;
                self.silero_seen = false;
                self.last_preview_samples = 0;

                if reached_max && !buf.is_empty() {
                    // Carry 320ms overlap into pre_roll so continuous speech split at max_utterance never drops words
                    let overlap = ms_to_samples(320).min(buf.len());
                    let start = buf.len() - overlap;
                    self.pre_roll.clear();
                    self.pre_roll.extend(buf[start..].iter().copied());
                }

                if enough_speech && looks_like_voice && !buf.is_empty() {
                    // Keep 320ms tail margin so trailing unvoiced Japanese morae never clip
                    let keep_tail = ms_to_samples(320);
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

    /// Single-path compatibility wrapper.
    pub fn push_frame_with_prob(
        &mut self,
        samples: &[f32],
        neural_vad_prob: f32,
        rnnoise_active: bool,
    ) -> (f32, bool, Vec<VadEvent>) {
        self.push_frame_dual(samples, samples, neural_vad_prob, rnnoise_active, None)
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
        self.silero_seen = false;
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
            silence_ms: 400,
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

        let (_, speaking, evs) = vad.push_frame(&frame_of(0.0, 400));
        assert!(!speaking);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            VadEvent::SpeechEnd(samples) => {
                // 100ms pre-roll + 250ms speech + 320ms trimmed tail = 670ms
                assert_eq!(samples.len(), ms_to_samples(670));
            }
            _ => panic!("expected SpeechEnd"),
        }
    }

    #[test]
    fn adaptive_vad_learns_room_noise_and_closes_promptly_without_hanging() {
        let cfg = VadConfig {
            threshold: 0.012,
            silence_ms: 320,
            min_speech_ms: 100,
            pre_roll_ms: 500,
            max_utterance_ms: 10000,
            silero_model_path: String::new(),
            auto_adjust: true,
        };
        let mut vad = VadSegmenter::new(cfg);

        // 1. Feed 1 second of steady room/game noise at RMS = 0.022
        for _ in 0..20 {
            let (_, speaking, evs) =
                vad.push_frame_dual(&frame_of(0.022, 50), &frame_of(0.022, 50), 0.10, true, Some(false));
            assert!(!speaking, "adaptive VAD must not false-trigger on room noise");
            assert!(evs.is_empty());
        }
        assert!(
            vad.effective_start_threshold() > 0.03,
            "auto threshold should rise cleanly above 0.022 noise floor, got {}",
            vad.effective_start_threshold()
        );

        // 2. User speaks clearly at RMS = 0.12 for 500ms
        let mut started = false;
        for _ in 0..10 {
            let (_, speaking, evs) = vad.push_frame_dual(
                &frame_of(0.12, 50),
                &frame_of(0.12, 50),
                0.92,
                true,
                Some(true),
            );
            if evs.contains(&VadEvent::SpeechStart) {
                started = true;
            }
            assert!(speaking);
        }
        assert!(started);

        // 3. User stops speaking -> signal returns to 0.022 room noise
        let mut ended_ms = None;
        for step in 1..=12 {
            let (_, _, evs) = vad.push_frame_dual(
                &frame_of(0.022, 50),
                &frame_of(0.022, 50),
                0.20,
                true,
                Some(false),
            );
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
    fn slow_quiet_speech_triggers_via_silero_without_raising_noise_floor() {
        let cfg = VadConfig::default();
        let mut vad = VadSegmenter::new(cfg);

        // Quiet room noise at 0.003
        for _ in 0..10 {
            let (_, speaking, _) = vad.push_frame_dual(
                &frame_of(0.003, 50),
                &frame_of(0.003, 50),
                0.05,
                true,
                Some(false),
            );
            assert!(!speaking);
        }
        let floor_before = vad.noise_floor();

        // User starts speaking quietly and slowly (0.007 RMS) with Silero active
        let (_, speaking, evs) = vad.push_frame_dual(
            &frame_of(0.007, 50),
            &frame_of(0.007, 50),
            0.35,
            true,
            Some(true),
        );
        assert!(speaking, "Silero should trigger speech start even on quiet 0.007 voice");
        assert_eq!(evs, vec![VadEvent::SpeechStart]);

        // Even if Silero wasn't triggered yet, soft onset with neural prob >= 0.22 must NOT drag noise floor up
        vad.reset();
        for _ in 0..10 {
            vad.push_frame_dual(
                &frame_of(0.008, 50),
                &frame_of(0.008, 50),
                0.30,
                true,
                Some(false),
            );
        }
        assert!(
            (vad.noise_floor() - floor_before).abs() < 0.002,
            "voice onset must not contaminate noise_floor (before={:.4}, after={:.4})",
            floor_before,
            vad.noise_floor()
        );
    }

    #[test]
    fn conversational_breath_pause_and_soft_tail_stay_connected_with_clean_stt_audio() {
        let cfg = VadConfig::default(); // silence_ms = 620
        let mut vad = VadSegmenter::new(cfg);

        // Loud start on VAD path vs clean un-gated STT path
        for _ in 0..6 {
            let (_, speaking, _) = vad.push_frame_dual(
                &frame_of(0.25, 50),
                &frame_of(0.30, 50),
                0.90,
                true,
                Some(true),
            );
            assert!(speaking);
        }

        // 450ms conversational breath pause (silence_ms = 620 must hold!)
        for _ in 0..9 {
            let (_, speaking, evs) = vad.push_frame_dual(
                &frame_of(0.002, 50),
                &frame_of(0.002, 50),
                0.05,
                true,
                Some(false),
            );
            assert!(speaking, "450ms breath pause must not split utterance");
            assert!(evs.is_empty());
        }

        // Soft trailing syllable (0.015 RMS after 0.25 peak -> 0.06x peak, saved by 0.05x threshold!)
        let (_, speaking, _) = vad.push_frame_dual(
            &frame_of(0.015, 50),
            &frame_of(0.020, 50),
            0.35,
            true,
            Some(false),
        );
        assert!(speaking);

        // End of utterance after 650ms silence
        let mut emitted = None;
        for _ in 0..15 {
            let (_, _, evs) = vad.push_frame_dual(
                &frame_of(0.001, 50),
                &frame_of(0.001, 50),
                0.02,
                true,
                Some(false),
            );
            if let Some(VadEvent::SpeechEnd(buf)) = evs.into_iter().next() {
                emitted = Some(buf);
                break;
            }
        }
        let buf = emitted.expect("utterance should end after 620ms silence");
        // Verify STT path samples (0.30) were buffered, not VAD path (0.25)
        assert!(buf.iter().any(|&s| (s - 0.30).abs() < 1e-5));
    }

    #[test]
    fn short_click_is_discarded() {
        let cfg = VadConfig {
            threshold: 0.05,
            silence_ms: 200,
            min_speech_ms: 200,
            pre_roll_ms: 50,
            max_utterance_ms: 5000,
            silero_model_path: String::new(),
            auto_adjust: false,
        };
        let mut vad = VadSegmenter::new(cfg);
        let (_, _, _) = vad.push_frame(&frame_of(0.3, 50));
        let (_, _, evs) = vad.push_frame(&frame_of(0.0, 250));
        assert_eq!(evs, vec![VadEvent::SpeechCancelled]);
    }
}
