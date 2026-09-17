//! The audio engine: one supervised thread per stream, reconnecting on device loss.
//!
//! ```text
//! cable A/B/VB (capture) ─► ring buffer ─┐
//!                                        ├─► output thread: EQ + volume + mix ─► headphones
//! physical mic (capture) ─► MicChain ─► ring buffer ─► mic thread ─► cable C (render)
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::Mutex;
use serde::Serialize;

use crate::audio::device::{self, Flow};
use crate::audio::{resample, routing, stream, disable_denormals, ComGuard};
use crate::config::{Config, CHANNEL_COUNT, CHANNEL_NAMES};
use crate::dsp::chain::{stereo_peak_db, ChannelChain, ChannelSettings, MicChain, MicMeters, MicSettings};
use crate::dsp::denoise::{Denoiser, FRAME};
use crate::dsp::limiter::Limiter;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Meters {
    pub mic: MicMeters,
    /// Peak per channel after its volume and EQ, left and right, in dBFS.
    pub channels: [[f32; 2]; CHANNEL_COUNT],
    /// Final mix after the master volume and limiter, left and right.
    pub master: [f32; 2],
    /// How much the output limiter is currently turning peaks down (dB, <= 0).
    pub master_reduction_db: f32,
    /// Largest channel buffer right now, in ms (part of the playback delay).
    pub buffer_ms: f32,
}

/// Settings slot the audio thread polls cheaply: it only locks when the version changed.
struct Slot<T> {
    value: Mutex<T>,
    version: AtomicU64,
}

impl<T: Clone> Slot<T> {
    fn new(value: T) -> Self {
        Self { value: Mutex::new(value), version: AtomicU64::new(0) }
    }

    fn set(&self, value: T) {
        *self.value.lock() = value;
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the new value if it changed since `seen`.
    fn poll(&self, seen: &mut u64) -> Option<T> {
        let v = self.version.load(Ordering::Relaxed);
        if v == *seen {
            return None;
        }
        let guard = self.value.try_lock()?;
        *seen = v;
        Some(guard.clone())
    }
}

pub struct Shared {
    mic: Slot<MicSettings>,
    channels: [Slot<ChannelSettings>; CHANNEL_COUNT],
    master: Slot<ChannelSettings>,
    rules: Mutex<BTreeMap<String, String>>,
    bridges: Mutex<Vec<(String, Arc<resample::BridgeStats>)>>,
    /// Latest step each stream thread reported, for the stall watchdog.
    phases: Mutex<HashMap<String, (&'static str, Instant)>>,
    /// Times an output device ran out of queued audio (audible gaps).
    output_starved: AtomicU64,
    mic_starved: AtomicU64,
    /// "Listen to my mic": mix the processed mic into the headphone output.
    monitor: AtomicBool,
    /// Headphones/speakers to play to (None follows the usual default). Bumping the generation
    /// makes the output stream reopen on it without touching the other streams.
    output_device: Mutex<Option<String>>,
    output_generation: AtomicU64,
    pub meters: Mutex<Meters>,
    pub status: Mutex<HashMap<String, String>>,
}

pub struct Engine {
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    pub fn start(config: &Config) -> Engine {
        let shared = Arc::new(Shared {
            mic: Slot::new(config.mic.clone()),
            channels: std::array::from_fn(|i| {
                Slot::new(config.channels.get(i).map(|c| c.settings.clone()).unwrap_or_default())
            }),
            master: Slot::new(config.master.clone()),
            rules: Mutex::new(config.routing_rules()),
            bridges: Mutex::new(Vec::new()),
            phases: Mutex::new(HashMap::new()),
            output_starved: AtomicU64::new(0),
            mic_starved: AtomicU64::new(0),
            monitor: AtomicBool::new(config.mic.monitor),
            output_device: Mutex::new(config.output_device.clone()),
            output_generation: AtomicU64::new(0),
            meters: Mutex::new(Meters::default()),
            status: Mutex::new(HashMap::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let mut engine = Engine { shared, stop, threads: Vec::new() };
        // Processed mic -> headphone output, used while "listen to my mic" is on.
        let (monitor_tx, monitor_rx) = resample::bridge(1, 10.0);
        let monitor_stats = monitor_rx.stats.clone();
        engine.shared.bridges.lock().push(("Mic monitor".to_string(), monitor_stats.clone()));
        engine.spawn_output(config, monitor_rx);
        engine.spawn_mic(config, monitor_tx, monitor_stats);
        engine.spawn_router();
        engine
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }

    pub fn set_mic(&self, settings: MicSettings) {
        self.shared.monitor.store(settings.monitor, Ordering::Relaxed);
        self.shared.mic.set(settings);
    }

    pub fn set_channel(&self, index: usize, settings: ChannelSettings) {
        if let Some(slot) = self.shared.channels.get(index) {
            slot.set(settings);
        }
    }

    pub fn set_master(&self, settings: ChannelSettings) {
        self.shared.master.set(settings);
    }

    /// Switches the headphones/speakers output live; only the output stream restarts.
    pub fn set_output(&self, output: Option<String>) {
        *self.shared.output_device.lock() = output;
        self.shared.output_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_rules(&self, rules: BTreeMap<String, String>) {
        *self.shared.rules.lock() = rules;
    }

    /// Current meters plus the largest channel buffer (a readout of playback delay).
    pub fn meters(&self) -> Meters {
        let mut meters = self.shared.meters.lock().clone();
        meters.buffer_ms = self
            .shared
            .bridges
            .lock()
            .iter()
            .filter(|(name, _)| CHANNEL_NAMES.contains(&name.as_str()))
            .map(|(_, stats)| stats.target_frames.load(Ordering::Relaxed) as f32 / 48.0)
            .fold(0.0, f32::max);
        meters
    }

    /// Runs `body` on a named thread, retrying every second until the engine stops.
    fn supervise(&mut self, name: &str, mut body: impl FnMut(&AtomicBool) -> Result<()> + Send + 'static) {
        self.supervise_with(name, || (), move |_, stop| body(stop));
    }

    /// Like `supervise`, with state built on the audio thread itself. Needed for the neural
    /// model, whose tensors are `Rc`-based and can't cross threads. The state survives reconnects.
    fn supervise_with<S>(
        &mut self,
        name: &str,
        init: impl FnOnce() -> S + Send + 'static,
        mut body: impl FnMut(&mut S, &AtomicBool) -> Result<()> + Send + 'static,
    ) {
        let (stop, shared, name) = (self.stop.clone(), self.shared.clone(), name.to_string());
        let handle = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || {
                let _com = ComGuard::new();
                disable_denormals();
                {
                    let (shared, name) = (shared.clone(), name.clone());
                    stream::set_phase_hook(move |phase| {
                        let mut phases = shared.phases.lock();
                        match phases.get_mut(&name) {
                            Some(entry) => *entry = (phase, Instant::now()),
                            None => {
                                phases.insert(name.clone(), (phase, Instant::now()));
                            }
                        }
                    });
                }
                stream::phase("thread started");
                let mut state = init();
                let mut last_error = String::new();
                while !stop.load(Ordering::Relaxed) {
                    stream::phase("starting stream");
                    shared.status.lock().insert(name.clone(), "running".into());
                    match body(&mut state, &stop) {
                        Ok(()) => {}
                        Err(e) => {
                            let message = format!("{e:#}");
                            log::warn!("{name}: {message}");
                            if cfg!(debug_assertions) {
                                eprintln!("[audio] {name} failed: {message}");
                            }
                            // Retries happen every second; only log when the reason changes.
                            if message != last_error {
                                crate::append_log(&format!("{name} failed: {message}"));
                                last_error = message;
                            }
                            shared.status.lock().insert(name.clone(), format!("{e:#}"));
                            for _ in 0..10 {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                }
            })
            .expect("spawn audio thread");
        self.threads.push(handle);
    }

    fn spawn_output(&mut self, config: &Config, mut monitor: resample::DriftReader) {
        let mut readers: Vec<Option<resample::DriftReader>> = Vec::new();
        for (i, channel) in config.channels.iter().enumerate() {
            let Some(source) = channel.source.clone() else {
                readers.push(None);
                continue;
            };
            let (mut producer, reader) = resample::bridge(2, 10.0);
            let stats = reader.stats.clone();
            self.shared.bridges.lock().push((CHANNEL_NAMES[i].to_string(), stats.clone()));
            readers.push(Some(reader));
            let mut last_opened = String::new();
            self.supervise(&format!("{} input", CHANNEL_NAMES[i]), move |stop| {
                stream::phase("find device");
                let device = device::by_id(&source)?;
                let device_name = device::friendly_name(&device).unwrap_or_else(|_| source.clone());
                if device_name != last_opened {
                    crate::append_log(&format!("{} input recording from {device_name}", CHANNEL_NAMES[i]));
                    last_opened = device_name;
                }
                let mut last_packet: Option<Instant> = None;
                stream::run_capture(&device, 2, stop, |samples, glitch| {
                    let now = Instant::now();
                    if let Some(prev) = last_packet.replace(now) {
                        stats.record_gap(now - prev);
                    }
                    if glitch {
                        stats.record_glitch();
                    }
                    stats.record_in(samples.len() / 2);
                    // Drop overflow rather than block the capture thread.
                    for s in samples {
                        if producer.push(*s).is_err() {
                            break;
                        }
                    }
                })
            });
        }

        let shared = self.shared.clone();
        let preferred_output = config.previous_default(Flow::Render);
        let mut chains: Vec<ChannelChain> = config.channels.iter().map(|c| ChannelChain::new(c.settings.clone())).collect();
        let mut seen = [u64::MAX; CHANNEL_COUNT];
        let mut scratch = vec![0f32; 48_000];
        let mut limiter = Limiter::new(-1.0, 80.0);
        let mut master_chain = ChannelChain::new(config.master.clone());
        let mut master_seen = u64::MAX;
        let mut monitor_mono = vec![0f32; 24_000];
        let mut last_output = String::new();
        self.supervise("Output", move |stop| {
            stream::phase("find device");
            let generation = shared.output_generation.load(Ordering::Relaxed);
            let output_id = shared.output_device.lock().clone();
            let device = device::resolve_physical(Flow::Render, output_id.as_deref(), preferred_output.as_deref())?;
            let device_name = device::friendly_name(&device).unwrap_or_default();
            if device_name != last_output {
                crate::append_log(&format!("Output playing to {device_name}"));
                last_output = device_name;
            }
            let keep_going =
                || !stop.load(Ordering::Relaxed) && shared.output_generation.load(Ordering::Relaxed) == generation;
            stream::run_render_while(&device, 2, keep_going, Some(&shared.output_starved), |out| {
                out.fill(0.0);
                if scratch.len() < out.len() {
                    scratch.resize(out.len(), 0.0);
                }
                let mut peaks = [[-120f32; 2]; CHANNEL_COUNT];
                for (i, reader) in readers.iter_mut().enumerate() {
                    let Some(reader) = reader else { continue };
                    if let Some(s) = shared.channels[i].poll(&mut seen[i]) {
                        chains[i].set(s);
                    }
                    let buf = &mut scratch[..out.len()];
                    reader.read(buf);
                    chains[i].process(buf);
                    peaks[i] = chains[i].peak_db;
                    out.iter_mut().zip(buf.iter()).for_each(|(o, s)| *o += s);
                }
                if shared.monitor.load(Ordering::Relaxed) {
                    let frames = out.len() / 2;
                    if monitor_mono.len() < frames {
                        monitor_mono.resize(frames, 0.0);
                    }
                    monitor.read(&mut monitor_mono[..frames]);
                    for (pair, s) in out.chunks_exact_mut(2).zip(&monitor_mono[..frames]) {
                        pair[0] += s;
                        pair[1] += s;
                    }
                }
                if let Some(s) = shared.master.poll(&mut master_seen) {
                    master_chain.set(s);
                }
                master_chain.process(out);
                limiter.process(out, 2);
                if let Some(mut m) = shared.meters.try_lock() {
                    m.channels = peaks;
                    m.master = stereo_peak_db(out);
                    m.master_reduction_db = limiter.gain_reduction_db();
                }
            })
        });
    }

    fn spawn_mic(
        &mut self,
        config: &Config,
        mut monitor: rtrb::Producer<f32>,
        monitor_stats: Arc<resample::BridgeStats>,
    ) {
        let Some(sink) = config.mic_sink.clone() else { return };
        let (mut producer, mut reader) = resample::bridge(1, 10.0);
        let mic_stats = reader.stats.clone();
        self.shared.bridges.lock().push(("Virtual mic".to_string(), mic_stats.clone()));

        struct MicState {
            chain: MicChain,
            seen: u64,
            frame: [f32; FRAME],
            filled: usize,
        }

        let settings = config.mic.clone();
        let init = move || {
            let mut chain = MicChain::new(settings.clone());
            match Denoiser::new(settings.denoise) {
                Ok(d) => chain.attach_denoiser(d),
                Err(e) => log::error!("noise removal unavailable: {e:#}"),
            }
            MicState { chain, seen: u64::MAX, frame: [0.0; FRAME], filled: 0 }
        };

        let shared = self.shared.clone();
        let mic_id = config.mic_device.clone();
        let preferred_mic = config.previous_default(Flow::Capture);
        self.supervise_with("Microphone", init, move |st, stop| {
            stream::phase("find device");
            let device = device::resolve_physical(Flow::Capture, mic_id.as_deref(), preferred_mic.as_deref())?;
            stream::run_capture(&device, 1, stop, |samples, glitch| {
                if glitch {
                    mic_stats.record_glitch();
                }
                for s in samples {
                    st.frame[st.filled] = *s;
                    st.filled += 1;
                    if st.filled < FRAME {
                        continue;
                    }
                    st.filled = 0;
                    if let Some(settings) = shared.mic.poll(&mut st.seen) {
                        st.chain.set(settings);
                    }
                    st.chain.process(&mut st.frame);
                    mic_stats.record_in(FRAME);
                    for s in st.frame.iter() {
                        if producer.push(*s).is_err() {
                            break;
                        }
                    }
                    if shared.monitor.load(Ordering::Relaxed) {
                        monitor_stats.record_in(FRAME);
                        for s in st.frame.iter() {
                            if monitor.push(*s).is_err() {
                                break;
                            }
                        }
                    }
                    if let Some(mut m) = shared.meters.try_lock() {
                        m.mic = st.chain.meters;
                    }
                }
            })
        });

        // A VB-Cable fed while nobody records it queues ~75 ms of audio, and that delay sticks once
        // an app (e.g. Discord) starts listening. So only feed the cable while someone records it.
        let listened = Arc::new(AtomicBool::new(false));
        let (watch_flag, watch_sink) = (listened.clone(), sink.clone());
        self.supervise("Virtual mic listeners", move |stop| {
            let partner = crate::config::cable_partner(&watch_sink);
            while !stop.load(Ordering::Relaxed) {
                stream::phase("watching listeners");
                let someone_listening = match &partner {
                    Some(source) => routing::has_other_capture_clients(source)?,
                    // Unknown cable layout: always feed the mic.
                    None => true,
                };
                watch_flag.store(someone_listening, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(250));
            }
            Ok(())
        });

        let mut mono = vec![0f32; 24_000];
        let mic_shared = self.shared.clone();
        self.supervise("Virtual mic", move |stop| {
            if !listened.load(Ordering::Relaxed) {
                stream::phase("paused");
                std::thread::sleep(Duration::from_millis(50));
                return Ok(());
            }
            stream::phase("find device");
            let device = device::by_id(&sink)?;
            let keep_going = || !stop.load(Ordering::Relaxed) && listened.load(Ordering::Relaxed);
            stream::run_render_while(&device, 2, keep_going, Some(&mic_shared.mic_starved), |out| {
                let frames = out.len() / 2;
                if mono.len() < frames {
                    mono.resize(frames, 0.0);
                }
                reader.read(&mut mono[..frames]);
                for (pair, s) in out.chunks_exact_mut(2).zip(&mono[..frames]) {
                    pair[0] = *s;
                    pair[1] = *s;
                }
            })
        });
    }

    #[cfg(test)]
    pub(crate) fn bridge_summaries(&self) -> Vec<String> {
        self.shared.bridges.lock().iter().map(|(name, s)| format!("{name}: {}", s.summary())).collect()
    }

    fn spawn_router(&mut self) {
        let shared = self.shared.clone();
        self.supervise("App routing", move |stop| {
            let mut previous: HashMap<String, (u64, u64)> = HashMap::new();
            let mut last_report = Instant::now();
            let mut reported_stalls: HashSet<String> = HashSet::new();
            while !stop.load(Ordering::Relaxed) {
                stream::phase("routing");
                // Stall watchdog: failing streams log an error and retry, so a stream thread that
                // hasn't moved on for 5 s is stuck inside a Windows or driver call.
                for (name, (phase, since)) in shared.phases.lock().iter() {
                    let secs = since.elapsed().as_secs();
                    if secs >= 5 && reported_stalls.insert(format!("{name}:{phase}")) {
                        crate::append_log(&format!("{name} stuck in '{phase}' for {secs} s"));
                    }
                }
                if cfg!(debug_assertions) {
                    let dt = last_report.elapsed().as_secs_f64().max(1e-3);
                    last_report = Instant::now();
                    for (name, stats) in shared.bridges.lock().iter() {
                        let (now_in, now_out) = stats.totals();
                        let (prev_in, prev_out) = previous.insert(name.clone(), (now_in, now_out)).unwrap_or((now_in, now_out));
                        let line = format!(
                            "[audio] {name}: in {:.0} Hz, out {:.0} Hz, {}",
                            (now_in - prev_in) as f64 / dt,
                            (now_out - prev_out) as f64 / dt,
                            stats.summary()
                        );
                        eprintln!("{line}");
                        // Dev builds started without a console (e.g. by Task Scheduler) still leave stats.
                        crate::append_log(&line);
                    }
                    eprintln!(
                        "[audio] output device ran dry {} times, virtual mic cable {} times",
                        shared.output_starved.load(Ordering::Relaxed),
                        shared.mic_starved.load(Ordering::Relaxed)
                    );
                }
                let rules = shared.rules.lock().clone();
                routing::apply_rules(&rules)?;
                for _ in 0..30 {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::stream::probe;

    fn cable(flow: Flow, hardware: &str) -> String {
        device::list(flow)
            .unwrap()
            .into_iter()
            .find(|d| d.hardware == hardware)
            .unwrap_or_else(|| panic!("{hardware} not installed"))
            .id
    }

    /// Measures the delay the engine itself adds: bursts -> Cable A -> engine (Game channel) -> Cable D.
    /// Each cable's own delay is measured first and subtracted. Plays ticks into cables A and D,
    /// so run it with Smowaudio closed.
    #[test]
    #[ignore = "measures real latency through VB-Audio cables A and D; close Smowaudio and run with --ignored"]
    fn measure_engine_latency() {
        let _com = ComGuard::new();
        let (a_in, a_out) = (cable(Flow::Render, "VB-Audio Cable A"), cable(Flow::Capture, "VB-Audio Cable A"));
        let (d_in, d_out) = (cable(Flow::Render, "VB-Audio Cable D"), cable(Flow::Capture, "VB-Audio Cable D"));

        // Clean measurements first: a backlogged cable may stay delayed afterwards.
        println!("cable A alone:");
        let cable_a = probe::measure(&a_in, &a_out, 6.0).unwrap();
        println!("cable D alone:");
        let cable_d = probe::measure(&d_in, &d_out, 6.0).unwrap();

        let mut config = Config::default();
        config.output_device = Some(d_in.clone());
        config.channels[0].source = Some(a_out.clone());
        println!("cable A -> engine -> cable D (recording A's output and D's output, started before the engine):");
        let (mut results, engine) = probe::measure_multi(&a_in, &[&a_out, &d_out], 12.0, 0.0, || {
            let engine = Engine::start(&config);
            std::thread::sleep(Duration::from_secs(3));
            engine
        })
        .unwrap();
        let summaries = engine.bridge_summaries();
        engine.stop();
        let (total, d_onsets) = results.pop().unwrap();
        let (arrival_at_engine, a_onsets) = results.pop().unwrap();
        let total = total.unwrap_or(f64::NAN);
        let arrival_at_engine = arrival_at_engine.unwrap_or(f64::NAN);

        // Burst arriving at cable A's output (the engine's input) vs at cable D's output, both
        // timestamped by the capture side: no render-clock estimate involved.
        let mut hops = probe::onset_to_onset_ms(&a_onsets, &d_onsets);
        hops.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if let (Some(min), Some(max)) = (hops.first(), hops.last()) {
            println!(
                "ENGINE+D capture-to-capture: median {:.1} ms, range {min:.1}..{max:.1} ms over {} bursts",
                hops[hops.len() / 2],
                hops.len()
            );
        } else {
            println!("ENGINE+D capture-to-capture: no bursts paired");
        }
        println!(
            "SPLIT arrives at cable A output {arrival_at_engine:.1} ms | engine + cable D {:.1} ms | engine alone (minus clean cable D) {:.1} ms",
            total - arrival_at_engine,
            total - arrival_at_engine - cable_d
        );
        std::thread::sleep(Duration::from_secs(1));

        println!("cable D after 3 s of audio nobody read:");
        let cable_d_backlog = probe::measure_with(&d_in, &d_out, 6.0, 3.0, || ()).map(|(ms, _)| ms);
        std::thread::sleep(Duration::from_secs(2));
        println!("cable D again, 2 s after the backlogged session ended:");
        let cable_d_after = probe::measure(&d_in, &d_out, 6.0).map_err(|e| e.to_string());

        let fmt = |r: &Result<f64, String>| r.as_ref().map_or_else(|e| format!("failed: {e}"), |ms| format!("{ms:.1} ms"));
        println!(
            "LATENCY cable A {cable_a:.1} ms | cable D {cable_d:.1} ms | through engine {total:.1} ms | engine adds {:.1} ms",
            total - cable_a - cable_d
        );
        println!(
            "BACKLOG cable D with 3 s unread audio {} | cable D in the next session {}",
            fmt(&cable_d_backlog.map_err(|e| e.to_string())),
            fmt(&cable_d_after)
        );
        for s in summaries {
            println!("BRIDGE {s}");
        }
    }
}
