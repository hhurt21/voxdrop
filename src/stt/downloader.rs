//! Resilient on-demand external model downloader into `models/` with:
//! - 3-tier merged catalog resolution (`overrides` > `cache` > `embedded`)
//! - WinHTTP URL health check (`HEAD` -> `GET Range: bytes=0-0` fallback) + auto mirror fallback
//! - WinHTTP Range-resume `.part` downloads, HTML error-page guard, exponential backoff retry
//! - Windows `GetDiskFreeSpaceExW` disk space pre-check & SHA-256 verification

use crate::stt::catalog::{
    embedded_catalog_entries, load_merged_catalog, ArchiveType, ModelUrlHealthDto,
    ResolvedCatalogModel,
};
use std::ffi::c_void;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_CONTENT_TYPE,
    WINHTTP_QUERY_CUSTOM, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDownloadSpec {
    pub id: String,
    pub url: String,
    pub mirror_urls: Vec<String>,
    pub filename: String,
    pub is_tar_bz2: bool,
    pub archive_type: ArchiveType,
    pub expected_sha256: Option<String>,
    pub expected_size_bytes: Option<u64>,
    pub download_size_mb: u32,
    pub skip_sha256: bool,
}

pub fn resolve_download_spec(
    model_id: &str,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
) -> Result<ModelDownloadSpec, String> {
    resolve_download_spec_with_entries(
        model_id,
        custom_url,
        custom_filename,
        embedded_catalog_entries()
            .into_iter()
            .map(|e| {
                let d = e.primary_url.clone();
                ResolvedCatalogModel {
                    entry: e,
                    default_url: d,
                    has_override: false,
                    skip_sha256: false,
                }
            })
            .collect(),
    )
}

pub fn resolve_download_spec_merged(
    model_id: &str,
    data_dir: &Path,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
) -> Result<ModelDownloadSpec, String> {
    let entries = load_merged_catalog(data_dir);
    resolve_download_spec_with_entries(model_id, custom_url, custom_filename, entries)
}

fn resolve_download_spec_with_entries(
    model_id: &str,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
    entries: Vec<ResolvedCatalogModel>,
) -> Result<ModelDownloadSpec, String> {
    let id = model_id.trim();
    if id == "custom" {
        let url = custom_url.unwrap_or("").trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("ダウンロードURLは http:// または https:// で指定してください".into());
        }
        let raw_name = custom_filename
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                url.split('?')
                    .next()
                    .unwrap_or(url)
                    .rsplit('/')
                    .next()
                    .unwrap_or("custom-model.bin")
            });
        let clean_name = sanitize_filename(raw_name)?;
        let lower = clean_name.to_ascii_lowercase();
        let archive_type = if lower.ends_with(".tar.bz2") {
            ArchiveType::TarBz2
        } else if lower.ends_with(".zip") {
            ArchiveType::Zip
        } else {
            ArchiveType::RawFile
        };
        return Ok(ModelDownloadSpec {
            id: "custom".into(),
            url: url.to_string(),
            mirror_urls: Vec::new(),
            filename: clean_name,
            is_tar_bz2: archive_type == ArchiveType::TarBz2,
            archive_type,
            expected_sha256: None,
            expected_size_bytes: None,
            download_size_mb: 0,
            skip_sha256: false,
        });
    }

    let found = entries
        .into_iter()
        .find(|r| r.entry.id == id)
        .ok_or_else(|| format!("未知のモデルIDです: {}", id))?;
    let e = found.entry;
    let url = custom_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(e.primary_url.trim())
        .to_string();
    if url.is_empty() {
        return Err(format!("モデル {} にはダウンロードURLが設定されていません", id));
    }
    let raw_name = if !e.local_rel_path.trim().is_empty() {
        match e.archive_type {
            ArchiveType::TarBz2 => format!(
                "{}.tar.bz2",
                e.local_rel_path.trim().trim_end_matches(".tar.bz2")
            ),
            ArchiveType::Zip => format!("{}.zip", e.local_rel_path.trim().trim_end_matches(".zip")),
            ArchiveType::RawFile => e.local_rel_path.trim().to_string(),
        }
    } else {
        url.split('?')
            .next()
            .unwrap_or(&url)
            .rsplit('/')
            .next()
            .unwrap_or("model.bin")
            .to_string()
    };
    let filename = sanitize_filename(
        custom_filename
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&raw_name),
    )?;

    Ok(ModelDownloadSpec {
        id: e.id,
        url,
        mirror_urls: e.mirror_urls,
        filename,
        is_tar_bz2: e.archive_type == ArchiveType::TarBz2,
        archive_type: e.archive_type,
        expected_sha256: e.sha256,
        expected_size_bytes: e.expected_size_bytes,
        download_size_mb: e.download_size_mb,
        skip_sha256: found.skip_sha256,
    })
}

/// Computes lowercase hex SHA-256 digest of a file cleanly in streaming 64KB chunks.
pub fn compute_file_sha256(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = fs::File::open(path).map_err(|e| format!("SHA-256 読み込み失敗: {}", e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("SHA-256 読み込み失敗: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

struct Sha256 {
    state: [u32; 8],
    len_bits: u64,
    buf: [u8; 64],
    buf_len: usize,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            len_bits: 0,
            buf: [0u8; 64],
            buf_len: 0,
        }
    }
    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(Self::K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g; g = f; f = e; e = d.wrapping_add(temp1);
            d = c; c = b; b = a; a = temp1.wrapping_add(temp2);
        }
        for (i, v) in [a, b, c, d, e, f, g, h].into_iter().enumerate() {
            self.state[i] = self.state[i].wrapping_add(v);
        }
    }
    fn update(&mut self, data: &[u8]) {
        self.len_bits = self.len_bits.wrapping_add((data.len() as u64) * 8);
        let mut idx = 0;
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            idx += take;
            if self.buf_len == 64 {
                let b = self.buf;
                self.compress(&b);
                self.buf_len = 0;
            }
        }
        while idx + 64 <= data.len() {
            let mut b = [0u8; 64];
            b.copy_from_slice(&data[idx..idx + 64]);
            self.compress(&b);
            idx += 64;
        }
        if idx < data.len() {
            let rem = data.len() - idx;
            self.buf[..rem].copy_from_slice(&data[idx..]);
            self.buf_len = rem;
        }
    }
    fn finalize_hex(mut self) -> String {
        let len_bits = self.len_bits;
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for b in &mut self.buf[self.buf_len..] { *b = 0; }
            let blk = self.buf;
            self.compress(&blk);
            self.buf_len = 0;
        }
        for b in &mut self.buf[self.buf_len..56] { *b = 0; }
        self.buf[56..64].copy_from_slice(&len_bits.to_be_bytes());
        let blk = self.buf;
        self.compress(&blk);
        self.state.iter().map(|w| format!("{:08x}", w)).collect()
    }
}

pub fn sanitize_filename(raw: &str) -> Result<String, String> {
    let name = Path::new(raw.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        return Err(format!("不正なファイル名です: {}", raw));
    }
    Ok(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlProbeResult {
    pub ok: bool,
    pub status_code: u32,
    pub content_length: Option<u64>,
    pub is_html: bool,
    pub message: String,
}

fn is_loopback(host: &str) -> bool {
    let l = host.to_ascii_lowercase();
    l == "127.0.0.1" || l == "localhost" || l == "::1" || l == "[::1]"
}

unsafe fn query_winhttp_string_header(req: *mut c_void, level: u32, name: Option<&[u16]>) -> Option<String> {
    let mut buf = vec![0u16; 512];
    let mut byte_len = (buf.len() * 2) as u32;
    let name_ptr = match name {
        Some(n) => PCWSTR(n.as_ptr()),
        None => PCWSTR::null(),
    };
    if WinHttpQueryHeaders(
        req,
        level,
        name_ptr,
        Some(buf.as_mut_ptr() as *mut c_void),
        &mut byte_len,
        std::ptr::null_mut(),
    )
    .is_ok()
    {
        let u16_len = (byte_len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..u16_len]).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn parse_total_from_content_range(cr: &str) -> Option<u64> {
    let (_, total_part) = cr.split_once('/')?;
    total_part.trim().parse::<u64>().ok()
}

fn probe_once_winhttp(url: &str, method: &str, range_zero: bool) -> Result<(u32, Option<u64>, String), String> {
    let parsed = crate::stt::http_asr::parse_url(url)?;
    let full_path = if parsed.base_path.is_empty() {
        "/".to_string()
    } else {
        parsed.base_path
    };
    let ua_w: Vec<u16> = "VoxDrop-UrlCheck/1.7\0".encode_utf16().collect();
    let method_w: Vec<u16> = method.encode_utf16().chain(std::iter::once(0)).collect();
    let host_w: Vec<u16> = parsed.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();
    let access = if is_loopback(&parsed.host) {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY
    };
    let flags = if parsed.is_https {
        WINHTTP_FLAG_SECURE
    } else {
        Default::default()
    };

    unsafe {
        let session = WinHttpOpen(PCWSTR(ua_w.as_ptr()), access, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err("WinHTTP 初期化失敗".into());
        }
        let out = (|| -> Result<(u32, Option<u64>, String), String> {
            let _ = WinHttpSetTimeouts(session, 4000, 5000, 8000, 8000);
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), parsed.port, 0);
            if connect.is_null() {
                return Err(format!("接続失敗: {}", parsed.host));
            }
            let req = WinHttpOpenRequest(
                connect,
                PCWSTR(method_w.as_ptr()),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if req.is_null() {
                let _ = WinHttpCloseHandle(connect);
                return Err("リクエスト作成失敗".into());
            }
            if range_zero {
                let hdr_w: Vec<u16> = "Range: bytes=0-0\r\n\0".encode_utf16().collect();
                let _ = WinHttpAddRequestHeaders(
                    req,
                    &hdr_w[..hdr_w.len().saturating_sub(1)],
                    WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                );
            }
            let send_res = WinHttpSendRequest(req, None, None, 0, 0, 0)
                .map_err(|e| format!("送信失敗: {}", e));
            if let Err(e) = send_res {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                return Err(e);
            }
            if let Err(e) = WinHttpReceiveResponse(req, std::ptr::null_mut()) {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                return Err(format!("応答失敗: {}", e));
            }

            let mut status: u32 = 0;
            let mut slen = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut slen,
                std::ptr::null_mut(),
            );
            let ctype = query_winhttp_string_header(req, WINHTTP_QUERY_CONTENT_TYPE, None)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let cr_name: Vec<u16> = "Content-Range\0".encode_utf16().collect();
            let content_len = if status == 206 {
                query_winhttp_string_header(req, WINHTTP_QUERY_CUSTOM, Some(&cr_name))
                    .and_then(|s| parse_total_from_content_range(&s))
            } else {
                query_winhttp_string_header(req, WINHTTP_QUERY_CONTENT_LENGTH, None)
                    .and_then(|s| s.parse::<u64>().ok())
            };
            let _ = WinHttpCloseHandle(req);
            let _ = WinHttpCloseHandle(connect);
            Ok((status, content_len, ctype))
        })();
        let _ = WinHttpCloseHandle(session);
        out
    }
}

/// Checks a URL via WinHTTP `HEAD`, automatically falling back to `GET + Range: bytes=0-0` on HTTP 405/403.
pub fn probe_url_health_winhttp(url: &str) -> UrlProbeResult {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return UrlProbeResult {
            ok: false,
            status_code: 0,
            content_length: None,
            is_html: false,
            message: "URLが空です".into(),
        };
    }

    let first = match probe_once_winhttp(trimmed, "HEAD", false) {
        Ok((405, _, _)) | Ok((403, _, _)) | Ok((400, _, _)) => {
            probe_once_winhttp(trimmed, "GET", true)
        }
        other => other,
    };

    match first {
        Ok((status, len, ctype)) => {
            let is_html = ctype.starts_with("text/html") || ctype.starts_with("application/xhtml");
            let ok = (status == 200 || status == 206) && !is_html;
            let size_txt = len
                .map(|b| format!(" ({})", crate::stt::catalog::format_bytes(b)))
                .unwrap_or_default();
            let msg = if is_html {
                format!("HTTP {} HTML応答を検出（バイナリ直リンクではありません）", status)
            } else if ok {
                format!("取得可能 HTTP {}{}", status, size_txt)
            } else {
                format!("HTTP {} エラー", status)
            };
            UrlProbeResult {
                ok,
                status_code: status,
                content_length: len,
                is_html,
                message: msg,
            }
        }
        Err(e) => UrlProbeResult {
            ok: false,
            status_code: 0,
            content_length: None,
            is_html: false,
            message: format!("接続エラー: {}", e),
        },
    }
}

pub fn check_model_url_health(model: &ResolvedCatalogModel) -> ModelUrlHealthDto {
    let p_url = model.entry.primary_url.trim();
    if p_url.is_empty() {
        return ModelUrlHealthDto {
            model_id: model.entry.id.clone(),
            status: "unknown".into(),
            http_code: 0,
            content_length: None,
            active_url: String::new(),
            message: if model.entry.bundled_default {
                "本体同梱モデル".into()
            } else {
                "URL未設定".into()
            },
        };
    }
    let primary_probe = probe_url_health_winhttp(p_url);
    if primary_probe.ok {
        return ModelUrlHealthDto {
            model_id: model.entry.id.clone(),
            status: "reachable".into(),
            http_code: primary_probe.status_code,
            content_length: primary_probe.content_length,
            active_url: p_url.to_string(),
            message: primary_probe.message,
        };
    }
    for m_url in &model.entry.mirror_urls {
        let m_probe = probe_url_health_winhttp(m_url);
        if m_probe.ok {
            return ModelUrlHealthDto {
                model_id: model.entry.id.clone(),
                status: "mirror_reachable".into(),
                http_code: m_probe.status_code,
                content_length: m_probe.content_length,
                active_url: m_url.clone(),
                message: format!("ミラー経由で取得可能 ({})", m_probe.message),
            };
        }
    }
    ModelUrlHealthDto {
        model_id: model.entry.id.clone(),
        status: "unreachable".into(),
        http_code: primary_probe.status_code,
        content_length: primary_probe.content_length,
        active_url: p_url.to_string(),
        message: format!("取得不可: {}", primary_probe.message),
    }
}

type FnGetDiskFreeSpaceExW = unsafe extern "system" fn(
    lp_directory_name: PCWSTR,
    lp_free_bytes_available_to_caller: *mut u64,
    lp_total_number_of_bytes: *mut u64,
    lp_total_number_of_free_bytes: *mut u64,
) -> i32;

pub fn check_free_disk_space_bytes(dir: &Path) -> Option<u64> {
    let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir_w: Vec<u16> = abs
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let hmod: HMODULE = LoadLibraryW(w!("kernel32.dll")).ok()?;
        let sym = GetProcAddress(hmod, PCSTR(b"GetDiskFreeSpaceExW\0".as_ptr()))?;
        let f: FnGetDiskFreeSpaceExW = std::mem::transmute(sym);
        let mut avail: u64 = 0;
        let mut total: u64 = 0;
        let mut free: u64 = 0;
        if f(PCWSTR(dir_w.as_ptr()), &mut avail, &mut total, &mut free) != 0 {
            Some(avail)
        } else {
            None
        }
    }
}

fn download_url_resumable_winhttp<F>(
    url: &str,
    tmp_path: &Path,
    model_id: &str,
    on_progress: &F,
) -> Result<(), String>
where
    F: Fn(&str, u8, &str),
{
    let parsed = crate::stt::http_asr::parse_url(url)?;
    let full_path = if parsed.base_path.is_empty() {
        "/".to_string()
    } else {
        parsed.base_path
    };
    let ua_w: Vec<u16> = "VoxDrop-Downloader/1.7\0".encode_utf16().collect();
    let host_w: Vec<u16> = parsed.host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = full_path.encode_utf16().chain(std::iter::once(0)).collect();
    let access = if is_loopback(&parsed.host) {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY
    };
    let flags = if parsed.is_https {
        WINHTTP_FLAG_SECURE
    } else {
        Default::default()
    };

    let existing_len = fs::metadata(tmp_path).map(|m| m.len()).unwrap_or(0);

    unsafe {
        let session = WinHttpOpen(PCWSTR(ua_w.as_ptr()), access, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err("WinHTTP 初期化失敗".into());
        }
        let res = (|| -> Result<(), String> {
            let _ = WinHttpSetTimeouts(session, 5000, 10_000, 15_000, 30_000);
            let connect = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), parsed.port, 0);
            if connect.is_null() {
                return Err(format!("サーバーへ接続できません: {}", parsed.host));
            }
            let req = WinHttpOpenRequest(
                connect,
                w!("GET"),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if req.is_null() {
                let _ = WinHttpCloseHandle(connect);
                return Err("GET リクエストの作成失敗".into());
            }

            if existing_len > 0 {
                let rhdr: Vec<u16> = format!("Range: bytes={}-\r\n\0", existing_len)
                    .encode_utf16()
                    .collect();
                let _ = WinHttpAddRequestHeaders(
                    req,
                    &rhdr[..rhdr.len().saturating_sub(1)],
                    WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                );
            }

            if let Err(e) = WinHttpSendRequest(req, None, None, 0, 0, 0) {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                return Err(format!("リクエスト送信失敗: {}", e));
            }
            if let Err(e) = WinHttpReceiveResponse(req, std::ptr::null_mut()) {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                return Err(format!("応答待機失敗: {}", e));
            }

            let mut status: u32 = 0;
            let mut slen = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut slen,
                std::ptr::null_mut(),
            );

            let ctype = query_winhttp_string_header(req, WINHTTP_QUERY_CONTENT_TYPE, None)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if ctype.starts_with("text/html") || ctype.starts_with("application/xhtml") {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                let _ = fs::remove_file(tmp_path);
                return Err(format!(
                    "HTMLエラーページ ({}) を受信したため保存を中止しました (HTTP {})",
                    ctype, status
                ));
            }

            if status == 416 && existing_len > 0 {
                // Range not satisfiable -> discard stale .part and retry cleanly
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                let _ = fs::remove_file(tmp_path);
                return Err("レジューム位置不一致 (HTTP 416)".into());
            }
            if status != 200 && status != 206 {
                let _ = WinHttpCloseHandle(req);
                let _ = WinHttpCloseHandle(connect);
                return Err(format!("ダウンロード応答エラー: HTTP {}", status));
            }

            let remaining_len = query_winhttp_string_header(req, WINHTTP_QUERY_CONTENT_LENGTH, None)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let resuming = status == 206 && existing_len > 0;
            let total_expected = if resuming {
                existing_len.saturating_add(remaining_len)
            } else {
                remaining_len
            };

            let mut file = if resuming {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(tmp_path)
                    .map_err(|e| format!("一時ファイル追記オープン失敗: {}", e))?
            } else {
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(tmp_path)
                    .map_err(|e| format!("一時ファイル作成失敗: {}", e))?
            };

            let mut written_total = if resuming { existing_len } else { 0 };
            let mut last_pct = 0u8;
            let mut buf = vec![0u8; 65_536];
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let to_read = (avail as usize).min(buf.len()) as u32;
                let mut read: u32 = 0;
                if WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, to_read, &mut read).is_err() {
                    let _ = WinHttpCloseHandle(req);
                    let _ = WinHttpCloseHandle(connect);
                    return Err("受信ストリームの読み取り中に切断されました".into());
                }
                if read == 0 {
                    break;
                }
                file.write_all(&buf[..read as usize])
                    .map_err(|e| format!("ディスク書き込み失敗: {}", e))?;
                written_total = written_total.saturating_add(read as u64);

                let pct = if total_expected > 0 {
                    10 + (((written_total as f64 / total_expected as f64) * 75.0) as u8).min(75)
                } else {
                    45
                };
                if pct >= last_pct + 5 || pct == 85 {
                    last_pct = pct;
                    on_progress(
                        model_id,
                        pct,
                        &format!(
                            "ダウンロード中... ({})",
                            crate::stt::catalog::format_bytes(written_total)
                        ),
                    );
                }
            }
            let _ = file.flush();
            let _ = WinHttpCloseHandle(req);
            let _ = WinHttpCloseHandle(connect);
            Ok(())
        })();
        let _ = WinHttpCloseHandle(session);
        res
    }
}

#[allow(dead_code)]
pub fn download_model_into<F>(
    model_id: &str,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
    target_dir: &Path,
    on_progress: F,
) -> Result<PathBuf, String>
where
    F: Fn(&str, u8, &str),
{
    download_model_into_with_data(
        model_id,
        custom_url,
        custom_filename,
        target_dir,
        Path::new("."),
        on_progress,
    )
}

pub fn download_model_into_with_data<F>(
    model_id: &str,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
    target_dir: &Path,
    data_dir: &Path,
    on_progress: F,
) -> Result<PathBuf, String>
where
    F: Fn(&str, u8, &str),
{
    let spec = resolve_download_spec_merged(model_id, data_dir, custom_url, custom_filename)?;
    fs::create_dir_all(target_dir)
        .map_err(|e| format!("モデル保存先フォルダの作成に失敗しました: {}", e))?;

    // Check 1.2x free disk space before downloading
    let est_bytes = spec
        .expected_size_bytes
        .unwrap_or_else(|| (spec.download_size_mb as u64) * 1024 * 1024);
    if est_bytes > 0 {
        let req_bytes = ((est_bytes as f64) * 1.2) as u64;
        if let Some(avail) = check_free_disk_space_bytes(target_dir) {
            if avail < req_bytes {
                return Err(format!(
                    "ディスク空き容量不足です（必要目安: {}, 空き: {}）",
                    crate::stt::catalog::format_bytes(req_bytes),
                    crate::stt::catalog::format_bytes(avail)
                ));
            }
        }
    }

    let dest_path = target_dir.join(&spec.filename);
    let tmp_path = target_dir.join(format!("{}.part", spec.filename));

    let mut url_candidates = vec![spec.url.clone()];
    for m in &spec.mirror_urls {
        if !m.trim().is_empty() && !url_candidates.contains(m) {
            url_candidates.push(m.clone());
        }
    }

    let mut actual_sha = String::new();
    let mut last_err = String::from("ダウンロードに失敗しました");
    let backoff_secs = [1u64, 3, 7];

    let mut downloaded_ok = false;
    for (u_idx, url) in url_candidates.iter().enumerate() {
        for attempt in 0..3usize {
            let start_msg = if u_idx > 0 && attempt == 0 {
                format!("ミラーURLへフォールバックして取得中: {}", spec.filename)
            } else if attempt == 0 {
                format!("ダウンロード開始: {}", spec.filename)
            } else {
                format!("リトライ {}/3 (レジューム): {}", attempt + 1, spec.filename)
            };
            on_progress(&spec.id, 10, &start_msg);

            match download_url_resumable_winhttp(url, &tmp_path, &spec.id, &on_progress) {
                Ok(()) => {
                    let file_len = fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
                    if file_len < 1024 {
                        let _ = fs::remove_file(&tmp_path);
                        last_err = "ファイルサイズが小さすぎます（破損またはHTML応答）".into();
                    } else {
                        actual_sha = compute_file_sha256(&tmp_path)?;
                        if !spec.skip_sha256 {
                            if let Some(ref expected) = spec.expected_sha256 {
                                if !actual_sha.eq_ignore_ascii_case(expected.trim()) {
                                    let _ = fs::remove_file(&tmp_path);
                                    last_err = format!(
                                        "SHA-256 不一致 ({} != {})",
                                        &actual_sha[..actual_sha.len().min(12)],
                                        &expected[..expected.len().min(12)]
                                    );
                                    continue;
                                }
                            }
                        }
                        downloaded_ok = true;
                        break;
                    }
                }
                Err(err) => {
                    last_err = err;
                    // On definitive HTTP 404/403 HTML errors, break retry loop early and try mirror URL
                    if last_err.contains("HTTP 404") || last_err.contains("HTMLエラーページ") {
                        break;
                    }
                }
            }
            if attempt < 2 {
                thread::sleep(Duration::from_secs(backoff_secs[attempt]));
            }
        }
        if downloaded_ok {
            break;
        }
    }

    if !downloaded_ok {
        return Err(last_err);
    }

    let short_sha = &actual_sha[..actual_sha.len().min(10)];
    match spec.archive_type {
        ArchiveType::TarBz2 => {
            on_progress(
                &spec.id,
                88,
                &format!("SHA-256 ({}) 確認済・tar.bz2 展開中...", short_sha),
            );
            let mut tar_cmd = Command::new("tar.exe");
            tar_cmd.creation_flags(CREATE_NO_WINDOW);
            tar_cmd.arg("-xjf").arg(&tmp_path).arg("-C").arg(target_dir);
            let tar_out = tar_cmd
                .output()
                .map_err(|e| format!("tar.exe での展開に失敗しました: {}", e))?;
            let _ = fs::remove_file(&tmp_path);
            if !tar_out.status.success() {
                let stderr = String::from_utf8_lossy(&tar_out.stderr);
                return Err(format!("モデルの展開に失敗しました: {}", stderr.trim()));
            }
            on_progress(&spec.id, 100, "モデルの展開が完了しました");
            Ok(target_dir.to_path_buf())
        }
        ArchiveType::Zip => {
            on_progress(
                &spec.id,
                88,
                &format!("SHA-256 ({}) 確認済・zip 展開中...", short_sha),
            );
            let mut tar_cmd = Command::new("tar.exe");
            tar_cmd.creation_flags(CREATE_NO_WINDOW);
            tar_cmd.arg("-xf").arg(&tmp_path).arg("-C").arg(target_dir);
            let tar_out = tar_cmd
                .output()
                .map_err(|e| format!("zip 展開に失敗しました: {}", e))?;
            let _ = fs::remove_file(&tmp_path);
            if !tar_out.status.success() {
                let stderr = String::from_utf8_lossy(&tar_out.stderr);
                return Err(format!("zip 展開に失敗しました: {}", stderr.trim()));
            }
            on_progress(&spec.id, 100, "zip 展開が完了しました");
            Ok(target_dir.to_path_buf())
        }
        ArchiveType::RawFile => {
            if dest_path.exists() {
                let _ = fs::remove_file(&dest_path);
            }
            fs::rename(&tmp_path, &dest_path)
                .map_err(|e| format!("モデルファイルの配置に失敗しました: {}", e))?;
            on_progress(
                &spec.id,
                100,
                &format!("ダウンロード完了 (SHA-256: {}): {}", short_sha, spec.filename),
            );
            Ok(dest_path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    #[test]
    fn resolves_builtin_and_custom_download_specs() {
        let qwen = resolve_download_spec("qwen3-asr-0.6b-int8", None, None).unwrap();
        assert!(qwen.is_tar_bz2);
        assert!(qwen.url.contains("qwen3-asr-0.6B-int8"));

        let large = resolve_download_spec("whisper-large-v3-q8", None, None).unwrap();
        assert!(!large.is_tar_bz2);
        assert_eq!(large.filename, "ggml-large-v3-q8_0.bin");

        let custom = resolve_download_spec(
            "custom",
            Some("https://example.com/models/my-jp-sft.bin?download=true"),
            None,
        )
        .unwrap();
        assert_eq!(custom.filename, "my-jp-sft.bin");
        assert!(resolve_download_spec("custom", Some("ftp://bad"), None).is_err());
        assert!(sanitize_filename("../evil.bin").is_ok());
        assert!(sanitize_filename("..").is_err());

        let tmp = std::env::temp_dir().join("voxdrop_sha256_test.txt");
        fs::write(&tmp, b"abc").unwrap();
        let digest = compute_file_sha256(&tmp).unwrap();
        let _ = fs::remove_file(&tmp);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn probe_and_resilient_download_handle_404_mirror_html_and_range_resume() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().unwrap();
        let payload = vec![0x5Au8; 4096];
        let payload_clone = payload.clone();

        thread::spawn(move || {
            for stream in listener.incoming().take(6).flatten() {
                let mut reader = std::io::BufReader::new(stream);
                let mut req_line = String::new();
                let _ = reader.read_line(&mut req_line);
                let mut range_start: Option<usize> = None;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                        break;
                    }
                    if let Some(rest) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        if let Some((start_s, _)) = rest.trim().split_once('-') {
                            range_start = start_s.parse::<usize>().ok();
                        }
                    }
                }
                let mut s = reader.into_inner();
                let parts: Vec<&str> = req_line.split_whitespace().collect();
                let method = *parts.first().unwrap_or(&"GET");
                let path = *parts.get(1).unwrap_or(&"/");
                if path == "/dead.bin" {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                } else if path == "/html.bin" {
                    let body = b"<html><body>Not found page</body></html>";
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(hdr.as_bytes());
                    let _ = s.write_all(body);
                } else if method == "HEAD" {
                    // Simulate storage returning 405 on HEAD so Range GET fallback triggers
                    let _ = s.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                } else if let Some(rs) = range_start {
                    let slice = if rs == 0 && req_line.contains("GET") && payload_clone.len() > 1 {
                        // Support both Range: bytes=0-0 (1 byte) and Range: bytes=2048- (tail)
                        if rs == 0 {
                            &payload_clone[..1]
                        } else {
                            &payload_clone[rs..]
                        }
                    } else {
                        &payload_clone[rs.min(payload_clone.len())..]
                    };
                    let end = rs + slice.len().saturating_sub(1);
                    let hdr = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        rs,
                        end,
                        payload_clone.len(),
                        slice.len()
                    );
                    let _ = s.write_all(hdr.as_bytes());
                    let _ = s.write_all(slice);
                } else {
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload_clone.len()
                    );
                    let _ = s.write_all(hdr.as_bytes());
                    let _ = s.write_all(&payload_clone);
                }
                let _ = s.flush();
            }
        });

        let dead = probe_url_health_winhttp(&format!("http://{}/dead.bin", addr));
        assert!(!dead.ok);
        assert_eq!(dead.status_code, 404);

        let html = probe_url_health_winhttp(&format!("http://{}/html.bin", addr));
        assert!(!html.ok);
        assert!(html.is_html);

        let mirror_url = format!("http://{}/mirror.bin", addr);
        let entry = ResolvedCatalogModel {
            entry: crate::stt::catalog::CatalogManifestEntry {
                id: "test-model".into(),
                primary_url: format!("http://{}/dead.bin", addr),
                mirror_urls: vec![mirror_url.clone()],
                benefits: vec!["test".into()],
                best_for: "test".into(),
                caution: "test".into(),
                ..Default::default()
            },
            default_url: String::new(),
            has_override: false,
            skip_sha256: false,
        };
        let health = check_model_url_health(&entry);
        assert_eq!(health.status, "mirror_reachable");
        assert_eq!(health.active_url, mirror_url);

        // Pre-populate first 2048 bytes of .part to test HTTP 206 Range Resume
        let tmp = std::env::temp_dir().join(format!("voxdrop_resume_test_{}.part", std::process::id()));
        fs::write(&tmp, &payload[..2048]).unwrap();
        download_url_resumable_winhttp(&mirror_url, &tmp, "test", &|_, _, _| {}).expect("resume ok");
        let got = fs::read(&tmp).unwrap();
        let _ = fs::remove_file(&tmp);
        assert_eq!(got.len(), payload.len());
        assert_eq!(got, payload);
    }
}
