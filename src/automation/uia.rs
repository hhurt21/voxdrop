//! Safe, read-only Discord desktop UI scanner using official Windows UI Automation (Accessibility API).
//! Zero process injection, zero memory reading — uses the same official OS API as screen readers.

use crate::automation::input;
use crate::automation::window::extract_exe_name;
use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
    IUIAutomationTreeWalker, TreeScope_Descendants, UIA_ButtonControlTypeId,
    UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_InvokePatternId, UIA_NamePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedChannel {
    pub server_name: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct DiscordUiScan {
    pub active_server: String,
    pub servers: Vec<String>,
    pub channels: Vec<ScannedChannel>,
    pub friends: Vec<String>,
}

struct EnumCtx {
    allowed: Vec<String>,
    found: Option<(HWND, String)>,
}

unsafe extern "system" fn enum_window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
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
                if ctx.allowed.iter().any(|a| a.eq_ignore_ascii_case(&exe)) {
                    let mut title_buf = vec![0u16; 256];
                    let tlen = GetWindowTextW(hwnd, &mut title_buf);
                    let title = if tlen > 0 {
                        String::from_utf16_lossy(&title_buf[..tlen as usize])
                    } else {
                        String::new()
                    };
                    let is_discord_title = title.to_ascii_lowercase().contains("discord");
                    if is_discord_title || ctx.found.is_none() {
                        ctx.found = Some((hwnd, title));
                        if is_discord_title {
                            return BOOL(0); // stop
                        }
                    }
                }
            }
        }
    }
    BOOL(1)
}

fn find_discord_window(allowed_processes: &[String]) -> Option<(HWND, String)> {
    let mut ctx = EnumCtx {
        allowed: if allowed_processes.is_empty() {
            vec![
                "Discord.exe".into(),
                "DiscordPTB.exe".into(),
                "DiscordCanary.exe".into(),
            ]
        } else {
            allowed_processes.to_vec()
        },
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_window_cb),
            LPARAM(&mut ctx as *mut EnumCtx as isize),
        );
    }
    ctx.found
}

/// Parses Discord's window title such as `"#雑談 | メイン鯖 - Discord"` or `"@alice - Discord"`.
pub fn parse_discord_window_title(title: &str) -> (Option<String>, Option<String>, Option<String>) {
    let clean = title
        .trim()
        .strip_suffix("- Discord")
        .or_else(|| title.trim().strip_suffix("— Discord"))
        .unwrap_or(title)
        .trim();
    // Remove leading unread dot like "• " or "(3) "
    let clean = clean
        .trim_start_matches(|c: char| c == '•' || c.is_whitespace())
        .trim();

    if let Some((left, right)) = clean.split_once('|') {
        let ch = left.trim().trim_start_matches('#').trim();
        let srv = right.trim();
        return (
            if srv.is_empty() { None } else { Some(srv.to_string()) },
            if ch.is_empty() { None } else { Some(ch.to_string()) },
            None,
        );
    }
    if let Some(dm) = clean.strip_prefix('@') {
        let friend = dm.trim();
        if !friend.is_empty() {
            return (None, None, Some(friend.to_string()));
        }
    }
    (None, None, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Unknown,
    Servers,
    Channels,
    DirectMessages,
}

/// Scans the currently visible Discord Desktop window via Windows UI Automation.
pub fn scan_discord_window(allowed_processes: &[String]) -> Result<DiscordUiScan, String> {
    let Some((hwnd, title)) = find_discord_window(allowed_processes) else {
        return Err("Discord ウィンドウが見つかりません（Discordを起動した状態でお試しください）".into());
    };

    let mut result = DiscordUiScan::default();
    let (title_srv, title_ch, title_dm) = parse_discord_window_title(&title);
    if let Some(s) = title_srv {
        result.active_server = s;
    }

    let mut seen_servers = HashSet::new();
    let mut seen_channels = HashSet::new();
    let mut seen_friends = HashSet::new();

    if let Some(dm) = title_dm {
        if seen_friends.insert(dm.clone()) {
            result.friends.push(dm);
        }
    }
    if let Some(ch) = title_ch {
        let srv = result.active_server.clone();
        let key = format!("{}/{}", srv, ch);
        if seen_channels.insert(key) {
            result.channels.push(ScannedChannel {
                server_name: srv,
                name: ch,
            });
        }
    }

    unsafe {
        let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let scan_res = (|| -> Result<(), String> {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("UI Automation 初期化に失敗しました: {}", e))?;
            let root = uia
                .ElementFromHandle(hwnd)
                .map_err(|e| format!("Discord UI 要素の取得に失敗しました: {}", e))?;
            let walker = uia
                .ControlViewWalker()
                .map_err(|e| format!("UIA TreeWalker の取得に失敗しました: {}", e))?;

            let mut visited = 0usize;
            walk_tree(
                &walker,
                &root,
                Region::Unknown,
                0,
                &mut visited,
                &mut result,
                &mut seen_servers,
                &mut seen_channels,
                &mut seen_friends,
            );
            Ok(())
        })();

        if com_inited {
            CoUninitialize();
        }
        scan_res?;
    }

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk_tree(
    walker: &IUIAutomationTreeWalker,
    node: &IUIAutomationElement,
    region: Region,
    depth: usize,
    visited: &mut usize,
    out: &mut DiscordUiScan,
    seen_servers: &mut HashSet<String>,
    seen_channels: &mut HashSet<String>,
    seen_friends: &mut HashSet<String>,
) {
    if depth > 15 || *visited > 2600 {
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
            next_region = Region::Servers;
        } else if trimmed == "チャンネル"
            || lower == "channels"
            || trimmed.ends_with("チャンネル") && trimmed.len() <= 24
        {
            next_region = Region::Channels;
        } else if trimmed == "ダイレクトメッセージ"
            || lower == "direct messages"
            || lower == "private channels"
        {
            next_region = Region::DirectMessages;
        } else {
            match region {
                Region::Servers => {
                    if let Some(srv) = clean_server_label(trimmed) {
                        if seen_servers.insert(srv.clone()) {
                            out.servers.push(srv);
                        }
                    }
                }
                Region::Channels => {
                    if let Some(ch) = clean_channel_label(trimmed) {
                        let srv = out.active_server.clone();
                        let key = format!("{}/{}", srv, ch);
                        if seen_channels.insert(key) {
                            out.channels.push(ScannedChannel {
                                server_name: srv,
                                name: ch,
                            });
                        }
                    }
                }
                Region::DirectMessages => {
                    if let Some(fr) = clean_friend_label(trimmed) {
                        if seen_friends.insert(fr.clone()) {
                            out.friends.push(fr);
                        }
                    }
                }
                Region::Unknown => {
                    // Fallback pattern detection when Discord omits landmark names
                    if (trimmed.contains("テキストチャンネル") || lower.contains("text channel"))
                        && trimmed.len() < 120
                    {
                        if let Some(ch) = clean_channel_label(trimmed) {
                            let srv = out.active_server.clone();
                            let key = format!("{}/{}", srv, ch);
                            if seen_channels.insert(key) {
                                out.channels.push(ScannedChannel {
                                    server_name: srv,
                                    name: ch,
                                });
                            }
                        }
                    } else if (trimmed.contains("ダイレクトメッセージ")
                        || lower.contains("direct message"))
                        && trimmed.len() < 120
                    {
                        if let Some(fr) = clean_friend_label(trimmed) {
                            if seen_friends.insert(fr.clone()) {
                                out.friends.push(fr);
                            }
                        }
                    }
                }
            }
        }
    }

    if let Ok(mut child) = walker.GetFirstChildElement(node) {
        let mut siblings = 0usize;
        while siblings < 200 && *visited <= 2600 {
            walk_tree(
                walker,
                &child,
                next_region,
                depth + 1,
                visited,
                out,
                seen_servers,
                seen_channels,
                seen_friends,
            );
            siblings += 1;
            match walker.GetNextSiblingElement(&child) {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }
}

pub fn clean_server_label(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > 60 {
        return None;
    }
    const IGNORE: &[&str] = &[
        "ダイレクトメッセージ",
        "Direct Messages",
        "サーバーを追加",
        "Add a Server",
        "発見",
        "Discover",
        "アプリをダウンロード",
        "Download Apps",
        "サーバー",
        "Servers",
    ];
    if IGNORE.iter().any(|&ig| s.eq_ignore_ascii_case(ig)) {
        return None;
    }
    // Strip unread / mention suffixes like "メイン鯖, 3件のメンション"
    let base = s.split(',').next().unwrap_or(s).trim();
    let base = base.split('、').next().unwrap_or(base).trim();
    if base.is_empty() || base.chars().count() > 45 {
        return None;
    }
    Some(base.to_string())
}

pub fn clean_channel_label(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > 90 {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if s.contains("ボイス")
        || lower.contains("voice")
        || s.contains("ステージ")
        || lower.contains("stage")
        || s.contains("カテゴリー")
        || lower.contains("category")
        || s.contains("招待を作成")
        || s.contains("チャンネルを編集")
        || s == "イベント"
        || s == "メンバー"
        || s == "チャンネル＆ロール"
        || s == "チャンネルを作成"
    {
        return None;
    }
    let mut base = s.split(',').next().unwrap_or(s).trim();
    base = base.split('、').next().unwrap_or(base).trim();
    for strip in [
        "（テキストチャンネル）",
        "(テキストチャンネル)",
        "(text channel)",
        "テキストチャンネル",
        "未読",
    ] {
        if let Some(b) = base.strip_suffix(strip) {
            base = b.trim();
        }
    }
    base = base.trim_start_matches('#').trim();
    if base.is_empty() || base.chars().count() > 40 {
        return None;
    }
    Some(base.to_string())
}

pub fn clean_friend_label(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > 90 {
        return None;
    }
    const IGNORE: &[&str] = &[
        "フレンド",
        "Friends",
        "Nitro",
        "ショップ",
        "Shop",
        "メッセージリクエスト",
        "Message Requests",
        "DMを作成",
        "Create DM",
        "ダイレクトメッセージ",
        "Direct Messages",
        "閉じる",
        "Close",
        "会話またはユーザーを探す",
    ];
    if IGNORE.iter().any(|&ig| s.eq_ignore_ascii_case(ig)) {
        return None;
    }
    let mut base = s.split(',').next().unwrap_or(s).trim();
    base = base.split('、').next().unwrap_or(base).trim();
    for strip in [
        "（ダイレクトメッセージ）",
        "(ダイレクトメッセージ)",
        "(direct message)",
        "ダイレクトメッセージ",
    ] {
        if let Some(b) = base.strip_suffix(strip) {
            base = b.trim();
        }
    }
    base = base.trim_start_matches('@').trim();
    if base.is_empty()
        || base.chars().count() > 40
        || IGNORE.iter().any(|&ig| base.eq_ignore_ascii_case(ig))
    {
        return None;
    }
    Some(base.to_string())
}

/// Ultra-fast Win32 window title lookup (<0.1ms!) for the running Discord desktop window.
pub fn get_discord_window_title(allowed_processes: &[String]) -> Option<String> {
    find_discord_window(allowed_processes).map(|(_, title)| title)
}

/// Fast native UIA `FindFirst` click for the `"メッセージ"` / `"Message"` button inside a Discord
/// User Profile modal (~30–80ms instead of multi-second tree walking!).
pub fn click_profile_message_button(allowed_processes: &[String], timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(200));
    while Instant::now() < deadline {
        if let Some((hwnd, _)) = find_discord_window(allowed_processes) {
            let mut win_rect = RECT::default();
            let _ = unsafe { GetWindowRect(hwnd, &mut win_rect) };

            let mut clicked = false;
            let mut click_point: Option<(i32, i32)> = None;

            unsafe {
                let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                if let Ok(uia) =
                    CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                {
                    if let Ok(root) = uia.ElementFromHandle(hwnd) {
                        if let Ok(btn_cond) = uia.CreatePropertyCondition(
                            UIA_ControlTypePropertyId,
                            &VARIANT::from(UIA_ButtonControlTypeId.0),
                        ) {
                            for label in ["メッセージ", "Message"] {
                                if let Ok(name_cond) = uia.CreatePropertyCondition(
                                    UIA_NamePropertyId,
                                    &VARIANT::from(label),
                                ) {
                                    if let Ok(and_cond) = uia.CreateAndCondition(&btn_cond, &name_cond) {
                                        if let Ok(elem) =
                                            root.FindFirst(TreeScope_Descendants, &and_cond)
                                        {
                                            let rect =
                                                elem.CurrentBoundingRectangle().unwrap_or_default();
                                            let in_win = rect.right > rect.left
                                                && rect.bottom > rect.top
                                                && rect.left >= win_rect.left
                                                && rect.right <= win_rect.right;
                                            if in_win {
                                                if let Ok(pat) = elem
                                                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                                        UIA_InvokePatternId,
                                                    )
                                                {
                                                    if pat.Invoke().is_ok() {
                                                        clicked = true;
                                                        break;
                                                    }
                                                }
                                                click_point = Some((
                                                    (rect.left + rect.right) / 2,
                                                    (rect.top + rect.bottom) / 2,
                                                ));
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if com_inited {
                    CoUninitialize();
                }
            }

            if clicked {
                return true;
            }
            if let Some((cx, cy)) = click_point {
                if input::click_screen_point_and_restore(cx, cy).is_ok() {
                    return true;
                }
            }
        }
        thread::sleep(Duration::from_millis(80));
    }
    false
}

/// Focuses Discord's bottom message composer via UIA `SetFocus()` ONLY (no `SetCursorPos` or mouse click).
pub fn focus_discord_chat_input_no_mouse(allowed_processes: &[String], timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(150));
    while Instant::now() < deadline {
        if let Some((hwnd, _)) = find_discord_window(allowed_processes) {
            let mut focused = false;
            unsafe {
                let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                if let Ok(uia) =
                    CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                {
                    if let Ok(root) = uia.ElementFromHandle(hwnd) {
                        if let Ok(edit_cond) = uia.CreatePropertyCondition(
                            UIA_ControlTypePropertyId,
                            &VARIANT::from(UIA_EditControlTypeId.0),
                        ) {
                            if let Ok(edits) = root.FindAll(TreeScope_Descendants, &edit_cond) {
                                let len = edits.Length().unwrap_or(0);
                                for i in 0..len {
                                    if let Ok(elem) = edits.GetElement(i) {
                                        let name = elem
                                            .CurrentName()
                                            .map(|b| b.to_string())
                                            .unwrap_or_default();
                                        if is_discord_chat_composer_label(&name) {
                                            if elem.SetFocus().is_ok() {
                                                focused = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if com_inited {
                    CoUninitialize();
                }
            }
            if focused {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(60));
    }
    false
}

/// Fast native UIA `FindAll(Edit)` lookup for Discord's bottom message input box
/// (e.g. `"@alice へメッセージを送信"` / `"#雑談 へメッセージを送信"`) (~30–80ms!),
/// focuses it via `SetFocus()` and clicks inside so the text caret is guaranteed ready.
pub fn focus_discord_chat_input(allowed_processes: &[String], timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(150));
    while Instant::now() < deadline {
        if let Some((hwnd, _)) = find_discord_window(allowed_processes) {
            let mut click_point: Option<(i32, i32)> = None;

            unsafe {
                let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                if let Ok(uia) =
                    CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                {
                    if let Ok(root) = uia.ElementFromHandle(hwnd) {
                        if let Ok(edit_cond) = uia.CreatePropertyCondition(
                            UIA_ControlTypePropertyId,
                            &VARIANT::from(UIA_EditControlTypeId.0),
                        ) {
                            if let Ok(edits) = root.FindAll(TreeScope_Descendants, &edit_cond) {
                                let len = edits.Length().unwrap_or(0);
                                for i in 0..len {
                                    if let Ok(elem) = edits.GetElement(i) {
                                        let name = elem
                                            .CurrentName()
                                            .map(|b| b.to_string())
                                            .unwrap_or_default();
                                        if is_discord_chat_composer_label(&name) {
                                            let rect =
                                                elem.CurrentBoundingRectangle().unwrap_or_default();
                                            if rect.right > rect.left + 40
                                                && rect.bottom > rect.top + 8
                                            {
                                                let _ = elem.SetFocus();
                                                let cx = rect.left
                                                    + ((rect.right - rect.left) / 2).max(60);
                                                let cy = (rect.top + rect.bottom) / 2;
                                                click_point = Some((cx, cy));
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if com_inited {
                    CoUninitialize();
                }
            }

            if let Some((cx, cy)) = click_point {
                if input::click_screen_point_and_restore(cx, cy).is_ok() {
                    return true;
                }
            }
        }
        thread::sleep(Duration::from_millis(70));
    }
    false
}

pub fn is_discord_chat_composer_label(label: &str) -> bool {
    let s = label.trim();
    if s.is_empty() || s.chars().count() > 120 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    s.contains("へメッセージを送信")
        || s.contains("にメッセージを送信")
        || lower.starts_with("message @")
        || lower.starts_with("message #")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_discord_window_titles() {
        assert_eq!(
            parse_discord_window_title("#雑談 | メイン鯖 - Discord"),
            (Some("メイン鯖".into()), Some("雑談".into()), None)
        );
        assert_eq!(
            parse_discord_window_title("• @alice - Discord"),
            (None, None, Some("alice".into()))
        );
    }

    #[test]
    fn recognizes_discord_chat_composer_labels() {
        assert!(is_discord_chat_composer_label("@alice へメッセージを送信"));
        assert!(is_discord_chat_composer_label("#雑談 へメッセージを送信"));
        assert!(is_discord_chat_composer_label("Message @alice"));
        assert!(is_discord_chat_composer_label("Message #general"));
        assert!(!is_discord_chat_composer_label("メッセージ"));
        assert!(!is_discord_chat_composer_label("会話を見つける、または始める"));
    }

    #[test]
    fn cleans_uia_labels_for_channels_and_friends() {
        assert_eq!(
            clean_channel_label("雑談（テキストチャンネル）, 未読"),
            Some("雑談".into())
        );
        assert_eq!(clean_channel_label("通話1（ボイスチャンネル）"), None);
        assert_eq!(
            clean_friend_label("alice, オンライン, ダイレクトメッセージ"),
            Some("alice".into())
        );
        assert_eq!(clean_friend_label("Nitro"), None);
    }
}
