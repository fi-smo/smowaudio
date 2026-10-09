//! Updates from the public GitHub repo. The release workflow builds a signed installer, attaches
//! it to a GitHub Release and writes `latest.json` (version, notes, signature, installer URL) to
//! the `updates` branch. The app reads that file and downloads the installer through the GitHub
//! API, checks its signature against the release key, then runs it.
//!
//! With automatic updates on (the default), a found update installs by itself at a moment it
//! can't interrupt anything: the app window is closed, nothing is playing, and either the app
//! only just started or it has been quiet for a couple of minutes. The installer restarts the
//! app with the same arguments, so one started in the tray comes back in the tray.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::app::AppHandle;
use crate::append_log;

const REPO: &str = "fi-smo/smowaudio";
/// The release signing key's public half (minisign, base64 like Tauri's updater wrote it). The
/// release workflow signs with the private half, the TAURI_SIGNING_PRIVATE_KEY secret.
const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDkxMkI3RUQ0RDMyRUE1NjMKUldSanBTN1QxSDRya1RGeHhOdWlvSmRXTjI5YjBNSkU3WUdPd3BhdXNXdGh3Z0hiYktDQjB3ZGEK";
const PLATFORM: &str = "windows-x86_64";
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

/// latest.json, as the release workflow writes it.
#[derive(Deserialize, Clone)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    platforms: std::collections::HashMap<String, Platform>,
}

#[derive(Deserialize, Clone)]
struct Platform {
    signature: String,
    url: String,
}

/// A newer version found by the last check.
#[derive(Clone)]
struct Update {
    version: String,
    signature: String,
    url: String,
}

#[derive(Default)]
pub struct Updates {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Update>>,
    /// Held while downloading, so two installs can't run at once.
    installing: Mutex<()>,
}

impl Updates {
    /// The newer version found by the last check, if any.
    pub fn status_available(&self) -> Option<String> {
        self.status.lock().available.clone()
    }
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let mut status = app.state().updates.status.lock().clone();
    status.current = current_version().to_string();
    status
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("Smowaudio")
        .build()
        .into()
}

/// Asks GitHub whether a newer version exists. Returns the new version, or None if up to date.
/// Blocks for the request, so call it off the main thread.
pub fn check(app: &AppHandle) -> Result<Option<String>, String> {
    let result = fetch_manifest().and_then(|manifest| newer(&manifest, current_version()));
    let state = app.state();
    {
        let mut status = state.updates.status.lock();
        status.checked = true;
        match &result {
            Ok(Some((update, notes))) => {
                status.available = Some(update.version.clone());
                status.notes = notes.clone().filter(|n| !n.trim().is_empty());
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
    let version = result.as_ref().ok().and_then(|u| u.as_ref().map(|(u, _)| u.version.clone()));
    if let Ok(found) = &result {
        *state.updates.pending.lock() = found.as_ref().map(|(u, _)| u.clone());
    }
    let handle = *app;
    let _ = slint::invoke_from_event_loop(move || crate::settingsui::updates_changed(&handle));
    if version.is_some() && state.config.lock().auto_update {
        install_when_idle(app);
    }
    result.map(|_| version)
}

fn fetch_manifest() -> Result<Manifest, String> {
    let url = format!("https://api.github.com/repos/{REPO}/contents/latest.json?ref=updates");
    agent()
        .get(&url)
        // The raw file rather than GitHub's JSON description of it.
        .header("Accept", "application/vnd.github.raw+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| friendly(&e.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|e| friendly(&e.to_string()))
        .and_then(|text| {
            serde_json::from_str::<Manifest>(&text).map_err(|e| format!("The update information couldn't be read: {e}"))
        })
}

/// The update in `manifest` if it's newer than `current`, with its notes.
fn newer(manifest: &Manifest, current: &str) -> Result<Option<(Update, Option<String>)>, String> {
    let parse = |v: &str| semver::Version::parse(v.trim_start_matches('v'));
    let latest = parse(&manifest.version).map_err(|_| format!("Unexpected version {}", manifest.version))?;
    let running = parse(current).map_err(|e| e.to_string())?;
    if latest <= running {
        return Ok(None);
    }
    let platform = manifest.platforms.get(PLATFORM).ok_or("No Windows installer in this release.")?;
    let update = Update { version: manifest.version.clone(), signature: platform.signature.clone(), url: platform.url.clone() };
    Ok(Some((update, manifest.notes.clone())))
}

fn friendly(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("403") || lower.contains("429") {
        "GitHub is limiting update checks from this network for now. Try again in an hour.".into()
    } else if lower.contains("404") {
        "No release published yet.".into()
    } else if lower.contains("dns") || lower.contains("connect") || lower.contains("timed out") || lower.contains("timeout") {
        "Couldn't reach GitHub. Check your internet connection.".into()
    } else {
        message.to_string()
    }
}

/// Downloads the pending update (reporting progress to Settings), checks its signature and runs
/// the installer, which closes the app and starts the new version. Returns only if that failed.
/// Blocks for the download, so call it off the main thread.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let state = app.state();
    let Some(_installing) = state.updates.installing.try_lock() else {
        return Err("The update is already being installed".into());
    };
    let update = state.updates.pending.lock().clone().ok_or("Check for updates first")?;
    append_log(&format!("downloading update {}", update.version));
    let result = download(app, &update).and_then(|bytes| {
        verify(&bytes, &update.signature, &update.version)?;
        run_installer(&update.version, &bytes)
    });
    result.map_err(|e| {
        append_log(&format!("update failed: {e}"));
        e
    })
}

fn download(app: &AppHandle, update: &Update) -> Result<Vec<u8>, String> {
    let mut response = ureq::Agent::config_builder()
        // A slow connection may need a while for the whole installer.
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .user_agent("Smowaudio")
        .build()
        .new_agent()
        .get(&update.url)
        // The installer is a release asset: the API serves its bytes with this Accept header.
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(|e| friendly(&e.to_string()))?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reader = response.body_mut().with_config().limit(512 * 1024 * 1024).reader();
    let mut bytes = Vec::with_capacity(total as usize);
    let mut chunk = vec![0; 64 * 1024];
    let mut reported = Instant::now();
    loop {
        let n = reader.read(&mut chunk).map_err(|e| friendly(&e.to_string()))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if reported.elapsed() > Duration::from_millis(100) || bytes.len() as u64 == total {
            reported = Instant::now();
            let (handle, done) = (*app, bytes.len() as u64);
            let _ = slint::invoke_from_event_loop(move || crate::settingsui::update_progress(&handle, done, total));
        }
    }
    Ok(bytes)
}

/// Checks the installer against the release key, and that it was signed as `version`: latest.json
/// itself isn't signed, so this stops it pairing a new version number with an older installer.
fn verify(bytes: &[u8], signature: &str, version: &str) -> Result<(), String> {
    let decode = |b64: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .ok()
            .and_then(|raw| String::from_utf8(raw).ok())
            .ok_or_else(|| "The update's signature is malformed.".to_string())
    };
    let key = minisign_verify::PublicKey::decode(&decode(PUBLIC_KEY)?).map_err(|e| e.to_string())?;
    let signature = minisign_verify::Signature::decode(&decode(signature)?).map_err(|_| "The update's signature is malformed.")?;
    key.verify(bytes, &signature, true).map_err(|_| "The downloaded update isn't signed by Smowaudio, so it wasn't installed.")?;
    // The trusted comment ("timestamp:…\tfile:…\tversion:0.9.4") is covered by the signature.
    let signed = signature.trusted_comment().split('\t').find_map(|f| f.strip_prefix("version:"));
    match signed {
        Some(signed) if signed.trim_start_matches('v') != version.trim_start_matches('v') => {
            Err(format!("The update claims to be {version} but is signed as {signed}, so it wasn't installed."))
        }
        _ => Ok(()),
    }
}

/// Saves the settings, starts the installer quietly and exits; it restarts the app afterwards with
/// the arguments this one had. The arguments are the ones Tauri's updater passed (`/S /UPDATE /R
/// /ARGS …`), which the installer still understands.
fn run_installer(version: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("smowaudio-update-{version}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("Smowaudio_{version}_x64-setup.exe"));
    std::fs::write(&path, bytes).map_err(|e| format!("The update couldn't be saved: {e}"))?;

    let mut parameters = vec!["/S".to_string(), "/UPDATE".into(), "/R".into(), "/ARGS".into()];
    parameters.extend(std::env::args().skip(1).map(|a| escape_nsis_arg(&a)));
    let parameters = parameters.join(" ");

    let _ = crate::state().config.lock().save();
    append_log(&format!("installing update {version}; exiting"));
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (file, params) = (wide(&path.to_string_lossy()), wide(&parameters));
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(file.as_ptr()),
            windows::core::PCWSTR(params.as_ptr()),
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
        )
    };
    if result.0 as isize <= 32 {
        return Err(format!("The installer didn't start: {}", std::io::Error::last_os_error()));
    }
    crate::shutdown();
    std::process::exit(0);
}

/// Quotes an argument for the installer's command line; `/` is quoted too, or NSIS would read it
/// as one of its own options.
fn escape_nsis_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '/', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', backslashes));
    out.push('"');
    out
}

/// Installs the available update once nothing would be interrupted (see the module docs). Stops
/// waiting if automatic updates get turned off, or the update is installed some other way.
pub fn install_when_idle(app: &AppHandle) {
    if WAITING.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = *app;
    let spawned = std::thread::Builder::new().name("Automatic update".into()).spawn(move || {
        let mut quiet_since: Option<Instant> = None;
        loop {
            let state = handle.state();
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
            let quiet_long_enough = quiet_since.is_some_and(|since| now - since >= QUIET_FOR);
            if !crate::mainwin::is_open() && !playing && (STARTED.elapsed() < JUST_STARTED || quiet_long_enough) {
                append_log(&format!("installing update {version} automatically: nothing is playing"));
                // Only returns if it failed; success restarts the app.
                if let Err(e) = install(&handle) {
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
    let handle = *app;
    let _ = std::thread::Builder::new().name("Update checks".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        loop {
            if let Err(e) = check(&handle) {
                append_log(&format!("update check failed: {e}"));
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_versions_are_offered() {
        let manifest = |v: &str| Manifest {
            version: v.into(),
            notes: Some("notes".into()),
            platforms: [(PLATFORM.to_string(), Platform { signature: "sig".into(), url: "url".into() })].into(),
        };
        assert!(newer(&manifest("0.9.4"), "0.9.4").unwrap().is_none());
        assert!(newer(&manifest("0.9.3"), "0.9.4").unwrap().is_none());
        assert_eq!(newer(&manifest("v0.10.0"), "0.9.4").unwrap().unwrap().0.version, "v0.10.0");
    }

    #[test]
    fn installer_arguments_are_quoted_like_tauri_did() {
        assert_eq!(escape_nsis_arg("--background"), "--background");
        assert_eq!(escape_nsis_arg("a b"), "\"a b\"");
        assert_eq!(escape_nsis_arg("/x"), "\"/x\"");
        assert_eq!(escape_nsis_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(escape_nsis_arg(r"C:\my dir\"), r#""C:\my dir\\""#);
    }

    #[test]
    fn the_release_key_decodes() {
        let raw = base64::engine::general_purpose::STANDARD.decode(PUBLIC_KEY).unwrap();
        assert!(minisign_verify::PublicKey::decode(std::str::from_utf8(&raw).unwrap()).is_ok());
    }
}
