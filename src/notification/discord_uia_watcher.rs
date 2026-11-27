//! Background Windows UI Automation Discord DM watcher.
//! Detects new / unread Discord DMs directly from the Discord desktop window's accessibility tree
//! so incoming DMs are read aloud even when Windows notifications or Discord desktop banners are OFF,
//! and during active open-DM conversations where Discord suppresses Windows toasts.
//! Uses only official Windows UI Automation (`IUIAutomation`) — zero process injection or memory access.

use crate::automation::uia::{clean_friend_label, parse_discord_window_title};
use crate::automation::window::extract_exe_name;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    UIA_ButtonControlTypeId, UIA_ListItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
};

const WM_GETOBJECT_MSG: u32 = 0x003D;
const OBJID_CLIENT_LPARAM: isize = -4;

#[derive(Debug, Clone)]
pub enum UiaDmEvent {
    /// New message observed directly in the currently open DM conversation
    OpenDmMessage { sender: String, body: String },
    /// Unread DM badge increased or newly appeared in Discord's sidebar
    UnreadBadge { sender: String, count: u32 },
    /// Optional debug trace line when `debug_log_toasts` is enabled
    Debug(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedMessage {
    pub key: String,
    pub snowflake_id: Option<u64>,
    pub explicit_author: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanRegion {
    Unknown,
    ServersOrDmRail,
    ServersContainer(usize),
    DirectMessagesList,
    UserArea,
}

#[derive(Default)]
struct UiaScanSnapshot {
    open_dm_friend: Option<String>,
    title_is_server_channel: bool,
    self_display_name: Option<String>,
    unread_badges: HashMap<String, u32>,
    recent_messages: Vec<ScannedMessage>,
}

struct DiscordHwndCtx {
    best_score: i32,
    best: Option<(HWND, String)>,
}

fn score_discord_window_candidate(
    visible: bool,
    iconic: bool,
    rect: RECT,
    title: &str,
) -> i32 {
    let w = rect.right.saturating_sub(rect.left);
    let h = rect.bottom.saturating_sub(rect.top);
    let non_trivial_rect = w >= 200 && h >= 150;
    let trimmed = title.trim();
    let lower = trimmed.to_ascii_lowercase();
    let has_hint = lower.contains("discord")
        || trimmed.contains('@')
        || trimmed.contains("ダイレクトメッセージ")
        || lower.contains("direct message");

    if visible && non_trivial_rect && has_hint {
        100
    } else if visible && non_trivial_rect && !trimmed.is_empty() {
        80
    } else if iconic && has_hint {
        60
    } else if visible && non_trivial_rect {
        40
    } else if !visible && !iconic && has_hint && non_trivial_rect {
        20
    } else if !visible && !iconic && has_hint {
        10
    } else {
        0
    }
}

unsafe extern "system" fn find_any_discord_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut DiscordHwndCtx);
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return BOOL(1);
    }
    if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        let mut buf = vec![0u16; 512];
        let mut sz = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut sz,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if ok && sz > 0 {
            let full = String::from_utf16_lossy(&buf[..sz as usize]);
            if let Some(exe) = extract_exe_name(&full) {
                if exe.eq_ignore_ascii_case("Discord.exe")
                    || exe.eq_ignore_ascii_case("DiscordPTB.exe")
                    || exe.eq_ignore_ascii_case("DiscordCanary.exe")
                {
                    let mut cls_buf = vec![0u16; 128];
                    let clen = GetClassNameW(hwnd, &mut cls_buf);
                    let cls = if clen > 0 {
                        String::from_utf16_lossy(&cls_buf[..clen as usize])
                    } else {
                        String::new()
                    };
                    if cls == "Chrome_WidgetWin_1" {
                        let mut title_buf = vec![0u16; 256];
                        let tlen = GetWindowTextW(hwnd, &mut title_buf);
                        let title = if tlen > 0 {
                            String::from_utf16_lossy(&title_buf[..tlen as usize])
                                .trim()
                                .to_string()
                        } else {
                            String::new()
                        };
                        let visible = IsWindowVisible(hwnd).as_bool();
                        let iconic = IsIconic(hwnd).as_bool();
                        let mut rect = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut rect);
                        let score = score_discord_window_candidate(visible, iconic, rect, &title);
                        if score > ctx.best_score {
                            ctx.best_score = score;
                            ctx.best = Some((hwnd, title));
                        }
                    }
                }
            }
        }
    }
    BOOL(1)
}

fn find_discord_hwnd_any() -> Option<(HWND, String)> {
    let mut ctx = DiscordHwndCtx {
        best_score: 0,
        best: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_any_discord_cb),
            LPARAM(&mut ctx as *mut DiscordHwndCtx as isize),
        );
    }
    ctx.best
}

/// Parses Discord sidebar/DM-list unread labels such as:
/// - `"Taro, 1件の未読メッセージ"` -> `Some(("Taro", 1))`
/// - `"Taro, 2件のメンション"` -> `Some(("Taro", 2))`
/// - `"Taro, 未読, ダイレクトメッセージ"` -> `Some(("Taro", 1))`
/// - `"alice, 3 unread messages"` -> `Some(("alice", 3))`
pub fn parse_unread_dm_badge(label: &str, in_dm_list: bool) -> Option<(String, u32)> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 110 {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    let has_dm_marker =
        trimmed.contains("ダイレクトメッセージ") || lower.contains("direct message");

    // Must explicitly indicate unread / mention status.
    // Outside DirectMessagesList, require explicit "ダイレクトメッセージ" / "unread message" / "件の未読メッセージ"
    // so server icons ("サーバー名, 1件のメンション") are NEVER mistaken for DMs!
    let has_dm_unread_marker = trimmed.contains("件の未読メッセージ")
        || lower.contains("unread message")
        || (in_dm_list && (trimmed.contains("未読") || lower.contains("unread")))
        || (has_dm_marker
            && (trimmed.contains("未読")
                || lower.contains("unread")
                || trimmed.contains("件のメンション")
                || lower.contains("mention")));

    if !has_dm_unread_marker {
        return None;
    }

    // Ignore server channels, voice channels, threads, forums, or unread divider banners
    if trimmed.contains("テキストチャンネル")
        || trimmed.contains("ボイスチャンネル")
        || trimmed.contains("アナウンスチャンネル")
        || trimmed.contains("フォーラム")
        || trimmed.contains("スレッド")
        || trimmed.contains("ステージ")
        || trimmed.contains("カテゴリー")
        || trimmed.contains("新しい未読")
        || lower.contains("text channel")
        || lower.contains("voice channel")
        || lower.contains("thread")
        || lower.contains("forum")
        || lower.contains("stage")
        || lower.contains("category")
        || trimmed.starts_with('#')
    {
        return None;
    }

    // Extract numeric count if present
    let mut count = 1u32;
    for part in trimmed.split([',', '、']) {
        let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty()
            && (part.contains("件")
                || part.to_ascii_lowercase().contains("unread")
                || part.to_ascii_lowercase().contains("mention"))
        {
            if let Ok(n) = digits.parse::<u32>() {
                if n > 0 && n < 10000 {
                    count = n;
                    break;
                }
            }
        }
    }

    let sender = clean_friend_label(trimmed)?;
    let s_lower = sender.to_ascii_lowercase();
    if sender.is_empty()
        || sender.chars().all(|c| c.is_ascii_digit())
        || sender.contains("未読")
        || sender.contains("件の")
        || s_lower.contains("unread")
        || s_lower.contains("mention")
    {
        return None;
    }
    Some((sender, count))
}

fn is_messages_area_label(trimmed: &str, lower: &str) -> bool {
    trimmed.chars().count() <= 100
        && (trimmed.contains("でのメッセージ") || lower.starts_with("messages in "))
}

/// Extracts the DM partner name from Discord's messages region ARIA label:
/// - `"@かげろー でのメッセージ"` / `"かげろー でのメッセージ"` -> `Some("かげろー")`
/// - `"Messages in @kagero"` -> `Some("kagero")`
/// - `"#雑談 でのメッセージ"` / `"Messages in #general"` -> `None` (server channel)
pub fn extract_dm_friend_from_messages_area(label: &str) -> Option<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 100 {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    if let Some(before) = trimmed
        .strip_suffix(" でのメッセージ")
        .or_else(|| trimmed.strip_suffix("でのメッセージ"))
    {
        let target = before.trim();
        if target.starts_with('#') || target.contains("チャンネル") || target.contains("スレッド")
        {
            return None;
        }
        let friend = target.trim_start_matches('@').trim();
        if friend.is_empty()
            || friend == "ダイレクトメッセージ"
            || friend.eq_ignore_ascii_case("direct messages")
        {
            return None;
        }
        return Some(friend.to_string());
    }

    if lower.starts_with("messages in @") && trimmed.is_char_boundary(13) {
        let friend = trimmed[13..].trim();
        if !friend.is_empty() {
            return Some(friend.to_string());
        }
    }

    if lower.starts_with("messages in #") {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("ダイレクトメッセージ:") {
        let friend = rest.trim().trim_start_matches('@').trim();
        if !friend.is_empty() {
            return Some(friend.to_string());
        }
    }
    if lower.starts_with("direct message:") && trimmed.is_char_boundary(15) {
        let friend = trimmed[15..].trim().trim_start_matches('@').trim();
        if !friend.is_empty() {
            return Some(friend.to_string());
        }
    }

    None
}

fn extract_self_name_from_status_label(label: &str) -> Option<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 90 {
        return None;
    }
    for sep in [", ", "、", " - "] {
        if let Some((left, right)) = trimmed.split_once(sep) {
            let r_lower = right.to_ascii_lowercase();
            let is_status = right.contains("オンライン")
                || right.contains("退席中")
                || right.contains("取り込み中")
                || right.contains("通話中")
                || right.contains("ステータス")
                || r_lower.contains("online")
                || r_lower.contains("idle")
                || r_lower.contains("do not disturb")
                || r_lower.contains("invisible")
                || r_lower.contains("set status");
            if is_status {
                let candidate = left.trim().trim_start_matches('@').trim();
                if !candidate.is_empty()
                    && candidate != "ユーザーエリア"
                    && !candidate.eq_ignore_ascii_case("user area")
                    && !candidate.eq_ignore_ascii_case("status")
                {
                    return Some(candidate.to_string());
                }
            }
        }
    }
    None
}

unsafe fn scan_discord_snapshot(hwnd: HWND, title: &str) -> Option<UiaScanSnapshot> {
    // Gently ping Chromium's accessibility tree even when Discord is behind another window / minimized
    let mut _msg_res: usize = 0;
    let _ = SendMessageTimeoutW(
        hwnd,
        WM_GETOBJECT_MSG,
        windows::Win32::Foundation::WPARAM(0),
        LPARAM(OBJID_CLIENT_LPARAM),
        SMTO_ABORTIFHUNG,
        80,
        Some(&mut _msg_res),
    );

    let uia: IUIAutomation =
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let root = uia.ElementFromHandle(hwnd).ok()?;
    let walker = uia.ControlViewWalker().ok()?;

    let mut snap = UiaScanSnapshot::default();
    let (title_srv, title_ch, title_dm) = parse_discord_window_title(title);
    snap.title_is_server_channel = title_srv.is_some() || title_ch.is_some();
    snap.open_dm_friend = title_dm;

    let mut visited = 0usize;
    walk_snapshot(
        &walker,
        &root,
        ScanRegion::Unknown,
        0,
        &mut visited,
        &mut snap,
    );
    Some(snap)
}

unsafe fn walk_snapshot(
    walker: &IUIAutomationTreeWalker,
    node: &IUIAutomationElement,
    region: ScanRegion,
    depth: usize,
    visited: &mut usize,
    snap: &mut UiaScanSnapshot,
) {
    const MAX_DEPTH: usize = 38;
    const MAX_VISITED: usize = 12_000;

    if depth > MAX_DEPTH || *visited > MAX_VISITED {
        return;
    }
    if let ScanRegion::ServersContainer(start_d) = region {
        if depth > start_d + 6 {
            return;
        }
    }
    *visited += 1;

    let name = node
        .CurrentName()
        .map(|b| b.to_string())
        .unwrap_or_default();
    let trimmed = name.trim();
    let mut next_region = region;

    if !trimmed.is_empty() {
        let lower = trimmed.to_ascii_lowercase();
        // Prune irrelevant/noisy subtrees early for speed and budget preservation
        if trimmed == "チャンネル"
            || lower == "channels"
            || (trimmed.ends_with("チャンネル") && trimmed.chars().count() <= 24)
            || trimmed == "メンバー"
            || trimmed == "メンバーリスト"
            || lower == "members"
            || lower.starts_with("members list")
            || trimmed.contains("通話コントロール")
            || lower.contains("voice connected")
            || trimmed == "検索"
            || lower == "search"
            || trimmed == "スレッド"
            || lower == "threads"
        {
            return;
        }

        if is_messages_area_label(trimmed, &lower) {
            if !snap.title_is_server_channel && snap.recent_messages.is_empty() {
                let area_friend = extract_dm_friend_from_messages_area(trimmed);
                let dm_friend = snap.open_dm_friend.clone().or(area_friend);
                if let Some(friend) = dm_friend {
                    snap.open_dm_friend = Some(friend);
                    scan_recent_messages_bottom_up(walker, node, &mut snap.recent_messages);
                }
            }
            return;
        }

        if trimmed == "ユーザーエリア" || lower == "user area" {
            next_region = ScanRegion::UserArea;
        } else if lower.contains("servers sidebar") || trimmed == "ホーム" || lower == "home" {
            next_region = ScanRegion::ServersOrDmRail;
        } else if trimmed == "サーバー" || lower == "servers" {
            next_region = ScanRegion::ServersContainer(depth);
        } else if trimmed == "ダイレクトメッセージ"
            || lower == "direct messages"
            || lower == "private channels"
        {
            next_region = ScanRegion::DirectMessagesList;
        } else {
            if snap.self_display_name.is_none()
                && (matches!(region, ScanRegion::UserArea)
                    || trimmed.contains("ステータスを設定")
                    || lower.contains("set status"))
            {
                if let Some(me) = extract_self_name_from_status_label(trimmed) {
                    snap.self_display_name = Some(me);
                }
            }

            match region {
                ScanRegion::ServersOrDmRail => {
                    if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, false) {
                        let entry = snap.unread_badges.entry(sender).or_insert(0);
                        *entry = (*entry).max(cnt);
                    }
                }
                ScanRegion::ServersContainer(_) => {
                    if trimmed.contains("ダイレクトメッセージ") || lower.contains("direct message") {
                        if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, false) {
                            let entry = snap.unread_badges.entry(sender).or_insert(0);
                            *entry = (*entry).max(cnt);
                        }
                    }
                }
                ScanRegion::DirectMessagesList => {
                    if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, true) {
                        let entry = snap.unread_badges.entry(sender).or_insert(0);
                        *entry = (*entry).max(cnt);
                    }
                }
                ScanRegion::UserArea | ScanRegion::Unknown => {
                    if trimmed.contains("ダイレクトメッセージ") || lower.contains("direct message") {
                        if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, false) {
                            let entry = snap.unread_badges.entry(sender).or_insert(0);
                            *entry = (*entry).max(cnt);
                        }
                    }
                }
            }
        }
    }

    if let Ok(mut child) = walker.GetFirstChildElement(node) {
        let mut siblings = 0usize;
        while siblings < 180 && *visited <= MAX_VISITED {
            walk_snapshot(walker, &child, next_region, depth + 1, visited, snap);
            siblings += 1;
            match walker.GetNextSiblingElement(&child) {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }
}

unsafe fn is_message_item_node(node: &IUIAutomationElement) -> bool {
    if let Ok(aid) = node.CurrentAutomationId() {
        let s = aid.to_string();
        if s.starts_with("chat-messages-") {
            return true;
        }
    }
    if let Ok(ct) = node.CurrentControlType() {
        if ct == UIA_ListItemControlTypeId {
            return true;
        }
    }
    false
}

unsafe fn collect_last_message_elements(
    walker: &IUIAutomationTreeWalker,
    container: &IUIAutomationElement,
    depth: usize,
) -> Vec<IUIAutomationElement> {
    let mut items = Vec::new();
    let mut containers = Vec::new();
    let Ok(mut cur) = walker.GetLastChildElement(container) else {
        return items;
    };

    let mut checked = 0usize;
    while checked < 24 && items.len() < 12 {
        if is_message_item_node(&cur) {
            items.push(cur.clone());
        } else if depth < 3 {
            containers.push(cur.clone());
        }
        checked += 1;
        match walker.GetPreviousSiblingElement(&cur) {
            Ok(prev) => cur = prev,
            Err(_) => break,
        }
    }

    if !items.is_empty() {
        items.reverse();
        return items;
    }

    // Discord sometimes wraps ListItems inside an inner List/Group scroller element
    for sub in containers.into_iter().take(4) {
        let found = collect_last_message_elements(walker, &sub, depth + 1);
        if !found.is_empty() {
            return found;
        }
    }

    items
}

fn is_ui_action_or_chrome_label(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "リアクションを追加"
            | "スーパーリアクションを追加"
            | "返信"
            | "編集"
            | "転送"
            | "その他"
            | "スレッドを作成"
            | "ピン留め"
            | "アプリ"
            | "bot"
            | "app"
            | "add reaction"
            | "add super reaction"
            | "reply"
            | "edit"
            | "forward"
            | "more"
            | "create thread"
            | "pin message"
            | "新しい未読"
            | "new"
            | "（編集済み）"
            | "(edited)"
            | "メッセージをクリックして表示"
            | "元のメッセージをクリックして表示"
            | "元のメッセージ"
    ) || trimmed.contains("リアクション、")
        || trimmed.contains("件のリアクション")
        || lower.contains("reactions")
}

fn is_reply_context_node(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with("返信先")
        || lower.starts_with("replying to")
        || trimmed.contains("返信しました")
}

unsafe fn collect_message_descendant_texts(
    walker: &IUIAutomationTreeWalker,
    node: &IUIAutomationElement,
    depth: usize,
    visited: &mut usize,
    out: &mut Vec<String>,
) {
    if depth > 7 || *visited >= 45 || out.len() >= 18 {
        return;
    }
    *visited += 1;

    let name = node
        .CurrentName()
        .map(|b| b.to_string())
        .unwrap_or_default();
    let trimmed = name.trim();

    if depth > 0 && is_reply_context_node(trimmed) {
        return;
    }

    let is_button = node
        .CurrentControlType()
        .map(|ct| ct == UIA_ButtonControlTypeId)
        .unwrap_or(false);

    if depth > 0 && !trimmed.is_empty() && !is_ui_action_or_chrome_label(trimmed) {
        // Record leaf / inline text or author header nodes without duplicating consecutive text
        let skip_btn = is_button && trimmed.chars().count() <= 2 && out.is_empty();
        if !skip_btn && out.last().map(|s| s.as_str()) != Some(trimmed) {
            // Only push non-button nodes or nodes that don't have children duplicating the exact label
            if !is_button || walker.GetFirstChildElement(node).is_err() {
                out.push(trimmed.to_string());
            } else {
                // Button wrapping text (like clickable author handle)
                out.push(trimmed.to_string());
                return;
            }
        }
    }

    if let Ok(mut child) = walker.GetFirstChildElement(node) {
        let mut siblings = 0usize;
        while siblings < 30 && *visited < 45 && out.len() < 18 {
            collect_message_descendant_texts(walker, &child, depth + 1, visited, out);
            siblings += 1;
            match walker.GetNextSiblingElement(&child) {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }
}

unsafe fn scan_recent_messages_bottom_up(
    walker: &IUIAutomationTreeWalker,
    messages_node: &IUIAutomationElement,
    out: &mut Vec<ScannedMessage>,
) {
    let elements = collect_last_message_elements(walker, messages_node, 0);
    for (idx, elem) in elements.into_iter().enumerate() {
        let automation_id = elem
            .CurrentAutomationId()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let mut raw_label = elem
            .CurrentName()
            .map(|b| b.to_string())
            .unwrap_or_default()
            .trim()
            .to_string();
        if raw_label.is_empty() {
            if let Ok(first_child) = walker.GetFirstChildElement(&elem) {
                if let Ok(child_name) = first_child.CurrentName() {
                    let s = child_name.to_string();
                    if !s.trim().is_empty() {
                        raw_label = s.trim().to_string();
                    }
                }
            }
        }
        let mut descendant_texts = Vec::new();
        let mut visited = 0usize;
        collect_message_descendant_texts(walker, &elem, 0, &mut visited, &mut descendant_texts);

        if let Some(parsed) = parse_scanned_message(
            automation_id.trim(),
            &raw_label,
            &descendant_texts,
            idx,
        ) {
            out.push(parsed);
        }
    }
}

fn parse_snowflake_from_automation_id(automation_id: &str) -> Option<u64> {
    let rest = automation_id.trim().strip_prefix("chat-messages-")?;
    let last_seg = rest.rsplit('-').next()?;
    if last_seg.len() >= 15 && last_seg.chars().all(|c| c.is_ascii_digit()) {
        last_seg.parse::<u64>().ok()
    } else {
        None
    }
}

fn looks_like_hh_mm(s: &str) -> bool {
    let mut parts = s.split(':');
    let Some(h) = parts.next() else {
        return false;
    };
    let Some(m) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let m_digits = m.split_whitespace().next().unwrap_or("");
    (1..=2).contains(&h.len())
        && h.chars().all(|c| c.is_ascii_digit())
        && m_digits.len() == 2
        && m_digits.chars().all(|c| c.is_ascii_digit())
}

pub fn is_discord_timestamp(token: &str) -> bool {
    let t = token
        .trim()
        .trim_start_matches(|c: char| c == '—' || c == '-' || c.is_whitespace())
        .trim();
    if t.is_empty() || t.chars().count() > 36 {
        return false;
    }
    if looks_like_hh_mm(t) {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    if t.starts_with("今日")
        || t.starts_with("昨日")
        || lower.starts_with("today at ")
        || lower.starts_with("yesterday at ")
    {
        return t.contains(':');
    }
    // Date + time such as "2026/11/27 22:56" or "11/27/2026 10:56 PM" or "2026年11月27日 22:56"
    if (t.contains('/') || (t.contains('年') && t.contains('日'))) && t.contains(':') {
        return t.chars().filter(|c| c.is_ascii_digit()).count() >= 4;
    }
    false
}

fn strip_message_noise_suffixes(s: &str) -> String {
    let mut cur = s.trim().to_string();
    loop {
        let before_len = cur.len();
        for suf in ["（編集済み）", "(edited)"] {
            if cur.ends_with(suf) {
                let cut = cur.len() - suf.len();
                if cur.is_char_boundary(cut) {
                    cur = cur[..cut].trim().to_string();
                }
            }
        }
        // Strip trailing timestamp like "強そう 22:56" or "強そう — 今日 22:56"
        if let Some(pos) = cur.rfind(" — ") {
            if is_discord_timestamp(&cur[pos + " — ".len()..]) {
                cur = cur[..pos].trim().to_string();
            }
        }
        if let Some(pos) = cur.rfind(' ') {
            if is_discord_timestamp(&cur[pos + 1..]) {
                // Also remove preceding "今日"/"昨日" if present
                let head = cur[..pos].trim_end();
                let trimmed_day = head
                    .strip_suffix("今日")
                    .or_else(|| head.strip_suffix("昨日"))
                    .unwrap_or(head)
                    .trim_end();
                cur = trimmed_day.to_string();
            }
        }
        if cur.len() == before_len {
            break;
        }
    }
    cur.trim().to_string()
}

fn strip_reply_preview(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(idx) = trimmed.find('\n') {
        let first = trimmed[..idx].trim();
        if is_reply_context_node(first) {
            return trimmed[idx + 1..].trim();
        }
    }
    trimmed
}

/// Pure, unit-testable parser converting a Discord `ListItem` ARIA label + descendant texts
/// into a normalized `ScannedMessage` (extracting explicit author when present vs grouped continuation).
pub fn parse_scanned_message(
    automation_id: &str,
    raw_label: &str,
    descendant_texts: &[String],
    fallback_idx: usize,
) -> Option<ScannedMessage> {
    let snowflake_id = parse_snowflake_from_automation_id(automation_id);

    let mut clean_desc: Vec<String> = Vec::new();
    for t in descendant_texts {
        let s = strip_reply_preview(t).trim();
        if s.is_empty() || is_ui_action_or_chrome_label(s) || is_reply_context_node(s) {
            continue;
        }
        if clean_desc.last().map(|p| p.as_str()) != Some(s) {
            clean_desc.push(s.to_string());
        }
    }

    let mut explicit_author: Option<String> = None;
    let mut body = String::new();

    if !clean_desc.is_empty() {
        // Find the first timestamp element inside descendant texts
        if let Some(ts_idx) = clean_desc.iter().position(|t| is_discord_timestamp(t)) {
            let before_ts: Vec<&str> = clean_desc[..ts_idx]
                .iter()
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .collect();
            if !before_ts.is_empty() {
                explicit_author = Some(before_ts[0].to_string());
            }
            let after_parts: Vec<String> = clean_desc[ts_idx + 1..]
                .iter()
                .filter(|s| !is_discord_timestamp(s) && !is_ui_action_or_chrome_label(s))
                .map(|s| strip_message_noise_suffixes(s))
                .filter(|s| !s.is_empty())
                .collect();
            body = after_parts.join(" ");
        }
    }

    // Fallback or supplement from `raw_label` when descendant texts had no timestamp structure or no body
    if body.is_empty() {
        let label_clean = strip_reply_preview(raw_label).trim();
        if !label_clean.is_empty() {
            let raw_parts: Vec<&str> = label_clean
                .split(", ")
                .map(|p| p.trim())
                .filter(|p| !p.is_empty() && !is_ui_action_or_chrome_label(p))
                .collect();

            let had_leading_ts = raw_parts
                .first()
                .map(|p| is_discord_timestamp(p))
                .unwrap_or(false);
            let ts_pos = raw_parts.iter().position(|p| is_discord_timestamp(p));
            let non_ts: Vec<String> = raw_parts
                .iter()
                .filter(|p| !is_discord_timestamp(p) && !is_reply_context_node(p))
                .map(|p| strip_message_noise_suffixes(p))
                .filter(|p| !p.is_empty())
                .collect();

            if ts_pos == Some(1) && non_ts.len() >= 2 {
                // Format: "<Author>, <Timestamp>, <Body...>"
                if explicit_author.is_none() {
                    explicit_author = Some(non_ts[0].clone());
                }
                body = non_ts[1..].join(", ");
            } else if !had_leading_ts && non_ts.len() >= 2 {
                // Format: "<Author>, <Body...>, <Timestamp>"
                if explicit_author.is_none() {
                    explicit_author = Some(non_ts[0].clone());
                }
                body = non_ts[1..].join(", ");
            } else if !non_ts.is_empty() {
                body = non_ts.join(", ");
            } else if !clean_desc.is_empty() {
                // Plain descendant lines without explicit timestamp (rare grouped message)
                let fallback_lines: Vec<String> = clean_desc
                    .iter()
                    .filter(|s| !is_discord_timestamp(s))
                    .map(|s| strip_message_noise_suffixes(s))
                    .filter(|s| !s.is_empty())
                    .collect();
                if !fallback_lines.is_empty() {
                    body = fallback_lines.join(" ");
                }
            }
        } else if !clean_desc.is_empty() {
            let fallback_lines: Vec<String> = clean_desc
                .iter()
                .filter(|s| !is_discord_timestamp(s))
                .map(|s| strip_message_noise_suffixes(s))
                .filter(|s| !s.is_empty())
                .collect();
            if !fallback_lines.is_empty() {
                body = fallback_lines.join(" ");
            }
        }
    }

    let body = strip_message_noise_suffixes(&body);
    if body.is_empty() {
        return None;
    }

    let key = if !automation_id.is_empty() {
        automation_id.to_string()
    } else {
        format!(
            "pos:{}:{}:{}",
            fallback_idx,
            explicit_author.as_deref().unwrap_or(""),
            body
        )
    };

    Some(ScannedMessage {
        key,
        snowflake_id,
        explicit_author,
        body,
    })
}

fn normalize_name(s: &str) -> String {
    s.trim().trim_start_matches('@').trim().to_ascii_lowercase()
}

fn is_group_dm_label(friend: &str) -> bool {
    let lower = friend.to_ascii_lowercase();
    friend.contains(',')
        || friend.contains('、')
        || friend.contains("人のメンバー")
        || lower.contains("members")
}

/// Checks whether a message author is the DM partner (and NOT self).
pub fn is_partner_author(
    author: &str,
    open_dm_friend: &str,
    self_display_name: Option<&str>,
) -> bool {
    let a = normalize_name(author);
    let f = normalize_name(open_dm_friend);
    if a.is_empty() || f.is_empty() {
        return false;
    }

    if let Some(me) = self_display_name.map(normalize_name).filter(|m| !m.is_empty()) {
        if a == me {
            return false;
        }
    }

    if a == f {
        return true;
    }

    // Handle mild nickname/status suffix differences when both names have >= 2 chars
    let a_chars = a.chars().count();
    let f_chars = f.chars().count();
    if a_chars >= 2 && f_chars >= 2 && (a.starts_with(&f) || f.starts_with(&a)) {
        return true;
    }

    // In 1-on-1 DMs, if we reliably detected self display name and author != self, it is the partner
    // (covers friend having different username vs global display name)
    if let Some(me) = self_display_name.map(normalize_name).filter(|m| !m.is_empty()) {
        if a != me && !is_group_dm_label(open_dm_friend) {
            return true;
        }
    }

    false
}

#[derive(Default)]
pub struct OpenDmTracker {
    active_friend: Option<String>,
    baseline_seeded: bool,
    last_snowflake: Option<u64>,
    seen_keys: VecDeque<String>,
    last_resolved_author: Option<String>,
}

impl OpenDmTracker {
    pub fn reset(&mut self) {
        self.active_friend = None;
        self.baseline_seeded = false;
        self.last_snowflake = None;
        self.seen_keys.clear();
        self.last_resolved_author = None;
    }

    fn record_key(&mut self, key: String) {
        if self.seen_keys.iter().any(|k| k == &key) {
            return;
        }
        while self.seen_keys.len() >= 64 {
            self.seen_keys.pop_front();
        }
        self.seen_keys.push_back(key);
    }

    /// Processes a chronological window of recent DM messages for `open_dm_friend`.
    /// Returns `(new_partner_messages, baseline_just_seeded, skipped_self_authors)`.
    pub fn update(
        &mut self,
        open_dm_friend: Option<&str>,
        self_display_name: Option<&str>,
        messages: &[ScannedMessage],
    ) -> (Vec<(String, String)>, bool, Vec<String>) {
        let Some(friend) = open_dm_friend.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            self.reset();
            return (Vec::new(), false, Vec::new());
        };

        if self.active_friend.as_deref() != Some(friend) {
            self.reset();
            self.active_friend = Some(friend.to_string());
        }

        // Resolve authors chronologically so grouped follow-up messages inherit previous author
        let mut running_author = self.last_resolved_author.clone();
        let mut resolved: Vec<(&ScannedMessage, Option<String>)> = Vec::with_capacity(messages.len());
        for m in messages {
            if let Some(ref a) = m.explicit_author {
                running_author = Some(a.clone());
            }
            resolved.push((m, running_author.clone()));
        }

        if !self.baseline_seeded {
            for (m, _) in &resolved {
                self.record_key(m.key.clone());
                if let Some(sf) = m.snowflake_id {
                    self.last_snowflake = Some(self.last_snowflake.map_or(sf, |p| p.max(sf)));
                }
            }
            if running_author.is_some() {
                self.last_resolved_author = running_author;
            }
            self.baseline_seeded = true;
            return (Vec::new(), true, Vec::new());
        }

        let mut new_partner_msgs = Vec::new();
        let mut skipped_self = Vec::new();

        for (m, author_opt) in &resolved {
            let already_seen = self.seen_keys.iter().any(|k| k == &m.key);
            if already_seen {
                continue;
            }
            if let (Some(sf), Some(last_sf)) = (m.snowflake_id, self.last_snowflake) {
                if sf <= last_sf {
                    // User scrolled up into older messages
                    self.record_key(m.key.clone());
                    continue;
                }
            }

            self.record_key(m.key.clone());
            if let Some(sf) = m.snowflake_id {
                self.last_snowflake = Some(self.last_snowflake.map_or(sf, |p| p.max(sf)));
            }

            if let Some(ref author) = author_opt {
                if is_partner_author(author, friend, self_display_name) {
                    if new_partner_msgs.len() < 5 {
                        new_partner_msgs.push((friend.to_string(), m.body.clone()));
                    }
                } else {
                    skipped_self.push(author.clone());
                }
            } else {
                // Edge case: snapshot scrolled past header so author wasn't visible, avoid guessing self
                skipped_self.push("(unknown-author)".to_string());
            }
        }

        if running_author.is_some() {
            self.last_resolved_author = running_author;
        }

        (new_partner_msgs, false, skipped_self)
    }
}

/// Polls Discord's UIA tree in the background when enabled so DMs are detected
/// even if Windows toast notifications are turned off or when the DM window is active.
pub struct DiscordUiaWatcher {
    active: Arc<AtomicBool>,
    debug_enabled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl DiscordUiaWatcher {
    pub fn spawn<F>(initially_active: bool, initially_debug: bool, on_event: F) -> Self
    where
        F: Fn(UiaDmEvent) + Send + Sync + 'static,
    {
        let active = Arc::new(AtomicBool::new(initially_active));
        let debug_enabled = Arc::new(AtomicBool::new(initially_debug));
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_bg = active.clone();
        let debug_bg = debug_enabled.clone();
        let shutdown_bg = shutdown.clone();
        let cb = Arc::new(on_event);

        thread::spawn(move || unsafe {
            let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mut initialized_badges = false;
            let mut prev_badges: HashMap<String, u32> = HashMap::new();
            let mut dm_tracker = OpenDmTracker::default();
            let mut prev_window_found = false;
            let mut last_logged_title = String::new();

            while !shutdown_bg.load(Ordering::Relaxed) {
                if !active_bg.load(Ordering::Relaxed) {
                    initialized_badges = false;
                    prev_badges.clear();
                    dm_tracker.reset();
                    prev_window_found = false;
                    last_logged_title.clear();
                    thread::sleep(Duration::from_millis(400));
                    continue;
                }

                let dbg = debug_bg.load(Ordering::Relaxed);
                match find_discord_hwnd_any() {
                    Some((hwnd, title)) => {
                        if !prev_window_found || title != last_logged_title {
                            prev_window_found = true;
                            last_logged_title = title.clone();
                            if dbg {
                                let (_, _, dm) = parse_discord_window_title(&title);
                                cb(UiaDmEvent::Debug(format!(
                                    "[UIA] Discord検出: '{}' (タイトル解析DM={:?})",
                                    title, dm
                                )));
                            }
                        }

                        if let Some(snap) = scan_discord_snapshot(hwnd, &title) {
                            let (new_msgs, baseline_seeded, skipped_self) = dm_tracker.update(
                                snap.open_dm_friend.as_deref(),
                                snap.self_display_name.as_deref(),
                                &snap.recent_messages,
                            );

                            if dbg && baseline_seeded {
                                if let Some(ref f) = snap.open_dm_friend {
                                    cb(UiaDmEvent::Debug(format!(
                                        "[UIA] DM監視開始: @{} (履歴{}件, self={:?})",
                                        f,
                                        snap.recent_messages.len(),
                                        snap.self_display_name
                                    )));
                                }
                            }

                            if dbg {
                                for author in skipped_self {
                                    cb(UiaDmEvent::Debug(format!(
                                        "[UIA] 自分の発言/判定外をスキップ: {}",
                                        author
                                    )));
                                }
                            }

                            for (sender, body) in new_msgs {
                                if dbg {
                                    cb(UiaDmEvent::Debug(format!(
                                        "[UIA] DM新着検知: {}: {}",
                                        sender, body
                                    )));
                                }
                                cb(UiaDmEvent::OpenDmMessage { sender, body });
                            }

                            if !initialized_badges {
                                prev_badges = snap.unread_badges;
                                initialized_badges = true;
                            } else {
                                for (sender, &count) in &snap.unread_badges {
                                    let prev_count = prev_badges.get(sender).copied().unwrap_or(0);
                                    if count > prev_count {
                                        if dbg {
                                            cb(UiaDmEvent::Debug(format!(
                                                "[UIA] 未読DMバッジ増加: {} ({} -> {})",
                                                sender, prev_count, count
                                            )));
                                        }
                                        cb(UiaDmEvent::UnreadBadge {
                                            sender: sender.clone(),
                                            count,
                                        });
                                    }
                                }
                                prev_badges = snap.unread_badges;
                            }
                        }
                    }
                    None => {
                        if prev_window_found && dbg {
                            cb(UiaDmEvent::Debug(
                                "[UIA] Discord ウィンドウが見つかりません".into(),
                            ));
                        }
                        prev_window_found = false;
                        last_logged_title.clear();
                    }
                }

                thread::sleep(Duration::from_millis(900));
            }

            if com_inited {
                CoUninitialize();
            }
        });

        Self {
            active,
            debug_enabled,
            shutdown,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.active.store(enabled, Ordering::Relaxed);
    }

    pub fn set_debug_enabled(&self, enabled: bool) {
        self.debug_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn shutdown(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_discord_unread_dm_badges_without_false_positives() {
        assert_eq!(
            parse_unread_dm_badge("Taro, 1件の未読メッセージ", false),
            Some(("Taro".into(), 1))
        );
        assert_eq!(
            parse_unread_dm_badge("alice, 3 unread messages", false),
            Some(("alice".into(), 3))
        );
        assert_eq!(
            parse_unread_dm_badge("Hanako, 未読, ダイレクトメッセージ", true),
            Some(("Hanako".into(), 1))
        );
        // Server channels & unread divider banners must be ignored
        assert_eq!(
            parse_unread_dm_badge("雑談（テキストチャンネル）, 2件の未読メッセージ", false),
            None
        );
        assert_eq!(parse_unread_dm_badge("3件の未読メッセージ", false), None);
        // Online friend without unread status must be ignored
        assert_eq!(
            parse_unread_dm_badge("Taro, オンライン, ダイレクトメッセージ", true),
            None
        );
    }

    #[test]
    fn extracts_dm_friend_from_messages_area_and_excludes_server_channels() {
        assert_eq!(
            extract_dm_friend_from_messages_area("@かげろー でのメッセージ"),
            Some("かげろー".into())
        );
        assert_eq!(
            extract_dm_friend_from_messages_area("かげろー でのメッセージ"),
            Some("かげろー".into())
        );
        assert_eq!(
            extract_dm_friend_from_messages_area("Messages in @suicune"),
            Some("suicune".into())
        );
        assert_eq!(extract_dm_friend_from_messages_area("#雑談 でのメッセージ"), None);
        assert_eq!(extract_dm_friend_from_messages_area("Messages in #general"), None);
    }

    #[test]
    fn scores_visible_main_discord_window_above_hidden_helpers() {
        let big = RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 900,
        };
        let zero = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let s_main = score_discord_window_candidate(true, false, big, "Discord | @かげろー");
        let s_helper = score_discord_window_candidate(false, false, zero, "Discord");
        assert!(s_main > s_helper);
    }

    #[test]
    fn parses_grouped_and_descendant_discord_messages() {
        let m1 = parse_scanned_message(
            "chat-messages-100-100000000000000001",
            "",
            &["かげろー".into(), "今日 22:56".into(), "強そう".into()],
            0,
        )
        .unwrap();
        assert_eq!(m1.explicit_author.as_deref(), Some("かげろー"));
        assert_eq!(m1.body, "強そう");
        assert_eq!(m1.snowflake_id, Some(100000000000000001));

        // Grouped follow-up message: only timestamp + text in descendants!
        let m2 = parse_scanned_message(
            "chat-messages-100-100000000000000002",
            "",
            &["22:56".into(), "そっちはどう？（編集済み）".into()],
            1,
        )
        .unwrap();
        assert_eq!(m2.explicit_author, None);
        assert_eq!(m2.body, "そっちはどう？");

        // Aria name fallback format
        let m3 = parse_scanned_message(
            "chat-messages-100-100000000000000003",
            "かげろー, 了解！, 22:58",
            &[],
            2,
        )
        .unwrap();
        assert_eq!(m3.explicit_author.as_deref(), Some("かげろー"));
        assert_eq!(m3.body, "了解！");
    }

    #[test]
    fn open_dm_tracker_tracks_grouped_multi_turn_and_filters_self() {
        let mut tracker = OpenDmTracker::default();
        let initial = vec![
            parse_scanned_message(
                "chat-messages-1-100000000000000001",
                "",
                &["RInfa".into(), "22:50".into(), "画像貼るね".into()],
                0,
            )
            .unwrap(),
            parse_scanned_message(
                "chat-messages-1-100000000000000002",
                "",
                &["かげろー".into(), "22:56".into(), "強そう".into()],
                1,
            )
            .unwrap(),
        ];

        // Baseline must NOT replay existing history
        let (out0, seeded, _) = tracker.update(Some("かげろー"), Some("RInfa"), &initial);
        assert!(seeded);
        assert!(out0.is_empty());

        // Turn 2: Partner sends a grouped follow-up message + an identical repeat later
        let mut turn2 = initial.clone();
        turn2.push(
            parse_scanned_message(
                "chat-messages-1-100000000000000003",
                "",
                &["22:57".into(), "それどこの装備？".into()],
                2,
            )
            .unwrap(),
        );
        let (out1, seeded1, _) = tracker.update(Some("かげろー"), Some("RInfa"), &turn2);
        assert!(!seeded1);
        assert_eq!(out1, vec![("かげろー".into(), "それどこの装備？".into())]);

        // Turn 3: Self replies -> must NOT read own message aloud!
        turn2.push(
            parse_scanned_message(
                "chat-messages-1-100000000000000004",
                "",
                &["RInfa".into(), "22:58".into(), "ありがとう！".into()],
                3,
            )
            .unwrap(),
        );
        let (out2, _, skipped) = tracker.update(Some("かげろー"), Some("RInfa"), &turn2);
        assert!(out2.is_empty());
        assert_eq!(skipped, vec!["RInfa".to_string()]);

        // Turn 4: Partner sends two rapid messages (even identical text "うん" with new snowflake!)
        turn2.push(
            parse_scanned_message(
                "chat-messages-1-100000000000000005",
                "",
                &["かげろー".into(), "22:59".into(), "うん".into()],
                4,
            )
            .unwrap(),
        );
        turn2.push(
            parse_scanned_message(
                "chat-messages-1-100000000000000006",
                "",
                &["22:59".into(), "うん".into()],
                5,
            )
            .unwrap(),
        );
        let (out3, _, _) = tracker.update(Some("かげろー"), Some("RInfa"), &turn2);
        assert_eq!(
            out3,
            vec![
                ("かげろー".into(), "うん".into()),
                ("かげろー".into(), "うん".into()),
            ]
        );
    }
}
