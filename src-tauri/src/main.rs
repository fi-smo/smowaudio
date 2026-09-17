#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod dsp;
mod engine;
mod hotkeys;
mod icons;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use audio::device::{self, DeviceInfo, Flow};
use audio::routing::{self, AudioApp};
use audio::defaults::WindowsDefaults;
use config::{cable_partner, Config};
use dsp::chain::{ChannelSettings, MicSettings};
use engine::{Engine, Meters};

struct AppState {
    config: Mutex<Config>,
    engine: Mutex<Option<Engine>>,
    dirty: Arc<AtomicBool>,
    /// Screen the main window should open on (set by the tray flyout before it exists).
    pending_view: Mutex<Option<String>>,
    /// When the flyout last hid itself on losing focus, so the same tray click doesn't reopen it.
    flyout_hidden_at: Mutex<Option<Instant>>,
    /// Shortcuts Windows refused to register, by action id (shown next to them in Settings).
    hotkey_errors: Mutex<BTreeMap<String, String>>,
    /// Devices the streams were last set up for; the device watcher restarts them when it changes.
    device_fingerprint: Mutex<String>,
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
        self.update(|c| c.master = settings.clone());
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

#[derive(Serialize)]
struct Snapshot {
    config: Config,
    render_devices: Vec<DeviceInfo>,
    capture_devices: Vec<DeviceInfo>,
    status: std::collections::HashMap<String, String>,
    hotkey_errors: BTreeMap<String, String>,
}

#[tauri::command]
fn get_state(state: State<AppState>) -> CmdResult<Snapshot> {
    let _com = audio::ComGuard::new();
    let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().clone()).unwrap_or_default();
    Ok(Snapshot {
        config: state.config.lock().clone(),
        render_devices: device::list(Flow::Render).map_err(err)?,
        capture_devices: device::list(Flow::Capture).map_err(err)?,
        status,
        hotkey_errors: state.hotkey_errors.lock().clone(),
    })
}

#[tauri::command]
fn get_meters(state: State<AppState>) -> Meters {
    state.engine.lock().as_ref().map(|e| e.meters()).unwrap_or_default()
}

#[tauri::command]
fn set_master(state: State<AppState>, settings: ChannelSettings) {
    state.set_master_settings(settings);
}

/// Async on purpose: creating a window from a synchronous command can deadlock WebView2 on
/// Windows, leaving a blank white window (tauri-apps/wry#583).
#[tauri::command]
async fn open_main_window(app: AppHandle, view: Option<String>) {
    show_main(&app, view.as_deref());
}

/// The screen a freshly created main window should show, if the flyout asked for one.
#[tauri::command]
fn take_pending_view(state: State<AppState>) -> Option<String> {
    state.pending_view.lock().take()
}

#[tauri::command]
fn set_mic(state: State<AppState>, settings: MicSettings) {
    state.set_mic_settings(settings);
}

#[tauri::command]
fn set_channel(state: State<AppState>, index: usize, settings: ChannelSettings) -> CmdResult<()> {
    if index >= config::CHANNEL_COUNT {
        return Err("invalid channel".into());
    }
    state.set_channel_settings(index, settings);
    Ok(())
}

/// Switches headphones/speakers from the tray flyout without restarting the other streams.
#[tauri::command]
async fn set_output_device(app: AppHandle, output: Option<String>) {
    let _com = audio::ComGuard::new();
    app.state::<AppState>().set_output(output);
    let _ = app.emit("config-changed", ());
}

/// Binds (or with `None` unbinds) a global shortcut and re-registers them all. A combination
/// moves over from any action that had it. Returns the shortcuts Windows refused, by action id.
#[tauri::command]
async fn set_hotkey(app: AppHandle, action: String, keys: Option<String>) -> BTreeMap<String, String> {
    app.state::<AppState>().update(|c| match keys {
        Some(keys) => {
            c.hotkeys.retain(|_, bound| *bound != keys);
            c.hotkeys.insert(action, keys);
        }
        None => {
            c.hotkeys.remove(&action);
        }
    });
    hotkeys::register_all(&app)
}

/// Suspends shortcuts while Settings records a key combination, then restores them.
#[tauri::command]
async fn pause_hotkeys(app: AppHandle, paused: bool) -> BTreeMap<String, String> {
    if paused {
        hotkeys::unregister_all(&app);
        BTreeMap::new()
    } else {
        hotkeys::register_all(&app)
    }
}

#[tauri::command]
fn set_volume_step(state: State<AppState>, step: f32) {
    state.update(|c| c.volume_step = step.clamp(0.01, 0.25));
}

#[tauri::command]
fn set_devices(
    state: State<AppState>,
    output: Option<String>,
    mic: Option<String>,
    mic_sink: Option<String>,
    sources: Vec<Option<String>>,
) {
    let _com = audio::ComGuard::new();
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
        config.previous_defaults = Some(WindowsDefaults::read());
        config_changed = true;
    }
    match targets.apply() {
        Ok(0) => {}
        Ok(n) => append_log(&format!("set {n} Windows default device(s): {targets:?}")),
        Err(e) => append_log(&format!("setting Windows default devices failed: {e:#}")),
    }
    config_changed
}

#[tauri::command]
fn set_windows_defaults(state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let _com = audio::ComGuard::new();
    set_windows_defaults_enabled(&state, enabled)
}

/// Turns Sonar-style Windows defaults on, or off by restoring the user's own. Needs COM.
fn set_windows_defaults_enabled(state: &AppState, enabled: bool) -> CmdResult<()> {
    {
        let mut config = state.config.lock();
        config.set_windows_defaults = enabled;
        if enabled {
            apply_windows_defaults(&mut config);
        } else if let Some(previous) = config.previous_defaults.take() {
            previous.apply().map_err(err)?;
            append_log("restored the Windows default devices from before Smowaudio");
        }
    }
    state.dirty.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
fn list_apps() -> CmdResult<Vec<AudioApp>> {
    let _com = audio::ComGuard::new();
    routing::list_apps().map_err(err)
}

/// The app's own icon as a PNG data URL, extracted off the UI thread and cached.
#[tauri::command]
async fn app_icon(path: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || icons::app_icon(&path)).await.ok().flatten()
}

/// Routes an app to a channel (or back to the Windows default with `None`) and remembers it.
#[tauri::command]
fn assign_app(state: State<AppState>, pid: u32, exe: String, channel: Option<usize>) -> CmdResult<()> {
    let _com = audio::ComGuard::new();
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

/// Starts Smowaudio in the tray at sign-in through a Task Scheduler task. Task Scheduler
/// launches immediately at logon, while Explorer's Run-key startup can skip entries entirely.
#[tauri::command]
fn set_launch_at_login(state: State<AppState>, enabled: bool) -> CmdResult<()> {
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
    state.update(|c| c.launch_at_login = enabled);
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

/// True if another Smowaudio process already holds the instance mutex.
/// The handle is intentionally leaked so the mutex lives as long as this process.
fn another_instance_running() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe { CreateMutexW(None, false, w!("Local\\Smowaudio.SingleInstance")).is_ok() && GetLastError() == ERROR_ALREADY_EXISTS }
}

fn open_window(app: &AppHandle) {
    show_main(app, None);
}

/// `--window` from style.css for the current Windows app theme.
fn window_background() -> tauri::window::Color {
    let light = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        .map_or(true, |v| v != 0);
    if light {
        tauri::window::Color(0xf6, 0xf7, 0xf9, 0xff)
    } else {
        tauri::window::Color(0x16, 0x18, 0x1d, 0xff)
    }
}

/// Shows the main window (creating it if needed), optionally on a given screen.
fn show_main(app: &AppHandle, view: Option<&str>) {
    if let Some(flyout) = app.get_webview_window("flyout") {
        let _ = flyout.hide();
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Some(view) = view {
            let _ = window.emit("show-view", view);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    *app.state::<AppState>().pending_view.lock() = view.map(str::to_string);
    // Created on demand and destroyed on close, so WebView2 only uses memory while visible.
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Smowaudio")
        .inner_size(1180.0, 760.0)
        .min_inner_size(440.0, 520.0)
        // The page's own background, so the window doesn't flash white while WebView2 starts.
        .background_color(window_background())
        // Tauri's file-drop handler swallows HTML drag and drop on Windows; the Apps view needs it.
        .disable_drag_drop_handler()
        .build();
    if let Ok(window) = window {
        let handle = app.clone();
        window.on_window_event(move |event| {
            // Closing the window while Settings records a shortcut would leave them all suspended.
            // Registering waits for the UI thread, which is the one running this handler.
            if let tauri::WindowEvent::Destroyed = event {
                let handle = handle.clone();
                std::thread::spawn(move || {
                    hotkeys::register_all(&handle);
                });
            }
        });
    }
}

/// Toggles the flyout above the tray icon, for the shortcut (there's no click to place it by).
fn toggle_flyout_at_tray(app: &AppHandle) {
    let anchor = app.tray_by_id("tray").and_then(|tray| tray.rect().ok().flatten()).map(|rect| {
        let position = rect.position.to_physical::<f64>(1.0);
        let size = rect.size.to_physical::<f64>(1.0);
        PhysicalPosition::new(position.x + size.width / 2.0, position.y)
    });
    let anchor = anchor.or_else(|| app.cursor_position().ok()).unwrap_or_default();
    toggle_flyout(app, anchor);
}

/// Starting size; the flyout page reports its real content height through `fit_flyout`.
const FLYOUT_SIZE: (f64, f64) = (360.0, 330.0);

/// Sizes the flyout to its content (a CSS height), keeping its bottom edge above the taskbar.
#[tauri::command]
fn fit_flyout(window: WebviewWindow, height: f64) {
    let (Ok(scale), Ok(position), Ok(size)) = (window.scale_factor(), window.outer_position(), window.outer_size())
    else {
        return;
    };
    let new_height = (height * scale).round().max(1.0) as u32;
    if new_height == size.height {
        return;
    }
    let bottom = position.y + size.height as i32;
    let _ = window.set_size(PhysicalSize::new(size.width, new_height));
    let _ = window.set_position(PhysicalPosition::new(position.x, bottom - new_height as i32));
}

/// Shows or hides the quick-controls flyout next to the tray icon.
fn toggle_flyout(app: &AppHandle, click: PhysicalPosition<f64>) {
    let state = app.state::<AppState>();
    // Clicking the tray icon while the flyout is open first takes focus from it, which hides it;
    // don't reopen it on that same click.
    if state.flyout_hidden_at.lock().is_some_and(|t| t.elapsed() < Duration::from_millis(350)) {
        return;
    }
    let window = match app.get_webview_window("flyout") {
        Some(window) if window.is_visible().unwrap_or(false) => {
            let _ = window.hide();
            return;
        }
        Some(window) => window,
        None => {
            let Ok(window) = WebviewWindowBuilder::new(app, "flyout", WebviewUrl::App("flyout.html".into()))
                .title("Smowaudio")
                .inner_size(FLYOUT_SIZE.0, FLYOUT_SIZE.1)
                .decorations(false)
                .resizable(false)
                .skip_taskbar(true)
                .always_on_top(true)
                .visible(false)
                .build()
            else {
                return;
            };
            let handle = app.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Focused(false) = event {
                    // Showing the window reports a spurious focus loss while focus moves into the
                    // WebView, so only hide once the window has really stayed unfocused.
                    let handle = handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(150));
                        let Some(flyout) = handle.get_webview_window("flyout") else { return };
                        if flyout.is_focused().unwrap_or(false) || !flyout.is_visible().unwrap_or(false) {
                            return;
                        }
                        let _ = flyout.hide();
                        *handle.state::<AppState>().flyout_hidden_at.lock() = Some(Instant::now());
                    });
                }
            });
            window
        }
    };
    place_flyout(&window, click);
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("flyout-shown", ());
}

/// Puts the flyout above the taskbar near the tray click, inside the screen's work area.
fn place_flyout(window: &WebviewWindow, click: PhysicalPosition<f64>) {
    let Ok(size) = window.outer_size() else { return };
    let (width, height) = (size.width as f64, size.height as f64);
    let margin = 12.0;
    let mut position = PhysicalPosition::new(click.x - width / 2.0, click.y - height - margin);
    if let Ok(Some(monitor)) = window.monitor_from_point(click.x, click.y) {
        let area = monitor.work_area();
        let (left, top) = (area.position.x as f64, area.position.y as f64);
        let (right, bottom) = (left + area.size.width as f64, top + area.size.height as f64);
        position.x = position.x.clamp(left + margin, (right - width - margin).max(left + margin));
        position.y = (bottom - height - margin).max(top + margin);
    }
    let _ = window.set_position(position);
}

/// Appends a timestamped line to %APPDATA%\Smowaudio\smowaudio.log. The release build has
/// no console, so this is where stream errors and panics leave a trace.
pub(crate) fn append_log(message: &str) {
    use std::io::Write;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("Smowaudio")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("smowaudio.log")) {
        // One write per line so lines from different threads don't interleave.
        let _ = file.write_all(format!("[unix {secs}] {message}\n").as_bytes());
    }
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

    // A second launch must not open a second set of audio streams. The single-instance plugin
    // below then hands off to the running copy (which shows its window) and exits this process.
    let duplicate = another_instance_running();

    // Audio first: the engine is running before Tauri even initializes.
    let config = {
        let _com = audio::ComGuard::new();
        let mut config = Config::load();
        if !duplicate {
            append_log(&format!("started pid {} with {}", std::process::id(), config.describe()));
            if config.set_windows_defaults && apply_windows_defaults(&mut config) {
                let _ = config.save();
            }
        }
        config
    };
    let engine = (!duplicate).then(|| Engine::start(&config));
    let dirty = Arc::new(AtomicBool::new(false));
    let state = AppState {
        config: Mutex::new(config),
        engine: Mutex::new(engine),
        dirty: dirty.clone(),
        pending_view: Mutex::new(None),
        flyout_hidden_at: Mutex::new(None),
        hotkey_errors: Mutex::new(BTreeMap::new()),
        device_fingerprint: Mutex::new(String::new()),
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| open_window(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_meters,
            set_mic,
            set_channel,
            set_devices,
            list_apps,
            app_icon,
            assign_app,
            set_launch_at_login,
            set_windows_defaults,
            set_master,
            open_main_window,
            fit_flyout,
            set_output_device,
            set_hotkey,
            pause_hotkeys,
            set_volume_step,
            take_pending_view
        ])
        .setup(move |app| {
            let open = MenuItem::with_id(app, "open", "Open Smowaudio", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("tray")
                // Bars without the app icon's tile, so they read at tray size on light and dark taskbars.
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .tooltip("Smowaudio")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => open_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left, button_state: MouseButtonState::Up, position, ..
                    } = event
                    {
                        toggle_flyout(tray.app_handle(), position);
                    }
                })
                .build(app)?;

            hotkeys::start(app.handle());

            // Restart streams when the devices they depend on change (headset plugged in, mic
            // turned on, Windows default changed while following it).
            let watcher_handle = app.handle().clone();
            std::thread::spawn(move || {
                let _com = audio::ComGuard::new();
                {
                    let state = watcher_handle.state::<AppState>();
                    let now = state.config.lock().device_fingerprint();
                    *state.device_fingerprint.lock() = now;
                }
                let result = audio::notify::watch(
                    || true,
                    || {
                        let state = watcher_handle.state::<AppState>();
                        let now = state.config.lock().device_fingerprint();
                        if now != *state.device_fingerprint.lock() {
                            append_log(&format!("audio devices changed ({now}); restarting streams"));
                            state.restart_engine();
                        }
                    },
                );
                if let Err(e) = result {
                    append_log(&format!("device change notifications unavailable: {e:#}"));
                }
            });

            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                if dirty.swap(false, Ordering::Relaxed) {
                    let config = handle.state::<AppState>().config.lock().clone();
                    if let Err(e) = config.save() {
                        log::error!("saving config: {e:#}");
                    }
                }
            });

            if !std::env::args().any(|a| a == "--background") {
                open_window(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build app");

    app.run(|app, event| match event {
        // Closing the window keeps the engine running in the tray; only "Quit" exits.
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        RunEvent::Exit => {
            let state = app.state::<AppState>();
            let _ = state.config.lock().save();
            let engine = state.engine.lock().take();
            if let Some(engine) = engine {
                engine.stop();
            }
        }
        _ => {}
    });
}
