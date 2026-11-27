//! On-demand external model downloader into the local `models/` directory.
//! Keeps `voxdrop.exe` compact while allowing one-click installation of Qwen3-ASR-0.6B,
//! full Whisper Large-v3, Parakeet-Ja, or custom SFT GGML `.bin` models.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDownloadSpec {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub is_tar_bz2: bool,
    pub expected_sha256: Option<String>,
}

pub fn resolve_download_spec(
    model_id: &str,
    custom_url: Option<&str>,
    custom_filename: Option<&str>,
) -> Result<ModelDownloadSpec, String> {
    match model_id.trim() {
        "qwen3-asr-0.6b-int8" => Ok(ModelDownloadSpec {
            id: "qwen3-asr-0.6b-int8".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2".into(),
            filename: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2".into(),
            is_tar_bz2: true,
            expected_sha256: None,
        }),
        "qwen3-asr-1.7b-gguf" => Ok(ModelDownloadSpec {
            id: "qwen3-asr-1.7b-gguf".into(),
            url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main/qwen3-asr-1.7b-q8_0.gguf".into(),
            filename: "qwen3-asr-1.7b-q8_0.gguf".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "parakeet-ja-int8" => Ok(ModelDownloadSpec {
            id: "parakeet-ja-int8".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8.tar.bz2".into(),
            filename: "sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8.tar.bz2".into(),
            is_tar_bz2: true,
            expected_sha256: None,
        }),
        "whisper-large-v3-q8" => Ok(ModelDownloadSpec {
            id: "whisper-large-v3-q8".into(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q8_0.bin".into(),
            filename: "ggml-large-v3-q8_0.bin".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "whisper-large-v3-turbo-q8" => Ok(ModelDownloadSpec {
            id: "whisper-large-v3-turbo-q8".into(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin".into(),
            filename: "ggml-large-v3-turbo-q8_0.bin".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "whisper-large-v3-turbo-ja-sft" => Ok(ModelDownloadSpec {
            id: "whisper-large-v3-turbo-ja-sft".into(),
            url: "https://huggingface.co/Holy-fox/whisper-large-v3-turbo-ja-personal-sft-v2-ggml/resolve/main/ggml-model-q8_0.bin".into(),
            filename: "ggml-whisper-turbo-ja-sft-q8_0.bin".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "dpdfnet-16k" => Ok(ModelDownloadSpec {
            id: "dpdfnet-16k".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/dpdfnet_baseline_16k.onnx".into(),
            filename: "dpdfnet_baseline_16k.onnx".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "dpdfnet-48k" => Ok(ModelDownloadSpec {
            id: "dpdfnet-48k".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/dpdfnet_48k.onnx".into(),
            filename: "dpdfnet_48k.onnx".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "gtcrn-16k" => Ok(ModelDownloadSpec {
            id: "gtcrn-16k".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/gtcrn_simple.onnx".into(),
            filename: "gtcrn_simple.onnx".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "qwen3-1.7b-llm" => Ok(ModelDownloadSpec {
            id: "qwen3-1.7b-llm".into(),
            url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf".into(),
            filename: "Qwen3-1.7B-Q4_K_M.gguf".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "qwen3-4b-llm" => Ok(ModelDownloadSpec {
            id: "qwen3-4b-llm".into(),
            url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf".into(),
            filename: "Qwen3-4B-Q4_K_M.gguf".into(),
            is_tar_bz2: false,
            expected_sha256: None,
        }),
        "custom" => {
            let url = custom_url.unwrap_or("").trim();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err("ダウンロードURLは http:// または https:// で指定してください".into());
            }
            let raw_name = custom_filename
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    url.split('?').next().unwrap_or(url).rsplit('/').next().unwrap_or("custom-model.bin")
                });
            let clean_name = sanitize_filename(raw_name)?;
            let is_tar_bz2 = clean_name.to_ascii_lowercase().ends_with(".tar.bz2");
            Ok(ModelDownloadSpec {
                id: "custom".into(),
                url: url.to_string(),
                filename: clean_name,
                is_tar_bz2,
                expected_sha256: None,
            })
        }
        other => Err(format!("未知のモデルIDです: {}", other)),
    }
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
    let spec = resolve_download_spec(model_id, custom_url, custom_filename)?;
    fs::create_dir_all(target_dir)
        .map_err(|e| format!("モデル保存先フォルダの作成に失敗しました: {}", e))?;

    let dest_path = target_dir.join(&spec.filename);
    let tmp_path = target_dir.join(format!("{}.part", spec.filename));

    let mut actual_sha = String::new();
    for attempt in 0..2 {
        let _ = fs::remove_file(&tmp_path);
        let start_msg = if attempt == 0 {
            format!("ダウンロード開始: {}", spec.filename)
        } else {
            format!("整合性再検証のため再ダウンロード中: {}", spec.filename)
        };
        on_progress(&spec.id, 10, &start_msg);

        let mut dl_cmd = Command::new("curl.exe");
        dl_cmd.creation_flags(CREATE_NO_WINDOW);
        dl_cmd
            .args(["-L", "--fail", "--retry", "2", "--retry-delay", "2", "-o"])
            .arg(&tmp_path)
            .arg(&spec.url);

        let out = dl_cmd
            .output()
            .map_err(|e| format!("curl.exe の起動に失敗しました: {}", e))?;
        if !out.status.success() {
            let _ = fs::remove_file(&tmp_path);
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "モデルのダウンロードに失敗しました ({}): {}",
                out.status,
                stderr.trim()
            ));
        }

        let file_len = fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
        if file_len < 1024 {
            let _ = fs::remove_file(&tmp_path);
            if attempt == 0 {
                continue;
            }
            return Err("ダウンロードファイルが空または破損しています（HTMLエラー等）".into());
        }

        actual_sha = compute_file_sha256(&tmp_path)?;
        if let Some(ref expected) = spec.expected_sha256 {
            if !actual_sha.eq_ignore_ascii_case(expected.trim()) {
                let _ = fs::remove_file(&tmp_path);
                if attempt == 0 {
                    continue;
                }
                return Err(format!(
                    "SHA-256 整合性検証に失敗しました（破損データのため削除: {} != {}）",
                    &actual_sha[..actual_sha.len().min(12)],
                    &expected[..expected.len().min(12)]
                ));
            }
        }
        break;
    }

    let short_sha = &actual_sha[..actual_sha.len().min(10)];
    if spec.is_tar_bz2 {
        on_progress(&spec.id, 75, &format!("SHA-256 ({}) 確認済・展開中...", short_sha));
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
    } else {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(sanitize_filename("../evil.bin").is_ok()); // file_name strips traversal -> evil.bin
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
}
