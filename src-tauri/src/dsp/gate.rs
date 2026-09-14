//! Noise gate with hysteresis and hold, so it doesn't chatter on word endings.

use serde::{Deserialize, Serialize};

use super::{db_to_lin, lin_to_db, time_coef, SAMPLE_RATE};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateSettings {
    pub enabled: bool,
    pub threshold_db: f32,
    /// How far below the threshold the signal must fall before the gate closes.
    pub hysteresis_db: f32,
    /// Attenuation applied while closed (e.g. -80 = silence, -15 = gentle expander).
    pub range_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -45.0,
            hysteresis_db: 6.0,
            range_db: -80.0,
            attack_ms: 2.0,
            hold_ms: 150.0,
            release_ms: 120.0,
        }
    }
}

pub struct NoiseGate {
    s: GateSettings,
    envelope: f32,
    env_release: f32,
    gain: f32,
    attack: f32,
    release: f32,
    hold_samples: u32,
    hold_left: u32,
    open: bool,
}

impl NoiseGate {
    pub fn new(settings: GateSettings) -> Self {
        let mut gate = Self {
            s: settings,
            envelope: 0.0,
            env_release: time_coef(20.0),
            gain: 0.0,
            attack: 0.0,
            release: 0.0,
            hold_samples: 0,
            hold_left: 0,
            open: false,
        };
        gate.set(settings);
        gate
    }

    pub fn set(&mut self, settings: GateSettings) {
        self.s = settings;
        self.attack = time_coef(settings.attack_ms);
        self.release = time_coef(settings.release_ms);
        self.hold_samples = (settings.hold_ms * 0.001 * SAMPLE_RATE) as u32;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.s.enabled {
            return;
        }
        let closed_gain = db_to_lin(self.s.range_db);
        let close_db = self.s.threshold_db - self.s.hysteresis_db;
        for sample in buf.iter_mut() {
            let x = sample.abs();
            self.envelope = if x > self.envelope { x } else { x + (self.envelope - x) * self.env_release };
            let level = lin_to_db(self.envelope);

            if level >= self.s.threshold_db {
                self.open = true;
                self.hold_left = self.hold_samples;
            } else if self.open && level < close_db {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }

            let target = if self.open { 1.0 } else { closed_gain };
            let coef = if target > self.gain { self.attack } else { self.release };
            self.gain = target + (self.gain - target) * coef;
            *sample *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_noise_is_silenced_and_loud_speech_passes() {
        let mut gate = NoiseGate::new(GateSettings::default());
        let mut quiet = vec![0.001f32; 48_000]; // -60 dBFS
        gate.process(&mut quiet);
        assert!(quiet[47_999].abs() < 1e-6);

        let mut loud = vec![0.3f32; 4_800]; // ~-10 dBFS
        gate.process(&mut loud);
        assert!((loud[4_799] - 0.3).abs() < 1e-3);
    }
}
