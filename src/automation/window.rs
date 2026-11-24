use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
};

/// Returns the raw `HWND` handle of the current foreground window as `isize` (0 if none).
pub fn get_foreground_hwnd_raw() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Safely queries the executable filename (e.g. `"Discord.exe"`) of the current foreground window.
pub fn get_foreground_process_name() -> Option<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    get_hwnd_process_name(hwnd)
}

pub fn get_hwnd_process_name(hwnd: HWND) -> Option<String> {
    unsafe {
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let query_ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);

        if !query_ok || size == 0 {
            return None;
        }
        let full_path = String::from_utf16_lossy(&buf[..size as usize]);
        extract_exe_name(&full_path)
    }
}

struct FindAllowedCtx {
    allowed: Vec<String>,
    found: Option<HWND>,
}

unsafe extern "system" fn find_allowed_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindAllowedCtx);
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    if let Some(exe) = get_hwnd_process_name(hwnd) {
        if ctx.allowed.iter().any(|a| a.eq_ignore_ascii_case(&exe)) {
            let mut title_buf = vec![0u16; 128];
            let len = GetWindowTextW(hwnd, &mut title_buf);
            let title = if len > 0 {
                String::from_utf16_lossy(&title_buf[..len as usize])
            } else {
                String::new()
            };
            let looks_main = !title.trim().is_empty();
            if looks_main || ctx.found.is_none() {
                ctx.found = Some(hwnd);
                if title.to_ascii_lowercase().contains("discord") {
                    return BOOL(0);
                }
            }
        }
    }
    BOOL(1)
}

/// Finds a top-level visible window belonging to any allowed target process (e.g. `Discord.exe`).
pub fn find_allowed_window(allowed_processes: &[String]) -> Option<HWND> {
    let mut ctx = FindAllowedCtx {
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
            Some(find_allowed_cb),
            LPARAM(&mut ctx as *mut FindAllowedCtx as isize),
        );
    }
    ctx.found
}

/// Reliably brings `target_hwnd` to the foreground on Windows (handles foreground lock cleanly).
pub fn focus_hwnd(target_hwnd: HWND) -> bool {
    unsafe {
        if target_hwnd.0.is_null() || !IsWindow(Some(target_hwnd)).as_bool() {
            return false;
        }
        if IsIconic(target_hwnd).as_bool() {
            let _ = ShowWindow(target_hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(target_hwnd, SW_SHOW);
        }

        let fg = GetForegroundWindow();
        if fg == target_hwnd {
            return true;
        }

        let cur_tid = GetCurrentThreadId();
        let fg_tid = if !fg.0.is_null() {
            GetWindowThreadProcessId(fg, None)
        } else {
            0
        };
        let target_tid = GetWindowThreadProcessId(target_hwnd, None);

        // Tap Alt key lightly so Windows allows SetForegroundWindow from background helper app
        let alt_tap = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        wScan: 0,
                        dwFlags: Default::default(),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];
        let _ = SendInput(&alt_tap, std::mem::size_of::<INPUT>() as i32);

        let attached_fg = fg_tid != 0 && fg_tid != cur_tid
            && AttachThreadInput(cur_tid, fg_tid, true).as_bool();
        let attached_tgt = target_tid != 0 && target_tid != cur_tid && target_tid != fg_tid
            && AttachThreadInput(cur_tid, target_tid, true).as_bool();

        let _ = BringWindowToTop(target_hwnd);
        let ok = SetForegroundWindow(target_hwnd).as_bool();

        if attached_tgt {
            let _ = AttachThreadInput(cur_tid, target_tid, false);
        }
        if attached_fg {
            let _ = AttachThreadInput(cur_tid, fg_tid, false);
        }
        ok || GetForegroundWindow() == target_hwnd
    }
}

pub fn restore_hwnd_raw(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut core::ffi::c_void);
    let _ = focus_hwnd(hwnd);
}

/// Ensures an allowed Discord window is in the foreground, polling up to `timeout_ms`.
/// Returns `Ok((was_already_foreground, previous_hwnd_raw, active_process_name))`.
pub fn ensure_allowed_foreground(
    allowed_processes: &[String],
    timeout_ms: u64,
) -> Result<(bool, isize, String), String> {
    let prev_hwnd = get_foreground_hwnd_raw();
    let cur_proc = get_foreground_process_name().unwrap_or_default();
    if is_process_allowed(&cur_proc, allowed_processes, true) {
        return Ok((true, prev_hwnd, cur_proc));
    }

    let Some(discord_hwnd) = find_allowed_window(allowed_processes) else {
        return Err("Discord ウィンドウが見つかりません（Discord を起動した状態でお試しください）".into());
    };

    let _ = focus_hwnd(discord_hwnd);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.clamp(200, 3000));
    while Instant::now() < deadline {
        let p = get_foreground_process_name().unwrap_or_default();
        if is_process_allowed(&p, allowed_processes, true) {
            // Brief settle time for Discord focus
            thread::sleep(Duration::from_millis(70));
            return Ok((false, prev_hwnd, p));
        }
        thread::sleep(Duration::from_millis(25));
    }

    let final_proc = get_foreground_process_name().unwrap_or_default();
    if is_process_allowed(&final_proc, allowed_processes, true) {
        Ok((false, prev_hwnd, final_proc))
    } else {
        Err(format!(
            "Discord を最前面に切り替えられませんでした（現在: {}）",
            if final_proc.is_empty() { "不明" } else { &final_proc }
        ))
    }
}

pub fn extract_exe_name(full_path: &str) -> Option<String> {
    let trimmed = full_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Checks if `process_name` is permitted by the whitelist when `guard_enabled` is true.
pub fn is_process_allowed(
    process_name: &str,
    allowed_processes: &[String],
    guard_enabled: bool,
) -> bool {
    if !guard_enabled {
        return true;
    }
    let candidate = process_name.trim();
    if candidate.is_empty() {
        return false;
    }
    allowed_processes.iter().any(|allowed| {
        let a = allowed.trim();
        !a.is_empty() && a.eq_ignore_ascii_case(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_filename_from_windows_path() {
        assert_eq!(
            extract_exe_name(r"C:\Users\alice\AppData\Local\Discord\app-1.0\Discord.exe"),
            Some("Discord.exe".to_string())
        );
        assert_eq!(extract_exe_name(""), None);
    }

    #[test]
    fn whitelist_matching_is_case_insensitive() {
        let list = vec![
            "Discord.exe".to_string(),
            "DiscordPTB.exe".to_string(),
            "DiscordCanary.exe".to_string(),
        ];
        assert!(is_process_allowed("discord.exe", &list, true));
        assert!(is_process_allowed("DiscordPTB.exe", &list, true));
        assert!(!is_process_allowed("notepad.exe", &list, true));
        assert!(!is_process_allowed("", &list, true));
        assert!(is_process_allowed("notepad.exe", &list, false));
    }
}
