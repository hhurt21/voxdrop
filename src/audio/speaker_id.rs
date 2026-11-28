//! Local Speaker Verification Profile Store ("Own Voiceprint" Filter) (#8).
//! Computes L2-normalized cosine similarity against a user's enrolled 5-second voiceprint
//! embedding extracted via `sherpa-onnx-c-api.dll` so game voices / videos / other speakers are ignored.

use crate::config::AppConfig;
use crate::stt::sherpa_ffi::ResidentSpeakerExtractor;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerProfile {
    pub embedding: Vec<f32>,
    pub model_name: String,
    pub sample_seconds: f32,
    pub created_at: String,
}

impl SpeakerProfile {
    pub fn default_path() -> PathBuf {
        AppConfig::app_data_dir().join("speaker_profile.json")
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
            .filter(|p| !p.embedding.is_empty())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("ディレクトリ作成失敗: {}", e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("声紋JSON保存失敗: {}", e))?;
        fs::write(path, json).map_err(|e| format!("声紋ファイル書込失敗: {}", e))
    }

    pub fn remove_file(path: &Path) {
        let _ = fs::remove_file(path);
    }
}

/// Computes cosine similarity (-1.0 ..= 1.0) between two speaker embedding vectors (#8).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let xf = x as f64;
        let yf = y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    let denom = (na * nb).sqrt();
    if denom <= 1e-12 {
        0.0
    } else {
        (dot / denom).clamp(-1.0, 1.0) as f32
    }
}

/// Verifies an incoming 16kHz utterance against the enrolled `SpeakerProfile`.
/// Returns `Ok(similarity)` cleanly; returns `1.0` if samples are too short (<0.35s) to judge reliably.
pub fn verify_speaker_similarity(
    extractor: &ResidentSpeakerExtractor,
    profile: &SpeakerProfile,
    samples_16k: &[f32],
) -> Result<f32, String> {
    if samples_16k.len() < 5600 {
        // Very short single-word backchannel (< 0.35s): skip harsh rejection
        return Ok(1.0);
    }
    let emb = extractor.compute_embedding(samples_16k)?;
    Ok(cosine_similarity(&profile.embedding, &emb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_scores_identical_high_and_orthogonal_low() {
        let a = vec![1.0, 2.0, -1.0, 0.5];
        let same = vec![2.0, 4.0, -2.0, 1.0];
        let orth = vec![1.0, -0.5, 0.0, 0.0];
        assert!((cosine_similarity(&a, &same) - 1.0).abs() < 1e-5);
        assert!(cosine_similarity(&a, &orth).abs() < 1e-5);
        assert_eq!(cosine_similarity(&a, &[1.0]), 0.0);
    }
}
