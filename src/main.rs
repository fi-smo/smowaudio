#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod dsp;
mod engine;
mod eqedit;
mod flyout;
mod mainwin;
mod settingsui;
/// The native (Slint) windows, compiled from ui/app.slint by build.rs.
mod ui {
    slint::include_modules!();
}
mod hotkeys;
mod icons;
mod instance;
mod osd;
mod updates;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use app::AppHandle;

use audio::device::{self, Flow};
use audio::routing;
use audio::defaults::WindowsDefaults;
use config::{cable_partner, Config};
use dsp::chain::{ChannelSettings, MicSettings};
use engine::Engine;

struct AppState {
    config: Mutex<Config>,
    engine: Mutex<Option<Engine>>,
    dirty: Arc<AtomicBool>,
    /// When the flyout last hid itself on losing focus, so the same tray click doesn't reopen it.
    flyout_hidden_at: Mutex<Option<Instant>>,
    /// Shortcuts Windows refused to register, by action id (shown next to them in Settings).
    hotkey_errors: Mutex<BTreeMap<String, String>>,
    /// Devices the streams were last set up for; the device watcher restarts them when it changes.
    device_fingerprint: Mutex<config::DeviceFingerprint>,
    updates: updates::Updates,
    /// The device the output is playing to right now, as the engine reported it.
    playing_output: Mutex<Option<String>>,
    /// Its name, for the tray icon's tooltip.
    playing_output_name: Mutex<Option<String>>,
    /// What the tray shows now: (mic muted, tooltip), so it's only redrawn when that changes.
    tray_shown: Mutex<(bool, String)>,
}

impl AppState {
    /// Mutates config and schedules a save (coalesced so slider drags don't hammer the disk).
    fn update(&self, f: impl FnOnce(&mut Config)) -> Config {
        let mut config = self.config.lock();
        f(&mut config);
        self.dirty.store(true, Ordering::Relaxed);
        config.clone()
    }

    fn set_channel_settings(&self, index: usize, settings: ChannelSettings) {
        self.update(|c| c.channels[index].settings = settings.clone());
        if let Some(e) = self.engine.lock().as_ref() {
            e.set_channel(index, settings);
        }
    }

    fn set_master_settings(&self, settings: ChannelSettings) {
        let playing = self.playing_output.lock().clone();
        self.update(|c| {
            c.master = settings.clone();
            if let (true, Some(id)) = (c.master_per_output, playing) {
                c.output_masters.insert(id, config::OutputMaster { volume: settings.volume, eq: settings.eq.clone() });
            }
        });
        if let Some(e) = self.engine.lock().as_ref() {
            e.set_master(settings);
        }
    }

    fn set_mic_settings(&self, settings: MicSettings) {
        self.update(|c| c.mic = settings.clone());
        if let Some(e) = self.engine.lock().as_ref() {
            e.set_mic(settings);
        }
    }

    /// Switches headphones/speakers right away, reopening only the output stream. Needs COM.
    fn set_output(&self, output: Option<String>) {
        let config = self.update(|c| c.output_device = output.clone());
        if let Some(e) = self.engine.lock().as_ref() {
            e.set_output(output);
        }
        // Not a device change: keep the watcher from restarting every stream over it.
        *self.device_fingerprint.lock() = config.device_fingerprint();
    }

    /// The output now plays to `id`: bring back the master volume and EQ it last had (or remember
    /// the current ones for it). Returns true when the master changed.
    fn output_opened(&self, id: String) -> bool {
        {
            let mut playing = self.playing_output.lock();
            if playing.as_deref() == Some(id.as_str()) {
                return false;
            }
            *playing = Some(id.clone());
        }
        let config = self.config.lock().clone();
        if !config.master_per_output {
            return false;
        }
        match config.output_masters.get(&id) {
            Some(saved) if saved.volume != config.master.volume || saved.eq != config.master.eq => {
                self.set_master_settings(ChannelSettings { volume: saved.volume, eq: saved.eq.clone(), muted: config.master.muted });
                true
            }
            Some(_) => false,
            None => {
                self.update(|c| {
                    let remembered = config::OutputMaster { volume: c.master.volume, eq: c.master.eq.clone() };
                    c.output_masters.insert(id, remembered);
                });
                false
            }
        }
    }

    fn restart_engine(&self) {
        let config = self.config.lock().clone();
        let mut engine = self.engine.lock();
        if let Some(old) = engine.take() {
            old.stop();
        }
        *engine = Some(Engine::start(&config));
        drop(engine);
        *self.device_fingerprint.lock() = config.device_fingerprint();
    }
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize, Clone)]
pub(crate) struct DelayResult {
    /// Channel index (see config::CHANNEL_NAMES).
    pub channel: usize,
    pub cable_ms: Option<f64>,
    pub engine_ms: Option<f64>,
    pub error: Option<String>,
}

/// The measurement itself; `progress` gets each channel's index before it's measured. Needs COM.
pub(crate) fn measure_delay_now(state: &AppState, progress: impl Fn(usize)) -> CmdResult<Vec<DelayResult>> {
    {
        let config = state.config.lock().clone();
        let running = state.engine.lock().as_ref().map(|e| e.shared.status.lock().get("Output").cloned());
        if running.flatten().as_deref() != Some("running") {
            return Err("Audio output isn't running, so there's nothing to measure. Check the stream status above.".to_string());
        }
        let output = device::resolve_physical(Flow::Render, config.output_device.as_deref(), config.previous_default(Flow::Render).as_deref())
            .and_then(|d| device::device_id(&d))
            .map_err(|e| format!("No output device: {e}"))?;
        let master_silent = config.master.muted || config.master.volume <= 0.0;
        let mut results = Vec::new();
        for (i, channel) in config.channels.iter().enumerate() {
            let (Some(sink), Some(source)) = (channel.sink.as_deref(), channel.source.as_deref()) else { continue };
            let blocked = if master_silent {
                Some("Master is muted or at 0, so nothing reaches your output.")
            } else if channel.settings.muted || channel.settings.volume <= 0.0 {
                Some("This channel is muted or at 0. Unmute it to measure.")
            } else {
                None
            };
            let result = match blocked {
                Some(reason) => DelayResult { channel: i, cable_ms: None, engine_ms: None, error: Some(reason.into()) },
                None => {
                    progress(i);
                    match audio::latency::measure(sink, source, &output) {
                        Ok(d) => DelayResult { channel: i, cable_ms: Some(d.cable_ms), engine_ms: Some(d.engine_ms), error: None },
                        Err(e) => DelayResult { channel: i, cable_ms: None, engine_ms: None, error: Some(format!("{e:#}")) },
                    }
                }
            };
            append_log(&format!(
                "delay {}: cable {:?} ms, smowaudio+windows {:?} ms{}",
                config::CHANNEL_NAMES[i],
                result.cable_ms.map(|v| v.round()),
                result.engine_ms.map(|v| v.round()),
                result.error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default()
            ));
            results.push(result);
        }
        Ok(results)
    }
}

/// The changelog this version was built with (Markdown, newest version first).
pub(crate) const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// Tells the windows that settings changed, so they show it right away, and updates the tray.
/// `source` is what made the change ("flyout", "main-native", "shortcut", "output"); that window
/// skips the redraw, so a fader being dragged doesn't jump under the cursor. Any thread.
fn notify_config_changed(app: &AppHandle, source: &str) {
    let (handle, source) = (*app, source.to_string());
    let _ = slint::invoke_from_event_loop(move || {
        flyout::config_changed(&handle, &source);
        mainwin::config_changed(&handle, &source);
    });
    refresh_tray(app);
}

/// Shows the mic's mute on the tray icon, and the output, master and mic in its tooltip.
fn refresh_tray(app: &AppHandle) {
    let state = app.state();
    let (muted, master) = {
        let config = state.config.lock();
        (config.mic.muted, config.master.clone())
    };
    let mic = state.engine.lock().as_ref().and_then(|e| e.shared.status.lock().get("Microphone").cloned());
    let mic = match mic.as_deref() {
        None => None,
        Some("running") if muted => Some("Mic muted"),
        Some("running") => Some("Mic on"),
        Some(_) if muted => Some("Mic muted (not connected)"),
        Some(_) => Some("Mic off"),
    };
    let master = if master.muted { "Master muted".to_string() } else { format!("Master {} %", (master.volume * 100.0).round()) };
    let mut tooltip = String::from("Smowaudio");
    if let Some(output) = state.playing_output_name.lock().as_deref() {
        tooltip += &format!("\n{output}");
    }
    tooltip += &format!("\n{master}");
    if let Some(mic) = mic {
        tooltip += &format!(" · {mic}");
    }
    // Windows cuts tray tooltips off at 127 characters.
    let tooltip: String = tooltip.chars().take(127).collect();
    {
        let mut shown = state.tray_shown.lock();
        if *shown == (muted, tooltip.clone()) {
            return;
        }
        *shown = (muted, tooltip.clone());
    }
    app::set_tray(muted, tooltip);
}

/// Stops and restarts every stream, which takes a moment. Needs COM.
pub(crate) fn set_devices_now(
    state: &AppState,
    output: Option<String>,
    mic: Option<String>,
    mic_sink: Option<String>,
    sources: Vec<Option<String>>,
) {
    let sinks: Vec<Option<String>> = sources.iter().map(|s| s.as_deref().and_then(cable_partner)).collect();
    state.update(|c| {
        c.output_device = output;
        c.mic_device = mic;
        c.mic_sink = mic_sink;
        for (i, channel) in c.channels.iter_mut().enumerate() {
            channel.source = sources.get(i).cloned().flatten();
            channel.sink = sinks.get(i).cloned().flatten();
        }
    });
    {
        let mut config = state.config.lock();
        if config.set_windows_defaults {
            apply_windows_defaults(&mut config);
        }
    }
    state.restart_engine();
}

/// Sonar-style Windows defaults. Remembers the user's own defaults the first time, then points
/// Windows at the Game, Chat and Virtual Mic cables. Returns true if the config changed.
fn apply_windows_defaults(config: &mut Config) -> bool {
    let targets = config.windows_default_targets();
    let mut config_changed = false;
    if config.previous_defaults.is_none() {
        // Only the user's own devices: if the cables are already the defaults, there's nothing
        // of theirs to remember.
        config.previous_defaults = Some(WindowsDefaults::read().without_virtual());
        config_changed = true;
    }
    match targets.apply() {
        Ok(0) => {}
        Ok(n) => append_log(&format!("set {n} Windows default device(s): {targets:?}")),
        Err(e) => append_log(&format!("setting Windows default devices failed: {e:#}")),
    }
    config_changed
}

/// Turns Sonar-style Windows defaults on, or off by restoring the user's own. Needs COM.
pub(crate) fn set_windows_defaults_enabled(state: &AppState, enabled: bool) -> CmdResult<()> {
    {
        let mut config = state.config.lock();
        config.set_windows_defaults = enabled;
        if enabled {
            apply_windows_defaults(&mut config);
        } else {
            // The user's own defaults from before, or else the devices Smowaudio plays to and
            // records from, so Windows never stays on a cable.
            let previous = config.previous_defaults.take().unwrap_or_default().without_virtual();
            let physical = |flow: Flow, id: &Option<String>| {
                device::resolve_physical(flow, id.as_deref(), None).ok().and_then(|d| device::device_id(&d).ok())
            };
            let output = physical(Flow::Render, &config.output_device);
            let mic = physical(Flow::Capture, &config.mic_device);
            let restore = WindowsDefaults {
                playback: previous.playback.or_else(|| output.clone()),
                playback_communications: previous.playback_communications.or(output),
                recording: previous.recording.or_else(|| mic.clone()),
                recording_communications: previous.recording_communications.or(mic),
            };
            restore.apply().map_err(err)?;
            append_log(&format!("restored Windows default devices: {restore:?}"));
        }
    }
    state.dirty.store(true, Ordering::Relaxed);
    Ok(())
}

pub(crate) fn assign_app_now(state: &AppState, pid: u32, exe: String, channel: Option<usize>) -> CmdResult<()> {
    let config = state.update(|c| match channel {
        Some(ch) => {
            c.app_rules.insert(exe.clone(), ch);
        }
        None => {
            c.app_rules.remove(&exe);
        }
    });
    let sink = channel.and_then(|ch| config.channels[ch].sink.clone());
    if channel.is_some() && sink.is_none() {
        return Err("That channel has no virtual cable assigned yet.".into());
    }
    routing::set_app_output(pid, sink.as_deref()).map_err(err)?;
    if let Some(e) = state.engine.lock().as_ref() {
        e.set_rules(config.routing_rules());
    }
    Ok(())
}

/// Runs PowerShell, which takes about a second.
pub(crate) fn set_launch_at_login_now(state: &AppState, enabled: bool) -> CmdResult<()> {
    let script = if enabled {
        let exe = std::env::current_exe().map_err(err)?.display().to_string().replace('\'', "''");
        format!(
            r#"$user = "$env:USERDOMAIN\$env:USERNAME"
$action = New-ScheduledTaskAction -Execute '{exe}' -Argument '--background'
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -Priority 4 -StartWhenAvailable
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName 'Smowaudio' -Description 'Starts Smowaudio in the tray at sign-in' -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force -ErrorAction Stop | Out-Null"#
        )
    } else {
        "Unregister-ScheduledTask -TaskName 'Smowaudio' -Confirm:$false -ErrorAction SilentlyContinue".to_string()
    };
    run_powershell(&script)?;
    // The app used to be called AudioManager; its task would start an exe that no longer exists.
    let _ = run_powershell("Unregister-ScheduledTask -TaskName 'AudioManager' -Confirm:$false -ErrorAction SilentlyContinue");
    remove_legacy_run_entry();
    let exe = std::env::current_exe().ok().map(|p| p.display().to_string());
    state.update(|c| {
        c.launch_at_login = enabled;
        c.autostart_exe = if enabled { exe } else { None };
    });
    Ok(())
}

fn run_powershell(script: &str) -> CmdResult<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(err)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Older builds registered a Run-key entry; remove it so the app can't be started twice.
fn remove_legacy_run_entry() {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    if let Ok(run) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE)
    {
        let _ = run.delete_value("AudioManager");
    }
}

/// Whether Windows apps use the light theme.
fn apps_use_light_theme() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        .map_or(true, |v| v != 0)
}

/// Appends a timestamped line to %APPDATA%\Smowaudio\smowaudio.log. The release build has
/// no console, so this is where stream errors and panics leave a trace.
/// The log is kept to about this size: past it, it becomes smowaudio.old.log (replacing the
/// previous one) and a new file starts, so there's always recent history and never much more.
const LOG_LIMIT_BYTES: u64 = 1024 * 1024;

pub(crate) fn append_log(message: &str) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("Smowaudio")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("smowaudio.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT_BYTES) {
        let _ = std::fs::rename(&path, dir.join("smowaudio.old.log"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        // One write per line so lines from different threads don't interleave.
        let _ = file.write_all(format!("[{}] {message}\n", local_timestamp()).as_bytes());
    }
}

/// Local time like "2026-09-25 18:40:31.207", as the clock in the taskbar shows it.
fn local_timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// Logs panics from any thread (an audio stream, say) before the default handler runs.
fn install_panic_log() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        append_log(&format!("panic in thread '{}': {info}", thread.name().unwrap_or("unnamed")));
        default_hook(info);
    }));
}

fn main() {
    install_panic_log();

    // A second launch must not open a second set of audio streams: it hands over to the running
    // copy (which shows its window) and exits.
    if instance::another_running() {
        instance::hand_over();
        return;
    }

    // Audio first: the engine is running before any window exists.
    let config = {
        let _com = audio::ComGuard::new();
        let mut config = Config::load();
        // Older versions could remember a cable as the user's "own" default; forget those.
        if let Some(previous) = config.previous_defaults.clone() {
            let cleaned = previous.clone().without_virtual();
            if cleaned != previous {
                append_log(&format!("forgot virtual devices remembered as your own Windows defaults: {previous:?}"));
                config.previous_defaults = Some(cleaned);
                let _ = config.save();
            }
        }
        append_log(&format!("started pid {} with {}", std::process::id(), config.describe()));
        if config.set_windows_defaults && apply_windows_defaults(&mut config) {
            let _ = config.save();
        }
        config
    };
    let engine = Some(Engine::start(&config));
    let dirty = Arc::new(AtomicBool::new(false));
    let state = AppState {
        config: Mutex::new(config),
        engine: Mutex::new(engine),
        dirty: dirty.clone(),
        flyout_hidden_at: Mutex::new(None),
        hotkey_errors: Mutex::new(BTreeMap::new()),
        device_fingerprint: Mutex::new(Default::default()),
        updates: updates::Updates::default(),
        playing_output: Mutex::new(None),
        playing_output_name: Mutex::new(None),
        tray_shown: Mutex::new((false, String::new())),
    };

    if STATE.set(state).is_err() {
        unreachable!("the state is set once");
    }
    let app = AppHandle;

    // Later launches open the window, or with `--flyout` toggle the tray flyout (for keyboard
    // launchers and testing).
    instance::listen(move |args| {
        let _ = slint::invoke_from_event_loop(move || {
            if args.iter().any(|a| a == "--flyout") {
                app::toggle_flyout_at_tray(&app);
            } else {
                mainwin::open(&app, None);
            }
        });
    });

    updates::start_background_checks(&app);
    // After an update installs the app somewhere new (or the first installer run), point the
    // sign-in task at the exe that's running. Release builds only: a dev build would steal it.
    if cfg!(not(debug_assertions)) {
        std::thread::spawn(move || {
            let state = app.state();
            let current = std::env::current_exe().ok().map(|p| p.display().to_string());
            let (enabled, registered) = {
                let config = state.config.lock();
                (config.launch_at_login, config.autostart_exe.clone())
            };
            if enabled && current.is_some() && registered != current {
                match set_launch_at_login_now(state, true) {
                    Ok(()) => append_log(&format!("sign-in task now starts {}", current.unwrap_or_default())),
                    Err(e) => append_log(&format!("updating the sign-in task failed: {e}")),
                }
            }
        });
    }

    // Each output device keeps its own master volume and EQ.
    engine::on_output_opened(move |id| {
        std::thread::spawn(move || {
            let state = app.state();
            {
                let _com = audio::ComGuard::new();
                let name = device::by_id(&id).and_then(|d| device::friendly_name(&d)).ok();
                *state.playing_output_name.lock() = name;
            }
            if state.output_opened(id) {
                append_log("master volume and EQ switched to the output device's own");
                notify_config_changed(&app, "output");
            } else {
                refresh_tray(&app);
            }
        });
    });

    watch_devices(app);

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if dirty.swap(false, Ordering::Relaxed) {
            let config = app.state().config.lock().clone();
            if let Err(e) = config.save() {
                log::error!("saving config: {e:#}");
            }
        }
    });

    // Windows, the tray and shortcuts all live on this thread from here on. Device names and
    // lists need COM on it too.
    let _com = audio::ComGuard::new();
    let result = app::run(app, move || {
        hotkeys::start(&app);
        refresh_tray(&app);
        if !std::env::args().any(|a| a == "--background") {
            mainwin::open(&app, None);
        }
    });
    if let Err(e) = result {
        append_log(&format!("the windows couldn't start: {e}"));
    }
    shutdown();
}

static STATE: std::sync::OnceLock<AppState> = std::sync::OnceLock::new();

/// The app's state; set before anything else runs.
fn state() -> &'static AppState {
    STATE.get().expect("the app state is set at start")
}

/// Saves the settings and stops the audio, before the process exits ("Quit", or an update).
fn shutdown() {
    let state = state();
    let _ = state.config.lock().save();
    let engine = state.engine.lock().take();
    if let Some(engine) = engine {
        engine.stop();
    }
}

/// Restarts streams when the devices they depend on change (headset plugged in, mic turned on,
/// Windows default changed while following it).
fn watch_devices(app: AppHandle) {
    std::thread::spawn(move || {
        let _com = audio::ComGuard::new();
        {
            let state = app.state();
            let now = state.config.lock().device_fingerprint();
            *state.device_fingerprint.lock() = now;
        }
        let result = audio::notify::watch(
            || true,
            || {
                let state = app.state();
                let now = state.config.lock().device_fingerprint();
                let before = state.device_fingerprint.lock().clone();
                if now == before {
                    return;
                }
                // A cable coming or going changes which streams exist: start over. The output or
                // the mic only needs its own stream reopened, so a mic being switched on doesn't
                // interrupt what's playing.
                if now.cables != before.cables {
                    append_log(&format!("audio devices changed ({now}); restarting streams"));
                    state.restart_engine();
                    return;
                }
                if let Some(engine) = state.engine.lock().as_ref() {
                    if now.output != before.output {
                        append_log(&format!("output device changed ({}); reopening the output", now.output));
                        engine.reopen_output();
                    }
                    if now.mic != before.mic {
                        append_log(&format!("microphone changed ({}); reopening the mic", now.mic));
                        engine.reopen_mic();
                    }
                }
                *state.device_fingerprint.lock() = now;
                // The mic reports whether it started a moment later; update the tray then.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    refresh_tray(&app);
                });
            },
        );
        if let Err(e) = result {
            append_log(&format!("device change notifications unavailable: {e:#}"));
        }
    });
}
