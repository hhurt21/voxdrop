//! Passive read-only Windows Push Notification (`wpndatabase.db`) watcher.
//! Uses Windows' built-in `winsqlite3.dll` (`SQLITE_OPEN_READONLY`) to observe incoming Discord
//! toast records written by Windows `WpnService`, even when Windows Focus Assist / Do Not Disturb
//! (おやすみモード), fullscreen games, taskbar minimization, or tray-hidden mode suppress visual banners.

use crate::notification::toast_watcher::RawToastNotification;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;

type FnSqlite3OpenV2 =
    unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type FnSqlite3PrepareV2 = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    c_int,
    *mut *mut c_void,
    *mut *const c_char,
) -> c_int;
type FnSqlite3BindInt64 = unsafe extern "C" fn(*mut c_void, c_int, i64) -> c_int;
type FnSqlite3Step = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnSqlite3ColumnInt64 = unsafe extern "C" fn(*mut c_void, c_int) -> i64;
type FnSqlite3ColumnText = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
type FnSqlite3ColumnBlob = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_void;
type FnSqlite3ColumnBytes = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type FnSqlite3Finalize = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnSqlite3Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnSqlite3BusyTimeout = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

struct WinSqliteFns {
    _hmod: HMODULE,
    open_v2: FnSqlite3OpenV2,
    prepare_v2: FnSqlite3PrepareV2,
    bind_int64: FnSqlite3BindInt64,
    step: FnSqlite3Step,
    column_int64: FnSqlite3ColumnInt64,
    column_text: FnSqlite3ColumnText,
    column_blob: FnSqlite3ColumnBlob,
    column_bytes: FnSqlite3ColumnBytes,
    finalize: FnSqlite3Finalize,
    close: FnSqlite3Close,
    busy_timeout: Option<FnSqlite3BusyTimeout>,
}

unsafe impl Send for WinSqliteFns {}
unsafe impl Sync for WinSqliteFns {}

unsafe fn load_sym<T>(hmod: HMODULE, name: &'static [u8]) -> Option<T> {
    let proc = GetProcAddress(hmod, PCSTR(name.as_ptr()))?;
    Some(std::mem::transmute_copy(&proc))
}

impl WinSqliteFns {
    fn load() -> Option<Self> {
        let candidates = [
            r"C:\Windows\System32\winsqlite3.dll",
            "winsqlite3.dll",
        ];
        for path in candidates {
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            if let Ok(hmod) = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) } {
                unsafe {
                    if let (
                        Some(open_v2),
                        Some(prepare_v2),
                        Some(bind_int64),
                        Some(step),
                        Some(column_int64),
                        Some(column_text),
                        Some(column_blob),
                        Some(column_bytes),
                        Some(finalize),
                        Some(close),
                    ) = (
                        load_sym(hmod, b"sqlite3_open_v2\0"),
                        load_sym(hmod, b"sqlite3_prepare_v2\0"),
                        load_sym(hmod, b"sqlite3_bind_int64\0"),
                        load_sym(hmod, b"sqlite3_step\0"),
                        load_sym(hmod, b"sqlite3_column_int64\0"),
                        load_sym(hmod, b"sqlite3_column_text\0"),
                        load_sym(hmod, b"sqlite3_column_blob\0"),
                        load_sym(hmod, b"sqlite3_column_bytes\0"),
                        load_sym(hmod, b"sqlite3_finalize\0"),
                        load_sym(hmod, b"sqlite3_close\0"),
                    ) {
                        return Some(Self {
                            _hmod: hmod,
                            open_v2,
                            prepare_v2,
                            bind_int64,
                            step,
                            column_int64,
                            column_text,
                            column_blob,
                            column_bytes,
                            finalize,
                            close,
                            busy_timeout: load_sym(hmod, b"sqlite3_busy_timeout\0"),
                        });
                    }
                }
            }
        }
        None
    }

    fn query_max_id(&self, db_path: &Path) -> Option<i64> {
        let c_path = CString::new(db_path.to_string_lossy().as_bytes()).ok()?;
        let sql = CString::new("SELECT COALESCE(MAX(Id), 0) FROM Notification").ok()?;
        unsafe {
            let mut db: *mut c_void = std::ptr::null_mut();
            if (self.open_v2)(c_path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, std::ptr::null())
                != SQLITE_OK
                || db.is_null()
            {
                if !db.is_null() {
                    (self.close)(db);
                }
                return None;
            }
            if let Some(bt) = self.busy_timeout {
                bt(db, 200);
            }
            let mut stmt: *mut c_void = std::ptr::null_mut();
            let mut out = None;
            if (self.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
                == SQLITE_OK
                && !stmt.is_null()
            {
                if (self.step)(stmt) == SQLITE_ROW {
                    out = Some((self.column_int64)(stmt, 0));
                }
                (self.finalize)(stmt);
            }
            (self.close)(db);
            out
        }
    }

    fn query_new_discord_rows(
        &self,
        db_path: &Path,
        since_id: i64,
    ) -> Option<(i64, Vec<(String, String)>)> {
        let c_path = CString::new(db_path.to_string_lossy().as_bytes()).ok()?;
        let sql = CString::new(
            "SELECT n.Id, h.PrimaryId, n.Payload \
             FROM Notification n \
             JOIN NotificationHandler h ON n.HandlerId = h.RecordId \
             WHERE n.Id > ?1 AND lower(h.PrimaryId) LIKE '%discord%' \
             ORDER BY n.Id ASC LIMIT 24",
        )
        .ok()?;
        let max_sql = CString::new("SELECT COALESCE(MAX(Id), 0) FROM Notification").ok()?;

        unsafe {
            let mut db: *mut c_void = std::ptr::null_mut();
            if (self.open_v2)(c_path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, std::ptr::null())
                != SQLITE_OK
                || db.is_null()
            {
                if !db.is_null() {
                    (self.close)(db);
                }
                return None;
            }
            if let Some(bt) = self.busy_timeout {
                bt(db, 200);
            }

            let mut max_id = since_id;
            let mut mstmt: *mut c_void = std::ptr::null_mut();
            if (self.prepare_v2)(db, max_sql.as_ptr(), -1, &mut mstmt, std::ptr::null_mut())
                == SQLITE_OK
                && !mstmt.is_null()
            {
                if (self.step)(mstmt) == SQLITE_ROW {
                    let m = (self.column_int64)(mstmt, 0);
                    if m > max_id {
                        max_id = m;
                    }
                }
                (self.finalize)(mstmt);
            }

            let mut rows = Vec::new();
            let mut stmt: *mut c_void = std::ptr::null_mut();
            if (self.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
                == SQLITE_OK
                && !stmt.is_null()
            {
                (self.bind_int64)(stmt, 1, since_id);
                while (self.step)(stmt) == SQLITE_ROW {
                    let row_id = (self.column_int64)(stmt, 0);
                    if row_id > max_id {
                        max_id = row_id;
                    }
                    let pid_ptr = (self.column_text)(stmt, 1);
                    let handler_id = if pid_ptr.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(pid_ptr).to_string_lossy().into_owned()
                    };
                    let blob_ptr = (self.column_blob)(stmt, 2);
                    let blob_len = (self.column_bytes)(stmt, 2);
                    let xml = if !blob_ptr.is_null() && blob_len > 0 {
                        let slice = std::slice::from_raw_parts(
                            blob_ptr as *const u8,
                            blob_len as usize,
                        );
                        String::from_utf8_lossy(slice).into_owned()
                    } else {
                        String::new()
                    };
                    if !xml.trim().is_empty() {
                        rows.push((handler_id, xml));
                    }
                }
                (self.finalize)(stmt);
            }

            (self.close)(db);
            Some((max_id, rows))
        }
    }
}

fn default_wpn_db_path() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(local)
        .join("Microsoft")
        .join("Windows")
        .join("Notifications")
        .join("wpndatabase.db");
    Some(p)
}

fn file_stamp(path: &Path) -> (u64, Option<SystemTime>) {
    match fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    }
}

fn unescape_xml(raw: &str) -> String {
    let mut s = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");

    // Numeric XML character entities (e.g. &#10; &#x1F600;)
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find("&#") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find(';') {
            let num_str = &after[..end];
            let decoded = if let Some(hex) = num_str.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else {
                num_str.parse::<u32>().ok().and_then(char::from_u32)
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[end + 1..];
                continue;
            }
        }
        out.push_str("&#");
        rest = after;
    }
    out.push_str(rest);
    s = out;
    s
}

/// Parses a Windows Toast XML payload emitted by Discord into `RawToastNotification`.
pub fn parse_wpn_toast_xml(handler_primary_id: &str, xml: &str) -> Option<RawToastNotification> {
    let mut texts = Vec::new();
    let mut rest = xml;
    while let Some(tag_start) = rest.find("<text") {
        let after_tag = &rest[tag_start + 5..];
        let Some(gt_rel) = after_tag.find('>') else {
            break;
        };
        let open_end = gt_rel;
        if open_end > 0 && after_tag.as_bytes()[open_end - 1] == b'/' {
            rest = &after_tag[open_end + 1..];
            continue;
        }
        let body_start = &after_tag[open_end + 1..];
        let Some(close_rel) = body_start.find("</text>") else {
            break;
        };
        let inner = unescape_xml(&body_start[..close_rel]);
        let trimmed = inner.trim();
        if !trimmed.is_empty() {
            texts.push(trimmed.to_string());
        }
        rest = &body_start[close_rel + 7..];
    }

    if texts.len() < 2 {
        return None;
    }

    // Check `<toast launch="...">` for Discord `/channels/@me/` vs `/channels/<guild_id>/`
    let lower_xml = xml.to_ascii_lowercase();
    let app_name_hint = if lower_xml.contains("/channels/@me/") {
        "Discord [dm]".to_string()
    } else if has_numeric_guild_channel_url(&lower_xml) {
        "Discord [server]".to_string()
    } else if handler_primary_id.to_ascii_lowercase().contains("discord") {
        "Discord".to_string()
    } else {
        String::new()
    };

    if app_name_hint.is_empty() {
        return None;
    }

    Some(RawToastNotification {
        window_name: "WpnNotification".into(),
        class_name: "WpnDatabase".into(),
        automation_id: handler_primary_id.to_string(),
        app_name_hint,
        texts,
    })
}

fn has_numeric_guild_channel_url(lower_xml: &str) -> bool {
    let mut rest = lower_xml;
    while let Some(idx) = rest.find("/channels/") {
        let after = &rest[idx + 10..];
        let first_seg = after.split(['/', '"', '\'', '&', ' ']).next().unwrap_or("");
        if first_seg.len() >= 15 && first_seg.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        rest = after;
    }
    false
}

/// Polls Windows' `wpndatabase.db` in the background so notifications suppressed by
/// Focus Assist / Do Not Disturb (おやすみモード) or hidden/minimized Discord are captured.
pub struct WpnWatcher {
    active: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl WpnWatcher {
    pub fn spawn<F>(initially_active: bool, on_toast: F) -> Self
    where
        F: Fn(RawToastNotification) + Send + Sync + 'static,
    {
        let active = Arc::new(AtomicBool::new(initially_active));
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_bg = active.clone();
        let shutdown_bg = shutdown.clone();
        let cb = Arc::new(on_toast);

        thread::spawn(move || {
            let Some(sqlite) = WinSqliteFns::load() else {
                return;
            };
            let Some(db_path) = default_wpn_db_path() else {
                return;
            };
            let wal_path = db_path.with_file_name("wpndatabase.db-wal");

            let mut last_id = sqlite.query_max_id(&db_path).unwrap_or(0);
            let mut last_db_stamp = file_stamp(&db_path);
            let mut last_wal_stamp = file_stamp(&wal_path);
            let mut was_active = initially_active;
            let mut ticks_since_query: u32 = 0;

            while !shutdown_bg.load(Ordering::Relaxed) {
                let is_active = active_bg.load(Ordering::Relaxed);
                if !is_active {
                    was_active = false;
                    ticks_since_query = 0;
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                if !was_active {
                    // Refresh baseline when newly enabled so old messages don't replay
                    if let Some(m) = sqlite.query_max_id(&db_path) {
                        last_id = m;
                    }
                    last_db_stamp = file_stamp(&db_path);
                    last_wal_stamp = file_stamp(&wal_path);
                    ticks_since_query = 0;
                    was_active = true;
                }

                ticks_since_query = ticks_since_query.saturating_add(1);
                let cur_db = file_stamp(&db_path);
                let cur_wal = file_stamp(&wal_path);
                if cur_db != last_db_stamp || cur_wal != last_wal_stamp || ticks_since_query >= 4 {
                    if let Some((new_max, rows)) = sqlite.query_new_discord_rows(&db_path, last_id) {
                        last_db_stamp = cur_db;
                        last_wal_stamp = cur_wal;
                        ticks_since_query = 0;
                        if new_max > last_id {
                            last_id = new_max;
                        }
                        for (handler_id, xml) in rows {
                            if let Some(raw) = parse_wpn_toast_xml(&handler_id, &xml) {
                                cb(raw);
                            }
                        }
                    }
                }

                thread::sleep(Duration::from_millis(650));
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
    fn parses_wpn_discord_toast_xml_and_unescapes_entities() {
        let dm_xml = r#"<toast launch="discord://-/channels/@me/123456789012345678/999"><visual><binding template="ToastGeneric"><text>Taro</text><text>A &amp; B &lt;3&#10;元気？</text></binding></visual></toast>"#;
        let dm = parse_wpn_toast_xml("com.squirrel.Discord.Discord", dm_xml).expect("dm");
        assert_eq!(dm.app_name_hint, "Discord [dm]");
        assert_eq!(dm.texts, vec!["Taro", "A & B <3\n元気？"]);

        let srv_xml = r#"<toast launch="https://discord.com/channels/112233445566778899/998877665544332211"><visual><binding template="ToastGeneric"><text>Hanako (#雑談, メイン鯖)</text><text>よろしく！</text></binding></visual></toast>"#;
        let srv = parse_wpn_toast_xml("com.squirrel.Discord.Discord", srv_xml).expect("srv");
        assert_eq!(srv.app_name_hint, "Discord [server]");
        assert_eq!(srv.texts, vec!["Hanako (#雑談, メイン鯖)", "よろしく！"]);

        let canary_dm_xml = "<toast><visual><binding template=\"ToastGeneric\"><text>suicune</text><text>送信されると思います。\r\n送信されてます。</text><image placement=\"appLogoOverride\" src=\"C:\\Users\\test\\avatar.png\"/></binding></visual><audio silent=\"true\"/></toast>";
        let canary = parse_wpn_toast_xml("com.squirrel.DiscordCanary.DiscordCanary", canary_dm_xml)
            .expect("canary");
        assert_eq!(canary.app_name_hint, "Discord");
        assert_eq!(
            canary.texts,
            vec!["suicune", "送信されると思います。\r\n送信されてます。"]
        );
    }
}
