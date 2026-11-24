//! Lightweight, deterministic Japanese naturalness & ASR post-correction filter (<0.1ms).
//! Fixes common Whisper/STT Japanese artifacts without corrupting valid game/chat vocabulary:
//! - Strips stray single-letter ASCII noise prefix before Japanese (e.g. "z聞こえますか？" -> "聞こえますか？")
//! - Repairs ungrammatical Japanese verb/phrase ASR slips (e.g. "聞こいました" -> "聞こえました", "聞こいだら" -> "聞こえたら")
//! - Normalizes half-width punctuation & collapses duplicate punctuation / stray spaces between Japanese chars.

/// Repairs unnatural Japanese ASR artifacts while keeping natural chat phrasing intact.
pub fn normalize_japanese_transcript(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return s;
    }

    // 1. Strip stray leading single ASCII letter noise directly attached to Japanese (e.g. "z聞こえますか")
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && is_japanese_char(chars[1]) {
        // Keep common intentional single-letter prefixes only if uppercase like "Aさん" / "Xで"
        if chars[0].is_ascii_lowercase() {
            s = chars[1..].iter().collect();
        }
    }

    // 2. Collapse spurious spaces between Japanese characters
    s = crate::stt::local::collapse_japanese_spaces(&s);

    // 3. Repair ungrammatical Japanese ASR phonetic slips (only forms that are never valid Japanese)
    const GRAMMAR_FIXES: &[(&str, &str)] = &[
        ("聞こいました", "聞こえました"),
        ("声ますか", "聞こえますか"),
        ("声マスか", "聞こえますか"),
        ("声待つか", "聞こえますか"),
        ("ごめんますか", "聞こえますか"),
        ("聞こいだら", "聞こえたら"),
        ("きこいだら", "聞こえたら"),
        ("おしいて", "教えて"),
        ("持っていきない", "持っていけない"),
        ("おつかれさまですた", "お疲れ様でした"),
        ("ありがとうございましだ", "ありがとうございました"),
        ("了解しましだ", "了解しました"),
        ("送信してくだい", "送信してください"),
        ("送ってくだい", "送ってください"),
    ];
    for &(bad, good) in GRAMMAR_FIXES {
        if s.contains(bad) {
            s = s.replace(bad, good);
        }
    }

    // 4. Normalize punctuation when the utterance contains Japanese characters
    if s.chars().any(is_japanese_char) {
        s = s.replace('?', "？").replace('!', "！");
        while s.contains("。。") {
            s = s.replace("。。", "。");
        }
        while s.contains("、、") {
            s = s.replace("、、", "、");
        }
        s = s.replace("、。", "。").replace("？。", "？").replace("！。", "！");
        s = s
            .trim_start_matches(['、', '。', ',', '.'])
            .trim()
            .to_string();
    }

    s
}

fn is_japanese_char(c: char) -> bool {
    let u = c as u32;
    (0x3040..=0x30FF).contains(&u) || (0x4E00..=0x9FFF).contains(&u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_ungrammatical_japanese_asr_artifacts() {
        assert_eq!(normalize_japanese_transcript("z聞こえますか?"), "聞こえますか？");
        assert_eq!(
            normalize_japanese_transcript("聞こいました。声ますか？"),
            "聞こえました。聞こえますか？"
        );
        assert_eq!(
            normalize_japanese_transcript("聞こいだらおしいて。。"),
            "聞こえたら教えて。"
        );
        // Valid Japanese/game vocabulary must remain untouched
        assert_eq!(
            normalize_japanese_transcript("宝箱のチェストを開ける"),
            "宝箱のチェストを開ける"
        );
    }
}
