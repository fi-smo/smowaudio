//! Feed-forward soft-knee compressor (log-domain detector, branching smoothing).

use serde::{Deserialize, Serialize};

use super::{lin_to_db, time_coef};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompressorSettings {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

impl Default for CompressorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -20.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            makeup_db: 4.0,
        }
    }
}

pub struct Compressor {
    s: CompressorSettings,
    attack: f32,
    release: f32,
    /// Smoothed gain reduction in dB (always <= 0).
    reduction_db: f32,
}

impl Compressor {
    pub fn new(settings: CompressorSettings) -> Self {
        let mut comp = Self { s: settings, attack: 0.0, release: 0.0, reduction_db: 0.0 };
        comp.set(settings);
        comp
    }

    pub fn set(&mut self, settings: CompressorSettings) {
        self.s = settings;
        self.attack = time_coef(settings.attack_ms);
        self.release = time_coef(settings.release_ms);
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.reduction_db
    }

    fn static_curve(&self, x_db: f32) -> f32 {
        let t = self.s.threshold_db;
        let r = self.s.ratio.max(1.0);
        let w = self.s.knee_db.max(0.0);
        let over = x_db - t;
        if 2.0 * over < -w {
            x_db
        } else if w > 0.0 && 2.0 * over.abs() <= w {
            x_db + (1.0 / r - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
        } else {
            t + over / r
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.s.enabled {
            self.reduction_db = 0.0;
            return;
        }
        let makeup = super::db_to_lin(self.s.makeup_db);
        for sample in buf.iter_mut() {
            let x_db = lin_to_db(sample.abs());
            let target = self.static_curve(x_db) - x_db;
            let coef = if target < self.reduction_db { self.attack } else { self.release };
            self.reduction_db = target + (self.reduction_db - target) * coef;
            *sample *= super::db_to_lin(self.reduction_db) * makeup;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loud_signal_is_reduced_by_ratio() {
        let settings = CompressorSettings { knee_db: 0.0, makeup_db: 0.0, ..Default::default() };
        let mut comp = Compressor::new(settings);
        let mut buf = vec![super::super::db_to_lin(-8.0); 48_000]; // 12 dB over threshold
        comp.process(&mut buf);
        // 3:1 ratio -> 12 dB over becomes 4 dB over -> -8 dB reduction.
        assert!((comp.gain_reduction_db() + 8.0).abs() < 0.1);
    }

    #[test]
    fn quiet_signal_is_untouched() {
        let settings = CompressorSettings { makeup_db: 0.0, ..Default::default() };
        let mut comp = Compressor::new(settings);
        let mut buf = vec![super::super::db_to_lin(-40.0); 4_800];
        comp.process(&mut buf);
        assert!(comp.gain_reduction_db().abs() < 0.01);
    }
}
