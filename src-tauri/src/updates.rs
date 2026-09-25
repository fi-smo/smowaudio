//! Updates from the public GitHub repo. The release workflow builds a signed installer, attaches
//! it to a GitHub Release and writes `latest.json` (version, notes, signature, installer URL) to
//! the `updates` branch. The app reads that file and downloads the installer through the GitHub
//! API; the updater checks the signature before installing.
//!
//! With automatic updates on (the default), a found update installs by itself at a moment it
//! can't interrupt anything: the app window is closed, nothing is playing, and either the app
//! only just started or it has been quiet for a couple of minutes. The installer restarts the
//! app with the same arguments, so one started in the tray comes back in the tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::http::{header, HeaderValue};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{append_log, AppState};

const REPO: &str = "fi-smo/smowaudio";
/// Checked again this often while the app runs (and once shortly after it starts).
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// An automatic install waits until nothing has played for this long...
const QUIET_FOR: Duration = Duration::from_secs(120);
/// ...unless the app started this recently, when a restart interrupts nothing anyway.
const JUST_STARTED: Duration = Duration::from_secs(180);
/// Channels quieter than this count as not playing.
const SILENT_DB: f32 = -60.0;

static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
/// An automatic install is waiting for its moment.
static WAITING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone, Default)]
pub struct UpdateStatus {
    /// The running version.
    pub current: String,
    /// A newer version that can be installed.
    pub available: Option<String>,
    /// Its release notes.
    pub notes: Option<String>,
    /// Why the last check failed.
    pub error: Option<String>,
    /// A check has completed since the app started.
    pub checked: bool,
    /// When the last successful check finished (Unix time in ms), for "checked 2 min ago".
    pub checked_at: Option<u64>,
}

#[derive(Default)]
pub struct Updates {
    status: Mutex<UpdateStatus>,
    pending: tauri::async_runtime::Mutex<Option<Update>>,
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let updates = app.state::<AppState>();
    let mut status = updates.updates.status.lock().clone();
    status.current = app.package_info().version.to_string();
    status
}

/// Asks GitHub whether a newer version exists. Returns the new version, or None if up to date.
pub async fn check(app: &AppHandle) -> Result<Option<String>, String> {
    let result = check_inner(app).await;
    let state = app.state::<AppState>();
    {
        let mut status = state.updates.status.lock();
        status.checked = true;
        match &result {
            Ok(Some(update)) => {
                status.available = Some(update.version.clone());
                status.notes = update.body.clone().filter(|b| !b.trim().is_empty());
                status.error = None;
            }
            Ok(None) => {
                status.available = None;
                status.notes = None;
                status.error = None;
            }
            Err(e) => status.error = Some(e.clone()),
        }
        if result.is_ok() {
            status.checked_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as u64);
        }
    }
    let version = result.as_ref().ok().and_then(|u| u.as_ref().map(|u| u.version.clone()));
    *state.updates.pending.lock().await = result.clone().ok().flatten();
    let _ = app.emit("update-status", status(app));
    if version.is_some() && state.config.lock().auto_update {
        install_when_idle(app);
    }
    result.map(|_| version)
}

async fn check_inner(app: &AppHandle) -> Result<Option<Update>, String> {
    let endpoint: Url = format!("https://api.github.com/repos/{REPO}/contents/latest.json?ref=updates")
        .parse()
        .map_err(|e| format!("{e}"))?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        // The raw file rather than GitHub's JSON description of it.
        .and_then(|b| b.header(header::ACCEPT, "application/vnd.github.raw+json"))
        .and_then(|b| b.header(header::USER_AGENT, "Smowaudio"))
        .and_then(|b| b.header("X-GitHub-Api-Version", "2022-11-28"))
        .map(|b| b.timeout(Duration::from_secs(30)))
        // Settings are saved before the installer takes over.
        .map(|b| {
            let handle = app.clone();
            b.on_before_exit(move || {
                let _ = handle.state::<AppState>().config.lock().save();
                append_log("installing an update; exiting");
            })
        })
        .and_then(|b| b.build())
        .map_err(|e| format!("{e}"))?;
    match updater.check().await {
        Ok(update) => Ok(update),
        Err(e) => Err(friendly(&e.to_string())),
    }
}

fn friendly(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("403") || lower.contains("429") {
        "GitHub is limiting update checks from this network for now. Try again in an hour.".into()
    } else if lower.contains("404") {
        "No release published yet.".into()
    } else if lower.contains("dns") || lower.contains("connect") || lower.contains("timed out") {
        "Couldn't reach GitHub. Check your internet connection.".into()
    } else {
        message.to_string()
    }
}

/// Downloads the pending update (reporting progress as "update-progress") and runs its installer,
/// which closes the app and starts the new version.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut pending = state.updates.pending.lock().await;
    let mut update = pending.take().ok_or("Check for updates first")?;
    // The installer is a release asset: the API serves its bytes with this Accept header.
    update.headers.insert(header::ACCEPT, HeaderValue::from_static("application/octet-stream"));
    let mut downloaded = 0usize;
    let handle = app.clone();
    append_log(&format!("downloading update {}", update.version));
    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk;
                let _ = handle.emit("update-progress", serde_json::json!({ "downloaded": downloaded, "total": total }));
            },
            || {},
        )
        .await;
    // Only reached if installing failed: success exits the process.
    result.map_err(|e| {
        let message = friendly(&e.to_string());
        append_log(&format!("update failed: {message}"));
        message
    })
}

/// Installs the available update once nothing would be interrupted (see the module docs). Stops
/// waiting if automatic updates get turned off, or the update is installed some other way.
pub fn install_when_idle(app: &AppHandle) {
    if WAITING.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    let spawned = std::thread::Builder::new().name("Automatic update".into()).spawn(move || {
        let mut quiet_since: Option<Instant> = None;
        loop {
            let state = handle.state::<AppState>();
            let enabled = state.config.lock().auto_update;
            let Some(version) = state.updates.status.lock().available.clone().filter(|_| enabled) else {
                break;
            };
            let playing = state
                .engine
                .lock()
                .as_ref()
                .is_some_and(|e| e.meters().channels.iter().flatten().any(|&db| db > SILENT_DB));
            let now = Instant::now();
            quiet_since = if playing { None } else { quiet_since.or(Some(now)) };
            let window_open = handle.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false));
            let quiet_long_enough = quiet_since.is_some_and(|since| now - since >= QUIET_FOR);
            if !window_open && !playing && (STARTED.elapsed() < JUST_STARTED || quiet_long_enough) {
                append_log(&format!("installing update {version} automatically: nothing is playing"));
                // Only returns if it failed; success restarts the app.
                if let Err(e) = tauri::async_runtime::block_on(install(&handle)) {
                    append_log(&format!("automatic update failed: {e}"));
                }
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        WAITING.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        WAITING.store(false, Ordering::SeqCst);
    }
}

/// Checks shortly after start, then every few hours.
pub fn start_background_checks(app: &AppHandle) {
    LazyLock::force(&STARTED);
    let handle = app.clone();
    let _ = std::thread::Builder::new().name("Update checks".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        loop {
            if let Err(e) = tauri::async_runtime::block_on(check(&handle)) {
                append_log(&format!("update check failed: {e}"));
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}
