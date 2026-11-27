use arboard::Clipboard;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    VIRTUAL_KEY, VK_A, VK_BACK, VK_CONTROL, VK_D, VK_DOWN, VK_ESCAPE, VK_F, VK_I, VK_K, VK_LEFT,
    VK_M, VK_MENU, VK_NEXT, VK_P, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_U, VK_UP, VK_V,
    VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, PostMessageW, SetCursorPos, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
};

/// Serializes clipboard & SendInput operations so concurrent triggers never clobber clipboard backup.
static INPUT_MUTEX: Mutex<()> = Mutex::new(());
/// Tracks the last timestamp (ms) VoxDrop modified the clipboard so our URL watcher ignores self-writes.
static LAST_OWN_CLIPBOARD_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn was_clipboard_touched_by_self_recently(window_ms: u64) -> bool {
    let last = LAST_OWN_CLIPBOARD_MS.load(Ordering::Relaxed);
    last > 0 && now_ms().saturating_sub(last) < window_ms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Enter,
    ShiftEnter,
    CtrlV,
    CtrlZ,
    CtrlK,
    DownArrow,
    UpArrow,
    Escape,
    ClearAll,
    BackSpace(usize),
    // Discord remote control actions (all reversible / bidirectional)
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
    DiscordQuickSwitcher,
    DiscordSearch,
}

fn is_extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(vk, VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_PRIOR | VK_NEXT)
}

fn key_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    let mut flags = if key_up {
        KEYEVENTF_KEYUP
    } else {
        Default::default()
    };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_wheel_input(delta: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(format!(
            "SendInput の送信数が一致しません (期待: {}, 実際: {})",
            inputs.len(),
            sent
        ));
    }
    Ok(())
}

fn send_combo(modifier: VIRTUAL_KEY, key: VIRTUAL_KEY) -> Result<(), String> {
    let seq = [
        key_input(modifier, false),
        key_input(key, false),
        key_input(key, true),
        key_input(modifier, true),
    ];
    send_inputs(&seq)
}

fn send_combo3(m1: VIRTUAL_KEY, m2: VIRTUAL_KEY, key: VIRTUAL_KEY) -> Result<(), String> {
    let seq = [
        key_input(m1, false),
        key_input(m2, false),
        key_input(key, false),
        key_input(key, true),
        key_input(m2, true),
        key_input(m1, true),
    ];
    send_inputs(&seq)
}

fn send_single_key(key: VIRTUAL_KEY) -> Result<(), String> {
    let seq = [key_input(key, false), key_input(key, true)];
    send_inputs(&seq)
}

/// Performs a real Win32 left mouse click at screen coordinates `(x, y)` and restores the user's cursor.
pub fn click_screen_point_and_restore(x: i32, y: i32) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "入力ロックの取得に失敗しました".to_string())?;
    let mut prev = POINT { x: 0, y: 0 };
    let has_prev = unsafe { GetCursorPos(&mut prev).is_ok() };

    unsafe {
        let _ = SetCursorPos(x, y);
    }
    thread::sleep(Duration::from_millis(20));

    let click = [
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_LEFTDOWN,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_LEFTUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    let res = send_inputs(&click);
    thread::sleep(Duration::from_millis(25));

    if has_prev {
        unsafe {
            let _ = SetCursorPos(prev.x, prev.y);
        }
    }
    res
}

/// Dispatches a keyboard or mouse-wheel action via Win32 `SendInput`.
pub fn send_key_action(action: KeyAction) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "入力ロックの取得に失敗しました".to_string())?;
    send_key_action_unlocked(action)
}

fn send_key_action_unlocked(action: KeyAction) -> Result<(), String> {
    match action {
        KeyAction::Enter => send_single_key(VK_RETURN),
        KeyAction::ShiftEnter => send_combo(VK_SHIFT, VK_RETURN),
        KeyAction::CtrlV => send_combo(VK_CONTROL, VK_V),
        KeyAction::CtrlZ => send_combo(VK_CONTROL, VK_Z),
        KeyAction::CtrlK => send_combo(VK_CONTROL, VK_K),
        KeyAction::DownArrow => send_single_key(VK_DOWN),
        KeyAction::UpArrow => send_single_key(VK_UP),
        KeyAction::Escape => send_single_key(VK_ESCAPE),
        KeyAction::ClearAll => {
            send_combo(VK_CONTROL, VK_A)?;
            thread::sleep(Duration::from_millis(20));
            send_single_key(VK_BACK)
        }
        KeyAction::BackSpace(count) => {
            let capped = count.min(200);
            for _ in 0..capped {
                send_single_key(VK_BACK)?;
            }
            Ok(())
        }
        // Discord official keyboard & mouse-wheel shortcuts
        KeyAction::DiscordMuteMic | KeyAction::DiscordUnmuteMic => {
            send_combo3(VK_CONTROL, VK_SHIFT, VK_M)
        }
        KeyAction::DiscordDeafen | KeyAction::DiscordUndeafen => {
            send_combo3(VK_CONTROL, VK_SHIFT, VK_D)
        }
        KeyAction::DiscordNavBack => send_combo(VK_MENU, VK_LEFT),
        KeyAction::DiscordNavForward => send_combo(VK_MENU, VK_RIGHT),
        KeyAction::DiscordNextChannel => send_combo(VK_MENU, VK_DOWN),
        KeyAction::DiscordPrevChannel => send_combo(VK_MENU, VK_UP),
        KeyAction::DiscordPrevUnread => send_combo3(VK_MENU, VK_SHIFT, VK_UP),
        KeyAction::DiscordNextUnread => send_combo3(VK_MENU, VK_SHIFT, VK_DOWN),
        KeyAction::DiscordNextServer => send_combo3(VK_CONTROL, VK_MENU, VK_DOWN),
        KeyAction::DiscordPrevServer => send_combo3(VK_CONTROL, VK_MENU, VK_UP),
        KeyAction::DiscordScrollUp => {
            // PageUp works when Discord chat has focus; also send gentle wheel up
            send_single_key(VK_PRIOR)?;
            let _ = send_inputs(&[mouse_wheel_input(360)]);
            Ok(())
        }
        KeyAction::DiscordScrollDown => {
            send_single_key(VK_NEXT)?;
            let _ = send_inputs(&[mouse_wheel_input(-360)]);
            Ok(())
        }
        KeyAction::DiscordJumpPresent => send_combo(VK_SHIFT, VK_NEXT),
        KeyAction::DiscordEditLast => send_single_key(VK_UP),
        KeyAction::DiscordCancelEscape => send_single_key(VK_ESCAPE),
        KeyAction::DiscordMarkRead => send_combo(VK_SHIFT, VK_ESCAPE),
        KeyAction::DiscordToggleMembers => send_combo(VK_CONTROL, VK_U),
        KeyAction::DiscordTogglePins => send_combo(VK_CONTROL, VK_P),
        KeyAction::DiscordToggleInbox => send_combo(VK_CONTROL, VK_I),
        KeyAction::DiscordQuickSwitcher => send_combo(VK_CONTROL, VK_K),
        KeyAction::DiscordSearch => send_combo(VK_CONTROL, VK_F),
    }
}

/// Maps simple non-modifier `KeyAction`s to a virtual key for `PostMessageW` (non-SendInput).
pub fn window_message_single_vk(action: KeyAction) -> Option<VIRTUAL_KEY> {
    match action {
        KeyAction::Enter => Some(VK_RETURN),
        KeyAction::Escape | KeyAction::DiscordCancelEscape => Some(VK_ESCAPE),
        KeyAction::UpArrow | KeyAction::DiscordEditLast => Some(VK_UP),
        KeyAction::DownArrow => Some(VK_DOWN),
        KeyAction::DiscordScrollUp => Some(VK_PRIOR),
        KeyAction::DiscordScrollDown => Some(VK_NEXT),
        _ => None,
    }
}

/// Posts a single non-modifier key down+up directly via `PostMessageW` (zero `SendInput`).
pub fn post_key_window_message(target_hwnd: HWND, vk: VIRTUAL_KEY) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "入力ロックの取得に失敗しました".to_string())?;
    if target_hwnd.0.is_null() {
        return Err("送信先ウィンドウハンドルが無効です".into());
    }
    unsafe {
        PostMessageW(Some(target_hwnd), WM_KEYDOWN, WPARAM(vk.0 as usize), LPARAM(1))
            .map_err(|e| format!("PostMessageW(WM_KEYDOWN) に失敗しました: {}", e))?;
        thread::sleep(Duration::from_millis(12));
        PostMessageW(
            Some(target_hwnd),
            WM_KEYUP,
            WPARAM(vk.0 as usize),
            LPARAM(0xC000_0001u32 as isize),
        )
        .map_err(|e| format!("PostMessageW(WM_KEYUP) に失敗しました: {}", e))?;
    }
    Ok(())
}

/// Copies `text` to the system clipboard WITHOUT issuing any `SendInput` or `SetCursorPos` calls
/// (safe for strict anti-cheat environments).
pub fn copy_text_to_clipboard_only(text: &str) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "クリップボード操作ロックの取得に失敗しました".to_string())?;
    LAST_OWN_CLIPBOARD_MS.store(now_ms(), Ordering::Relaxed);
    let mut clipboard = Clipboard::new()
        .map_err(|e| format!("クリップボードのオープンに失敗しました: {}", e))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("クリップボードへの書き込みに失敗しました: {}", e))?;
    LAST_OWN_CLIPBOARD_MS.store(now_ms(), Ordering::Relaxed);
    Ok(())
}

/// Posts only UTF-16 text via `WM_CHAR` (without pressing Return yet so UIA verification can run first).
pub fn post_chars_window_message(target_hwnd: HWND, text: &str) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "入力ロックの取得に失敗しました".to_string())?;
    if target_hwnd.0.is_null() {
        return Err("送信先ウィンドウハンドルが無効です".into());
    }
    let normalized = text.replace("\r\n", " ").replace('\n', " ");
    for unit in normalized.encode_utf16() {
        unsafe {
            PostMessageW(Some(target_hwnd), WM_CHAR, WPARAM(unit as usize), LPARAM(1))
                .map_err(|e| format!("PostMessageW(WM_CHAR) に失敗しました: {}", e))?;
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Posts `VK_RETURN` down+up directly to `target_hwnd`.
pub fn post_enter_window_message(target_hwnd: HWND) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "入力ロックの取得に失敗しました".to_string())?;
    if target_hwnd.0.is_null() {
        return Err("送信先ウィンドウハンドルが無効です".into());
    }
    thread::sleep(Duration::from_millis(16));
    unsafe {
        let lparam_down = LPARAM(0x001C_0001);
        let lparam_up = LPARAM((0xC01C_0001u32) as isize);
        PostMessageW(
            Some(target_hwnd),
            WM_KEYDOWN,
            WPARAM(VK_RETURN.0 as usize),
            lparam_down,
        )
        .map_err(|e| format!("PostMessageW(VK_RETURN down) に失敗しました: {}", e))?;
        thread::sleep(Duration::from_millis(24));
        PostMessageW(
            Some(target_hwnd),
            WM_KEYUP,
            WPARAM(VK_RETURN.0 as usize),
            lparam_up,
        )
        .map_err(|e| format!("PostMessageW(VK_RETURN up) に失敗しました: {}", e))?;
    }
    Ok(())
}

/// Reads current text on the Windows clipboard safely (#25).
pub fn copy_clipboard_read_fallback() -> String {
    let Ok(_guard) = INPUT_MUTEX.lock() else {
        return String::new();
    };
    Clipboard::new()
        .and_then(|mut cb| cb.get_text())
        .unwrap_or_default()
}

/// Sends text directly to a target window handle using Win32 `PostMessageW(WM_CHAR / VK_RETURN)`
/// without calling `SendInput`, `SetCursorPos`, or modifying the global input stream (`LLKHF_INJECTED`).
pub fn post_text_window_message(
    target_hwnd: HWND,
    text: &str,
    press_enter: bool,
) -> Result<(), String> {
    post_chars_window_message(target_hwnd, text)?;
    if press_enter {
        post_enter_window_message(target_hwnd)?;
    }
    Ok(())
}

/// Safely backs up current clipboard text, pastes `text` via Ctrl+V (and optional Enter),
/// waits `restore_delay_ms` (default 50..100ms), and restores the original clipboard text.
pub fn paste_with_clipboard_restore(
    text: &str,
    press_enter: bool,
    restore_delay_ms: u64,
) -> Result<(), String> {
    let _guard = INPUT_MUTEX
        .lock()
        .map_err(|_| "クリップボード操作ロックの取得に失敗しました".to_string())?;

    LAST_OWN_CLIPBOARD_MS.store(now_ms(), Ordering::Relaxed);

    let mut clipboard = Clipboard::new()
        .map_err(|e| format!("クリップボードのオープンに失敗しました: {}", e))?;

    let previous_text = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("クリップボードへの書き込みに失敗しました: {}", e))?;
    LAST_OWN_CLIPBOARD_MS.store(now_ms(), Ordering::Relaxed);

    thread::sleep(Duration::from_millis(20));

    let paste_res = send_key_action_unlocked(KeyAction::CtrlV);

    if paste_res.is_ok() && press_enter {
        thread::sleep(Duration::from_millis(30));
        let _ = send_key_action_unlocked(KeyAction::Enter);
    }

    let delay = restore_delay_ms.clamp(30, 1000);
    thread::sleep(Duration::from_millis(delay));

    if let Some(prev) = previous_text {
        let _ = clipboard.set_text(prev);
        LAST_OWN_CLIPBOARD_MS.store(now_ms(), Ordering::Relaxed);
    }

    paste_res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_input_builds_down_and_up_flags() {
        let down = key_input(VK_RETURN, false);
        let up = key_input(VK_RETURN, true);
        unsafe {
            assert_eq!(down.Anonymous.ki.wVk, VK_RETURN);
            assert_eq!(down.Anonymous.ki.dwFlags.0, 0);
            assert_eq!(up.Anonymous.ki.dwFlags, KEYEVENTF_KEYUP);
        }
    }
}
