use flate2::write::GzEncoder;
use flate2::Compression;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

const BUNDLE_MAGIC: &[u8; 16] = b"VOXDROP_BNDL_V01";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        eprintln!("Usage: pack_standalone <input_exe> <output_exe> <bin_dir> <model_dir>");
        std::process::exit(1);
    }

    let input_exe = PathBuf::from(&args[1]);
    let output_exe = PathBuf::from(&args[2]);
    let bin_dir = PathBuf::from(&args[3]);
    let model_dir = PathBuf::from(&args[4]);

    let mut files: Vec<(String, PathBuf)> = Vec::new();

    // Mode A: Whisper.cpp Vulkan GPU + Kotoba-Whisper v2.0 GGML model (single .bin file!)
    if model_dir.is_file() && model_dir.extension().and_then(|s| s.to_str()) == Some("bin") {
        for dll in [
            "whisper.dll",
            "ggml-whisper.dll",
            "ggml-base-whisper.dll",
            "ggml-cpu-whisper.dll",
            "ggml-vulkan-whisper.dll",
        ] {
            let p = bin_dir.join(dll);
            if p.is_file() {
                files.push((format!("bin/{}", dll), p));
            } else {
                return Err(format!("Missing required Vulkan Whisper DLL: {}", p.display()).into());
            }
        }
        let fname = model_dir.file_name().unwrap().to_string_lossy().into_owned();
        files.push((format!("models/{}", fname), model_dir.clone()));
    } else {
        for dll in [
            "onnxruntime.dll",
            "onnxruntime_providers_shared.dll",
            "sherpa-onnx-c-api.dll",
        ] {
            let p = bin_dir.join(dll);
            if p.is_file() {
                files.push((format!("bin/{}", dll), p));
            } else {
                return Err(format!("Missing required DLL: {}", p.display()).into());
            }
        }

        // Auto-detect Whisper Large-v3-Turbo, Moonshine, or SenseVoice model files
        let find_matching = |pred: fn(&str) -> bool| -> Option<PathBuf> {
            fs::read_dir(&model_dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.is_file()
                        && p.file_name()
                            .and_then(|s| s.to_str())
                            .map(pred)
                            .unwrap_or(false)
                })
        };

        let w_enc = find_matching(|n| {
            let l = n.to_ascii_lowercase();
            l.contains("encoder") && l.ends_with(".int8.onnx")
        });
        let w_dec = find_matching(|n| {
            let l = n.to_ascii_lowercase();
            l.contains("decoder") && l.ends_with(".int8.onnx")
        });
        let w_tok = find_matching(|n| n.to_ascii_lowercase().ends_with("tokens.txt"));

        let ms_enc = model_dir.join("encoder_model.ort");
        let ms_dec = model_dir.join("decoder_model_merged.ort");
        let sv_model = model_dir.join("model.int8.onnx");
        let tokens = model_dir.join("tokens.txt");

        if let (Some(enc), Some(dec), Some(tok)) = (w_enc, w_dec, w_tok) {
            let enc_name = enc.file_name().unwrap().to_string_lossy().into_owned();
            let dec_name = dec.file_name().unwrap().to_string_lossy().into_owned();
            let tok_name = tok.file_name().unwrap().to_string_lossy().into_owned();
            files.push((format!("models/whisper-turbo/{}", enc_name), enc));
            files.push((format!("models/whisper-turbo/{}", dec_name), dec));
            files.push((format!("models/whisper-turbo/{}", tok_name), tok));
        } else if ms_enc.is_file() && ms_dec.is_file() && tokens.is_file() {
            files.push(("models/moonshine-ja/encoder_model.ort".into(), ms_enc));
            files.push(("models/moonshine-ja/decoder_model_merged.ort".into(), ms_dec));
            files.push(("models/moonshine-ja/tokens.txt".into(), tokens));
        } else if sv_model.is_file() && tokens.is_file() {
            files.push(("models/sense-voice/model.int8.onnx".into(), sv_model));
            files.push(("models/sense-voice/tokens.txt".into(), tokens));
        } else {
            return Err(
                format!("No supported ONNX/ORT model found in {}", model_dir.display()).into(),
            );
        }

        // Also bundle DPDFNet2 (high-fidelity radio/noisy speech enhancer) or GTCRN if present alongside models!
        for enhancer_name in ["dpdfnet2.onnx", "gtcrn_simple.onnx"] {
            let local = model_dir.join(enhancer_name);
            let parent = model_dir
                .parent()
                .map(|p| p.join(enhancer_name))
                .filter(|p| p.is_file());
            if local.is_file() {
                files.push((format!("models/{}", enhancer_name), local));
                break;
            } else if let Some(p) = parent {
                files.push((format!("models/{}", enhancer_name), p));
                break;
            }
        }
    }

    println!(
        "Packing {} runtime files (max compression) into standalone {} ...",
        files.len(),
        output_exe.display()
    );

    let mut raw_exe = fs::read(&input_exe)?;
    if raw_exe.len() >= 24 && &raw_exe[raw_exe.len() - 16..] == BUNDLE_MAGIC {
        let old_len = u64::from_le_bytes(
            raw_exe[raw_exe.len() - 24..raw_exe.len() - 16]
                .try_into()
                .unwrap(),
        ) as usize;
        let cut = raw_exe.len().saturating_sub(24 + old_len);
        raw_exe.truncate(cut);
    }

    let mut out_file = File::create(&output_exe)?;
    out_file.write_all(&raw_exe)?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&(files.len() as u32).to_le_bytes())?;

    for (rel, abs) in &files {
        let size = fs::metadata(abs)?.len();
        println!("  + {} ({:.2} MB)", rel, size as f64 / (1024.0 * 1024.0));
        encoder.write_all(&(rel.len() as u32).to_le_bytes())?;
        encoder.write_all(rel.as_bytes())?;
        encoder.write_all(&size.to_le_bytes())?;
        let mut src = File::open(abs)?;
        io::copy(&mut src, &mut encoder)?;
    }

    let payload = encoder.finish()?;
    let payload_len = payload.len() as u64;
    out_file.write_all(&payload)?;
    out_file.write_all(&payload_len.to_le_bytes())?;
    out_file.write_all(BUNDLE_MAGIC)?;
    out_file.flush()?;

    let final_size = fs::metadata(&output_exe)?.len();
    println!(
        "Done! Standalone {} = {:.2} MB (base exe {:.2} MB + compressed payload {:.2} MB)",
        output_exe.display(),
        final_size as f64 / (1024.0 * 1024.0),
        raw_exe.len() as f64 / (1024.0 * 1024.0),
        payload_len as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}
