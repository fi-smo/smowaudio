//! The output device's own volume: Windows' volume for that device. On Bluetooth speakers and
//! headphones with hardware volume (AVRCP Absolute Volume) it is the speaker's own volume, the
//! same one its +/− buttons change, so the user doesn't have to reach for the speaker.

use anyhow::{Context, Result};
use windows::core::GUID;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::System::Com::CLSCTX_ALL;

use super::device;

/// ENDPOINT_HARDWARE_SUPPORT_VOLUME: the device itself does the volume, not Windows.
const HARDWARE_VOLUME: u32 = 0x1;

/// Marks our own changes in Windows' volume notifications.
const CONTEXT: GUID = GUID::from_u128(0x5b0f_6a3e_9c41_4d7e_8a2b_51e0_c7d4_9f13);

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceVolume {
    /// 0..1, the percentage Windows shows for the device.
    pub volume: f32,
    pub muted: bool,
    /// The device changes its own volume (a speaker over Bluetooth, most USB headsets).
    pub hardware: bool,
}

fn endpoint(id: &str) -> Result<IAudioEndpointVolume> {
    let device = device::by_id(id)?;
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }.context("open the device's volume")
}

/// The device's volume. Needs COM.
pub fn get(id: &str) -> Result<DeviceVolume> {
    let endpoint = endpoint(id)?;
    unsafe {
        Ok(DeviceVolume {
            volume: endpoint.GetMasterVolumeLevelScalar()?,
            muted: endpoint.GetMute()?.as_bool(),
            hardware: endpoint.QueryHardwareSupport().is_ok_and(|s| s & HARDWARE_VOLUME != 0),
        })
    }
}

/// Sets the device's volume (0..1). Needs COM.
pub fn set_volume(id: &str, volume: f32) -> Result<()> {
    let endpoint = endpoint(id)?;
    unsafe { endpoint.SetMasterVolumeLevelScalar(volume.clamp(0.0, 1.0), &CONTEXT) }.context("set the device's volume")
}

/// Mutes or unmutes the device. Needs COM.
pub fn set_muted(id: &str, muted: bool) -> Result<()> {
    let endpoint = endpoint(id)?;
    unsafe { endpoint.SetMute(muted, &CONTEXT) }.context("mute the device")
}
