//! Accuracy Pipeline Layers 2 & 4:
//! - Long utterance overlap splitting & fuzzy suffix-prefix merging (#35)
//! - Low-confidence / repetition detection & automatic 2-pass rescue (#6)
//! - Heterogeneous dual-engine consensus ensemble (`Whisper` x `Qwen3-ASR`) (#34)

use crate::audio::vad::TARGET_SAMPLE_RATE;
use crate::processor::phonetic::{levenshtein_chars, phonetic_similarity};
use crate::stt::{SttEngine, TranscribeContext};

/// Merges two overlapping transcript chunks smoothly without duplicating or dropping boundary words (#35).
pub fn fuzzy_suffix_prefix_merge(prev: &str, next: &str) -> String {
    let a = prev.trim_end();
    let b = next.trim_start();
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() {
        return a.to_string();
    }

    let a_idx: Vec<(usize, char)> = a.char_indices().collect();
    let b_idx: Vec<(usize, char)> = b.char_indices().collect();
    let max_ov = a_idx.len().min(b_idx.len()).min(28);

    // Search longest overlapping suffix of `a` matching prefix of `b` (exact first, then phonetic/fuzzy).
    for ov in (2..=max_ov).rev() {
        let a_start = a_idx[a_idx.len() - ov].0;
        let b_end = if ov < b_idx.len() {
            b_idx[ov].0
        } else {
            b.len()
        };
        let suf = &a[a_start..];
        let pre = &b[..b_end];
        if suf == pre {
            return format!("{}{}", a, &b[b_end..]);
        }
        let max_err = if ov >= 8 { 2 } else if ov >= 4 { 1 } else { 0 };
        if max_err > 0 && levenshtein_chars(suf, pre) <= max_err {
            return format!("{}{}", a, &b[b_end..]);
        }
    }
    format!("{}{}", a, b)
}

/// Checks whether an ASR output looks degenerate/incomplete relative to audio duration (#6).
pub fn looks_low_confidence(text: &str, duration_sec: f32) -> bool {
    let t = text.trim();
    if duration_sec >= 1.5 && t.is_empty() {
        return true;
    }
    let chars: Vec<char> = t.chars().collect();
    if duration_sec >= 2.8 && chars.len() <= 2 {
        return true;
    }
    // Check degenerate single-char loop ("ああああああ")
    let mut run = 1usize;
    for w in chars.windows(2) {
        if w[0] == w[1] {
            run += 1;
            if run >= 6 {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

/// Splits long utterances (> `threshold_sec`) into overlapping chunks (`overlap_sec`) and merges (#35).
pub fn transcribe_with_overlap(
    engine: &dyn SttEngine,
    samples_16k: &[f32],
    ctx: &TranscribeContext,
    threshold_sec: f32,
    overlap_sec: f32,
) -> Result<String, String> {
    let sr = TARGET_SAMPLE_RATE as usize;
    let total_sec = samples_16k.len() as f32 / sr as f32;
    if total_sec <= threshold_sec.max(4.0) {
        return engine.transcribe_with_context(samples_16k, ctx);
    }

    let chunk_len = ((threshold_sec.max(5.0) - overlap_sec.clamp(0.4, 1.5)) * sr as f32) as usize;
    let overlap_samples = (overlap_sec.clamp(0.4, 1.5) * sr as f32) as usize;
    let win_len = chunk_len + overlap_samples;

    let mut pos = 0usize;
    let mut merged = String::new();
    while pos < samples_16k.len() {
        let end = (pos + win_len).min(samples_16k.len());
        let slice = &samples_16k[pos..end];
        if slice.len() >= sr / 4 {
            let part = engine.transcribe_with_context(slice, ctx)?;
            if merged.is_empty() {
                merged = part;
            } else {
                merged = fuzzy_suffix_prefix_merge(&merged, &part);
            }
        }
        if end >= samples_16k.len() {
            break;
        }
        pos += chunk_len.max(sr * 2);
    }
    Ok(merged)
}

/// Heuristic dual-engine candidate selector/reconciler when two heterogeneous engines ran (#34).
pub fn reconcile_dual_candidates(primary: &str, secondary: &str, duration_sec: f32) -> String {
    let p = primary.trim();
    let s = secondary.trim();
    if p.is_empty() {
        return s.to_string();
    }
    if s.is_empty() || p == s {
        return p.to_string();
    }
    if looks_low_confidence(p, duration_sec) && !looks_low_confidence(s, duration_sec) {
        return s.to_string();
    }
    if looks_low_confidence(s, duration_sec) && !looks_low_confidence(p, duration_sec) {
        return p.to_string();
    }
    // If secondary captured notably more substantive content for similar phonetics, prefer longer non-looping text
    let p_len = p.chars().count();
    let s_len = s.chars().count();
    if s_len > p_len + 3 && phonetic_similarity(p, s) >= 0.45 && (s_len as f32) < duration_sec * 16.0 {
        return s.to_string();
    }
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_japanese_chunks_cleanly() {
        assert_eq!(
            fuzzy_suffix_prefix_merge("今日は天気がいいので", "いいので散歩に行く"),
            "今日は天気がいいので散歩に行く"
        );
        // With 1-char ASR variation inside overlap ("いいので" vs "いいのて")
        assert_eq!(
            fuzzy_suffix_prefix_merge("今日は天気がいいので", "いいのて散歩に行く"),
            "今日は天気がいいので散歩に行く"
        );
    }

    #[test]
    fn flags_low_confidence_degenerate_outputs() {
        assert!(looks_low_confidence("", 2.0));
        assert!(looks_low_confidence("え", 3.5));
        assert!(looks_low_confidence("あああああああ", 2.0));
        assert!(!looks_low_confidence("了解、すぐ行くね", 2.2));
    }

    #[test]
    fn reconciles_dual_engine_outputs_rescuing_chopped_primary() {
        assert_eq!(
            reconcile_dual_candidates("え", "ヴァンダル買ってミッド見るよ", 3.2),
            "ヴァンダル買ってミッド見るよ"
        );
        assert_eq!(
            reconcile_dual_candidates("おはようございます", "おはようござい", 1.8),
            "おはようございます"
        );
    }
}
