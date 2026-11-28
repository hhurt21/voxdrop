//! Pure-Rust Acoustic Echo Canceller (NLMS) + WASAPI Loopback Reference Capture (#7).
//! Uses official `cpal` WASAPI output loopback (`default_output_device().build_input_stream`)
//! to capture PC speaker audio (game voices, BGM, Discord VC) as a reference signal at 16 kHz mono,
//! subtracting correlated speaker echo from the mic input without external DLLs.

use crate::audio::capture::{downmix_to_mono, LinearResampler};
use crate::audio::vad::TARGET_SAMPLE_RATE;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_REF_SAMPLES_16K: usize = 8000; // 500ms reference history @ 16kHz
const FILTER_TAPS: usize = 512; // 32ms adaptive FIR window @ 16kHz

#[derive(Default)]
pub struct LoopbackRefBuffer {
    samples_16k: VecDeque<f32>,
}

impl LoopbackRefBuffer {
    pub fn push(&mut self, chunk_16k: &[f32]) {
        for &s in chunk_16k {
            if self.samples_16k.len() >= MAX_REF_SAMPLES_16K {
                self.samples_16k.pop_front();
            }
            self.samples_16k.push_back(s);
        }
    }

    pub fn take_tail_aligned(&mut self, count: usize) -> Vec<f32> {
        let len = self.samples_16k.len();
        if len == 0 {
            return vec![0.0; count];
        }
        let take = count.min(len);
        let mut out = Vec::with_capacity(count);
        for _ in 0..(count - take) {
            out.push(0.0);
        }
        let start = len - take;
        for i in start..len {
            out.push(self.samples_16k[i]);
        }
        // Keep a tiny tail overlap, drain consumed samples so we stay synced
        let drain_n = take.saturating_sub(0);
        for _ in 0..drain_n {
            let _ = self.samples_16k.pop_front();
        }
        out
    }

    pub fn clear(&mut self) {
        self.samples_16k.clear();
    }
}

/// Normalized Least Mean Squares (NLMS) adaptive echo canceller (#7).
pub struct NlmsEchoCanceller {
    weights: Vec<f32>,
    ref_hist: Vec<f32>,
    mu: f32,
}

impl NlmsEchoCanceller {
    pub fn new(strength: f32) -> Self {
        Self {
            weights: vec![0.0; FILTER_TAPS],
            ref_hist: vec![0.0; FILTER_TAPS],
            mu: (0.15 + 0.55 * strength.clamp(0.0, 1.0)).clamp(0.1, 0.8),
        }
    }

    pub fn set_strength(&mut self, strength: f32) {
        self.mu = (0.15 + 0.55 * strength.clamp(0.0, 1.0)).clamp(0.1, 0.8);
    }

    pub fn reset(&mut self) {
        self.weights.fill(0.0);
        self.ref_hist.fill(0.0);
    }

    /// Cancels correlated speaker echo `ref_samples` from `mic_samples` in place.
    /// Completely bypasses adaptation and subtraction when speaker reference is silent (`RMS < 0.002`).
    pub fn process_in_place(&mut self, mic_samples: &mut [f32], ref_samples: &[f32]) {
        if mic_samples.is_empty() || ref_samples.is_empty() {
            return;
        }
        let ref_rms = (ref_samples.iter().map(|&r| r * r).sum::<f32>()
            / ref_samples.len().max(1) as f32)
            .sqrt();
        if ref_rms < 0.002 {
            // Silent speaker: true zero-distortion bypass!
            return;
        }

        let n = mic_samples.len().min(ref_samples.len());
        let mic_rms =
            (mic_samples[..n].iter().map(|&m| m * m).sum::<f32>() / n.max(1) as f32).sqrt();
        // Mild double-talk guard: freeze/slow step if user speaks significantly louder than ref
        let eff_mu = if mic_rms > ref_rms * 2.5 {
            self.mu * 0.15
        } else {
            self.mu
        };

        for idx in 0..n {
            // Shift reference delay line
            self.ref_hist.copy_within(0..FILTER_TAPS - 1, 1);
            self.ref_hist[0] = ref_samples[idx];

            let mut y_hat = 0.0f32;
            let mut norm = 1e-4f32;
            for k in 0..FILTER_TAPS {
                let r = self.ref_hist[k];
                y_hat += self.weights[k] * r;
                norm += r * r;
            }

            let err = mic_samples[idx] - y_hat;
            let step = (eff_mu * err) / norm;
            for k in 0..FILTER_TAPS {
                let w = self.weights[k] + step * self.ref_hist[k];
                self.weights[k] = w.clamp(-1.5, 1.5);
            }
            mic_samples[idx] = err.clamp(-1.0, 1.0);
        }
    }
}

/// Starts a non-fatal WASAPI Loopback reference capture stream into `ref_buf` @ 16 kHz mono.
pub fn start_loopback_reference_stream(ref_buf: Arc<Mutex<LoopbackRefBuffer>>) -> Option<Stream> {
    let host = cpal::default_host();
    let out_dev = host.default_output_device()?;
    let supported = out_dev.default_output_config().ok()?;
    let sample_format = supported.sample_format();
    let cfg: StreamConfig = supported.into();
    let channels = cfg.channels as usize;
    let in_rate = cfg.sample_rate.0;

    let resampler = Arc::new(Mutex::new(LinearResampler::new(in_rate, TARGET_SAMPLE_RATE)));
    let handle_f32 = {
        let ref_buf = ref_buf.clone();
        let resampler = resampler.clone();
        move |data: &[f32]| {
            let mono = downmix_to_mono(data, channels);
            let pcm_16k = match resampler.lock() {
                Ok(mut r) => r.process(&mono),
                Err(_) => return,
            };
            if !pcm_16k.is_empty() {
                if let Ok(mut b) = ref_buf.lock() {
                    b.push(&pcm_16k);
                }
            }
        }
    };

    let err_cb = |err: cpal::StreamError| {
        eprintln!("[voxdrop aec loopback warning] {}", err);
    };

    // On Windows WASAPI, `build_input_stream` on an output device opens WASAPI Loopback!
    let stream = match sample_format {
        SampleFormat::F32 => out_dev
            .build_input_stream(&cfg, move |d: &[f32], _| handle_f32(d), err_cb, None)
            .ok()?,
        SampleFormat::I16 => out_dev
            .build_input_stream(
                &cfg,
                move |d: &[i16], _| {
                    let f: Vec<f32> = d.iter().map(|&s| s as f32 / 32768.0).collect();
                    handle_f32(&f);
                },
                err_cb,
                None,
            )
            .ok()?,
        SampleFormat::U16 => out_dev
            .build_input_stream(
                &cfg,
                move |d: &[u16], _| {
                    let f: Vec<f32> = d.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                    handle_f32(&f);
                },
                err_cb,
                None,
            )
            .ok()?,
        _ => return None,
    };

    stream.play().ok()?;
    Some(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nlms_aec_bypasses_when_silent_and_attenuates_correlated_speaker_echo() {
        let mut aec = NlmsEchoCanceller::new(0.85);

        // 1. Silent reference -> zero modification
        let mut mic_clean = vec![0.12, -0.08, 0.05, 0.22];
        let silent_ref = vec![0.0; 4];
        aec.process_in_place(&mut mic_clean, &silent_ref);
        assert_eq!(mic_clean, vec![0.12, -0.08, 0.05, 0.22]);

        // 2. Correlated echo (delayed speaker sine + white-like mixture in mic)
        let n = 4800;
        let spk: Vec<f32> = (0..n)
            .map(|i| {
                0.20 * (i as f32 * 0.07).sin()
                    + 0.12 * (i as f32 * 0.23).cos()
                    + 0.08 * ((i * 7 % 19) as f32 - 9.0) / 9.0
            })
            .collect();
        let delay = 8usize;
        let mut mic: Vec<f32> = (0..n)
            .map(|i| if i >= delay { spk[i - delay] * 0.65 } else { 0.0 })
            .collect();
        let before_rms = (mic[2400..].iter().map(|x| x * x).sum::<f32>() / 2400.0).sqrt();
        aec.process_in_place(&mut mic, &spk);
        let after_rms = (mic[2400..].iter().map(|x| x * x).sum::<f32>() / 2400.0).sqrt();
        assert!(
            after_rms < before_rms * 0.35,
            "expected NLMS AEC to strongly suppress echo ({:.4} < {:.4})",
            after_rms,
            before_rms
        );
    }
}
