use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Muted,
    Listening,
    Speaking,
    Sleeping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ToggleListening,
    ToggleOverlay,
    TogglePanicSafeMode,
    ShowMainWindow,
    Quit,
}

pub struct TrayManager {
    tray_icon: TrayIcon,
    toggle_item: MenuItem,
    overlay_item: MenuItem,
    panic_item: MenuItem,
    icon_muted: Icon,
    icon_listening: Icon,
    icon_speaking: Icon,
    icon_sleeping: Icon,
}

impl TrayManager {
    pub fn new<F>(on_action: F) -> Result<Self, String>
    where
        F: Fn(TrayAction) + Send + Sync + 'static,
    {
        let menu = Menu::new();
        let toggle_item = MenuItem::new("録音開始 (OFF)", true, None);
        let overlay_item = MenuItem::new("ミニHUD表示切替", true, None);
        let panic_item = MenuItem::new("配信セーフモード (パニックミュート): OFF", true, None);
        let show_item = MenuItem::new("設定画面を開く", true, None);
        let quit_item = MenuItem::new("VoxDrop を終了", true, None);

        let toggle_id = toggle_item.id().clone();
        let overlay_id = overlay_item.id().clone();
        let panic_id = panic_item.id().clone();
        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();

        menu.append(&toggle_item).map_err(|e| e.to_string())?;
        menu.append(&overlay_item).map_err(|e| e.to_string())?;
        menu.append(&panic_item).map_err(|e| e.to_string())?;
        menu.append(&show_item).map_err(|e| e.to_string())?;
        menu.append(&PredefinedMenuItem::separator())
            .map_err(|e| e.to_string())?;
        menu.append(&quit_item).map_err(|e| e.to_string())?;

        let icon_muted = build_badge_icon([114, 118, 125], [220, 221, 222])?;
        let icon_listening = build_badge_icon([35, 165, 90], [255, 255, 255])?;
        let icon_speaking = build_badge_icon([88, 101, 242], [255, 255, 255])?;
        let icon_sleeping = build_badge_icon([240, 178, 50], [30, 31, 34])?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("VoxDrop - 待機中 (OFF)")
            .with_icon(icon_muted.clone())
            .build()
            .map_err(|e| format!("システムトレイの作成に失敗しました: {}", e))?;

        let on_action = std::sync::Arc::new(on_action);
        let on_menu = on_action.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id: &MenuId = &event.id;
            if *id == toggle_id {
                on_menu(TrayAction::ToggleListening);
            } else if *id == overlay_id {
                on_menu(TrayAction::ToggleOverlay);
            } else if *id == panic_id {
                on_menu(TrayAction::TogglePanicSafeMode);
            } else if *id == show_id {
                on_menu(TrayAction::ShowMainWindow);
            } else if *id == quit_id {
                on_menu(TrayAction::Quit);
            }
        }));

        let on_tray = on_action;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                on_tray(TrayAction::ShowMainWindow);
            }
        }));

        Ok(Self {
            tray_icon,
            toggle_item,
            overlay_item,
            panic_item,
            icon_muted,
            icon_listening,
            icon_speaking,
            icon_sleeping,
        })
    }

    pub fn update_state(&self, state: TrayState, overlay_visible: bool) {
        let (icon, tooltip, toggle_label) = match state {
            TrayState::Muted => (
                &self.icon_muted,
                "VoxDrop - 停止中 (OFF)",
                "録音開始 (現在: OFF)",
            ),
            TrayState::Listening => (
                &self.icon_listening,
                "VoxDrop - 録音待機中 (ON)",
                "録音停止 (現在: ON)",
            ),
            TrayState::Speaking => (
                &self.icon_speaking,
                "VoxDrop - 発話認識中...",
                "録音停止 (現在: 認識中)",
            ),
            TrayState::Sleeping => (
                &self.icon_sleeping,
                "VoxDrop - 音声スリープ中",
                "録音停止 (現在: スリープ)",
            ),
        };
        let _ = self.tray_icon.set_icon(Some(icon.clone()));
        let _ = self.tray_icon.set_tooltip(Some(tooltip));
        self.toggle_item.set_text(toggle_label);
        self.overlay_item.set_text(if overlay_visible {
            "ミニHUDを非表示"
        } else {
            "ミニHUDを表示"
        });
    }

    pub fn set_panic_mode(&self, active: bool) {
        self.panic_item.set_text(if active {
            "配信セーフモード (パニックミュート): ON [解除]"
        } else {
            "配信セーフモード (パニックミュート): OFF"
        });
    }
}

/// Generates a crisp 32x32 RGBA circular microphone-style icon in memory.
pub fn generate_badge_rgba(bg: [u8; 3], fg: [u8; 3]) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let cx = 15.5_f32;
    let cy = 15.5_f32;
    let radius = 14.0_f32;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * SIZE + x) * 4;

            if dist <= radius {
                // Simple anti-aliased edge
                let alpha = ((radius - dist).clamp(0.0, 1.0) * 255.0) as u8;
                // Draw a crisp microphone glyph in the center
                let in_capsule = (dx.abs() <= 3.2 && dy >= -7.5 && dy <= 0.5)
                    || ((dx * dx + (dy + 7.5).powi(2)).sqrt() <= 3.2)
                    || ((dx * dx + (dy - 0.5).powi(2)).sqrt() <= 3.2);
                let u_dist = (dx * dx + (dy - 0.5).powi(2)).sqrt();
                let in_arc = dy >= 0.5 && (5.2..=7.0).contains(&u_dist);
                let in_stem = dx.abs() <= 1.1 && dy >= 6.5 && dy <= 10.2;
                let in_base = dx.abs() <= 4.2 && (9.2..=11.0).contains(&dy);

                let color = if in_capsule || in_arc || in_stem || in_base {
                    fg
                } else {
                    bg
                };

                rgba[idx] = color[0];
                rgba[idx + 1] = color[1];
                rgba[idx + 2] = color[2];
                rgba[idx + 3] = alpha;
            }
        }
    }
    rgba
}

fn build_badge_icon(bg: [u8; 3], fg: [u8; 3]) -> Result<Icon, String> {
    let rgba = generate_badge_rgba(bg, fg);
    Icon::from_rgba(rgba, 32, 32).map_err(|e| format!("アイコン生成に失敗しました: {}", e))
}
