#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod dsp;
mod engine;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

use audio::device::{self, DeviceInfo, Flow};
use audio::routing::{self, AudioApp};
use config::{cable_partner, Config};
use dsp::chain::{ChannelSettings, MicSettings};
use engine::{Engine, Meters};

struct AppState {
    config: Mutex<Config>,
    engine: Mutex<Option<Engine>>,
    dirty: Arc<AtomicBool>,
}

impl AppState {
    /// Mutates config and schedules a save (coalesced so slider drags don't hammer the disk).
    fn update(&self, f: impl FnOnce(&mut Config)) -> Config {
        let mut config = self.config.lock();
        f(&mut config);
        self.dirty.store(true, Ordering::Relaxed);
        config.clone()
    }

    fn restart_engine(&self) {
        let config = self.config.lock().clone();
        let mut engine = self.engine.lock();
        if let Some(old) = engine.take() {
            old.stop();
        }
        *engine = Some(Engine::start(&config));
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
    })
}

#[tauri::command]
fn get_meters(state: State<AppState>) -> Meters {
    state.engine.lock().as_ref().map(|e| e.shared.meters.lock().clone()).unwrap_or_default()
}

#[tauri::command]
fn set_mic(state: State<AppState>, settings: MicSettings) {
    state.update(|c| c.mic = settings.clone());
    if let Some(e) = state.engine.lock().as_ref() {
        e.set_mic(settings);
    }
}

#[tauri::command]
fn set_channel(state: State<AppState>, index: usize, settings: ChannelSettings) -> CmdResult<()> {
    if index >= config::CHANNEL_COUNT {
        return Err("invalid channel".into());
    }
    state.update(|c| c.channels[index].settings = settings.clone());
    if let Some(e) = state.engine.lock().as_ref() {
        e.set_channel(index, settings);
    }
    Ok(())
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
    state.restart_engine();
}

#[tauri::command]
fn list_apps() -> CmdResult<Vec<AudioApp>> {
    let _com = audio::ComGuard::new();
    routing::list_apps().map_err(err)
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

/// Starts AudioManager in the tray at sign-in through a Task Scheduler task. Task Scheduler
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
Register-ScheduledTask -TaskName 'AudioManager' -Description 'Starts AudioManager in the tray at sign-in' -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force -ErrorAction Stop | Out-Null"#
        )
    } else {
        "Unregister-ScheduledTask -TaskName 'AudioManager' -Confirm:$false -ErrorAction SilentlyContinue".to_string()
    };
    run_powershell(&script)?;
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

/// True if another AudioManager process already holds the instance mutex.
/// The handle is intentionally leaked so the mutex lives as long as this process.
fn another_instance_running() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe { CreateMutexW(None, false, w!("Local\\AudioManager.SingleInstance")).is_ok() && GetLastError() == ERROR_ALREADY_EXISTS }
}

fn open_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    // Created on demand and destroyed on close, so WebView2 only uses memory while visible.
    let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("AudioManager")
        .inner_size(1000.0, 720.0)
        .min_inner_size(420.0, 500.0)
        .build();
}

/// Appends a timestamped line to %APPDATA%\AudioManager\audiomanager.log. The release build has
/// no console, so this is where stream errors and panics leave a trace.
pub(crate) fn append_log(message: &str) {
    use std::io::Write;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("AudioManager")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("audiomanager.log")) {
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
        let config = Config::load();
        if !duplicate {
            append_log(&format!("started pid {} with {}", std::process::id(), config.describe()));
        }
        config
    };
    let engine = (!duplicate).then(|| Engine::start(&config));
    let dirty = Arc::new(AtomicBool::new(false));
    let state = AppState { config: Mutex::new(config), engine: Mutex::new(engine), dirty: dirty.clone() };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| open_window(app)))
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_meters,
            set_mic,
            set_channel,
            set_devices,
            list_apps,
            assign_app,
            set_launch_at_login
        ])
        .setup(move |app| {
            let open = MenuItem::with_id(app, "open", "Open AudioManager", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("AudioManager")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => open_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        open_window(tray.app_handle());
                    }
                })
                .build(app)?;

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
