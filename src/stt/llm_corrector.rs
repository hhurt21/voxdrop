//! Local LLM "Slurred Speech & Mishearing" Phonetic Context Post-Corrector (#3, #11, #34, #37).
//! Talks cleanly to OpenAI-compatible `llama-server` (Vulkan / CPU, zero CUDA) via WinHTTP.

use crate::config::{DictionaryEntry, LlmCorrectionConfig, SpeechStyle};
use crate::processor::phonetic::to_normalized_mora;
use crate::stt::http_asr::parse_url;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static OWNED_LLM_CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub struct LlmCorrectionRequest<'a> {
    pub primary_text: &'a str,
    pub secondary_text: Option<&'a str>,
    pub recent_messages: &'a [String],
    pub target_label: &'a str,
    pub game_name: Option<&'a str>,
    pub dictionary: &'a [DictionaryEntry],
    pub style: SpeechStyle,
}

fn build_system_prompt(style: SpeechStyle) -> &'static str {
    match style {
        SpeechStyle::Normal => {
            "あなたは日本語音声入力の最小限誤字補正器です。音韻（読み）と直前文脈・辞書から空耳や同音異義語だけを最小限で直し、意味や語尾は変えず補正後テキストのみを出力してください。説明や引用符は出力しないでください。"
        }
        SpeechStyle::Casual => {
            "あなたは日本語チャット補正器です。音韻と文脈から誤認識を最小限で直し、親しい友人向けの自然なタメ口・チャット調に整えてテキストのみを出力してください。"
        }
        SpeechStyle::Polite => {
            "あなたは日本語チャット補正器です。誤認識を最小限で直し、丁寧語（です・ます調）に整えてテキストのみを出力してください。"
        }
        SpeechStyle::GameShort => {
            "あなたはFPS・チームゲーム通話テキスト補正器です。誤認識を直し、簡潔で読みやすい短いゲーム報告テキストのみを出力してください。"
        }
    }
}

pub fn build_user_prompt(req: &LlmCorrectionRequest<'_>) -> String {
    let mut parts = Vec::new();
    let kana_hint = to_normalized_mora(req.primary_text);
    parts.push(format!("第一認識(STT1): {}", req.primary_text.trim()));
    if let Some(sec) = req.secondary_text {
        if !sec.trim().is_empty() && sec.trim() != req.primary_text.trim() {
            parts.push(format!("第二認識(STT2合議用): {}", sec.trim()));
        }
    }
    if !kana_hint.is_empty() {
        parts.push(format!("読み目安: {}", kana_hint));
    }
    if !req.target_label.trim().is_empty() {
        parts.push(format!("宛先: {}", req.target_label.trim()));
    }
    if let Some(g) = req.game_name {
        if !g.trim().is_empty() {
            parts.push(format!("アクティブゲーム: {}", g.trim()));
        }
    }
    if !req.recent_messages.is_empty() {
        let ctx: Vec<&str> = req.recent_messages.iter().rev().take(3).map(|s| s.as_str()).collect();
        parts.push(format!("直近発言: {}", ctx.join(" / ")));
    }
    if !req.dictionary.is_empty() {
        let dict_hint: Vec<String> = req
            .dictionary
            .iter()
            .take(15)
            .map(|d| format!("{}->{}", d.from, d.to))
            .collect();
        parts.push(format!("辞書ヒント: {}", dict_hint.join(", ")));
    }
    parts.push("出力（補正後テキストのみ）:".into());
    parts.join("\n")
}

pub fn parse_chat_completion_response(body: &[u8]) -> Result<String, String> {
    let val: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("LLM JSON解析失敗: {}", e))?;
    if let Some(err) = val.get("error").and_then(|e| e.get("message")).and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    let text = val
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim_matches('「')
        .trim_matches('」')
        .trim()
        .to_string();
    Ok(text)
}

pub fn correct_transcript_with_llm(
    cfg: &LlmCorrectionConfig,
    req: &LlmCorrectionRequest<'_>,
) -> Result<String, String> {
    if !cfg.enabled || req.primary_text.trim().is_empty() {
        return Ok(req.primary_text.to_string());
    }
    let parsed = parse_url(&cfg.base_url)?;
    let ep = cfg.endpoint_path.trim();
    let ep = if ep.is_empty() {
        "/v1/chat/completions".to_string()
    } else if ep.starts_with('/') {
        ep.to_string()
    } else {
        format!("/{}", ep)
    };
    let full_path = if parsed.base_path.is_empty() {
        ep
    } else {
        format!("{}{}", parsed.base_path.trim_end_matches('/'), ep)
    };

    let payload = serde_json::json!({
        "model": if cfg.model.trim().is_empty() { "qwen3-1.7b" } else { cfg.model.trim() },
        "messages": [
            { "role": "system", "content": build_system_prompt(req.style) },
            { "role": "user", "content": build_user_prompt(req) }
        ],
        "temperature": 0.1,
        "max_tokens": 180
    });
    let body_bytes = serde_json::to_vec(&payload).map_err(|e| format!("JSON生成失敗: {}", e))?;

    let ua_w: Vec<u16> = "VoxDrop-LLM/1.6\0".encode_utf16().collect();
    let host_w: Vec<u16> = parsed.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();
    let timeout_ms = cfg.timeout_ms.clamp(250, 10_000) as i32;

    let mut header_str = String::from("Content-Type: application/json\r\n");
    if !cfg.api_key.trim().is_empty() {
        header_str.push_str(&format!("Authorization: Bearer {}\r\n", cfg.api_key.trim()));
    }
    let headers_w: Vec<u16> = header_str.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(ua_w.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("LLM WinHTTP session初期化失敗".into());
        }

        let out = (|| -> Result<(u32, Vec<u8>), String> {
            let _ = WinHttpSetTimeouts(session, 500, 1000, timeout_ms, timeout_ms);
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), parsed.port, 0);
            if connect.is_null() {
                return Err("LLM WinHttpConnect失敗".into());
            }
            let flags = if parsed.is_https {
                WINHTTP_FLAG_SECURE
            } else {
                Default::default()
            };
            let req_h = WinHttpOpenRequest(
                connect,
                w!("POST"),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if req_h.is_null() {
                let _ = WinHttpCloseHandle(connect);
                return Err("LLM OpenRequest失敗".into());
            }

            let _ = WinHttpAddRequestHeaders(
                req_h,
                &headers_w[..headers_w.len().saturating_sub(1)],
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );

            WinHttpSendRequest(
                req_h,
                None,
                Some(body_bytes.as_ptr() as *const c_void),
                body_bytes.len() as u32,
                body_bytes.len() as u32,
                0,
            )
            .map_err(|e| format!("LLM送信失敗: {}", e))?;

            WinHttpReceiveResponse(req_h, std::ptr::null_mut())
                .map_err(|e| format!("LLM応答待機失敗: {}", e))?;

            let mut status: u32 = 0;
            let mut slen = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req_h,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut slen,
                std::ptr::null_mut(),
            );

            let mut resp = Vec::new();
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req_h, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let mut chunk = vec![0u8; avail as usize];
                let mut read: u32 = 0;
                if WinHttpReadData(
                    req_h,
                    chunk.as_mut_ptr() as *mut c_void,
                    avail,
                    &mut read,
                )
                .is_err()
                    || read == 0
                {
                    break;
                }
                resp.extend_from_slice(&chunk[..read as usize]);
            }
            let _ = WinHttpCloseHandle(req_h);
            let _ = WinHttpCloseHandle(connect);
            Ok((status, resp))
        })();
        let _ = WinHttpCloseHandle(session);

        let (status, bytes) = out?;
        if status >= 400 {
            return Err(format!("LLM HTTP {}", status));
        }
        let corrected = parse_chat_completion_response(&bytes)?;
        if corrected.is_empty() {
            Ok(req.primary_text.to_string())
        } else {
            Ok(corrected)
        }
    }
}

pub fn check_llm_health(base_url: &str) -> Result<String, String> {
    let parsed = parse_url(base_url)?;
    let health_path = if parsed.base_path.is_empty() {
        "/health".to_string()
    } else {
        format!("{}/health", parsed.base_path.trim_end_matches('/'))
    };
    let ua_w: Vec<u16> = "VoxDrop-LLM/1.6\0".encode_utf16().collect();
    let host_w: Vec<u16> = parsed.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = health_path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(ua_w.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("WinHTTP 初期化失敗".into());
        }
        let _ = WinHttpSetTimeouts(session, 400, 600, 800, 800);
        let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), parsed.port, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return Err("接続失敗".into());
        }
        let flags = if parsed.is_https {
            WINHTTP_FLAG_SECURE
        } else {
            Default::default()
        };
        let req_h = WinHttpOpenRequest(
            connect,
            w!("GET"),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        );
        let ok = if !req_h.is_null() {
            let sent = WinHttpSendRequest(req_h, None, None, 0, 0, 0).is_ok()
                && WinHttpReceiveResponse(req_h, std::ptr::null_mut()).is_ok();
            let mut status: u32 = 0;
            let mut slen = std::mem::size_of::<u32>() as u32;
            if sent {
                let _ = WinHttpQueryHeaders(
                    req_h,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    PCWSTR::null(),
                    Some(&mut status as *mut u32 as *mut c_void),
                    &mut slen,
                    std::ptr::null_mut(),
                );
            }
            let _ = WinHttpCloseHandle(req_h);
            sent && status < 500
        } else {
            false
        };
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        if ok {
            Ok(format!("ローカルLLM接続OK ({})", parsed.host))
        } else {
            Err("ローカルLLMサーバーに接続できませんでした".into())
        }
    }
}

pub fn ensure_llm_server_running(cfg: &LlmCorrectionConfig) -> Result<String, String> {
    if check_llm_health(&cfg.base_url).is_ok() {
        return Ok("ローカルLLMサーバーは既に稼働中です".into());
    }
    let exe = cfg.server_exe_path.trim();
    if exe.is_empty() || !Path::new(exe).is_file() {
        return Err("ローカルLLMサーバー実行ファイルが見つかりません".into());
    }
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if !cfg.server_args.trim().is_empty() {
        for a in cfg.server_args.split_whitespace() {
            cmd.arg(a);
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("ローカルLLMサーバー起動失敗: {}", e))?;
    if let Ok(mut g) = OWNED_LLM_CHILD.lock() {
        *g = Some(child);
    }
    Ok("ローカルLLMサーバーをヘッドレス起動しました".into())
}

pub fn stop_llm_server_if_started_by_us() {
    if let Ok(mut g) = OWNED_LLM_CHILD.lock() {
        if let Some(mut c) = g.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_chat_completion_cleanly() {
        let json = r#"{"choices":[{"message":{"content":"「ヴァンダル買ってミッド見よう」"}}]}"#.as_bytes();
        let out = parse_chat_completion_response(json).unwrap();
        assert_eq!(out, "ヴァンダル買ってミッド見よう");
    }

    #[test]
    fn formats_user_prompt_with_dual_candidates_and_context() {
        let dict = vec![DictionaryEntry {
            from: "ばんだる".into(),
            to: "ヴァンダル".into(),
        }];
        let recent = vec!["次どうする？".into()];
        let req = LlmCorrectionRequest {
            primary_text: "ばんだるかう",
            secondary_text: Some("ヴァンダル買う"),
            recent_messages: &recent,
            target_label: "雑談 (#general)",
            game_name: Some("VALORANT"),
            dictionary: &dict,
            style: SpeechStyle::GameShort,
        };
        let p = build_user_prompt(&req);
        assert!(p.contains("STT1"));
        assert!(p.contains("STT2"));
        assert!(p.contains("VALORANT"));
        assert!(p.contains("ばんだる->ヴァンダル"));
    }
}
