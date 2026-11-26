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
        }),
        "parakeet-ja-int8" => Ok(ModelDownloadSpec {
            id: "parakeet-ja-int8".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8.tar.bz2".into(),
            filename: "sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8.tar.bz2".into(),
            is_tar_bz2: true,
        }),
        "whisper-large-v3-q8" => Ok(ModelDownloadSpec {
            id: "whisper-large-v3-q8".into(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q8_0.bin".into(),
            filename: "ggml-large-v3-q8_0.bin".into(),
            is_tar_bz2: false,
        }),
        "whisper-large-v3-turbo-q8" => Ok(ModelDownloadSpec {
            id: "whisper-large-v3-turbo-q8".into(),
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin".into(),
            filename: "ggml-large-v3-turbo-q8_0.bin".into(),
            is_tar_bz2: false,
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
            })
        }
        other => Err(format!("未知のモデルIDです: {}", other)),
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
    let _ = fs::remove_file(&tmp_path);

    on_progress(&spec.id, 10, &format!("ダウンロード開始: {}", spec.filename));

    let mut dl_cmd = Command::new("curl.exe");
    dl_cmd.creation_flags(CREATE_NO_WINDOW);
    dl_cmd.args(["-L", "--fail", "--retry", "2", "--retry-delay", "2", "-o"])
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

    if spec.is_tar_bz2 {
        on_progress(&spec.id, 75, "アーカイブを展開中...");
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
        on_progress(&spec.id, 100, &format!("ダウンロード完了: {}", spec.filename));
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
    }
}
