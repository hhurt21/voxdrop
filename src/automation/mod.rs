pub mod discord;
pub mod input;
pub mod uia;
pub mod window;

use crate::config::{
    DiscordIntegrationConfig, DiscordTarget, DiscordTargetKind, GuardConfig, SendMethod,
};
use input::KeyAction;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Remembers `(target_id, discord_window_title)` from our last successful navigation so
/// repeated sends to the SAME active DM/channel skip redundant deep-link & UIA searches (~0.14s send!).
static LAST_NAVIGATED: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Evaluates whether the current foreground window is permitted under `guard`.
pub fn check_target(guard: &GuardConfig) -> (bool, String) {
    let process = window::get_foreground_process_name().unwrap_or_default();
    let allowed = window::is_process_allowed(&process, &guard.allowed_processes, guard.enabled);
    (allowed, process)
}

/// Ensures an allowed target window is focused (automatically bringing Discord to foreground
/// when `guard.auto_focus_discord` is true so voice commands work even when Discord isn't active!).
fn prepare_foreground(guard: &GuardConfig) -> Result<(Option<isize>, String), String> {
    let (allowed, proc_name) = check_target(guard);
    if allowed {
        return Ok((None, proc_name));
    }
    if guard.auto_focus_discord {
        let (was_fg, prev_hwnd, active_proc) =
            window::ensure_allowed_foreground(&guard.allowed_processes, 1400)?;
        let restore = if !was_fg && guard.restore_previous_window {
            Some(prev_hwnd)
        } else {
            None
        };
        return Ok((restore, active_proc));
    }
    Err(format!(
        "安全ガードによりブロックされました（最前面: {}）",
        if proc_name.is_empty() { "不明" } else { &proc_name }
    ))
}

fn maybe_restore(restore_hwnd: Option<isize>) {
    if let Some(prev) = restore_hwnd {
        thread::sleep(Duration::from_millis(80));
        window::restore_hwnd_raw_ex(prev, true);
    }
}

fn maybe_restore_ex(restore_hwnd: Option<isize>, allow_send_input: bool) {
    if let Some(prev) = restore_hwnd {
        thread::sleep(Duration::from_millis(80));
        window::restore_hwnd_raw_ex(prev, allow_send_input);
    }
}

/// Pastes text into the active (or auto-focused Discord) window with clipboard backup/restore,
/// or copies to clipboard only when `guard.send_method` avoids `SendInput`.
pub fn paste_text_guarded(
    text: &str,
    press_enter: bool,
    guard: &GuardConfig,
) -> Result<String, String> {
    if text.is_empty() && !press_enter {
        return Ok("送信テキストが空のためスキップしました".into());
    }
    let (already_allowed, _) = check_target(guard);
    if guard.send_method == SendMethod::WindowMessage {
        if text.is_empty() {
            return Ok("WindowMessage モード: 空テキストをスキップしました".into());
        }
        if let Some((top_hwnd, render_hwnd)) =
            window::find_discord_render_hwnd(&guard.allowed_processes)
        {
            let prev = window::get_foreground_hwnd_raw();
            let _ = window::focus_hwnd_ex(top_hwnd, false);
            let _ = uia::focus_discord_chat_input_no_mouse(&guard.allowed_processes, 300);
            if input::post_text_window_message(render_hwnd, text, press_enter).is_ok() {
                if !already_allowed && guard.restore_previous_window && prev != 0 {
                    maybe_restore_ex(Some(prev), false);
                }
                return Ok("PostMessage送信完了（SendInput不使用）".into());
            }
        }
        input::copy_text_to_clipboard_only(text)?;
        return Ok("SendInput不使用モード: クリップボードへコピーしました".into());
    }
    if guard.should_avoid_send_input(already_allowed) {
        if text.is_empty() {
            return Ok("セーフモード中のため Enter 送信をスキップしました".into());
        }
        input::copy_text_to_clipboard_only(text)?;
        return Ok("セーフモード: SendInputを使わずクリップボードへコピーしました（Ctrl+Vで貼り付け）".into());
    }
    let (restore_hwnd, proc_name) = prepare_foreground(guard)?;

    let res = if text.is_empty() {
        input::send_key_action(KeyAction::Enter)
    } else {
        input::paste_with_clipboard_restore(text, press_enter, guard.clipboard_restore_delay_ms)
    };

    maybe_restore(restore_hwnd);
    res?;

    Ok(format!(
        "{}へ{}しました",
        if proc_name.is_empty() { "最前面ウィンドウ" } else { &proc_name },
        if press_enter { "貼り付け＋送信" } else { "貼り付け" }
    ))
}

/// Switches Discord to `target`, automatically clicks `"メッセージ"` if `discord://-/users/<id>`
/// opened a User Profile modal, and clicks/focuses the bottom chat text box so typing/sending works immediately!
pub fn navigate_and_focus_discord_target(
    target: &DiscordTarget,
    guard: &GuardConfig,
    navigate_delay_ms: u64,
    skip_if_already_open: bool,
) -> Result<String, String> {
    let allowed = &guard.allowed_processes;
    let is_user_only_dm = target.kind == DiscordTargetKind::Dm
        && target.channel_id.trim().is_empty()
        && !target.user_id.trim().is_empty();

    // Instant <1ms fast-path when sending: if we ALREADY navigated to this target AND Discord's
    // window title hasn't changed away from it (or title already matches target), skip re-navigation!
    let cur_title = uia::get_discord_window_title(allowed).unwrap_or_default();
    if skip_if_already_open && !cur_title.is_empty() {
        let same_as_last = LAST_NAVIGATED
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .map(|(last_id, last_title)| last_id == target.id && !last_title.is_empty() && last_title == cur_title)
            .unwrap_or(false);
        let norm_title = discord::normalize_for_match(&cur_title);
        let norm_name = discord::normalize_for_match(&target.name);
        let title_matches = (!norm_name.is_empty() && norm_title.contains(&norm_name))
            || target.passphrases.iter().any(|p| {
                let np = discord::normalize_for_match(p);
                !np.is_empty() && norm_title.contains(&np)
            });
        // Also if Discord title is already a DM (`@username - Discord`) and target is user_only_dm
        // that we already opened, we don't need to re-open the profile modal!
        if same_as_last || title_matches {
            return Ok(format!(
                "{} は既に開いています（高速送信）",
                discord::target_display_label(Some(target))
            ));
        }
    }

    let (already_allowed, _) = check_target(guard);
    if guard.send_method == SendMethod::WindowMessage {
        if let Some(link) = discord::build_deep_link(target) {
            let _ = discord::open_deep_link(&link);
            thread::sleep(Duration::from_millis(navigate_delay_ms.clamp(120, 1000)));
        }
        let _ = window::ensure_allowed_foreground_ex(allowed, 700, false);
        let _ = uia::focus_discord_chat_input_no_mouse(allowed, 500);
        return Ok(format!(
            "{}（ウィンドウメッセージ送信: SendInput不使用）",
            discord::target_display_label(Some(target))
        ));
    }
    if guard.send_method == SendMethod::NoSendInput
        || (guard.send_method == SendMethod::SafeWhenGameActive && !already_allowed)
    {
        if guard.send_method == SendMethod::NoSendInput {
            if let Some(link) = discord::build_deep_link(target) {
                let _ = discord::open_deep_link(&link);
            }
        }
        return Ok(format!(
            "{}（セーフモード: SendInput / マウス移動をスキップ）",
            discord::target_display_label(Some(target))
        ));
    }

    if let Some(link) = discord::build_deep_link(target) {
        discord::open_deep_link(&link)?;
        let _ = window::ensure_allowed_foreground(allowed, 1000);

        if is_user_only_dm {
            // `discord://-/users/<id>` opens the User Profile modal -> fast native FindFirst click on "メッセージ"!
            let clicked_dm = uia::click_profile_message_button(allowed, 1200);
            if clicked_dm {
                thread::sleep(Duration::from_millis(180));
            } else if !target.name.trim().is_empty() {
                let _ = input::send_key_action(KeyAction::Escape);
                thread::sleep(Duration::from_millis(60));
                if input::send_key_action(KeyAction::CtrlK).is_ok() {
                    thread::sleep(Duration::from_millis(120));
                    let query = format!("@{}", target.name.trim());
                    let _ = input::paste_with_clipboard_restore(
                        &query,
                        false,
                        guard.clipboard_restore_delay_ms,
                    );
                    thread::sleep(Duration::from_millis(180));
                    let _ = input::send_key_action(KeyAction::Enter);
                    thread::sleep(Duration::from_millis(200));
                }
            }
        } else {
            thread::sleep(Duration::from_millis(navigate_delay_ms.clamp(120, 1200)));
        }
    } else if target.kind == DiscordTargetKind::Dm && !target.name.trim().is_empty() {
        let _ = window::ensure_allowed_foreground(allowed, 1000);
        if input::send_key_action(KeyAction::CtrlK).is_ok() {
            thread::sleep(Duration::from_millis(120));
            let query = format!("@{}", target.name.trim());
            let _ = input::paste_with_clipboard_restore(
                &query,
                false,
                guard.clipboard_restore_delay_ms,
            );
            thread::sleep(Duration::from_millis(180));
            let _ = input::send_key_action(KeyAction::Enter);
            thread::sleep(Duration::from_millis(200));
        }
    }

    // Fast native FindAll(Edit) click & focus on Discord's chat input box (~40ms!)
    let _ = window::ensure_allowed_foreground(allowed, 800);
    let focused = uia::focus_discord_chat_input(allowed, 800);

    // Record the navigated Discord window title so subsequent sends take ~0.14s!
    let new_title = uia::get_discord_window_title(allowed).unwrap_or_default();
    if let Ok(mut g) = LAST_NAVIGATED.lock() {
        *g = Some((target.id.clone(), new_title));
    }

    if focused {
        Ok(format!(
            "{} のチャット画面を開き、入力欄にフォーカスしました",
            discord::target_display_label(Some(target))
        ))
    } else {
        Ok(format!(
            "{} へ切り替えました",
            discord::target_display_label(Some(target))
        ))
    }
}

/// Sends text to a configured Discord target even when Discord is in the background!
pub fn send_to_discord_target(
    text: &str,
    press_enter: bool,
    target: Option<&DiscordTarget>,
    auto_navigate: bool,
    navigate_delay_ms: u64,
    guard: &GuardConfig,
    integration: &DiscordIntegrationConfig,
) -> Result<String, String> {
    let formatted = discord::format_message_for_target(text, target);

    if let Some(t) = target {
        if press_enter && !t.webhook_url.trim().is_empty() {
            if formatted.is_empty() {
                return Ok("送信テキストが空のためスキップしました".into());
            }
            discord::send_via_webhook(&t.webhook_url, &formatted)?;
            return Ok(format!(
                "{} へWebhook直接送信しました",
                discord::target_display_label(Some(t))
            ));
        }

        if press_enter
            && integration.use_bot_for_send
            && !integration.bot_token.trim().is_empty()
            && t.kind == DiscordTargetKind::Channel
            && !t.channel_id.trim().is_empty()
        {
            if formatted.is_empty() {
                return Ok("送信テキストが空のためスキップしました".into());
            }
            discord::send_via_bot_api(&integration.bot_token, &t.channel_id, &formatted)?;
            return Ok(format!(
                "{} へ公式Bot直接送信しました",
                discord::target_display_label(Some(t))
            ));
        }
    }

    // Remember user's current foreground window BEFORE focusing/navigating Discord!
    let prev_hwnd = window::get_foreground_hwnd_raw();
    let (already_allowed, _) = check_target(guard);

    if guard.send_method == SendMethod::WindowMessage {
        if formatted.is_empty() && !press_enter {
            return Ok("送信テキストが空のためスキップしました".into());
        }
        if let Some(t) = target {
            if auto_navigate {
                let _ = navigate_and_focus_discord_target(t, guard, navigate_delay_ms, true);
            }
        }
        let focused = if already_allowed {
            true
        } else {
            window::ensure_allowed_foreground_ex(&guard.allowed_processes, 900, false).is_ok()
        };
        let _ = uia::focus_discord_chat_input_no_mouse(&guard.allowed_processes, 350);

        if focused {
            if let Some((_, render_hwnd)) =
                window::find_discord_render_hwnd(&guard.allowed_processes)
            {
                let res = input::post_text_window_message(render_hwnd, &formatted, press_enter);
                if !already_allowed && guard.restore_previous_window && prev_hwnd != 0 {
                    maybe_restore_ex(Some(prev_hwnd), false);
                }
                if res.is_ok() {
                    let label = discord::target_display_label(target);
                    return Ok(format!(
                        "{} → PostMessage送信完了（SendInput不使用）",
                        label
                    ));
                }
            }
        }

        // Graceful anti-cheat-safe fallback: never touch SendInput
        if !formatted.is_empty() {
            input::copy_text_to_clipboard_only(&formatted)?;
        }
        return Ok("SendInput不使用モード: フォーカスできなかったためクリップボードへコピーしました".into());
    }

    if guard.should_avoid_send_input(already_allowed) {
        if formatted.is_empty() {
            return Ok("セーフモード中のため空送信をスキップしました".into());
        }
        input::copy_text_to_clipboard_only(&formatted)?;
        let label = discord::target_display_label(target);
        return Ok(format!(
            "{} → セーフモード: SendInputを使わずクリップボードへコピーしました（Discordで Ctrl+V → Enter）",
            label
        ));
    }

    if let Some(t) = target {
        if auto_navigate {
            let _ = navigate_and_focus_discord_target(t, guard, navigate_delay_ms, true);
        }
    }

    // Ensure Discord is foreground; only do an extra UIA focus check if we had no target navigation
    let (_, proc_name) = prepare_foreground(guard)?;
    if target.is_none() || !auto_navigate {
        let _ = uia::focus_discord_chat_input(&guard.allowed_processes, 250);
    }

    let res = if formatted.is_empty() {
        if press_enter {
            input::send_key_action(KeyAction::Enter)
        } else {
            Ok(())
        }
    } else {
        input::paste_with_clipboard_restore(&formatted, press_enter, guard.clipboard_restore_delay_ms)
    };

    if !already_allowed && guard.restore_previous_window && prev_hwnd != 0 {
        maybe_restore(Some(prev_hwnd));
    }

    res?;

    let base_msg = format!(
        "{}へ{}しました",
        if proc_name.is_empty() { "Discord" } else { &proc_name },
        if press_enter { "貼り付け＋送信" } else { "貼り付け" }
    );
    if let Some(t) = target {
        Ok(format!(
            "{} → {}",
            discord::target_display_label(Some(t)),
            base_msg
        ))
    } else {
        Ok(base_msg)
    }
}

/// Sends a keyboard or mouse-wheel command action (auto-focusing Discord if configured).
pub fn send_action_guarded(action: KeyAction, guard: &GuardConfig) -> Result<String, String> {
    let (already_allowed, _) = check_target(guard);
    if guard.should_avoid_send_input(already_allowed) {
        return Ok(format!(
            "セーフモード中のためキー操作({:?})をスキップしました",
            action
        ));
    }
    let (restore_hwnd, proc_name) = prepare_foreground(guard)?;
    let res = input::send_key_action(action);
    maybe_restore(restore_hwnd);
    res?;
    Ok(format!(
        "{}へ操作({:?})を実行しました",
        if proc_name.is_empty() { "Discord" } else { &proc_name },
        action
    ))
}
