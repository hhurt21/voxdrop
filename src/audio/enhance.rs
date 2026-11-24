//! Broadcast & radio-grade utterance speech conditioning for slurred / quiet / noisy speech:
//! 1. High-pass rumble removal (80 Hz)
//! 2. Gentle quiet-mic Automatic Gain Control (AGC) + soft-knee limiter (only lifts quiet signals,
//!    never distorts normal loudness or amplifies hiss into clipping!)
//! 3. Clean short silence margins (80ms head / 120ms tail) so ASR never clips first/last consonant
//!    without skewing SenseVoice's utterance-level CMVN.

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

    // 1. Remove DC offset & sub-80Hz rumble (1st-order high-pass at 16 kHz, alpha ≈ 0.969)
    let mut buf = Vec::with_capacity(samples.len());
    let mut hp_prev_in = 0.0f32;
    let mut hp_prev_out = 0.0f32;
    for &x in samples {
        let y = 0.969 * (hp_prev_out + x - hp_prev_in);
        hp_prev_in = x;
        hp_prev_out = y;
        buf.push(y);
    }

    // 2. Mild vocal-presence contour (gentle 15% blend so muffled voices brighten WITHOUT boosting 6-8kHz radio hiss)
    if clarity_boost {
        let mut prev = 0.0f32;
        for s in &mut buf {
            let x = *s;
            let pre = x - 0.35 * prev;
            prev = x;
            *s = x * 0.82 + pre * 0.18;
        }
    }

    // 3. Smart Automatic Gain Control (AGC):
    //    Only boosts when the utterance is genuinely quiet (e.g. Steinberg UR22mkII / distant mic / quiet radio),
    //    and gently limits overly hot signals, preserving natural dynamics for SenseVoice.
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
        // 85th percentile frame RMS represents active speech loudness
        let p85 = frame_rms
            .get((frame_rms.len() * 85) / 100)
            .copied()
            .unwrap_or(0.0);

        if p85 > 0.0012 && (p85 < 0.065 || p85 > 0.25) {
            let target_rms = 0.095f32;
            let gain = (target_rms / p85).clamp(0.4, 12.0);
            for s in &mut buf {
                let boosted = *s * gain;
                *s = if boosted.abs() <= 0.88 {
                    boosted
                } else {
                    boosted.signum() * (0.88 + 0.11 * ((boosted.abs() - 0.88) / 0.11).tanh())
                };
            }
        }
    }

    // 4. Short clean silence padding (80ms head + 120ms tail)
    let head_pad = (TARGET_SAMPLE_RATE as usize * 80) / 1000;
    let tail_pad = (TARGET_SAMPLE_RATE as usize * 120) / 1000;
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
        // Simulate quiet Steinberg / radio signal (amplitude 0.015, 0.5s at 16kHz)
        let quiet: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 0.12).sin() * 0.015)
            .collect();
        let out = enhance_utterance_16k(&quiet, true, true);
        // 80ms (1280) + 8000 + 120ms (1920) = 11200
        assert_eq!(out.len(), 11200);
        let mid_rms = (out[2000..8000].iter().map(|x| x * x).sum::<f32>() / 6000.0).sqrt();
        assert!(
            mid_rms > 0.06 && mid_rms < 0.25,
            "expected AGC to normalize quiet audio to ~0.095 RMS, got {:.4}",
            mid_rms
        );
    }
}
