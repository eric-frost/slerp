//! `slerpw.exe`: opens files in the Slerp viewer without a console
//! window.  Explorer runs it from the "Open with" entry.
//!
//! `slerp.exe` is a console program, so its command line prints errors
//! and returns exit codes in any terminal; started from Explorer, Windows
//! would give it a console window of its own, which flashes up before the
//! viewer can close it.  This launcher is a Windows-subsystem program, so
//! it gets no console, and it starts `slerp.exe` beside it with its own
//! arguments and no console window either.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    /// CreateProcess flag: the console program runs without a console
    /// window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let exe = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("slerp.exe")));
    let Some(exe) = exe else { return };
    let started = Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if let Err(e) = started {
        show_error(&format!("Slerp couldn't start {}:\n{e}", exe.display()));
    }
}

#[cfg(windows)]
fn show_error(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    // SAFETY: plain message box with valid strings and no owner window.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("Slerp"), MB_OK | MB_ICONERROR) };
}

/// Only Windows needs a launcher.
#[cfg(not(windows))]
fn main() {
    eprintln!("slerpw is the Windows viewer launcher; run slerp instead");
    std::process::exit(1);
}
