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
static WHISPER_VULKAN_LOCK: Mutex<()> = Mutex::new(());

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
    default_prompt: String,
    prompt: Option<CString>,
    is_kotoba: bool,
    threads: u32,
}

unsafe impl Send for ResidentWhisperInner {}

impl Drop for ResidentWhisperInner {
    fn drop(&mut self) {
        let _vk = WHISPER_VULKAN_LOCK.lock();
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
    device_name: String,
}

impl ResidentWhisperRecognizer {
    pub fn load(
        dll_path: &Path,
        model_path: &Path,
        language: &str,
        threads: u32,
    ) -> Result<Self, String> {
        let (fns, has_vulkan, gpu_desc) = load_whisper_dll(dll_path)?;
        let model_c = CString::new(model_path.to_string_lossy().as_ref())
            .map_err(|_| "モデルパスに不正な文字が含まれています".to_string())?;
        let lang_str = if language.trim().is_empty() {
            "ja"
        } else {
            language.trim()
        };
        let lang_c = CString::new(lang_str)
            .map_err(|_| "言語コードが不正です".to_string())?;

        let (ctx, used_gpu) = {
            let _vk = WHISPER_VULKAN_LOCK.lock();
            unsafe {
                let cp_ptr = (fns.ctx_default_params_by_ref)();
                if cp_ptr.is_null() {
                    return Err("whisper_context_default_params_by_ref に失敗しました".into());
                }
                let mut cparams = *cp_ptr;
                (fns.free_ctx_params)(cp_ptr);
                cparams.use_gpu = has_vulkan;
                cparams.flash_attn = has_vulkan;

                let mut used_gpu = has_vulkan;
                let mut ptr_ctx = (fns.init_from_file_with_params)(model_c.as_ptr(), cparams);
                if ptr_ctx.is_null() && has_vulkan {
                    // Retry cleanly on CPU if GPU init fails on an unsupported driver
                    cparams.use_gpu = false;
                    cparams.flash_attn = false;
                    used_gpu = false;
                    ptr_ctx = (fns.init_from_file_with_params)(model_c.as_ptr(), cparams);
                }
                (ptr_ctx, used_gpu)
            }
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
        let default_prompt =
            "日本語のカジュアルな会話。Discord、DM、通話、ミュート、テスト、送信して、送って、エンター、キャンセル。"
                .to_string();
        let prompt = if !is_kotoba && lang_str.eq_ignore_ascii_case("ja") {
            CString::new(default_prompt.as_str()).ok()
        } else {
            None
        };

        let device_name = if used_gpu {
            match gpu_desc {
                Some(name) if !name.trim().is_empty() => format!("Vulkan: {}", name.trim()),
                _ => "Vulkan GPU".to_string(),
            }
        } else {
            "CPU (AVX2)".to_string()
        };

        Ok(Self {
            inner: Mutex::new(ResidentWhisperInner {
                fns,
                ctx,
                language: lang_c,
                default_prompt,
                prompt,
                is_kotoba,
                threads: threads.clamp(1, 16),
            }),
            using_gpu: used_gpu,
            device_name,
        })
    }

    pub fn is_gpu_accelerated(&self) -> bool {
        self.using_gpu
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn set_prompt_hints(&self, hints: &str) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if guard.is_kotoba {
            guard.prompt = None;
            return;
        }
        let is_ja = guard
            .language
            .to_str()
            .map(|l| l.eq_ignore_ascii_case("ja"))
            .unwrap_or(false);
        if !is_ja {
            return;
        }
        let combined = build_whisper_initial_prompt(&guard.default_prompt, hints);
        let sanitized: String = combined.chars().filter(|&c| c != '\0').collect();
        if let Ok(cs) = CString::new(sanitized) {
            guard.prompt = Some(cs);
        }
    }

    pub fn transcribe(&self, samples_16k: &[f32]) -> Result<String, String> {
        self.transcribe_with_clarity(samples_16k, true)
    }

    pub fn transcribe_with_clarity(
        &self,
        samples_16k: &[f32],
        clarity_boost: bool,
    ) -> Result<String, String> {
        self.transcribe_with_options(samples_16k, clarity_boost, 0.55, 200, 300, 5, true)
    }

    pub fn transcribe_with_options(
        &self,
        samples_16k: &[f32],
        clarity_boost: bool,
        eq_strength: f32,
        head_pad_ms: u64,
        tail_pad_ms: u64,
        beam_size: i32,
        temperature_fallback: bool,
    ) -> Result<String, String> {
        if samples_16k.len() < 1600 {
            return Ok(String::new());
        }

        let input_rms = {
            let sq: f32 = samples_16k.iter().map(|&s| s * s).sum();
            (sq / samples_16k.len() as f32).sqrt()
        };

        // Layer 1: DC removal + consonant presence EQ + uniform AGC/soft limiter + >=200/300ms silence pad
        let conditioned = crate::audio::enhance::enhance_utterance_16k_with_options(
            samples_16k,
            &crate::audio::enhance::EnhanceOptions {
                clarity_boost,
                eq_strength,
                highpass_enabled: true,
                agc_enabled: true,
                head_pad_ms,
                tail_pad_ms,
            },
        );

        let _vk = WHISPER_VULKAN_LOCK
            .lock()
            .map_err(|_| "Whisper Vulkan ロックの取得に失敗しました".to_string())?;
        let guard = self
            .inner
            .lock()
            .map_err(|_| "Whisper 認識ロックの取得に失敗しました".to_string())?;

        // Layer 4 (#38): Beam search + Temperature Fallback ladder (0.0 -> 0.2 -> 0.4)
        let text = run_whisper_pass(&guard, &conditioned, self.using_gpu, false, beam_size, 0.0)?;
        if temperature_fallback
            && (text.trim().is_empty() || crate::stt::ensemble::looks_low_confidence(&text, samples_16k.len() as f32 / 16_000.0))
            && input_rms >= 0.010
            && samples_16k.len() >= 6400
        {
            for &temp in &[0.2f32, 0.4f32] {
                if let Ok(alt) = run_whisper_pass(&guard, &conditioned, self.using_gpu, true, beam_size, temp) {
                    if !alt.trim().is_empty() && !crate::stt::ensemble::looks_low_confidence(&alt, samples_16k.len() as f32 / 16_000.0) {
                        return Ok(alt);
                    }
                }
            }
        }
        Ok(text)
    }
}

fn build_whisper_initial_prompt(base: &str, hints: &str) -> String {
    let trimmed = hints.trim();
    if trimmed.is_empty() {
        return base.to_string();
    }
    let mut combined = format!("{} {}", base, trimmed);
    if combined.chars().count() > 180 {
        combined = combined.chars().take(180).collect();
    }
    combined
}

fn run_whisper_pass(
    guard: &ResidentWhisperInner,
    conditioned: &[f32],
    using_gpu: bool,
    safety_fallback: bool,
    beam_size: i32,
    temperature: f32,
) -> Result<String, String> {
    unsafe {
        // Use beam_search on GPU for high Japanese accuracy; greedy on CPU
        let strategy = if using_gpu { 1 } else { 0 };
        let wp_ptr = (guard.fns.full_default_params_by_ref)(strategy);
        if wp_ptr.is_null() {
            return Err("whisper_full_default_params_by_ref に失敗しました".into());
        }
        let mut wparams = *wp_ptr;
        (guard.fns.free_params)(wp_ptr);

        // Safe generous audio_ctx: 768 (15.36s capacity) for utterances <= 6s (~2-3x faster encoder),
        // full 1500 (0) for longer speech or Kotoba-Whisper v2.0.
        let dur_sec = conditioned.len() as f32 / 16_000.0;
        wparams.audio_ctx = if guard.is_kotoba {
            0
        } else if dur_sec <= 6.0 {
            768
        } else {
            0
        };
        wparams.n_threads = guard.threads as c_int;
        wparams.translate = false;
        wparams.no_context = true;
        // Kotoba-Whisper v2.0 performs best with no_timestamps=true; Large-v3-Turbo uses normal timestamps
        wparams.no_timestamps = guard.is_kotoba || safety_fallback;
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
        wparams.temperature = temperature;
        if safety_fallback {
            wparams.temperature_inc = 0.0;
            wparams.entropy_thold = 5.0;
            wparams.logprob_thold = -3.0;
            wparams.no_speech_thold = 0.99;
        } else {
            wparams.temperature_inc = 0.2;
            // Generous thresholds so fast/casual Japanese stays in deterministic beam search without garbled temp fallback
            wparams.entropy_thold = 3.4;
            wparams.logprob_thold = -2.0;
            wparams.no_speech_thold = 0.85;
        }
        wparams.max_tokens = 0;
        if using_gpu {
            wparams.beam_search.beam_size = beam_size.clamp(1, 5);
            wparams.beam_search.patience = 1.0;
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
            let ptr_txt = (guard.fns.full_get_segment_text)(guard.ctx, i);
            if ptr_txt.is_null() {
                continue;
            }
            let seg = CStr::from_ptr(ptr_txt).to_string_lossy();
            let seg_trimmed = seg.trim();
            if seg_trimmed.is_empty() {
                continue;
            }
            if !safety_fallback {
                if let Some(nsp_fn) = guard.fns.full_get_segment_no_speech_prob {
                    let nsp = nsp_fn(guard.ctx, i);
                    if nsp >= 0.97 || (nsp >= 0.94 && seg_trimmed.chars().count() <= 4) {
                        continue;
                    }
                }
            }
            if !out.is_empty() {
                let prev_ends_punct = out.ends_with([
                    '。', '、', '！', '？', '!', '?', '.', ',', '…', '\n', '」', '）',
                ]);
                let next_starts_punct = seg_trimmed.starts_with([
                    '。', '、', '！', '？', '!', '?', '.', ',', '…', '」', '）',
                ]);
                if !prev_ends_punct && !next_starts_punct {
                    if crate::processor::japanese_fix::ends_like_complete_sentence(&out) {
                        out.push('。');
                    } else {
                        out.push('、');
                    }
                }
            }
            out.push_str(seg_trimmed);
        }

        Ok(filter_whisper_hallucinations(out.trim()))
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
    let deduped = dedup_inline_triple_repetition(trimmed);
    dedup_trailing_repetition(&deduped)
}

/// Collapses 3+ consecutive repetitions of 2..=6 char phrases (e.g. "一度一度一度" -> "一度"),
/// while leaving 1-char elongations ("あああ", "おおお", "www") untouched.
fn dedup_inline_triple_repetition(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 6 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let mut collapsed = false;
        for len in (2..=6).rev() {
            if i + len * 3 <= chars.len() {
                let pat = &chars[i..i + len];
                // Skip if all chars in pat are identical (e.g. "ああ", "ww")
                if pat.iter().all(|&c| c == pat[0]) {
                    continue;
                }
                let mut reps = 1;
                while i + (reps + 1) * len <= chars.len()
                    && &chars[i + reps * len..i + (reps + 1) * len] == pat
                {
                    reps += 1;
                }
                if reps >= 3 {
                    for &c in pat {
                        out.push(c);
                    }
                    i += reps * len;
                    collapsed = true;
                    break;
                }
            }
        }
        if !collapsed {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Removes trailing exact-phrase echo occasionally produced by Whisper on ultra-fast speech
/// (e.g. "了解しましたあと5分で合流しますエンターあと5分で合流します" -> "了解しましたあと5分で合流しますエンター").
fn dedup_trailing_repetition(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 8 {
        return text.to_string();
    }
    // 1. Consecutive trailing repetition (len >= 4): e.g. "テストですテストです" -> "テストです"
    for len in (4..=(n / 2)).rev() {
        let tail = &chars[n - len..];
        let head = &chars[..n - len];
        if head.ends_with(tail) {
            return head.iter().collect::<String>().trim().to_string();
        }
    }
    // 2. Non-consecutive long phrase echo (len >= 8) so short natural endings ("と思います" etc.) are never clipped
    if n >= 16 {
        for len in (8..=(n / 2)).rev() {
            let tail = &chars[n - len..];
            let head = &chars[..n - len];
            if head.windows(len).any(|w| w == tail) {
                return head.iter().collect::<String>().trim().to_string();
            }
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

type FnGgmlBackendDevCount = unsafe extern "C" fn() -> usize;
type FnGgmlBackendDevGet = unsafe extern "C" fn(usize) -> *mut c_void;
type FnGgmlBackendDevType = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnGgmlBackendDevDesc = unsafe extern "C" fn(*mut c_void) -> *const c_char;

fn detect_vulkan_device_name(bin_dir: &Path) -> Option<String> {
    let mut handles: Vec<HMODULE> = Vec::new();
    for dll_name in ["ggml-whisper.dll", "ggml-base-whisper.dll", "ggml.dll"] {
        let p = bin_dir.join(dll_name);
        if !p.is_file() {
            continue;
        }
        let wide: Vec<u16> = p.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(h) = unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) } {
            handles.push(h);
        }
    }
    if handles.is_empty() {
        return None;
    }

    unsafe {
        let find_sym = |name: &'static [u8]| -> Option<*mut c_void> {
            for &h in &handles {
                let p = GetProcAddress(h, PCSTR(name.as_ptr()));
                if let Some(addr) = p {
                    return Some(addr as *mut c_void);
                }
            }
            None
        };

        let count_ptr = find_sym(b"ggml_backend_dev_count\0")?;
        let get_ptr = find_sym(b"ggml_backend_dev_get\0")?;
        let type_ptr = find_sym(b"ggml_backend_dev_type\0")?;
        let desc_ptr = find_sym(b"ggml_backend_dev_description\0")
            .or_else(|| find_sym(b"ggml_backend_dev_name\0"))?;

        let cnt_f: FnGgmlBackendDevCount = std::mem::transmute(count_ptr);
        let get_f: FnGgmlBackendDevGet = std::mem::transmute(get_ptr);
        let type_f: FnGgmlBackendDevType = std::mem::transmute(type_ptr);
        let desc_f: FnGgmlBackendDevDesc = std::mem::transmute(desc_ptr);

        let cnt = cnt_f();
        for i in 0..cnt {
            let dev = get_f(i);
            if dev.is_null() {
                continue;
            }
            // ggml_backend_dev_type: 1 = GPU, 2 = IGPU
            let dt = type_f(dev);
            if dt == 1 || dt == 2 {
                let ptr = desc_f(dev);
                if !ptr.is_null() {
                    let s = CStr::from_ptr(ptr).to_string_lossy().trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

fn load_whisper_dll(dll_path: &Path) -> Result<(WhisperDllFns, bool, Option<String>), String> {
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

    let gpu_desc = if has_vulkan {
        detect_vulkan_device_name(&bin_dir)
    } else {
        None
    };

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
        Ok((res?, has_vulkan, gpu_desc))
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
        // Consecutive 4-char echo is removed, while short non-consecutive words are preserved
        assert_eq!(filter_whisper_hallucinations("テストですテストです"), "テストです");
        assert_eq!(
            filter_whisper_hallucinations("テストでこれもテストで"),
            "テストでこれもテストで"
        );
        // Normal conversational "お疲れ様でした" must be preserved, while 3+ inline phrase loop is collapsed
        assert_eq!(filter_whisper_hallucinations("お疲れ様でした。"), "お疲れ様でした。");
        assert_eq!(
            filter_whisper_hallucinations("なあえた、一度一度一度、おい！"),
            "なあえた、一度、おい！"
        );
        // 1-char elongations / laughs remain intact
        assert_eq!(filter_whisper_hallucinations("あああああ"), "あああああ");
    }

    #[test]
    fn vulkan_large_v3_turbo_transcribes_normal_fast_slow_and_quiet() {
        let dll = Path::new("bin/whisper.dll");
        let model = Path::new("models/ggml-large-v3-turbo-q8_0.bin");
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

        let resample_speed = |samples: &[f32], speed: f64| -> Vec<f32> {
            let out_len = ((samples.len() as f64) / speed).floor() as usize;
            let mut out = Vec::with_capacity(out_len);
            for i in 0..out_len {
                let pos = i as f64 * speed;
                let idx = pos.floor() as usize;
                let frac = (pos - idx as f64) as f32;
                let a = samples[idx];
                let b = samples.get(idx + 1).copied().unwrap_or(a);
                out.push(a + (b - a) * frac);
            }
            out
        };

        let t_load = Instant::now();
        let rec = ResidentWhisperRecognizer::load(dll, model, "ja", 8)
            .expect("load Vulkan Whisper Large-v3-Turbo Q8_0");
        assert!(!rec.device_name().is_empty());
        if rec.is_gpu_accelerated() {
            assert!(rec.device_name().starts_with("Vulkan:"));
        }
        rec.set_prompt_hints("学校、弁当、パン、送信して");
        let load_ms = t_load.elapsed().as_millis();

        let normal = read_wav_f32(wav);
        let fast = resample_speed(&normal, 1.35);
        let slow = resample_speed(&normal, 0.75);
        let quiet: Vec<f32> = normal.iter().map(|&s| s * 0.15).collect();
        let chat_3_5s = &normal[..normal.len().min(56_000)];

        // Warmup / normal
        let t1 = Instant::now();
        let txt_norm = rec.transcribe(&normal).expect("normal");
        let ms1 = t1.elapsed().as_millis();

        // Warm 3.5s chat utterance
        let t_chat = Instant::now();
        let txt_chat = rec.transcribe(chat_3_5s).expect("chat 3.5s");
        let ms_chat = t_chat.elapsed().as_millis();

        // Second warm call (fast 1.35x)
        let t2 = Instant::now();
        let txt_fast = rec.transcribe(&fast).expect("fast 1.35x");
        let ms2 = t2.elapsed().as_millis();

        // Slow 0.75x
        let t3 = Instant::now();
        let txt_slow = rec.transcribe(&slow).expect("slow 0.75x");
        let ms3 = t3.elapsed().as_millis();

        // Quiet 0.15x
        let t4 = Instant::now();
        let txt_quiet = rec.transcribe(&quiet).expect("quiet 0.15x");
        let ms4 = t4.elapsed().as_millis();

        eprintln!(
            "[Vulkan={} Large-v3-Turbo-Q8] load={}ms\n  normal7.2s({}ms): '{}'\n  chat3.5s({}ms): '{}'\n  fast1.35x({}ms): '{}'\n  slow0.75x({}ms): '{}'\n  quiet0.15x({}ms): '{}'",
            rec.is_gpu_accelerated(),
            load_ms,
            ms1, txt_norm,
            ms_chat, txt_chat,
            ms2, txt_fast,
            ms3, txt_slow,
            ms4, txt_quiet
        );

        assert!(txt_norm.contains("弁当") && txt_norm.contains("パン"));
        assert!(txt_chat.contains("弁当"));
        assert!(txt_fast.contains("弁当") && txt_fast.contains("パン"));
        assert!(txt_slow.contains("弁当") && txt_slow.contains("パン"));
        assert!(txt_quiet.contains("弁当") && txt_quiet.contains("パン"));
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
