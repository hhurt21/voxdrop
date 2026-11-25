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

    // Compute 20ms frame RMS distribution to separate room noise floor vs active syllables
    let frame_len = 320; // 20ms at 16kHz
    let frame_rms: Vec<f32> = buf
        .chunks(frame_len)
        .map(|c| {
            let sq: f32 = c.iter().map(|&v| v * v).sum();
            (sq / c.len().max(1) as f32).sqrt()
        })
        .collect();
    let mut sorted_rms = frame_rms.clone();
    sorted_rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p20_noise = sorted_rms
        .get((sorted_rms.len() * 20) / 100)
        .copied()
        .unwrap_or(0.0);
    let p85 = sorted_rms
        .get((sorted_rms.len() * 85) / 100)
        .copied()
        .unwrap_or(0.0);

    // 2. Gated sibilant / vocal-presence contour (lifts 3-7.5 kHz 'さしすせそ' clarity cleanly ONLY on
    //    active speech frames so silent background hiss is never amplified!)
    if clarity_boost {
        let mut prev = 0.0f32;
        let speech_gate_th = (p20_noise * 1.45).max(0.0020);
        for (chunk_idx, chunk) in buf.chunks_mut(frame_len).enumerate() {
            let f_rms = frame_rms.get(chunk_idx).copied().unwrap_or(0.0);
            let blend = if f_rms > speech_gate_th { 0.14 } else { 0.0 };
            for s in chunk.iter_mut() {
                let x = *s;
                let pre = x - 0.38 * prev;
                prev = x;
                *s = x * (1.0 - blend) + pre * blend;
            }
        }
    }

    // 3. Smart Automatic Gain Control (AGC) + Adaptive Upward Syllabic Leveler:
    //    Normalizes overall speech level AND smoothly lifts softer consonants / slurred syllables
    //    (like unvoiced 'さしすせそ') relative to louder vowels without boosting quiet gaps!
    if agc_enabled && p85 > 0.0012 {
        let target_rms = 0.095f32;
        let global_gain = if p85 < 0.080 || p85 > 0.25 {
            (target_rms / p85).clamp(0.4, 12.0)
        } else {
            1.0
        };
        let active_th = (p20_noise * 1.65).max(0.0025);
        let mut smooth_frame_gain = 1.0f32;

        for (chunk_idx, chunk) in buf.chunks_mut(frame_len).enumerate() {
            let raw_rms = frame_rms.get(chunk_idx).copied().unwrap_or(0.0);
            let normed_rms = raw_rms * global_gain;
            let syllabic_target = if raw_rms > active_th && normed_rms < 0.080 {
                (0.080 / normed_rms.max(0.004)).clamp(1.0, 1.85)
            } else {
                1.0
            };
            smooth_frame_gain = smooth_frame_gain * 0.45 + syllabic_target * 0.55;
            let total_gain = global_gain * smooth_frame_gain;

            for s in chunk.iter_mut() {
                let boosted = *s * total_gain;
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
