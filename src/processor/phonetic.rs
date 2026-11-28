//! Japanese Phonetic / Mora Fuzzy Matching (#4), Morphological Full-Reading Expansion (#1),
//! IME User Dictionary Import (#5), Keyword Carryover Extraction (#10), and Re-speak Habit Learning (#5).

use crate::config::DictionaryEntry;
use crate::processor::ngram::PersonalNgramModel;
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct MorphemeToken {
    pub surface: String,
    pub reading_hira: String,
    pub pos: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImeParsedEntry {
    pub reading: String,
    pub word: String,
    pub pos: String,
    pub is_duplicate: bool,
}

static LINDERA_SEGMENTER: OnceLock<Option<Segmenter>> = OnceLock::new();

fn get_segmenter() -> Option<&'static Segmenter> {
    LINDERA_SEGMENTER
        .get_or_init(|| {
            load_dictionary("embedded://ipadic")
                .ok()
                .map(|dict| Segmenter::new(Mode::Normal, dict, None))
        })
        .as_ref()
}

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

fn strip_punctuation_and_ws(raw: &str) -> String {
    raw.chars()
        .filter(|&ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '。' | '、' | '！' | '？' | '!' | '?' | '.' | ',' | '・' | '　' | '-' | '_' | '「' | '」'
                )
        })
        .collect()
}

/// Fallback reading expansions only for common game/UI Kanji compounds when a morphological
/// dictionary treats an uncommon character sequence as `UNK`.
fn fallback_kanji_reading(surface: &str) -> String {
    let mut expanded = surface.to_string();
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
        ("輝晶獣", "きしょうじゅう"),
        ("気象獣", "きしょうじゅう"),
        ("輝", "き"),
        ("晶", "しょう"),
        ("戻", "もど"),
        ("送っ", "おくっ"),
        ("消", "け"),
    ] {
        if expanded.contains(kanji) {
            expanded = expanded.replace(kanji, kana);
        }
    }
    katakana_to_hiragana(&expanded)
}

/// Segments Japanese text using Lindera IPADIC morphological analysis and extracts
/// each token's normalized hiragana reading and part-of-speech tag (#1).
pub fn segment_morphemes_with_readings(text: &str) -> Vec<MorphemeToken> {
    if text.is_empty() {
        return Vec::new();
    }
    if let Some(seg) = get_segmenter() {
        if let Ok(mut tokens) = seg.segment(Cow::Borrowed(text)) {
            let mut out = Vec::with_capacity(tokens.len());
            for tok in tokens.iter_mut() {
                let surface = tok.surface.to_string();
                let b_start = tok.byte_start;
                let b_end = tok.byte_end;
                let details = tok.details();
                let pos = details.first().copied().unwrap_or("UNK").to_string();
                let reading_raw = details.get(7).copied().unwrap_or("*");
                let reading_hira = if reading_raw != "*" && !reading_raw.is_empty() {
                    katakana_to_hiragana(reading_raw)
                } else {
                    fallback_kanji_reading(&surface)
                };
                out.push(MorphemeToken {
                    surface,
                    reading_hira,
                    pos,
                    byte_start: b_start,
                    byte_end: b_end,
                });
            }
            // In multi-kanji compounds (e.g. coined terms like "輝晶獣"), IPADIC often tags single
            // kanji as standalone names (輝->てる, 晶->あきら); apply on'yomi fallback when adjacent to kanji.
            let is_kanji = |c: char| ('\u{4E00}'..='\u{9FFF}').contains(&c);
            let adj_kanji: Vec<bool> = (0..out.len())
                .map(|idx| {
                    let prev_k = idx > 0
                        && out[idx - 1]
                            .surface
                            .chars()
                            .last()
                            .map(is_kanji)
                            .unwrap_or(false);
                    let next_k = idx + 1 < out.len()
                        && out[idx + 1]
                            .surface
                            .chars()
                            .next()
                            .map(is_kanji)
                            .unwrap_or(false);
                    prev_k || next_k
                })
                .collect();
            for (idx, tok) in out.iter_mut().enumerate() {
                if adj_kanji[idx]
                    && tok.surface.chars().count() == 1
                    && tok.surface.chars().next().map(is_kanji).unwrap_or(false)
                {
                    let fb = fallback_kanji_reading(&tok.surface);
                    if fb != tok.surface {
                        tok.reading_hira = fb;
                    }
                }
            }
            return out;
        }
    }
    vec![MorphemeToken {
        surface: text.to_string(),
        reading_hira: fallback_kanji_reading(text),
        pos: "UNK".into(),
        byte_start: 0,
        byte_end: text.len(),
    }]
}

/// Converts any mixed Japanese (Kanji/Kana/Alnum) string into its full hiragana reading
/// via morphological analysis (`lindera` IPADIC) (#1).
pub fn to_full_hiragana_reading(text: &str) -> String {
    let mut out = String::new();
    for tok in segment_morphemes_with_readings(text) {
        let cleaned = strip_punctuation_and_ws(&tok.reading_hira);
        out.push_str(&cleaned);
    }
    out
}

/// Normalizes text for mora comparison: full morphological Kanji->Hiragana conversion,
/// drops whitespace & punctuation (#1).
pub fn to_normalized_mora(raw: &str) -> String {
    strip_punctuation_and_ws(&to_full_hiragana_reading(raw))
}

/// Maps voiced/semi-voiced pairs to a soft phonetic skeleton so slurred voicing errors
/// (e.g., ざ↔さ, だ↔た, ば↔ぱ↔は) and slurred small vowels remain matchable.
pub fn to_soft_phonetic_key(raw: &str) -> String {
    to_normalized_mora(raw)
        .chars()
        .filter(|&c| !matches!(c, 'ー' | 'っ' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ'))
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

#[derive(Clone)]
struct PhoneticDictTarget {
    to: String,
    reading: String,
    soft_key: String,
    mora_len: usize,
}

fn spans_overlap(a: (usize, usize), b: &[(usize, usize)]) -> bool {
    b.iter().any(|&(s, e)| a.0 < e && s < a.1)
}

/// Non-LLM automatic phonetic dictionary correction (#1 + #2 + #4):
/// Segments `text` via Lindera and slides 1..=4 morphemes to replace homophones or near-homophone
/// misrecognitions with registered dictionary terms (>= 3 moras only to prevent short-word misfires).
pub fn apply_phonetic_dictionary_correction(
    text: &str,
    dictionary: &[DictionaryEntry],
    low_conf_spans: &[(usize, usize)],
    ngram: Option<&PersonalNgramModel>,
) -> String {
    if text.trim().is_empty() || dictionary.is_empty() {
        return text.to_string();
    }

    let mut targets: Vec<PhoneticDictTarget> = Vec::new();
    let mut seen_pair = HashSet::new();

    for entry in dictionary {
        let to = entry.to.trim();
        if to.is_empty() || (to.starts_with(':') && to.ends_with(':')) {
            continue;
        }
        // Derive readings from both the canonical `to` word and optional `from` reading hint
        let mut seeds = vec![to_normalized_mora(to)];
        let from_t = entry.from.trim();
        if !from_t.is_empty() && from_t != to {
            let from_m = to_normalized_mora(from_t);
            if !from_m.is_empty() && !seeds.contains(&from_m) {
                seeds.push(from_m);
            }
        }
        for r in seeds {
            let m_len = r.chars().count();
            // Safety guard (#1): never auto-replace 1-2 mora words phonetically
            if m_len < 3 {
                continue;
            }
            if seen_pair.insert((to.to_string(), r.clone())) {
                let soft_key = to_soft_phonetic_key(&r);
                targets.push(PhoneticDictTarget {
                    to: to.to_string(),
                    reading: r,
                    soft_key,
                    mora_len: m_len,
                });
            }
        }
    }
    if targets.is_empty() {
        return text.to_string();
    }

    let morphemes = segment_morphemes_with_readings(text);
    if morphemes.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor_byte = 0usize;
    let mut i = 0usize;

    while i < morphemes.len() {
        if morphemes[i].byte_start > cursor_byte {
            out.push_str(&text[cursor_byte..morphemes[i].byte_start]);
        }

        let mut best_match: Option<(usize, &str, f32)> = None;
        let max_w = 4.min(morphemes.len() - i);

        for w in (1..=max_w).rev() {
            // Skip spans containing purely punctuation or multi-morpheme spans anchored by particles
            let span_tokens = &morphemes[i..i + w];
            if w > 1
                && (matches!(span_tokens[0].pos.as_str(), "助詞" | "助動詞" | "記号" | "接続詞")
                    || matches!(span_tokens[w - 1].pos.as_str(), "記号"))
            {
                continue;
            }
            let span_start = span_tokens[0].byte_start;
            let span_end = span_tokens[w - 1].byte_end;
            let orig_span = &text[span_start..span_end];
            if orig_span.trim().is_empty()
                || orig_span.chars().all(|c| {
                    matches!(c, '。' | '、' | '！' | '？' | '!' | '?' | '\n' | ' ')
                })
            {
                continue;
            }

            let span_reading: String = span_tokens
                .iter()
                .map(|m| strip_punctuation_and_ws(&m.reading_hira))
                .collect();
            let span_len = span_reading.chars().count();
            if span_len < 3 {
                continue;
            }
            let span_soft = to_soft_phonetic_key(&span_reading);
            let is_low_conf = spans_overlap((span_start, span_end), low_conf_spans);
            let ends_with_particle =
                w > 1 && matches!(span_tokens[w - 1].pos.as_str(), "助詞" | "助動詞");

            let prev_surf = if i > 0 {
                Some(morphemes[i - 1].surface.as_str())
            } else {
                None
            };
            let next_surf = morphemes.get(i + w).map(|m| m.surface.as_str());

            for tgt in &targets {
                if span_len.abs_diff(tgt.mora_len) > 1 {
                    continue;
                }
                let exact_match = span_reading == tgt.reading;
                let soft_match = !tgt.soft_key.is_empty() && span_soft == tgt.soft_key;
                let dist_match = !ends_with_particle
                    && (is_low_conf && tgt.mora_len >= 4 || tgt.mora_len >= 5)
                    && levenshtein_chars(&span_reading, &tgt.reading) <= 1;

                if !(exact_match || soft_match || dist_match) {
                    continue;
                }

                // Prefer exact target surface immediately unless exact match doesn't need replacement
                let mut score = if orig_span == tgt.to {
                    12.0
                } else if exact_match {
                    10.0
                } else if soft_match {
                    8.5
                } else {
                    7.0
                };
                score += (w as f32) * 0.25;
                if let Some(lm) = ngram {
                    score += lm.score_phrase(prev_surf, &tgt.to, next_surf) * 0.8;
                }

                let take = best_match
                    .as_ref()
                    .map(|(bw, _, bs)| {
                        if (score - *bs).abs() > 0.05 {
                            score > *bs
                        } else {
                            w > *bw
                        }
                    })
                    .unwrap_or(true);
                if take {
                    best_match = Some((w, tgt.to.as_str(), score));
                }
            }
        }

        if let Some((w, replacement, _)) = best_match {
            let span_end = morphemes[i + w - 1].byte_end;
            out.push_str(replacement);
            cursor_byte = span_end;
            i += w;
        } else {
            out.push_str(&text[morphemes[i].byte_start..morphemes[i].byte_end]);
            cursor_byte = morphemes[i].byte_end;
            i += 1;
        }
    }

    if cursor_byte < text.len() {
        out.push_str(&text[cursor_byte..]);
    }
    out
}

/// Extracts key content words (proper nouns, nouns, katakana >= 2 chars) for short-term context carryover (#10).
pub fn extract_keywords_for_prompt(text: &str, max_terms: usize) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    for tok in segment_morphemes_with_readings(text) {
        let s = tok.surface.trim();
        let len = s.chars().count();
        if len < 2 || len > 16 {
            continue;
        }
        let is_katakana = s
            .chars()
            .all(|c| ('\u{30A0}'..='\u{30FF}').contains(&c) || c == 'ー');
        let is_noun = tok.pos == "名詞" || tok.pos == "UNK";
        let is_filler_noun = matches!(
            s,
            "こと" | "もの" | "それ" | "これ" | "あれ" | "どれ" | "ここ" | "そこ" | "どこ" | "さん"
                | "ちゃん" | "くん" | "やつ" | "感じ" | "ため" | "よう"
        );
        if (is_noun || is_katakana) && !is_filler_noun {
            if seen.insert(s.to_string()) {
                terms.push(s.to_string());
                if terms.len() >= max_terms {
                    break;
                }
            }
        }
    }
    terms
}

/// Standard RFC-4648 base64 decoder for uploaded IME dictionary bytes (#5).
pub fn decode_base64_bytes(b64: &str) -> Result<Vec<u8>, String> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = b64.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.len() % 4 != 0 {
        return Err("Base64 入力長が不正です".into());
    }
    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    for chunk in clean.chunks_exact(4) {
        let a = val(chunk[0]).ok_or_else(|| "Base64 文字が不正です".to_string())?;
        let b = val(chunk[1]).ok_or_else(|| "Base64 文字が不正です".to_string())?;
        let c = if chunk[2] == b'=' {
            None
        } else {
            Some(val(chunk[2]).ok_or_else(|| "Base64 文字が不正です".to_string())?)
        };
        let d = if chunk[3] == b'=' {
            None
        } else {
            Some(val(chunk[3]).ok_or_else(|| "Base64 文字が不正です".to_string())?)
        };
        out.push((a << 2) | (b >> 4));
        if let Some(cv) = c {
            out.push((b << 4) | (cv >> 2));
            if let Some(dv) = d {
                out.push((cv << 6) | dv);
            }
        }
    }
    Ok(out)
}

/// Decodes MS-IME (UTF-16LE / Shift-JIS / UTF-8) or Google Japanese Input (UTF-8) text bytes (#5).
pub fn decode_ime_text_bytes(bytes: &[u8]) -> Result<(String, &'static str), String> {
    if bytes.is_empty() {
        return Err("辞書ファイルが空です".into());
    }
    // Check UTF-16LE / UTF-16BE / UTF-8 BOM
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, had_err) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        if !had_err {
            return Ok((cow.into_owned(), "UTF-16LE"));
        }
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, had_err) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        if !had_err {
            return Ok((cow.into_owned(), "UTF-16BE"));
        }
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        if let Ok(s) = std::str::from_utf8(rest) {
            return Ok((s.to_string(), "UTF-8 (BOM)"));
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok((s.to_string(), "UTF-8"));
    }
    let (cow, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
    Ok((cow.into_owned(), "Shift-JIS"))
}

/// Parses MS-IME / Google IME tab-separated dictionary text (`reading \t word \t pos [\t comment]`) (#5).
pub fn parse_ime_dictionary_text(
    text: &str,
    existing: &[DictionaryEntry],
) -> (Vec<ImeParsedEntry>, usize) {
    let mut existing_set: HashSet<(String, String)> = HashSet::new();
    for d in existing {
        existing_set.insert((d.from.trim().to_string(), d.to.trim().to_string()));
    }

    let mut entries = Vec::new();
    let mut dup_count = 0usize;
    let mut seen_in_file = HashSet::new();

    for line in text.lines() {
        let trimmed = line.trim_matches(|c| c == '\r' || c == '\u{FEFF}').trim();
        if trimmed.is_empty() || trimmed.starts_with('!') || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let reading = katakana_to_hiragana(cols[0].trim());
        let word = cols[1].trim().to_string();
        let pos = cols.get(2).map(|s| s.trim()).unwrap_or("名詞").to_string();
        if reading.is_empty() || word.is_empty() || reading == word {
            continue;
        }
        if !seen_in_file.insert((reading.clone(), word.clone())) {
            continue;
        }
        let is_duplicate = existing_set.contains(&(reading.clone(), word.clone()));
        if is_duplicate {
            dup_count += 1;
        }
        entries.push(ImeParsedEntry {
            reading,
            word,
            pos,
            is_duplicate,
        });
    }

    (entries, dup_count)
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
                                || (win.abs_diff(t_chars) == prev_len.abs_diff(t_chars)
                                    && win > prev_len)
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
    fn full_hiragana_reading_matches_homophones_and_replaces_safely() {
        assert_eq!(to_full_hiragana_reading("東京特許許可局"), "とうきょうとっきょきょかきょく");
        assert_eq!(
            to_full_hiragana_reading("気象獣"),
            to_full_hiragana_reading("輝晶獣")
        );

        let dict = vec![
            DictionaryEntry {
                from: "".into(),
                to: "輝晶獣".into(),
            },
            DictionaryEntry {
                from: "".into(),
                to: "気".into(), // <= 2 moras: MUST NOT auto-replace!
            },
        ];
        let out = apply_phonetic_dictionary_correction("今日の気象獣いく？木をつけて", &dict, &[], None);
        assert_eq!(out, "今日の輝晶獣いく？木をつけて");
    }

    #[test]
    fn decodes_and_parses_ms_ime_dictionaries_utf8_utf16le_and_sjis() {
        let sample = "!Microsoft IME Dictionary Tool\r\nきしょうじゅう\t輝晶獣\t名詞\tDQ10\r\nばんだる\tヴァンダル\t名詞\r\n";

        // UTF-8
        let (txt8, enc8) = decode_ime_text_bytes(sample.as_bytes()).unwrap();
        assert_eq!(enc8, "UTF-8");
        let (rows8, dup8) = parse_ime_dictionary_text(&txt8, &[]);
        assert_eq!(dup8, 0);
        assert_eq!(rows8.len(), 2);
        assert_eq!(rows8[0].reading, "きしょうじゅう");
        assert_eq!(rows8[0].word, "輝晶獣");

        // UTF-16LE with BOM
        let mut utf16 = vec![0xFFu8, 0xFEu8];
        for u in sample.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let (txt16, enc16) = decode_ime_text_bytes(&utf16).unwrap();
        assert_eq!(enc16, "UTF-16LE");
        let (rows16, _) = parse_ime_dictionary_text(&txt16, &[]);
        assert_eq!(rows16.len(), 2);

        // Shift-JIS
        let (sjis_bytes, _, _) = encoding_rs::SHIFT_JIS.encode(sample);
        let (txtsj, encsj) = decode_ime_text_bytes(&sjis_bytes).unwrap();
        assert_eq!(encsj, "Shift-JIS");
        let (rowssj, _) = parse_ime_dictionary_text(&txtsj, &[DictionaryEntry { from: "ばんだる".into(), to: "ヴァンダル".into() }]);
        assert_eq!(rowssj.len(), 2);
        assert!(rowssj[1].is_duplicate);
    }

    #[test]
    fn fuzzy_matches_slurred_japanese_commands_by_phonetics() {
        let cmds = vec!["送信して".into(), "キャンセル".into()];
        let res = fuzzy_match_suffix("了解ぞうしんしちぇ", &cmds).expect("fuzzy match");
        assert_eq!(res.0, "送信して");
        assert_eq!(res.1, "了解");

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
