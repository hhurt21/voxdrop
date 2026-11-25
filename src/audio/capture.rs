use crate::audio::denoise::{AudioDenoiser, RNNOISE_SAMPLE_RATE};
use crate::audio::vad::{VadEvent, VadSegmenter};
use crate::audio::AudioEvent;
use crate::config::{NoiseCancelConfig, VadConfig};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Lists available audio input device names on the default host (WASAPI on Windows).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Smart multi-channel -> mono downmixer specifically designed for 2-channel USB audio interfaces
/// (like Steinberg UR22mkII / Scarlett 2i2) where a mono mic is plugged into Input 1 (Left)
/// and Input 2 (Right) is empty: automatically locks onto the active mic channel at 100% full gain
/// instead of halving the voice amplitude (-6 dB) and mixing in empty-channel noise!
pub struct SmartDownmixer {
    ch_energy: [f32; 2],
}

impl SmartDownmixer {
    pub fn new() -> Self {
        Self {
            ch_energy: [0.0, 0.0],
        }
    }

    pub fn process(&mut self, samples: &[f32], channels: usize) -> Vec<f32> {
        if channels <= 1 {
            return samples.to_vec();
        }
        if channels == 2 {
            let frames = samples.len() / 2;
            let mut sum_l = 0.0f32;
            let mut sum_r = 0.0f32;
            for chunk in samples.chunks_exact(2) {
                sum_l += chunk[0] * chunk[0];
                sum_r += chunk[1] * chunk[1];
            }
            let rms_l = (sum_l / frames.max(1) as f32).sqrt();
            let rms_r = (sum_r / frames.max(1) as f32).sqrt();
            // Smooth rolling channel activity estimate
            self.ch_energy[0] = self.ch_energy[0] * 0.92 + rms_l * 0.08;
            self.ch_energy[1] = self.ch_energy[1] * 0.92 + rms_r * 0.08;

            let mut out = Vec::with_capacity(frames);
            if self.ch_energy[0] > self.ch_energy[1] * 2.2 && self.ch_energy[0] > 0.0005 {
                for chunk in samples.chunks_exact(2) {
                    out.push(chunk[0]);
                }
            } else if self.ch_energy[1] > self.ch_energy[0] * 2.2 && self.ch_energy[1] > 0.0005 {
                for chunk in samples.chunks_exact(2) {
                    out.push(chunk[1]);
                }
            } else {
                for chunk in samples.chunks_exact(2) {
                    out.push((chunk[0] + chunk[1]) * 0.5);
                }
            }
            return out;
        }
        downmix_to_mono(samples, channels)
    }
}

/// Converts interleaved multi-channel PCM into mono `f32`.
pub fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(frames);
    let inv = 1.0 / channels as f32;
    for chunk in samples.chunks_exact(channels) {
        let sum: f32 = chunk.iter().copied().sum();
        out.push(sum * inv);
    }
    out
}

/// Anti-aliasing 3x decimator from 48 kHz -> 16 kHz (windowed-sinc FIR low-pass at ~7.0 kHz)
/// so Japanese high-frequency sibilants ('さ', 'し', 'き', 'た') NEVER alias or sound garbled!
pub struct Decimator48To16 {
    history: [f32; 15],
    phase: usize,
}

// Symmetric 15-tap Hann-windowed low-pass FIR (fc = 7.0 kHz @ 48 kHz, exact unity gain sum = 1.0000)
const LPF_TAPS: [f32; 15] = [
    -0.0022, -0.0069, 0.0000, 0.0268, 0.0742, 0.1313, 0.1784, 0.1968, 0.1784, 0.1313, 0.0742,
    0.0268, 0.0000, -0.0069, -0.0022,
];

impl Decimator48To16 {
    pub fn new() -> Self {
        Self {
            history: [0.0; 15],
            phase: 0,
        }
    }

    pub fn process(&mut self, input_48k: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(input_48k.len() / 3 + 1);
        for &s in input_48k {
            // Shift delay line
            self.history.copy_within(1..15, 0);
            self.history[14] = s;
            self.phase += 1;
            if self.phase >= 3 {
                self.phase = 0;
                let mut acc = 0.0f32;
                for (h, &t) in self.history.iter().zip(LPF_TAPS.iter()) {
                    acc += h * t;
                }
                out.push(acc);
            }
        }
        out
    }
}

/// Stateful 4-point Catmull-Rom cubic resampler from `in_rate` Hz mono `f32` to `out_rate` Hz mono `f32`.
/// Eliminates linear-interpolation high-frequency smearing on 44.1 kHz -> 48 kHz interfaces (e.g. Steinberg UR22mkII)
/// so Japanese plosives ('て', 'き', 'か') stay crisp and never turn into fricatives ('ちぇ', 'し')!
pub struct LinearResampler {
    in_rate: f64,
    out_rate: f64,
    pos: f64,
    hist: [f32; 3],
    has_hist: bool,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            in_rate: in_rate.max(1) as f64,
            out_rate: out_rate.max(1) as f64,
            pos: 0.0,
            hist: [0.0; 3],
            has_hist: false,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if (self.in_rate - self.out_rate).abs() < 1.0 {
            let n = input.len();
            self.hist[2] = input[n - 1];
            self.hist[1] = if n >= 2 { input[n - 2] } else { input[n - 1] };
            self.hist[0] = if n >= 3 { input[n - 3] } else { self.hist[1] };
            self.has_hist = true;
            return input.to_vec();
        }

        let step = self.in_rate / self.out_rate;
        let est_len = ((input.len() as f64) / step).ceil() as usize + 2;
        let mut out = Vec::with_capacity(est_len);

        let sample_at = |idx: isize, hist: &[f32; 3], has_hist: bool, input: &[f32]| -> f32 {
            if idx < 0 {
                if !has_hist {
                    input[0]
                } else {
                    match idx {
                        -1 => hist[2],
                        -2 => hist[1],
                        _ => hist[0],
                    }
                }
            } else if (idx as usize) < input.len() {
                input[idx as usize]
            } else {
                input[input.len() - 1]
            }
        };

        while self.pos < input.len() as f64 {
            let idx = self.pos.floor() as isize;
            let t = (self.pos - idx as f64) as f32;

            let y0 = sample_at(idx - 1, &self.hist, self.has_hist, input);
            let y1 = sample_at(idx, &self.hist, self.has_hist, input);
            let y2 = sample_at(idx + 1, &self.hist, self.has_hist, input);
            let y3 = sample_at(idx + 2, &self.hist, self.has_hist, input);

            // Catmull-Rom cubic spline (C1 continuous, crisp transient preservation!)
            let t2 = t * t;
            let t3 = t2 * t;
            let val = 0.5
                * ((2.0 * y1)
                    + (-y0 + y2) * t
                    + (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3) * t2
                    + (-y0 + 3.0 * y1 - 3.0 * y2 + y3) * t3);
            out.push(val.clamp(-1.0, 1.0));
            self.pos += step;
        }

        self.pos -= input.len() as f64;
        let n = input.len();
        if n >= 3 {
            self.hist = [input[n - 3], input[n - 2], input[n - 1]];
        } else if n == 2 {
            self.hist = [self.hist[2], input[0], input[1]];
        } else {
            self.hist = [self.hist[1], self.hist[2], input[0]];
        }
        self.has_hist = true;
        out
    }
}

enum ControlMsg {
    SetListening(bool),
    SetDevice(Option<String>),
    UpdateVad(VadConfig),
    UpdateNoiseCancel(NoiseCancelConfig),
    CalibrateNoise(u64),
    Flush,
    Shutdown,
}

/// Manages microphone capture + RNNoise denoising + VAD on a dedicated thread.
pub struct AudioController {
    tx: mpsc::Sender<ControlMsg>,
}

impl AudioController {
    pub fn spawn<F>(
        initial_device: Option<String>,
        initial_vad: VadConfig,
        initial_nc: NoiseCancelConfig,
        on_event: F,
    ) -> Self
    where
        F: Fn(AudioEvent) + Send + Sync + 'static,
    {
        let (tx, rx) = mpsc::channel::<ControlMsg>();
        let callback = Arc::new(on_event);

        thread::spawn(move || {
            let mut active = false;
            let mut device_name = initial_device;
            let vad_shared = Arc::new(Mutex::new(VadSegmenter::new(initial_vad)));
            let denoiser_shared = Arc::new(Mutex::new(AudioDenoiser::new(initial_nc)));
            let listening_flag = Arc::new(AtomicBool::new(false));
            let mut active_stream: Option<Stream> = None;

            while let Ok(msg) = rx.recv() {
                match msg {
                    ControlMsg::SetListening(enable) => {
                        if active == enable {
                            continue;
                        }
                        active = enable;
                        listening_flag.store(enable, Ordering::SeqCst);
                        if enable {
                            if let Ok(mut v) = vad_shared.lock() {
                                v.reset();
                            }
                            if let Ok(mut d) = denoiser_shared.lock() {
                                d.reset();
                            }
                            match start_stream(
                                device_name.as_deref(),
                                vad_shared.clone(),
                                denoiser_shared.clone(),
                                listening_flag.clone(),
                                callback.clone(),
                            ) {
                                Ok(s) => {
                                    drop(active_stream.replace(s));
                                }
                                Err(err) => {
                                    active = false;
                                    listening_flag.store(false, Ordering::SeqCst);
                                    callback(AudioEvent::Error(err));
                                }
                            }
                        } else {
                            if let Ok(mut v) = vad_shared.lock() {
                                if let Some(buf) = v.flush() {
                                    callback(AudioEvent::Utterance(buf));
                                }
                                v.reset();
                            }
                            drop(active_stream.take());
                            let rn_enabled = denoiser_shared
                                .lock()
                                .map(|d| d.is_rnnoise_enabled())
                                .unwrap_or(false);
                            callback(AudioEvent::Level {
                                rms: 0.0,
                                is_speaking: false,
                                vad_prob: 0.0,
                                rnnoise_active: rn_enabled,
                                noise_floor: 0.0,
                                effective_threshold: 0.0,
                            });
                        }
                    }
                    ControlMsg::SetDevice(new_dev) => {
                        if device_name == new_dev {
                            continue;
                        }
                        device_name = new_dev;
                        if active {
                            drop(active_stream.take());
                            if let Ok(mut v) = vad_shared.lock() {
                                v.reset();
                            }
                            if let Ok(mut d) = denoiser_shared.lock() {
                                d.reset();
                            }
                            match start_stream(
                                device_name.as_deref(),
                                vad_shared.clone(),
                                denoiser_shared.clone(),
                                listening_flag.clone(),
                                callback.clone(),
                            ) {
                                Ok(s) => drop(active_stream.replace(s)),
                                Err(err) => callback(AudioEvent::Error(err)),
                            }
                        }
                    }
                    ControlMsg::UpdateVad(cfg) => {
                        if let Ok(mut v) = vad_shared.lock() {
                            v.update_config(cfg);
                        }
                    }
                    ControlMsg::UpdateNoiseCancel(cfg) => {
                        if let Ok(mut d) = denoiser_shared.lock() {
                            d.update_config(cfg);
                        }
                    }
                    ControlMsg::CalibrateNoise(duration_ms) => {
                        if let Ok(mut v) = vad_shared.lock() {
                            v.start_calibration(duration_ms);
                        }
                    }
                    ControlMsg::Flush => {
                        if let Ok(mut v) = vad_shared.lock() {
                            if let Some(buf) = v.flush() {
                                callback(AudioEvent::Utterance(buf));
                            }
                        }
                    }
                    ControlMsg::Shutdown => {
                        listening_flag.store(false, Ordering::SeqCst);
                        drop(active_stream.take());
                        break;
                    }
                }
            }
        });

        Self { tx }
    }

    pub fn set_listening(&self, listening: bool) {
        let _ = self.tx.send(ControlMsg::SetListening(listening));
    }

    pub fn set_device(&self, device: Option<String>) {
        let _ = self.tx.send(ControlMsg::SetDevice(device));
    }

    pub fn update_vad(&self, cfg: VadConfig) {
        let _ = self.tx.send(ControlMsg::UpdateVad(cfg));
    }

    pub fn update_noise_cancel(&self, cfg: NoiseCancelConfig) {
        let _ = self.tx.send(ControlMsg::UpdateNoiseCancel(cfg));
    }

    pub fn calibrate_noise(&self, duration_ms: u64) {
        let _ = self.tx.send(ControlMsg::CalibrateNoise(duration_ms));
    }

    pub fn flush(&self) {
        let _ = self.tx.send(ControlMsg::Flush);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(ControlMsg::Shutdown);
    }
}

fn find_input_device(preferred_name: Option<&str>) -> Result<Device, String> {
    let host = cpal::default_host();
    if let Some(name) = preferred_name {
        if !name.trim().is_empty() {
            if let Ok(mut devices) = host.input_devices() {
                if let Some(found) = devices.find(|d| d.name().ok().as_deref() == Some(name)) {
                    return Ok(found);
                }
            }
        }
    }
    host.default_input_device()
        .ok_or_else(|| "利用可能なマイク入力デバイスが見つかりません".to_string())
}

fn start_stream<F>(
    preferred_device: Option<&str>,
    vad_shared: Arc<Mutex<VadSegmenter>>,
    denoiser_shared: Arc<Mutex<AudioDenoiser>>,
    listening_flag: Arc<AtomicBool>,
    callback: Arc<F>,
) -> Result<Stream, String>
where
    F: Fn(AudioEvent) + Send + Sync + 'static,
{
    let device = find_input_device(preferred_device)?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("マイク設定の取得に失敗しました: {}", e))?;

    let sample_format = supported.sample_format();
    let stream_config: StreamConfig = supported.into();
    let channels = stream_config.channels as usize;
    let in_rate = stream_config.sample_rate.0;

    // Pipeline: Mic (in_rate, stereo->smart active channel) -> 48kHz -> RNNoise -> Anti-aliased 16kHz FIR -> Hybrid VAD -> STT
    let downmixer = Arc::new(Mutex::new(SmartDownmixer::new()));
    let to_48k = Arc::new(Mutex::new(LinearResampler::new(in_rate, RNNOISE_SAMPLE_RATE)));
    let to_16k = Arc::new(Mutex::new(Decimator48To16::new()));
    let last_meter = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(50)));
    let tts_echo_muted = Arc::new(AtomicBool::new(false));

    let process_f32 = {
        let vad_shared = vad_shared.clone();
        let denoiser_shared = denoiser_shared.clone();
        let listening_flag = listening_flag.clone();
        let callback = callback.clone();
        let downmixer = downmixer.clone();
        let to_48k = to_48k.clone();
        let to_16k = to_16k.clone();
        let last_meter = last_meter.clone();
        let tts_echo_muted = tts_echo_muted.clone();
        move |interleaved: &[f32]| {
            if !listening_flag.load(Ordering::Relaxed) {
                return;
            }
            // Echo-prevention guard: while VOICEVOX TTS is speaking, pause VAD/STT input
            // so speaker output is never picked up by the mic and sent back to Discord.
            if crate::tts::is_tts_playing() {
                if !tts_echo_muted.swap(true, Ordering::SeqCst) {
                    if let Ok(mut v) = vad_shared.lock() {
                        v.reset();
                    }
                    callback(AudioEvent::SpeechCancelled);
                }
                if let Ok(mut last) = last_meter.lock() {
                    if last.elapsed() >= Duration::from_millis(66) {
                        *last = Instant::now();
                        callback(AudioEvent::Level {
                            rms: 0.0,
                            is_speaking: false,
                            vad_prob: 0.0,
                            rnnoise_active: false,
                            noise_floor: 0.0,
                            effective_threshold: 0.0,
                        });
                    }
                }
                return;
            } else if tts_echo_muted.swap(false, Ordering::SeqCst) {
                if let Ok(mut v) = vad_shared.lock() {
                    v.reset();
                }
            }
            let mono = match downmixer.lock() {
                Ok(mut dm) => dm.process(interleaved, channels),
                Err(_) => return,
            };
            let pcm_48k = match to_48k.lock() {
                Ok(mut r) => r.process(&mono),
                Err(_) => return,
            };
            if pcm_48k.is_empty() {
                return;
            }

            let (cleaned_48k, vad_prob, rnnoise_active) = match denoiser_shared.lock() {
                Ok(mut d) => d.process(&pcm_48k),
                Err(_) => return,
            };
            if cleaned_48k.is_empty() {
                return;
            }

            let resampled_16k = match to_16k.lock() {
                Ok(mut r) => r.process(&cleaned_48k),
                Err(_) => return,
            };
            if resampled_16k.is_empty() {
                return;
            }

            let (rms, is_speaking, events, noise_floor, effective_threshold) = match vad_shared.lock()
            {
                Ok(mut v) => {
                    let (r, sp, evs) = v.push_frame_with_prob(&resampled_16k, vad_prob, rnnoise_active);
                    (r, sp, evs, v.noise_floor(), v.effective_start_threshold())
                }
                Err(_) => return,
            };

            // Throttle UI meter updates to ~30 fps (every 33ms)
            if let Ok(mut last) = last_meter.lock() {
                if last.elapsed() >= Duration::from_millis(33) {
                    *last = Instant::now();
                    callback(AudioEvent::Level {
                        rms,
                        is_speaking,
                        vad_prob,
                        rnnoise_active,
                        noise_floor,
                        effective_threshold,
                    });
                }
            }

            for ev in events {
                match ev {
                    VadEvent::SpeechStart => callback(AudioEvent::SpeechStart),
                    VadEvent::SpeechCancelled => callback(AudioEvent::SpeechCancelled),
                    VadEvent::SpeechEnd(samples) => callback(AudioEvent::Utterance(samples)),
                    VadEvent::CalibrationDone {
                        noise_floor,
                        threshold,
                    } => callback(AudioEvent::CalibrationDone {
                        noise_floor,
                        threshold,
                    }),
                }
            }
        }
    };

    let err_cb = {
        let callback = callback.clone();
        move |err: cpal::StreamError| {
            callback(AudioEvent::Error(format!("音声ストリームエラー: {}", err)));
        }
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| process_f32(data),
            err_cb,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                let converted: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                process_f32(&converted);
            },
            err_cb,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                let converted: Vec<f32> =
                    data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                process_f32(&converted);
            },
            err_cb,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _| {
                let converted: Vec<f32> = data.iter().map(|&s| s as f32 / 2147483648.0).collect();
                process_f32(&converted);
            },
            err_cb,
            None,
        ),
        other => {
            return Err(format!("未対応の音声サンプル形式です: {:?}", other));
        }
    }
    .map_err(|e| format!("マイクストリームの構築に失敗しました: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("マイクストリームの開始に失敗しました: {}", e))?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_downmixer_preserves_full_gain_on_single_channel_usb_interface() {
        let mut dm = SmartDownmixer::new();
        // Simulate Steinberg UR22mkII: Left channel (Input 1) = mic 0.4, Right channel (Input 2) = silent 0.0
        let mut stereo = Vec::new();
        for _ in 0..480 {
            stereo.push(0.4);
            stereo.push(0.0);
        }
        let mut mono = Vec::new();
        for _ in 0..5 {
            mono = dm.process(&stereo, 2);
        }
        assert_eq!(mono.len(), 480);
        assert!(
            (mono[0] - 0.4).abs() < 1e-5,
            "expected full 100% mic gain (0.4), got {}",
            mono[0]
        );
    }

    #[test]
    fn downmix_stereo_averages_channels() {
        let stereo = vec![0.2, 0.6, -0.4, 0.4];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn resampler_48k_to_16k_preserves_length_ratio() {
        let mut r = LinearResampler::new(48_000, 16_000);
        let input = vec![0.5f32; 4800];
        let out = r.process(&input);
        assert_eq!(out.len(), 1600);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1599] - 0.5).abs() < 1e-6);
    }
}
