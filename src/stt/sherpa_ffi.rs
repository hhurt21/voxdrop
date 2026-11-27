//! Direct resident FFI wrapper for `sherpa-onnx-c-api.dll` (v2.0.3 ABI).
//! Loads the ONNX model ONCE in memory at startup so every utterance transcribes in ~0.15-0.30s
//! instead of spawning a subprocess and paying a 3-second model reload every time!

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxFeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineCtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxHomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineTransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineParaformerModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineNemoEncDecCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineWhisperModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    task: *const c_char,
    tail_paddings: i32,
    enable_token_timestamps: i32,
    enable_segment_timestamps: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineCanaryModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    src_lang: *const c_char,
    tgt_lang: *const c_char,
    use_pnc: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineCohereTranscribeModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    language: *const c_char,
    use_punct: i32,
    use_itn: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineFireRedAsrModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineFireRedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineMoonshineModelConfig {
    preprocessor: *const c_char,
    encoder: *const c_char,
    uncached_decoder: *const c_char,
    cached_decoder: *const c_char,
    merged_decoder: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineTdnnModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineLMConfig {
    model: *const c_char,
    scale: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSenseVoiceModelConfig {
    model: *const c_char,
    language: *const c_char,
    use_itn: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineDolphinModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineZipformerCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineWenetCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineOmnilingualAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineFunASRNanoModelConfig {
    encoder_adaptor: *const c_char,
    llm: *const c_char,
    embedding: *const c_char,
    tokenizer: *const c_char,
    system_prompt: *const c_char,
    user_prompt: *const c_char,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    language: *const c_char,
    itn: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineQwen3ASRModelConfig {
    conv_frontend: *const c_char,
    encoder: *const c_char,
    decoder: *const c_char,
    tokenizer: *const c_char,
    max_total_len: i32,
    max_new_tokens: i32,
    temperature: f32,
    top_p: f32,
    seed: i32,
    hotwords: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineMedAsrCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineModelConfig {
    transducer: SherpaOnnxOfflineTransducerModelConfig,
    paraformer: SherpaOnnxOfflineParaformerModelConfig,
    nemo_ctc: SherpaOnnxOfflineNemoEncDecCtcModelConfig,
    whisper: SherpaOnnxOfflineWhisperModelConfig,
    tdnn: SherpaOnnxOfflineTdnnModelConfig,
    tokens: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    telespeech_ctc: *const c_char,
    sense_voice: SherpaOnnxOfflineSenseVoiceModelConfig,
    moonshine: SherpaOnnxOfflineMoonshineModelConfig,
    fire_red_asr: SherpaOnnxOfflineFireRedAsrModelConfig,
    dolphin: SherpaOnnxOfflineDolphinModelConfig,
    zipformer_ctc: SherpaOnnxOfflineZipformerCtcModelConfig,
    canary: SherpaOnnxOfflineCanaryModelConfig,
    wenet_ctc: SherpaOnnxOfflineWenetCtcModelConfig,
    omnilingual: SherpaOnnxOfflineOmnilingualAsrCtcModelConfig,
    medasr: SherpaOnnxOfflineMedAsrCtcModelConfig,
    funasr_nano: SherpaOnnxOfflineFunASRNanoModelConfig,
    fire_red_asr_ctc: SherpaOnnxOfflineFireRedAsrCtcModelConfig,
    qwen3_asr: SherpaOnnxOfflineQwen3ASRModelConfig,
    cohere_transcribe: SherpaOnnxOfflineCohereTranscribeModelConfig,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineRecognizerConfig {
    feat_config: SherpaOnnxFeatureConfig,
    model_config: SherpaOnnxOfflineModelConfig,
    lm_config: SherpaOnnxOfflineLMConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hr: SherpaOnnxHomophoneReplacerConfig,
    ctc_fst_decoder_config: SherpaOnnxOfflineCtcFstDecoderConfig,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSpeechDenoiserGtcrnModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSpeechDenoiserDpdfNetModelConfig {
    model: *const c_char,
    attenuation_limit_db: f32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSpeechDenoiserVoxLockModelConfig {
    enroll_model: *const c_char,
    chunk_model: *const c_char,
    voice_sample: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSpeechDenoiserModelConfig {
    gtcrn: SherpaOnnxOfflineSpeechDenoiserGtcrnModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    dpdfnet: SherpaOnnxOfflineSpeechDenoiserDpdfNetModelConfig,
    voxlock: SherpaOnnxOfflineSpeechDenoiserVoxLockModelConfig,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SherpaOnnxOfflineSpeechDenoiserConfig {
    model: SherpaOnnxOfflineSpeechDenoiserModelConfig,
}

#[repr(C)]
struct SherpaOnnxDenoisedAudio {
    samples: *const f32,
    n: i32,
    sample_rate: i32,
}

type FnCreateOfflineRecognizer =
    unsafe extern "C" fn(*const SherpaOnnxOfflineRecognizerConfig) -> *const c_void;
type FnDestroyOfflineRecognizer = unsafe extern "C" fn(*const c_void);
type FnCreateOfflineStream = unsafe extern "C" fn(*const c_void) -> *const c_void;
type FnDestroyOfflineStream = unsafe extern "C" fn(*const c_void);
type FnAcceptWaveformOffline = unsafe extern "C" fn(*const c_void, i32, *const f32, i32);
type FnDecodeOfflineStream = unsafe extern "C" fn(*const c_void, *const c_void);
type FnGetOfflineStreamResultAsJson = unsafe extern "C" fn(*const c_void) -> *const c_char;
type FnDestroyOfflineStreamResultJson = unsafe extern "C" fn(*const c_char);
type FnCreateOfflineSpeechDenoiser =
    unsafe extern "C" fn(*const SherpaOnnxOfflineSpeechDenoiserConfig) -> *const c_void;
type FnDestroyOfflineSpeechDenoiser = unsafe extern "C" fn(*const c_void);
type FnOfflineSpeechDenoiserRun =
    unsafe extern "C" fn(*const c_void, *const f32, i32, i32) -> *const SherpaOnnxDenoisedAudio;
type FnDestroyDenoisedAudio = unsafe extern "C" fn(*const SherpaOnnxDenoisedAudio);

struct SherpaDllFns {
    hmodule: HMODULE,
    create_recognizer: FnCreateOfflineRecognizer,
    destroy_recognizer: FnDestroyOfflineRecognizer,
    create_stream: FnCreateOfflineStream,
    destroy_stream: FnDestroyOfflineStream,
    accept_waveform: FnAcceptWaveformOffline,
    decode_stream: FnDecodeOfflineStream,
    get_result_json: FnGetOfflineStreamResultAsJson,
    destroy_result_json: FnDestroyOfflineStreamResultJson,
    create_denoiser: Option<FnCreateOfflineSpeechDenoiser>,
    destroy_denoiser: Option<FnDestroyOfflineSpeechDenoiser>,
    run_denoiser: Option<FnOfflineSpeechDenoiserRun>,
    destroy_denoised_audio: Option<FnDestroyDenoisedAudio>,
}

struct ResidentInner {
    fns: SherpaDllFns,
    recognizer: *const c_void,
    denoiser: *const c_void,
}

// Safety: Access is strictly serialized behind a Mutex.
unsafe impl Send for ResidentInner {}

impl Drop for ResidentInner {
    fn drop(&mut self) {
        unsafe {
            if !self.denoiser.is_null() {
                if let Some(destroy_fn) = self.fns.destroy_denoiser {
                    destroy_fn(self.denoiser);
                }
                self.denoiser = ptr::null();
            }
            if !self.recognizer.is_null() {
                (self.fns.destroy_recognizer)(self.recognizer);
                self.recognizer = ptr::null();
            }
            if !self.fns.hmodule.0.is_null() {
                let _ = FreeLibrary(self.fns.hmodule);
            }
        }
    }
}

pub enum ResidentModelSpec<'a> {
    Whisper {
        encoder: &'a Path,
        decoder: &'a Path,
        tokens: &'a Path,
        language: &'a str,
    },
    NemoCtc {
        model: &'a Path,
        tokens: &'a Path,
    },
    SenseVoice {
        model: &'a Path,
        tokens: &'a Path,
        language: &'a str,
    },
    Moonshine {
        encoder: &'a Path,
        merged_decoder: &'a Path,
        tokens: &'a Path,
    },
    Transducer {
        encoder: &'a Path,
        decoder: &'a Path,
        joiner: &'a Path,
        tokens: &'a Path,
    },
    Paraformer {
        model: &'a Path,
        tokens: &'a Path,
    },
    Qwen3Asr {
        conv_frontend: &'a Path,
        encoder: &'a Path,
        decoder: &'a Path,
        tokenizer: &'a Path,
        hotwords: &'a str,
    },
}

pub enum SpeechDenoiserSpec<'a> {
    DpdfNet {
        model: &'a Path,
        attenuation_limit_db: f32,
    },
    Gtcrn {
        model: &'a Path,
    },
}

/// In-memory resident Sherpa-ONNX recognizer for sub-300ms low-latency transcription.
pub struct ResidentSherpaRecognizer {
    inner: Mutex<ResidentInner>,
    denoiser_label: Option<String>,
    provider_label: String,
    is_whisper_or_qwen: bool,
}

impl ResidentSherpaRecognizer {
    pub fn load(
        dll_path: &Path,
        spec: ResidentModelSpec<'_>,
        denoiser_spec: Option<SpeechDenoiserSpec<'_>>,
        threads: u32,
    ) -> Result<Self, String> {
        let fns = load_dll_fns(dll_path)?;

        let mut keep_alive: Vec<CString> = Vec::new();
        let mut cstr = |s: &str| -> Result<*const c_char, String> {
            let cs = CString::new(s)
                .map_err(|_| format!("文字列にNUL文字が含まれています: {}", s))?;
            let ptr = cs.as_ptr();
            keep_alive.push(cs);
            Ok(ptr)
        };

        let mut config: SherpaOnnxOfflineRecognizerConfig = unsafe { std::mem::zeroed() };
        config.feat_config.sample_rate = 16_000;
        config.feat_config.feature_dim = 80;
        config.model_config.num_threads = threads.clamp(1, 16) as i32;
        config.model_config.debug = 0;
        config.decoding_method = cstr("greedy_search")?;
        config.max_active_paths = 4;

        let is_qwen3 = matches!(spec, ResidentModelSpec::Qwen3Asr { .. });
        let is_whisper_or_qwen = matches!(
            spec,
            ResidentModelSpec::Whisper { .. } | ResidentModelSpec::Qwen3Asr { .. }
        );

        match spec {
            ResidentModelSpec::Whisper {
                encoder,
                decoder,
                tokens,
                language,
            } => {
                config.model_config.whisper.encoder = cstr(&encoder.to_string_lossy())?;
                config.model_config.whisper.decoder = cstr(&decoder.to_string_lossy())?;
                config.model_config.whisper.language = cstr(language)?;
                config.model_config.whisper.task = cstr("transcribe")?;
                config.model_config.whisper.tail_paddings = -1;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::NemoCtc { model, tokens } => {
                config.model_config.nemo_ctc.model = cstr(&model.to_string_lossy())?;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::SenseVoice {
                model,
                tokens,
                language,
            } => {
                config.model_config.sense_voice.model = cstr(&model.to_string_lossy())?;
                config.model_config.sense_voice.language = cstr(language)?;
                config.model_config.sense_voice.use_itn = 1;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::Moonshine {
                encoder,
                merged_decoder,
                tokens,
            } => {
                config.model_config.moonshine.encoder = cstr(&encoder.to_string_lossy())?;
                config.model_config.moonshine.merged_decoder =
                    cstr(&merged_decoder.to_string_lossy())?;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::Transducer {
                encoder,
                decoder,
                joiner,
                tokens,
            } => {
                config.model_config.transducer.encoder = cstr(&encoder.to_string_lossy())?;
                config.model_config.transducer.decoder = cstr(&decoder.to_string_lossy())?;
                config.model_config.transducer.joiner = cstr(&joiner.to_string_lossy())?;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::Paraformer { model, tokens } => {
                config.model_config.paraformer.model = cstr(&model.to_string_lossy())?;
                config.model_config.tokens = cstr(&tokens.to_string_lossy())?;
            }
            ResidentModelSpec::Qwen3Asr {
                conv_frontend,
                encoder,
                decoder,
                tokenizer,
                hotwords,
            } => {
                config.model_config.qwen3_asr.conv_frontend =
                    cstr(&conv_frontend.to_string_lossy())?;
                config.model_config.qwen3_asr.encoder = cstr(&encoder.to_string_lossy())?;
                config.model_config.qwen3_asr.decoder = cstr(&decoder.to_string_lossy())?;
                config.model_config.qwen3_asr.tokenizer = cstr(&tokenizer.to_string_lossy())?;
                config.model_config.qwen3_asr.max_total_len = 512;
                config.model_config.qwen3_asr.max_new_tokens = 256;
                config.model_config.qwen3_asr.temperature = 0.0;
                config.model_config.qwen3_asr.top_p = 1.0;
                config.model_config.qwen3_asr.seed = 42;
                config.model_config.qwen3_asr.hotwords = cstr(hotwords)?;
                config.model_config.tokens = cstr("")?;
            }
        }

        // Try DirectML (AMD RX 6700 XT GPU acceleration) first for Qwen3-ASR when available,
        // falling back cleanly to CPU if the bundled ONNX Runtime was built CPU-only.
        let mut provider_label = "CPU".to_string();
        let mut recognizer: *const c_void = ptr::null();
        if is_qwen3 {
            config.model_config.provider = cstr("directml")?;
            recognizer = unsafe { (fns.create_recognizer)(&config) };
            if !recognizer.is_null() {
                provider_label = "DirectML (GPU)".to_string();
            }
        }
        if recognizer.is_null() {
            config.model_config.provider = cstr("cpu")?;
            recognizer = unsafe { (fns.create_recognizer)(&config) };
            provider_label = "CPU".to_string();
        }
        drop(keep_alive);

        if recognizer.is_null() {
            unsafe {
                let _ = FreeLibrary(fns.hmodule);
            }
            return Err("SherpaOnnxCreateOfflineRecognizer が失敗しました（モデルまたは設定を確認してください）".into());
        }

        // Whisper Large-v3-Turbo & Qwen3-ASR have noise-robust encoders;
        // double neural denoising (RNNoise + DPDFNet2) degrades muffled/slurred consonants,
        // so they use clean AGC directly for maximum accuracy & speed!
        let mut denoiser: *const c_void = ptr::null();
        let mut denoiser_label: Option<String> = None;
        if !is_whisper_or_qwen {
            if let (Some(dspec), Some(create_sd)) = (denoiser_spec, fns.create_denoiser) {
                if let Ok(prov_c) = CString::new("cpu") {
                    let mut sd_cfg: SherpaOnnxOfflineSpeechDenoiserConfig =
                        unsafe { std::mem::zeroed() };
                    sd_cfg.model.num_threads = threads.clamp(1, 4) as i32;
                    sd_cfg.model.debug = 0;
                    sd_cfg.model.provider = prov_c.as_ptr();
                    match dspec {
                        SpeechDenoiserSpec::DpdfNet {
                            model,
                            attenuation_limit_db,
                        } => {
                            if let Ok(m_c) = CString::new(model.to_string_lossy().as_ref()) {
                                sd_cfg.model.dpdfnet.model = m_c.as_ptr();
                                sd_cfg.model.dpdfnet.attenuation_limit_db = attenuation_limit_db;
                                let ptr_sd = unsafe { create_sd(&sd_cfg) };
                                if !ptr_sd.is_null() {
                                    denoiser = ptr_sd;
                                    denoiser_label = Some("DPDFNet2".into());
                                }
                            }
                        }
                        SpeechDenoiserSpec::Gtcrn { model } => {
                            if let Ok(m_c) = CString::new(model.to_string_lossy().as_ref()) {
                                sd_cfg.model.gtcrn.model = m_c.as_ptr();
                                let ptr_sd = unsafe { create_sd(&sd_cfg) };
                                if !ptr_sd.is_null() {
                                    denoiser = ptr_sd;
                                    denoiser_label = Some("GTCRN".into());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(Self {
            inner: Mutex::new(ResidentInner {
                fns,
                recognizer,
                denoiser,
            }),
            denoiser_label,
            provider_label,
            is_whisper_or_qwen,
        })
    }

    pub fn denoiser_name(&self) -> Option<&str> {
        self.denoiser_label.as_deref()
    }

    pub fn device_name(&self) -> &str {
        &self.provider_label
    }

    pub fn transcribe_json_enhanced(
        &self,
        samples_16k: &[f32],
        radio_clarity: bool,
    ) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let guard = self
            .inner
            .lock()
            .map_err(|_| "STT認識エンジンのロック取得に失敗しました".to_string())?;

        // 1. Apply smart quiet-mic AGC (preserve natural Mel tilt for Whisper / Qwen3-ASR!)
        let pre_conditioned = crate::audio::enhance::enhance_utterance_16k(
            samples_16k,
            radio_clarity && !self.is_whisper_or_qwen && guard.denoiser.is_null(),
            true,
        );

        // 2. Run neural speech enhancement (DPDFNet2 / GTCRN) if enabled & loaded!
        //    Running AFTER quiet-mic AGC ensures DPDFNet2 never treats quiet speech as background noise!
        let final_samples: Vec<f32> = if radio_clarity && !guard.denoiser.is_null() {
            if let (Some(run_sd), Some(free_sd)) =
                (guard.fns.run_denoiser, guard.fns.destroy_denoised_audio)
            {
                unsafe {
                    let res = run_sd(
                        guard.denoiser,
                        pre_conditioned.as_ptr(),
                        pre_conditioned.len() as i32,
                        16_000,
                    );
                    if !res.is_null() && !(*res).samples.is_null() && (*res).n > 0 {
                        let slice = std::slice::from_raw_parts((*res).samples, (*res).n as usize);
                        let copy = slice.to_vec();
                        free_sd(res);
                        copy
                    } else {
                        if !res.is_null() {
                            free_sd(res);
                        }
                        pre_conditioned
                    }
                }
            } else {
                pre_conditioned
            }
        } else {
            pre_conditioned
        };

        unsafe {
            let stream = (guard.fns.create_stream)(guard.recognizer);
            if stream.is_null() {
                return Err("SherpaOnnxCreateOfflineStream に失敗しました".into());
            }

            (guard.fns.accept_waveform)(
                stream,
                16_000,
                final_samples.as_ptr(),
                final_samples.len() as i32,
            );
            (guard.fns.decode_stream)(guard.recognizer, stream);

            let json_ptr = (guard.fns.get_result_json)(stream);
            let json_str = if !json_ptr.is_null() {
                let s = CStr::from_ptr(json_ptr).to_string_lossy().into_owned();
                (guard.fns.destroy_result_json)(json_ptr);
                s
            } else {
                String::new()
            };

            (guard.fns.destroy_stream)(stream);
            Ok(json_str)
        }
    }

    pub fn transcribe_json(&self, samples_16k: &[f32]) -> Result<String, String> {
        self.transcribe_json_enhanced(samples_16k, true)
    }
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

fn load_dll_fns(dll_path: &Path) -> Result<SherpaDllFns, String> {
    let abs_path = std::fs::canonicalize(dll_path).unwrap_or_else(|_| dll_path.to_path_buf());
    // Strip extended-length verbatim prefix \\?\ if present so LoadLibraryExW resolves sibling DLLs cleanly
    let path_str = abs_path.to_string_lossy();
    let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
    let wide: Vec<u16> = clean_path.encode_utf16().chain(std::iter::once(0)).collect();

    if let Some(bin_dir) = Path::new(clean_path).parent() {
        let dir_wide: Vec<u16> = bin_dir
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let _ = windows::Win32::System::LibraryLoader::SetDllDirectoryW(PCWSTR(dir_wide.as_ptr()));
        }
    }

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

        let result = (|| -> Result<SherpaDllFns, String> {
            Ok(SherpaDllFns {
                hmodule,
                create_recognizer: load_symbol(
                    hmodule,
                    b"SherpaOnnxCreateOfflineRecognizer\0",
                )?,
                destroy_recognizer: load_symbol(
                    hmodule,
                    b"SherpaOnnxDestroyOfflineRecognizer\0",
                )?,
                create_stream: load_symbol(hmodule, b"SherpaOnnxCreateOfflineStream\0")?,
                destroy_stream: load_symbol(hmodule, b"SherpaOnnxDestroyOfflineStream\0")?,
                accept_waveform: load_symbol(hmodule, b"SherpaOnnxAcceptWaveformOffline\0")?,
                decode_stream: load_symbol(hmodule, b"SherpaOnnxDecodeOfflineStream\0")?,
                get_result_json: load_symbol(
                    hmodule,
                    b"SherpaOnnxGetOfflineStreamResultAsJson\0",
                )?,
                destroy_result_json: load_symbol(
                    hmodule,
                    b"SherpaOnnxDestroyOfflineStreamResultJson\0",
                )?,
                create_denoiser: load_symbol(
                    hmodule,
                    b"SherpaOnnxCreateOfflineSpeechDenoiser\0",
                )
                .ok(),
                destroy_denoiser: load_symbol(
                    hmodule,
                    b"SherpaOnnxDestroyOfflineSpeechDenoiser\0",
                )
                .ok(),
                run_denoiser: load_symbol(hmodule, b"SherpaOnnxOfflineSpeechDenoiserRun\0").ok(),
                destroy_denoised_audio: load_symbol(
                    hmodule,
                    b"SherpaOnnxDestroyDenoisedAudio\0",
                )
                .ok(),
            })
        })();

        if result.is_err() {
            let _ = FreeLibrary(hmodule);
        }
        result
    }
}

/// Standalone neural speech denoiser (DPDFNet-16k / DPDFNet-48k / GTCRN-16k) using
/// `sherpa-onnx-c-api.dll` with DirectML -> CPU safe fallback (Section 3.1 / #21).
pub struct ResidentSherpaDenoiser {
    fns: SherpaDllFns,
    denoiser: *const c_void,
    #[allow(dead_code)]
    label: String,
    device_label: String,
}

unsafe impl Send for ResidentSherpaDenoiser {}
unsafe impl Sync for ResidentSherpaDenoiser {}

impl Drop for ResidentSherpaDenoiser {
    fn drop(&mut self) {
        unsafe {
            if !self.denoiser.is_null() {
                if let Some(destroy_fn) = self.fns.destroy_denoiser {
                    destroy_fn(self.denoiser);
                }
                self.denoiser = ptr::null();
            }
        }
    }
}

impl ResidentSherpaDenoiser {
    pub fn load(
        dll_path: &Path,
        kind: &str,
        model_path: &Path,
        attenuation_limit_db: f32,
        threads: u32,
    ) -> Result<Self, String> {
        let fns = load_dll_fns(dll_path)?;
        let create_sd = fns
            .create_denoiser
            .ok_or_else(|| "sherpa-onnx-c-api.dll に SpeechDenoiser API がありません".to_string())?;
        let model_c = CString::new(model_path.to_string_lossy().as_ref())
            .map_err(|_| "ノイズ抑制モデルパスが不正です".to_string())?;

        let mut picked_ptr: *const c_void = ptr::null();
        let mut device_label = String::from("CPU");

        for prov in ["directml", "cpu"] {
            let Ok(prov_c) = CString::new(prov) else {
                continue;
            };
            let mut sd_cfg: SherpaOnnxOfflineSpeechDenoiserConfig = unsafe { std::mem::zeroed() };
            sd_cfg.model.num_threads = threads.clamp(1, 4) as i32;
            sd_cfg.model.debug = 0;
            sd_cfg.model.provider = prov_c.as_ptr();
            if kind.starts_with("gtcrn") {
                sd_cfg.model.gtcrn.model = model_c.as_ptr();
            } else {
                sd_cfg.model.dpdfnet.model = model_c.as_ptr();
                sd_cfg.model.dpdfnet.attenuation_limit_db = normalize_dpdfnet_db(attenuation_limit_db);
            }
            let ptr_sd = unsafe { create_sd(&sd_cfg) };
            if !ptr_sd.is_null() {
                picked_ptr = ptr_sd;
                device_label = if prov == "directml" {
                    "DirectML (GPU)".into()
                } else {
                    "CPU".into()
                };
                break;
            }
        }

        if picked_ptr.is_null() {
            return Err(format!(
                "ノイズ抑制モデルの初期化に失敗しました ({}): {}",
                kind,
                model_path.display()
            ));
        }

        let label = match kind {
            "dpdfnet-48k" => "DPDFNet (48kHz)",
            "gtcrn-16k" => "GTCRN (16kHz)",
            _ => "DPDFNet (16kHz)",
        }
        .to_string();

        Ok(Self {
            fns,
            denoiser: picked_ptr,
            label,
            device_label,
        })
    }

    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.label
    }

    pub fn device_name(&self) -> &str {
        &self.device_label
    }

    pub fn denoise(&self, samples: &[f32], sample_rate: i32) -> Result<Vec<f32>, String> {
        if samples.is_empty() || self.denoiser.is_null() {
            return Ok(samples.to_vec());
        }
        let run_sd = self
            .fns
            .run_denoiser
            .ok_or_else(|| "SherpaOnnxOfflineSpeechDenoiserRun が見つかりません".to_string())?;
        let free_sd = self
            .fns
            .destroy_denoised_audio
            .ok_or_else(|| "SherpaOnnxDestroyDenoisedAudio が見つかりません".to_string())?;
        unsafe {
            let res = run_sd(self.denoiser, samples.as_ptr(), samples.len() as i32, sample_rate);
            if !res.is_null() && !(*res).samples.is_null() && (*res).n > 0 {
                let slice = std::slice::from_raw_parts((*res).samples, (*res).n as usize);
                let out = slice.to_vec();
                free_sd(res);
                Ok(out)
            } else {
                if !res.is_null() {
                    free_sd(res);
                }
                Ok(samples.to_vec())
            }
        }
    }
}

pub(crate) fn normalize_dpdfnet_db(db: f32) -> f32 {
    db.abs().clamp(0.0, 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::local::parse_sherpa_output;
    use std::time::Instant;

    #[test]
    fn resident_dll_transcribes_japanese_wav_fast() {
        let dll = Path::new("bin/sherpa-onnx-c-api.dll");
        let w_enc = Path::new("models/sherpa-onnx-whisper-turbo/turbo-encoder.int8.onnx");
        let w_dec = Path::new("models/sherpa-onnx-whisper-turbo/turbo-decoder.int8.onnx");
        let w_tok = Path::new("models/sherpa-onnx-whisper-turbo/turbo-tokens.txt");
        let wav = Path::new(
            "models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/test_wavs/ja.wav",
        );
        if !dll.is_file() || !w_enc.is_file() || !w_dec.is_file() || !w_tok.is_file() || !wav.is_file() {
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

        let samples = read_wav_f32(wav);
        let dpdfnet = Path::new("models/dpdfnet2.onnx");
        let dspec = if dpdfnet.is_file() {
            Some(SpeechDenoiserSpec::DpdfNet {
                model: dpdfnet,
                attenuation_limit_db: 12.0,
            })
        } else {
            None
        };
        let t_load = Instant::now();
        let rec = ResidentSherpaRecognizer::load(
            dll,
            ResidentModelSpec::Whisper {
                encoder: w_enc,
                decoder: w_dec,
                tokens: w_tok,
                language: "ja",
            },
            dspec,
            8,
        )
        .expect("load resident whisper-turbo recognizer");
        let load_ms = t_load.elapsed().as_millis();

        let t1 = Instant::now();
        let json1 = rec.transcribe_json_enhanced(&samples, true).expect("transcribe 1");
        let ms1 = t1.elapsed().as_millis();
        let text1 = parse_sherpa_output(&json1);
        eprintln!(
            "[Whisper-Large-v3-Turbo + SmartAGC] load={}ms, infer={}ms, text='{}'",
            load_ms, ms1, text1
        );
        assert!(text1.contains("弁当") && text1.contains("パン"));
        assert!(text1.contains("持っていけない"));
    }

    #[test]
    fn standalone_dpdfnet_denoiser_processes_16k_audio_cleanly() {
        let dll = Path::new("bin/sherpa-onnx-c-api.dll");
        let model = Path::new("models/dpdfnet2.onnx");
        if !dll.is_file() || !model.is_file() {
            return;
        }
        let sd = ResidentSherpaDenoiser::load(dll, "dpdfnet-16k", model, 12.0, 4)
            .expect("load DPDFNet2 standalone denoiser");
        assert!(!sd.device_name().is_empty());
        let noisy: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 0.08).sin() * 0.08)
            .collect();
        let out = sd.denoise(&noisy, 16_000).expect("denoise 16k");
        assert!(!out.is_empty());
    }

    #[test]
    fn normalizes_dpdfnet_attenuation_limit_db_safely() {
        assert_eq!(normalize_dpdfnet_db(-18.0), 18.0);
        assert_eq!(normalize_dpdfnet_db(12.0), 12.0);
        assert_eq!(normalize_dpdfnet_db(99.0), 60.0);
    }
}

