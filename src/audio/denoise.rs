use crate::config::NoiseCancelConfig;
use nnnoiseless::DenoiseState;

pub const RNNOISE_SAMPLE_RATE: u32 = 48_000;
const FRAME_SIZE: usize = DenoiseState::FRAME_SIZE; // 480 samples (10ms at 48kHz)
const PCM16_SCALE: f32 = 32768.0;

/// Real-time 48 kHz voice noise canceller combining:
/// 1. 1st-order ~85 Hz High-Pass Filter (cuts PC fan hum, AC rumble, desk bumps)
/// 2. Pure-Rust RNNoise neural network (`nnnoiseless`) — tuned so Japanese unvoiced consonants are preserved!
/// 3. Optional gentle neural-VAD noise gate (never crushes consonant onsets to 0!)
pub struct AudioDenoiser {
    config: NoiseCancelConfig,
    rnnoise: Box<DenoiseState<'static>>,
    input_buf: Vec<f32>,
    hp_prev_in: f32,
    hp_prev_out: f32,
    gate_gain: f32,
    hangover_frames: usize,
    last_vad_prob: f32,
    hf_noise_floor: f32,
}

impl AudioDenoiser {
    pub fn new(config: NoiseCancelConfig) -> Self {
        Self {
            config,
            rnnoise: DenoiseState::new(),
            input_buf: Vec::with_capacity(FRAME_SIZE * 4),
            hp_prev_in: 0.0,
            hp_prev_out: 0.0,
            gate_gain: 1.0,
            hangover_frames: 0,
            last_vad_prob: 0.0,
            hf_noise_floor: 0.002,
        }
    }

    pub fn update_config(&mut self, config: NoiseCancelConfig) {
        self.config = config;
    }

    pub fn is_rnnoise_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn reset(&mut self) {
        self.input_buf.clear();
        self.hp_prev_in = 0.0;
        self.hp_prev_out = 0.0;
        self.gate_gain = 1.0;
        self.hangover_frames = 0;
        self.last_vad_prob = 0.0;
        self.hf_noise_floor = 0.002;
        self.rnnoise = DenoiseState::new();
    }

    /// Processes mono 48 kHz `f32` (-1.0..=1.0) samples.
    /// Returns `(processed_48k_samples, latest_neural_vad_prob, rnnoise_active)`.
    pub fn process(&mut self, samples_48k: &[f32]) -> (Vec<f32>, f32, bool) {
        if samples_48k.is_empty() {
            return (Vec::new(), self.last_vad_prob, self.config.enabled);
        }

        let mut stage1 = Vec::with_capacity(samples_48k.len());
        if self.config.highpass_enabled {
            const ALPHA: f32 = 0.9889;
            for &x in samples_48k {
                let y = ALPHA * (self.hp_prev_out + x - self.hp_prev_in);
                self.hp_prev_in = x;
                self.hp_prev_out = y;
                stage1.push(y.clamp(-1.0, 1.0));
            }
        } else {
            stage1.extend_from_slice(samples_48k);
        }

        if !self.config.enabled {
            self.last_vad_prob = 0.0;
            return (stage1, 0.0, false);
        }

        self.input_buf.extend_from_slice(&stage1);
        let mut output = Vec::with_capacity(self.input_buf.len());
        let strength = self.config.strength.clamp(0.0, 1.0);
        let gate_enabled = self.config.noise_gate_enabled;
        let vad_th = self.config.vad_prob_threshold.clamp(0.05, 0.95);

        let mut in_frame = [0.0f32; FRAME_SIZE];
        let mut out_frame = [0.0f32; FRAME_SIZE];

        while self.input_buf.len() >= FRAME_SIZE {
            let mut sum_sq = 0.0f32;
            let mut zero_crossings = 0usize;
            let mut prev_s = self.input_buf[0];
            for (idx, (dst, &src)) in in_frame
                .iter_mut()
                .zip(self.input_buf.iter().take(FRAME_SIZE))
                .enumerate()
            {
                *dst = (src * PCM16_SCALE).clamp(-PCM16_SCALE, PCM16_SCALE);
                sum_sq += src * src;
                if idx > 0 && ((prev_s >= 0.0) != (src >= 0.0)) {
                    zero_crossings += 1;
                }
                prev_s = src;
            }
            let frame_rms = (sum_sq / FRAME_SIZE as f32).sqrt();
            let zcr = zero_crossings as f32 / (FRAME_SIZE - 1) as f32;

            let vad_prob = self.rnnoise.process_frame(&mut out_frame, &in_frame);
            self.last_vad_prob = vad_prob;

            if vad_prob < 0.18 && self.hangover_frames == 0 {
                self.hf_noise_floor = (self.hf_noise_floor * 0.90 + frame_rms * 0.10).clamp(0.0005, 0.08);
            }

            // Protect Japanese unvoiced sibilants ('さ・し・す・せ・そ', /ts/, /k/):
            // Real sibilants have high 48kHz zero-crossing rate (ZCR >= 0.14) AND appear as a burst
            // above the room noise floor or right next to active voiced speech (hangover_frames > 0).
            let is_sibilant_consonant = zcr >= 0.14
                && frame_rms >= 0.0045
                && (frame_rms >= self.hf_noise_floor * 2.0 || self.hangover_frames > 0);

            // Gentle noise gate with 250ms (25 frames) hangover & safe floor (0.30)
            // so initial & trailing Japanese consonants ('き', 'さ', 'し', 'す') are NEVER chopped!
            if vad_prob >= vad_th || is_sibilant_consonant {
                self.hangover_frames = 25;
            } else if self.hangover_frames > 0 {
                self.hangover_frames -= 1;
            }

            let target_gain = if !gate_enabled || self.hangover_frames > 0 {
                1.0
            } else {
                0.30 + 0.70 * (vad_prob / vad_th).clamp(0.0, 1.0)
            };
            let coeff = if target_gain > self.gate_gain {
                0.85 // Ultra-fast attack (<10ms)
            } else {
                0.10 // Gentle release
            };
            self.gate_gain += (target_gain - self.gate_gain) * coeff;

            let eff_strength = if is_sibilant_consonant {
                strength * 0.30
            } else {
                strength
            };

            for i in 0..FRAME_SIZE {
                let dry = self.input_buf[i];
                let wet = (out_frame[i] / PCM16_SCALE).clamp(-1.0, 1.0);
                let mixed = dry * (1.0 - eff_strength) + wet * eff_strength;
                output.push((mixed * self.gate_gain).clamp(-1.0, 1.0));
            }

            self.input_buf.drain(0..FRAME_SIZE);
        }

        (output, self.last_vad_prob, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highpass_removes_dc_offset() {
        let cfg = NoiseCancelConfig {
            enabled: false,
            highpass_enabled: true,
            ..Default::default()
        };
        let mut denoiser = AudioDenoiser::new(cfg);
        let dc = vec![0.5f32; 4800];
        let (out, _, active) = denoiser.process(&dc);
        assert!(!active);
        assert_eq!(out.len(), 4800);
        assert!(out[4799].abs() < 0.01);
    }

    #[test]
    fn rnnoise_processes_48k_frames_and_suppresses_stationary_noise() {
        let cfg = NoiseCancelConfig {
            enabled: true,
            strength: 1.0,
            highpass_enabled: true,
            noise_gate_enabled: false,
            vad_prob_threshold: 0.30,
            ..Default::default()
        };
        let mut denoiser = AudioDenoiser::new(cfg);

        let mut seed: u32 = 123456789;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let u = ((seed >> 8) as f32 / 16777216.0) * 2.0 - 1.0;
                u * 0.025
            })
            .collect();

        let (out, vad_prob, active) = denoiser.process(&noise);
        assert!(active);
        assert_eq!(out.len(), 48_000);
        assert!((0.0..=1.0).contains(&vad_prob));

        let tail = 38_400;
        let in_rms = (noise[tail..].iter().map(|x| x * x).sum::<f32>() / 9600.0).sqrt();
        let out_rms = (out[tail..].iter().map(|x| x * x).sum::<f32>() / 9600.0).sqrt();
        assert!(
            out_rms < in_rms * 0.75,
            "expected RNNoise to suppress stationary noise (in_rms={:.4}, out_rms={:.4})",
            in_rms,
            out_rms
        );
    }

    #[test]
    fn preserves_high_frequency_sibilant_consonant_bursts() {
        let cfg = NoiseCancelConfig {
            enabled: true,
            strength: 0.85,
            highpass_enabled: true,
            noise_gate_enabled: true,
            vad_prob_threshold: 0.35,
            ..Default::default()
        };
        let mut denoiser = AudioDenoiser::new(cfg);

        // 200ms quiet room lead-in followed by 60ms 5.5kHz sibilant burst ('さ/し/す/せ/そ')
        let mut sig = vec![0.0005f32; 9600];
        let mut burst: Vec<f32> = (0..2880)
            .map(|i| (2.0 * std::f32::consts::PI * 5500.0 * (i as f32 / 48_000.0)).sin() * 0.08)
            .collect();
        sig.append(&mut burst);

        let (out, _, _) = denoiser.process(&sig);
        let in_burst_rms = (sig[10_080..12_000].iter().map(|x| x * x).sum::<f32>() / 1920.0).sqrt();
        let out_burst_rms = (out[10_080..12_000].iter().map(|x| x * x).sum::<f32>() / 1920.0).sqrt();
        assert!(
            out_burst_rms > in_burst_rms * 0.65,
            "expected sibilant burst preservation (in={:.4}, out={:.4})",
            in_burst_rms,
            out_burst_rms
        );
    }
}
