//! The focused app and idle time from Win32: read-only queries that change nothing. One of
//! the two modules allowed `unsafe` (Cargo.toml lints); each call states why it is sound.
#![allow(unsafe_code)]

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Longest executable path read, in UTF-16 units.
const MAX_PATH_UNITS: usize = 1024;

/// The file name of the process that owns the foreground window, e.g. `Code.exe`.
pub fn focused_app() -> Option<String> {
    // SAFETY: no arguments; returns a window handle or null.
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `window` came from the system; `pid` is a valid u32 to write to.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: plain call with a pid; the handle is closed by `Process` on drop.
    let process =
        Process(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buffer = [0u16; MAX_PATH_UNITS];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` units and outlives the call; the call writes at most `len`
    // units and stores the length it wrote in `len`.
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    let path = String::from_utf16_lossy(buffer.get(..len as usize)?);
    path.rsplit(['\\', '/']).next().map(str::to_owned)
}

/// Seconds since the last keyboard or mouse input in this session.
pub fn idle_seconds() -> Option<u64> {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a valid LASTINPUTINFO with `cbSize` set, as the call requires.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    // Both counters wrap after 49.7 days; wrapping subtraction stays right across it.
    Some(u64::from(now.wrapping_sub(info.dwTime)) / 1000)
}

/// A process handle closed on drop.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful OpenProcess and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
