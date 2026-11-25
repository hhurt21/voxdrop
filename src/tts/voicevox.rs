//! Zero-extra-dependency local HTTP client for VOICEVOX ENGINE (`http://127.0.0.1:50021`)
//! built directly on native Windows WinHTTP (`Win32_Networking_WinHttp`).

use crate::config::VoicevoxConfig;
use crate::tts::TtsEngine;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicevoxSpeaker {
    pub id: u32,
    pub name: String,
    pub style_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBaseUrl {
    is_https: bool,
    host: String,
    port: u16,
    base_path: String,
}

/// Parses `http://127.0.0.1:50021` (or custom host/port/https URL) into WinHTTP connection parts.
fn parse_base_url(raw: &str) -> Result<ParsedBaseUrl, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let (is_https, rest, default_port) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r, 443u16)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r, 80u16)
    } else if !trimmed.is_empty() && !trimmed.contains("://") {
        (false, trimmed, 50021u16)
    } else {
        return Err(format!("VOICEVOX URL の形式が不正です: {}", raw));
    };

    let (authority, path_part) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p.trim_matches('/'))),
        None => (rest, String::new()),
    };

    if authority.is_empty() {
        return Err("VOICEVOX のホスト名が空です".into());
    }

    let (host, port) = if let Some((h, p_str)) = authority.rsplit_once(':') {
        let parsed_port = p_str
            .parse::<u16>()
            .map_err(|_| format!("VOICEVOX ポート番号が不正です: {}", p_str))?;
        (h.to_string(), parsed_port)
    } else {
        (authority.to_string(), default_port)
    };

    let base_path = if path_part == "/" {
        String::new()
    } else {
        path_part
    };

    Ok(ParsedBaseUrl {
        is_https,
        host,
        port,
        base_path,
    })
}

/// RFC 3986 UTF-8 percent-encoding for VOICEVOX query parameters (`?text=...`).
pub fn percent_encode_utf8(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Applies user VOICEVOX voice tuning scales onto `/audio_query` JSON without touching unknown fields.
pub fn apply_voicevox_params(query: &mut serde_json::Value, cfg: &VoicevoxConfig) {
    if let Some(obj) = query.as_object_mut() {
        obj.insert(
            "speedScale".into(),
            serde_json::json!(cfg.speed_scale.clamp(0.5, 2.0)),
        );
        obj.insert(
            "pitchScale".into(),
            serde_json::json!(cfg.pitch_scale.clamp(-0.15, 0.15)),
        );
        obj.insert(
            "intonationScale".into(),
            serde_json::json!(cfg.intonation_scale.clamp(0.0, 2.0)),
        );
        obj.insert(
            "volumeScale".into(),
            serde_json::json!(cfg.volume_scale.clamp(0.0, 2.0)),
        );
    }
}

/// Parses VOICEVOX `/speakers` JSON response into a flat `(id, name, style_name)` list.
pub fn parse_speakers_json(raw: &[u8]) -> Result<Vec<VoicevoxSpeaker>, String> {
    let items: Vec<serde_json::Value> = serde_json::from_slice(raw)
        .map_err(|e| format!("VOICEVOX /speakers JSON の解析に失敗しました: {}", e))?;
    let mut out = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("不明")
            .to_string();
        if let Some(styles) = item.get("styles").and_then(|v| v.as_array()) {
            for st in styles {
                let Some(id) = st.get("id").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let style_name = st
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ノーマル")
                    .to_string();
                out.push(VoicevoxSpeaker {
                    id: id as u32,
                    name: name.clone(),
                    style_name,
                });
            }
        }
    }
    Ok(out)
}

fn is_loopback_host(host: &str) -> bool {
    let l = host.to_ascii_lowercase();
    l == "127.0.0.1" || l == "localhost" || l == "::1" || l == "[::1]"
}

fn winhttp_bytes_request(
    base: &ParsedBaseUrl,
    method: &str,
    path_and_query: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout_ms: i32,
) -> Result<(u32, Vec<u8>), String> {
    let full_path = format!("{}{}", base.base_path, path_and_query);
    let method_w: Vec<u16> = method.encode_utf16().chain(std::iter::once(0)).collect();
    let host_w: Vec<u16> = base.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();

    let headers_w: Option<Vec<u16>> = content_type.map(|ct| {
        format!("Content-Type: {}\r\n", ct)
            .encode_utf16()
            .collect()
    });
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
            w!("VoxDrop/1.1"),
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
                    "VOICEVOX ({}:{}) への接続に失敗しました",
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
                    return Err("HTTP リクエストの作成に失敗しました".into());
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
                            "VOICEVOX に接続できません（{}:{} が起動しているか確認してください）",
                            base.host, base.port
                        )
                    })?;

                    WinHttpReceiveResponse(request, std::ptr::null_mut()).map_err(|_| {
                        "VOICEVOX からの応答受信に失敗しました（タイムアウトまたは切断）"
                            .to_string()
                    })?;

                    let mut status_code: u32 = 0;
                    let mut size = std::mem::size_of::<u32>() as u32;
                    WinHttpQueryHeaders(
                        request,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        PCWSTR::null(),
                        Some(&mut status_code as *mut u32 as *mut c_void),
                        &mut size,
                        std::ptr::null_mut(),
                    )
                    .map_err(|e| format!("HTTP ステータス取得に失敗しました: {}", e))?;

                    let mut resp_bytes = Vec::new();
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
                        resp_bytes.extend_from_slice(&chunk[..read as usize]);
                        // Cap at 32 MB for safety (long WAV files are typically < 3 MB)
                        if resp_bytes.len() > 32 * 1024 * 1024 {
                            break;
                        }
                    }

                    Ok((status_code, resp_bytes))
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

/// Client for local VOICEVOX ENGINE REST API.
#[derive(Debug, Default, Clone)]
pub struct VoicevoxClient;

impl VoicevoxClient {
    pub fn new() -> Self {
        Self
    }

    /// `GET /version` (2s timeout). Returns trimmed version string (e.g. `"0.21.0"`).
    pub fn check_health(&self, base_url: &str) -> Result<String, String> {
        let base = parse_base_url(base_url)?;
        let (status, bytes) = winhttp_bytes_request(&base, "GET", "/version", None, None, 2000)?;
        if !(200..300).contains(&status) {
            return Err(format!("VOICEVOX /version が HTTP {} を返しました", status));
        }
        let raw = String::from_utf8_lossy(&bytes);
        let ver = raw.trim().trim_matches('"').to_string();
        Ok(if ver.is_empty() { "unknown".into() } else { ver })
    }

    /// `GET /speakers` (3s timeout). Dynamically fetches character names & style IDs.
    pub fn get_speakers(&self, base_url: &str) -> Result<Vec<VoicevoxSpeaker>, String> {
        let base = parse_base_url(base_url)?;
        let (status, bytes) = winhttp_bytes_request(&base, "GET", "/speakers", None, None, 3000)?;
        if !(200..300).contains(&status) {
            return Err(format!("VOICEVOX /speakers が HTTP {} を返しました", status));
        }
        parse_speakers_json(&bytes)
    }

    /// `POST /audio_query` (5s) -> override scales -> `POST /synthesis` (10s) -> WAV bytes.
    pub fn synthesize_wav(&self, text: &str, cfg: &VoicevoxConfig) -> Result<Vec<u8>, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("読み上げテキストが空です".into());
        }
        let base = parse_base_url(&cfg.base_url)?;
        let encoded = percent_encode_utf8(trimmed);
        let query_path = format!("/audio_query?text={}&speaker={}", encoded, cfg.speaker_id);

        let (q_status, q_bytes) =
            winhttp_bytes_request(&base, "POST", &query_path, None, Some(b""), 5000)?;
        if !(200..300).contains(&q_status) {
            return Err(format!(
                "VOICEVOX /audio_query が HTTP {} を返しました（話者ID {} を確認してください）",
                q_status, cfg.speaker_id
            ));
        }

        let mut query_json: serde_json::Value = serde_json::from_slice(&q_bytes)
            .map_err(|e| format!("AudioQuery JSON の解析に失敗しました: {}", e))?;
        apply_voicevox_params(&mut query_json, cfg);
        let synth_body = serde_json::to_vec(&query_json)
            .map_err(|e| format!("AudioQuery JSON のシリアライズに失敗しました: {}", e))?;

        let synth_path = format!("/synthesis?speaker={}", cfg.speaker_id);
        let (s_status, wav_bytes) = winhttp_bytes_request(
            &base,
            "POST",
            &synth_path,
            Some("application/json; charset=utf-8"),
            Some(&synth_body),
            10_000,
        )?;
        if !(200..300).contains(&s_status) {
            return Err(format!(
                "VOICEVOX /synthesis が HTTP {} を返しました",
                s_status
            ));
        }
        if wav_bytes.len() < 44 {
            return Err("VOICEVOX から有効な WAV データが返されませんでした".into());
        }
        Ok(wav_bytes)
    }
}

impl TtsEngine for VoicevoxClient {
    fn synthesize(&self, text: &str, cfg: &VoicevoxConfig) -> Result<Vec<u8>, String> {
        self.synthesize_wav(text, cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_and_https_base_urls() {
        let p = parse_base_url("http://127.0.0.1:50021/").unwrap();
        assert!(!p.is_https);
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 50021);
        assert_eq!(p.base_path, "");

        let p2 = parse_base_url("https://voice.example.com:8443/api").unwrap();
        assert!(p2.is_https);
        assert_eq!(p2.host, "voice.example.com");
        assert_eq!(p2.port, 8443);
        assert_eq!(p2.base_path, "/api");
    }

    #[test]
    fn percent_encodes_japanese_utf8_cleanly() {
        assert_eq!(percent_encode_utf8("abc-1.0"), "abc-1.0");
        assert_eq!(percent_encode_utf8("あ"), "%E3%81%82");
        assert_eq!(percent_encode_utf8("a b"), "a%20b");
    }

    #[test]
    fn overrides_audio_query_scales_without_dropping_unknown_fields() {
        let mut q = serde_json::json!({
            "accent_phrases": [{"moras": []}],
            "speedScale": 1.0,
            "pitchScale": 0.0,
            "intonationScale": 1.0,
            "volumeScale": 1.0,
            "outputSamplingRate": 24000,
            "customFutureField": "keep_me"
        });
        let cfg = VoicevoxConfig {
            speed_scale: 1.3,
            pitch_scale: 0.05,
            intonation_scale: 1.2,
            volume_scale: 0.8,
            ..Default::default()
        };
        apply_voicevox_params(&mut q, &cfg);
        assert_eq!(q["customFutureField"], "keep_me");
        assert_eq!(q["outputSamplingRate"], 24000);
        assert!((q["speedScale"].as_f64().unwrap() - 1.3).abs() < 1e-5);
        assert!((q["pitchScale"].as_f64().unwrap() - 0.05).abs() < 1e-5);
        assert!((q["intonationScale"].as_f64().unwrap() - 1.2).abs() < 1e-5);
        assert!((q["volumeScale"].as_f64().unwrap() - 0.8).abs() < 1e-5);
    }

    #[test]
    fn parses_voicevox_speakers_payload() {
        let sample = r#"[
            {"name": "ずんだもん", "styles": [{"id": 3, "name": "ノーマル"}, {"id": 1, "name": "あまあま"}]},
            {"name": "四国めたん", "styles": [{"id": 2, "name": "ノーマル"}]}
        ]"#
        .as_bytes();
        let list = parse_speakers_json(sample).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, 3);
        assert_eq!(list[0].name, "ずんだもん");
        assert_eq!(list[0].style_name, "ノーマル");
        assert_eq!(list[2].id, 2);
    }
}
