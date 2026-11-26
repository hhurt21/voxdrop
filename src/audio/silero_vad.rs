//! Direct FFI wrapper for `sherpa-onnx-c-api.dll` Silero VAD (`SherpaOnnxVoiceActivityDetector`).
//! Provides neural voice activity detection at 16 kHz alongside adaptive energy + RNNoise VAD
//! so quiet/slow speech is captured reliably without ambient hum false triggers.

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

const WINDOW_SIZE: usize = 512;

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxSileroVadModelConfig {
    model: *const c_char,
    threshold: f32,
    min_silence_duration: f32,
    min_speech_duration: f32,
    window_size: i32,
    max_speech_duration: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxTenVadModelConfig {
    model: *const c_char,
    threshold: f32,
    min_silence_duration: f32,
    min_speech_duration: f32,
    window_size: i32,
    max_speech_duration: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxVadModelConfig {
    silero_vad: SherpaOnnxSileroVadModelConfig,
    sample_rate: i32,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    ten_vad: SherpaOnnxTenVadModelConfig,
}

type FnCreateVad = unsafe extern "C" fn(*const SherpaOnnxVadModelConfig, f32) -> *const c_void;
type FnDestroyVad = unsafe extern "C" fn(*const c_void);
type FnAcceptWaveform = unsafe extern "C" fn(*const c_void, *const f32, i32);
type FnDetected = unsafe extern "C" fn(*const c_void) -> i32;
type FnClear = unsafe extern "C" fn(*const c_void);
type FnReset = unsafe extern "C" fn(*const c_void);

struct VadDllFns {
    hmodule: HMODULE,
    create_vad: FnCreateVad,
    destroy_vad: FnDestroyVad,
    accept_waveform: FnAcceptWaveform,
    detected: FnDetected,
    clear: FnClear,
    reset: FnReset,
}

pub struct SileroVadDetector {
    fns: VadDllFns,
    vad: *const c_void,
    buf: Vec<f32>,
    last_detected: bool,
}

// Safety: SileroVadDetector is owned by the capture processing pipeline behind a Mutex.
unsafe impl Send for SileroVadDetector {}

impl Drop for SileroVadDetector {
    fn drop(&mut self) {
        unsafe {
            if !self.vad.is_null() {
                (self.fns.destroy_vad)(self.vad);
                self.vad = ptr::null();
            }
            if !self.fns.hmodule.0.is_null() {
                let _ = FreeLibrary(self.fns.hmodule);
            }
        }
    }
}

impl SileroVadDetector {
    pub fn discover(explicit_model_path: &str, runtime_dir: Option<&Path>) -> Option<Self> {
        let dll_path = find_dll_path(runtime_dir)?;
        let model_path = find_silero_model_path(explicit_model_path, runtime_dir)?;
        match Self::load(&dll_path, &model_path) {
            Ok(det) => Some(det),
            Err(err) => {
                eprintln!("[silero vad warning] {}", err);
                None
            }
        }
    }

    pub fn load(dll_path: &Path, model_path: &Path) -> Result<Self, String> {
        let fns = load_vad_dll_fns(dll_path)?;

        let model_c = CString::new(model_path.to_string_lossy().as_ref())
            .map_err(|_| "Silero VAD モデルパスに不正な文字が含まれています".to_string())?;
        let provider_c = CString::new("cpu").unwrap();

        let mut cfg: SherpaOnnxVadModelConfig = unsafe { std::mem::zeroed() };
        cfg.silero_vad.model = model_c.as_ptr();
        cfg.silero_vad.threshold = 0.35;
        cfg.silero_vad.min_silence_duration = 0.12;
        cfg.silero_vad.min_speech_duration = 0.06;
        cfg.silero_vad.window_size = WINDOW_SIZE as i32;
        cfg.silero_vad.max_speech_duration = 30.0;
        cfg.sample_rate = 16_000;
        cfg.num_threads = 1;
        cfg.provider = provider_c.as_ptr();
        cfg.debug = 0;

        let vad = unsafe { (fns.create_vad)(&cfg, 10.0) };
        if vad.is_null() {
            unsafe {
                let _ = FreeLibrary(fns.hmodule);
            }
            return Err(format!(
                "SherpaOnnxCreateVoiceActivityDetector に失敗しました: {}",
                model_path.display()
            ));
        }

        Ok(Self {
            fns,
            vad,
            buf: Vec::with_capacity(WINDOW_SIZE * 2),
            last_detected: false,
        })
    }

    /// Feeds 16 kHz mono samples and returns whether human speech is currently detected.
    pub fn accept_and_detect(&mut self, samples_16k: &[f32]) -> bool {
        if samples_16k.is_empty() {
            return self.last_detected;
        }
        self.buf.extend_from_slice(samples_16k);
        let mut processed = false;
        let mut any_detected = false;

        while self.buf.len() >= WINDOW_SIZE {
            unsafe {
                (self.fns.accept_waveform)(self.vad, self.buf.as_ptr(), WINDOW_SIZE as i32);
                if (self.fns.detected)(self.vad) != 0 {
                    any_detected = true;
                }
            }
            self.buf.drain(0..WINDOW_SIZE);
            processed = true;
        }

        if processed {
            unsafe {
                (self.fns.clear)(self.vad);
            }
            self.last_detected = any_detected;
        }

        self.last_detected
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.last_detected = false;
        unsafe {
            (self.fns.reset)(self.vad);
            (self.fns.clear)(self.vad);
        }
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn find_dll_path(runtime_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from("bin"), PathBuf::from(".")];
    if let Some(dir) = exe_dir() {
        dirs.push(dir.join("bin"));
        dirs.push(dir.clone());
        if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
            dirs.push(parent.join("bin"));
        }
    }
    if let Some(rt) = runtime_dir {
        dirs.push(rt.join("bin"));
        dirs.push(rt.to_path_buf());
    }
    dirs.push(crate::runtime::runtime_cache_dir().join("bin"));

    for dir in dirs {
        let p = dir.join("sherpa-onnx-c-api.dll");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn find_silero_model_path(explicit_path: &str, runtime_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = explicit_path.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("models/silero_vad.onnx")];
    if let Some(dir) = exe_dir() {
        candidates.push(dir.join("models").join("silero_vad.onnx"));
        if let Some(parent) = dir.parent().and_then(|p| p.parent()) {
            candidates.push(parent.join("models").join("silero_vad.onnx"));
        }
    }
    if let Some(rt) = runtime_dir {
        candidates.push(rt.join("models").join("silero_vad.onnx"));
    }
    candidates.push(
        crate::runtime::runtime_cache_dir()
            .join("models")
            .join("silero_vad.onnx"),
    );

    candidates.into_iter().find(|p| p.is_file())
}

unsafe fn load_symbol<T>(hmodule: HMODULE, name: &'static [u8]) -> Result<T, String> {
    let proc = GetProcAddress(hmodule, PCSTR(name.as_ptr())).ok_or_else(|| {
        format!(
            "DLLシンボル '{}' が見つかりません",
            String::from_utf8_lossy(name).trim_end_matches('\0')
        )
    })?;
    Ok(std::mem::transmute_copy(&proc))
}

fn load_vad_dll_fns(dll_path: &Path) -> Result<VadDllFns, String> {
    let abs_path = std::fs::canonicalize(dll_path).unwrap_or_else(|_| dll_path.to_path_buf());
    let path_str = abs_path.to_string_lossy();
    let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
    let wide: Vec<u16> = clean_path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let hmodule = LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
        .map_err(|e| {
            format!(
                "sherpa-onnx-c-api.dll の読み込みに失敗しました ({}): {}",
                clean_path, e
            )
        })?;

        let result = (|| -> Result<VadDllFns, String> {
            Ok(VadDllFns {
                hmodule,
                create_vad: load_symbol(hmodule, b"SherpaOnnxCreateVoiceActivityDetector\0")?,
                destroy_vad: load_symbol(hmodule, b"SherpaOnnxDestroyVoiceActivityDetector\0")?,
                accept_waveform: load_symbol(
                    hmodule,
                    b"SherpaOnnxVoiceActivityDetectorAcceptWaveform\0",
                )?,
                detected: load_symbol(hmodule, b"SherpaOnnxVoiceActivityDetectorDetected\0")?,
                clear: load_symbol(hmodule, b"SherpaOnnxVoiceActivityDetectorClear\0")?,
                reset: load_symbol(hmodule, b"SherpaOnnxVoiceActivityDetectorReset\0")?,
            })
        })();

        if result.is_err() {
            let _ = FreeLibrary(hmodule);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_wav_f32(p: &Path) -> Vec<f32> {
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
    }

    fn resample_speed(samples: &[f32], speed: f64) -> Vec<f32> {
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
    }

    #[test]
    fn silero_vad_detects_normal_quiet_and_fast_speech_and_rejects_silence() {
        let dll = Path::new("bin/sherpa-onnx-c-api.dll");
        let model = Path::new("models/silero_vad.onnx");
        let wav = Path::new(
            "models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/test_wavs/ja.wav",
        );
        if !dll.is_file() || !model.is_file() || !wav.is_file() {
            return;
        }

        let mut det = SileroVadDetector::load(dll, model).expect("load silero vad");

        // 1. 1 second of silence must stay false
        let silence = vec![0.0f32; 16_000];
        for chunk in silence.chunks(320) {
            assert!(!det.accept_and_detect(chunk));
        }

        let samples = read_wav_f32(wav);

        // 2. Normal speech
        det.reset();
        let mut normal_hits = 0usize;
        for chunk in samples.chunks(320) {
            if det.accept_and_detect(chunk) {
                normal_hits += 1;
            }
        }
        assert!(normal_hits > 10, "expected normal speech hits, got {}", normal_hits);

        // 3. Quiet speech (0.2x amplitude)
        det.reset();
        let quiet: Vec<f32> = samples.iter().map(|&s| s * 0.2).collect();
        let mut quiet_hits = 0usize;
        for chunk in quiet.chunks(320) {
            if det.accept_and_detect(chunk) {
                quiet_hits += 1;
            }
        }
        assert!(quiet_hits > 5, "expected quiet speech hits, got {}", quiet_hits);

        // 4. Fast speech (1.35x speed)
        det.reset();
        let fast = resample_speed(&samples, 1.35);
        let mut fast_hits = 0usize;
        for chunk in fast.chunks(320) {
            if det.accept_and_detect(chunk) {
                fast_hits += 1;
            }
        }
        assert!(fast_hits > 5, "expected fast speech hits, got {}", fast_hits);
    }
}
