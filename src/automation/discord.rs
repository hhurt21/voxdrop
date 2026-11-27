use crate::automation::input::{self, KeyAction};
use crate::automation::window;
use crate::config::{DiscordTarget, DiscordTargetKind, GuardConfig};
use std::ffi::c_void;
use std::thread;
use std::time::Duration;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Normalizes Japanese Katakana -> Hiragana, ASCII lowercase, and strips whitespace/honorifics.
pub fn normalize_for_match(raw: &str) -> String {
    let base = raw.trim();
    let without_honorific = base
        .strip_suffix("さん")
        .or_else(|| base.strip_suffix("くん"))
        .or_else(|| base.strip_suffix("ちゃん"))
        .unwrap_or(base);

    without_honorific
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '・' | '_' | '-' | '　'))
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

/// Finds all registered Discord targets matching a spoken friend/channel name query.
pub fn find_matching_targets(query: &str, targets: &[DiscordTarget]) -> (Vec<DiscordTarget>, bool) {
    let q = normalize_for_match(query);
    if q.is_empty() || targets.is_empty() {
        return (Vec::new(), false);
    }

    let mut exact: Vec<DiscordTarget> = Vec::new();
    let mut scored: Vec<(i32, DiscordTarget)> = Vec::new();

    for t in targets {
        let mut keys: Vec<String> = Vec::new();
        let n = normalize_for_match(&t.name);
        if !n.is_empty() {
            keys.push(n);
        }
        for p in &t.passphrases {
            let np = normalize_for_match(p);
            if !np.is_empty() {
                keys.push(np);
            }
        }

        if keys.iter().any(|k| k == &q) {
            exact.push(t.clone());
            continue;
        }

        let mut best_score = 0i32;
        for k in &keys {
            if k.starts_with(&q) || q.starts_with(k) {
                best_score = best_score.max(90);
            } else if k.contains(&q) || q.contains(k) {
                best_score = best_score.max(75);
            } else {
                let sim = char_overlap_score(&q, k);
                if sim >= 55 {
                    best_score = best_score.max(sim);
                }
            }
        }
        if best_score > 0 {
            if t.kind == DiscordTargetKind::Dm {
                best_score += 5;
            }
            scored.push((best_score, t.clone()));
        }
    }

    if !exact.is_empty() {
        let single_exact = exact.len() == 1;
        return (exact, single_exact);
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let candidates: Vec<DiscordTarget> = scored.into_iter().map(|(_, t)| t).collect();
    (candidates, false)
}

fn char_overlap_score(a: &str, b: &str) -> i32 {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    if ac.is_empty() || bc.is_empty() {
        return 0;
    }
    let common = ac.iter().filter(|c| bc.contains(c)).count();
    let denom = ac.len().max(bc.len());
    ((common * 100) / denom) as i32
}

/// Parses a copied Discord channel/DM URL into `(is_dm, guild_id, channel_id)`.
pub fn parse_discord_channel_url(raw: &str) -> Option<(bool, String, String)> {
    let trimmed = raw.trim();
    let idx = trimmed.find("/channels/")?;
    let rest = &trimmed[idx + "/channels/".len()..];
    let mut parts = rest.split('/');
    let guild = parts.next()?.trim();
    let channel = parts.next()?.split(['?', '#', ' ']).next()?.trim();
    if channel.is_empty() || !channel.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if guild.eq_ignore_ascii_case("@me") {
        Some((true, String::new(), channel.to_string()))
    } else if !guild.is_empty() && guild.chars().all(|c| c.is_ascii_digit()) {
        Some((false, guild.to_string(), channel.to_string()))
    } else {
        None
    }
}

/// Builds a safe, official `discord://` deep link for switching the Discord desktop client.
pub fn build_deep_link(target: &DiscordTarget) -> Option<String> {
    let channel_id = clean_snowflake(&target.channel_id);
    let guild_id = target.guild_id.trim();
    let user_id = clean_snowflake(&target.user_id);

    match target.kind {
        DiscordTargetKind::Channel => {
            if channel_id.is_empty() {
                return None;
            }
            if guild_id.is_empty() || guild_id.eq_ignore_ascii_case("@me") {
                Some(format!("discord://-/channels/@me/{}", channel_id))
            } else {
                let g = clean_snowflake(guild_id);
                if g.is_empty() {
                    Some(format!("discord://-/channels/@me/{}", channel_id))
                } else {
                    Some(format!("discord://-/channels/{}/{}", g, channel_id))
                }
            }
        }
        DiscordTargetKind::Dm => {
            if !channel_id.is_empty() {
                Some(format!("discord://-/channels/@me/{}", channel_id))
            } else if !user_id.is_empty() {
                Some(format!("discord://-/users/{}", user_id))
            } else {
                None
            }
        }
        DiscordTargetKind::Mention => {
            if !channel_id.is_empty() {
                let g = clean_snowflake(guild_id);
                if g.is_empty() {
                    Some(format!("discord://-/channels/@me/{}", channel_id))
                } else {
                    Some(format!("discord://-/channels/{}/{}", g, channel_id))
                }
            } else {
                None
            }
        }
    }
}

/// Focuses Discord and opens its Quick Switcher (`Ctrl+K`) pre-filled with `@<friend_name>`.
pub fn open_quick_switcher_search(
    friend_name: &str,
    guard: &GuardConfig,
    navigate_delay_ms: u64,
) -> Result<(), String> {
    let cur = window::get_foreground_process_name().unwrap_or_default();
    let already_discord = window::is_process_allowed(&cur, &guard.allowed_processes, true);
    if !already_discord {
        let _ = window::ensure_allowed_foreground(&guard.allowed_processes, navigate_delay_ms.max(1000));
    }

    let after = window::get_foreground_process_name().unwrap_or_default();
    if guard.enabled && !window::is_process_allowed(&after, &guard.allowed_processes, true) {
        return Err(format!(
            "Discord を最前面にできませんでした（現在: {}）。Discordを起動した状態でお試しください。",
            if after.is_empty() { "不明" } else { &after }
        ));
    }

    input::send_key_action(KeyAction::CtrlK)?;
    thread::sleep(Duration::from_millis(140));
    let query = format!("@{}", friend_name.trim());
    input::paste_with_clipboard_restore(&query, false, guard.clipboard_restore_delay_ms)?;
    Ok(())
}

pub fn format_message_for_target(text: &str, target: Option<&DiscordTarget>) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Some(t) = target else {
        return text.to_string();
    };
    let should_mention = t.prepend_mention || t.kind == DiscordTargetKind::Mention;
    let uid = clean_snowflake(&t.user_id);
    if should_mention && !uid.is_empty() {
        let prefix = format!("<@{}>", uid);
        if trimmed.starts_with(&prefix) {
            text.to_string()
        } else {
            format!("{} {}", prefix, text)
        }
    } else {
        text.to_string()
    }
}

pub fn target_display_label(target: Option<&DiscordTarget>) -> String {
    let Some(t) = target else {
        return "現在の画面（未指定）".into();
    };
    let kind_icon = match t.kind {
        DiscordTargetKind::Channel => "#",
        DiscordTargetKind::Dm => "DM:",
        DiscordTargetKind::Mention => "@",
    };
    let name = if !t.name.trim().is_empty() {
        t.name.trim()
    } else if !t.channel_id.trim().is_empty() {
        t.channel_id.trim()
    } else if !t.user_id.trim().is_empty() {
        t.user_id.trim()
    } else {
        "名称未設定"
    };
    if !t.webhook_url.trim().is_empty() {
        format!("{}{} [Webhook]", kind_icon, name)
    } else {
        format!("{}{}", kind_icon, name)
    }
}

pub fn open_deep_link(url: &str) -> Result<(), String> {
    if !url.starts_with("discord://") {
        return Err(format!("不正なDiscordリンクです: {}", url));
    }
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(format!(
            "Discordチャンネルへの移動に失敗しました (code={})",
            res.0 as isize
        ));
    }
    Ok(())
}

pub fn parse_webhook_url(url: &str) -> Result<(String, String), String> {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .ok_or_else(|| "Webhook URL は https:// で始まる必要があります".to_string())?;
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| "Webhook URL のパスが不正です".to_string())?;
    let host_lower = host.to_ascii_lowercase();
    let allowed_host = host_lower == "discord.com"
        || host_lower == "discordapp.com"
        || host_lower == "ptb.discord.com"
        || host_lower == "canary.discord.com";
    if !allowed_host || !path.starts_with("api/webhooks/") {
        return Err(
            "公式 Discord Webhook URL（https://discord.com/api/webhooks/...）のみ使用できます"
                .into(),
        );
    }
    Ok((host.to_string(), format!("/{}", path)))
}

/// Generic HTTPS request via native Windows WinHTTP (zero external HTTP crates).
fn winhttp_request(
    method: &str,
    host: &str,
    path: &str,
    auth_header: Option<&str>,
    body: Option<&str>,
) -> Result<(u32, String), String> {
    let method_w: Vec<u16> = method.encode_utf16().chain(std::iter::once(0)).collect();
    let host_w: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    let mut headers = String::from("Content-Type: application/json; charset=utf-8\r\n");
    if let Some(auth) = auth_header {
        headers.push_str(&format!("Authorization: {}\r\n", auth));
    }
    let headers_w: Vec<u16> = headers.encode_utf16().collect();
    let body_bytes = body.map(|b| b.as_bytes()).unwrap_or(&[]);

    unsafe {
        let session = WinHttpOpen(
            w!("VoxDrop/1.0"),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHTTP セッションの初期化に失敗しました".into());
        }

        let result = (|| -> Result<(u32, String), String> {
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), 443, 0);
            if connect.is_null() {
                return Err("Discord サーバーへの接続に失敗しました".into());
            }

            let req_res = (|| -> Result<(u32, String), String> {
                let request = WinHttpOpenRequest(
                    connect,
                    PCWSTR(method_w.as_ptr()),
                    PCWSTR(path_w.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    WINHTTP_FLAG_SECURE,
                );
                if request.is_null() {
                    return Err("HTTPS リクエストの作成に失敗しました".into());
                }

                let send_res = (|| -> Result<(u32, String), String> {
                    let body_ptr = if body_bytes.is_empty() {
                        None
                    } else {
                        Some(body_bytes.as_ptr() as *const c_void)
                    };
                    WinHttpSendRequest(
                        request,
                        Some(&headers_w),
                        body_ptr,
                        body_bytes.len() as u32,
                        body_bytes.len() as u32,
                        0,
                    )
                    .map_err(|e| format!("HTTPS 送信に失敗しました: {}", e))?;

                    WinHttpReceiveResponse(request, std::ptr::null_mut())
                        .map_err(|e| format!("HTTPS 応答の受信に失敗しました: {}", e))?;

                    let mut status_code: u32 = 0;
                    let mut size = std::mem::size_of::<u32>() as u32;
                    WinHttpQueryHeaders(
                        request,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        PCWSTR::null(),
                        Some(&mut status_code as *mut u32 as *mut c_void),
                        &mut size,
                        std::ptr::null_mut(),
                    )
                    .map_err(|e| format!("HTTP ステータス取得に失敗しました: {}", e))?;

                    let mut resp_bytes = Vec::new();
                    loop {
                        let mut avail: u32 = 0;
                        if WinHttpQueryDataAvailable(request, &mut avail).is_err() || avail == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; avail as usize];
                        let mut read: u32 = 0;
                        if WinHttpReadData(
                            request,
                            chunk.as_mut_ptr() as *mut c_void,
                            avail,
                            &mut read,
                        )
                        .is_err()
                            || read == 0
                        {
                            break;
                        }
                        resp_bytes.extend_from_slice(&chunk[..read as usize]);
                        if resp_bytes.len() > 2 * 1024 * 1024 {
                            break;
                        }
                    }

                    let resp_text = String::from_utf8_lossy(&resp_bytes).into_owned();
                    Ok((status_code, resp_text))
                })();

                let _ = WinHttpCloseHandle(request);
                send_res
            })();

            let _ = WinHttpCloseHandle(connect);
            req_res
        })();

        let _ = WinHttpCloseHandle(session);
        result
    }
}

pub fn send_via_webhook(webhook_url: &str, content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Ok(());
    }
    let (host, path) = parse_webhook_url(webhook_url)?;
    let body = serde_json::json!({ "content": content }).to_string();
    let (status, _) = winhttp_request("POST", &host, &path, None, Some(&body))?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("Discord Webhook が HTTP {} を返しました", status))
    }
}

/// Sends a message using an official Discord Bot Token to a server channel.
pub fn send_via_bot_api(bot_token: &str, channel_id: &str, content: &str) -> Result<(), String> {
    let token = bot_token.trim();
    let cid = clean_snowflake(channel_id);
    if token.is_empty() || cid.is_empty() {
        return Err("Bot Token またはチャンネルIDが未設定です".into());
    }
    let auth = format!("Bot {}", token);
    let path = format!("/api/v10/channels/{}/messages", cid);
    let body = serde_json::json!({ "content": content }).to_string();
    let (status, _) = winhttp_request("POST", "discord.com", &path, Some(&auth), Some(&body))?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("Discord Bot API が HTTP {} を返しました", status))
    }
}

#[derive(Debug, Clone)]
pub struct BotSyncedChannel {
    pub guild_id: String,
    pub server_name: String,
    pub channel_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct BotSyncedMember {
    pub user_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct BotSyncData {
    pub channels: Vec<BotSyncedChannel>,
    pub members: Vec<BotSyncedMember>,
    pub member_note: Option<String>,
}

/// Syncs servers, text channels, and (if intent enabled) server members via official Discord Bot API.
pub fn sync_with_bot_token(bot_token: &str) -> Result<BotSyncData, String> {
    let token = bot_token.trim();
    if token.is_empty() {
        return Err("公式 Discord Bot Token を入力してください".into());
    }
    let auth = format!("Bot {}", token);
    let (status, body) = winhttp_request(
        "GET",
        "discord.com",
        "/api/v10/users/@me/guilds",
        Some(&auth),
        None,
    )?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "Discord Bot 認証に失敗しました (HTTP {})。Bot Token が正しいか確認してください。",
            status
        ));
    }

    let guilds: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("サーバー一覧JSON解析エラー: {}", e))?;

    let mut data = BotSyncData::default();
    let mut member_forbidden = false;

    for g in guilds.iter().take(12) {
        let Some(gid) = g.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let gname = g
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("サーバー")
            .to_string();

        // Fetch channels in guild
        let ch_path = format!("/api/v10/guilds/{}/channels", gid);
        if let Ok((200..=299, ch_body)) =
            winhttp_request("GET", "discord.com", &ch_path, Some(&auth), None)
        {
            if let Ok(ch_list) = serde_json::from_str::<Vec<serde_json::Value>>(&ch_body) {
                for ch in ch_list {
                    let kind = ch.get("type").and_then(|v| v.as_u64()).unwrap_or(99);
                    // 0 = GuildText, 5 = GuildAnnouncement
                    if kind == 0 || kind == 5 {
                        if let (Some(cid), Some(cname)) = (
                            ch.get("id").and_then(|v| v.as_str()),
                            ch.get("name").and_then(|v| v.as_str()),
                        ) {
                            data.channels.push(BotSyncedChannel {
                                guild_id: gid.to_string(),
                                server_name: gname.clone(),
                                channel_id: cid.to_string(),
                                name: cname.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Fetch guild members (works when SERVER MEMBERS INTENT is enabled in Developer Portal)
        let mem_path = format!("/api/v10/guilds/{}/members?limit=100", gid);
        match winhttp_request("GET", "discord.com", &mem_path, Some(&auth), None) {
            Ok((200..=299, m_body)) => {
                if let Ok(m_list) = serde_json::from_str::<Vec<serde_json::Value>>(&m_body) {
                    for m in m_list {
                        let Some(user) = m.get("user") else {
                            continue;
                        };
                        if user.get("bot").and_then(|v| v.as_bool()).unwrap_or(false) {
                            continue;
                        }
                        let Some(uid) = user.get("id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let display = m
                            .get("nick")
                            .and_then(|v| v.as_str())
                            .or_else(|| user.get("global_name").and_then(|v| v.as_str()))
                            .or_else(|| user.get("username").and_then(|v| v.as_str()))
                            .unwrap_or(uid);
                        data.members.push(BotSyncedMember {
                            user_id: uid.to_string(),
                            name: display.to_string(),
                        });
                    }
                }
            }
            Ok((403, _)) => {
                member_forbidden = true;
            }
            _ => {}
        }
    }

    if member_forbidden && data.members.is_empty() {
        data.member_note = Some(
            "※ メンバー一覧も取得する場合は Discord Developer Portal で「SERVER MEMBERS INTENT」をONにしてください".into(),
        );
    }

    Ok(data)
}

fn clean_snowflake(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_discord_channel_and_dm_urls() {
        assert_eq!(
            parse_discord_channel_url("https://discord.com/channels/111222/333444"),
            Some((false, "111222".into(), "333444".into()))
        );
        assert_eq!(
            parse_discord_channel_url("https://discord.com/channels/@me/555666"),
            Some((true, String::new(), "555666".into()))
        );
        assert_eq!(parse_discord_channel_url("https://example.com"), None);
    }

    #[test]
    fn builds_channel_dm_and_user_deep_links() {
        let ch = DiscordTarget {
            id: "1".into(),
            name: "雑談".into(),
            kind: DiscordTargetKind::Channel,
            guild_id: "111222333".into(),
            channel_id: "444555666".into(),
            ..Default::default()
        };
        assert_eq!(
            build_deep_link(&ch).as_deref(),
            Some("discord://-/channels/111222333/444555666")
        );
    }

    #[test]
    fn matches_multiple_similar_friends_with_kana_normalization() {
        let targets = vec![
            DiscordTarget {
                id: "t1".into(),
                name: "田中たろう".into(),
                passphrases: vec!["たなかたろう".into()],
                kind: DiscordTargetKind::Dm,
                channel_id: "111".into(),
                ..Default::default()
            },
            DiscordTarget {
                id: "t2".into(),
                name: "田中じろう".into(),
                passphrases: vec!["たなかじろう".into()],
                kind: DiscordTargetKind::Dm,
                channel_id: "222".into(),
                ..Default::default()
            },
        ];

        let (cands, single_exact) = find_matching_targets("タナカさん", &targets);
        assert!(!single_exact);
        assert_eq!(cands.len(), 2);
    }

    #[test]
    fn formats_mention_prefix_without_duplication() {
        let mention_target = DiscordTarget {
            id: "m1".into(),
            name: "太郎".into(),
            kind: DiscordTargetKind::Mention,
            user_id: "99887766".into(),
            prepend_mention: true,
            ..Default::default()
        };
        assert_eq!(
            format_message_for_target("おはよう", Some(&mention_target)),
            "<@99887766> おはよう"
        );
    }

    #[test]
    fn validates_official_discord_webhook_urls() {
        let (host, path) =
            parse_webhook_url("https://discord.com/api/webhooks/12345/abcdef").unwrap();
        assert_eq!(host, "discord.com");
        assert_eq!(path, "/api/webhooks/12345/abcdef");
        assert!(parse_webhook_url("https://example.com/api/webhooks/1/2").is_err());
    }

    #[test]
    fn splits_long_messages_within_discord_limit() {
        let short = "こんにちは";
        assert_eq!(split_message_for_discord(short, 1950), vec!["こんにちは"]);

        let long = "これは長文テストです。".repeat(300); // > 3000 chars
        let chunks = split_message_for_discord(&long, 1950);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= 1950));
        assert_eq!(chunks.concat(), long);
    }
}

/// Splits outgoing Discord text safely at newlines or sentence boundaries (`。！？`)
/// so messages exceeding Discord's 2000-character limit never fail silently (#23).
pub fn split_message_for_discord(text: &str, max_chars: usize) -> Vec<String> {
    let limit = max_chars.max(200);
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.chars().count() <= limit {
            chunks.push(rest.to_string());
            break;
        }
        let mut split_byte = rest.len();
        let mut best_punct: Option<usize> = None;
        let mut best_newline: Option<usize> = None;
        for (count, (idx, ch)) in rest.char_indices().enumerate() {
            let end_idx = idx + ch.len_utf8();
            if count + 1 > limit {
                split_byte = idx;
                break;
            }
            if ch == '\n' {
                best_newline = Some(end_idx);
            } else if matches!(ch, '。' | '！' | '？' | '.' | '!' | '?') {
                best_punct = Some(end_idx);
            }
        }
        let chosen = best_newline
            .or(best_punct)
            .filter(|&b| b > split_byte / 3)
            .unwrap_or(split_byte)
            .max(1);
        let (head, tail) = rest.split_at(chosen);
        chunks.push(head.to_string());
        rest = tail;
    }
    chunks
}
