//! Background Windows UI Automation Discord DM watcher.
//! Detects new / unread Discord DMs directly from the Discord desktop window's accessibility tree
//! so incoming DMs are read aloud even when Windows notifications or Discord desktop banners are OFF.
//! Uses only official Windows UI Automation (`IUIAutomation`) — zero process injection or memory access.

use crate::automation::uia::{clean_friend_label, parse_discord_window_title};
use crate::automation::window::extract_exe_name;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    UIA_ListItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
};

#[derive(Debug, Clone)]
pub enum UiaDmEvent {
    /// New message observed directly in the currently open DM conversation
    OpenDmMessage { sender: String, body: String },
    /// Unread DM badge increased or newly appeared in Discord's sidebar
    UnreadBadge { sender: String, count: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanRegion {
    Unknown,
    ServersOrDmRail,
    DirectMessagesList,
    MessagesArea,
}

#[derive(Default)]
struct UiaScanSnapshot {
    open_dm_friend: Option<String>,
    unread_badges: HashMap<String, u32>,
    last_message_text: Option<String>,
}

struct DiscordHwndCtx {
    found: Option<(HWND, String)>,
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
                    let mut title_buf = vec![0u16; 256];
                    let tlen = GetWindowTextW(hwnd, &mut title_buf);
                    let title = if tlen > 0 {
                        String::from_utf16_lossy(&title_buf[..tlen as usize])
                            .trim()
                            .to_string()
                    } else {
                        String::new()
                    };
                    if title.to_ascii_lowercase().contains("discord") {
                        ctx.found = Some((hwnd, title));
                        return BOOL(0);
                    }
                }
            }
        }
    }
    BOOL(1)
}

fn find_discord_hwnd_any() -> Option<(HWND, String)> {
    let mut ctx = DiscordHwndCtx { found: None };
    unsafe {
        let _ = EnumWindows(
            Some(find_any_discord_cb),
            LPARAM(&mut ctx as *mut DiscordHwndCtx as isize),
        );
    }
    ctx.found
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

    // Must explicitly indicate unread / mention status
    let has_dm_unread_marker = trimmed.contains("件の未読メッセージ")
        || lower.contains("unread message")
        || (in_dm_list && (trimmed.contains("未読") || lower.contains("unread")))
        || (trimmed.contains("ダイレクトメッセージ") && (trimmed.contains("未読") || trimmed.contains("件のメンション")))
        || (lower.contains("direct message") && (lower.contains("unread") || lower.contains("mention")));

    if !has_dm_unread_marker {
        return None;
    }

    // Ignore server channels or voice channels
    if trimmed.contains("テキストチャンネル")
        || trimmed.contains("ボイスチャンネル")
        || lower.contains("text channel")
        || lower.contains("voice channel")
        || trimmed.starts_with('#')
    {
        return None;
    }

    // Extract numeric count if present
    let mut count = 1u32;
    for part in trimmed.split([',', '、']) {
        let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && (part.contains("件") || part.to_ascii_lowercase().contains("unread") || part.to_ascii_lowercase().contains("mention")) {
            if let Ok(n) = digits.parse::<u32>() {
                if n > 0 && n < 10000 {
                    count = n;
                    break;
                }
            }
        }
    }

    let sender = clean_friend_label(trimmed)?;
    if sender.is_empty() {
        return None;
    }
    Some((sender, count))
}

unsafe fn scan_discord_snapshot(hwnd: HWND, title: &str) -> Option<UiaScanSnapshot> {
    let uia: IUIAutomation =
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let root = uia.ElementFromHandle(hwnd).ok()?;
    let walker = uia.ControlViewWalker().ok()?;

    let mut snap = UiaScanSnapshot::default();
    let (_, _, dm_friend) = parse_discord_window_title(title);
    snap.open_dm_friend = dm_friend;

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
    if depth > 13 || *visited > 1300 {
        return;
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
        if trimmed == "サーバー" || lower == "servers" || lower.contains("servers sidebar") {
            next_region = ScanRegion::ServersOrDmRail;
        } else if trimmed == "ダイレクトメッセージ"
            || lower == "direct messages"
            || lower == "private channels"
        {
            next_region = ScanRegion::DirectMessagesList;
        } else if (trimmed.contains("でのメッセージ") || lower.starts_with("messages in @"))
            && trimmed.chars().count() <= 80
        {
            next_region = ScanRegion::MessagesArea;
        } else {
            match region {
                ScanRegion::ServersOrDmRail => {
                    if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, false) {
                        let entry = snap.unread_badges.entry(sender).or_insert(0);
                        *entry = (*entry).max(cnt);
                    }
                }
                ScanRegion::DirectMessagesList => {
                    if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, true) {
                        let entry = snap.unread_badges.entry(sender).or_insert(0);
                        *entry = (*entry).max(cnt);
                    }
                }
                ScanRegion::MessagesArea => {
                    if let Ok(ct) = node.CurrentControlType() {
                        if ct == UIA_ListItemControlTypeId && trimmed.chars().count() >= 2 {
                            snap.last_message_text = Some(trimmed.to_string());
                        }
                    }
                }
                ScanRegion::Unknown => {
                    if let Some((sender, cnt)) = parse_unread_dm_badge(trimmed, false) {
                        let entry = snap.unread_badges.entry(sender).or_insert(0);
                        *entry = (*entry).max(cnt);
                    }
                }
            }
        }
    }

    if let Ok(mut child) = walker.GetFirstChildElement(node) {
        let mut siblings = 0usize;
        while siblings < 120 && *visited <= 1300 {
            walk_snapshot(walker, &child, next_region, depth + 1, visited, snap);
            siblings += 1;
            match walker.GetNextSiblingElement(&child) {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }
}

/// Polls Discord's UIA tree in the background when enabled so DMs are detected
/// even if Windows toast notifications are turned off.
pub struct DiscordUiaWatcher {
    active: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl DiscordUiaWatcher {
    pub fn spawn<F>(initially_active: bool, on_event: F) -> Self
    where
        F: Fn(UiaDmEvent) + Send + Sync + 'static,
    {
        let active = Arc::new(AtomicBool::new(initially_active));
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_bg = active.clone();
        let shutdown_bg = shutdown.clone();
        let cb = Arc::new(on_event);

        thread::spawn(move || unsafe {
            let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mut initialized_baseline = false;
            let mut prev_badges: HashMap<String, u32> = HashMap::new();
            let mut prev_open_dm: Option<String> = None;
            let mut prev_last_msg: Option<String> = None;

            while !shutdown_bg.load(Ordering::Relaxed) {
                if !active_bg.load(Ordering::Relaxed) {
                    initialized_baseline = false;
                    prev_badges.clear();
                    prev_open_dm = None;
                    prev_last_msg = None;
                    thread::sleep(Duration::from_millis(400));
                    continue;
                }

                if let Some((hwnd, title)) = find_discord_hwnd_any() {
                    if let Some(snap) = scan_discord_snapshot(hwnd, &title) {
                        if !initialized_baseline {
                            prev_badges = snap.unread_badges;
                            prev_open_dm = snap.open_dm_friend;
                            prev_last_msg = snap.last_message_text;
                            initialized_baseline = true;
                        } else {
                            // 1. Check live message in currently open DM view
                            if snap.open_dm_friend == prev_open_dm {
                                if let (Some(ref friend), Some(ref msg)) =
                                    (&snap.open_dm_friend, &snap.last_message_text)
                                {
                                    if prev_last_msg.as_deref() != Some(msg.as_str()) {
                                        // Only speak if the message looks authored by the DM partner
                                        // (Discord ListItem labels start with sender display name)
                                        let msg_lower = msg.to_ascii_lowercase();
                                        let friend_lower = friend.to_ascii_lowercase();
                                        if msg.starts_with(friend.as_str())
                                            || msg_lower.starts_with(&friend_lower)
                                        {
                                            let body = msg
                                                .get(friend.len()..)
                                                .unwrap_or(msg)
                                                .trim_start_matches([':', '：', ',', '、', ' '])
                                                .trim()
                                                .to_string();
                                            if !body.is_empty() {
                                                cb(UiaDmEvent::OpenDmMessage {
                                                    sender: friend.clone(),
                                                    body,
                                                });
                                            }
                                        }
                                    }
                                }
                            }

                            // 2. Check unread DM badge increases in sidebar/DM rail
                            for (sender, &count) in &snap.unread_badges {
                                let prev_count = prev_badges.get(sender).copied().unwrap_or(0);
                                if count > prev_count {
                                    cb(UiaDmEvent::UnreadBadge {
                                        sender: sender.clone(),
                                        count,
                                    });
                                }
                            }

                            prev_badges = snap.unread_badges;
                            prev_open_dm = snap.open_dm_friend;
                            prev_last_msg = snap.last_message_text;
                        }
                    }
                }

                thread::sleep(Duration::from_millis(1500));
            }

            if com_inited {
                CoUninitialize();
            }
        });

        Self { active, shutdown }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.active.store(enabled, Ordering::Relaxed);
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
        // Server channels must be ignored
        assert_eq!(
            parse_unread_dm_badge("雑談（テキストチャンネル）, 2件の未読メッセージ", false),
            None
        );
        // Online friend without unread status must be ignored
        assert_eq!(
            parse_unread_dm_badge("Taro, オンライン, ダイレクトメッセージ", true),
            None
        );
    }
}
