use crate::config::TextFilterConfig;
use regex::Regex;

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

/// Applies punctuation removal or automatic period insertion after command stripping.
pub fn apply_punctuation_rules(input: &str, cfg: &TextFilterConfig) -> String {
    let mut text = input.trim().to_string();
    if text.is_empty() {
        return text;
    }

    if cfg.strip_punctuation {
        text = text.replace(['。', '、'], "");
        text = text.trim().to_string();
    } else if cfg.append_period {
        let ends_with_punct = text.ends_with(['。', '！', '？', '!', '?', '.', ':']);
        if !ends_with_punct {
            text.push('。');
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
}
