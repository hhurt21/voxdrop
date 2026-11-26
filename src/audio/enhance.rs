//! Clean utterance speech conditioning for Whisper / neural STT:
//! 1. Mean DC offset removal (preserves phase & low-frequency naturalness without double high-pass)
//! 2. Optional phase-continuous gentle vocal-presence pre-emphasis (zero frame-boundary clicks!)
//! 3. Utterance-level constant-gain AGC + soft-knee limiter (only scales quiet/clipping mics;
//!    normal conversational loudness passes through at unity gain without syllabic pumping!)
//! 4. Generous clean silence margins (250ms head / 350ms tail) so Whisper never drops first/last morae.

use crate::audio::vad::TARGET_SAMPLE_RATE;

/// Enhances a 16 kHz mono utterance buffer prior to neural STT inference.
pub fn enhance_utterance_16k(
    samples: &[f32],
    clarity_boost: bool,
    agc_enabled: bool,
) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    // 1. Simple DC offset removal (subtract mean)
    let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
    let mut buf: Vec<f32> = samples.iter().map(|&x| x - mean).collect();

    // 2. Phase-continuous gentle clarity contour across entire utterance (no 20ms boundary clicks)
    if clarity_boost {
        let mut prev = 0.0f32;
        for s in buf.iter_mut() {
            let x = *s;
            *s = x - 0.10 * prev;
            prev = x;
        }
    }

    // 3. Utterance-level constant-gain AGC (avoids 20ms frame pumping that breaks Whisper Mel features)
    if agc_enabled {
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

    // 4. Clean silence margins (250ms head + 350ms tail) so Whisper never drops leading/trailing syllables
    let head_pad = (TARGET_SAMPLE_RATE as usize * 250) / 1000;
    let tail_pad = (TARGET_SAMPLE_RATE as usize * 350) / 1000;
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
}
