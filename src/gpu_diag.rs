//! Cross-vendor GPU (AMD / NVIDIA / Intel Arc / iGPU) hardware diagnostics,
//! DXGI VRAM watchdog (#16), battery/eco detection (#30), and first-run wizard presets (#29).
//! Uses official Win32 DXGI & Kernel32 APIs only — zero CUDA runtime dependencies.

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;
use windows::core::{w, GUID, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub gpu_name: String,
    pub gpu_vendor: String, // "AMD" | "NVIDIA" | "Intel" | "CPU"
    pub vram_gb: f32,
    pub vram_used_gb: Option<f32>,
    pub vram_usage_pct: Option<u8>,
    pub is_on_battery: bool,
    pub battery_pct: Option<u8>,
    pub cpu_cores: u32,
    pub vulkan_available: bool,
    pub directml_available: bool,
    pub recommended_stt_engine: String,
    pub recommended_stt_model: String,
    pub recommended_vad_noise: String,
    pub recommended_stt_noise: String,
    pub recommended_llm_class: String,
    pub summary_note: String,
}

#[repr(C)]
struct Luid {
    low_part: u32,
    high_part: i32,
}

#[repr(C)]
struct DxgiAdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    sub_sys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    adapter_luid: Luid,
    flags: u32,
}

#[repr(C)]
struct DxgiQueryVideoMemoryInfo {
    budget: u64,
    current_usage: u64,
    available_for_reservation: u64,
    current_reservation: u64,
}

#[repr(C)]
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ComObj {
    vtbl: *const *const c_void,
}

const IID_IDXGIFACTORY1: GUID = GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);
const IID_IDXGIADAPTER3: GUID = GUID::from_u128(0x645967a4_1392_4310_a798_8053ce3e93fd);

type FnCreateDxgiFactory1 =
    unsafe extern "system" fn(riid: *const GUID, ppfactory: *mut *mut c_void) -> i32;
type FnEnumAdapters1 =
    unsafe extern "system" fn(this: *mut c_void, adapter: u32, ppadapter: *mut *mut c_void) -> i32;
type FnGetDesc1 =
    unsafe extern "system" fn(this: *mut c_void, pdesc: *mut DxgiAdapterDesc1) -> i32;
type FnQueryInterface = unsafe extern "system" fn(
    this: *mut c_void,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> i32;
type FnRelease = unsafe extern "system" fn(this: *mut c_void) -> u32;
type FnQueryVideoMemoryInfo = unsafe extern "system" fn(
    this: *mut c_void,
    node_index: u32,
    memory_segment_group: u32,
    p_video_memory_info: *mut DxgiQueryVideoMemoryInfo,
) -> i32;
type FnGetSystemPowerStatus = unsafe extern "system" fn(*mut SystemPowerStatus) -> i32;

fn vendor_from_id(id: u32, desc: &str) -> String {
    let lower = desc.to_ascii_lowercase();
    match id {
        0x1002 | 0x1022 => "AMD".into(),
        0x10DE => "NVIDIA".into(),
        0x8086 => "Intel".into(),
        _ if lower.contains("radeon") || lower.contains("amd") => "AMD".into(),
        _ if lower.contains("geforce") || lower.contains("nvidia") || lower.contains("rtx") => {
            "NVIDIA".into()
        }
        _ if lower.contains("intel") || lower.contains("arc") => "Intel".into(),
        _ => "GPU".into(),
    }
}

/// Queries active battery/AC power status via Kernel32 `GetSystemPowerStatus`.
pub fn query_power_status() -> (bool, Option<u8>) {
    unsafe {
        let Ok(k32) = LoadLibraryW(w!("kernel32.dll")) else {
            return (false, None);
        };
        let Some(sym) = GetProcAddress(k32, PCSTR(b"GetSystemPowerStatus\0".as_ptr())) else {
            return (false, None);
        };
        let f: FnGetSystemPowerStatus = std::mem::transmute(sym);
        let mut st = SystemPowerStatus {
            ac_line_status: 255,
            battery_flag: 255,
            battery_life_percent: 255,
            system_status_flag: 0,
            battery_life_time: 0,
            battery_full_life_time: 0,
        };
        if f(&mut st) != 0 {
            let on_battery = st.ac_line_status == 0 && (st.battery_flag & 128) == 0;
            let pct = if st.battery_life_percent <= 100 {
                Some(st.battery_life_percent)
            } else {
                None
            };
            return (on_battery, pct);
        }
    }
    (false, None)
}

#[derive(Debug, Clone)]
pub struct DxgiAdapterSummary {
    pub name: String,
    pub vendor: String,
    pub vram_gb: f32,
    pub vram_used_gb: Option<f32>,
    pub vram_usage_pct: Option<u8>,
}

/// Enumerates hardware DXGI adapters without needing D3D12/CUDA toolkits.
pub fn query_best_dxgi_adapter() -> Option<DxgiAdapterSummary> {
    unsafe {
        let Ok(hmod): Result<HMODULE, _> = LoadLibraryW(w!("dxgi.dll")) else {
            return None;
        };
        let Some(create_sym) = GetProcAddress(hmod, PCSTR(b"CreateDXGIFactory1\0".as_ptr())) else {
            return None;
        };
        let create_factory: FnCreateDxgiFactory1 = std::mem::transmute(create_sym);
        let mut factory_ptr: *mut c_void = std::ptr::null_mut();
        if create_factory(&IID_IDXGIFACTORY1, &mut factory_ptr) < 0 || factory_ptr.is_null() {
            return None;
        }

        let f_vtbl = *(factory_ptr as *mut ComObj);
        let f_release: FnRelease = std::mem::transmute(*f_vtbl.vtbl.add(2));
        let enum_adapters1: FnEnumAdapters1 = std::mem::transmute(*f_vtbl.vtbl.add(12));

        let mut best: Option<(usize, DxgiAdapterSummary)> = None;
        for idx in 0..8u32 {
            let mut adapter_ptr: *mut c_void = std::ptr::null_mut();
            if enum_adapters1(factory_ptr, idx, &mut adapter_ptr) < 0 || adapter_ptr.is_null() {
                break;
            }
            let a_vtbl = *(adapter_ptr as *mut ComObj);
            let a_qi: FnQueryInterface = std::mem::transmute(*a_vtbl.vtbl.add(0));
            let a_release: FnRelease = std::mem::transmute(*a_vtbl.vtbl.add(2));
            let get_desc1: FnGetDesc1 = std::mem::transmute(*a_vtbl.vtbl.add(10));

            let mut desc: DxgiAdapterDesc1 = std::mem::zeroed();
            if get_desc1(adapter_ptr, &mut desc) >= 0 {
                // Skip Microsoft Basic Render Driver (Vendor 0x1414) or software flag (bit 1)
                let is_software = desc.vendor_id == 0x1414 || (desc.flags & 2) != 0;
                if !is_software {
                    let end = desc
                        .description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.description.len());
                    let name = String::from_utf16_lossy(&desc.description[..end])
                        .trim()
                        .to_string();
                    let vram_bytes = if desc.dedicated_video_memory > 0 {
                        desc.dedicated_video_memory
                    } else {
                        desc.shared_system_memory / 2
                    };
                    let vram_gb = (vram_bytes as f64 / (1024.0 * 1024.0 * 1024.0)) as f32;

                    let mut used_gb = None;
                    let mut usage_pct = None;
                    let mut a3_ptr: *mut c_void = std::ptr::null_mut();
                    if a_qi(adapter_ptr, &IID_IDXGIADAPTER3, &mut a3_ptr) >= 0 && !a3_ptr.is_null() {
                        let a3_vtbl = *(a3_ptr as *mut ComObj);
                        let a3_rel: FnRelease = std::mem::transmute(*a3_vtbl.vtbl.add(2));
                        let qvmi: FnQueryVideoMemoryInfo =
                            std::mem::transmute(*a3_vtbl.vtbl.add(14));
                        let mut info = DxgiQueryVideoMemoryInfo {
                            budget: 0,
                            current_usage: 0,
                            available_for_reservation: 0,
                            current_reservation: 0,
                        };
                        if qvmi(adapter_ptr, 0, 0, &mut info) >= 0 && info.budget > 0 {
                            used_gb =
                                Some((info.current_usage as f64 / (1024.0 * 1024.0 * 1024.0)) as f32);
                            let pct = ((info.current_usage as f64 / info.budget as f64) * 100.0)
                                .round()
                                .clamp(0.0, 100.0) as u8;
                            usage_pct = Some(pct);
                        }
                        a3_rel(a3_ptr);
                    }

                    let vendor = vendor_from_id(desc.vendor_id, &name);
                    let summary = DxgiAdapterSummary {
                        name,
                        vendor,
                        vram_gb,
                        vram_used_gb: used_gb,
                        vram_usage_pct: usage_pct,
                    };
                    let rank = desc.dedicated_video_memory;
                    if best.as_ref().map(|(r, _)| rank > *r).unwrap_or(true) {
                        best = Some((rank, summary));
                    }
                }
            }
            a_release(adapter_ptr);
        }
        f_release(factory_ptr);
        best.map(|(_, s)| s)
    }
}

/// Inspects live GPU, Vulkan/DirectML runtime availability, CPU cores, and battery status,
/// then picks the best vendor-agnostic recommendation (#29).
pub fn detect_hardware_profile(runtime_dir: Option<&Path>) -> HardwareProfile {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
    let (is_on_battery, battery_pct) = query_power_status();
    let dxgi = query_best_dxgi_adapter();

    let vulkan_dll = runtime_dir
        .map(|d| d.join("bin").join("ggml-vulkan-whisper.dll").is_file() || d.join("ggml-vulkan-whisper.dll").is_file())
        .unwrap_or_else(|| Path::new("bin/ggml-vulkan-whisper.dll").is_file());
    let directml_dll = runtime_dir
        .map(|d| d.join("bin").join("sherpa-onnx-c-api.dll").is_file() || d.join("sherpa-onnx-c-api.dll").is_file())
        .unwrap_or_else(|| Path::new("bin/sherpa-onnx-c-api.dll").is_file());

    let (gpu_name, gpu_vendor, vram_gb, vram_used_gb, vram_usage_pct) = match dxgi {
        Some(a) => (
            a.name,
            a.vendor,
            (a.vram_gb * 10.0).round() / 10.0,
            a.vram_used_gb,
            a.vram_usage_pct,
        ),
        None => ("CPU 汎用環境".into(), "CPU".into(), 0.0, None, None),
    };

    let dpdfnet_present = Path::new("models/dpdfnet2.onnx").is_file()
        || runtime_dir
            .map(|d| d.join("models").join("dpdfnet2.onnx").is_file())
            .unwrap_or(false);

    let (rec_stt_engine, rec_stt_model, rec_vad_noise, rec_stt_noise, rec_llm, note) =
        if is_on_battery || (gpu_vendor == "CPU" || vram_gb < 2.5) {
            (
                "whisper-cpp".into(),
                "whisper-large-v3-turbo-q8".into(),
                if dpdfnet_present { "gtcrn-16k" } else { "rnnoise" }.into(),
                "off-hpf".into(),
                "none".into(),
                format!(
                    "{} ({:.1} GB VRAM / {}スレッド): 省電力・軽量バランス構成（CUDA不要・CPU/iGPU対応）",
                    gpu_name, vram_gb, cpu_cores
                ),
            )
        } else if vram_gb >= 8.0 {
            (
                "whisper-cpp".into(),
                "whisper-large-v3-q8".into(),
                "rnnoise".into(),
                if dpdfnet_present { "dpdfnet-16k" } else { "rnnoise" }.into(),
                "qwen3-4b-llm".into(),
                format!(
                    "{} ({:.1} GB VRAM / {}): 潤沢なVRAMにより最高精度 Large-v3 + DPDFNet(12dB) + 2エンジン合議/LLM補正まで快適動作",
                    gpu_name, vram_gb, if vulkan_dll { "Vulkan対応" } else { "CPU" }
                ),
            )
        } else if vram_gb >= 4.0 {
            (
                "whisper-cpp".into(),
                "whisper-large-v3-turbo-q8".into(),
                "rnnoise".into(),
                if dpdfnet_present { "dpdfnet-16k" } else { "rnnoise" }.into(),
                "qwen3-1.7b-llm".into(),
                format!(
                    "{} ({:.1} GB VRAM): 標準 Turbo Q8 + DPDFNet による低遅延高精度構成（Vulkan/DirectML対応）",
                    gpu_name, vram_gb
                ),
            )
        } else {
            (
                "sherpa-qwen3".into(),
                "qwen3-asr-0.6b-int8".into(),
                "rnnoise".into(),
                "gtcrn-16k".into(),
                "none".into(),
                format!(
                    "{} ({:.1} GB VRAM): 軽量 DirectML / CPU 最適化構成",
                    gpu_name, vram_gb
                ),
            )
        };

    let _ = size_of::<DxgiAdapterDesc1>();
    HardwareProfile {
        gpu_name,
        gpu_vendor,
        vram_gb,
        vram_used_gb,
        vram_usage_pct,
        is_on_battery,
        battery_pct,
        cpu_cores,
        vulkan_available: vulkan_dll && vram_gb >= 1.0,
        directml_available: directml_dll && vram_gb >= 1.0,
        recommended_stt_engine: rec_stt_engine,
        recommended_stt_model: rec_stt_model,
        recommended_vad_noise: rec_vad_noise,
        recommended_stt_noise: rec_stt_noise,
        recommended_llm_class: rec_llm,
        summary_note: note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_profile_detects_cleanly_without_cuda() {
        let prof = detect_hardware_profile(None);
        assert!(prof.cpu_cores >= 1);
        assert!(!prof.gpu_name.is_empty());
        assert!(!prof.recommended_stt_engine.is_empty());
        assert!(!prof.summary_note.is_empty());
    }
}
