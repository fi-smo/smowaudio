use anyhow::{Context, Result};
use serde::Serialize;
use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    Render,
    Capture,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Render => eRender,
            Flow::Capture => eCapture,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub flow: Flow,
    pub is_default: bool,
    /// Hardware description in the trailing parentheses, e.g. "VB-Audio Cable A".
    /// Survives the user renaming the endpoint, so it is used to pair cable sides.
    pub hardware: String,
}

/// Hardware names of virtual audio drivers that must never be used as "physical" devices.
/// Other mixer apps (Sonar, Wave Link, Voicemeeter) often leave theirs set as the Windows default.
const VIRTUAL_HARDWARE: &[&str] = &["VB-Audio", "SteelSeries Sonar", "Elgato Virtual Audio", "Voicemeeter", "VoiceMeeter"];

impl DeviceInfo {
    pub fn is_virtual(&self) -> bool {
        VIRTUAL_HARDWARE.iter().any(|v| self.hardware.contains(v))
    }
}

pub fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("create device enumerator")
}

pub fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let raw = device.GetId()?;
        let id = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(id?)
    }
}

pub fn friendly_name(device: &IMMDevice) -> Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let value = store.GetValue(&PKEY_Device_FriendlyName)?;
        let raw = PropVariantToStringAlloc(&value)?;
        let name = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(name?)
    }
}

fn hardware_part(name: &str) -> String {
    match (name.rfind('('), name.rfind(')')) {
        (Some(open), Some(close)) if close > open => name[open + 1..close].to_string(),
        _ => name.to_string(),
    }
}

pub fn list(flow: Flow) -> Result<Vec<DeviceInfo>> {
    let enumerator = enumerator()?;
    let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(flow.data_flow(), eConsole) }
        .ok()
        .and_then(|d| device_id(&d).ok());
    let collection = unsafe { enumerator.EnumAudioEndpoints(flow.data_flow(), DEVICE_STATE_ACTIVE)? };
    let count = unsafe { collection.GetCount()? };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let device = unsafe { collection.Item(i)? };
        let id = device_id(&device)?;
        let name = friendly_name(&device).unwrap_or_else(|_| id.clone());
        out.push(DeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            hardware: hardware_part(&name),
            id,
            name,
            flow,
        });
    }
    Ok(out)
}

pub fn by_id(id: &str) -> Result<IMMDevice> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { enumerator()?.GetDevice(PCWSTR(wide.as_ptr())) }.with_context(|| format!("device {id} not found"))
}

/// The device to use for a physical role, in order: the user's explicit choice if it's connected;
/// `preferred` (their own Windows default from before AudioManager made the cables default) if
/// it's connected and physical; the current Windows default if physical; any physical device.
/// Never a virtual device: a cable would feed back into itself, another mixer's device may be dead.
pub fn resolve_physical(flow: Flow, configured: Option<&str>, preferred: Option<&str>) -> Result<IMMDevice> {
    let devices = list(flow)?;
    let active = |id: Option<&str>| id.and_then(|id| devices.iter().find(|d| d.id == id));
    let pick = active(configured)
        .or_else(|| active(preferred).filter(|d| !d.is_virtual()))
        .or_else(|| devices.iter().find(|d| d.is_default && !d.is_virtual()))
        .or_else(|| devices.iter().find(|d| !d.is_virtual()))
        .context("no physical audio device available")?;
    by_id(&pick.id)
}
