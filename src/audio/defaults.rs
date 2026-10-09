//! Windows default playback/recording devices, set the way SteelSeries Sonar does it:
//! Game = default playback, Chat = default communications playback, Virtual Mic = default
//! recording (both roles).
//!
//! Uses IPolicyConfig, the undocumented interface the Sound control panel itself uses (also used
//! by SoundSwitch and NirSoft SoundVolumeView). The method order below matches both known IIDs.

#![allow(non_snake_case)]

use std::ffi::c_void;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::core::{interface, IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Media::Audio::{eCapture, eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::device;

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Windows 10/11 variant.
#[interface("8f9fb2aa-1c0b-4d54-b6bb-b2f2a10ce03c")]
unsafe trait IPolicyConfigX: IUnknown {
    fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, device: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, device: PCWSTR, endpoint: *const c_void, mix: *const c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, device: PCWSTR, default: i32, default_period: *mut i64, min_period: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, device: PCWSTR, period: *const i64) -> HRESULT;
    fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, device: PCWSTR, mode: *const c_void) -> HRESULT;
    fn GetPropertyValue(&self, device: PCWSTR, fx_store: i32, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, device: PCWSTR, fx_store: i32, key: *const c_void, value: *const c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
}

/// Windows 7+ variant with the same method order.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig7: IUnknown {
    fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, device: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, device: PCWSTR, endpoint: *const c_void, mix: *const c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, device: PCWSTR, default: i32, default_period: *mut i64, min_period: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, device: PCWSTR, period: *const i64) -> HRESULT;
    fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, device: PCWSTR, mode: *const c_void) -> HRESULT;
    fn GetPropertyValue(&self, device: PCWSTR, fx_store: i32, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, device: PCWSTR, fx_store: i32, key: *const c_void, value: *const c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
}

fn set_default_endpoint(id: &str, role: ERole) -> Result<()> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    let client: IUnknown =
        unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) }.context("create PolicyConfig")?;
    let hr = if let Ok(policy) = client.cast::<IPolicyConfigX>() {
        unsafe { policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role) }
    } else {
        let policy: IPolicyConfig7 = client.cast().context("PolicyConfig interface not available")?;
        unsafe { policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role) }
    };
    hr.ok().with_context(|| format!("set default device {id}"))
}

fn current(flow: EDataFlow, role: ERole) -> Option<String> {
    let enumerator = device::enumerator().ok()?;
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(flow, role) }.ok()?;
    device::device_id(&endpoint).ok()
}

/// A full set of Windows default devices (endpoint ids).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowsDefaults {
    pub playback: Option<String>,
    pub playback_communications: Option<String>,
    pub recording: Option<String>,
    pub recording_communications: Option<String>,
}

impl WindowsDefaults {
    pub fn read() -> Self {
        Self {
            playback: current(eRender, eConsole),
            playback_communications: current(eRender, eCommunications),
            recording: current(eCapture, eConsole),
            recording_communications: current(eCapture, eCommunications),
        }
    }

    /// Drops every slot that points at a virtual device (a cable, another mixer's device).
    /// "Restoring" one of those would just leave a cable as the Windows default.
    pub fn without_virtual(mut self) -> Self {
        let virtual_ids: Vec<String> = [device::Flow::Render, device::Flow::Capture]
            .into_iter()
            .filter_map(|flow| device::list(flow).ok())
            .flatten()
            .filter(|d| d.is_virtual())
            .map(|d| d.id)
            .collect();
        for slot in [&mut self.playback, &mut self.playback_communications, &mut self.recording, &mut self.recording_communications] {
            if slot.as_ref().is_some_and(|id| virtual_ids.contains(id)) {
                *slot = None;
            }
        }
        self
    }

    /// Sets every device in `self` that differs from the current default. Returns how many
    /// defaults were changed.
    pub fn apply(&self) -> Result<usize> {
        let now = Self::read();
        let mut changed = 0;
        let slots = [
            (&self.playback, &now.playback, [eConsole, eMultimedia].as_slice()),
            (&self.playback_communications, &now.playback_communications, [eCommunications].as_slice()),
            (&self.recording, &now.recording, [eConsole, eMultimedia].as_slice()),
            (&self.recording_communications, &now.recording_communications, [eCommunications].as_slice()),
        ];
        for (target, current, roles) in slots {
            if let Some(id) = target {
                if Some(id) != current.as_ref() {
                    for role in roles {
                        set_default_endpoint(id, *role)?;
                    }
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }
}
