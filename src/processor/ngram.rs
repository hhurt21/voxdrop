//! Personal N-gram Language Model Reranker (Non-LLM, 0 MB VRAM) (#4).
//! Learns unigram / bigram / trigram frequencies locally from confirmed Discord messages
//! and uses smoothed log-probabilities to break ties in phonetic dictionary corrections (#1)
//! and word-level ROVER dual-engine consensus (#3).

use crate::config::AppConfig;
use crate::processor::phonetic::segment_morphemes_with_readings;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_NGRAM_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonalNgramModel {
    pub unigrams: HashMap<String, u32>,
    pub bigrams: HashMap<String, u32>,
    pub trigrams: HashMap<String, u32>,
    pub total_tokens: u64,
    #[serde(default)]
    lru_keys: VecDeque<(u8, String)>,
}

impl PersonalNgramModel {
    pub fn default_path() -> PathBuf {
        AppConfig::app_data_dir().join("personal_ngram.json")
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
        if let Ok(json) = serde_json::to_string(self) {
            let _ = fs::write(path, json);
        }
    }

    pub fn clear(&mut self) {
        self.unigrams.clear();
        self.bigrams.clear();
        self.trigrams.clear();
        self.total_tokens = 0;
        self.lru_keys.clear();
    }

    pub fn entry_count(&self) -> usize {
        self.unigrams.len() + self.bigrams.len() + self.trigrams.len()
    }

    fn record_entry(&mut self, order: u8, key: String) {
        let map = match order {
            1 => &mut self.unigrams,
            2 => &mut self.bigrams,
            _ => &mut self.trigrams,
        };
        if let Some(cnt) = map.get_mut(&key) {
            *cnt = cnt.saturating_add(1);
            return;
        }
        map.insert(key.clone(), 1);
        self.lru_keys.push_back((order, key));

        while self.entry_count() > MAX_NGRAM_ENTRIES {
            let Some((ev_order, ev_key)) = self.lru_keys.pop_front() else {
                break;
            };
            match ev_order {
                1 => {
                    self.unigrams.remove(&ev_key);
                }
                2 => {
                    self.bigrams.remove(&ev_key);
                }
                _ => {
                    self.trigrams.remove(&ev_key);
                }
            }
        }
    }

    /// Learns token sequences from a confirmed sent message.
    pub fn observe_text(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let toks: Vec<String> = segment_morphemes_with_readings(trimmed)
            .into_iter()
            .map(|t| t.surface)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if toks.is_empty() {
            return;
        }

        for i in 0..toks.len() {
            self.total_tokens = self.total_tokens.saturating_add(1);
            self.record_entry(1, toks[i].clone());
            if i + 1 < toks.len() {
                let bi = format!("{}\t{}", toks[i], toks[i + 1]);
                self.record_entry(2, bi);
            }
            if i + 2 < toks.len() {
                let tri = format!("{}\t{}\t{}", toks[i], toks[i + 1], toks[i + 2]);
                self.record_entry(3, tri);
            }
        }
    }

    /// Computes a smoothed log-likelihood score (>= 0.0) for placing `candidate` between optional `prev` and `next` surfaces.
    pub fn score_phrase(&self, prev: Option<&str>, candidate: &str, next: Option<&str>) -> f32 {
        let cand_toks: Vec<String> = segment_morphemes_with_readings(candidate)
            .into_iter()
            .map(|t| t.surface)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if cand_toks.is_empty() {
            return 0.0;
        }

        let mut score = 0.0f32;
        for t in &cand_toks {
            if let Some(&cnt) = self.unigrams.get(t) {
                score += ((cnt as f32) + 1.0).ln() * 0.6;
            }
        }
        for w in cand_toks.windows(2) {
            let k = format!("{}\t{}", w[0], w[1]);
            if let Some(&cnt) = self.bigrams.get(&k) {
                score += ((cnt as f32) + 1.0).ln() * 1.1;
            }
        }
        if let Some(p) = prev.map(str::trim).filter(|s| !s.is_empty()) {
            let p_last = segment_morphemes_with_readings(p)
                .into_iter()
                .last()
                .map(|m| m.surface)
                .unwrap_or_else(|| p.to_string());
            let bi = format!("{}\t{}", p_last, cand_toks[0]);
            if let Some(&cnt) = self.bigrams.get(&bi) {
                score += ((cnt as f32) + 1.0).ln() * 1.5;
            }
            if cand_toks.len() >= 2 {
                let tri = format!("{}\t{}\t{}", p_last, cand_toks[0], cand_toks[1]);
                if let Some(&cnt) = self.trigrams.get(&tri) {
                    score += ((cnt as f32) + 1.0).ln() * 1.8;
                }
            }
        }
        if let Some(n) = next.map(str::trim).filter(|s| !s.is_empty()) {
            let n_first = segment_morphemes_with_readings(n)
                .into_iter()
                .next()
                .map(|m| m.surface)
                .unwrap_or_else(|| n.to_string());
            let bi = format!("{}\t{}", cand_toks[cand_toks.len() - 1], n_first);
            if let Some(&cnt) = self.bigrams.get(&bi) {
                score += ((cnt as f32) + 1.0).ln() * 1.5;
            }
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_ngram_reranks_preferred_phrase_higher() {
        let mut lm = PersonalNgramModel::default();
        for _ in 0..4 {
            lm.observe_text("輝晶獣に行くよ");
        }
        let s_good = lm.score_phrase(None, "輝晶獣", Some("に"));
        let s_bad = lm.score_phrase(None, "気象獣", Some("に"));
        assert!(
            s_good > s_bad + 1.0,
            "expected '輝晶獣' ({:.2}) to beat '気象獣' ({:.2})",
            s_good,
            s_bad
        );
    }
}
