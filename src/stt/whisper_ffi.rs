//! Direct resident FFI wrapper for `whisper.cpp` (`whisper.dll` v1.8.5 with Vulkan GPU + AVX2 CPU fallback).
//! Runs `ggml-kotoba-whisper-v2.0-q5_0.bin` (100% Japanese-only, 7.5M Japanese clips, zero English hallucination)
//! on AMD/NVIDIA/Intel Vulkan GPUs in ~0.15–0.30s with full beam search (beam_size=5)!

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::{Mutex, Once};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, SetDllDirectoryW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

static GGML_BACKEND_INIT: Once = Once::new();

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperAhead {
    n_text_layer: c_int,
    n_head: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperAheads {
    n_heads: usize,
    heads: *const WhisperAhead,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperContextParams {
    use_gpu: bool,
    flash_attn: bool,
    gpu_device: c_int,
    dtw_token_timestamps: bool,
    dtw_aheads_preset: c_int,
    dtw_n_top: c_int,
    dtw_aheads: WhisperAheads,
    dtw_mem_size: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperGreedyParams {
    best_of: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperBeamSearchParams {
    beam_size: c_int,
    patience: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperVadParams {
    threshold: f32,
    min_speech_duration_ms: c_int,
    min_silence_duration_ms: c_int,
    max_speech_duration_s: f32,
    speech_pad_ms: c_int,
    samples_overlap: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WhisperFullParams {
    strategy: c_int, // 0 = GREEDY, 1 = BEAM_SEARCH
    n_threads: c_int,
    n_max_text_ctx: c_int,
    offset_ms: c_int,
    duration_ms: c_int,

    translate: bool,
    no_context: bool,
    no_timestamps: bool,
    single_segment: bool,
    print_special: bool,
    print_progress: bool,
    print_realtime: bool,
    print_timestamps: bool,

    token_timestamps: bool,
    thold_pt: f32,
    thold_ptsum: f32,
    max_len: c_int,
    split_on_word: bool,
    max_tokens: c_int,

    debug_mode: bool,
    audio_ctx: c_int,

    tdrz_enable: bool,
    suppress_regex: *const c_char,

    initial_prompt: *const c_char,
    carry_initial_prompt: bool,
    prompt_tokens: *const i32,
    prompt_n_tokens: c_int,

    language: *const c_char,
    detect_language: bool,

    suppress_blank: bool,
    suppress_nst: bool,

    temperature: f32,
    max_initial_ts: f32,
    length_penalty: f32,

    temperature_inc: f32,
    entropy_thold: f32,
    logprob_thold: f32,
    no_speech_thold: f32,

    greedy: WhisperGreedyParams,
    beam_search: WhisperBeamSearchParams,

    new_segment_callback: *const c_void,
    new_segment_callback_user_data: *mut c_void,
    progress_callback: *const c_void,
    progress_callback_user_data: *mut c_void,
    encoder_begin_callback: *const c_void,
    encoder_begin_callback_user_data: *mut c_void,
    abort_callback: *const c_void,
    abort_callback_user_data: *mut c_void,
    logits_filter_callback: *const c_void,
    logits_filter_callback_user_data: *mut c_void,

    grammar_rules: *const *const c_void,
    n_grammar_rules: usize,
    i_start_rule: usize,
    grammar_penalty: f32,

    vad: bool,
    vad_model_path: *const c_char,
    vad_params: WhisperVadParams,
}

type FnGgmlBackendLoadAll = unsafe extern "C" fn();
type FnWhisperContextDefaultParamsByRef = unsafe extern "C" fn() -> *mut WhisperContextParams;
type FnWhisperFreeContextParams = unsafe extern "C" fn(*mut WhisperContextParams);
type FnWhisperInitFromFileWithParams =
    unsafe extern "C" fn(*const c_char, WhisperContextParams) -> *mut c_void;
type FnWhisperFree = unsafe extern "C" fn(*mut c_void);
type FnWhisperFullDefaultParamsByRef = unsafe extern "C" fn(c_int) -> *mut WhisperFullParams;
type FnWhisperFreeParams = unsafe extern "C" fn(*mut WhisperFullParams);
type FnWhisperFull =
    unsafe extern "C" fn(*mut c_void, WhisperFullParams, *const f32, c_int) -> c_int;
type FnWhisperFullNSegments = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnWhisperFullGetSegmentText = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
type FnWhisperFullGetSegmentNoSpeechProb = unsafe extern "C" fn(*mut c_void, c_int) -> f32;

struct WhisperDllFns {
    hmodule: HMODULE,
    ctx_default_params_by_ref: FnWhisperContextDefaultParamsByRef,
    free_ctx_params: FnWhisperFreeContextParams,
    init_from_file_with_params: FnWhisperInitFromFileWithParams,
    free_ctx: FnWhisperFree,
    full_default_params_by_ref: FnWhisperFullDefaultParamsByRef,
    free_params: FnWhisperFreeParams,
    full: FnWhisperFull,
    full_n_segments: FnWhisperFullNSegments,
    full_get_segment_text: FnWhisperFullGetSegmentText,
    full_get_segment_no_speech_prob: Option<FnWhisperFullGetSegmentNoSpeechProb>,
}

struct ResidentWhisperInner {
    fns: WhisperDllFns,
    ctx: *mut c_void,
    language: CString,
    prompt: Option<CString>,
    is_kotoba: bool,
    threads: u32,
}

unsafe impl Send for ResidentWhisperInner {}

impl Drop for ResidentWhisperInner {
    fn drop(&mut self) {
        unsafe {
            if !self.ctx.is_null() {
                (self.fns.free_ctx)(self.ctx);
                self.ctx = ptr::null_mut();
            }
            // Keep whisper.dll / ggml-whisper.dll pinned in process memory so reloading a model
            // never re-runs ggml's global terminate handler registration!
            let _ = self.fns.hmodule;
        }
    }
}

/// Resident GPU/CPU `whisper.dll` recognizer (Vulkan GPU accelerated when available!).
pub struct ResidentWhisperRecognizer {
    inner: Mutex<ResidentWhisperInner>,
    using_gpu: bool,
}

impl ResidentWhisperRecognizer {
    pub fn load(
        dll_path: &Path,
        model_path: &Path,
        language: &str,
        threads: u32,
    ) -> Result<Self, String> {
        let (fns, has_vulkan) = load_whisper_dll(dll_path)?;
        let model_c = CString::new(model_path.to_string_lossy().as_ref())
            .map_err(|_| "モデルパスに不正な文字が含まれています".to_string())?;
        let lang_str = if language.trim().is_empty() {
            "ja"
        } else {
            language.trim()
        };
        let lang_c = CString::new(lang_str)
            .map_err(|_| "言語コードが不正です".to_string())?;

        let ctx = unsafe {
            let cp_ptr = (fns.ctx_default_params_by_ref)();
            if cp_ptr.is_null() {
                return Err("whisper_context_default_params_by_ref に失敗しました".into());
            }
            let mut cparams = *cp_ptr;
            (fns.free_ctx_params)(cp_ptr);
            cparams.use_gpu = true;
            cparams.flash_attn = true;

            let mut ptr_ctx = (fns.init_from_file_with_params)(model_c.as_ptr(), cparams);
            if ptr_ctx.is_null() {
                // Retry cleanly on CPU if GPU init fails on an unsupported driver
                cparams.use_gpu = false;
                cparams.flash_attn = false;
                ptr_ctx = (fns.init_from_file_with_params)(model_c.as_ptr(), cparams);
            }
            ptr_ctx
        };

        if ctx.is_null() {
            return Err(format!(
                "whisper.dll でモデルを読み込めませんでした: {}",
                model_path.display()
            ));
        }

        let is_kotoba = model_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.to_ascii_lowercase().contains("kotoba"))
            .unwrap_or(false);
        // Whisper Large-v3-Turbo benefits hugely from a short natural Japanese initial_prompt
        // (locks language to natural Japanese chat & boosts voice commands);
        // Distil-Whisper (Kotoba) is trained without prompts so keep prompt None for Kotoba.
        let prompt = if !is_kotoba && lang_str.eq_ignore_ascii_case("ja") {
            CString::new("日本語のチャット音声入力。テスト、聞こえますか、聞こえたら、送信して、送って、エンター、キャンセル。").ok()
        } else {
            None
        };

        Ok(Self {
            inner: Mutex::new(ResidentWhisperInner {
                fns,
                ctx,
                language: lang_c,
                prompt,
                is_kotoba,
                threads: threads.clamp(1, 16),
            }),
            using_gpu: has_vulkan,
        })
    }

    pub fn is_gpu_accelerated(&self) -> bool {
        self.using_gpu
    }

    pub fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        if samples_16k.len() < 1600 {
            return Ok(String::new());
        }

        // Apply Smart Quiet-Mic AGC + 80Hz High-Pass (NO spectral tilt EQ so Whisper's Mel spectrum stays exact!)
        let conditioned = crate::audio::enhance::enhance_utterance_16k(samples_16k, false, true);

        let guard = self
            .inner
            .lock()
            .map_err(|_| "Whisper 認識ロックの取得に失敗しました".to_string())?;

        unsafe {
            // Use beam_size=3 on Vulkan GPU for stronger Japanese phrasing & kanji accuracy
            // while staying ~0.55s fast via timestamp-anchored audio_ctx=512!
            let strategy = if self.using_gpu { 1 } else { 0 };
            let wp_ptr = (guard.fns.full_default_params_by_ref)(strategy);
            if wp_ptr.is_null() {
                return Err("whisper_full_default_params_by_ref に失敗しました".into());
            }
            let mut wparams = *wp_ptr;
            (guard.fns.free_params)(wp_ptr);

            let dur_sec = conditioned.len() as f32 / 16_000.0;
            // Round dynamic audio_ctx up to multiple of 64 (min 256 = 5.12s, full 1500 if > 14s)
            let dyn_ctx = if dur_sec > 14.0 {
                0
            } else {
                let raw = ((dur_sec + 1.8) * 50.0).ceil() as i32;
                let rounded = ((raw + 63) / 64) * 64;
                rounded.clamp(256, 1500)
            };
            wparams.audio_ctx = if guard.is_kotoba {
                if dur_sec <= 8.0 { 512 } else { 0 }
            } else {
                dyn_ctx
            };

            wparams.n_threads = guard.threads as c_int;
            wparams.translate = false;
            wparams.no_context = true;
            wparams.no_timestamps = false;
            wparams.single_segment = false;
            wparams.print_special = false;
            wparams.print_progress = false;
            wparams.print_realtime = false;
            wparams.print_timestamps = false;
            wparams.language = guard.language.as_ptr();
            wparams.detect_language = false;
            if let Some(ref p) = guard.prompt {
                wparams.initial_prompt = p.as_ptr();
                wparams.carry_initial_prompt = true;
            }
            wparams.suppress_blank = true;
            wparams.suppress_nst = true;
            wparams.temperature = 0.0;
            wparams.temperature_inc = 0.2;
            wparams.entropy_thold = 2.2;
            wparams.no_speech_thold = 0.60;
            wparams.max_tokens = 96;
            if self.using_gpu {
                wparams.beam_search.beam_size = 3;
            } else {
                wparams.greedy.best_of = 1;
            }

            let rc = (guard.fns.full)(
                guard.ctx,
                wparams,
                conditioned.as_ptr(),
                conditioned.len() as c_int,
            );
            if rc != 0 {
                return Err(format!("whisper_full がエラーを返しました (code={})", rc));
            }

            let n_seg = (guard.fns.full_n_segments)(guard.ctx);
            let mut out = String::new();
            for i in 0..n_seg {
                // Skip segments Whisper itself believes are non-speech / background noise
                if let Some(nsp_fn) = guard.fns.full_get_segment_no_speech_prob {
                    if nsp_fn(guard.ctx, i) >= 0.62 {
                        continue;
                    }
                }
                let ptr_txt = (guard.fns.full_get_segment_text)(guard.ctx, i);
                if !ptr_txt.is_null() {
                    let seg = CStr::from_ptr(ptr_txt).to_string_lossy();
                    out.push_str(seg.trim());
                }
            }

            Ok(filter_whisper_hallucinations(out.trim()))
        }
    }
}

/// Filters common Whisper silence/noise hallucinations (e.g. YouTube outro phrases on empty clicks).
pub fn filter_whisper_hallucinations(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let clean = trimmed
        .trim_matches(|c: char| matches!(c, '。' | '、' | '！' | '？' | '!' | '?' | '.' | ' '))
        .to_ascii_lowercase();
    const HALLUCINATIONS: &[&str] = &[
        "ご視聴ありがとうございました",
        "ご視聴ありがとうございます",
        "チャンネル登録お願いします",
        "チャンネル登録よろしくお願いします",
        "お疲れ様でした",
        "字幕",
        "thank you",
        "thank you for watching",
        "thanks for watching",
        "subscribe",
        "you",
        "bye",
        "mbc",
    ];
    if HALLUCINATIONS.iter().any(|&h| clean == h) {
        return String::new();
    }
    dedup_trailing_repetition(trimmed)
}

/// Removes trailing exact-phrase echo occasionally produced by Whisper on ultra-fast speech
/// (e.g. "了解しましたあと5分で合流しますエンターあと5分で合流します" -> "了解しましたあと5分で合流しますエンター").
fn dedup_trailing_repetition(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 8 {
        return text.to_string();
    }
    // Check if the trailing slice of length L (>=4 chars) already appeared earlier in the utterance
    for len in (4..=(n / 2)).rev() {
        let tail = &chars[n - len..];
        let head = &chars[..n - len];
        if head.windows(len).any(|w| w == tail) {
            return head.iter().collect::<String>().trim().to_string();
        }
    }
    text.to_string()
}

unsafe fn load_sym<T>(hmodule: HMODULE, name: &'static [u8]) -> Result<T, String> {
    let proc = GetProcAddress(hmodule, PCSTR(name.as_ptr())).ok_or_else(|| {
        format!(
            "whisper.dll シンボル '{}' が見つかりません",
            String::from_utf8_lossy(name).trim_end_matches('\0')
        )
    })?;
    Ok(std::mem::transmute_copy(&proc))
}

fn load_whisper_dll(dll_path: &Path) -> Result<(WhisperDllFns, bool), String> {
    let abs_path = std::fs::canonicalize(dll_path).unwrap_or_else(|_| dll_path.to_path_buf());
    let path_str = abs_path.to_string_lossy();
    let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);

    let bin_dir = Path::new(clean_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let has_vulkan = bin_dir.join("ggml-vulkan-whisper.dll").is_file()
        || bin_dir.join("ggml-vulkan.dll").is_file();

    // Ensure sibling ggml-vulkan-whisper.dll / ggml-cpu-whisper.dll are discoverable by ggml_backend_load_all()
    let dir_wide: Vec<u16> = bin_dir
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetDllDirectoryW(PCWSTR(dir_wide.as_ptr()));
    }

    // Load ggml backend loader ONCE per process so Vulkan GPU backend registers safely without re-init crash!
    GGML_BACKEND_INIT.call_once(|| {
        for ggml_name in ["ggml-whisper.dll", "ggml.dll"] {
            let ggml_path = bin_dir.join(ggml_name);
            if ggml_path.is_file() {
                let gw: Vec<u16> = ggml_path
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                if let Ok(gh) = unsafe {
                    LoadLibraryExW(PCWSTR(gw.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
                } {
                    if let Ok(load_all) =
                        unsafe { load_sym::<FnGgmlBackendLoadAll>(gh, b"ggml_backend_load_all\0") }
                    {
                        unsafe { load_all() };
                    }
                    break;
                }
            }
        }
    });

    let wide: Vec<u16> = clean_path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let hmodule = LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
        .map_err(|e| format!("whisper.dll の読み込みに失敗しました ({}): {}", clean_path, e))?;

        let res = (|| -> Result<WhisperDllFns, String> {
            Ok(WhisperDllFns {
                hmodule,
                ctx_default_params_by_ref: load_sym(
                    hmodule,
                    b"whisper_context_default_params_by_ref\0",
                )?,
                free_ctx_params: load_sym(hmodule, b"whisper_free_context_params\0")?,
                init_from_file_with_params: load_sym(
                    hmodule,
                    b"whisper_init_from_file_with_params\0",
                )?,
                free_ctx: load_sym(hmodule, b"whisper_free\0")?,
                full_default_params_by_ref: load_sym(
                    hmodule,
                    b"whisper_full_default_params_by_ref\0",
                )?,
                free_params: load_sym(hmodule, b"whisper_free_params\0")?,
                full: load_sym(hmodule, b"whisper_full\0")?,
                full_n_segments: load_sym(hmodule, b"whisper_full_n_segments\0")?,
                full_get_segment_text: load_sym(hmodule, b"whisper_full_get_segment_text\0")?,
                full_get_segment_no_speech_prob: load_sym(
                    hmodule,
                    b"whisper_full_get_segment_no_speech_prob\0",
                )
                .ok(),
            })
        })();

        if res.is_err() {
            let _ = FreeLibrary(hmodule);
        }
        Ok((res?, has_vulkan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn filters_known_silence_hallucinations() {
        assert_eq!(filter_whisper_hallucinations("Thank you."), "");
        assert_eq!(filter_whisper_hallucinations("ご視聴ありがとうございました。"), "");
        assert_eq!(filter_whisper_hallucinations("聞こえますか、送信して。"), "聞こえますか、送信して。");
        assert_eq!(
            filter_whisper_hallucinations("了解しましたあと5分で合流しますエンターあと5分で合流します"),
            "了解しましたあと5分で合流しますエンター"
        );
    }

    #[test]
    fn vulkan_kotoba_whisper_transcribes_japanese_fast_and_accurately() {
        let dll = Path::new("bin/whisper.dll");
        let model = Path::new("models/ggml-kotoba-whisper-v2.0-q5_0.bin");
        let wav = Path::new(
            "models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/test_wavs/ja.wav",
        );
        if !dll.is_file() || !model.is_file() || !wav.is_file() {
            return;
        }

        let read_wav_f32 = |p: &Path| -> Vec<f32> {
            let bytes = std::fs::read(p).unwrap();
            let mut pos = 12usize;
            while pos + 8 < bytes.len() {
                let id = &bytes[pos..pos + 4];
                let sz = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
                if id == b"data" {
                    pos += 8;
                    break;
                }
                pos += 8 + sz;
            }
            bytes[pos..]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect()
        };

        let t_load = Instant::now();
        let rec = ResidentWhisperRecognizer::load(dll, model, "ja", 8)
            .expect("load Vulkan Kotoba-Whisper v2.0");
        let load_ms = t_load.elapsed().as_millis();

        let samples = read_wav_f32(wav);
        let t1 = Instant::now();
        let txt1 = rec.transcribe(&samples).expect("transcribe ja.wav");
        let ms1 = t1.elapsed().as_millis();
        eprintln!(
            "[Vulkan={} Kotoba-Whisper-v2.0] load={}ms, ja.wav({}ms) => '{}'",
            rec.is_gpu_accelerated(),
            load_ms,
            ms1,
            txt1
        );
        assert!(txt1.contains("弁当") && txt1.contains("パン"));
        assert!(txt1.contains("持っていけない"));
    }
}
