//! An optional diagnostic log.  Explorer runs the handler inside a
//! surrogate process with no console, so when a file named `shell.log`
//! exists beside `slerp_shell.dll`, each thumbnail appends a line to
//! it: the host process, how the handler was initialised, how long it took
//! and any error.  Creating the file turns logging on; deleting it turns it
//! off for processes started afterwards.  It sits beside the DLL because
//! the surrogate may not see the user's profile folders: with UAC off, its
//! `%LOCALAPPDATA%` is the system profile's.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// The log file, when logging is on, and the moment the DLL first logged.
static LOG: OnceLock<(Option<PathBuf>, Instant)> = OnceLock::new();

fn state() -> &'static (Option<PathBuf>, Instant) {
    LOG.get_or_init(|| {
        let path = crate::this_dll().ok().map(|dll| dll.with_file_name("shell.log")).filter(|p| p.is_file());
        (path, Instant::now())
    })
}

/// True when lines are being written, so callers can skip costly details.
pub fn enabled() -> bool {
    state().0.is_some()
}

/// Appends one line, stamped with the process id and the milliseconds since
/// the DLL first logged in this process.
pub fn line(args: std::fmt::Arguments) {
    let (Some(path), start) = state() else { return };
    let text = format!("{:9.1} ms pid {:5} {args}\n", start.elapsed().as_secs_f64() * 1000.0, std::process::id());
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

/// How long ago the current process started, in milliseconds: for a
/// thumbnail host, the cost of starting it is part of the first thumbnail.
pub fn process_age_ms() -> f64 {
    let filetime = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64 / 1e4;
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: the pseudo handle needs no closing; all pointers are valid.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if ok.is_err() {
        return f64::NAN;
    }
    // SAFETY: no preconditions.
    let now = unsafe { GetSystemTimeAsFileTime() };
    filetime(now) - filetime(created)
}

macro_rules! log {
    ($($t:tt)*) => {
        $crate::log::line(format_args!($($t)*))
    };
}
pub(crate) use log;
