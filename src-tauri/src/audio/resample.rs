//! Bridges two audio devices running on independent clocks.
//!
//! Samples arrive in a lock-free ring buffer from a capture thread. The reader keeps the
//! buffer near a target fill level by resampling at a ratio within ±0.05% of 1.0 (well
//! below audible pitch change), using a windowed-sinc interpolator so music stays clean.
//!
//! The target adapts for the lowest safe latency: it starts small, grows by 50% when the
//! buffer runs dry while input is still arriving (the devices move audio in bigger bursts
//! than expected), and shrinks slowly toward the lowest fill actually observed when there
//! is spare room. After growing it won't shrink below that size for 5 minutes.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use rtrb::{Consumer, Producer, RingBuffer};

const TAPS: usize = 16;
const PHASES: usize = 1024;
const MAX_RATIO_DEVIATION: f64 = 0.0005;
const MAX_CHANNELS: usize = 2;
const SAMPLE_RATE: f64 = 48_000.0;
const MIN_TARGET_MS: f64 = 5.0;
const MAX_TARGET_MS: f64 = 200.0;
const CAPACITY_MS: f64 = 500.0;
/// Kept above the lowest fill seen so normal timing jitter never runs the buffer dry.
const HEADROOM_MS: f64 = 3.0;
/// Output frames without new input after which an underrun means the source stopped.
const INPUT_STALL_FRAMES: u64 = 4_800;
const SHRINK_EVERY_FRAMES: u64 = 48_000 * 5;
const SHRINK_COOLDOWN_FRAMES: u64 = 48_000 * 300;

fn ms_to_frames(ms: f64) -> f64 {
    (SAMPLE_RATE * ms / 1000.0).round()
}

fn sinc_table() -> &'static [[f32; TAPS]] {
    static TABLE: OnceLock<Vec<[f32; TAPS]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=PHASES)
            .map(|phase| {
                let frac = phase as f64 / PHASES as f64;
                let mut row = [0f32; TAPS];
                let mut sum = 0.0;
                for (k, tap) in row.iter_mut().enumerate() {
                    let x = k as f64 - (TAPS / 2 - 1) as f64 - frac;
                    let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                    // Blackman window spanning the kernel.
                    let n = (x + TAPS as f64 / 2.0) / TAPS as f64;
                    let window = 0.42 - 0.5 * (std::f64::consts::TAU * n).cos() + 0.08 * (2.0 * std::f64::consts::TAU * n).cos();
                    let v = sinc * window;
                    *tap = v as f32;
                    sum += v;
                }
                row.iter_mut().for_each(|t| *t = (*t as f64 / sum) as f32);
                row
            })
            .collect()
    })
}

/// Counters shared between the audio threads and diagnostics.
#[derive(Default)]
pub struct BridgeStats {
    pub underruns: AtomicU64,
    pub skips: AtomicU64,
    pub target_frames: AtomicU32,
    /// Largest chunk the capture side delivered at once.
    pub max_in_frames: AtomicU32,
    /// Largest block the render side asked for at once.
    pub max_out_frames: AtomicU32,
    pub in_frames: AtomicU64,
    pub out_frames: AtomicU64,
    /// Longest pause between capture packets since the last summary, in microseconds.
    pub max_in_gap_us: AtomicU32,
    pub avg_fill_frames: AtomicU32,
    /// Packets where Windows reported audio lost before capture (capture buffer overflow).
    pub capture_glitches: AtomicU64,
}

impl BridgeStats {
    pub fn record_in(&self, frames: usize) {
        self.max_in_frames.fetch_max(frames as u32, Ordering::Relaxed);
        self.in_frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    pub fn record_glitch(&self) {
        self.capture_glitches.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_gap(&self, gap: std::time::Duration) {
        self.max_in_gap_us.fetch_max(gap.as_micros().min(u32::MAX as u128) as u32, Ordering::Relaxed);
    }

    pub fn totals(&self) -> (u64, u64) {
        (self.in_frames.load(Ordering::Relaxed), self.out_frames.load(Ordering::Relaxed))
    }

    /// One-line report; resets the max-gap reading.
    pub fn summary(&self) -> String {
        let load = |a: &AtomicU32| a.load(Ordering::Relaxed);
        format!(
            "buffer {:.0} ms (avg fill {:.0} ms), underruns {}, capture glitches {}, skips {}, largest chunk in {} / out {} frames, max input gap {:.1} ms",
            load(&self.target_frames) as f64 * 1000.0 / SAMPLE_RATE,
            load(&self.avg_fill_frames) as f64 * 1000.0 / SAMPLE_RATE,
            self.underruns.load(Ordering::Relaxed),
            self.capture_glitches.load(Ordering::Relaxed),
            self.skips.load(Ordering::Relaxed),
            load(&self.max_in_frames),
            load(&self.max_out_frames),
            self.max_in_gap_us.swap(0, Ordering::Relaxed) as f64 / 1000.0,
        )
    }
}

/// Creates a producer for the capture side and a drift-compensating reader for the render side.
/// `start_ms` is the initial buffer target; it adapts from there.
pub fn bridge(channels: usize, start_ms: f32) -> (Producer<f32>, DriftReader) {
    let (producer, consumer) = RingBuffer::new(ms_to_frames(CAPACITY_MS) as usize * channels);
    (producer, DriftReader::new(consumer, channels, ms_to_frames(start_ms as f64)))
}

pub struct DriftReader {
    consumer: Consumer<f32>,
    channels: usize,
    target: f64,
    /// Lowest target allowed right now (raised after an underrun, for a cooldown period).
    floor: f64,
    avg_fill: f64,
    frac: f64,
    history: [[f32; TAPS]; MAX_CHANNELS],
    primed: bool,
    seen_in_frames: u64,
    out_since_input: u64,
    frames_since_underrun: u64,
    window_frames: u64,
    window_min_fill: usize,
    pub stats: Arc<BridgeStats>,
}

impl DriftReader {
    fn new(consumer: Consumer<f32>, channels: usize, target_frames: f64) -> Self {
        let stats = Arc::new(BridgeStats::default());
        stats.target_frames.store(target_frames as u32, Ordering::Relaxed);
        Self {
            consumer,
            channels: channels.min(MAX_CHANNELS),
            target: target_frames,
            floor: ms_to_frames(MIN_TARGET_MS),
            avg_fill: target_frames,
            frac: 1.0,
            history: [[0.0; TAPS]; MAX_CHANNELS],
            primed: false,
            seen_in_frames: 0,
            out_since_input: 0,
            frames_since_underrun: 0,
            window_frames: 0,
            window_min_fill: usize::MAX,
            stats,
        }
    }

    fn fill_frames(&self) -> usize {
        self.consumer.slots() / self.channels
    }

    fn pop_frame(&mut self) -> bool {
        if self.consumer.slots() < self.channels {
            return false;
        }
        for ch in 0..self.channels {
            let sample = self.consumer.pop().unwrap_or(0.0);
            let h = &mut self.history[ch];
            h.copy_within(1.., 0);
            h[TAPS - 1] = sample;
        }
        true
    }

    fn set_target(&mut self, frames: f64) {
        self.target = frames.clamp(ms_to_frames(MIN_TARGET_MS), ms_to_frames(MAX_TARGET_MS));
        self.stats.target_frames.store(self.target as u32, Ordering::Relaxed);
    }

    fn reset_window(&mut self) {
        self.window_frames = 0;
        self.window_min_fill = usize::MAX;
    }

    fn input_flowing(&self) -> bool {
        self.out_since_input < INPUT_STALL_FRAMES
    }

    fn on_underrun(&mut self) {
        self.primed = false;
        self.stats.underruns.fetch_add(1, Ordering::Relaxed);
        // A source that stopped isn't a reason to add latency; a too-small buffer is.
        if self.input_flowing() {
            self.set_target(self.target * 1.5);
            self.floor = self.target;
        }
        self.frames_since_underrun = 0;
        self.reset_window();
    }

    fn maybe_shrink(&mut self) {
        let converged = (self.avg_fill - self.target).abs() < ms_to_frames(2.0);
        if !converged || !self.input_flowing() || self.window_min_fill == usize::MAX {
            return;
        }
        if self.frames_since_underrun >= SHRINK_COOLDOWN_FRAMES {
            self.floor = ms_to_frames(MIN_TARGET_MS);
        }
        let surplus = self.window_min_fill as f64 - ms_to_frames(HEADROOM_MS);
        if surplus >= ms_to_frames(1.0) {
            // Halve the spare room each step; the drift controller then drains it inaudibly.
            self.set_target((self.target - surplus / 2.0).max(self.floor));
        }
    }

    /// Fills `out` (interleaved, same channel count as the bridge). Outputs silence while
    /// buffering after start-up or an underrun.
    pub fn read(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;
        self.stats.max_out_frames.fetch_max(frames as u32, Ordering::Relaxed);
        self.stats.out_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.stats.avg_fill_frames.store(self.avg_fill as u32, Ordering::Relaxed);

        let in_frames = self.stats.in_frames.load(Ordering::Relaxed);
        if in_frames != self.seen_in_frames {
            self.seen_in_frames = in_frames;
            self.out_since_input = 0;
        } else {
            self.out_since_input += frames as u64;
        }

        let mut fill = self.fill_frames();
        if !self.primed {
            if (fill as f64) < self.target {
                out.fill(0.0);
                return;
            }
            // Input arrives in bursts, so priming usually overshoots the target by most of a
            // burst. Trim it now, at the start of playback, rather than draining it inaudibly
            // over the next minute.
            for _ in 0..fill - self.target as usize {
                self.pop_frame();
            }
            fill = self.fill_frames();
            self.primed = true;
            self.avg_fill = fill as f64;
            self.reset_window();
        }

        // A long stall (e.g. device switch, paused Virtual Mic) leaves a backlog: jump back to the target.
        if fill as f64 > self.target * 6.0 + ms_to_frames(100.0) {
            for _ in 0..fill - self.target as usize {
                self.pop_frame();
            }
            self.avg_fill = self.target;
            self.stats.skips.fetch_add(1, Ordering::Relaxed);
        }

        self.avg_fill += (self.fill_frames() as f64 - self.avg_fill) * 0.01;
        let error = (self.avg_fill - self.target) / self.target;
        let ratio = 1.0 + (error * 0.002).clamp(-MAX_RATIO_DEVIATION, MAX_RATIO_DEVIATION);

        let table = sinc_table();
        for i in 0..frames {
            while self.frac >= 1.0 {
                if !self.pop_frame() {
                    // Never leave stale samples from a previous block in the output.
                    out[i * ch..].fill(0.0);
                    self.on_underrun();
                    return;
                }
                self.frac -= 1.0;
            }
            let kernel = &table[(self.frac * PHASES as f64).round() as usize];
            for (c, sample) in out[i * ch..(i + 1) * ch].iter_mut().enumerate() {
                *sample = self.history[c].iter().zip(kernel).map(|(h, k)| h * k).sum();
            }
            self.frac += ratio;
        }

        self.frames_since_underrun += frames as u64;
        self.window_min_fill = self.window_min_fill.min(self.fill_frames());
        self.window_frames += frames as u64;
        if self.window_frames >= SHRINK_EVERY_FRAMES {
            self.maybe_shrink();
            self.reset_window();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_audio_through_without_loss() {
        let (mut tx, mut rx) = bridge(1, 20.0);
        // Above the 960-frame target but below the backlog-skip threshold.
        let tone: Vec<f32> = (0..2_000).map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin() * 0.5).collect();
        for s in &tone {
            tx.push(*s).unwrap();
        }
        let mut out = vec![0f32; 1_500];
        rx.read(&mut out);
        let peak = out[500..].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
    }

    #[test]
    fn waits_for_target_fill_before_playing() {
        let (mut tx, mut rx) = bridge(2, 20.0);
        tx.push(1.0).unwrap();
        tx.push(1.0).unwrap();
        let mut out = vec![0.5f32; 32];
        rx.read(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn underrun_outputs_silence_and_grows_buffer() {
        let (mut tx, mut rx) = bridge(1, 20.0);
        for _ in 0..1_000 {
            tx.push(0.7).unwrap();
        }
        rx.stats.record_in(1_000);
        // Stale data the reader must overwrite.
        let mut out = vec![0.7f32; 2_000];
        rx.read(&mut out);
        assert!(out[1_200..].iter().all(|s| *s == 0.0), "stale samples left after underrun");
        assert_eq!(rx.stats.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(rx.stats.target_frames.load(Ordering::Relaxed), 1_440);
    }

    #[test]
    fn buffer_shrinks_when_input_is_steady() {
        let (mut tx, mut rx) = bridge(1, 30.0);
        let mut out = vec![0f32; 480];
        // 80 seconds of steady 10 ms blocks on both sides.
        for _ in 0..8_000 {
            for _ in 0..480 {
                let _ = tx.push(0.1);
            }
            rx.stats.record_in(480);
            rx.read(&mut out);
        }
        let target = rx.stats.target_frames.load(Ordering::Relaxed);
        assert_eq!(rx.stats.underruns.load(Ordering::Relaxed), 0);
        assert!(target < 1_440, "target stayed at {target} frames");
    }
}
