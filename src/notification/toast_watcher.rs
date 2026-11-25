//! Official Windows UI Automation (`IUIAutomation`) Toast Notification Watcher.
//! Monitors `UIA_Window_WindowOpenedEventId` on a dedicated MTA COM background thread
//! to capture Windows 10/11 toast notification text safely with zero process injection.

use crate::automation::window::extract_exe_name;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows::core::{implement, Ref, Result as WinResult, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationEventHandler,
    IUIAutomationEventHandler_Impl, TreeScope_Descendants, TreeScope_Subtree,
    UIA_ControlTypePropertyId, UIA_TextControlTypeId, UIA_Window_WindowOpenedEventId,
    UIA_EVENT_ID,
};

#[derive(Debug, Clone)]
pub struct RawToastNotification {
    pub window_name: String,
    pub class_name: String,
    pub automation_id: String,
    pub app_name_hint: String,
    pub texts: Vec<String>,
}

#[implement(IUIAutomationEventHandler)]
struct ToastEventHandler {
    uia: IUIAutomation,
    active: Arc<AtomicBool>,
    callback: Arc<dyn Fn(RawToastNotification) + Send + Sync>,
}

impl IUIAutomationEventHandler_Impl for ToastEventHandler_Impl {
    fn HandleAutomationEvent(
        &self,
        sender: Ref<'_, IUIAutomationElement>,
        eventid: UIA_EVENT_ID,
    ) -> WinResult<()> {
        if eventid != UIA_Window_WindowOpenedEventId || !self.active.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Some(elem) = sender.as_ref() else {
            return Ok(());
        };

        let class_name = unsafe { elem.CurrentClassName() }
            .map(|b| b.to_string())
            .unwrap_or_default();
        let window_name = unsafe { elem.CurrentName() }
            .map(|b| b.to_string())
            .unwrap_or_default();
        let automation_id = unsafe { elem.CurrentAutomationId() }
            .map(|b| b.to_string())
            .unwrap_or_default();

        if !looks_like_windows_toast(elem, &class_name, &window_name, &automation_id) {
            return Ok(());
        }

        // Extract Text descendants (retry once after 45ms if Win11 XAML toast hasn't populated yet)
        let mut texts = unsafe { extract_toast_texts(&self.uia, elem) };
        if texts.len() < 2 {
            thread::sleep(Duration::from_millis(45));
            let retry = unsafe { extract_toast_texts(&self.uia, elem) };
            if retry.len() > texts.len() {
                texts = retry;
            }
        }

        if texts.is_empty() {
            return Ok(());
        }

        let app_name_hint = detect_app_hint(&window_name, &texts);
        (self.callback)(RawToastNotification {
            window_name,
            class_name,
            automation_id,
            app_name_hint,
            texts,
        });
        Ok(())
    }
}

fn looks_like_windows_toast(
    elem: &IUIAutomationElement,
    class_name: &str,
    window_name: &str,
    automation_id: &str,
) -> bool {
    let win_lower = window_name.to_ascii_lowercase();
    let aid_lower = automation_id.to_ascii_lowercase();

    // Direct match on toast AutomationId
    if aid_lower.contains("toast") {
        return true;
    }

    // Standard Windows 10/11 toast popup class is `Windows.UI.Core.CoreWindow`
    let is_core_window = class_name == "Windows.UI.Core.CoreWindow";
    let name_is_notification = window_name.contains("新しい通知")
        || win_lower.contains("new notification")
        || window_name == "通知"
        || win_lower == "notification";

    if is_core_window && name_is_notification {
        return true;
    }

    if is_core_window {
        if let Ok(pid) = unsafe { elem.CurrentProcessId() } {
            if let Some(proc_name) = query_process_exe_name(pid as u32) {
                let p_lower = proc_name.to_ascii_lowercase();
                if p_lower == "shellexperiencehost.exe" || p_lower == "shellhost.exe" {
                    return true;
                }
            }
        }
    }

    false
}

fn query_process_exe_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
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
            extract_exe_name(&full)
        } else {
            None
        }
    }
}

unsafe fn extract_toast_texts(uia: &IUIAutomation, root: &IUIAutomationElement) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(text_cond) = uia.CreatePropertyCondition(
        UIA_ControlTypePropertyId,
        &VARIANT::from(UIA_TextControlTypeId.0),
    ) else {
        return out;
    };
    let Ok(arr) = root.FindAll(TreeScope_Descendants, &text_cond) else {
        return out;
    };
    let len = arr.Length().unwrap_or(0).min(32);
    for i in 0..len {
        if let Ok(item) = arr.GetElement(i) {
            if let Ok(name) = item.CurrentName() {
                let s = name.to_string();
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
        }
    }
    out
}

fn detect_app_hint(window_name: &str, texts: &[String]) -> String {
    if window_name.to_ascii_lowercase().contains("discord") {
        return "Discord".into();
    }
    for t in texts {
        let l = t.trim().to_ascii_lowercase();
        if l == "discord" || l.starts_with("discord ") {
            return t.trim().to_string();
        }
    }
    String::new()
}

/// Handle to the background UIA toast watcher thread.
pub struct ToastWatcher {
    active: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl ToastWatcher {
    pub fn spawn<F>(initially_active: bool, on_toast: F) -> Self
    where
        F: Fn(RawToastNotification) + Send + Sync + 'static,
    {
        let active = Arc::new(AtomicBool::new(initially_active));
        let shutdown = Arc::new(AtomicBool::new(false));

        let active_bg = active.clone();
        let shutdown_bg = shutdown.clone();
        let callback: Arc<dyn Fn(RawToastNotification) + Send + Sync> = Arc::new(on_toast);

        thread::spawn(move || unsafe {
            let com_inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let uia_res: WinResult<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);

            match uia_res {
                Ok(uia) => {
                    if let Ok(root) = uia.GetRootElement() {
                        let handler: IUIAutomationEventHandler = ToastEventHandler {
                            uia: uia.clone(),
                            active: active_bg,
                            callback,
                        }
                        .into();

                        if let Err(err) = uia.AddAutomationEventHandler(
                            UIA_Window_WindowOpenedEventId,
                            &root,
                            TreeScope_Subtree,
                            None,
                            &handler,
                        ) {
                            eprintln!("[voxdrop toast watcher error] AddAutomationEventHandler: {}", err);
                        } else {
                            while !shutdown_bg.load(Ordering::Relaxed) {
                                thread::sleep(Duration::from_millis(150));
                            }
                            let _ = uia.RemoveAutomationEventHandler(
                                UIA_Window_WindowOpenedEventId,
                                &root,
                                &handler,
                            );
                        }
                    }
                }
                Err(err) => {
                    eprintln!("[voxdrop toast watcher error] IUIAutomation init: {}", err);
                }
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
