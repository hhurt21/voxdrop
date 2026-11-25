//! Discord Windows Toast notification parser, DM vs Server Channel classifier,
//! sender allow/block filter, ring-buffer deduplicator, and TTS text sanitizer.

use crate::config::DiscordTtsConfig;
use crate::notification::toast_watcher::RawToastNotification;
use regex::Regex;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDiscordNotification {
    pub sender: String,
    pub raw_body: String,
    pub sanitized_body: String,
    pub speech_text: String,
    pub is_server_channel: bool,
}

/// Fixed-capacity ring buffer deduplicator preventing double-reading identical toast notifications
/// within `dedup_window_ms`.
pub struct NotificationDeduplicator {
    cap: usize,
    entries: VecDeque<(u64, Instant)>,
}

impl NotificationDeduplicator {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(4),
            entries: VecDeque::with_capacity(cap.max(4)),
        }
    }

    pub fn is_duplicate_and_record(
        &mut self,
        sender: &str,
        body: &str,
        window_ms: u64,
        now: Instant,
    ) -> bool {
        let key = hash_pair(sender, body);
        let window = Duration::from_millis(window_ms);
        if self
            .entries
            .iter()
            .any(|&(h, ts)| h == key && now.saturating_duration_since(ts) <= window)
        {
            return true;
        }
        while self.entries.len() >= self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back((key, now));
        false
    }
}

fn hash_pair(a: &str, b: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    a.hash(&mut hasher);
    0xffu8.hash(&mut hasher);
    b.hash(&mut hasher);
    hasher.finish()
}

fn is_discord_app_label(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l == "discord" || l == "discord ptb" || l == "discord canary" || l.starts_with("discord ")
}

fn is_toast_chrome_noise(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    let l = t.to_ascii_lowercase();
    matches!(
        l.as_str(),
        "閉じる"
            | "通知を閉じる"
            | "無視"
            | "返信"
            | "送信"
            | "設定"
            | "今"
            | "close"
            | "dismiss"
            | "reply"
            | "send"
            | "settings"
            | "just now"
            | "新しい通知"
            | "new notification"
    )
}

/// Checks if a Discord notification title represents a server channel rather than a DM.
/// Typical Discord server toast title: `"Taro (#general, MyServer)"` or `"Taro - #general"`.
pub fn is_server_channel_title(title: &str) -> bool {
    let t = title.trim();
    t.contains("(#") || t.contains("( #") || t.contains("（#") || t.contains(" - #") || t.starts_with('#')
}

/// Extracts clean sender name from title (e.g. `"Taro (#general, MyServer)"` -> `"Taro"`).
pub fn extract_sender_name(title: &str) -> String {
    let t = title.trim();
    for sep in ["(#", "( #", "（#", " - #"] {
        if let Some((left, _)) = t.split_once(sep) {
            let trimmed = left.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    t.to_string()
}

/// Sanitizes Discord message body for natural VOICEVOX TTS output:
/// - Code blocks ```...``` -> "コード省略"
/// - URLs `https?://...` -> `url_replacement`
/// - Custom emojis `<a?:name:123>` -> removed
/// - Raw ID mentions `<@123>` / `<#123>` / `<@&123>` -> removed
/// - Markdown formatting (`**`, `~~`, `||`, `` ` ``, `__`) -> removed
/// - Truncates to `max_chars` with `"以下略"` suffix if exceeded
pub fn sanitize_message_for_tts(raw: &str, url_replacement: &str, max_chars: usize) -> String {
    static RE_CODEBLOCK: OnceLock<Regex> = OnceLock::new();
    static RE_URL: OnceLock<Regex> = OnceLock::new();
    static RE_CUSTOM_EMOJI: OnceLock<Regex> = OnceLock::new();
    static RE_MENTION: OnceLock<Regex> = OnceLock::new();
    static RE_MULTI_WS: OnceLock<Regex> = OnceLock::new();

    let re_code = RE_CODEBLOCK.get_or_init(|| Regex::new(r"(?s)```.*?```").unwrap());
    let re_url = RE_URL.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
    let re_emoji = RE_CUSTOM_EMOJI.get_or_init(|| Regex::new(r"<a?:[A-Za-z0-9_]+:\d+>").unwrap());
    let re_mention = RE_MENTION.get_or_init(|| Regex::new(r"<(?:@!?|@&|#)\d+>").unwrap());
    let re_ws = RE_MULTI_WS.get_or_init(|| Regex::new(r"[\r\n\t ]+").unwrap());

    let mut text = re_code.replace_all(raw, "コード省略").into_owned();
    let url_rep = if url_replacement.trim().is_empty() {
        "URL省略"
    } else {
        url_replacement.trim()
    };
    text = re_url.replace_all(&text, url_rep).into_owned();
    text = re_emoji.replace_all(&text, "").into_owned();
    text = re_mention.replace_all(&text, "").into_owned();

    // Strip Discord markdown symbols
    for sym in ["**", "~~", "||", "__", "`"] {
        if text.contains(sym) {
            text = text.replace(sym, "");
        }
    }

    text = re_ws.replace_all(&text, " ").trim().to_string();
    if text.is_empty() {
        return text;
    }

    let limit = max_chars.max(5);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > limit {
        let truncated: String = chars[..limit].iter().collect();
        format!("{}以下略", truncated.trim_end())
    } else {
        text
    }
}

fn sender_matches_list(sender: &str, list: &[String]) -> bool {
    let s = sender.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    list.iter()
        .map(|x| x.trim().to_ascii_lowercase())
        .filter(|x| !x.is_empty())
        .any(|pat| s == pat || s.contains(&pat))
}

/// Parses, filters, deduplicates, and formats a raw Windows toast notification for Discord TTS.
pub fn filter_and_format_toast(
    raw: &RawToastNotification,
    cfg: &DiscordTtsConfig,
    dedup: &mut NotificationDeduplicator,
) -> Option<ParsedDiscordNotification> {
    filter_and_format_toast_at(raw, cfg, dedup, Instant::now())
}

pub fn filter_and_format_toast_at(
    raw: &RawToastNotification,
    cfg: &DiscordTtsConfig,
    dedup: &mut NotificationDeduplicator,
    now: Instant,
) -> Option<ParsedDiscordNotification> {
    // 1. Verify notification originated from Discord
    let mut is_discord = raw.app_name_hint.to_ascii_lowercase().contains("discord")
        || raw.window_name.to_ascii_lowercase().contains("discord");

    let mut content_lines: Vec<String> = Vec::new();
    for line in &raw.texts {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_discord_app_label(trimmed) {
            is_discord = true;
            continue;
        }
        if is_toast_chrome_noise(trimmed) {
            continue;
        }
        // Avoid duplicate consecutive text nodes (UIA sometimes exposes Name on both wrapper & child)
        if content_lines.last().map(|s| s.as_str()) == Some(trimmed) {
            continue;
        }
        content_lines.push(trimmed.to_string());
    }

    if !is_discord || content_lines.len() < 2 {
        return None;
    }

    // 2. Split into title (sender / channel header) and body lines
    let title = &content_lines[0];
    let raw_body = content_lines[1..].join(" ");
    let is_server = is_server_channel_title(title);

    if cfg.dm_only && is_server {
        return None;
    }

    let sender = extract_sender_name(title);
    if sender.is_empty() {
        return None;
    }

    // 3. Sender allowlist / blocklist filtering
    if !cfg.allow_senders.is_empty() && !sender_matches_list(&sender, &cfg.allow_senders) {
        return None;
    }
    if sender_matches_list(&sender, &cfg.block_senders) {
        return None;
    }

    // 4. Deduplication within `dedup_window_ms`
    if dedup.is_duplicate_and_record(&sender, &raw_body, cfg.dedup_window_ms, now) {
        return None;
    }

    // 5. Sanitize & format speech text
    let sanitized_body = sanitize_message_for_tts(&raw_body, &cfg.url_replacement, cfg.max_chars);
    if sanitized_body.is_empty() {
        return None;
    }

    let speech_text = if cfg.read_sender_name {
        let tpl = if cfg.template.trim().is_empty() {
            "{sender}さんから。{message}"
        } else {
            cfg.template.as_str()
        };
        tpl.replace("{sender}", &sender)
            .replace("{message}", &sanitized_body)
    } else {
        sanitized_body.clone()
    };

    Some(ParsedDiscordNotification {
        sender,
        raw_body,
        sanitized_body,
        speech_text,
        is_server_channel: is_server,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(texts: &[&str]) -> RawToastNotification {
        RawToastNotification {
            window_name: "新しい通知".into(),
            class_name: "Windows.UI.Core.CoreWindow".into(),
            automation_id: "NormalToastView".into(),
            app_name_hint: String::new(),
            texts: texts.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn accepts_discord_dm_and_rejects_server_channel_when_dm_only() {
        let cfg = DiscordTtsConfig {
            enabled: true,
            dm_only: true,
            ..Default::default()
        };
        let mut dedup = NotificationDeduplicator::new(20);
        let now = Instant::now();

        let dm = toast(&["Discord", "Taro", "今ログインできる？"]);
        let parsed = filter_and_format_toast_at(&dm, &cfg, &mut dedup, now).expect("DM parsed");
        assert_eq!(parsed.sender, "Taro");
        assert!(!parsed.is_server_channel);
        assert_eq!(parsed.speech_text, "Taroさんから。今ログインできる？");

        let srv = toast(&["Discord", "Taro (#general, MyServer)", "雑談メッセージ"]);
        assert!(filter_and_format_toast_at(&srv, &cfg, &mut dedup, now).is_none());

        // Non-Discord toast must be ignored
        let other = toast(&["Slack", "Taro", "会議です"]);
        assert!(filter_and_format_toast_at(&other, &cfg, &mut dedup, now).is_none());
    }

    #[test]
    fn sanitizes_urls_codeblocks_emojis_markdown_and_truncates() {
        let msg = "これ見て！ https://example.com/very/long/url <:nice:123456> **重要** ```let x = 1;```";
        let out = sanitize_message_for_tts(msg, "URL省略", 80);
        assert_eq!(out, "これ見て！ URL省略 重要 コード省略");

        let long = "あ".repeat(100);
        let trunc = sanitize_message_for_tts(&long, "URL省略", 10);
        assert_eq!(trunc, format!("{}以下略", "あ".repeat(10)));
    }

    #[test]
    fn deduplicates_identical_notifications_within_window() {
        let cfg = DiscordTtsConfig {
            enabled: true,
            dedup_window_ms: 3000,
            ..Default::default()
        };
        let mut dedup = NotificationDeduplicator::new(20);
        let t0 = Instant::now();

        let dm = toast(&["Discord", "Alice", "おつかれ！"]);
        assert!(filter_and_format_toast_at(&dm, &cfg, &mut dedup, t0).is_some());
        // Within 3s -> skipped
        assert!(
            filter_and_format_toast_at(&dm, &cfg, &mut dedup, t0 + Duration::from_millis(1500))
                .is_none()
        );
        // After 3s -> accepted again
        assert!(
            filter_and_format_toast_at(&dm, &cfg, &mut dedup, t0 + Duration::from_millis(3500))
                .is_some()
        );
    }

    #[test]
    fn respects_sender_allow_and_block_lists() {
        let mut cfg = DiscordTtsConfig {
            enabled: true,
            block_senders: vec!["spam".into()],
            ..Default::default()
        };
        let mut dedup = NotificationDeduplicator::new(20);
        let now = Instant::now();

        assert!(
            filter_and_format_toast_at(&toast(&["Discord", "SpamBot", "広告"]), &cfg, &mut dedup, now)
                .is_none()
        );
        assert!(
            filter_and_format_toast_at(&toast(&["Discord", "Taro", "おはよう"]), &cfg, &mut dedup, now)
                .is_some()
        );

        cfg.allow_senders = vec!["hanako".into()];
        assert!(
            filter_and_format_toast_at(&toast(&["Discord", "Taro", "別メッセージ"]), &cfg, &mut dedup, now)
                .is_none()
        );
        assert!(
            filter_and_format_toast_at(&toast(&["Discord", "Hanako", "よろしく"]), &cfg, &mut dedup, now)
                .is_some()
        );
    }
}
