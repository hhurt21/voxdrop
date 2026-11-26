use crate::config::HotkeyConfig;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleListening,
    PushToTalkDown,
    PushToTalkUp,
    ClearDraft,
    ToggleOverlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    ToggleListening,
    PushToTalk,
    ClearDraft,
    ToggleOverlay,
}

pub struct HotkeyManager {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    id_map: Arc<RwLock<HashMap<u32, SlotKind>>>,
    ptt_mouse_vk: Arc<AtomicI32>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl HotkeyManager {
    pub fn new<F>(initial_cfg: &HotkeyConfig, on_action: F) -> Result<(Self, Vec<String>), String>
    where
        F: Fn(HotkeyAction) + Send + Sync + 'static,
    {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| format!("グローバルホットキーマネージャの初期化に失敗しました: {}", e))?;
        let id_map = Arc::new(RwLock::new(HashMap::<u32, SlotKind>::new()));
        let on_action = Arc::new(on_action);

        let map_clone = id_map.clone();
        let cb_hk = on_action.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let kind = map_clone
                .read()
                .ok()
                .and_then(|m| m.get(&event.id).copied());
            let Some(kind) = kind else {
                return;
            };
            match (kind, event.state) {
                (SlotKind::ToggleListening, HotKeyState::Pressed) => {
                    cb_hk(HotkeyAction::ToggleListening);
                }
                (SlotKind::PushToTalk, HotKeyState::Pressed) => {
                    cb_hk(HotkeyAction::PushToTalkDown);
                }
                (SlotKind::PushToTalk, HotKeyState::Released) => {
                    cb_hk(HotkeyAction::PushToTalkUp);
                }
                (SlotKind::ClearDraft, HotKeyState::Pressed) => {
                    cb_hk(HotkeyAction::ClearDraft);
                }
                (SlotKind::ToggleOverlay, HotKeyState::Pressed) => {
                    cb_hk(HotkeyAction::ToggleOverlay);
                }
                _ => {}
            }
        }));

        let ptt_mouse_vk = Arc::new(AtomicI32::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let vk_poll = ptt_mouse_vk.clone();
        let shut_poll = shutdown.clone();
        let cb_mouse = on_action;
        thread::spawn(move || {
            let mut prev_down = false;
            while !shut_poll.load(Ordering::Relaxed) {
                let vk = vk_poll.load(Ordering::Relaxed);
                if vk == 0 {
                    if prev_down {
                        prev_down = false;
                        cb_mouse(HotkeyAction::PushToTalkUp);
                    }
                    thread::sleep(Duration::from_millis(120));
                    continue;
                }
                let down = unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
                if down && !prev_down {
                    prev_down = true;
                    cb_mouse(HotkeyAction::PushToTalkDown);
                } else if !down && prev_down {
                    prev_down = false;
                    cb_mouse(HotkeyAction::PushToTalkUp);
                }
                thread::sleep(Duration::from_millis(20));
            }
        });

        let mut this = Self {
            manager,
            registered: Vec::new(),
            id_map,
            ptt_mouse_vk,
            shutdown,
        };
        let warnings = this.reload(initial_cfg);
        Ok((this, warnings))
    }

    pub fn reload(&mut self, cfg: &HotkeyConfig) -> Vec<String> {
        for hk in self.registered.drain(..) {
            let _ = self.manager.unregister(hk);
        }
        let mut new_map = HashMap::new();
        let mut warnings = Vec::new();

        if let Some(vk) = parse_mouse_side_button(&cfg.push_to_talk) {
            self.ptt_mouse_vk.store(vk, Ordering::Relaxed);
        } else {
            self.ptt_mouse_vk.store(0, Ordering::Relaxed);
        }

        let specs = [
            (SlotKind::ToggleListening, "録音トグル", cfg.toggle_listening.as_str()),
            (SlotKind::PushToTalk, "Push-to-Talk", cfg.push_to_talk.as_str()),
            (SlotKind::ClearDraft, "緊急ドラフトクリア", cfg.clear_draft.as_str()),
            (SlotKind::ToggleOverlay, "ミニHUD切替", cfg.toggle_overlay.as_str()),
        ];

        for (slot, label, spec) in specs {
            let trimmed = spec.trim();
            if trimmed.is_empty() {
                continue;
            }
            if slot == SlotKind::PushToTalk && parse_mouse_side_button(trimmed).is_some() {
                continue;
            }
            match parse_hotkey(trimmed) {
                Some(hk) => {
                    if new_map.contains_key(&hk.id()) {
                        warnings.push(format!("ホットキー '{}' ({}) が重複しています", trimmed, label));
                        continue;
                    }
                    match self.manager.register(hk) {
                        Ok(()) => {
                            new_map.insert(hk.id(), slot);
                            self.registered.push(hk);
                        }
                        Err(e) => {
                            warnings.push(format!(
                                "ホットキー '{}' ({}) の登録に失敗: {}",
                                trimmed, label, e
                            ));
                        }
                    }
                }
                None => {
                    warnings.push(format!(
                        "ホットキー '{}' ({}) の形式を解釈できませんでした",
                        trimmed, label
                    ));
                }
            }
        }

        if let Ok(mut guard) = self.id_map.write() {
            *guard = new_map;
        }
        warnings
    }
}

/// Parses mouse side-button names (`"Mouse4"`/`"XButton1"` -> VK_XBUTTON1=0x05, `"Mouse5"`/`"XButton2"` -> VK_XBUTTON2=0x06).
pub fn parse_mouse_side_button(spec: &str) -> Option<i32> {
    match spec.trim().to_ascii_uppercase().as_str() {
        "MOUSE4" | "XBUTTON1" | "MB4" => Some(0x05),
        "MOUSE5" | "XBUTTON2" | "MB5" => Some(0x06),
        _ => None,
    }
}

/// Parses human-readable shortcut strings such as `"F8"`, `"Ctrl+Shift+X"`, `"Alt+Space"`.
pub fn parse_hotkey(spec: &str) -> Option<HotKey> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut mods = Modifiers::empty();
    let mut key_code: Option<Code> = None;

    for raw_part in trimmed.split('+') {
        let part = raw_part.trim().to_ascii_uppercase();
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "CTRL" | "CONTROL" => mods |= Modifiers::CONTROL,
            "SHIFT" => mods |= Modifiers::SHIFT,
            "ALT" => mods |= Modifiers::ALT,
            "WIN" | "SUPER" | "META" => mods |= Modifiers::SUPER,
            other => {
                if key_code.is_some() {
                    return None;
                }
                key_code = parse_code(other);
                key_code?;
            }
        }
    }

    let code = key_code?;
    let mods_opt = if mods.is_empty() { None } else { Some(mods) };
    Some(HotKey::new(mods_opt, code))
}

fn parse_code(token: &str) -> Option<Code> {
    Some(match token {
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        "A" => Code::KeyA,
        "B" => Code::KeyB,
        "C" => Code::KeyC,
        "D" => Code::KeyD,
        "E" => Code::KeyE,
        "F" => Code::KeyF,
        "G" => Code::KeyG,
        "H" => Code::KeyH,
        "I" => Code::KeyI,
        "J" => Code::KeyJ,
        "K" => Code::KeyK,
        "L" => Code::KeyL,
        "M" => Code::KeyM,
        "N" => Code::KeyN,
        "O" => Code::KeyO,
        "P" => Code::KeyP,
        "Q" => Code::KeyQ,
        "R" => Code::KeyR,
        "S" => Code::KeyS,
        "T" => Code::KeyT,
        "U" => Code::KeyU,
        "V" => Code::KeyV,
        "W" => Code::KeyW,
        "X" => Code::KeyX,
        "Y" => Code::KeyY,
        "Z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "SPACE" => Code::Space,
        "INSERT" | "INS" => Code::Insert,
        "HOME" => Code::Home,
        "END" => Code::End,
        "PAGEUP" | "PGUP" => Code::PageUp,
        "PAGEDOWN" | "PGDN" => Code::PageDown,
        "PAUSE" => Code::Pause,
        "SCROLLLOCK" => Code::ScrollLock,
        "`" | "BACKQUOTE" => Code::Backquote,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function_and_combo_hotkeys() {
        let f8 = parse_hotkey("F8").expect("F8");
        assert_eq!(f8, HotKey::new(None, Code::F8));

        let clear = parse_hotkey("Ctrl+Shift+X").expect("Ctrl+Shift+X");
        assert_eq!(
            clear,
            HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyX)
        );

        let ptt = parse_hotkey("Alt+Space").expect("Alt+Space");
        assert_eq!(ptt, HotKey::new(Some(Modifiers::ALT), Code::Space));

        assert!(parse_hotkey("").is_none());
        assert!(parse_hotkey("Ctrl+").is_none());
        assert!(parse_hotkey("UnknownKey").is_none());
    }

    #[test]
    fn parses_mouse_side_buttons() {
        assert_eq!(parse_mouse_side_button("Mouse4"), Some(0x05));
        assert_eq!(parse_mouse_side_button("xbutton1"), Some(0x05));
        assert_eq!(parse_mouse_side_button("MB4"), Some(0x05));
        assert_eq!(parse_mouse_side_button("Mouse5"), Some(0x06));
        assert_eq!(parse_mouse_side_button("XButton2"), Some(0x06));
        assert_eq!(parse_mouse_side_button("F9"), None);
    }
}
