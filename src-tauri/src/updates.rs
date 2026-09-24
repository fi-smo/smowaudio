//! Updates from the public GitHub repo. The release workflow builds a signed installer, attaches
//! it to a GitHub Release and writes `latest.json` (version, notes, signature, installer URL) to
//! the `updates` branch. The app reads that file and downloads the installer through the GitHub
//! API; the updater checks the signature before installing. A saved token is sent along if there
//! is one (only needed if the repo is ever private again).

use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::http::{header, HeaderValue};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{append_log, AppState};

const REPO: &str = "fi-smo/smowaudio";
/// Checked again this often while the app runs (and once shortly after it starts).
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Serialize, Clone, Default)]
pub struct UpdateStatus {
    /// The running version.
    pub current: String,
    /// End of the saved token (e.g. "…a1B2"), or None if there's no token.
    pub token_hint: Option<String>,
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
    /// The saved token with its middle hidden, e.g. "github_pat_••••UzW9".
    pub token_mask: Option<String>,
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
    let token = crate::secrets::read_token();
    status.token_hint = token.as_deref().map(hint);
    status.token_mask = token.as_deref().map(mask);
    status
}

fn mask(token: &str) -> String {
    let prefix = ["github_pat_", "ghp_"].into_iter().find(|p| token.starts_with(p)).unwrap_or("");
    format!("{prefix}••••{}", hint(token).trim_start_matches('…'))
}

fn hint(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
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
    result.map(|_| version)
}

async fn check_inner(app: &AppHandle) -> Result<Option<Update>, String> {
    let auth = match crate::secrets::read_token() {
        Some(token) => Some(HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| "The token has characters GitHub never uses")?),
        None => None,
    };
    let endpoint: Url = format!("https://api.github.com/repos/{REPO}/contents/latest.json?ref=updates")
        .parse()
        .map_err(|e| format!("{e}"))?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|b| match auth { Some(auth) => b.header(header::AUTHORIZATION, auth), None => Ok(b) })
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
    if lower.contains("401") || lower.contains("403") {
        "GitHub refused the token. Check it hasn't expired and can read the smowaudio repo.".into()
    } else if lower.contains("404") {
        "No release published yet (or the token can't see the repo).".into()
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

/// Checks shortly after start, then every few hours.
pub fn start_background_checks(app: &AppHandle) {
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
