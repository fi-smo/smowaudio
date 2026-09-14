//! Per-application output device assignment and audio session discovery.
//!
//! Uses the undocumented `Windows.Media.Internal.AudioPolicyConfig` factory, the same API the
//! Settings app ("App volume and device preferences") and EarTrumpet use. Windows persists the
//! choice per executable, so an assignment survives app and PC restarts.

// COM method names must match the Windows interface definition.
#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::ffi::c_void;

use anyhow::Result;
use serde::Serialize;
use windows::core::{interface, IUnknown, IUnknown_Vtbl, Interface, HRESULT, HSTRING, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, AudioSessionStateExpired, EDataFlow, ERole, IAudioSessionControl2,
    IAudioSessionManager2, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::RoGetActivationFactory;

use super::device;

const MMDEVAPI_PREFIX: &str = r"\\?\SWD#MMDEVAPI#";
const RENDER_SUFFIX: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

/// Windows 11 (21H2+) layout. The first three pads are IInspectable's methods.
#[interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: IUnknown {
    fn get_iids(&self) -> HRESULT;
    fn get_runtime_class_name(&self) -> HRESULT;
    fn get_trust_level(&self) -> HRESULT;
    fn add_ctx_volume_change(&self) -> HRESULT;
    fn remove_ctx_volume_changed(&self) -> HRESULT;
    fn add_ringer_vibrate_state_changed(&self) -> HRESULT;
    fn remove_ringer_vibrate_state_change(&self) -> HRESULT;
    fn set_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_active_volume_group_for_endpoint_id(&self) -> HRESULT;
    fn get_volume_groups_for_endpoint(&self) -> HRESULT;
    fn get_current_volume_context(&self) -> HRESULT;
    fn set_volume_group_mute_for_id(&self) -> HRESULT;
    fn get_volume_group_mute_for_id(&self) -> HRESULT;
    fn set_ringer_vibrate_state(&self) -> HRESULT;
    fn get_ringer_vibrate_state(&self) -> HRESULT;
    fn set_preferred_chat_application(&self) -> HRESULT;
    fn reset_preferred_chat_application(&self) -> HRESULT;
    fn get_preferred_chat_application(&self) -> HRESULT;
    fn get_current_chat_applications(&self) -> HRESULT;
    fn add_chat_context_changed(&self) -> HRESULT;
    fn remove_chat_context_changed(&self) -> HRESULT;
    fn SetPersistedDefaultAudioEndpoint(&self, pid: u32, flow: EDataFlow, role: ERole, device: *mut c_void) -> HRESULT;
    fn GetPersistedDefaultAudioEndpoint(&self, pid: u32, flow: EDataFlow, role: ERole, device: *mut *mut c_void) -> HRESULT;
    fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
}

fn factory() -> Result<IAudioPolicyConfigFactory> {
    let class = HSTRING::from("Windows.Media.Internal.AudioPolicyConfig");
    Ok(unsafe { RoGetActivationFactory(&class)? })
}

/// Sends a process's audio to the render device `device_id`, or back to the Windows default with `None`.
pub fn set_app_output(pid: u32, device_id: Option<&str>) -> Result<()> {
    let factory = factory()?;
    let hstring = device_id.map(|id| HSTRING::from(format!("{MMDEVAPI_PREFIX}{id}{RENDER_SUFFIX}")));
    let raw: *mut c_void = hstring.as_ref().map_or(std::ptr::null_mut(), |h| unsafe { std::mem::transmute_copy(h) });
    unsafe {
        factory.SetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, raw).ok()?;
        factory.SetPersistedDefaultAudioEndpoint(pid, eRender, eConsole, raw).ok()?;
    }
    Ok(())
}

pub fn get_app_output(pid: u32) -> Option<String> {
    let factory = factory().ok()?;
    let mut raw: *mut c_void = std::ptr::null_mut();
    unsafe { factory.GetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, &mut raw).ok().ok()? };
    if raw.is_null() {
        return None;
    }
    // Take ownership so the HSTRING is released.
    let hstring: HSTRING = unsafe { std::mem::transmute(raw) };
    let full = hstring.to_string_lossy();
    let id = full.strip_prefix(MMDEVAPI_PREFIX).unwrap_or(&full);
    let id = id.split("#{").next().unwrap_or(id);
    (!id.is_empty()).then(|| id.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioApp {
    pub pid: u32,
    /// Lower-case executable file name, used as the rule key.
    pub exe: String,
    pub path: String,
    /// Device the app is persisted to, if it isn't following the Windows default.
    pub assigned_device: Option<String>,
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Lists processes that currently have an audio session on any output device.
pub fn list_apps() -> Result<Vec<AudioApp>> {
    let enumerator = device::enumerator()?;
    let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    let own_pid = std::process::id();
    let mut apps: BTreeMap<u32, AudioApp> = BTreeMap::new();

    for i in 0..unsafe { collection.GetCount()? } {
        let Ok(manager) = (unsafe { collection.Item(i)?.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else {
            continue;
        };
        let sessions = unsafe { manager.GetSessionEnumerator()? };
        for j in 0..unsafe { sessions.GetCount()? } {
            let Ok(control) = (unsafe { sessions.GetSession(j) }) else { continue };
            let Ok(control) = control.cast::<IAudioSessionControl2>() else { continue };
            let pid = unsafe { control.GetProcessId() }.unwrap_or(0);
            let expired = unsafe { control.GetState() }.map_or(true, |s| s == AudioSessionStateExpired);
            if pid == 0 || pid == own_pid || expired || apps.contains_key(&pid) {
                continue;
            }
            let Some(path) = process_path(pid) else { continue };
            let exe = path.rsplit('\\').next().unwrap_or(&path).to_lowercase();
            apps.insert(pid, AudioApp { pid, exe, assigned_device: get_app_output(pid), path });
        }
    }
    Ok(apps.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::device::Flow;
    use crate::audio::ComGuard;

    /// Read-only: exercises the undocumented vtable layout without changing any routing.
    #[test]
    #[ignore = "talks to this PC's real audio devices; run with --ignored"]
    fn lists_devices_and_reads_app_assignments() {
        let _com = ComGuard::new();
        let renders = device::list(Flow::Render).expect("list render devices");
        assert!(!renders.is_empty());
        for d in &renders {
            println!("render: {} [{}]", d.name, d.hardware);
        }
        let _ = factory().expect("AudioPolicyConfig factory activates");
        for app in list_apps().expect("list audio apps") {
            println!("app: {} pid={} assigned={:?}", app.exe, app.pid, app.assigned_device);
        }
    }

    /// Read-only diagnostic: which processes have streams open on each endpoint.
    #[test]
    #[ignore = "diagnostic for this PC's audio sessions; run with --ignored"]
    fn print_sessions_per_device() {
        use windows::Win32::Media::Audio::{eCapture, AudioSessionStateActive};
        let _com = ComGuard::new();
        let enumerator = device::enumerator().unwrap();
        for flow in [eRender, eCapture] {
            let collection = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE).unwrap() };
            for i in 0..unsafe { collection.GetCount().unwrap() } {
                let dev = unsafe { collection.Item(i).unwrap() };
                let name = device::friendly_name(&dev).unwrap_or_default();
                let Ok(manager) = (unsafe { dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
                let sessions = unsafe { manager.GetSessionEnumerator().unwrap() };
                for j in 0..unsafe { sessions.GetCount().unwrap() } {
                    let Ok(control) = (unsafe { sessions.GetSession(j) }) else { continue };
                    let Ok(control) = control.cast::<IAudioSessionControl2>() else { continue };
                    let pid_result = unsafe { control.GetProcessId() };
                    let pid = pid_result.as_ref().map_or(0, |p| *p);
                    let state = match unsafe { control.GetState() } {
                        Ok(s) if s == AudioSessionStateActive => "active",
                        Ok(s) if s == AudioSessionStateExpired => "expired",
                        Ok(_) => "inactive",
                        Err(_) => "unknown",
                    };
                    // ALL_SESSIONS also shows sessions Windows can't attribute to one process
                    // (pid 0 / lookup failure) and the session instance id, which names the exe.
                    let all = std::env::var_os("ALL_SESSIONS").is_some();
                    if (pid != 0 && state == "active") || all {
                        let exe = process_path(pid).and_then(|p| p.rsplit('\\').next().map(str::to_string)).unwrap_or_default();
                        let pid_note = match &pid_result {
                            Ok(_) => String::new(),
                            Err(e) => format!(", pid lookup failed: {}", e.code()),
                        };
                        let instance = if all {
                            unsafe { control.GetSessionInstanceIdentifier() }
                                .ok()
                                .and_then(|id| unsafe { id.to_string() }.ok())
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        println!(
                            "session: [{}] {name} <- {exe} (pid {pid}{pid_note}) {state} {instance}",
                            if flow == eRender { "render" } else { "capture" }
                        );
                    }
                }
            }
        }
    }
}

/// Whether any other process is currently recording from the capture endpoint `capture_id`.
pub fn has_other_capture_clients(capture_id: &str) -> Result<bool> {
    use windows::Win32::Media::Audio::AudioSessionStateActive;
    let manager: IAudioSessionManager2 = unsafe { device::by_id(capture_id)?.Activate(CLSCTX_ALL, None)? };
    let sessions = unsafe { manager.GetSessionEnumerator()? };
    let own_pid = std::process::id();
    for i in 0..unsafe { sessions.GetCount()? } {
        let Ok(control) = (unsafe { sessions.GetSession(i) }) else { continue };
        let Ok(control) = control.cast::<IAudioSessionControl2>() else { continue };
        let active = unsafe { control.GetState() }.is_ok_and(|s| s == AudioSessionStateActive);
        let pid = unsafe { control.GetProcessId() }.unwrap_or(0);
        if active && pid != own_pid {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Applies exe -> render device rules to running apps whose assignment differs.
pub fn apply_rules(rules: &BTreeMap<String, String>) -> Result<()> {
    if rules.is_empty() {
        return Ok(());
    }
    for app in list_apps()? {
        if let Some(target) = rules.get(&app.exe) {
            if app.assigned_device.as_deref() != Some(target.as_str()) {
                let _ = set_app_output(app.pid, Some(target));
            }
        }
    }
    Ok(())
}
