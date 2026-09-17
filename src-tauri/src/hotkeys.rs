//! Global keyboard shortcuts. Nothing is bound by default; the user binds actions in Settings.
//!
//! Action ids:
//! - `channel.<game|chat|media|aux|master>.<volume_up|volume_down|mute|eq>`
//! - `mic.<mute|push_to_talk|push_to_mute|gain_up|gain_down|monitor|denoise|low_latency|gate|eq|compressor>`
//! - `output.<next|previous>`, `app.<mixer|flyout>`, `windows_defaults`

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::audio::device::{self, Flow};
use crate::config::CHANNEL_NAMES;
use crate::dsp::chain::ChannelSettings;
use crate::{append_log, AppState};

const MAX_VOLUME: f32 = 1.5;
const MIC_GAIN_STEP_DB: f32 = 1.0;
const MIC_GAIN_LIMIT_DB: f32 = 24.0;

/// Shortcut presses go to one worker thread: actions touch COM and may restart the output
/// stream, which must not happen on the UI thread, and presses must apply in order.
static WORKER: OnceLock<Sender<(String, bool)>> = OnceLock::new();

pub fn start(app: &AppHandle) {
    let (tx, rx) = channel::<(String, bool)>();
    let _ = WORKER.set(tx);
    let handle = app.clone();
    let _ = std::thread::Builder::new().name("Shortcuts".into()).spawn(move || {
        let _com = crate::audio::ComGuard::new();
        // Registering waits for the UI thread, so it can't happen during setup, which runs on it.
        register_all(&handle);
        for (action, pressed) in rx {
            if run(&handle, &action, pressed) {
                let _ = handle.emit("config-changed", ());
            }
        }
    });
}

/// Registers every bound shortcut, replacing the previous set, and returns why bindings failed
/// by action id. Blocks on the UI thread, so never call it from there.
pub fn register_all(app: &AppHandle) -> BTreeMap<String, String> {
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();
    let state = app.state::<AppState>();
    let bindings = state.config.lock().hotkeys.clone();

    let mut by_keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (action, keys) in bindings {
        by_keys.entry(keys).or_default().push(action);
    }
    let mut errors = BTreeMap::new();
    for (keys, actions) in by_keys {
        let targets = actions.clone();
        let result = shortcuts.on_shortcut(keys.as_str(), move |_, _, event| {
            let pressed = event.state() == ShortcutState::Pressed;
            if let Some(worker) = WORKER.get() {
                for action in &targets {
                    let _ = worker.send((action.clone(), pressed));
                }
            }
        });
        if let Err(e) = result {
            let message = e.to_string();
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
    *state.hotkey_errors.lock() = errors.clone();
    errors
}

/// Frees every shortcut while the user records a new one, so pressing a bound combination
/// reaches the Settings page instead of running its action.
pub fn unregister_all(app: &AppHandle) {
    let _ = app.global_shortcut().unregister_all();
}

/// Performs one action. Returns true if settings changed and the windows should refresh.
fn run(app: &AppHandle, action: &str, pressed: bool) -> bool {
    let hold = matches!(action, "mic.push_to_talk" | "mic.push_to_mute");
    if !pressed && !hold {
        return false;
    }
    let state = app.state::<AppState>();
    let parts: Vec<&str> = action.split('.').collect();
    match parts.as_slice() {
        ["channel", name, op] => {
            let step = state.config.lock().volume_step;
            let edit = |s: &mut ChannelSettings| match *op {
                "volume_up" => s.volume = round_volume(s.volume + step).min(MAX_VOLUME),
                "volume_down" => s.volume = round_volume(s.volume - step).max(0.0),
                "mute" => s.muted = !s.muted,
                "eq" => s.eq.enabled = !s.eq.enabled,
                _ => {}
            };
            if *name == "master" {
                let mut settings = state.config.lock().master.clone();
                edit(&mut settings);
                state.set_master_settings(settings);
            } else if let Some(index) = CHANNEL_NAMES.iter().position(|c| c.eq_ignore_ascii_case(name)) {
                let mut settings = state.config.lock().channels[index].settings.clone();
                edit(&mut settings);
                state.set_channel_settings(index, settings);
            } else {
                return false;
            }
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
                _ => return false,
            }
            state.set_mic_settings(mic);
        }
        ["output", "next"] => return cycle_output(&state, 1),
        ["output", "previous"] => return cycle_output(&state, -1),
        ["app", "mixer"] => {
            crate::show_main(app, None);
            return false;
        }
        ["app", "flyout"] => {
            crate::toggle_flyout_at_tray(app);
            return false;
        }
        ["windows_defaults"] => {
            let enabled = !state.config.lock().set_windows_defaults;
            if let Err(e) = crate::set_windows_defaults_enabled(&state, enabled) {
                append_log(&format!("shortcut could not change Windows default devices: {e}"));
            }
        }
        _ => return false,
    }
    true
}

/// Two decimals, so repeated steps land on 55 % rather than 54.999 %.
fn round_volume(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// Moves the output to the next or previous connected headphones/speakers.
fn cycle_output(state: &AppState, direction: isize) -> bool {
    let Ok(devices) = device::list(Flow::Render) else { return false };
    let physical: Vec<_> = devices.into_iter().filter(|d| !d.is_virtual()).collect();
    if physical.is_empty() {
        return false;
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
    state.set_output(Some(physical[next].id.clone()));
    true
}
