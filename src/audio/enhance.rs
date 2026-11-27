//! Clean utterance speech conditioning for Whisper / neural STT:
//! 1. Mean DC offset removal (preserves phase & low-frequency naturalness)
//! 2. Consonant-clarity presence EQ (85Hz 2nd-order HPF + ~3.6kHz presence peak) (#2)
//! 3. Utterance-level constant-gain AGC + soft-knee limiter (no syllabic gain pumping!) (#36)
//! 4. Adaptive silence padding (default 200ms head / 300ms tail) (#36)
//! 5. Pitch-preserving WSOLA time-stretch for fast speakers (0.85x..0.92x) (#1)

use crate::audio::vad::TARGET_SAMPLE_RATE;

#[derive(Debug, Clone)]
pub struct EnhanceOptions {
    pub clarity_boost: bool,
    pub eq_strength: f32,
    pub highpass_enabled: bool,
    pub agc_enabled: bool,
    pub head_pad_ms: u64,
    pub tail_pad_ms: u64,
}

impl Default for EnhanceOptions {
    fn default() -> Self {
        Self {
            clarity_boost: true,
            eq_strength: 0.55,
            highpass_enabled: true,
            agc_enabled: true,
            head_pad_ms: 200,
            tail_pad_ms: 300,
        }
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn highpass(fs: f32, fc: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * (fc / fs);
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos_w0) * 0.5) / a0,
            b1: (-(1.0 + cos_w0)) / a0,
            b2: ((1.0 + cos_w0) * 0.5) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn peaking(fs: f32, fc: f32, q: f32, gain_db: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (fc / fs);
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos_w0) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }

    fn process_in_place(&self, buf: &mut [f32]) {
        let (mut z1, mut z2) = (0.0f32, 0.0f32);
        for x in buf.iter_mut() {
            let inp = *x;
            let y = self.b0 * inp + z1;
            z1 = self.b1 * inp - self.a1 * y + z2;
            z2 = self.b2 * inp - self.a2 * y;
            *x = y;
        }
    }
}

/// Applies consonant-clarity 85Hz high-pass + 3.6kHz presence peaking EQ at 16 kHz (#2).
pub fn apply_presence_eq_16k(buf: &mut [f32], highpass: bool, eq_strength: f32) {
    if buf.is_empty() {
        return;
    }
    if highpass {
        let hpf = Biquad::highpass(TARGET_SAMPLE_RATE as f32, 85.0, 0.707);
        hpf.process_in_place(buf);
    }
    let st = eq_strength.clamp(0.0, 1.0);
    if st > 0.02 {
        // Gentle +0..5.0 dB boost centered at 3.6 kHz crispens Japanese unvoiced sibilants/plosives
        let peak = Biquad::peaking(TARGET_SAMPLE_RATE as f32, 3600.0, 1.05, st * 5.0);
        peak.process_in_place(buf);
    }
}

/// Estimates syllable/energy modulation tempo (peaks/sec) to trigger auto WSOLA on fast speech (#1).
pub fn estimate_speech_tempo_score(samples_16k: &[f32]) -> f32 {
    if samples_16k.len() < 4000 {
        return 0.0;
    }
    let frame = 320usize; // 20ms
    let envs: Vec<f32> = samples_16k
        .chunks(frame)
        .map(|c| (c.iter().map(|&x| x * x).sum::<f32>() / c.len().max(1) as f32).sqrt())
        .collect();
    let max_e = envs.iter().copied().fold(0.0f32, f32::max);
    if max_e < 0.005 {
        return 0.0;
    }
    let thresh = max_e * 0.28;
    let mut peaks = 0usize;
    for w in envs.windows(3) {
        if w[1] > thresh && w[1] > w[0] * 1.08 && w[1] >= w[2] {
            peaks += 1;
        }
    }
    let active_secs = (envs.iter().filter(|&&e| e > thresh * 0.5).count() as f32 * 0.02).max(0.25);
    peaks as f32 / active_secs
}

/// Pure-Rust WSOLA (Waveform Similarity Overlap-Add) time-stretching preserving pitch (#1).
/// `speed_rate < 1.0` (e.g. `0.85..=0.92`) slows fast speech smoothly without lowering pitch.
pub fn wsola_time_stretch(samples_16k: &[f32], speed_rate: f32) -> Vec<f32> {
    let rate = speed_rate.clamp(0.75, 1.05);
    if (rate - 1.0).abs() < 0.015 || samples_16k.len() < 800 {
        return samples_16k.to_vec();
    }
    let win_len = 400usize; // 25ms @ 16kHz
    let hop_out = 200usize; // 12.5ms
    let search_radius = 96usize; // +-6ms cross-correlation tolerance

    let hann: Vec<f32> = (0..win_len)
        .map(|i| {
            let phase = (2.0 * std::f32::consts::PI * i as f32) / (win_len as f32);
            0.5 * (1.0 - phase.cos())
        })
        .collect();

    let est_out = ((samples_16k.len() as f64) / (rate as f64)).ceil() as usize + win_len;
    let mut out = vec![0.0f32; est_out];
    let mut norm = vec![0.0f32; est_out];

    // Copy initial frame directly
    let first_n = win_len.min(samples_16k.len());
    for i in 0..first_n {
        out[i] += samples_16k[i] * hann[i];
        norm[i] += hann[i];
    }

    let mut out_pos = hop_out;
    let mut in_pos_f = (hop_out as f64) * (rate as f64);
    let mut prev_src_end = win_len.min(samples_16k.len());

    while out_pos + win_len <= out.len() {
        let nominal_in = in_pos_f.round() as usize;
        if nominal_in + win_len > samples_16k.len() {
            break;
        }
        // Target continuity region from tail of previous source frame cleanly
        let natural_next = prev_src_end.saturating_sub(hop_out);
        let lo = nominal_in.saturating_sub(search_radius);
        let hi = (nominal_in + search_radius).min(samples_16k.len() - win_len);

        let mut best_in = nominal_in.clamp(lo, hi);
        if natural_next + hop_out <= samples_16k.len() {
            let mut best_corr = f32::NEG_INFINITY;
            for cand in lo..=hi {
                let mut dot = 0.0f32;
                let mut e1 = 0.0f32;
                let mut e2 = 0.0f32;
                for j in (0..hop_out).step_by(2) {
                    let a = samples_16k[natural_next + j];
                    let b = samples_16k[cand + j];
                    dot += a * b;
                    e1 += a * a;
                    e2 += b * b;
                }
                let denom = (e1 * e2).sqrt().max(1e-6);
                let corr = dot / denom;
                if corr > best_corr {
                    best_corr = corr;
                    best_in = cand;
                }
            }
        }

        for i in 0..win_len {
            let w = hann[i];
            out[out_pos + i] += samples_16k[best_in + i] * w;
            norm[out_pos + i] += w;
        }
        prev_src_end = best_in + win_len;
        out_pos += hop_out;
        in_pos_f += (hop_out as f64) * (rate as f64);
    }

    let final_len = out_pos.min(out.len());
    out.truncate(final_len);
    for i in 0..final_len {
        if norm[i] > 1e-4 {
            out[i] = (out[i] / norm[i]).clamp(-1.0, 1.0);
        }
    }
    out
}

/// Backward-compatible wrapper preserving default enhancement behavior.
pub fn enhance_utterance_16k(
    samples: &[f32],
    clarity_boost: bool,
    agc_enabled: bool,
) -> Vec<f32> {
    enhance_utterance_16k_with_options(
        samples,
        &EnhanceOptions {
            clarity_boost,
            agc_enabled,
            head_pad_ms: 250,
            tail_pad_ms: 350,
            highpass_enabled: false,
            eq_strength: if clarity_boost { 0.55 } else { 0.0 },
        },
    )
}

/// Full Layer-1 utterance speech conditioning (#2, #36).
pub fn enhance_utterance_16k_with_options(samples: &[f32], opts: &EnhanceOptions) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    // 1. Simple DC offset removal (subtract mean)
    let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
    let mut buf: Vec<f32> = samples.iter().map(|&x| x - mean).collect();

    // 2. Consonant-clarity 85Hz HPF + 3.6kHz presence EQ (or legacy gentle lift when EQ=0)
    if opts.highpass_enabled || (opts.clarity_boost && opts.eq_strength > 0.02) {
        apply_presence_eq_16k(&mut buf, opts.highpass_enabled, if opts.clarity_boost { opts.eq_strength } else { 0.0 });
    } else if opts.clarity_boost {
        let mut prev = 0.0f32;
        for s in buf.iter_mut() {
            let x = *s;
            *s = x - 0.10 * prev;
            prev = x;
        }
    }

    // 3. Utterance-level constant-gain AGC + soft-knee limiter
    if opts.agc_enabled {
        let frame_len = 320; // 20ms at 16kHz
        let mut frame_rms: Vec<f32> = buf
            .chunks(frame_len)
            .map(|c| {
                let sq: f32 = c.iter().map(|&v| v * v).sum();
                (sq / c.len().max(1) as f32).sqrt()
            })
            .collect();
        frame_rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p85 = frame_rms
            .get((frame_rms.len() * 85) / 100)
            .copied()
            .unwrap_or(0.0);

        if p85 > 0.0012 {
            let gain = if (0.055..=0.20).contains(&p85) {
                1.0
            } else {
                (0.085 / p85).clamp(0.4, 6.0)
            };

            if (gain - 1.0).abs() > 0.01 {
                for s in buf.iter_mut() {
                    let boosted = *s * gain;
                    *s = if boosted.abs() <= 0.90 {
                        boosted
                    } else {
                        boosted.signum() * (0.90 + 0.09 * ((boosted.abs() - 0.90) / 0.09).tanh())
                    };
                }
            } else {
                for s in buf.iter_mut() {
                    if s.abs() > 0.90 {
                        let v = *s;
                        *s = v.signum() * (0.90 + 0.09 * ((v.abs() - 0.90) / 0.09).tanh());
                    }
                }
            }
        }
    }

    // 4. Clean silence margins (head >= 200ms + tail >= 300ms per #36)
    let head_pad = (TARGET_SAMPLE_RATE as usize * opts.head_pad_ms.max(200) as usize) / 1000;
    let tail_pad = (TARGET_SAMPLE_RATE as usize * opts.tail_pad_ms.max(300) as usize) / 1000;
    let mut padded = Vec::with_capacity(head_pad + buf.len() + tail_pad);
    padded.resize(head_pad, 0.0);
    padded.extend_from_slice(&buf);
    padded.resize(padded.len() + tail_pad, 0.0);
    padded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc_lifts_quiet_utterance_and_adds_silence_padding() {
        // Simulate quiet signal (amplitude 0.015, 0.5s at 16kHz)
        let quiet: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 0.12).sin() * 0.015)
            .collect();
        let out = enhance_utterance_16k(&quiet, true, true);
        // 250ms (4000) + 8000 + 350ms (5600) = 17600
        assert_eq!(out.len(), 17600);
        let mid_rms = (out[4500..11500].iter().map(|x| x * x).sum::<f32>() / 7000.0).sqrt();
        assert!(
            mid_rms > 0.05 && mid_rms < 0.20,
            "expected AGC to normalize quiet audio to ~0.085 RMS, got {:.4}",
            mid_rms
        );
    }

    #[test]
    fn normal_loudness_passes_through_without_gain_pumping() {
        let normal: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 0.12).sin() * 0.12)
            .collect();
        let out = enhance_utterance_16k(&normal, false, true);
        assert_eq!(out.len(), 17600);
        let in_rms = (normal.iter().map(|x| x * x).sum::<f32>() / normal.len() as f32).sqrt();
        let out_rms = (out[4500..11500].iter().map(|x| x * x).sum::<f32>() / 7000.0).sqrt();
        assert!(
            (out_rms - in_rms).abs() < 0.01,
            "expected unity gain on normal speech (in={:.4}, out={:.4})",
            in_rms,
            out_rms
        );
    }

    #[test]
    fn wsola_stretches_fast_speech_preserving_pitch() {
        // Generate 0.6s 220Hz sine wave at 16kHz (9600 samples)
        let sig: Vec<f32> = (0..9600)
            .map(|i| (2.0 * std::f32::consts::PI * 220.0 * (i as f32 / 16_000.0)).sin() * 0.25)
            .collect();
        let slow = wsola_time_stretch(&sig, 0.85);
        // Should be longer (~1 / 0.85 = ~11294 samples)
        assert!(
            slow.len() > 10_400,
            "expected stretched len > 10400, got {}",
            slow.len()
        );
        // Check zero-crossing rate in middle matches ~220Hz (two crossings per period -> ~440 crossings/sec)
        let mid = &slow[2000..6000]; // 0.25s
        let zc = mid.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
        let est_hz = (zc as f32 / 2.0) / 0.25;
        assert!(
            (est_hz - 220.0).abs() < 20.0,
            "expected preserved pitch ~220Hz, got {:.1}Hz",
            est_hz
        );
    }
}
