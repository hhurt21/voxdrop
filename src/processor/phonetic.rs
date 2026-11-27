//! Japanese Phonetic / Mora Fuzzy Matching (#4) & Re-speak Habit Learning (#5).
//! Converts mixed Kanji/Kana/ASCII strings to normalized Hiragana/mora keys and tolerance-matches
//! slurred or slightly misheard commands, passphrases, and partial-replace targets.

use crate::config::DictionaryEntry;

pub fn katakana_to_hiragana(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            let code = c as u32;
            if (0x30A1..=0x30F6).contains(&code) {
                char::from_u32(code - 0x60).unwrap_or(c)
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// Normalizes text for mora comparison: Katakana->Hiragana, drops whitespace & punctuation,
/// collapses repeated vowel elongation marks.
pub fn to_normalized_mora(raw: &str) -> String {
    let mut expanded = raw.to_string();
    for (kanji, kana) in [
        ("取り消し", "とりけし"),
        ("取消", "とりけし"),
        ("送信", "そうしん"),
        ("削除", "さくじょ"),
        ("改行", "かいぎょう"),
        ("録音", "ろくおん"),
        ("停止", "ていし"),
        ("再開", "さいかい"),
        ("解除", "かいじょ"),
        ("返信", "へんしん"),
        ("宛先", "あてさき"),
        ("戻", "もど"),
        ("送っ", "おくっ"),
        ("消", "け"),
    ] {
        if expanded.contains(kanji) {
            expanded = expanded.replace(kanji, kana);
        }
    }
    let mut out = String::with_capacity(expanded.len());
    for ch in katakana_to_hiragana(&expanded).chars() {
        if ch.is_whitespace()
            || matches!(
                ch,
                '。' | '、' | '！' | '？' | '!' | '?' | '.' | ',' | '・' | '　' | '-' | '_'
            )
        {
            continue;
        }
        out.push(ch);
    }
    out
}

/// Maps voiced/semi-voiced pairs to a soft phonetic skeleton so slurred voicing errors
/// (e.g., ざ↔さ, だ↔た, ば↔ぱ↔は) and slurred small vowels (しちぇ↔して) remain matchable.
pub fn to_soft_phonetic_key(raw: &str) -> String {
    to_normalized_mora(raw)
        .chars()
        .filter(|&c| {
            !matches!(
                c,
                'ー' | 'っ' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ'
            )
        })
        .map(|c| match c {
            'が' => 'か', 'ぎ' => 'き', 'ぐ' => 'く', 'げ' => 'け', 'ご' => 'こ',
            'ざ' => 'さ', 'じ' => 'し', 'ず' => 'す', 'ぜ' => 'せ', 'ぞ' => 'そ',
            'だ' => 'た', 'ぢ' => 'ち', 'づ' => 'つ', 'で' => 'て', 'ど' => 'と',
            'ば' | 'ぱ' => 'は', 'び' | 'ぴ' => 'ひ', 'ぶ' | 'ぷ' => 'ふ',
            'べ' | 'ぺ' => 'へ', 'ぼ' | 'ぽ' => 'ほ',
            'ゃ' => 'や', 'ゅ' => 'ゆ', 'ょ' => 'よ',
            _ => c,
        })
        .collect()
}

pub fn levenshtein_chars(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    if av.is_empty() {
        return bv.len();
    }
    if bv.is_empty() {
        return av.len();
    }
    let mut prev: Vec<usize> = (0..=bv.len()).collect();
    let mut curr = vec![0usize; bv.len() + 1];
    for (i, &ca) in av.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in bv.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[bv.len()]
}

/// Computes normalized similarity (0.0 ..= 1.0) combining exact mora and soft phonetic keys.
pub fn phonetic_similarity(a: &str, b: &str) -> f32 {
    let ma = to_normalized_mora(a);
    let mb = to_normalized_mora(b);
    if ma.is_empty() || mb.is_empty() {
        return 0.0;
    }
    if ma == mb {
        return 1.0;
    }
    let max_len = ma.chars().count().max(mb.chars().count()).max(1);
    let strict_sim = 1.0 - (levenshtein_chars(&ma, &mb) as f32 / max_len as f32);

    let sa = to_soft_phonetic_key(&ma);
    let sb = to_soft_phonetic_key(&mb);
    let s_max = sa.chars().count().max(sb.chars().count()).max(1);
    let soft_sim = 1.0 - (levenshtein_chars(&sa, &sb) as f32 / s_max as f32);

    strict_sim.max(soft_sim * 0.96)
}

/// Fuzzily checks if a trailing segment of `input` matches any spoken command phrase (#4),
/// returning `(matched_phrase, remaining_prefix_text)`.
pub fn fuzzy_match_suffix<'a>(input: &'a str, phrases: &[String]) -> Option<(String, &'a str)> {
    let trimmed = input.trim_end();
    if trimmed.is_empty() || phrases.is_empty() {
        return None;
    }
    let char_indices: Vec<(usize, char)> = trimmed.char_indices().collect();
    let n_chars = char_indices.len();

    let mut best: Option<(f32, usize, String)> = None;
    for phrase in phrases {
        let p_norm = to_normalized_mora(phrase);
        let p_len = p_norm.chars().count();
        if p_len < 3 || n_chars + 1 < p_len {
            continue;
        }
        for take_len in p_len.saturating_sub(1)..=(p_len + 1).min(n_chars) {
            if take_len < 3 {
                continue;
            }
            let start_byte = char_indices[n_chars - take_len].0;
            let tail = &trimmed[start_byte..];
            let sim = phonetic_similarity(tail, phrase);
            let min_req = if p_len <= 3 { 0.85 } else { 0.78 };
            if sim >= min_req {
                let replace = best
                    .as_ref()
                    .map(|(bs, _, _)| sim > *bs)
                    .unwrap_or(true);
                if replace {
                    best = Some((sim, start_byte, phrase.clone()));
                }
            }
        }
    }

    best.map(|(_, byte_idx, matched)| {
        let rem = trimmed[..byte_idx].trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '、' | ',' | '。')
        });
        (matched, rem)
    })
}

/// Replaces the fuzzily closest segment inside `haystack` matching `target` with `replacement` (#14).
pub fn fuzzy_replace_segment(haystack: &str, target: &str, replacement: &str) -> Option<String> {
    if haystack.contains(target) {
        return Some(haystack.replacen(target, replacement, 1));
    }
    let t_chars = target.chars().count();
    if t_chars < 2 {
        return None;
    }
    let indices: Vec<(usize, char)> = haystack.char_indices().collect();
    let total = indices.len();
    let mut best: Option<(f32, usize, usize)> = None;

    for win in t_chars.saturating_sub(1).max(2)..=(t_chars + 2).min(total) {
        for start in 0..=total.saturating_sub(win) {
            let s_byte = indices[start].0;
            let e_byte = if start + win < indices.len() {
                indices[start + win].0
            } else {
                haystack.len()
            };
            let cand = &haystack[s_byte..e_byte];
            let sim = phonetic_similarity(cand, target);
            if sim >= 0.72 {
                let take = match &best {
                    None => true,
                    Some((bs, prev_s, prev_e)) => {
                        if (sim - *bs).abs() > 0.015 {
                            sim > *bs
                        } else {
                            let prev_len = haystack[*prev_s..*prev_e].chars().count();
                            win.abs_diff(t_chars) < prev_len.abs_diff(t_chars)
                                || (win.abs_diff(t_chars) == prev_len.abs_diff(t_chars) && win > prev_len)
                        }
                    }
                };
                if take {
                    best = Some((sim, s_byte, e_byte));
                }
            }
        }
    }
    best.map(|(_, s, e)| format!("{}{}{}", &haystack[..s], replacement, &haystack[e..]))
}

/// Extracts a recurring misrecognition -> correction pair when a cancelled utterance
/// is re-spoken immediately (within 10 seconds) with a similar overall sentence (#5).
pub fn extract_respeak_habit_pair(cancelled: &str, redone: &str) -> Option<DictionaryEntry> {
    let a: Vec<char> = cancelled.trim().chars().collect();
    let b: Vec<char> = redone.trim().chars().collect();
    if a.len() < 2 || b.len() < 2 || a == b {
        return None;
    }

    let mut pref = 0usize;
    while pref < a.len() && pref < b.len() && a[pref] == b[pref] {
        pref += 1;
    }
    let mut suf = 0usize;
    while suf + pref < a.len()
        && suf + pref < b.len()
        && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }

    let old_mid: String = a[pref..a.len() - suf].iter().collect();
    let new_mid: String = b[pref..b.len() - suf].iter().collect();
    let om = old_mid.trim();
    let nm = new_mid.trim();

    if om.is_empty()
        || nm.is_empty()
        || om == nm
        || om.chars().count() > 14
        || nm.chars().count() > 14
    {
        return None;
    }
    // Ensure either strong shared framing context (at least 2 chars) or phonetic similarity exists
    let has_context = pref >= 2 || suf >= 2 || (pref >= 1 && suf >= 1);
    if has_context || phonetic_similarity(om, nm) >= 0.45 {
        Some(DictionaryEntry {
            from: om.to_string(),
            to: nm.to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_slurred_japanese_commands_by_phonetics() {
        let cmds = vec!["送信して".into(), "キャンセル".into()];
        // Slurred voicing / mora drift: "ぞうしんしちぇ" ~ "そうしんして"
        let res = fuzzy_match_suffix("了解ぞうしんしちぇ", &cmds).expect("fuzzy match");
        assert_eq!(res.0, "送信して");
        assert_eq!(res.1, "了解");

        // Normal unrelated chat must NOT falsely trigger
        assert!(fuzzy_match_suffix("今日はカレー食べよう", &cmds).is_none());
    }

    #[test]
    fn fuzzy_partial_replaces_buffer_phrase() {
        let out = fuzzy_replace_segment("今からエコラウンズでいこう", "エコラウンド", "エコラウンド")
            .expect("partial phonetic replace");
        assert_eq!(out, "今からエコラウンドでいこう");
    }

    #[test]
    fn extracts_respeak_habit_from_cancelled_and_redone_pair() {
        let pair = extract_respeak_habit_pair("ばんだる買って", "ヴァンダル買って")
            .expect("respeak pair");
        assert_eq!(pair.from, "ばんだる");
        assert_eq!(pair.to, "ヴァンダル");

        assert!(extract_respeak_habit_pair("おはよう", "ぜんぜんちがうぶんしょう").is_none());
    }
}
