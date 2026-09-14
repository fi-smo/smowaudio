//! Windows audio plumbing: device discovery, WASAPI streams, clock drift handling
//! and per-app routing.

pub mod defaults;
pub mod device;
pub mod notify;
pub mod resample;
pub mod routing;
pub mod stream;

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Initializes COM (MTA) for the lifetime of the guard.
pub struct ComGuard;

impl ComGuard {
    pub fn new() -> Self {
        // S_FALSE (already initialized) is fine; a mode mismatch just means COM is usable already.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Enables flush-to-zero / denormals-are-zero so filters decaying to silence don't spike CPU.
pub fn disable_denormals() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        #[allow(deprecated)]
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        #[allow(deprecated)]
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
}
