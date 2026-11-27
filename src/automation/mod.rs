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
        if press_enter {
            input::send_key_action(KeyAction::Enter)
        } else {
            Ok(())
        }
    } else if press_enter {
        let r = input::paste_with_clipboard_restore(
            text,
            false,
            guard.clipboard_restore_delay_ms.max(100),
        );
        if r.is_ok() {
            send_enter_with_verify_and_retry(&guard.allowed_processes)
        } else {
            r
        }
    } else {
        input::paste_with_clipboard_restore(text, false, guard.clipboard_restore_delay_ms)
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
        let _ = window::ensure_allowed_foreground_ex(allowed, 800, false);
        let render_hwnd = window::find_discord_render_hwnd(allowed).map(|(_, r)| r);
        // Step 2: Detect & dismiss profile modal / invoke "メッセージ" button zero-mouse!
        let _ = uia::recover_from_profile_modal_no_mouse(allowed, render_hwnd, 1100);
        // Step 3-4: Strict composer filter + SetFocus() + HasKeyboardFocus poll!
        let focus_res = uia::focus_main_composer_verified_no_mouse(allowed, 550);
        let new_title = uia::get_discord_window_title(allowed).unwrap_or_default();
        if let Ok(mut g) = LAST_NAVIGATED.lock() {
            *g = Some((target.id.clone(), new_title));
        }
        return match focus_res {
            Ok(()) => Ok(format!(
                "{}（PostMessage送信準備完了: 入力欄フォーカス確定）",
                discord::target_display_label(Some(target))
            )),
            Err(code) => Ok(format!(
                "{}（PostMessage遷移注意 [{}]）",
                discord::target_display_label(Some(target)),
                code
            )),
        };
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
    let chunks = if formatted.is_empty() {
        Vec::new()
    } else {
        discord::split_message_for_discord(&formatted, 1950)
    };

    if let Some(t) = target {
        if press_enter && !t.webhook_url.trim().is_empty() {
            if chunks.is_empty() {
                return Ok("送信テキストが空のためスキップしました".into());
            }
            for (idx, chunk) in chunks.iter().enumerate() {
                discord::send_via_webhook(&t.webhook_url, chunk)?;
                if idx + 1 < chunks.len() {
                    thread::sleep(Duration::from_millis(120));
                }
            }
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
            if chunks.is_empty() {
                return Ok("送信テキストが空のためスキップしました".into());
            }
            for (idx, chunk) in chunks.iter().enumerate() {
                discord::send_via_bot_api(&integration.bot_token, &t.channel_id, chunk)?;
                if idx + 1 < chunks.len() {
                    thread::sleep(Duration::from_millis(120));
                }
            }
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
        if chunks.is_empty() && !press_enter {
            return Ok("送信テキストが空のためスキップしました".into());
        }
        if let Some(t) = target {
            if auto_navigate {
                let _ = navigate_and_focus_discord_target(t, guard, navigate_delay_ms, true);
            }
        }
        let focused_win = if already_allowed {
            true
        } else {
            window::ensure_allowed_foreground_ex(&guard.allowed_processes, 900, false).is_ok()
        };

        let render_hwnd_opt = window::find_discord_render_hwnd(&guard.allowed_processes).map(|(_, r)| r);
        if focused_win {
            // Recover automatically if a user profile modal/popout opened instead of DM chat!
            if let Err(code) = uia::recover_from_profile_modal_no_mouse(
                &guard.allowed_processes,
                render_hwnd_opt,
                900,
            ) {
                if !formatted.is_empty() {
                    let _ = input::copy_text_to_clipboard_only(&formatted);
                }
                return Err(format!(
                    "PostMessage送信失敗 [{}]: プロフィールモーダルからDM入力欄へ遷移できませんでした（クリップボードへ退避済み）",
                    code
                ));
            }
            if let Err(code) = uia::focus_main_composer_verified_no_mouse(&guard.allowed_processes, 550) {
                if !formatted.is_empty() {
                    let _ = input::copy_text_to_clipboard_only(&formatted);
                }
                return Err(format!(
                    "PostMessage送信失敗 [{}]: チャット入力欄のキーボードフォーカス確定に失敗（クリップボードへ退避済み）",
                    code
                ));
            }

            if let Some(render_hwnd) = render_hwnd_opt {
                for (idx, chunk) in chunks.iter().enumerate() {
                    let mut inserted = false;
                    for attempt in 0..2 {
                        if attempt > 0 {
                            let _ = uia::focus_main_composer_verified_no_mouse(
                                &guard.allowed_processes,
                                350,
                            );
                        }
                        input::post_chars_window_message(render_hwnd, chunk)?;
                        thread::sleep(Duration::from_millis(120));
                        match uia::read_main_composer_text_no_mouse(&guard.allowed_processes) {
                            Some(val) if val.trim().is_empty() => {
                                continue;
                            }
                            _ => {
                                inserted = true;
                                break;
                            }
                        }
                    }
                    if !inserted {
                        let _ = input::copy_text_to_clipboard_only(&formatted);
                        return Err(
                            "PostMessage送信失敗 [text_not_inserted]: 入力欄へのテキスト反映が確認できませんでした（クリップボードへ退避済み）"
                                .into(),
                        );
                    }
                    if press_enter {
                        input::post_enter_window_message(render_hwnd)?;
                        thread::sleep(Duration::from_millis(140));
                        let mut cleared = match uia::read_main_composer_text_no_mouse(&guard.allowed_processes) {
                            Some(rem) => rem.trim().is_empty(),
                            None => true,
                        };
                        if !cleared {
                            let _ = uia::focus_main_composer_verified_no_mouse(&guard.allowed_processes, 300);
                            input::post_enter_window_message(render_hwnd)?;
                            thread::sleep(Duration::from_millis(140));
                            cleared = match uia::read_main_composer_text_no_mouse(&guard.allowed_processes) {
                                Some(rem) => rem.trim().is_empty(),
                                None => true,
                            };
                        }
                        if !cleared {
                            let _ = input::copy_text_to_clipboard_only(&formatted);
                            return Err(
                                "PostMessage送信失敗 [enter_not_sent]: Enter送信後も入力欄にテキストが残っています（クリップボードへ退避済み）"
                                    .into(),
                            );
                        }
                        if idx + 1 < chunks.len() {
                            thread::sleep(Duration::from_millis(120));
                        }
                    }
                }
                if chunks.is_empty() && press_enter {
                    input::post_enter_window_message(render_hwnd)?;
                }
                if !already_allowed && guard.restore_previous_window && prev_hwnd != 0 {
                    maybe_restore_ex(Some(prev_hwnd), false);
                }
                let label = discord::target_display_label(target);
                return Ok(format!(
                    "{} → PostMessage送信・入力検証完了（SendInput不使用）",
                    label
                ));
            }
        }

        // Graceful anti-cheat-safe fallback: never touch SendInput
        if !formatted.is_empty() {
            input::copy_text_to_clipboard_only(&formatted)?;
        }
        return Ok("SendInput不使用モード [composer_not_found]: クリップボードへコピーしました".into());
    }

    if guard.should_avoid_send_input(already_allowed) {
        if chunks.is_empty() {
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

    // Ensure Discord is foreground and composer has focus when returning from another window or skipping nav
    let (_, proc_name) = prepare_foreground(guard)?;
    if !already_allowed || target.is_none() || !auto_navigate {
        let _ = uia::focus_discord_chat_input(&guard.allowed_processes, 250);
    }

    let res = if chunks.is_empty() {
        if press_enter {
            input::send_key_action(KeyAction::Enter)
        } else {
            Ok(())
        }
    } else {
        let mut last_res = Ok(());
        for (idx, chunk) in chunks.iter().enumerate() {
            if press_enter {
                last_res = input::paste_with_clipboard_restore(
                    chunk,
                    false,
                    guard.clipboard_restore_delay_ms.max(100),
                );
                if last_res.is_ok() {
                    last_res = send_enter_with_verify_and_retry(&guard.allowed_processes);
                }
            } else {
                last_res = input::paste_with_clipboard_restore(
                    chunk,
                    false,
                    guard.clipboard_restore_delay_ms,
                );
            }
            if last_res.is_err() {
                break;
            }
            if idx + 1 < chunks.len() {
                thread::sleep(Duration::from_millis(120));
            }
        }
        last_res
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

/// Waits for Slate.js/Chromium composer to reflect pasted text, sends Enter, and retries via UIA +
/// fallback `PostMessageW(VK_RETURN)` if text remains unsubmitted in the composer.
fn send_enter_with_verify_and_retry(allowed_processes: &[String]) -> Result<(), String> {
    // Poll briefly so Chromium/Slate.js finishes DOM commit before Enter
    for _ in 0..5 {
        match uia::read_main_composer_text_no_mouse(allowed_processes) {
            Some(t) if !t.trim().is_empty() => break,
            None => break,
            Some(_) => thread::sleep(Duration::from_millis(45)),
        }
    }

    for attempt in 0..3usize {
        if attempt > 0 {
            let _ = window::ensure_allowed_foreground(allowed_processes, 400);
            let _ = uia::focus_discord_chat_input(allowed_processes, 250);
            thread::sleep(Duration::from_millis(40));
        }

        input::send_key_action(KeyAction::Enter)?;
        if attempt == 2 {
            if let Some((_, render_hwnd)) = window::find_discord_render_hwnd(allowed_processes) {
                let _ = input::post_enter_window_message(render_hwnd);
            }
        }

        let check_wait = if attempt == 0 { 130 } else { 170 };
        thread::sleep(Duration::from_millis(check_wait));

        match uia::read_main_composer_text_no_mouse(allowed_processes) {
            Some(rem) if rem.trim().is_empty() => return Ok(()),
            None => return Ok(()),
            Some(_) => continue,
        }
    }
    Ok(())
}

/// Sends a keyboard or mouse-wheel command action (auto-focusing Discord if configured).
/// Supports idempotent Mute/Unmute & Deafen/Undeafen via UIA state check, plus non-SendInput
/// single keys via `PostMessageW` in `WindowMessage` mode.
pub fn send_action_guarded(action: KeyAction, guard: &GuardConfig) -> Result<String, String> {
    let (already_allowed, _) = check_target(guard);

    // 1. Idempotent UIA Mute / Unmute / Deafen / Undeafen check & direct UIA toggle
    let audio_req = match action {
        KeyAction::DiscordMuteMic => Some((false, true)),
        KeyAction::DiscordUnmuteMic => Some((false, false)),
        KeyAction::DiscordDeafen => Some((true, true)),
        KeyAction::DiscordUndeafen => Some((true, false)),
        _ => None,
    };
    if let Some((is_deafen, desired_muted)) = audio_req {
        let allow_uia_invoke = guard.send_method != SendMethod::NoSendInput;
        if let Some((already_target, msg)) = uia::handle_discord_audio_toggle_uia(
            &guard.allowed_processes,
            is_deafen,
            desired_muted,
            allow_uia_invoke,
        ) {
            if already_target || allow_uia_invoke {
                return Ok(msg);
            }
        }
    }

    // 2. Non-SendInput single-key dispatch in `WindowMessage` mode
    if guard.send_method == SendMethod::WindowMessage {
        if let Some(vk) = input::window_message_single_vk(action) {
            if let Some((top_hwnd, render_hwnd)) =
                window::find_discord_render_hwnd(&guard.allowed_processes)
            {
                let prev = window::get_foreground_hwnd_raw();
                let _ = window::focus_hwnd_ex(top_hwnd, false);
                if input::post_key_window_message(render_hwnd, vk).is_ok() {
                    if !already_allowed && guard.restore_previous_window && prev != 0 {
                        maybe_restore_ex(Some(prev), false);
                    }
                    return Ok(format!("PostMessage操作({:?})を実行しました（SendInput不使用）", action));
                }
            }
        }
        return Ok(format!(
            "PostMessageモード中はショートカット({:?})を送信できません（標準 または ゲーム中セーフで有効）",
            action
        ));
    }

    if guard.should_avoid_send_input(already_allowed) {
        return Ok(format!(
            "セーフモード中のためスキップしました: Discordを前面にして実行するか、送信方式を確認してください ({:?})",
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
