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
    IUIAutomationTextPattern, IUIAutomationTogglePattern, IUIAutomationTreeWalker,
    IUIAutomationValuePattern, ToggleState_On, TreeScope_Descendants, UIA_ButtonControlTypeId,
    UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_InvokePatternId, UIA_NamePropertyId,
    UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
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

pub fn is_excluded_edit_label(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    ["メモ", "note", "検索", "search", "ステータス", "status", "プロフィール", "profile"]
        .iter()
        .any(|kw| lower.contains(kw))
}

pub fn score_composer_candidate(label: &str, elem_rect: RECT, win_rect: RECT) -> Option<i32> {
    if is_excluded_edit_label(label) {
        return None;
    }
    let win_w = win_rect.right - win_rect.left;
    let win_h = win_rect.bottom - win_rect.top;
    let elem_w = elem_rect.right - elem_rect.left;
    let elem_h = elem_rect.bottom - elem_rect.top;
    if elem_w < 80 || elem_h < 12 || win_w <= 0 || win_h <= 0 {
        return None;
    }
    // Exclude top search bar / header controls
    if elem_rect.top < win_rect.top + (win_h / 4) {
        return None;
    }
    // Exclude narrow right-sidebar profile note fields unless wide enough to be main composer
    let in_right_sidebar = elem_rect.left > win_rect.left + (win_w * 72) / 100 && elem_w < (win_w * 35) / 100;
    if in_right_sidebar {
        return None;
    }

    let mut score = if is_discord_chat_composer_label(label) { 100 } else { 30 };
    if elem_rect.top >= win_rect.top + (win_h / 2) {
        score += 20;
    }
    if elem_w >= (win_w * 35) / 100 {
        score += 10;
    }
    Some(score)
}

fn find_best_composer_element(
    root: &IUIAutomationElement,
    uia: &IUIAutomation,
    win_rect: RECT,
) -> Option<(IUIAutomationElement, RECT)> {
    let edit_cond = unsafe {
        uia.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_EditControlTypeId.0),
        )
        .ok()?
    };
    let edits = unsafe { root.FindAll(TreeScope_Descendants, &edit_cond).ok()? };
    let len = unsafe { edits.Length().unwrap_or(0) };
    let mut best: Option<(i32, IUIAutomationElement, RECT)> = None;
    for i in 0..len {
        if let Ok(elem) = unsafe { edits.GetElement(i) } {
            let name = unsafe { elem.CurrentName().map(|b| b.to_string()).unwrap_or_default() };
            let rect = unsafe { elem.CurrentBoundingRectangle().unwrap_or_default() };
            if let Some(score) = score_composer_candidate(&name, rect, win_rect) {
                if best.as_ref().map(|(bs, _, _)| score > *bs).unwrap_or(true) {
                    best = Some((score, elem, rect));
                }
            }
        }
    }
    best.map(|(_, elem, rect)| (elem, rect))
}

/// Zero-mouse User Profile modal recovery for PostMessage / anti-cheat-safe flows (Section 2.2):
/// Invokes "メッセージ / メッセージを送る / Message" button via UIA InvokePattern only; if obstructed by a
/// profile popout, sends PostMessage(VK_ESCAPE) and retries up to 2 times.
pub fn recover_from_profile_modal_no_mouse(
    allowed_processes: &[String],
    render_hwnd: Option<HWND>,
    timeout_ms: u64,
) -> Result<bool, &'static str> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(250));
    let mut esc_attempts = 0u8;

    while Instant::now() < deadline {
        if let Some((top_hwnd, _)) = find_discord_window(allowed_processes) {
            let mut win_rect = RECT::default();
            let _ = unsafe { GetWindowRect(top_hwnd, &mut win_rect) };

            let mut invoked = false;
            let mut has_main_composer = false;
            unsafe {
                let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                if let Ok(uia) =
                    CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                {
                    if let Ok(root) = uia.ElementFromHandle(top_hwnd) {
                        if let Ok(btn_cond) = uia.CreatePropertyCondition(
                            UIA_ControlTypePropertyId,
                            &VARIANT::from(UIA_ButtonControlTypeId.0),
                        ) {
                            for label in ["メッセージを送る", "メッセージ", "Send Message", "Message"] {
                                if let Ok(name_cond) = uia.CreatePropertyCondition(
                                    UIA_NamePropertyId,
                                    &VARIANT::from(label),
                                ) {
                                    if let Ok(and_cond) = uia.CreateAndCondition(&btn_cond, &name_cond) {
                                        if let Ok(elem) = root.FindFirst(TreeScope_Descendants, &and_cond) {
                                            let rect = elem.CurrentBoundingRectangle().unwrap_or_default();
                                            let in_win = rect.right > rect.left
                                                && rect.bottom > rect.top
                                                && rect.left >= win_rect.left
                                                && rect.right <= win_rect.right;
                                            if in_win {
                                                if let Ok(pat) = elem.GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                                    UIA_InvokePatternId,
                                                ) {
                                                    if pat.Invoke().is_ok() {
                                                        invoked = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !invoked {
                            has_main_composer = find_best_composer_element(&root, &uia, win_rect).is_some();
                        }
                    }
                }
                if com_inited {
                    CoUninitialize();
                }
            }

            if invoked {
                thread::sleep(Duration::from_millis(160));
                return Ok(true);
            }
            if has_main_composer {
                return Ok(false);
            }
            if esc_attempts < 2 {
                esc_attempts += 1;
                let target = render_hwnd.unwrap_or(top_hwnd);
                let _ = input::post_key_window_message(target, VK_ESCAPE);
                thread::sleep(Duration::from_millis(120));
                continue;
            }
            return Err("profile_modal_stuck");
        }
        thread::sleep(Duration::from_millis(60));
    }
    Err("composer_not_found")
}

/// Strictly locates the main Discord composer (`Edit`), calls `SetFocus()`, and polls
/// `HasKeyboardFocus == true` every 50ms up to `timeout_ms` (default 500ms).
pub fn focus_main_composer_verified_no_mouse(
    allowed_processes: &[String],
    timeout_ms: u64,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(200));
    let mut found_any_composer = false;

    while Instant::now() < deadline {
        if let Some((hwnd, _)) = find_discord_window(allowed_processes) {
            let mut win_rect = RECT::default();
            let _ = unsafe { GetWindowRect(hwnd, &mut win_rect) };
            let mut focused = false;

            unsafe {
                let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                if let Ok(uia) =
                    CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                {
                    if let Ok(root) = uia.ElementFromHandle(hwnd) {
                        if let Some((elem, _)) = find_best_composer_element(&root, &uia, win_rect) {
                            found_any_composer = true;
                            let _ = elem.SetFocus();
                            for _ in 0..10 {
                                if elem
                                    .CurrentHasKeyboardFocus()
                                    .map(|b| b.as_bool())
                                    .unwrap_or(false)
                                {
                                    focused = true;
                                    break;
                                }
                                thread::sleep(Duration::from_millis(50));
                                let _ = elem.SetFocus();
                            }
                        }
                    }
                }
                if com_inited {
                    CoUninitialize();
                }
            }
            if focused {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(50));
    }

    if found_any_composer {
        Err("focus_failed")
    } else {
        Err("composer_not_found")
    }
}

/// Reads the current text inside Discord's main chat composer using UIA ValuePattern or TextPattern.
pub fn read_main_composer_text_no_mouse(allowed_processes: &[String]) -> Option<String> {
    let (hwnd, _) = find_discord_window(allowed_processes)?;
    let mut win_rect = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut win_rect) };
    let mut text_out = None;

    unsafe {
        let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        if let Ok(uia) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        {
            if let Ok(root) = uia.ElementFromHandle(hwnd) {
                if let Some((elem, _)) = find_best_composer_element(&root, &uia, win_rect) {
                    if let Ok(vp) =
                        elem.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    {
                        if let Ok(val) = vp.CurrentValue() {
                            text_out = Some(val.to_string());
                        }
                    } else if let Ok(tp) =
                        elem.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                    {
                        if let Ok(range) = tp.DocumentRange() {
                            if let Ok(val) = range.GetText(-1) {
                                text_out = Some(val.to_string());
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
    text_out
}

/// Checks Discord's voice control UI button (Mic Mute / Deafen) via UIA without mouse/keyboard,
/// and optionally toggles it ONLY if `desired_muted` differs from the live state (`invoke_toggle = true`).
/// Returns `Some(Ok((was_already_target, action_msg)))` if the UIA control was found.
pub fn handle_discord_audio_toggle_uia(
    allowed_processes: &[String],
    is_deafen: bool,
    desired_muted: bool,
    allow_uia_toggle: bool,
) -> Option<(bool, String)> {
    let (hwnd, _) = find_discord_window(allowed_processes)?;
    let labels: &[(&str, Option<bool>)] = if is_deafen {
        &[
            ("スピーカーミュート解除", Some(true)),
            ("スピーカーミュート", None),
            ("Undeafen", Some(true)),
            ("Deafen", None),
        ]
    } else {
        &[
            ("ミュート解除", Some(true)),
            ("ミュート", None),
            ("Unmute", Some(true)),
            ("Mute", None),
        ]
    };
    let target_label = if is_deafen { "スピーカー" } else { "マイク" };

    let mut found_result: Option<(bool, String)> = None;
    unsafe {
        let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        if let Ok(uia) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        {
            if let Ok(root) = uia.ElementFromHandle(hwnd) {
                for &(name_str, label_implies_muted) in labels {
                    if let Ok(name_cond) =
                        uia.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(name_str))
                    {
                        if let Ok(elem) = root.FindFirst(TreeScope_Descendants, &name_cond) {
                            let toggle_pat = elem
                                .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
                                .ok();
                            let current_muted = if let Some(ref tp) = toggle_pat {
                                tp.CurrentToggleState()
                                    .map(|st| st == ToggleState_On)
                                    .unwrap_or(false)
                            } else if let Some(m) = label_implies_muted {
                                m
                            } else {
                                false
                            };

                            if current_muted == desired_muted {
                                let state_txt = if desired_muted {
                                    "すでにミュート中です"
                                } else {
                                    "すでにミュート解除されています"
                                };
                                found_result =
                                    Some((true, format!("{}は{}", target_label, state_txt)));
                                break;
                            }

                            if allow_uia_toggle {
                                let mut toggled = false;
                                if let Some(ref tp) = toggle_pat {
                                    if tp.Toggle().is_ok() {
                                        toggled = true;
                                    }
                                } else if let Ok(ip) = elem
                                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                        UIA_InvokePatternId,
                                    )
                                {
                                    if ip.Invoke().is_ok() {
                                        toggled = true;
                                    }
                                }
                                if toggled {
                                    let done_txt = if desired_muted {
                                        "をミュートにしました"
                                    } else {
                                        "のミュートを解除しました"
                                    };
                                    found_result =
                                        Some((false, format!("{}{}", target_label, done_txt)));
                                    break;
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        if com_inited {
            CoUninitialize();
        }
    }
    found_result
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
        assert!(is_excluded_edit_label("フレンド メモ (Note)"));
        assert!(is_excluded_edit_label("検索 Search"));

        let win = RECT { left: 0, top: 0, right: 1200, bottom: 900 };
        let good_box = RECT { left: 310, top: 820, right: 1120, bottom: 868 };
        let search_box = RECT { left: 400, top: 40, right: 800, bottom: 72 };
        let sidebar_note = RECT { left: 930, top: 450, right: 1170, bottom: 510 };
        assert!(score_composer_candidate("@alice へメッセージを送信", good_box, win).unwrap() >= 100);
        assert!(score_composer_candidate("検索", search_box, win).is_none());
        assert!(score_composer_candidate("メモ", sidebar_note, win).is_none());
        assert!(score_composer_candidate("", sidebar_note, win).is_none());
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
