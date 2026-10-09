//! Peak limiter for final outputs: bit-exact below the threshold, instant gain reduction on
//! peaks that would otherwise hard-clip, smooth release. No lookahead, so no added delay.

use super::{db_to_lin, lin_to_db, time_coef};

pub struct Limiter {
    threshold: f32,
    release: f32,
    /// 1 - gain. Smoothing this instead of the gain keeps the release exact in f32: near
    /// unity, per-sample gain steps are smaller than float precision and would stall.
    reduction: f32,
}

impl Limiter {
    pub fn new(threshold_db: f32, release_ms: f32) -> Self {
        Self { threshold: db_to_lin(threshold_db), release: time_coef(release_ms), reduction: 0.0 }
    }

    pub fn gain_reduction_db(&self) -> f32 {
        lin_to_db(1.0 - self.reduction)
    }

    /// Processes interleaved audio in place. Channels share one gain so the stereo image stays put.
    pub fn process(&mut self, buf: &mut [f32], channels: usize) {
        for frame in buf.chunks_exact_mut(channels.max(1)) {
            let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
            let needed = if peak > self.threshold { 1.0 - self.threshold / peak } else { 0.0 };
            if needed > self.reduction {
                self.reduction = needed;
            } else if self.reduction > 0.0 {
                self.reduction = needed + (self.reduction - needed) * self.release;
                if self.reduction < 1e-4 {
                    self.reduction = 0.0;
                }
            }
            if self.reduction > 0.0 {
                let gain = 1.0 - self.reduction;
                frame.iter_mut().for_each(|s| *s *= gain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_threshold_is_bit_exact() {
        let mut limiter = Limiter::new(-1.0, 80.0);
        let original: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.01).sin() * 0.8).collect();
        let mut buf = original.clone();
        limiter.process(&mut buf, 2);
        assert_eq!(buf, original);
    }

    #[test]
    fn peaks_never_exceed_threshold_and_gain_recovers() {
        let mut limiter = Limiter::new(-1.0, 80.0);
        let threshold = db_to_lin(-1.0);
        let mut loud: Vec<f32> =
            (0..9_600).map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * 1.6).collect();
        limiter.process(&mut loud, 1);
        assert!(loud.iter().all(|s| s.abs() <= threshold + 1e-6));

        let mut quiet = vec![0.1f32; 48_000];
        limiter.process(&mut quiet, 1);
        assert_eq!(quiet[47_999], 0.1);
        assert_eq!(limiter.gain_reduction_db(), 0.0);
    }
}
