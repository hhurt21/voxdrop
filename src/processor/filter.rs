use crate::config::{EmojiAlias, SnippetMacro, TextFilterConfig};
use regex::Regex;

/// Cleans mild Japanese stuttering ("そ、それで" / "あ、あの") (#8).
pub fn cleanup_stutter(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Ok(re) = Regex::new(r"(?u)([ぁ-んァ-ヶ]{1,2})[、,]\s*([ぁ-んァ-ヶ]{2,})") {
        // Collapse repeated mora prefixes like "そ、それで" -> "それで"
        let out = re.replace_all(&s, |caps: &regex::Captures| {
            let head = &caps[1];
            let body = &caps[2];
            if body.starts_with(head) {
                body.to_string()
            } else {
                caps[0].to_string()
            }
        });
        s = out.into_owned();
    }
    s
}

/// Expands registered snippet macros ("合言葉定型" -> long template) (#10).
pub fn expand_snippet_macros(raw: &str, snippets: &[SnippetMacro]) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || snippets.is_empty() {
        return raw.to_string();
    }
    for sn in snippets {
        let trg = sn.trigger.trim();
        if trg.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case(trg)
            || crate::processor::phonetic::to_normalized_mora(trimmed)
                == crate::processor::phonetic::to_normalized_mora(trg)
        {
            return sn.expansion.clone();
        }
        if let Some(rest) = trimmed.strip_prefix(trg) {
            let rest_t = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '、' | ','));
            if rest_t.is_empty() {
                return sn.expansion.clone();
            }
            return format!("{}\n{}", sn.expansion, rest_t);
        }
    }
    raw.to_string()
}

/// Replaces spoken emoji/stamp triggers ("くさスタンプ" -> "🌿") (#12).
pub fn apply_emoji_aliases(raw: &str, aliases: &[EmojiAlias]) -> String {
    let mut out = raw.to_string();
    for al in aliases {
        let s = al.spoken.trim();
        if !s.is_empty() && !al.emoji.trim().is_empty() {
            out = out.replace(s, al.emoji.trim());
        }
    }
    out
}

/// Applies voice-triggered Discord markdown & spoiler formatting (#24):
/// "ネタバレ 〇〇" -> "||〇〇||", "強調 〇〇" -> "**〇〇**", "コード 〇〇" -> "`〇〇`", "引用 〇〇" -> "> 〇〇".
pub fn apply_discord_markdown_commands(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let rules: &[(&str, &str, &str)] = &[
        ("ネタバレ", "||", "||"),
        ("スポイラー", "||", "||"),
        ("強調", "**", "**"),
        ("太字", "**", "**"),
        ("コード", "`", "`"),
        ("引用", "> ", ""),
    ];
    for &(prefix, wrap_l, wrap_r) in rules {
        if let Some(rest) = t.strip_prefix(prefix) {
            let body = rest
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '、' | ',' | '：' | ':'))
                .trim();
            if !body.is_empty() {
                return format!("{}{}{}", wrap_l, body, wrap_r);
            }
        }
    }
    raw.to_string()
}

/// Removes configured fillers and applies custom dictionary replacements.
/// Punctuation rules are applied separately after command parsing so period insertion
/// never interferes with suffix command detection.
pub fn apply_fillers_and_dictionary(raw: &str, cfg: &TextFilterConfig) -> String {
    let mut text = raw.trim().to_string();
    if text.is_empty() {
        return text;
    }

    if cfg.remove_fillers && !cfg.fillers.is_empty() {
        let mut fillers: Vec<&str> = cfg
            .fillers
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        // Longest first so "えーと" matches before "えー"
        fillers.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));

        let pattern = fillers
            .iter()
            .map(|f| regex::escape(f))
            .collect::<Vec<_>>()
            .join("|");
        if !pattern.is_empty() {
            // Strip fillers plus any immediately trailing Japanese comma/whitespace
            let full = format!("(?:{})[、,\\s]*", pattern);
            if let Ok(re) = Regex::new(&full) {
                text = re.replace_all(&text, "").to_string();
            }
        }
    }

    if !cfg.dictionary.is_empty() {
        let mut entries: Vec<_> = cfg
            .dictionary
            .iter()
            .filter(|e| !e.from.trim().is_empty())
            .collect();
        entries.sort_by(|a, b| b.from.chars().count().cmp(&a.from.chars().count()));
        for entry in entries {
            text = text.replace(entry.from.trim(), &entry.to);
        }
    }

    // Collapse repeated spaces/tabs while preserving intentional newlines
    if let Ok(ws_re) = Regex::new(r"[ \t\u{3000}]{2,}") {
        text = ws_re.replace_all(&text, " ").to_string();
    }

    text.trim().to_string()
}

/// Applies punctuation removal, automatic Japanese punctuation restoration,
/// automatic newline insertion, or automatic period insertion after command stripping.
pub fn apply_punctuation_rules(input: &str, cfg: &TextFilterConfig) -> String {
    let mut text = input.trim().to_string();
    if text.is_empty() {
        return text;
    }

    if cfg.strip_punctuation {
        if cfg.auto_newline {
            text = crate::processor::japanese_fix::restore_japanese_punctuation(
                &text,
                false,
                true,
            );
        }
        text = text.replace(['。', '、'], "");
        text = text.trim().to_string();
        return text;
    }

    if cfg.auto_punctuation || cfg.auto_newline {
        text = crate::processor::japanese_fix::restore_japanese_punctuation(
            &text,
            cfg.auto_punctuation,
            cfg.auto_newline,
        );
    }

    if cfg.append_period && !text.is_empty() {
        let ends_with_punct = text.ends_with(['。', '！', '？', '!', '?', '.', ':']);
        if !ends_with_punct {
            if crate::processor::japanese_fix::is_question_ending(&text) {
                text.push('？');
            } else {
                text.push('。');
            }
        }
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DictionaryEntry;

    #[test]
    fn removes_fillers_and_applies_dictionary() {
        let cfg = TextFilterConfig {
            dictionary: vec![
                DictionaryEntry {
                    from: "くさ".into(),
                    to: "草".into(),
                },
                DictionaryEntry {
                    from: "ナイス".into(),
                    to: ":nice:".into(),
                },
            ],
            fillers: vec!["えーと".into(), "あのー".into(), "えー".into()],
            remove_fillers: true,
            auto_space: false,
            strip_punctuation: false,
            append_period: false,
            auto_punctuation: true,
            auto_newline: true,
        };

        let out = apply_fillers_and_dictionary("えーと、あのーくさナイス", &cfg);
        assert_eq!(out, "草:nice:");
    }

    #[test]
    fn punctuation_strip_and_append_rules() {
        let mut cfg = TextFilterConfig::default();
        cfg.strip_punctuation = true;
        assert_eq!(apply_punctuation_rules("こんにちは、世界。", &cfg), "こんにちは世界");

        cfg.strip_punctuation = false;
        cfg.append_period = true;
        assert_eq!(apply_punctuation_rules("了解", &cfg), "了解。");
        assert_eq!(apply_punctuation_rules(":nice:", &cfg), ":nice:");
    }

    #[test]
    fn expands_stutter_snippets_emojis_and_markdown() {
        assert_eq!(cleanup_stutter("そ、それでさ"), "それでさ");
        let sn = vec![SnippetMacro {
            id: "s1".into(),
            trigger: "集合合図".into(),
            expansion: "@everyone 21時から集合お願いします！".into(),
        }];
        assert_eq!(
            expand_snippet_macros("集合合図", &sn),
            "@everyone 21時から集合お願いします！"
        );
        let em = vec![EmojiAlias {
            spoken: "いいね絵文字".into(),
            emoji: "👍".into(),
        }];
        assert_eq!(apply_emoji_aliases("ナイスいいね絵文字", &em), "ナイス👍");
        assert_eq!(apply_discord_markdown_commands("ネタバレ 黒幕は犯人"), "||黒幕は犯人||");
        assert_eq!(apply_discord_markdown_commands("強調 重要"), "**重要**");
        assert_eq!(apply_discord_markdown_commands("コード cargo test"), "`cargo test`");
        assert_eq!(apply_discord_markdown_commands("引用 了解"), "> 了解");
    }
}
