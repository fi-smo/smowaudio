//! One Smowaudio at a time: a second launch hands its arguments to the running copy (which opens
//! its window, or with `--flyout` toggles the tray flyout) and exits, so audio is never opened
//! twice.

use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};

const PIPE: PCWSTR = w!(r"\\.\pipe\Smowaudio.Instance");
const PIPE_PATH: &str = r"\\.\pipe\Smowaudio.Instance";

/// True if another Smowaudio process already holds the instance mutex. The handle is leaked on
/// purpose, so the mutex lives as long as this process.
pub fn another_running() -> bool {
    unsafe { CreateMutexW(None, false, w!("Local\\Smowaudio.SingleInstance")).is_ok() && GetLastError() == ERROR_ALREADY_EXISTS }
}

/// Sends this launch's arguments to the running copy. It may only just have started, so this
/// keeps trying for a few seconds.
pub fn hand_over() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Let the running copy bring its window to the front: Windows only allows that to the app the
    // user just started, which is this one.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(PIPE_PATH) {
            let _ = pipe.write_all(args.join("\n").as_bytes());
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::append_log("a second launch couldn't reach the running Smowaudio");
}

/// Listens for later launches on a thread of its own; `on_launch` gets each one's arguments.
pub fn listen(on_launch: impl Fn(Vec<String>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("Instance".into()).spawn(move || loop {
        let pipe = unsafe {
            CreateNamedPipeW(
                PIPE,
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                4096,
                0,
                None,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            crate::append_log(&format!("later launches can't reach this one: {}", std::io::Error::last_os_error()));
            return;
        }
        let connected = unsafe { ConnectNamedPipe(pipe, None) }.is_ok() || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        // The File owns the handle from here and closes it.
        let mut file = unsafe { std::fs::File::from_raw_handle(pipe.0) };
        if !connected {
            continue;
        }
        let mut text = String::new();
        let _ = Read::take(&mut file, 64 * 1024).read_to_string(&mut text);
        on_launch(text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect());
    });
    if let Err(e) = spawned {
        crate::append_log(&format!("instance listener failed to start: {e}"));
    }
}
