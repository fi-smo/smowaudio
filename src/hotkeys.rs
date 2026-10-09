//! Global keyboard shortcuts. Nothing is bound by default; the user binds actions in Settings.
//!
//! Action ids:
//! - `channel.<game|chat|media|aux|master>.<volume_up|volume_down|mute|eq>`
//! - `mic.<mute|push_to_talk|push_to_mute|gain_up|gain_down|monitor|denoise|low_latency|gate|eq|compressor|limiter>`
//! - `output.<next|previous>`, `app.<mixer|flyout>`, `windows_defaults`

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::Mutex;

use crate::app::AppHandle;

use crate::audio::device::{self, Flow};
use crate::config::CHANNEL_NAMES;
use crate::dsp::chain::ChannelSettings;
use crate::osd::{self, Osd};
use crate::{append_log, AppState};

const MAX_VOLUME: f32 = 1.5;
const MIC_GAIN_STEP_DB: f32 = 1.0;
const MIC_GAIN_LIMIT_DB: f32 = 24.0;
/// Holding a volume or gain shortcut keeps adjusting after this delay, like a held key repeats.
const REPEAT_DELAY: Duration = Duration::from_millis(350);
/// Fine steps while held: 1 % of volume, 1 dB of gain.
const FINE_VOLUME_STEP: f32 = 0.01;
const GAIN_REPEAT_INTERVAL: Duration = Duration::from_millis(80);
/// Stops a repeat whose key release was somehow missed.
const MAX_HOLD: Duration = Duration::from_secs(15);

/// Shortcut presses go to one worker thread: actions touch COM and may restart the output
/// stream, which must not happen on the UI thread, and presses must apply in order.
static WORKER: OnceLock<Sender<(String, bool)>> = OnceLock::new();
/// The actions each registered shortcut runs, by the shortcut's id.
static ACTIONS: LazyLock<Mutex<HashMap<u32, Vec<String>>>> = LazyLock::new(Default::default);
/// Shortcuts belong to the thread that registered them, whose message loop receives them: the
/// main thread.
static MAIN_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();

thread_local! {
    static MANAGER: RefCell<Option<(GlobalHotKeyManager, Vec<HotKey>)>> = const { RefCell::new(None) };
}

/// Starts the worker that runs shortcut actions and registers the bound shortcuts. Call on the
/// main thread.
pub fn start(app: &AppHandle) {
    let _ = MAIN_THREAD.set(std::thread::current().id());
    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
        let pressed = event.state() == HotKeyState::Pressed;
        if let (Some(worker), Some(actions)) = (WORKER.get(), ACTIONS.lock().get(&event.id())) {
            for action in actions {
                let _ = worker.send((action.clone(), pressed));
            }
        }
    }));
    let (tx, rx) = channel::<(String, bool)>();
    let _ = WORKER.set(tx);
    let handle = app.clone();
    let _ = std::thread::Builder::new().name("Shortcuts".into()).spawn(move || {
        let _com = crate::audio::ComGuard::new();
        register_all(&handle);
        let apply = |action: &str, pressed: bool, fine: bool| {
            let overlay = run(&handle, action, pressed, fine);
            if let Some(overlay) = &overlay {
                crate::notify_config_changed(&handle, "shortcut");
                osd::show(&handle, overlay.clone());
            }
            overlay.is_some()
        };
        // The volume/gain shortcut being held: (action, next repeat, first pressed).
        let mut held: Option<(String, Instant, Instant)> = None;
        loop {
            let message = match &held {
                Some((_, due, _)) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(message) => Some(message),
                    Err(_) => break,
                },
            };
            match message {
                Some((action, pressed)) => {
                    if !pressed && held.as_ref().is_some_and(|(a, _, _)| *a == action) {
                        held = None;
                    }
                    if apply(&action, pressed, false) && pressed && repeat_interval(&action, Duration::ZERO).is_some() {
                        let now = Instant::now();
                        held = Some((action, now + REPEAT_DELAY, now));
                    }
                }
                None => {
                    let Some((action, _, since)) = held.take() else { continue };
                    let Some(interval) = repeat_interval(&action, since.elapsed()) else { continue };
                    if since.elapsed() < MAX_HOLD && apply(&action, true, true) {
                        held = Some((action, Instant::now() + interval, since));
                    }
                }
            }
        }
    });
}

/// How often a held shortcut repeats, or None if it doesn't. Volume glides in 1 % increments at
/// 30 %/s, speeding up to 80 %/s over 1.5 s of holding, whatever step a single press uses.
fn repeat_interval(action: &str, held_for: Duration) -> Option<Duration> {
    if action.ends_with(".volume_up") || action.ends_with(".volume_down") {
        let ramp = (held_for.as_secs_f32() / 1.5).min(1.0);
        let per_second = 0.30 + 0.50 * ramp;
        Some(Duration::from_secs_f32(FINE_VOLUME_STEP / per_second).max(Duration::from_millis(12)))
    } else if action == "mic.gain_up" || action == "mic.gain_down" {
        Some(GAIN_REPEAT_INTERVAL)
    } else {
        None
    }
}

/// Runs `f` on the main thread and waits for its result (right away on the main thread).
fn on_main_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    if MAIN_THREAD.get() == Some(&std::thread::current().id()) {
        return f();
    }
    let (tx, rx) = channel();
    let _ = slint::invoke_from_event_loop(move || {
        let _ = tx.send(f());
    });
    rx.recv().expect("the event loop stopped")
}

/// Registers every bound shortcut, replacing the previous set, and returns why bindings failed
/// by action id. Any thread.
pub fn register_all(app: &AppHandle) -> BTreeMap<String, String> {
    let bindings = app.state().config.lock().hotkeys.clone();
    let errors = on_main_thread(move || register_now(bindings));
    *app.state().hotkey_errors.lock() = errors.clone();
    errors
}

fn register_now(bindings: BTreeMap<String, String>) -> BTreeMap<String, String> {
    unregister_now();
    let mut by_keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (action, keys) in bindings {
        by_keys.entry(keys).or_default().push(action);
    }
    MANAGER.with(|m| {
        let mut m = m.borrow_mut();
        if m.is_none() {
            match GlobalHotKeyManager::new() {
                Ok(manager) => *m = Some((manager, Vec::new())),
                Err(e) => append_log(&format!("keyboard shortcuts unavailable: {e}")),
            }
        }
    });
    let mut errors = BTreeMap::new();
    for (keys, actions) in by_keys {
        let result = HotKey::from_str(&keys).map_err(|e| e.to_string()).and_then(|hotkey| {
            MANAGER.with(|m| {
                let mut m = m.borrow_mut();
                let (manager, registered) = m.as_mut().ok_or("Keyboard shortcuts aren't available")?;
                manager.register(hotkey).map_err(|e| e.to_string())?;
                registered.push(hotkey);
                ACTIONS.lock().insert(hotkey.id(), actions.clone());
                Ok(())
            })
        });
        if let Err(message) = result {
            append_log(&format!("shortcut {keys} could not be registered: {message}"));
            let friendly = if message.to_lowercase().contains("register") {
                "Windows or another app already uses this shortcut".to_string()
            } else {
                message
            };
            for action in actions {
                errors.insert(action, friendly.clone());
            }
        }
    }
    errors
}

/// Frees every shortcut while the user records a new one, so pressing a bound combination
/// reaches Settings instead of running its action. Any thread.
pub fn unregister_all(_app: &AppHandle) {
    on_main_thread(unregister_now);
}

fn unregister_now() {
    MANAGER.with(|m| {
        if let Some((manager, registered)) = m.borrow_mut().as_mut() {
            let _ = manager.unregister_all(registered);
            registered.clear();
        }
    });
    ACTIONS.lock().clear();
}

/// Performs one action; `fine` is a repeat of a held volume shortcut, which moves 1 % at a time.
/// Returns the overlay describing the change, or None if nothing changed (or the action only
/// opens a window).
fn run(app: &AppHandle, action: &str, pressed: bool, fine: bool) -> Option<Osd> {
    let hold = matches!(action, "mic.push_to_talk" | "mic.push_to_mute");
    if !pressed && !hold {
        return None;
    }
    let state = app.state();
    let parts: Vec<&str> = action.split('.').collect();
    match parts.as_slice() {
        ["channel", name, op] => {
            let step = if fine { FINE_VOLUME_STEP } else { state.config.lock().volume_step };
            let edit = |s: &mut ChannelSettings| match *op {
                "volume_up" => s.volume = round_volume(s.volume + step).min(MAX_VOLUME),
                "volume_down" => s.volume = round_volume(s.volume - step).max(0.0),
                "mute" => s.muted = !s.muted,
                "eq" => s.eq.enabled = !s.eq.enabled,
                _ => {}
            };
            let (group, tape, settings) = if *name == "master" {
                let mut settings = state.config.lock().master.clone();
                edit(&mut settings);
                state.set_master_settings(settings.clone());
                ("master", "Master", settings)
            } else {
                let index = CHANNEL_NAMES.iter().position(|c| c.eq_ignore_ascii_case(name))?;
                let mut settings = state.config.lock().channels[index].settings.clone();
                edit(&mut settings);
                state.set_channel_settings(index, settings.clone());
                (["game", "chat", "media", "aux"][index], CHANNEL_NAMES[index], settings)
            };
            Some(match *op {
                "eq" => toggle(group, tape, "EQ", settings.eq.enabled),
                "volume_up" | "volume_down" | "mute" => Osd {
                    group,
                    tape: tape.into(),
                    label: "Volume".into(),
                    value: if settings.muted { "Muted".into() } else { format!("{:.0} %", settings.volume * 100.0) },
                    level: Some(settings.volume / MAX_VOLUME),
                    unity: Some(1.0 / MAX_VOLUME),
                    dim: settings.muted,
                },
                _ => return None,
            })
        }
        ["mic", op] => {
            let mut mic = state.config.lock().mic.clone();
            match *op {
                "mute" => mic.muted = !mic.muted,
                "push_to_talk" => mic.muted = !pressed,
                "push_to_mute" => mic.muted = pressed,
                "gain_up" => mic.gain_db = (mic.gain_db + MIC_GAIN_STEP_DB).min(MIC_GAIN_LIMIT_DB),
                "gain_down" => mic.gain_db = (mic.gain_db - MIC_GAIN_STEP_DB).max(-MIC_GAIN_LIMIT_DB),
                "monitor" => mic.monitor = !mic.monitor,
                "denoise" => mic.denoise.enabled = !mic.denoise.enabled,
                "low_latency" => mic.denoise.low_latency = !mic.denoise.low_latency,
                "gate" => mic.gate.enabled = !mic.gate.enabled,
                "eq" => mic.eq.enabled = !mic.eq.enabled,
                "compressor" => mic.compressor.enabled = !mic.compressor.enabled,
                "limiter" => mic.limiter.enabled = !mic.limiter.enabled,
                _ => return None,
            }
            state.set_mic_settings(mic.clone());
            Some(match *op {
                "mute" | "push_to_talk" | "push_to_mute" => Osd {
                    value: if mic.muted { "Muted".into() } else { "Live".into() },
                    dim: mic.muted,
                    ..toggle("mic", "Mic", "Microphone", !mic.muted)
                },
                "gain_up" | "gain_down" => Osd {
                    group: "mic",
                    tape: "Mic".into(),
                    label: "Gain".into(),
                    value: format!("{:+.0} dB", mic.gain_db),
                    level: Some((mic.gain_db + MIC_GAIN_LIMIT_DB) / (2.0 * MIC_GAIN_LIMIT_DB)),
                    unity: Some(0.5),
                    dim: mic.muted,
                },
                "monitor" => toggle("mic", "Mic", "Listen to yourself", mic.monitor),
                "denoise" => toggle("mic", "Mic", "Noise removal", mic.denoise.enabled),
                "low_latency" => toggle("mic", "Mic", "Low-latency model", mic.denoise.low_latency),
                "gate" => toggle("mic", "Mic", "Noise gate", mic.gate.enabled),
                "eq" => toggle("mic", "Mic", "EQ", mic.eq.enabled),
                "limiter" => toggle("mic", "Mic", "Limiter", mic.limiter.enabled),
                _ => toggle("mic", "Mic", "Compressor", mic.compressor.enabled),
            })
        }
        ["output", "next"] => cycle_output(&state, 1),
        ["output", "previous"] => cycle_output(&state, -1),
        ["app", "mixer"] => {
            let app = *app;
            let _ = slint::invoke_from_event_loop(move || crate::mainwin::open(&app, None));
            None
        }
        ["app", "flyout"] => {
            let app = *app;
            let _ = slint::invoke_from_event_loop(move || crate::app::toggle_flyout_at_tray(&app));
            None
        }
        ["windows_defaults"] => {
            let enabled = !state.config.lock().set_windows_defaults;
            if let Err(e) = crate::set_windows_defaults_enabled(&state, enabled) {
                append_log(&format!("shortcut could not change Windows default devices: {e}"));
                return None;
            }
            Some(toggle("system", "Windows", "Default devices", enabled))
        }
        _ => None,
    }
}

fn toggle(group: &'static str, tape: &str, label: &str, on: bool) -> Osd {
    Osd {
        group,
        tape: tape.into(),
        label: label.into(),
        value: if on { "On".into() } else { "Off".into() },
        level: None,
        unity: None,
        dim: !on,
    }
}

/// "Speakers (5- soundcore Select 4 Go )" -> "soundcore Select 4 Go", like the UI shows it.
fn device_label(name: &str) -> String {
    let inner = name.strip_suffix(')').and_then(|rest| rest.split_once(" (")).map(|(_, inner)| inner);
    let Some(inner) = inner else { return name.to_string() };
    let digits = inner.len() - inner.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let inner = if digits > 0 { inner[digits..].strip_prefix('-').unwrap_or(&inner[digits..]) } else { inner };
    match inner.trim() {
        "" => name.to_string(),
        label => label.to_string(),
    }
}

/// Two decimals, so repeated steps land on 55 % rather than 54.999 %.
fn round_volume(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// Moves the output to the next or previous connected headphones/speakers.
fn cycle_output(state: &AppState, direction: isize) -> Option<Osd> {
    let devices = device::list(Flow::Render).ok()?;
    let physical: Vec<_> = devices.into_iter().filter(|d| !d.is_virtual()).collect();
    if physical.is_empty() {
        return None;
    }
    let current = {
        let config = state.config.lock();
        device::resolve_physical(Flow::Render, config.output_device.as_deref(), config.previous_default(Flow::Render).as_deref())
            .ok()
            .and_then(|d| device::device_id(&d).ok())
    };
    let len = physical.len() as isize;
    let next = match current.and_then(|id| physical.iter().position(|d| d.id == id)) {
        Some(i) => (i as isize + direction).rem_euclid(len) as usize,
        None => 0,
    };
    let device = &physical[next];
    state.set_output(Some(device.id.clone()));
    Some(Osd {
        group: "output",
        tape: "Output".into(),
        label: String::new(),
        value: device_label(&device.name),
        level: None,
        unity: None,
        dim: false,
    })
}

#[cfg(test)]
mod tests {
    use super::device_label;

    #[test]
    fn device_label_shows_the_device_not_the_endpoint_type() {
        assert_eq!(device_label("Speakers (5- soundcore Select 4 Go )"), "soundcore Select 4 Go");
        assert_eq!(device_label("Headphones (Arctis Nova Pro Wireless)"), "Arctis Nova Pro Wireless");
        assert_eq!(device_label("CABLE-A Input (VB-Audio Cable A)"), "VB-Audio Cable A");
        assert_eq!(device_label("Plain name"), "Plain name");
    }
}
