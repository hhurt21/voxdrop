//! Local HTTP ASR client (`llama-server` Vulkan Qwen3-ASR-1.7B / OpenAI `/v1/audio/transcriptions`)
//! built directly on native Windows WinHTTP (`Win32_Networking_WinHttp`), plus optional headless background server launcher.

use crate::config::HttpAsrConfig;
use crate::stt::local::encode_wav_16k_mono;
use crate::stt::{SttEngine, TranscribeContext};
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static HTTP_ASR_PROC: Mutex<Option<Child>> = Mutex::new(None);
static HTTP_ASR_LAUNCH_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedUrl {
    is_https: bool,
    host: String,
    port: u16,
    base_path: String,
}

fn parse_url(raw: &str) -> Result<ParsedUrl, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let (is_https, rest, default_port) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r, 443u16)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r, 80u16)
    } else if !trimmed.is_empty() && !trimmed.contains("://") {
        (false, trimmed, 8080u16)
    } else {
        return Err(format!("HTTP ASR URL の形式が不正です: {}", raw));
    };

    let (authority, path_part) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p.trim_matches('/'))),
        None => (rest, String::new()),
    };

    if authority.is_empty() {
        return Err("HTTP ASR のホスト名が空です".into());
    }

    let (host, port) = if let Some((h, p_str)) = authority.rsplit_once(':') {
        let parsed_port = p_str
            .parse::<u16>()
            .map_err(|_| format!("HTTP ASR ポート番号が不正です: {}", p_str))?;
        let norm_h = if h.eq_ignore_ascii_case("localhost") {
            "127.0.0.1".to_string()
        } else {
            h.to_string()
        };
        (norm_h, parsed_port)
    } else {
        let norm_h = if authority.eq_ignore_ascii_case("localhost") {
            "127.0.0.1".to_string()
        } else {
            authority.to_string()
        };
        (norm_h, default_port)
    };

    let base_path = if path_part == "/" {
        String::new()
    } else {
        path_part
    };

    Ok(ParsedUrl {
        is_https,
        host,
        port,
        base_path,
    })
}

fn is_loopback_host(host: &str) -> bool {
    let l = host.to_ascii_lowercase();
    l == "127.0.0.1" || l == "localhost" || l == "::1" || l == "[::1]"
}

pub fn build_multipart_audio_body(
    boundary: &str,
    wav_bytes: &[u8],
    model: &str,
    language: &str,
    prompt: Option<&str>,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav_bytes.len() + 1024);
    let mut push_field = |name: &str, val: &str| {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
        );
        body.extend_from_slice(val.as_bytes());
        body.extend_from_slice(b"\r\n");
    };

    if !model.trim().is_empty() {
        push_field("model", model.trim());
    }
    if !language.trim().is_empty() {
        push_field("language", language.trim());
    }
    push_field("response_format", "json");
    if let Some(p) = prompt {
        if !p.trim().is_empty() {
            push_field("prompt", p.trim());
        }
    }

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"utterance.wav\"\r\nContent-Type: audio/wav\r\n\r\n",
    );
    body.extend_from_slice(wav_bytes);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
    body
}

pub fn parse_http_asr_response(raw: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.is_empty() {
        return Ok(String::new());
    }
    if text.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
                return Ok(t.trim().to_string());
            }
            if let Some(t) = val
                .get("data")
                .and_then(|d| d.get("text"))
                .and_then(|v| v.as_str())
            {
                return Ok(t.trim().to_string());
            }
            if let Some(arr) = val.get("results").and_then(|v| v.as_array()) {
                if let Some(first) = arr.first().and_then(|f| f.get("text")).and_then(|v| v.as_str()) {
                    return Ok(first.trim().to_string());
                }
            }
            if let Some(err) = val
                .get("error")
                .and_then(|e| e.get("message").or(Some(e)))
                .and_then(|v| v.as_str())
            {
                return Err(format!("HTTP ASR エラー応答: {}", err));
            }
        }
    }
    Ok(text)
}

fn winhttp_request(
    base: &ParsedUrl,
    method: &str,
    path: &str,
    extra_headers: &str,
    body: Option<&[u8]>,
    timeout_ms: i32,
) -> Result<(u32, Vec<u8>), String> {
    let normalized_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    let full_path = format!("{}{}", base.base_path, normalized_path);
    let method_w: Vec<u16> = method.encode_utf16().chain(std::iter::once(0)).collect();
    let host_w: Vec<u16> = base.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();
    let headers_w: Option<Vec<u16>> = if extra_headers.is_empty() {
        None
    } else {
        Some(extra_headers.encode_utf16().collect())
    };
    let body_bytes = body.unwrap_or(&[]);
    let access_type = if is_loopback_host(&base.host) {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY
    };
    let req_flags = if base.is_https {
        WINHTTP_FLAG_SECURE
    } else {
        Default::default()
    };

    unsafe {
        let session = WinHttpOpen(
            w!("VoxDrop/1.5"),
            access_type,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHTTP セッションの初期化に失敗しました".into());
        }
        let _ = WinHttpSetTimeouts(
            session,
            timeout_ms.min(2000),
            timeout_ms.min(3000),
            timeout_ms,
            timeout_ms,
        );

        let result = (|| -> Result<(u32, Vec<u8>), String> {
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), base.port, 0);
            if connect.is_null() {
                return Err(format!(
                    "ローカル HTTP ASR ({}:{}) に接続できません",
                    base.host, base.port
                ));
            }
            let req_res = (|| -> Result<(u32, Vec<u8>), String> {
                let request = WinHttpOpenRequest(
                    connect,
                    PCWSTR(method_w.as_ptr()),
                    PCWSTR(path_w.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    req_flags,
                );
                if request.is_null() {
                    return Err("HTTP ASR リクエストの作成に失敗しました".into());
                }
                let send_res = (|| -> Result<(u32, Vec<u8>), String> {
                    let body_ptr = if body_bytes.is_empty() {
                        None
                    } else {
                        Some(body_bytes.as_ptr() as *const c_void)
                    };
                    WinHttpSendRequest(
                        request,
                        headers_w.as_deref(),
                        body_ptr,
                        body_bytes.len() as u32,
                        body_bytes.len() as u32,
                        0,
                    )
                    .map_err(|_| {
                        format!(
                            "HTTP ASR サーバーに接続できません ({}:{} が起動しているか確認してください)",
                            base.host, base.port
                        )
                    })?;
                    WinHttpReceiveResponse(request, std::ptr::null_mut())
                        .map_err(|e| format!("HTTP ASR 応答待機エラー: {}", e))?;

                    let mut status: u32 = 0;
                    let mut status_len = std::mem::size_of::<u32>() as u32;
                    let _ = WinHttpQueryHeaders(
                        request,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        PCWSTR::null(),
                        Some(&mut status as *mut u32 as *mut c_void),
                        &mut status_len,
                        std::ptr::null_mut(),
                    );

                    let mut out = Vec::new();
                    loop {
                        let mut avail: u32 = 0;
                        if WinHttpQueryDataAvailable(request, &mut avail).is_err() || avail == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; avail as usize];
                        let mut read: u32 = 0;
                        if WinHttpReadData(
                            request,
                            chunk.as_mut_ptr() as *mut c_void,
                            avail,
                            &mut read,
                        )
                        .is_err()
                            || read == 0
                        {
                            break;
                        }
                        out.extend_from_slice(&chunk[..read as usize]);
                    }
                    Ok((status, out))
                })();
                let _ = WinHttpCloseHandle(request);
                send_res
            })();
            let _ = WinHttpCloseHandle(connect);
            req_res
        })();
        let _ = WinHttpCloseHandle(session);
        result
    }
}

pub fn check_http_asr_health(cfg: &HttpAsrConfig) -> Result<String, String> {
    let base = parse_url(&cfg.base_url)?;
    for probe in ["/health", "/v1/models", "/"] {
        if let Ok((status, _)) = winhttp_request(&base, "GET", probe, "", None, 3000) {
            if (200..400).contains(&status) {
                return Ok(format!(
                    "HTTP ASR サーバー応答OK ({}:{}, status={})",
                    base.host, base.port, status
                ));
            }
        }
    }
    Err(format!(
        "HTTP ASR サーバー ({}:{}) が応答しません",
        base.host, base.port
    ))
}

pub fn ensure_http_asr_running(cfg: &HttpAsrConfig) -> Result<String, String> {
    if check_http_asr_health(cfg).is_ok() {
        return Ok("HTTP ASR サーバーは既に起動しています".into());
    }
    let exe_str = cfg.server_exe_path.trim();
    if exe_str.is_empty() {
        return Err("HTTP ASR サーバーの実行ファイルパスが未設定です".into());
    }
    let exe_path = PathBuf::from(exe_str);
    if !exe_path.is_file() {
        return Err(format!(
            "HTTP ASR 実行ファイルが見つかりません: {}",
            exe_path.display()
        ));
    }

    let _guard = HTTP_ASR_LAUNCH_LOCK
        .lock()
        .map_err(|_| "HTTP ASR 起動ロック取得に失敗しました".to_string())?;
    if check_http_asr_health(cfg).is_ok() {
        return Ok("HTTP ASR サーバーは既に起動しています".into());
    }

    let alive = HTTP_ASR_PROC
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|c| matches!(c.try_wait(), Ok(None))))
        .unwrap_or(false);

    if !alive {
        let mut cmd = Command::new(&exe_path);
        if let Some(dir) = exe_path.parent() {
            cmd.current_dir(dir);
        }
        let parsed = parse_url(&cfg.base_url)?;
        if cfg.server_args.trim().is_empty() {
            cmd.args(["--host", &parsed.host, "--port", &parsed.port.to_string()]);
        } else {
            for part in cfg.server_args.split_whitespace() {
                cmd.arg(part);
            }
        }
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| format!("HTTP ASR サーバー起動エラー: {}", e))?;
        if let Ok(mut g) = HTTP_ASR_PROC.lock() {
            *g = Some(child);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        if let Ok(msg) = check_http_asr_health(cfg) {
            return Ok(msg);
        }
        let exited = HTTP_ASR_PROC
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|c| c.try_wait().ok().flatten()))
            .is_some();
        if exited {
            return Err("HTTP ASR サーバーが起動直後に終了しました（引数・ポートを確認してください）".into());
        }
    }
    Err("HTTP ASR サーバーを起動しましたが応答がありません（30秒タイムアウト）".into())
}

pub fn stop_http_asr_if_started_by_us() {
    if let Ok(mut g) = HTTP_ASR_PROC.lock() {
        if let Some(mut child) = g.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct LocalHttpAsrEngine {
    config: HttpAsrConfig,
    language: String,
    prompt_hints: Mutex<String>,
}

impl LocalHttpAsrEngine {
    pub fn new(config: HttpAsrConfig, language: &str) -> Self {
        Self {
            config,
            language: if language.trim().is_empty() {
                "ja".into()
            } else {
                language.trim().to_string()
            },
            prompt_hints: Mutex::new(String::new()),
        }
    }
}

impl SttEngine for LocalHttpAsrEngine {
    fn id(&self) -> &'static str {
        "local-http"
    }

    fn name(&self) -> String {
        let m = if self.config.model.trim().is_empty() {
            "Qwen3-ASR / OpenAI互換"
        } else {
            self.config.model.trim()
        };
        format!("Local HTTP ASR ({})", m)
    }

    fn device_name(&self) -> String {
        "Local HTTP Server (Vulkan/External)".into()
    }

    fn is_ready(&self) -> bool {
        !self.config.base_url.trim().is_empty()
    }

    fn set_prompt_hints(&self, hints: &str) {
        if let Ok(mut g) = self.prompt_hints.lock() {
            *g = hints.trim().to_string();
        }
    }

    fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        let ctx = TranscribeContext {
            language: self.language.clone(),
            hotwords_prompt: self.prompt_hints.lock().ok().map(|s| s.clone()),
            ..Default::default()
        };
        self.transcribe_with_context(samples_16k, &ctx)
    }

    fn transcribe_with_context(
        &self,
        samples_16k: &[f32],
        ctx: &TranscribeContext,
    ) -> Result<String, String> {
        if samples_16k.len() < 1600 {
            return Ok(String::new());
        }
        let base = parse_url(&self.config.base_url)?;
        let endpoint = if self.config.endpoint_path.trim().is_empty() {
            "/v1/audio/transcriptions"
        } else {
            self.config.endpoint_path.trim()
        };

        let conditioned =
            crate::audio::enhance::enhance_utterance_16k(samples_16k, ctx.radio_clarity, true);
        let wav_bytes = encode_wav_16k_mono(&conditioned);

        let boundary = "----VoxDropBoundary7F9A2C";
        let stored_prompt = self.prompt_hints.lock().ok().map(|s| s.clone());
        let prompt_ref = ctx
            .hotwords_prompt
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| stored_prompt.as_deref().filter(|s| !s.trim().is_empty()));
        let lang = if ctx.language.trim().is_empty() {
            self.language.as_str()
        } else {
            ctx.language.trim()
        };

        let body = build_multipart_audio_body(
            boundary,
            &wav_bytes,
            &self.config.model,
            lang,
            prompt_ref,
        );

        let mut headers = format!(
            "Content-Type: multipart/form-data; boundary={}\r\n",
            boundary
        );
        if !self.config.api_key.trim().is_empty() {
            headers.push_str(&format!(
                "Authorization: Bearer {}\r\n",
                self.config.api_key.trim()
            ));
        }

        let (status, resp) = winhttp_request(&base, "POST", endpoint, &headers, Some(&body), 30_000)?;
        if !(200..300).contains(&status) {
            let err_txt = String::from_utf8_lossy(&resp);
            return Err(format!(
                "HTTP ASR エラー (status={}): {}",
                status,
                err_txt.trim()
            ));
        }
        parse_http_asr_response(&resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_asr_urls_and_json_responses() {
        let u = parse_url("http://localhost:8080/api").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.base_path, "/api");

        assert_eq!(
            parse_http_asr_response(r#"{"text": " こんにちは世界 " }"#.as_bytes()).unwrap(),
            "こんにちは世界"
        );
        assert_eq!(
            parse_http_asr_response(r#"{"results": [{"text": "テスト"}]}"#.as_bytes()).unwrap(),
            "テスト"
        );
        assert!(parse_http_asr_response(br#"{"error":{"message":"bad audio"}}"#).is_err());
    }

    #[test]
    fn builds_valid_multipart_form_data() {
        let wav = b"RIFF1234WAVE";
        let body = build_multipart_audio_body("B123", wav, "qwen3-asr-1.7b", "ja", Some("Discord"));
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("--B123\r\n"));
        assert!(s.contains("name=\"model\"\r\n\r\nqwen3-asr-1.7b"));
        assert!(s.contains("name=\"prompt\"\r\n\r\nDiscord"));
        assert!(s.contains("filename=\"utterance.wav\""));
        assert!(s.contains("RIFF1234WAVE"));
        assert!(s.ends_with("--B123--\r\n"));
    }
}
