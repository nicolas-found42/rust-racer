//! Local persistence: settings, selected car, and legitimate personal bests.
//! Stored as JSON next to the game (project-local `saved/` directory), loaded
//! defensively: missing or malformed saves fall back to defaults without
//! failing the game.

use crate::tuning::CarId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Per-car best timed-run score. Only legitimate runs (no debug stepping, no
/// altered tuning during the run) may write a personal best.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CarBest {
    pub score: f32,
    /// Car code + duration, for display.
    pub date: String,
    /// True when the score was set with default tuning and no debug stepping.
    pub legitimate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Master volume 0..1.
    pub volume_master: f32,
    /// Engine/tire/impact relative volumes 0..1.
    pub volume_engine: f32,
    pub volume_tires: f32,
    pub volume_effects: f32,
    /// Graphics quality 0 = low, 1 = medium, 2 = high.
    pub graphics_quality: u8,
    pub selected_car: CarId,
    /// Per-car personal bests for timed runs.
    pub bests: std::collections::BTreeMap<String, CarBest>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume_master: 0.8,
            volume_engine: 0.7,
            volume_tires: 0.6,
            volume_effects: 0.8,
            graphics_quality: 2,
            selected_car: CarId::FairladyZ,
            bests: std::collections::BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn car_key(car: CarId) -> String {
        format!("{car:?}")
    }

    pub fn best_for(&self, car: CarId) -> Option<&CarBest> {
        self.bests
            .get(&Self::car_key(car))
            .filter(|b| b.legitimate && b.score > 0.0)
    }

    /// Record a personal best if legitimate and higher than the existing one.
    /// Returns true when a new best was stored.
    pub fn record_best(&mut self, car: CarId, score: f32, date: String) -> bool {
        if score <= 0.0 {
            return false;
        }
        let key = Self::car_key(car);
        let improved = match self.bests.get(&key) {
            Some(existing) if existing.legitimate => score > existing.score,
            _ => true,
        };
        if improved {
            self.bests.insert(
                key,
                CarBest {
                    score,
                    date,
                    legitimate: true,
                },
            );
        }
        improved
    }

    /// Malformed/partial input: keep known-good fields, drop the rest.
    pub fn sanitized_from(value: serde_json::Value) -> Settings {
        match serde_json::from_value::<Settings>(value.clone()) {
            Ok(s) => {
                let mut s = s;
                s.clamp();
                s
            }
            Err(_) => {
                // Field-by-field salvage for partially valid saves.
                let mut s = Settings::default();
                if let Some(obj) = value.as_object() {
                    if let Some(v) = obj.get("volume_master").and_then(|v| v.as_f64()) {
                        s.volume_master = v as f32;
                    }
                    if let Some(v) = obj.get("graphics_quality").and_then(|v| v.as_u64()) {
                        s.graphics_quality = (v as u8).min(2);
                    }
                    if let Some(v) = obj
                        .get("selected_car")
                        .and_then(|v| serde_json::from_value::<CarId>(v.clone()).ok())
                    {
                        s.selected_car = v;
                    }
                    if let Some(map) = obj.get("bests").and_then(|v| v.as_object()) {
                        for (k, v) in map {
                            if let Ok(b) = serde_json::from_value::<CarBest>(v.clone()) {
                                if b.legitimate && b.score.is_finite() && b.score >= 0.0 {
                                    s.bests.insert(k.clone(), b);
                                }
                            }
                        }
                    }
                }
                s.clamp();
                s
            }
        }
    }

    pub fn clamp(&mut self) {
        self.volume_master = self.volume_master.clamp(0.0, 1.0);
        self.volume_engine = self.volume_engine.clamp(0.0, 1.0);
        self.volume_tires = self.volume_tires.clamp(0.0, 1.0);
        self.volume_effects = self.volume_effects.clamp(0.0, 1.0);
        self.graphics_quality = self.graphics_quality.min(2);
        if !self.volume_master.is_finite() {
            self.volume_master = 0.8;
        }
    }
}

/// File-backed settings store.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(base_dir: &Path) -> Self {
        SettingsStore {
            path: base_dir.join("settings.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load settings; missing or malformed files produce defaults.
    pub fn load(&self) -> Settings {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => Settings::sanitized_from(value),
                Err(_) => Settings::default(),
            },
            Err(_) => Settings::default(),
        }
    }

    /// Persist settings atomically (write temp, rename).
    pub fn save(&self, settings: &Settings) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(settings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rust-racer-test-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_gives_defaults() {
        let store = SettingsStore::new(&tmpdir("missing"));
        let s = store.load();
        assert_eq!(s.graphics_quality, 2);
        assert_eq!(s.selected_car, CarId::FairladyZ);
    }

    #[test]
    fn roundtrip_preserves_bests() {
        let dir = tmpdir("roundtrip");
        let store = SettingsStore::new(&dir);
        let mut s = Settings::default();
        s.record_best(CarId::Rx7, 12345.6, "2026-10-25".into());
        s.volume_master = 0.35;
        store.save(&s).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.volume_master, 0.35);
        let b = loaded.best_for(CarId::Rx7).unwrap();
        assert!((b.score - 12345.6).abs() < 0.01);
    }

    #[test]
    fn malformed_file_recovers() {
        let dir = tmpdir("malformed");
        std::fs::write(dir.join("settings.json"), "{{{ not json !!!").unwrap();
        let store = SettingsStore::new(&dir);
        let s = store.load();
        assert_eq!(s.graphics_quality, 2);
        // And saving over it works.
        store.save(&Settings::default()).unwrap();
    }

    #[test]
    fn partial_salvage_keeps_valid_fields() {
        let v: serde_json::Value = serde_json::json!({
            "volume_master": 0.5,
            "graphics_quality": 9,
            "selected_car": "Rx7",
            "bests": {
                "Rx7": { "score": 500.0, "date": "d", "legitimate": true },
                "Skyline": { "score": "bad", "legitimate": true }
            },
            "volume_engine": "oops"
        });
        let s = Settings::sanitized_from(v);
        assert_eq!(s.volume_master, 0.5);
        assert_eq!(s.graphics_quality, 2); // clamped
        assert_eq!(s.selected_car, CarId::Rx7);
        assert!(s.best_for(CarId::Rx7).is_some());
        assert!(s.best_for(CarId::Skyline).is_none());
        // volume_engine fell back to default.
        assert_eq!(s.volume_engine, Settings::default().volume_engine);
    }

    #[test]
    fn bests_only_improve_and_only_legitimate() {
        let mut s = Settings::default();
        assert!(s.record_best(CarId::Skyline, 100.0, "d".into()));
        assert!(!s.record_best(CarId::Skyline, 50.0, "d".into()));
        assert_eq!(s.best_for(CarId::Skyline).unwrap().score, 100.0);
        assert!(s.record_best(CarId::Skyline, 200.0, "e".into()));
        assert_eq!(s.best_for(CarId::Skyline).unwrap().score, 200.0);
        // Illegitimate scores never stored via record_best.
        assert!(!s.record_best(CarId::Rx7, -5.0, "d".into()));
    }
}
