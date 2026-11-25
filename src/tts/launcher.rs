//! Headless VOICEVOX ENGINE auto-launcher (beast-tools style):
//! detects `run.exe` (window-less engine) and starts it in the background with `CREATE_NO_WINDOW`,
//! falling back to the GUI `VOICEVOX.exe` only when no engine is found. Stops the engine on exit
//! only if VoxDrop started it.

use crate::tts::voicevox::VoicevoxClient;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static ENGINE_PROC: Mutex<Option<Child>> = Mutex::new(None);
static LAUNCH_LOCK: Mutex<()> = Mutex::new(());

/// Finds a VOICEVOX executable. Returns `(path, is_headless_engine)`.
pub fn detect_voicevox_path() -> Option<(PathBuf, bool)> {
    let mut headless: Vec<PathBuf> = Vec::new();
    let mut gui: Vec<PathBuf> = Vec::new();

    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = Path::new(&appdata).join("voicevox").join("vvpp-engines");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut found: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("VOICEVOX_Engine"))
                        .unwrap_or(false)
                })
                .map(|p| p.join("run.exe"))
                .filter(|p| p.is_file())
                .collect();
            found.sort();
            found.reverse(); // newest-looking first
            headless.extend(found);
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        roots.push(Path::new(&l).join("Programs").join("VOICEVOX"));
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(p) = std::env::var(var) {
            roots.push(Path::new(&p).join("VOICEVOX"));
        }
    }
    for base in roots {
        headless.push(base.join("vv-engine").join("run.exe"));
        gui.push(base.join("VOICEVOX.exe"));
    }

    if let Some(p) = headless.into_iter().find(|p| p.is_file()) {
        return Some((p, true));
    }
    gui.into_iter().find(|p| p.is_file()).map(|p| (p, false))
}

fn host_port(base_url: &str) -> (String, u16) {
    let rest = base_url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = rest.split('/').next().unwrap_or("127.0.0.1:50021");
    let (h, p) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(50021)),
        None => (authority.to_string(), 50021),
    };
    let host = if h.eq_ignore_ascii_case("localhost") {
        "127.0.0.1".to_string()
    } else {
        h
    };
    (host, p)
}

/// Ensures VOICEVOX ENGINE is responding at `base_url`; launches it headlessly if not.
/// Serialized via `LAUNCH_LOCK` so concurrent calls never spawn duplicate engines.
pub fn ensure_running(base_url: &str, configured_path: &str) -> Result<String, String> {
    let client = VoicevoxClient::new();
    if client.check_health(base_url).is_ok() {
        return Ok("VOICEVOX は既に起動しています".into());
    }
    let _launch_guard = LAUNCH_LOCK
        .lock()
        .map_err(|_| "VOICEVOX 起動ロックの取得に失敗しました".to_string())?;
    if client.check_health(base_url).is_ok() {
        return Ok("VOICEVOX は既に起動しています".into());
    }

    // If we already spawned one that is still alive, just wait for it.
    let already_alive = ENGINE_PROC
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|c| matches!(c.try_wait(), Ok(None))))
        .unwrap_or(false);

    if !already_alive {
        let cfg_path = PathBuf::from(configured_path.trim());
        let (exe, headless) = if !configured_path.trim().is_empty() && cfg_path.is_file() {
            let is_engine = cfg_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case("run.exe"))
                .unwrap_or(false);
            if is_engine {
                (cfg_path, true)
            } else {
                // Prefer a window-less engine over the GUI app when available
                detect_voicevox_path()
                    .filter(|(_, h)| *h)
                    .unwrap_or((cfg_path, false))
            }
        } else {
            detect_voicevox_path().ok_or_else(|| {
                "VOICEVOX が見つかりません（インストールするか、実行ファイルの場所を指定してください）"
                    .to_string()
            })?
        };

        let mut cmd = Command::new(&exe);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        if headless {
            let (host, port) = host_port(base_url);
            cmd.args(["--host", &host, "--port", &port.to_string()]);
            cmd.creation_flags(CREATE_NO_WINDOW);
            cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("VOICEVOX の起動に失敗しました ({}): {}", exe.display(), e))?;
        if let Ok(mut g) = ENGINE_PROC.lock() {
            *g = Some(child);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(ver) = client.check_health(base_url) {
            return Ok(format!("VOICEVOX をバックグラウンドで起動しました (v{})", ver));
        }
        let exited = ENGINE_PROC
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|c| c.try_wait().ok().flatten()))
            .is_some();
        if exited {
            return Err("VOICEVOX エンジンが起動直後に終了しました（実行ファイルやポート競合を確認してください）".into());
        }
    }
    Err("VOICEVOX を起動しましたが応答がありません（45秒タイムアウト）".into())
}

/// Stops the engine only if VoxDrop started it.
pub fn stop_if_started_by_us() {
    if let Ok(mut g) = ENGINE_PROC.lock() {
        if let Some(mut child) = g.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port_from_base_url() {
        assert_eq!(host_port("http://127.0.0.1:50021"), ("127.0.0.1".into(), 50021));
        assert_eq!(host_port("http://localhost:50121/"), ("127.0.0.1".into(), 50121));
        assert_eq!(host_port("127.0.0.1"), ("127.0.0.1".into(), 50021));
    }
}
