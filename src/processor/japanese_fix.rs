//! Lightweight, deterministic Japanese naturalness, punctuation & ASR post-correction filter (<0.1ms).
//! Fixes common Whisper/STT Japanese artifacts without corrupting valid game/chat vocabulary:
//! - Strips stray single-letter ASCII noise prefix before Japanese (e.g. "z聞こえますか？" -> "聞こえますか？")
//! - Repairs ungrammatical Japanese verb/phrase ASR slips (e.g. "聞こいました" -> "聞こえました", "くとうてん" -> "句読点")
//! - Restores natural Japanese punctuation (「、」「。」) at clause/sentence boundaries
//! - Inserts automatic line breaks (\n) at clear sentence/topic boundaries when enabled
//! - Joins consecutive utterances naturally in Draft & Direct modes.

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
        ("おつかれしゃまでした", "お疲れ様でした"),
        ("ありがとうございましだ", "ありがとうございました"),
        ("ありがとうございまちた", "ありがとうございました"),
        ("了解しましだ", "了解しました"),
        ("送信してくだい", "送信してください"),
        ("送ってくだい", "送ってください"),
        ("くとうてん", "句読点"),
        ("カタカラ", "カタカナ"),
        // Unambiguous slurred sibilant ('さしすせそ') ASR slips
        ("しゃしすせそ", "さしすせそ"),
        ("しゃししゅしぇしょ", "さしすせそ"),
        ("おつかれしゃま", "お疲れ様"),
        ("ございましゅ", "ございます"),
        ("おねがいしましゅ", "お願いします"),
        ("お願いしましゅ", "お願いします"),
        ("わかりましゅ", "わかります"),
        ("わかりまちた", "わかりました"),
        ("分かりまちた", "分かりました"),
        ("そうでしゅか", "そうですか"),
        ("そうでしゅね", "そうですね"),
        ("そうでしゅ", "そうです"),
        ("しまちた", "しました"),
        ("でちた", "でした"),
        ("送信ちて", "送信して"),
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

pub fn is_japanese_char(c: char) -> bool {
    let u = c as u32;
    (0x3040..=0x30FF).contains(&u) || (0x4E00..=0x9FFF).contains(&u)
}

fn is_punct_or_newline(c: char) -> bool {
    matches!(
        c,
        '。' | '、' | '！' | '？' | '!' | '?' | '.' | ',' | '…' | '\n' | '」' | '』' | '）' | ')'
    )
}

/// Returns true when an unpunctuated Japanese string clearly ends as a question.
pub fn is_question_ending(text: &str) -> bool {
    let t = text.trim();
    const Q_ENDINGS: &[&str] = &[
        "でしょうか",
        "ませんか",
        "ですか",
        "ますか",
        "なのかな",
        "るのかな",
        "でしょうかね",
        "なのかな",
        "だろうか",
        "いいのか",
        "できるの",
        "あるの",
        "いるの",
        "ないの",
        "るの",
        "たの",
    ];
    if Q_ENDINGS.iter().any(|&e| t.ends_with(e)) {
        return true;
    }
    false
}

/// Returns true when `text` clearly ends mid-sentence (continuative / connective clause).
pub fn is_continuative_ending(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if ends_like_complete_sentence(t) {
        return false;
    }
    const CONT_SUFFIXES: &[&str] = &[
        "けれども",
        "けれど",
        "だけど",
        "んだけど",
        "けど",
        "のに",
        "ので",
        "ないから",
        "るから",
        "たから",
        "だから",
        "てから",
        "ますから",
        "ですから",
        "ながら",
        "たり",
        "つつ",
        "ときは",
        "とき",
        "たら",
        "なら",
        "れば",
        "というか",
        "ていうか",
        "ますが",
        "ですが",
        "ましたが",
        "でしたが",
    ];
    if CONT_SUFFIXES.iter().any(|&s| t.ends_with(s)) {
        return true;
    }
    // Plain te-form / de-form / quotative tte-form
    t.ends_with(['て', 'で'])
}

/// Returns true when `text` clearly ends a complete Japanese sentence (even without trailing '。').
pub fn ends_like_complete_sentence(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with(['。', '！', '？', '!', '?', '…']) {
        return true;
    }
    const STRONG_ENDINGS: &[&str] = &[
        "ありがとうございました",
        "お疲れ様でした",
        "よろしくお願いします",
        "お願いします",
        "ください",
        "でしょうか",
        "ませんか",
        "ましょう",
        "ました",
        "でした",
        "ません",
        "ですか",
        "ますか",
        "ですよ",
        "ますよ",
        "ですね",
        "ますね",
        "です",
        "ます",
    ];
    if STRONG_ENDINGS.iter().any(|&e| t.ends_with(e)) {
        return true;
    }
    let len = t.chars().count();
    if len >= 4 {
        const CASUAL_ENDINGS: &[&str] = &[
            "ちゃうんだよね",
            "んですよね",
            "んだよね",
            "だよね",
            "ですよね",
            "なんだよ",
            "なんだね",
            "んだよ",
            "んだね",
            "だよな",
            "だよ",
            "だね",
            "だわ",
            "じゃん",
            "てるよ",
            "てるね",
            "てたよ",
            "てたね",
            "ちゃうよ",
            "ちゃうね",
            "ないよ",
            "ないね",
            "るよ",
            "るね",
            "たよ",
            "たね",
            "わよ",
            "わね",
            "かな",
            "かも",
            "よさそう",
            "すごそう",
            "わかった",
            "分かった",
            "了解",
            "いいね",
            "いいよ",
            "すご",
            "すごい",
            "なかった",
            "かった",
            "だった",
        ];
        if CASUAL_ENDINGS.iter().any(|&e| t.ends_with(e)) {
            return true;
        }
        if is_question_ending(t) {
            return true;
        }
    }
    false
}

/// Returns true when `text` starts with a clear sentence transition / new topic.
pub fn starts_like_new_topic_or_sentence(text: &str) -> bool {
    let t = text.trim();
    const STARTERS: &[&str] = &[
        "それはそうと",
        "話変わるけど",
        "ちなみに",
        "ところで",
        "とりあえず",
        "それから",
        "それじゃあ",
        "それと",
        "それで",
        "だから",
        "なので",
        "だけど",
        "でも",
        "ただ",
        "あと",
        "しかも",
        "それに",
        "というか",
        "ていうか",
        "要するに",
        "つまり",
        "たとえば",
        "例えば",
        "実は",
        "正直",
        "まず",
        "次に",
        "最後に",
        "じゃあ",
        "では",
        "もし",
        "やっぱり",
        "やっぱ",
    ];
    STARTERS.iter().any(|&s| t.starts_with(s))
}

fn starts_with_strong_topic_switch(text: &str) -> bool {
    let t = text.trim();
    const STRONG: &[&str] = &[
        "それはそうと",
        "話変わるけど",
        "ちなみに",
        "ところで",
        "それで",
        "それから",
        "まず",
        "次に",
        "最後に",
        "要するに",
    ];
    STRONG.iter().any(|&s| t.starts_with(s))
}

fn is_adnominal_prefix(text: &str) -> bool {
    let t = text.trim();
    t.ends_with(['そ', 'こ', 'あ', 'ど']) // fallback guard unused, check exact words below
        && false
        || ["その", "この", "あの", "どの", "ある", "いる", "もう", "まだ", "ちょっと", "なんか"]
            .iter()
            .any(|&w| t.ends_with(w))
}

fn starts_with_laugh_or_closing(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    let first = t.chars().next().unwrap();
    if matches!(first, '」' | '』' | '）' | ')' | '。' | '、' | '！' | '？') {
        return true;
    }
    // Short trailing laugh like "w", "ｗ", "笑"
    if t.chars().count() <= 3
        && t.chars()
            .all(|c| matches!(c, 'w' | 'ｗ' | 'W' | 'Ｗ' | '笑' | '草'))
    {
        return true;
    }
    false
}

/// Restores natural Japanese commas (「、」), periods (「。」), and optional automatic line breaks (\n)
/// inside a single utterance without disturbing English text or already-punctuated passages.
pub fn restore_japanese_punctuation(
    text: &str,
    auto_punct: bool,
    auto_newline: bool,
) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || (!auto_punct && !auto_newline) {
        return trimmed.to_string();
    }
    if !trimmed.chars().any(is_japanese_char) {
        return trimmed.to_string();
    }

    let mut result_lines: Vec<String> = Vec::new();
    for raw_line in trimmed.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let formatted = format_single_line(line, auto_punct, auto_newline);
        result_lines.push(formatted);
    }

    result_lines.join("\n")
}

fn format_single_line(line: &str, auto_punct: bool, auto_newline: bool) -> String {
    // Pass 1: Insert sentence breaks & clause commas on unpunctuated spans
    let punctuated = if auto_punct {
        insert_internal_punctuation(line, auto_newline)
    } else {
        line.to_string()
    };

    // Pass 2: Break lines after existing '。', '？', '！' when followed by another full sentence
    if auto_newline {
        insert_newlines_after_terminal_punct(&punctuated)
    } else {
        punctuated
    }
}

fn insert_internal_punctuation(line: &str, auto_newline: bool) -> String {
    const STRONG_SENTENCE_BREAK_ENDINGS: &[&str] = &[
        "ありがとうございました",
        "お疲れ様でした",
        "よろしくお願いします",
        "お願いします",
        "ました",
        "でした",
        "ません",
        "ましょう",
        "ですか",
        "ますか",
        "ですよ",
        "ますよ",
        "ですね",
        "ますね",
        "です",
        "ます",
        "ください",
        "ちゃうんだよね",
        "んですよね",
        "んだよね",
        "だよね",
        "ですよね",
        "なんだよ",
        "なんだね",
        "んだよ",
        "んだね",
        "だよ",
        "だね",
        "じゃん",
        "てるよ",
        "てるね",
        "てたよ",
        "てたね",
        "ちゃうよ",
        "ちゃうね",
        "よさそう",
        "すごそう",
        "わかった",
        "分かった",
        "了解",
    ];

    const DEMO_SUBJECT_STARTERS: &[&str] = &[
        "でも", "ただ", "それで", "なので", "あと", "ちなみに", "ところで", "じゃあ", "それじゃあ",
        "では", "それと", "それから", "そして", "しかも", "それに", "というか", "ていうか",
        "とりあえず", "まず", "次に", "最後に", "実は", "正直", "要するに", "つまり",
        "たとえば", "例えば", "もし", "やっぱり", "やっぱ", "これ", "それ", "あれ", "ここ",
        "そこ", "何か", "なんか", "一応", "今回", "今後", "今日", "明日", "ちょっと", "多分",
        "たぶん",
    ];

    const CLAUSE_COMMA_ENDINGS: &[&str] = &[
        "けれども",
        "けれど",
        "だけど",
        "なんだけど",
        "んだけど",
        "てるけど",
        "あるけど",
        "いるけど",
        "ないけど",
        "ますけど",
        "ですけど",
        "けど",
        "ましたが",
        "でしたが",
        "ますが",
        "ですが",
        "ですので",
        "ますので",
        "なので",
        "ますから",
        "ですから",
        "ないから",
        "るから",
        "たから",
        "だから",
        "てから",
        "いくとき",
        "するとき",
        "くるとき",
        "あるとき",
        "いるとき",
        "ないとき",
        "たとき",
        "ときは",
        "ない場合は",
        "る場合は",
        "た場合は",
        "だったら",
    ];

    const LINE_START_CONNECTORS: &[&str] = &[
        "ちなみに",
        "ところで",
        "とりあえず",
        "それで",
        "それから",
        "それと",
        "要するに",
        "つまり",
        "たとえば",
        "例えば",
        "実は",
        "正直",
        "まず",
        "次に",
        "最後に",
    ];

    const MID_CONJUNCTIONS: &[&str] = &[
        "でも", "だけど", "それで", "だから", "なので", "ちなみに", "ところで", "じゃあ",
        "というか", "ていうか",
    ];

    let mut out = String::with_capacity(line.len() + 16);
    let mut byte_idx = 0usize;
    let mut chars_since_punct = 0usize;

    while byte_idx < line.len() {
        let rest = &line[byte_idx..];
        let remaining_chars = rest.chars().count();

        // 0. Check line-start / post-sentence connector (e.g. "ちなみにこれ..." -> "ちなみに、これ...")
        if chars_since_punct == 0 {
            if let Some(conn) = LINE_START_CONNECTORS.iter().find(|&&c| rest.starts_with(c)) {
                let conn_len = conn.len();
                let after = &rest[conn_len..];
                if after.chars().count() >= 5 {
                    if let Some(next_ch) = after.chars().next() {
                        if !is_punct_or_newline(next_ch)
                            && !matches!(
                                next_ch,
                                'の' | 'に' | 'が' | 'を' | 'は' | 'も' | 'で' | 'と' | 'だ'
                                    | 'て' | 'っ' | 'か'
                            )
                        {
                            out.push_str(conn);
                            out.push('、');
                            byte_idx += conn_len;
                            chars_since_punct = 0;
                            continue;
                        }
                    }
                }
            }
        }

        // Advance one character
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        byte_idx += ch.len_utf8();
        if is_punct_or_newline(ch) {
            chars_since_punct = 0;
        } else {
            chars_since_punct += 1;
        }

        let after = &line[byte_idx..];
        let after_chars = after.chars().count();
        if after_chars < 3 {
            continue;
        }
        let next_ch = after.chars().next().unwrap();
        if is_punct_or_newline(next_ch) || next_ch.is_whitespace() {
            continue;
        }

        // 1. Check strong sentence break inside run-on utterance
        if chars_since_punct >= 4 && after_chars >= 4 {
            let current_line = out.rsplit('\n').next().unwrap_or(&out);
            if let Some(ending) = STRONG_SENTENCE_BREAK_ENDINGS
                .iter()
                .find(|&&e| current_line.ends_with(e))
            {
                let starter_matched = DEMO_SUBJECT_STARTERS.iter().any(|&st| after.starts_with(st));
                // Avoid cutting polite "です"+"ても"/"てた" or "ます"+"た"/"て"
                let verb_inflection_continuation = (*ending == "です" || *ending == "ます")
                    && matches!(next_ch, 'た' | 'て' | 'せ' | 'が' | 'け' | 'の');
                // Avoid cutting "よさそう"+"でも" (which can mean "even if it looks good")
                let soft_stem_with_demo = matches!(*ending, "よさそう" | "すごそう")
                    && after.starts_with("でも");
                if starter_matched && !verb_inflection_continuation && !soft_stem_with_demo {
                    if !(*ending == "ます" && matches!(next_ch, 'た' | 'て' | 'せ'))
                        && !(*ending == "です" && matches!(next_ch, 'た' | 'て'))
                    {
                        if is_question_ending(current_line) {
                            out.push('？');
                        } else {
                            out.push('。');
                        }
                        if auto_newline {
                            out.push('\n');
                        }
                        chars_since_punct = 0;
                        continue;
                    }
                }
            }
        }

        // 2. Check clause-level comma after conjunctions/clause-endings ("〜あるけど..." -> "〜あるけど、...")
        if chars_since_punct >= 5 && after_chars >= 4 {
            let current_line = out.rsplit('\n').next().unwrap_or(&out);
            if CLAUSE_COMMA_ENDINGS.iter().any(|&c| current_line.ends_with(c)) {
                let blocked_particle = matches!(
                    next_ch,
                    'は' | 'が' | 'を' | 'に' | 'も' | 'の' | 'で' | 'と' | 'っ' | 'か' | 'ね'
                        | 'よ' | 'だ' | 'し' | 'ば' | 'て' | 'れ'
                );
                // Allow "でも"/"だけど"/"だから" right after clause ending (e.g. "感覚で、でも" / "あるけど、だから")
                let is_conj_next = MID_CONJUNCTIONS.iter().any(|&m| after.starts_with(m));
                if !blocked_particle || is_conj_next {
                    out.push('、');
                    chars_since_punct = 0;
                    continue;
                }
            }

            // 3. Check continuative char ('て'/'で'/'し') immediately before mid-utterance conjunction ("感覚ででも" -> "感覚で、でも")
            if chars_since_punct >= 6
                && matches!(ch, 'て' | 'で' | 'し')
                && MID_CONJUNCTIONS.iter().any(|&m| after.starts_with(m))
            {
                out.push('、');
                chars_since_punct = 0;
                continue;
            }
        }

        let _ = remaining_chars;
    }

    out
}

fn insert_newlines_after_terminal_punct(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars_on_line = 0usize;

    for (i, &c) in chars.iter().enumerate() {
        out.push(c);
        if c == '\n' {
            chars_on_line = 0;
            continue;
        }
        chars_on_line += 1;

        if matches!(c, '。' | '？' | '！' | '?' | '!') {
            // Skip repeated terminal punctuation like "！？" or "！！"
            let next_is_terminal = chars
                .get(i + 1)
                .map(|&n| matches!(n, '。' | '？' | '！' | '?' | '!' | '…'))
                .unwrap_or(false);
            if next_is_terminal {
                continue;
            }
            let rest_str: String = chars[i + 1..].iter().collect();
            let rest_trimmed = rest_str.trim_start();
            if chars_on_line >= 4
                && rest_trimmed.chars().count() >= 4
                && !starts_with_laugh_or_closing(rest_trimmed)
            {
                if !rest_str.starts_with('\n') {
                    out.push('\n');
                    chars_on_line = 0;
                }
            }
        }
    }
    out
}

/// Computes the natural separator to place between `prev` and `next` utterances.
pub fn compute_utterance_joiner(
    prev: &str,
    next: &str,
    auto_punct: bool,
    auto_newline: bool,
    auto_space: bool,
    strip_punctuation: bool,
) -> &'static str {
    let next_trim = next.trim();
    if prev.is_empty() || next_trim.is_empty() {
        return "";
    }
    if prev.ends_with(['\n', ' ', '\t', '　']) || next.starts_with('\n') {
        return "";
    }

    let last_line = prev.rsplit('\n').next().unwrap_or(prev).trim();
    if last_line.is_empty() {
        return "";
    }
    let last_len = last_line.chars().count();
    let next_len = next_trim.chars().count();

    // 1. Previous utterance already ended with terminal punctuation
    if last_line.ends_with(['。', '！', '？', '!', '?', '.', '…']) {
        if auto_newline
            && last_len >= 4
            && next_len >= 2
            && !starts_with_laugh_or_closing(next_trim)
        {
            return "\n";
        }
        return if auto_space { " " } else { "" };
    }

    // 2. Previous utterance ended with a comma (mid-sentence)
    if last_line.ends_with(['、', ',']) {
        if auto_newline && last_len >= 12 && starts_with_strong_topic_switch(next_trim) {
            return "\n";
        }
        return if auto_space { " " } else { "" };
    }

    // 3. Unpunctuated previous utterance: classify grammatical ending
    if is_continuative_ending(last_line) {
        if auto_punct && !strip_punctuation && last_len >= 3 {
            return if auto_space { "、 " } else { "、" };
        }
        return if auto_space { " " } else { "" };
    }

    if ends_like_complete_sentence(last_line) && last_len >= 4 {
        let is_q = is_question_ending(last_line);
        let do_punct = auto_punct && !strip_punctuation;
        let do_nl = auto_newline && next_len >= 2 && !starts_with_laugh_or_closing(next_trim);
        return match (do_punct, is_q, do_nl, auto_space) {
            (true, true, true, _) => "？\n",
            (true, false, true, _) => "。\n",
            (true, true, false, true) => "？ ",
            (true, false, false, true) => "。 ",
            (true, true, false, false) => "？",
            (true, false, false, false) => "。",
            (false, _, true, _) => "\n",
            (false, _, false, true) => " ",
            (false, _, false, false) => "",
        };
    }

    // 4. Ambiguous ending (noun / short phrase)
    if starts_like_new_topic_or_sentence(next_trim) && last_len >= 4 {
        let do_punct = auto_punct && !strip_punctuation;
        let do_nl = auto_newline && next_len >= 2 && !starts_with_laugh_or_closing(next_trim);
        return match (do_punct, do_nl, auto_space) {
            (true, true, _) => "。\n",
            (true, false, true) => "。 ",
            (true, false, false) => "。",
            (false, true, _) => "\n",
            (false, false, true) => " ",
            (false, false, false) => "",
        };
    }

    if auto_punct && !strip_punctuation && last_len >= 4 && !is_adnominal_prefix(last_line) {
        return if auto_space { "、 " } else { "、" };
    }

    if auto_space {
        " "
    } else {
        ""
    }
}

/// Joins a sequence of draft segments with natural Japanese punctuation and optional auto-newlines.
pub fn join_utterances_smart(
    segments: &[String],
    auto_punct: bool,
    auto_newline: bool,
    auto_space: bool,
    strip_punctuation: bool,
) -> String {
    let mut out = String::new();
    for seg in segments {
        if seg.is_empty() {
            continue;
        }
        if !out.is_empty() {
            let joiner = compute_utterance_joiner(
                &out,
                seg,
                auto_punct,
                auto_newline,
                auto_space,
                strip_punctuation,
            );
            out.push_str(joiner);
        }
        out.push_str(seg);
    }
    out
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
        assert_eq!(
            normalize_japanese_transcript("くとうてんがないとカタカラになる"),
            "句読点がないとカタカナになる"
        );
        // Valid Japanese/game vocabulary must remain untouched
        assert_eq!(
            normalize_japanese_transcript("宝箱のチェストを開ける"),
            "宝箱のチェストを開ける"
        );
    }

    #[test]
    fn restores_punctuation_and_newlines_on_fast_run_on_speech() {
        let raw = "複数いっぱいタスクあるけどじゃあちょっとテストで";
        let out = restore_japanese_punctuation(raw, true, true);
        assert_eq!(out, "複数いっぱいタスクあるけど、じゃあちょっとテストで");

        let multi = "これでいいのか？よさそうこれ一部だけ送信できるの？";
        let out2 = restore_japanese_punctuation(multi, true, true);
        assert_eq!(
            out2,
            "これでいいのか？\nよさそう。\nこれ一部だけ送信できるの？"
        );

        let out2_no_nl = restore_japanese_punctuation(multi, true, false);
        assert_eq!(
            out2_no_nl,
            "これでいいのか？よさそう。これ一部だけ送信できるの？"
        );
    }

    #[test]
    fn joins_continuous_utterances_naturally_with_and_without_auto_newline() {
        let segs = vec![
            "句読点とかしゃべる感覚で".to_string(),
            "でもなんかこれ全部つながってるよ".to_string(),
            "下書きがとんでもないことです。".to_string(),
            "このような感じで随時しゃべって追加していくとき".to_string(),
            "その句読点がないから多分全部つながっちゃうんだよね".to_string(),
        ];

        let with_nl = join_utterances_smart(&segs, true, true, false, false);
        assert_eq!(
            with_nl,
            "句読点とかしゃべる感覚で、でもなんかこれ全部つながってるよ。\n下書きがとんでもないことです。\nこのような感じで随時しゃべって追加していくとき、その句読点がないから多分全部つながっちゃうんだよね"
        );

        let without_nl = join_utterances_smart(&segs, true, false, false, false);
        assert_eq!(
            without_nl,
            "句読点とかしゃべる感覚で、でもなんかこれ全部つながってるよ。下書きがとんでもないことです。このような感じで随時しゃべって追加していくとき、その句読点がないから多分全部つながっちゃうんだよね"
        );
    }
}
