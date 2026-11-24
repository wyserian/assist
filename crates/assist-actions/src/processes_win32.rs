//! Suspending and resuming a process on Windows. Windows has no documented call for a whole
//! process, so this uses ntdll's `NtSuspendProcess` / `NtResumeProcess`, as Process Explorer
//! does; PLAN.md calls Windows pause best effort. One of the two modules allowed `unsafe`
//! (Cargo.toml lints); each call states why it is sound.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SUSPEND_RESUME};

#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(process: HANDLE) -> i32;
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// Suspends every thread of process `pid`.
pub fn suspend(pid: u32) -> Result<(), String> {
    let process = open(pid)?;
    // SAFETY: `process` is a valid handle with PROCESS_SUSPEND_RESUME access, open until
    // `Process` drops after this call.
    let status = unsafe { NtSuspendProcess(process.0) };
    check(status, "suspend", pid)
}

/// Resumes process `pid`.
pub fn resume(pid: u32) -> Result<(), String> {
    let process = open(pid)?;
    // SAFETY: as in `suspend`.
    let status = unsafe { NtResumeProcess(process.0) };
    check(status, "resume", pid)
}

fn open(pid: u32) -> Result<Process, String> {
    // SAFETY: plain call with a pid; the handle is closed by `Process` on drop.
    unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, false, pid) }
        .map(Process)
        .map_err(|err| format!("cannot open process {pid}: {}", err.message()))
}

/// NTSTATUS: negative means failure.
fn check(status: i32, what: &str, pid: u32) -> Result<(), String> {
    if status >= 0 {
        Ok(())
    } else {
        Err(format!("cannot {what} process {pid} (status {status:#x})"))
    }
}

/// A process handle closed on drop.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful OpenProcess and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
