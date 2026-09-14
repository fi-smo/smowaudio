//! Notifications for audio device changes: plugged in, removed, enabled/disabled, or a new
//! Windows default device.

#![allow(non_snake_case)]

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    EDataFlow, ERole, IMMNotificationClient, IMMNotificationClient_Impl, DEVICE_STATE,
};
use windows_core::{implement, PCWSTR};

use super::device;

#[implement(IMMNotificationClient)]
struct Listener {
    // Windows calls these from its own threads.
    events: Mutex<Sender<()>>,
}

impl Listener {
    fn notify(&self) {
        if let Ok(events) = self.events.lock() {
            let _ = events.send(());
        }
    }
}

impl IMMNotificationClient_Impl for Listener_Impl {
    fn OnDeviceStateChanged(&self, _device: &PCWSTR, _state: DEVICE_STATE) -> windows_core::Result<()> {
        self.notify();
        Ok(())
    }

    fn OnDeviceAdded(&self, _device: &PCWSTR) -> windows_core::Result<()> {
        self.notify();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device: &PCWSTR) -> windows_core::Result<()> {
        self.notify();
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _device: &PCWSTR) -> windows_core::Result<()> {
        self.notify();
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _device: &PCWSTR, _key: &PROPERTYKEY) -> windows_core::Result<()> {
        Ok(())
    }
}

/// Calls `on_change` after audio devices change, once things have been quiet for 1.5 s (plugging
/// in a USB headset fires a burst of events). Runs until `keep_going` returns false.
pub fn watch(keep_going: impl Fn() -> bool, mut on_change: impl FnMut()) -> Result<()> {
    let enumerator = device::enumerator()?;
    let (tx, rx) = channel();
    let client: IMMNotificationClient = Listener { events: Mutex::new(tx) }.into();
    unsafe { enumerator.RegisterEndpointNotificationCallback(&client)? };

    while keep_going() {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(()) => {
                let mut last_event = Instant::now();
                while last_event.elapsed() < Duration::from_millis(1500) {
                    if rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                        last_event = Instant::now();
                    }
                }
                on_change();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    unsafe { enumerator.UnregisterEndpointNotificationCallback(&client)? };
    Ok(())
}
