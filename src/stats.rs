//! Local Recognition Telemetry (#27), Benchmark Regression Suite (#17),
//! Auto Parameter Tuner (#38), Settings Backup/Restore (#19), and Startup Update Check (#33).

use crate::config::AppConfig;
use crate::processor::phonetic::phonetic_similarity;
use crate::stt::{SttEngine, TranscribeContext};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
};

pub const CURRENT_APP_TAG: &str = "v1.8.0";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DailyStat {
    pub utterances: u64,
    pub cancels: u64,
    pub total_infer_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineStat {
    pub device: String,
    pub utterances: u64,
    pub cancels: u64,
    pub total_infer_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsStore {
    pub total_utterances: u64,
    pub total_cancels: u64,
    pub by_date: BTreeMap<String, DailyStat>,
    pub by_engine: BTreeMap<String, EngineStat>,
    pub misrecognition_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummaryDto {
    pub total_utterances: u64,
    pub total_cancels: u64,
    pub cancel_rate_pct: f32,
    pub avg_infer_ms: u64,
    pub daily: Vec<(String, DailyStat)>,
    pub engines: Vec<(String, EngineStat)>,
    pub top_misrecognitions: Vec<(String, u64)>,
}

impl StatsStore {
    pub fn default_path() -> PathBuf {
        AppConfig::app_data_dir().join("stats.json")
    }

    pub fn load_from(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(p) = path.parent() {
            let _ = fs::create_dir_all(p);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }

    pub fn record_utterance(&mut self, date: &str, engine: &str, device: &str, infer_ms: u64) {
        self.total_utterances += 1;
        let d = self.by_date.entry(date.to_string()).or_default();
        d.utterances += 1;
        d.total_infer_ms += infer_ms;
        // Keep last 30 days bounded
        while self.by_date.len() > 30 {
            if let Some(first_k) = self.by_date.keys().next().cloned() {
                self.by_date.remove(&first_k);
            }
        }
        let e = self.by_engine.entry(engine.to_string()).or_default();
        e.device = device.to_string();
        e.utterances += 1;
        e.total_infer_ms += infer_ms;
    }

    pub fn record_cancel(&mut self, date: &str, engine: &str) {
        self.total_cancels += 1;
        self.by_date.entry(date.to_string()).or_default().cancels += 1;
        self.by_engine.entry(engine.to_string()).or_default().cancels += 1;
    }

    pub fn record_learning(&mut self, from: &str, to: &str) {
        let key = format!("{} → {}", from.trim(), to.trim());
        *self.misrecognition_counts.entry(key).or_insert(0) += 1;
    }

    pub fn summary(&self) -> StatsSummaryDto {
        let cancel_rate_pct = if self.total_utterances > 0 {
            (self.total_cancels as f32 / self.total_utterances as f32) * 100.0
        } else {
            0.0
        };
        let total_ms: u64 = self.by_engine.values().map(|e| e.total_infer_ms).sum();
        let avg_infer_ms = if self.total_utterances > 0 {
            total_ms / self.total_utterances
        } else {
            0
        };
        let mut top: Vec<(String, u64)> = self
            .misrecognition_counts
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        top.truncate(10);

        StatsSummaryDto {
            total_utterances: self.total_utterances,
            total_cancels: self.total_cancels,
            cancel_rate_pct,
            avg_infer_ms,
            daily: self
                .by_date
                .iter()
                .rev()
                .take(14)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            engines: self
                .by_engine
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            top_misrecognitions: top,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCase {
    pub id: String,
    pub label: String,
    pub expected_text: String,
    pub wav_path: String,
    pub duration_sec: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCaseResult {
    pub id: String,
    pub label: String,
    pub expected_text: String,
    pub actual_text: String,
    pub similarity: f32,
    pub infer_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreeStageBenchCaseResult {
    pub id: String,
    pub label: String,
    pub expected_text: String,
    pub raw_text: String,
    pub raw_cer: f32,
    pub raw_ms: u64,
    pub non_llm_text: String,
    pub non_llm_cer: f32,
    pub non_llm_ms: u64,
    pub non_llm_delta: String,
    pub llm_text: String,
    pub llm_cer: f32,
    pub llm_ms: u64,
    pub llm_delta: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreeStageBenchmarkReport {
    pub cases: Vec<ThreeStageBenchCaseResult>,
    pub avg_raw_cer: f32,
    pub avg_non_llm_cer: f32,
    pub avg_llm_cer: f32,
    pub raw_exact_pct: f32,
    pub non_llm_exact_pct: f32,
    pub llm_exact_pct: f32,
    pub avg_raw_ms: u64,
    pub avg_non_llm_ms: u64,
    pub avg_llm_ms: u64,
    pub non_llm_improved: usize,
    pub non_llm_unchanged: usize,
    pub non_llm_regressed: usize,
    pub llm_improved: usize,
    pub llm_unchanged: usize,
    pub llm_regressed: usize,
    pub llm_available: bool,
    pub advice: String,
}

/// Computes Japanese Character Error Rate (CER %) ignoring whitespace differences (#6).
pub fn char_error_rate(hypothesis: &str, reference: &str) -> f32 {
    let h: String = hypothesis.chars().filter(|c| !c.is_whitespace()).collect();
    let r: String = reference.chars().filter(|c| !c.is_whitespace()).collect();
    let r_len = r.chars().count();
    if r_len == 0 {
        return if h.is_empty() { 0.0 } else { 100.0 };
    }
    let dist = crate::processor::phonetic::levenshtein_chars(&h, &r);
    (dist as f32 / r_len as f32) * 100.0
}

pub fn stage_delta_label(base_cer: f32, next_cer: f32) -> String {
    if next_cer + 0.01 < base_cer {
        "improved".into()
    } else if next_cer > base_cer + 0.01 {
        "regressed".into()
    } else {
        "unchanged".into()
    }
}

pub fn compute_three_stage_report(
    cases: Vec<ThreeStageBenchCaseResult>,
    llm_available: bool,
) -> ThreeStageBenchmarkReport {
    let n = cases.len().max(1) as f32;
    let avg_raw_cer = cases.iter().map(|c| c.raw_cer).sum::<f32>() / n;
    let avg_non_llm_cer = cases.iter().map(|c| c.non_llm_cer).sum::<f32>() / n;
    let avg_llm_cer = cases.iter().map(|c| c.llm_cer).sum::<f32>() / n;

    let exact_cnt = |f: fn(&ThreeStageBenchCaseResult) -> f32| -> f32 {
        (cases.iter().filter(|c| f(c) < 0.01).count() as f32 / n) * 100.0
    };
    let raw_exact_pct = if cases.is_empty() { 0.0 } else { exact_cnt(|c| c.raw_cer) };
    let non_llm_exact_pct = if cases.is_empty() { 0.0 } else { exact_cnt(|c| c.non_llm_cer) };
    let llm_exact_pct = if cases.is_empty() { 0.0 } else { exact_cnt(|c| c.llm_cer) };

    let avg_raw_ms = (cases.iter().map(|c| c.raw_ms).sum::<u64>() as f32 / n).round() as u64;
    let avg_non_llm_ms = (cases.iter().map(|c| c.non_llm_ms).sum::<u64>() as f32 / n).round() as u64;
    let avg_llm_ms = (cases.iter().map(|c| c.llm_ms).sum::<u64>() as f32 / n).round() as u64;

    let non_llm_improved = cases.iter().filter(|c| c.non_llm_delta == "improved").count();
    let non_llm_unchanged = cases.iter().filter(|c| c.non_llm_delta == "unchanged").count();
    let non_llm_regressed = cases.iter().filter(|c| c.non_llm_delta == "regressed").count();

    let llm_improved = cases.iter().filter(|c| c.llm_delta == "improved").count();
    let llm_unchanged = cases.iter().filter(|c| c.llm_delta == "unchanged").count();
    let llm_regressed = cases.iter().filter(|c| c.llm_delta == "regressed").count();

    let advice = if cases.is_empty() {
        "保存されたベンチマークWAVがありません。音声診断タブから「ベンチ保存」を行ってください。".into()
    } else if !llm_available {
        format!(
            "非LLM補正のみで CER が {:.1}% → {:.1}%（改善 {}件 / 改悪 {}件, 追加遅延 +{}ms）。ローカルLLMサーバー未起動のため Stage 3 はバイパスされました。",
            avg_raw_cer,
            avg_non_llm_cer,
            non_llm_improved,
            non_llm_regressed,
            avg_non_llm_ms.saturating_sub(avg_raw_ms)
        )
    } else if avg_llm_cer + 0.3 < avg_non_llm_cer {
        format!(
            "非LLM補正で CER {:.1}% → {:.1}% に改善。ローカルLLMはさらに {:.1}% まで改善しました（改善 {}件 / 改悪 {}件, LLM追加遅延 +{}ms）。",
            avg_raw_cer,
            avg_non_llm_cer,
            avg_llm_cer,
            llm_improved,
            llm_regressed,
            avg_llm_ms.saturating_sub(avg_non_llm_ms)
        )
    } else if avg_llm_cer > avg_non_llm_cer + 0.5 {
        format!(
            "非LLM補正のみで CER {:.1}% → {:.1}% に改善！現在の設定ではローカルLLMが過補正傾向（CER {:.1}% / 改悪 {}件）のため、オフまたは辞書強化を推奨します。",
            avg_raw_cer, avg_non_llm_cer, avg_llm_cer, llm_regressed
        )
    } else {
        format!(
            "非LLM補正のみで CER {:.1}% → {:.1}% まで十分改善（ローカルLLM時 {:.1}%, +{}ms）。通常は非LLM運用で超低遅延と高精度を両立できます。",
            avg_raw_cer,
            avg_non_llm_cer,
            avg_llm_cer,
            avg_llm_ms.saturating_sub(avg_non_llm_ms)
        )
    };

    ThreeStageBenchmarkReport {
        cases,
        avg_raw_cer: if avg_raw_cer.is_nan() { 0.0 } else { avg_raw_cer },
        avg_non_llm_cer: if avg_non_llm_cer.is_nan() { 0.0 } else { avg_non_llm_cer },
        avg_llm_cer: if avg_llm_cer.is_nan() { 0.0 } else { avg_llm_cer },
        raw_exact_pct,
        non_llm_exact_pct,
        llm_exact_pct,
        avg_raw_ms,
        avg_non_llm_ms,
        avg_llm_ms,
        non_llm_improved,
        non_llm_unchanged,
        non_llm_regressed,
        llm_improved,
        llm_unchanged,
        llm_regressed,
        llm_available,
        advice,
    }
}

pub fn bench_dir() -> PathBuf {
    AppConfig::app_data_dir().join("bench_wavs")
}

pub fn load_benchmark_cases(dir: &Path) -> Vec<BenchmarkCase> {
    let idx = dir.join("cases.json");
    fs::read_to_string(idx)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_benchmark_cases(dir: &Path, cases: &[BenchmarkCase]) {
    let _ = fs::create_dir_all(dir);
    if let Ok(json) = serde_json::to_string_pretty(cases) {
        let _ = fs::write(dir.join("cases.json"), json);
    }
}

pub fn write_f32_wav_16k(path: &Path, samples_16k: &[f32]) -> Result<(), String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| format!("ディレクトリ作成失敗: {}", e))?;
    }
    let bytes = crate::stt::local::encode_wav_16k_mono(samples_16k);
    fs::write(path, bytes).map_err(|e| format!("WAV 保存失敗: {}", e))
}

pub fn read_f32_wav_16k(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = fs::read(path).map_err(|e| format!("WAV 読み込み失敗: {}", e))?;
    let mut pos = 12usize;
    while pos + 8 < bytes.len() {
        let id = &bytes[pos..pos + 4];
        let sz = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap_or([0; 4])) as usize;
        if id == b"data" {
            pos += 8;
            break;
        }
        pos += 8 + sz;
    }
    if pos >= bytes.len() {
        return Err("WAV data チャンクが見つかりません".into());
    }
    Ok(bytes[pos..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoTunerProposal {
    pub best_eq_strength: f32,
    pub best_room_debox_strength: f32,
    pub best_wsola_rate: f32,
    pub best_beam_size: i32,
    pub best_dpdfnet_db: f32,
    pub avg_similarity: f32,
    pub tested_cases: usize,
}

/// Grid-searches optimal `(eq_strength, room_debox_strength, wsola_rate, beam_size, dpdfnet_db)` against saved hard WAV cases (#9 + #38).
pub fn run_auto_parameter_tuner(
    cases: &[BenchmarkCase],
    engine: &dyn SttEngine,
    base_ctx: &TranscribeContext,
    denoiser_16k: Option<&crate::stt::sherpa_ffi::ResidentSherpaDenoiser>,
) -> Result<AutoTunerProposal, String> {
    if cases.is_empty() {
        return Err("ベンチマーク用に保存された発話WAVがありません（音声診断タブから保存してください）".into());
    }
    let mut loaded: Vec<(&BenchmarkCase, Vec<f32>)> = Vec::new();
    for c in cases.iter().take(6) {
        if let Ok(samples) = read_f32_wav_16k(Path::new(&c.wav_path)) {
            if !samples.is_empty() {
                loaded.push((c, samples));
            }
        }
    }
    if loaded.is_empty() {
        return Err("有効なベンチマークWAVファイルを読み込めませんでした".into());
    }

    let candidates: &[(f32, f32, f32, i32, f32)] = &[
        (0.35, 0.20, 1.00, 3, 12.0),
        (0.55, 0.40, 0.90, 5, 12.0),
        (0.75, 0.60, 0.85, 5, 16.0),
        (0.20, 0.00, 0.85, 5, 18.0),
    ];

    let mut best: Option<AutoTunerProposal> = None;
    for &(eq, debox, wsola_r, beam, dpdf_db) in candidates {
        let mut sum_sim = 0.0f32;
        let mut ctx = base_ctx.clone();
        ctx.eq_strength = eq;
        ctx.room_debox_strength = debox;
        ctx.beam_size = beam;
        for (c, samples) in &loaded {
            let base_in = if dpdf_db <= 14.0 {
                if let Some(den) = denoiser_16k {
                    den.denoise(samples, 16_000).unwrap_or_else(|_| samples.clone())
                } else {
                    samples.clone()
                }
            } else {
                samples.clone()
            };
            let input = if (wsola_r - 1.0).abs() > 0.02 {
                crate::audio::enhance::wsola_time_stretch(&base_in, wsola_r)
            } else {
                base_in
            };
            let actual = engine.transcribe_with_context(&input, &ctx).unwrap_or_default();
            sum_sim += phonetic_similarity(&actual, &c.expected_text);
        }
        let avg = sum_sim / loaded.len() as f32;
        if best.as_ref().map(|b| avg > b.avg_similarity).unwrap_or(true) {
            best = Some(AutoTunerProposal {
                best_eq_strength: eq,
                best_room_debox_strength: debox,
                best_wsola_rate: wsola_r,
                best_beam_size: beam,
                best_dpdfnet_db: dpdf_db,
                avg_similarity: avg,
                tested_cases: loaded.len(),
            });
        }
    }

    best.ok_or_else(|| "パラメータ探索に失敗しました".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupBundle {
    pub voxdrop_version: String,
    pub config: AppConfig,
    pub stats: StatsStore,
}

pub fn export_backup_json(config: &AppConfig, stats: &StatsStore) -> Result<String, String> {
    let bundle = BackupBundle {
        voxdrop_version: CURRENT_APP_TAG.into(),
        config: config.clone(),
        stats: stats.clone(),
    };
    serde_json::to_string_pretty(&bundle)
        .map_err(|e| format!("バックアップJSON生成失敗: {}", e))
}

pub fn import_backup_json(raw: &str) -> Result<(AppConfig, Option<StatsStore>), String> {
    if let Ok(bundle) = serde_json::from_str::<BackupBundle>(raw) {
        let mut cfg = bundle.config;
        cfg.migrate();
        return Ok((cfg, Some(bundle.stats)));
    }
    let mut cfg: AppConfig = serde_json::from_str(raw)
        .map_err(|e| format!("バックアップ形式の解析に失敗しました: {}", e))?;
    cfg.migrate();
    Ok((cfg, None))
}

/// Checks GitHub Releases once at startup via official HTTPS GET (non-intrusive notification only, #33).
pub fn check_latest_release_non_blocking() -> Option<(String, String)> {
    let ua_w: Vec<u16> = "VoxDrop-UpdateCheck/1.6\0".encode_utf16().collect();
    unsafe {
        let session = WinHttpOpen(
            PCWSTR(ua_w.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session, 1500, 2000, 3000, 3000);
        let connect = WinHttpConnect(session, w!("api.github.com"), 443, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let req_h = WinHttpOpenRequest(
            connect,
            w!("GET"),
            w!("/repos/hhurt21/voxdrop/releases/latest"),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req_h.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let mut out = Vec::new();
        if WinHttpSendRequest(req_h, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(req_h, std::ptr::null_mut()).is_ok()
        {
            loop {
                let mut avail: u32 = 0;
                if WinHttpQueryDataAvailable(req_h, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let mut chunk = vec![0u8; avail as usize];
                let mut read: u32 = 0;
                if WinHttpReadData(
                    req_h,
                    chunk.as_mut_ptr() as *mut c_void,
                    avail,
                    &mut read,
                )
                .is_err()
                    || read == 0
                {
                    break;
                }
                out.extend_from_slice(&chunk[..read as usize]);
                if out.len() > 262_144 {
                    break;
                }
            }
        }
        let _ = WinHttpCloseHandle(req_h);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);

        let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
        let tag = v.get("tag_name").and_then(|s| s.as_str())?.trim().to_string();
        let url = v.get("html_url").and_then(|s| s.as_str())?.trim().to_string();
        if !tag.is_empty() && tag != CURRENT_APP_TAG {
            return Some((tag, url));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_and_backup_roundtrip() {
        let mut st = StatsStore::default();
        st.record_utterance("2026-11-27", "whisper-cpp", "Vulkan", 180);
        st.record_learning("ばんだる", "ヴァンダル");
        let sum = st.summary();
        assert_eq!(sum.total_utterances, 1);
        assert_eq!(sum.avg_infer_ms, 180);
        assert_eq!(sum.top_misrecognitions[0].0, "ばんだる → ヴァンダル");

        let cfg = AppConfig::default();
        let json = export_backup_json(&cfg, &st).unwrap();
        let (cfg_back, st_back) = import_backup_json(&json).unwrap();
        assert_eq!(cfg, cfg_back);
        assert_eq!(st_back.unwrap().total_utterances, 1);
    }

    #[test]
    fn computes_cer_and_three_stage_benchmark_improvements() {
        assert!((char_error_rate("輝晶獣", "輝晶獣") - 0.0).abs() < 0.01);
        assert!((char_error_rate("気象獣", "輝晶獣") - 66.666).abs() < 0.5);

        let c1 = ThreeStageBenchCaseResult {
            id: "1".into(),
            label: "case1".into(),
            expected_text: "輝晶獣".into(),
            raw_text: "気象獣".into(),
            raw_cer: char_error_rate("気象獣", "輝晶獣"),
            raw_ms: 150,
            non_llm_text: "輝晶獣".into(),
            non_llm_cer: char_error_rate("輝晶獣", "輝晶獣"),
            non_llm_ms: 152,
            non_llm_delta: stage_delta_label(66.6, 0.0),
            llm_text: "輝晶獣".into(),
            llm_cer: 0.0,
            llm_ms: 350,
            llm_delta: stage_delta_label(66.6, 0.0),
        };
        let c2 = ThreeStageBenchCaseResult {
            id: "2".into(),
            label: "case2".into(),
            expected_text: "了解".into(),
            raw_text: "了解".into(),
            raw_cer: 0.0,
            raw_ms: 120,
            non_llm_text: "了解".into(),
            non_llm_cer: 0.0,
            non_llm_ms: 121,
            non_llm_delta: stage_delta_label(0.0, 0.0),
            llm_text: "了解しました".into(),
            llm_cer: char_error_rate("了解しました", "了解"),
            llm_ms: 300,
            llm_delta: stage_delta_label(0.0, 200.0),
        };
        let rep = compute_three_stage_report(vec![c1, c2], true);
        assert_eq!(rep.non_llm_improved, 1);
        assert_eq!(rep.non_llm_unchanged, 1);
        assert_eq!(rep.non_llm_regressed, 0);
        assert_eq!(rep.llm_improved, 1);
        assert_eq!(rep.llm_regressed, 1);
        assert!(rep.avg_non_llm_cer < rep.avg_raw_cer);
    }
}
