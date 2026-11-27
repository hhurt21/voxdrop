use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const BUNDLE_MAGIC: &[u8; 16] = b"VOXDROP_BNDL_V01";
const FOOTER_SIZE: u64 = 24; // [payload_len: u64 LE][magic: 16 bytes]

/// Returns the local runtime cache directory (`%LOCALAPPDATA%\VoxDrop\runtime\v7`).
pub fn runtime_cache_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let trimmed = local.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed)
                .join("VoxDrop")
                .join("runtime")
                .join("v7");
        }
    }
    std::env::temp_dir().join("VoxDrop").join("runtime").join("v7")
}

/// Checks if the running `voxdrop.exe` carries an appended self-contained runtime bundle
/// (sherpa-onnx DLLs + SenseVoice model). If so, extracts it once into `%LOCALAPPDATA%\VoxDrop\runtime\v1`
/// and returns that directory so VoxDrop runs cleanly as a single standalone `.exe`.
pub fn ensure_embedded_runtime_extracted() -> Result<Option<PathBuf>, String> {
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("実行ファイルパスの取得に失敗しました: {}", e))?;
    extract_bundle_from_file(&exe_path, &runtime_cache_dir())
}

pub fn extract_bundle_from_file(exe_path: &Path, target_dir: &Path) -> Result<Option<PathBuf>, String> {
    let mut file = File::open(exe_path)
        .map_err(|e| format!("実行ファイルのオープンに失敗しました: {}", e))?;
    let file_len = file
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    if file_len < FOOTER_SIZE {
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))
        .map_err(|e| e.to_string())?;
    let mut footer = [0u8; 24];
    file.read_exact(&mut footer).map_err(|e| e.to_string())?;

    if &footer[8..24] != BUNDLE_MAGIC {
        return Ok(None);
    }

    let payload_len = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    if payload_len == 0 || payload_len + FOOTER_SIZE > file_len {
        return Err("内蔵ランタイムバンドルのサイズが不正です".into());
    }

    let marker_path = target_dir.join(".bundle_ready");
    let expected_header = format!("v2:{}", payload_len);
    if let Ok(existing) = fs::read_to_string(&marker_path) {
        let mut lines = existing.lines();
        if lines.next().map(str::trim) == Some(expected_header.as_str()) {
            let listed: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
            if !listed.is_empty() && listed.iter().all(|rel| target_dir.join(rel).is_file()) {
                return Ok(Some(target_dir.to_path_buf()));
            }
        }
    }

    fs::create_dir_all(target_dir)
        .map_err(|e| format!("ランタイム展開先フォルダの作成に失敗しました: {}", e))?;

    let payload_start = file_len - FOOTER_SIZE - payload_len;
    file.seek(SeekFrom::Start(payload_start))
        .map_err(|e| e.to_string())?;

    let limited = file.take(payload_len);
    let mut decoder = GzDecoder::new(limited);

    let mut count_buf = [0u8; 4];
    decoder
        .read_exact(&mut count_buf)
        .map_err(|e| format!("内蔵ランタイムのヘッダ展開に失敗しました: {}", e))?;
    let file_count = u32::from_le_bytes(count_buf);
    let mut extracted_files: Vec<String> = Vec::with_capacity(file_count as usize);

    for _ in 0..file_count {
        let mut plen_buf = [0u8; 4];
        decoder
            .read_exact(&mut plen_buf)
            .map_err(|e| format!("バンドル内パスの読み込みに失敗: {}", e))?;
        let plen = u32::from_le_bytes(plen_buf) as usize;
        let mut path_bytes = vec![0u8; plen];
        decoder
            .read_exact(&mut path_bytes)
            .map_err(|e| format!("バンドル内パス文字列の読み込みに失敗: {}", e))?;
        let rel_path = String::from_utf8(path_bytes)
            .map_err(|_| "バンドル内のファイルパスがUTF-8ではありません".to_string())?;

        // Prevent path traversal
        if rel_path.contains("..") || Path::new(&rel_path).is_absolute() {
            return Err(format!("不正なバンドル内パス: {}", rel_path));
        }

        let mut dlen_buf = [0u8; 8];
        decoder
            .read_exact(&mut dlen_buf)
            .map_err(|e| format!("バンドル内サイズ読み込みに失敗: {}", e))?;
        let data_len = u64::from_le_bytes(dlen_buf);

        let dest = target_dir.join(&rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out_file = File::create(&dest)
            .map_err(|e| format!("展開ファイルの作成に失敗 ({}): {}", dest.display(), e))?;

        let mut remaining = data_len;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let chunk = remaining.min(buf.len() as u64) as usize;
            decoder
                .read_exact(&mut buf[..chunk])
                .map_err(|e| format!("展開データの読み込みに失敗 ({}): {}", rel_path, e))?;
            std::io::Write::write_all(&mut out_file, &buf[..chunk])
                .map_err(|e| format!("展開データの書き込みに失敗 ({}): {}", rel_path, e))?;
            remaining -= chunk as u64;
        }
        extracted_files.push(rel_path);
    }

    let mut manifest = expected_header;
    for rel in extracted_files {
        manifest.push('\n');
        manifest.push_str(&rel);
    }
    fs::write(&marker_path, manifest)
        .map_err(|e| format!("完了マーカーの書き込みに失敗: {}", e))?;

    Ok(Some(target_dir.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn roundtrip_bundle_append_and_extract() {
        let dir = std::env::temp_dir().join(format!("voxdrop_bundle_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fake_exe = dir.join("fake.exe");
        let extract_dir = dir.join("extracted");

        // Build compressed payload containing 2 test files
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all(&2u32.to_le_bytes()).unwrap();
        for (rel, data) in [
            ("bin/sherpa-onnx-c-api.dll", b"FAKE_DLL_BYTES".as_slice()),
            ("models/sense-voice/tokens.txt", b"0 <blk>\n".as_slice()),
        ] {
            gz.write_all(&(rel.len() as u32).to_le_bytes()).unwrap();
            gz.write_all(rel.as_bytes()).unwrap();
            gz.write_all(&(data.len() as u64).to_le_bytes()).unwrap();
            gz.write_all(data).unwrap();
        }
        let payload = gz.finish().unwrap();

        let mut exe_bytes = b"MZ_FAKE_PE_HEADER".to_vec();
        exe_bytes.extend_from_slice(&payload);
        exe_bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        exe_bytes.extend_from_slice(BUNDLE_MAGIC);
        fs::write(&fake_exe, exe_bytes).unwrap();

        let res = extract_bundle_from_file(&fake_exe, &extract_dir).unwrap();
        assert_eq!(res, Some(extract_dir.clone()));
        assert_eq!(
            fs::read(extract_dir.join("bin/sherpa-onnx-c-api.dll")).unwrap(),
            b"FAKE_DLL_BYTES"
        );
        assert_eq!(
            fs::read(extract_dir.join("models/sense-voice/tokens.txt")).unwrap(),
            b"0 <blk>\n"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
