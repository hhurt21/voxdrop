//! Accuracy Pipeline Layers 2 & 4:
//! - Long utterance overlap splitting & fuzzy suffix-prefix merging (#35)
//! - Low-confidence / repetition detection & automatic 2-pass rescue (#6)
//! - Heterogeneous dual-engine consensus ensemble (`Whisper` x `Qwen3-ASR`) (#34)
//! - Morpheme / phrase-level ROVER consensus combining token probabilities, dictionary & N-gram (#3)

use crate::audio::vad::TARGET_SAMPLE_RATE;
use crate::config::DictionaryEntry;
use crate::processor::ngram::PersonalNgramModel;
use crate::processor::phonetic::{
    levenshtein_chars, phonetic_similarity, segment_morphemes_with_readings, MorphemeToken,
};
use crate::stt::{SttEngine, TranscribeContext, TranscribedToken, TranscriptWithConfidence};

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
#[allow(dead_code)]
pub fn transcribe_with_overlap(
    engine: &dyn SttEngine,
    samples_16k: &[f32],
    ctx: &TranscribeContext,
    threshold_sec: f32,
    overlap_sec: f32,
) -> Result<String, String> {
    transcribe_with_overlap_confidence(engine, samples_16k, ctx, threshold_sec, overlap_sec)
        .map(|r| r.text)
}

/// Overlap chunking variant preserving per-token confidence on single-chunk utterances (#2 + #35).
pub fn transcribe_with_overlap_confidence(
    engine: &dyn SttEngine,
    samples_16k: &[f32],
    ctx: &TranscribeContext,
    threshold_sec: f32,
    overlap_sec: f32,
) -> Result<TranscriptWithConfidence, String> {
    let sr = TARGET_SAMPLE_RATE as usize;
    let total_sec = samples_16k.len() as f32 / sr as f32;
    if total_sec <= threshold_sec.max(4.0) {
        return engine.transcribe_with_confidence(samples_16k, ctx);
    }

    let chunk_len = ((threshold_sec.max(5.0) - overlap_sec.clamp(0.4, 1.5)) * sr as f32) as usize;
    let overlap_samples = (overlap_sec.clamp(0.4, 1.5) * sr as f32) as usize;
    let win_len = chunk_len + overlap_samples;

    let mut pos = 0usize;
    let mut merged = String::new();
    let mut all_tokens: Vec<TranscribedToken> = Vec::new();
    while pos < samples_16k.len() {
        let end = (pos + win_len).min(samples_16k.len());
        let slice = &samples_16k[pos..end];
        if slice.len() >= sr / 4 {
            let part = engine.transcribe_with_confidence(slice, ctx)?;
            if merged.is_empty() {
                merged = part.text;
            } else {
                merged = fuzzy_suffix_prefix_merge(&merged, &part.text);
            }
            all_tokens.extend(part.tokens);
        }
        if end >= samples_16k.len() {
            break;
        }
        pos += chunk_len.max(sr * 2);
    }
    let avg_prob = if all_tokens.is_empty() {
        1.0
    } else {
        all_tokens.iter().map(|t| t.prob).sum::<f32>() / all_tokens.len() as f32
    };
    Ok(TranscriptWithConfidence {
        text: merged,
        tokens: all_tokens,
        avg_prob,
    })
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
    let p_len = p.chars().count();
    let s_len = s.chars().count();
    if s_len > p_len + 3 && phonetic_similarity(p, s) >= 0.45 && (s_len as f32) < duration_sec * 16.0 {
        return s.to_string();
    }
    p.to_string()
}

fn map_morpheme_probs(
    text: &str,
    morphemes: &[MorphemeToken],
    tokens: &[TranscribedToken],
) -> Vec<f32> {
    if morphemes.is_empty() {
        return Vec::new();
    }
    if tokens.is_empty() {
        return vec![1.0; morphemes.len()];
    }
    let mut token_spans: Vec<(usize, usize, f32)> = Vec::new();
    let mut cursor = 0usize;
    for t in tokens {
        let p = t.text.trim();
        if p.is_empty() {
            continue;
        }
        if let Some(rel) = text[cursor..].find(p) {
            let s = cursor + rel;
            let e = s + p.len();
            cursor = e;
            token_spans.push((s, e, t.prob));
        }
    }

    morphemes
        .iter()
        .map(|m| {
            let mut sum = 0.0f32;
            let mut cnt = 0usize;
            for &(s, e, p) in &token_spans {
                if s < m.byte_end && m.byte_start < e {
                    sum += p;
                    cnt += 1;
                }
            }
            if cnt > 0 {
                sum / cnt as f32
            } else {
                0.75
            }
        })
        .collect()
}

fn has_triple_char_loop(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut run = 1usize;
    for w in chars.windows(2) {
        if w[0] == w[1] {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

fn score_conflict_span(
    cand: &str,
    other: &str,
    is_primary: bool,
    primary_avg_prob: f32,
    dict: &[DictionaryEntry],
    ngram: Option<&PersonalNgramModel>,
    prev_surface: Option<&str>,
    next_surface: Option<&str>,
) -> f32 {
    let c = cand.trim();
    if c.is_empty() {
        return -3.0;
    }
    let mut score = if is_primary { 0.1 } else { 0.0 };

    // 1. Dictionary match boost (+2.5)
    for d in dict {
        let tgt = d.to.trim();
        if tgt.chars().count() >= 2 && c.contains(tgt) {
            score += 2.5;
            break;
        }
    }

    // 2. Personal N-gram LM context boost
    if let Some(lm) = ngram {
        score += lm.score_phrase(prev_surface, c, next_surface).min(2.5);
    }

    // 3. Whisper token probability signal
    if is_primary {
        if primary_avg_prob >= 0.78 {
            score += 1.2;
        } else if primary_avg_prob < 0.55 {
            score -= 1.2;
        }
    } else if primary_avg_prob < 0.55 {
        score += 1.0;
    }

    // 4. Unnatural hiragana run / repetition penalty
    if has_triple_char_loop(c) {
        score -= 3.0;
    }
    let c_only_hira = c.chars().all(|ch| ('\u{3041}'..='\u{3096}').contains(&ch) || ch == 'ー');
    let other_has_content_script = other.chars().any(|ch| {
        ('\u{30A1}'..='\u{30F6}').contains(&ch) || ('\u{4E00}'..='\u{9FFF}').contains(&ch)
    });
    if c_only_hira && c.chars().count() >= 3 && other_has_content_script && !c.is_empty() {
        score -= 0.8;
    }

    score
}

/// Morpheme / bunsetsu-level ROVER consensus combining Whisper primary (+ token confidence),
/// secondary STT candidate, custom dictionary, and personal N-gram LM (#3).
pub fn reconcile_dual_candidates_word_level(
    primary: &TranscriptWithConfidence,
    secondary_text: &str,
    dict: &[DictionaryEntry],
    ngram: Option<&PersonalNgramModel>,
    duration_sec: f32,
) -> String {
    let p_text = primary.text.trim();
    let s_text = secondary_text.trim();
    if p_text.is_empty() {
        return s_text.to_string();
    }
    if s_text.is_empty() || p_text == s_text {
        return p_text.to_string();
    }
    if looks_low_confidence(p_text, duration_sec) && !looks_low_confidence(s_text, duration_sec) {
        return s_text.to_string();
    }
    if looks_low_confidence(s_text, duration_sec) && !looks_low_confidence(p_text, duration_sec) {
        return p_text.to_string();
    }

    let p_morphs = segment_morphemes_with_readings(p_text);
    let s_morphs = segment_morphemes_with_readings(s_text);
    if p_morphs.is_empty() || s_morphs.is_empty() {
        return reconcile_dual_candidates(p_text, s_text, duration_sec);
    }

    let p_probs = map_morpheme_probs(p_text, &p_morphs, &primary.tokens);

    // Exact surface DP anchors so we localize differences cleanly between shared morphemes
    let m = p_morphs.len();
    let n = s_morphs.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if p_morphs[i].surface == s_morphs[j].surface {
                1 + dp[i + 1][j + 1]
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Extract anchor pairs (i, j) where p_morphs[i].surface == s_morphs[j].surface
    let mut anchors: Vec<(usize, usize)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if p_morphs[i].surface == s_morphs[j].surface {
            anchors.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    let mut out = String::new();
    let mut p_cur = 0usize;
    let mut s_cur = 0usize;

    let mut anchor_list = anchors;
    anchor_list.push((m, n));

    for (ai, aj) in anchor_list {
        if p_cur < ai || s_cur < aj {
            let p_seg: String = p_morphs[p_cur..ai]
                .iter()
                .map(|t| t.surface.as_str())
                .collect();
            let s_seg: String = s_morphs[s_cur..aj]
                .iter()
                .map(|t| t.surface.as_str())
                .collect();

            if p_seg == s_seg {
                out.push_str(&p_seg);
            } else {
                let p_avg_prob = if p_cur < ai {
                    let slice = &p_probs[p_cur..ai];
                    slice.iter().sum::<f32>() / slice.len() as f32
                } else {
                    0.5
                };
                let prev_surf = p_cur.checked_sub(1).and_then(|idx| p_morphs.get(idx)).map(|t| t.surface.as_str());
                let next_surf = p_morphs.get(ai).map(|t| t.surface.as_str());

                let p_score = score_conflict_span(
                    &p_seg,
                    &s_seg,
                    true,
                    p_avg_prob,
                    dict,
                    ngram,
                    prev_surf,
                    next_surf,
                );
                let s_score = score_conflict_span(
                    &s_seg,
                    &p_seg,
                    false,
                    p_avg_prob,
                    dict,
                    ngram,
                    prev_surf,
                    next_surf,
                );
                if s_score > p_score {
                    out.push_str(&s_seg);
                } else {
                    out.push_str(&p_seg);
                }
            }
        }
        if ai < m && aj < n {
            out.push_str(&p_morphs[ai].surface);
        }
        p_cur = ai + 1;
        s_cur = aj + 1;
    }

    if out.trim().is_empty() {
        p_text.to_string()
    } else {
        out
    }
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

    #[test]
    fn word_level_rover_combines_best_phrases_from_primary_and_secondary() {
        // Primary got the first half ("輝晶獣で") cleanly (prob 0.93), failed second half ("えこら") (prob 0.31)
        // Secondary missed the first half ("気象獣で"), got second half ("エコラウンド") right!
        let primary = TranscriptWithConfidence {
            text: "輝晶獣で戦ってからえこらしよう".into(),
            tokens: vec![
                TranscribedToken { text: "輝".into(), prob: 0.93 },
                TranscribedToken { text: "晶".into(), prob: 0.95 },
                TranscribedToken { text: "獣".into(), prob: 0.92 },
                TranscribedToken { text: "で".into(), prob: 0.95 },
                TranscribedToken { text: "戦".into(), prob: 0.94 },
                TranscribedToken { text: "っ".into(), prob: 0.94 },
                TranscribedToken { text: "て".into(), prob: 0.94 },
                TranscribedToken { text: "から".into(), prob: 0.92 },
                TranscribedToken { text: "え".into(), prob: 0.31 },
                TranscribedToken { text: "こ".into(), prob: 0.28 },
                TranscribedToken { text: "ら".into(), prob: 0.30 },
                TranscribedToken { text: "しよう".into(), prob: 0.95 },
            ],
            avg_prob: 0.78,
        };
        let secondary = "気象獣で戦ってからエコラウンドしよう";
        let dict = vec![DictionaryEntry {
            from: "えこら".into(),
            to: "エコラウンド".into(),
        }];
        let mut lm = PersonalNgramModel::default();
        lm.observe_text("輝晶獣で戦う");
        lm.observe_text("輝晶獣で戦う");

        let merged =
            reconcile_dual_candidates_word_level(&primary, secondary, &dict, Some(&lm), 3.0);
        assert_eq!(merged, "輝晶獣で戦ってからエコラウンドしよう");
    }
}
