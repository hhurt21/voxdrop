use crate::automation::discord;
use crate::config::{DiscordTargetsConfig, VoiceCommandsConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    Send,
    ClearAll,
    Undo,
    Newline,
    Mute,
    Resume,
    // Discord remote control commands (bidirectional / reversible)
    DiscordMuteMic,
    DiscordUnmuteMic,
    DiscordDeafen,
    DiscordUndeafen,
    DiscordNavBack,
    DiscordNavForward,
    DiscordNextChannel,
    DiscordPrevChannel,
    DiscordPrevUnread,
    DiscordNextUnread,
    DiscordNextServer,
    DiscordPrevServer,
    DiscordScrollUp,
    DiscordScrollDown,
    DiscordJumpPresent,
    DiscordEditLast,
    DiscordCancelEscape,
    DiscordMarkRead,
    DiscordToggleMembers,
    DiscordTogglePins,
    DiscordToggleInbox,
    DiscordSearch,
    DiscordQuickSwitcher,
}

impl VoiceCommand {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::ClearAll => "clear_all",
            Self::Undo => "undo",
            Self::Newline => "newline",
            Self::Mute => "mute",
            Self::Resume => "resume",
            Self::DiscordMuteMic => "discord_mute_mic",
            Self::DiscordUnmuteMic => "discord_unmute_mic",
            Self::DiscordDeafen => "discord_deafen",
            Self::DiscordUndeafen => "discord_undeafen",
            Self::DiscordNavBack => "discord_nav_back",
            Self::DiscordNavForward => "discord_nav_forward",
            Self::DiscordNextChannel => "discord_next_channel",
            Self::DiscordPrevChannel => "discord_prev_channel",
            Self::DiscordPrevUnread => "discord_prev_unread",
            Self::DiscordNextUnread => "discord_next_unread",
            Self::DiscordNextServer => "discord_next_server",
            Self::DiscordPrevServer => "discord_prev_server",
            Self::DiscordScrollUp => "discord_scroll_up",
            Self::DiscordScrollDown => "discord_scroll_down",
            Self::DiscordJumpPresent => "discord_jump_present",
            Self::DiscordEditLast => "discord_edit_last",
            Self::DiscordCancelEscape => "discord_cancel_escape",
            Self::DiscordMarkRead => "discord_mark_read",
            Self::DiscordToggleMembers => "discord_toggle_members",
            Self::DiscordTogglePins => "discord_toggle_pins",
            Self::DiscordToggleInbox => "discord_toggle_inbox",
            Self::DiscordSearch => "discord_search",
            Self::DiscordQuickSwitcher => "discord_quick_switcher",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationReply {
    Yes,
    Next,
    Prev,
    Cancel,
}

/// Parses spoken responses while VoxDrop is asking "この人で合っていますか？".
/// Only matches short standalone confirmation replies so normal conversation is never hijacked.
pub fn parse_confirmation_reply(input: &str) -> Option<ConfirmationReply> {
    let s = trim_trailing_punct(input.trim())
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '、' | ','))
        .trim();
    if s.is_empty() || s.chars().count() > 14 {
        return None;
    }
    let lower = s.to_ascii_lowercase();

    const CANCEL: &[&str] = &[
        "いいえ",
        "いーえ",
        "いや",
        "no",
        "キャンセル",
        "やめて",
        "やめ",
        "中止",
        "閉じる",
        "とじて",
    ];
    if CANCEL.iter().any(|&w| lower == w || s == w) {
        return Some(ConfirmationReply::Cancel);
    }

    const PREV: &[&str] = &["前", "まえ", "戻って", "もどって", "前の人", "ひとつ前"];
    if PREV.iter().any(|&w| lower == w || s == w) {
        return Some(ConfirmationReply::Prev);
    }

    const NEXT: &[&str] = &[
        "違う",
        "ちがう",
        "ちがいます",
        "違います",
        "次",
        "つぎ",
        "別の人",
        "べつのひと",
        "別",
        "ほか",
        "他の人",
        "次の人",
        "next",
    ];
    if NEXT.iter().any(|&w| lower == w || s == w) {
        return Some(ConfirmationReply::Next);
    }

    const YES: &[&str] = &[
        "はい",
        "はいよ",
        "はいはい",
        "うん",
        "そう",
        "そうです",
        "合ってる",
        "あってる",
        "合ってます",
        "あってます",
        "オッケー",
        "おっけー",
        "ok",
        "yes",
        "この人",
        "その人",
        "決定",
        "開いて",
        "ひらいて",
    ];
    if YES.iter().any(|&w| lower == w || s == w) {
        return Some(ConfirmationReply::Yes);
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSwitchAction {
    /// Switch by exact target ID (unambiguous single registered passphrase match)
    SwitchExact(String),
    /// Switch/confirm by spoken friend or channel query (may match multiple registered targets or Quick Switcher)
    QueryFriend(String),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSwitchResult {
    pub action: TargetSwitchAction,
    pub remaining_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandParseResult {
    pub remaining_text: String,
    pub command: Option<VoiceCommand>,
}

/// Detects voice target switching via passphrase (合言葉) or friend name calling:
/// - Reset phrases: e.g. `"宛先解除"`
/// - Prefix form: e.g. `"合言葉 雑談"`, `"合言葉 たろう、今行くよ送信して"`, `"DM たなか"`
/// - Suffix form: e.g. `"雑談に切り替えて"`, `"たなかにDM"`, `"たなかを開いて"`, `"たなかと話す"`
/// - Optional standalone passphrase (when `allow_standalone_passphrase` is true)
pub fn parse_target_switch(
    input: &str,
    cfg: &DiscordTargetsConfig,
) -> Option<TargetSwitchResult> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let stripped = trim_trailing_punct(trimmed);

    // 1. Check target reset phrases
    let mut resets: Vec<&str> = cfg
        .reset_phrases
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    resets.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    for phrase in resets {
        if stripped.eq_ignore_ascii_case(phrase) {
            return Some(TargetSwitchResult {
                action: TargetSwitchAction::Reset,
                remaining_text: String::new(),
            });
        }
        if let Some(rest) = strip_prefix_ci(stripped, phrase) {
            if rest.is_empty() || starts_with_separator(rest) {
                return Some(TargetSwitchResult {
                    action: TargetSwitchAction::Reset,
                    remaining_text: trim_leading_sep(rest).to_string(),
                });
            }
        }
    }

    let mut prefixes: Vec<&str> = cfg
        .switch_prefixes
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    prefixes.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));

    let mut suffixes: Vec<&str> = cfg
        .switch_suffixes
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    suffixes.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));

    // Collect registered (target_id, passphrase/name) pairs sorted longest first
    let mut passphrases: Vec<(&str, &str)> = Vec::new();
    for target in &cfg.targets {
        for p in &target.passphrases {
            let pt = p.trim();
            if !pt.is_empty() {
                passphrases.push((target.id.as_str(), pt));
            }
        }
    }
    passphrases.sort_by(|a, b| b.1.chars().count().cmp(&a.1.chars().count()));

    // 2. Prefix form: "<prefix>[、,\s]*<passphrase_or_friend_name>[<suffix>][、,\s]*<remaining>"
    for prefix in &prefixes {
        if let Some(after_prefix) = strip_prefix_ci(stripped, prefix) {
            let body = trim_leading_sep(after_prefix);
            if body.is_empty() {
                continue;
            }
            // First check registered passphrases (longest first), ensuring ambiguity triggers QueryFriend!
            for &(_target_id, pass) in &passphrases {
                if let Some(after_pass) = strip_prefix_ci(body, pass) {
                    let mut rem = after_pass;
                    for suf in &suffixes {
                        if let Some(after_suf) = strip_prefix_ci(rem, suf) {
                            rem = after_suf;
                            break;
                        }
                    }
                    let rem_clean = trim_leading_sep(rem).to_string();
                    let (cands, is_single_exact) =
                        discord::find_matching_targets(pass, &cfg.targets);
                    let action = if is_single_exact && cands.len() == 1 {
                        TargetSwitchAction::SwitchExact(cands[0].id.clone())
                    } else {
                        TargetSwitchAction::QueryFriend(pass.to_string())
                    };
                    return Some(TargetSwitchResult {
                        action,
                        remaining_text: rem_clean,
                    });
                }
            }

            // Otherwise split at first comma/space to extract arbitrary friend name (for fuzzy / Quick Switcher!)
            let (raw_query, rem) = split_first_clause(body);
            let mut query_clean = raw_query;
            for suf in &suffixes {
                if let Some(q_before) = strip_suffix_ci(query_clean, suf) {
                    query_clean = trim_trailing_sep(q_before);
                    break;
                }
            }
            if !query_clean.is_empty() {
                return Some(TargetSwitchResult {
                    action: TargetSwitchAction::QueryFriend(query_clean.to_string()),
                    remaining_text: rem.to_string(),
                });
            }
        }
    }

    // 3. Suffix form: "<friend_or_passphrase><suffix>" (e.g. "たなかにDM", "雑談に切り替えて", "やまだを開いて")
    for suf in &suffixes {
        if let Some(before_suf) = strip_suffix_ci(stripped, suf) {
            let query = trim_trailing_sep(before_suf);
            if !query.is_empty() && query.chars().count() <= 20 {
                let (cands, is_single_exact) = discord::find_matching_targets(query, &cfg.targets);
                if is_single_exact && cands.len() == 1 {
                    return Some(TargetSwitchResult {
                        action: TargetSwitchAction::SwitchExact(cands[0].id.clone()),
                        remaining_text: String::new(),
                    });
                }
                if !cands.is_empty() || cfg.use_quick_switcher_for_unknown {
                    return Some(TargetSwitchResult {
                        action: TargetSwitchAction::QueryFriend(query.to_string()),
                        remaining_text: String::new(),
                    });
                }
            }
        }
    }

    // 4. Optional standalone passphrase match (only if user explicitly enabled it)
    if cfg.allow_standalone_passphrase && !cfg.targets.is_empty() {
        let (cands, is_single_exact) = discord::find_matching_targets(stripped, &cfg.targets);
        if is_single_exact && cands.len() == 1 {
            return Some(TargetSwitchResult {
                action: TargetSwitchAction::SwitchExact(cands[0].id.clone()),
                remaining_text: String::new(),
            });
        } else if !cands.is_empty() {
            return Some(TargetSwitchResult {
                action: TargetSwitchAction::QueryFriend(stripped.to_string()),
                remaining_text: String::new(),
            });
        }
    }

    None
}

/// Detects standalone or suffix voice commands and strips the command phrase from the text.
pub fn parse_voice_command(input: &str, cfg: &VoiceCommandsConfig) -> CommandParseResult {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return CommandParseResult {
            remaining_text: String::new(),
            command: None,
        };
    }

    let without_trailing_punct = trim_trailing_punct(trimmed);

    let mut candidates: Vec<(VoiceCommand, String)> = Vec::new();
    let groups: &[(VoiceCommand, &[String])] = &[
        (VoiceCommand::Send, &cfg.send),
        (VoiceCommand::ClearAll, &cfg.clear_all),
        (VoiceCommand::Undo, &cfg.undo),
        (VoiceCommand::Newline, &cfg.newline),
        (VoiceCommand::Mute, &cfg.mute),
        (VoiceCommand::Resume, &cfg.resume),
        (VoiceCommand::DiscordMuteMic, &cfg.discord_mute_mic),
        (VoiceCommand::DiscordUnmuteMic, &cfg.discord_unmute_mic),
        (VoiceCommand::DiscordDeafen, &cfg.discord_deafen),
        (VoiceCommand::DiscordUndeafen, &cfg.discord_undeafen),
        (VoiceCommand::DiscordNavBack, &cfg.discord_nav_back),
        (VoiceCommand::DiscordNavForward, &cfg.discord_nav_forward),
        (VoiceCommand::DiscordNextChannel, &cfg.discord_next_channel),
        (VoiceCommand::DiscordPrevChannel, &cfg.discord_prev_channel),
        (VoiceCommand::DiscordPrevUnread, &cfg.discord_prev_unread),
        (VoiceCommand::DiscordNextUnread, &cfg.discord_next_unread),
        (VoiceCommand::DiscordNextServer, &cfg.discord_next_server),
        (VoiceCommand::DiscordPrevServer, &cfg.discord_prev_server),
        (VoiceCommand::DiscordScrollUp, &cfg.discord_scroll_up),
        (VoiceCommand::DiscordScrollDown, &cfg.discord_scroll_down),
        (VoiceCommand::DiscordJumpPresent, &cfg.discord_jump_present),
        (VoiceCommand::DiscordEditLast, &cfg.discord_edit_last),
        (VoiceCommand::DiscordCancelEscape, &cfg.discord_cancel_escape),
        (VoiceCommand::DiscordMarkRead, &cfg.discord_mark_read),
        (VoiceCommand::DiscordToggleMembers, &cfg.discord_toggle_members),
        (VoiceCommand::DiscordTogglePins, &cfg.discord_toggle_pins),
        (VoiceCommand::DiscordToggleInbox, &cfg.discord_toggle_inbox),
        (VoiceCommand::DiscordSearch, &cfg.discord_search),
        (VoiceCommand::DiscordQuickSwitcher, &cfg.discord_quick_switcher),
    ];
    for (cmd, list) in groups {
        for p in *list {
            let pt = p.trim();
            if !pt.is_empty() {
                candidates.push((*cmd, pt.to_string()));
            }
        }
    }

    // Built-in phonetic & common Japanese STT mishearing aliases for Send so users
    // never get stuck when slurred speech or radio noise turns "送信して" into "そうしって"/"応戦して"
    // or "エンター" into "エンタ"/"エンサー"/"enter"
    if !cfg.send.is_empty() {
        for alias in [
            "送信してください",
            "送ってください",
            "送信してね",
            "送ってね",
            "そうしんして",
            "そうしんって",
            "そう信して",
            "そう信って",
            "そ信して",
            "そ信って",
            "ど信って",
            "そうしって",
            "どうしって",
            "そうしん",
            "送信って",
            "送信する",
            "送信し",
            "そうしんし",
            "ソーシーして",
            "そーしーして",
            "そうしーして",
            "そっしんして",
            "率進して",
            "送信",
            "通信って",
            "応戦して",
            "送進して",
            "送ってて",
            "送っって",
            "おくってて",
            "送って",
            "おくって",
            "エンターキー",
            "えんたー",
            "えんた",
            "エンタ",
            "エンサー",
            "エンザー",
            "ンター",
            "enter",
        ] {
            candidates.push((VoiceCommand::Send, alias.to_string()));
        }
    }

    candidates.sort_by(|a, b| b.1.chars().count().cmp(&a.1.chars().count()));

    for (cmd, phrase) in &candidates {
        if let Some(prefix) = strip_suffix_kana_ci(without_trailing_punct, phrase) {
            let cleaned_prefix = trim_trailing_sep(prefix);
            return CommandParseResult {
                remaining_text: cleaned_prefix.to_string(),
                command: Some(*cmd),
            };
        }
    }

    CommandParseResult {
        remaining_text: trimmed.to_string(),
        command: None,
    }
}

/// Case-insensitive + Katakana/Hiragana-insensitive suffix strip that returns the original prefix slice.
fn strip_suffix_kana_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    if let Some(rest) = text.strip_suffix(suffix) {
        return Some(rest);
    }
    let s_chars: Vec<char> = suffix.chars().map(fold_kana_ascii).collect();
    let t_indices: Vec<(usize, char)> = text.char_indices().collect();
    if s_chars.is_empty() || s_chars.len() > t_indices.len() {
        return None;
    }
    let start_idx = t_indices.len() - s_chars.len();
    for (offset, &sc) in s_chars.iter().enumerate() {
        let tc = fold_kana_ascii(t_indices[start_idx + offset].1);
        if tc != sc {
            return None;
        }
    }
    Some(&text[..t_indices[start_idx].0])
}

fn fold_kana_ascii(c: char) -> char {
    let code = c as u32;
    if (0x30A1..=0x30F6).contains(&code) {
        char::from_u32(code - 0x60).unwrap_or(c)
    } else {
        c.to_ascii_lowercase()
    }
}

fn split_first_clause(s: &str) -> (&str, &str) {
    for (idx, ch) in s.char_indices() {
        if ch.is_whitespace() || matches!(ch, '、' | ',' | '：' | ':') {
            let left = s[..idx].trim();
            let right = trim_leading_sep(&s[idx..]);
            return (left, right);
        }
    }
    (s.trim(), "")
}

fn trim_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| matches!(c, '。' | '、' | '！' | '？' | '!' | '?' | '.'))
}

fn trim_leading_sep(s: &str) -> &str {
    s.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '、' | ',' | '：' | ':' | '「' | '」')
    })
    .trim()
}

fn trim_trailing_sep(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '、' | ',' | '：' | ':'))
        .trim()
}

fn starts_with_separator(s: &str) -> bool {
    s.chars()
        .next()
        .map(|c| c.is_whitespace() || matches!(c, '、' | ',' | '。' | '：' | ':'))
        .unwrap_or(false)
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if let Some(rest) = text.strip_prefix(prefix) {
        return Some(rest);
    }
    let p_len = prefix.chars().count();
    if p_len == 0 {
        return Some(text);
    }
    for (i, ch) in text.char_indices() {
        let byte_idx = i + ch.len_utf8();
        if text[..byte_idx].chars().count() == p_len {
            if text[..byte_idx].eq_ignore_ascii_case(prefix) {
                return Some(&text[byte_idx..]);
            }
            return None;
        }
    }
    None
}

fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    if let Some(rest) = text.strip_suffix(suffix) {
        return Some(rest);
    }
    let s_chars = suffix.chars().count();
    let t_chars = text.chars().count();
    if s_chars > t_chars {
        return None;
    }
    let split_char_idx = t_chars - s_chars;
    let byte_idx = text
        .char_indices()
        .nth(split_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    if text[byte_idx..].eq_ignore_ascii_case(suffix) {
        Some(&text[..byte_idx])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DiscordTarget, DiscordTargetKind};

    fn sample_targets_cfg() -> DiscordTargetsConfig {
        let mut cfg = DiscordTargetsConfig::default();
        cfg.targets = vec![
            DiscordTarget {
                id: "ch_chat".into(),
                name: "雑談".into(),
                passphrases: vec!["ざつだん".into(), "雑談".into()],
                kind: DiscordTargetKind::Channel,
                guild_id: "111".into(),
                channel_id: "222".into(),
                ..Default::default()
            },
            DiscordTarget {
                id: "dm_taro".into(),
                name: "太郎".into(),
                passphrases: vec!["たろう".into(), "太郎".into()],
                kind: DiscordTargetKind::Dm,
                channel_id: "333".into(),
                user_id: "444".into(),
                ..Default::default()
            },
        ];
        cfg
    }

    #[test]
    fn parses_suffix_and_standalone_commands() {
        let cfg = VoiceCommandsConfig::default();

        let r1 = parse_voice_command("おはよう送信して。", &cfg);
        assert_eq!(r1.remaining_text, "おはよう");
        assert_eq!(r1.command, Some(VoiceCommand::Send));

        let r2 = parse_voice_command("キャンセル！", &cfg);
        assert_eq!(r2.remaining_text, "");
        assert_eq!(r2.command, Some(VoiceCommand::ClearAll));

        // Phonetic & common Japanese STT mishearing aliases for Send
        let r3 = parse_voice_command("どうですか？応戦して。", &cfg);
        assert_eq!(r3.remaining_text, "どうですか？");
        assert_eq!(r3.command, Some(VoiceCommand::Send));

        let r3b = parse_voice_command("おい！ソーシーして", &cfg);
        assert_eq!(r3b.remaining_text, "おい！");
        assert_eq!(r3b.command, Some(VoiceCommand::Send));

        let r3c = parse_voice_command("テストーストを、率進して", &cfg);
        assert_eq!(r3c.remaining_text, "テストーストを");
        assert_eq!(r3c.command, Some(VoiceCommand::Send));

        let r4 = parse_voice_command("おはようえんたー", &cfg);
        assert_eq!(r4.remaining_text, "おはよう");
        assert_eq!(r4.command, Some(VoiceCommand::Send));

        let r5 = parse_voice_command("送信。", &cfg);
        assert_eq!(r5.remaining_text, "");
        assert_eq!(r5.command, Some(VoiceCommand::Send));

        // Bidirectional Discord remote control commands
        assert_eq!(
            parse_voice_command("ミュート解除", &cfg).command,
            Some(VoiceCommand::DiscordUnmuteMic)
        );
        assert_eq!(
            parse_voice_command("デフン解除", &cfg).command,
            Some(VoiceCommand::DiscordUndeafen)
        );
        assert_eq!(
            parse_voice_command("前の未読", &cfg).command,
            Some(VoiceCommand::DiscordPrevUnread)
        );
        assert_eq!(
            parse_voice_command("前の画面", &cfg).command,
            Some(VoiceCommand::DiscordNavBack)
        );
        assert_eq!(
            parse_voice_command("編集キャンセル", &cfg).command,
            Some(VoiceCommand::DiscordCancelEscape)
        );
    }

    #[test]
    fn normal_text_has_no_command() {
        let cfg = VoiceCommandsConfig::default();
        let r = parse_voice_command("今日はいい天気ですね", &cfg);
        assert_eq!(r.remaining_text, "今日はいい天気ですね");
        assert_eq!(r.command, None);
    }

    #[test]
    fn parses_passphrase_and_friend_dm_queries() {
        let cfg = sample_targets_cfg();

        // Registered exact match
        let r1 = parse_target_switch("合言葉 雑談。", &cfg).expect("prefix match");
        assert_eq!(r1.action, TargetSwitchAction::SwitchExact("ch_chat".into()));
        assert_eq!(r1.remaining_text, "");

        // Prefix + message in one utterance
        let r2 = parse_target_switch("合言葉 たろう、今からログインするよ送信して", &cfg)
            .expect("prefix+msg");
        assert_eq!(r2.action, TargetSwitchAction::SwitchExact("dm_taro".into()));
        assert_eq!(r2.remaining_text, "今からログインするよ送信して");

        // Unregistered friend called by suffix -> QueryFriend (opens Discord Quick Switcher!)
        let r3 = parse_target_switch("やまだにDM", &cfg).expect("unregistered friend dm");
        assert_eq!(r3.action, TargetSwitchAction::QueryFriend("やまだ".into()));

        // Confirmation replies
        assert_eq!(parse_confirmation_reply("はい！"), Some(ConfirmationReply::Yes));
        assert_eq!(parse_confirmation_reply("いいえ。"), Some(ConfirmationReply::Cancel));
        assert_eq!(parse_confirmation_reply("違う"), Some(ConfirmationReply::Next));
        assert_eq!(parse_confirmation_reply("別の人"), Some(ConfirmationReply::Next));
        assert_eq!(parse_confirmation_reply("前の人"), Some(ConfirmationReply::Prev));
        assert_eq!(parse_confirmation_reply("キャンセル"), Some(ConfirmationReply::Cancel));
        // Long normal utterances containing "そう"/"うん"/"それ" must NOT be mistaken for confirmation
        assert_eq!(parse_confirmation_reply("まあでもこれでよさそうだしテストしてみよう"), None);
    }
}
