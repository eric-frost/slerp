//! A program started to run one job (see `process.rs` for the parent).
//!
//! Before anything else it makes sure a crash ends it quietly: on Windows
//! no "has stopped working" dialog, no C runtime message box, and an
//! unhandled exception ends the process at once with the exception's code
//! as its exit code, which the parent turns into words.  Then it keeps its
//! standard output for the answers and points the standard output and
//! error that libraries print to (jxrlib complains about metadata tags it
//! doesn't know) at the null device, so their text can't corrupt an answer.

use crate::{Jobs, encode, find, guarded};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};

/// The stack of the thread that runs the job.  Decoders recurse into
/// nested structures (SVG groups, PDF forms), and a crafted file can
/// always nest deeper than any stack, but legitimate files shouldn't fail
/// for a stack smaller than a Linux program's main thread gets.
const STACK: usize = 16 << 20;

/// Runs the job called `name` from `groups` for the parent, then exits.
pub fn serve(groups: &'static [Jobs], name: OsString) -> ! {
    quiet_crashes();
    let answers = take_stdout();
    let name = name.to_string_lossy().into_owned();
    let code = std::thread::Builder::new()
        .name("slerp-isolated".into())
        .stack_size(STACK)
        .spawn(move || work(groups, &name, answers))
        .ok()
        .and_then(|t| t.join().ok())
        .unwrap_or(2);
    std::process::exit(code)
}

/// Reads the input, answers, and for a serving job answers each request,
/// until the parent closes its end.  Returns the exit code.
fn work(groups: &'static [Jobs], name: &str, mut answers: File) -> i32 {
    let mut answer = |result: Result<Vec<u8>, String>| {
        let (head, body) = encode(result);
        answers.write_all(&head).and_then(|_| answers.write_all(&body)).and_then(|_| answers.flush()).is_ok()
    };
    let mut stdin = std::io::stdin().lock();
    // 1. The input, which comes before anything else.
    let Some(input) = read_message(&mut stdin) else { return 2 };
    let Some(job) = find(groups, name) else {
        answer(Err(format!("this program has no job called {name:?}")));
        return 0;
    };
    // 2. The first answer.
    let mut session = match guarded(job, || job.open(&input)) {
        Ok((session, opening)) => {
            drop(input);
            if !answer(Ok(opening)) {
                return 2;
            }
            match session {
                Some(s) => s,
                None => return 0,
            }
        }
        Err(e) => {
            answer(Err(e));
            return 0;
        }
    };
    // 3. Requests.
    while let Some(request) = read_message(&mut stdin) {
        if !answer(guarded(job, || session(&request))) {
            return 2;
        }
    }
    0
}

/// A message from the parent: its length as a little-endian u64, then its
/// bytes.  `None` at the end of the input.
fn read_message(from: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 8];
    from.read_exact(&mut len).ok()?;
    let mut message = vec![0u8; usize::try_from(u64::from_le_bytes(len)).ok()?];
    from.read_exact(&mut message).ok()?;
    Some(message)
}

/*┌─────────────────╮
| Unix            |
└─────────────────╯*/
//region Unix

/// Crashes end the process with a signal, which the parent reads, without
/// a core dump.
#[cfg(unix)]
fn quiet_crashes() {
    crate::fork::no_core_dump();
}

/// The standard output as a file of its own for the answers, and the
/// standard output and error descriptors pointed at /dev/null.
#[cfg(unix)]
fn take_stdout() -> File {
    use std::os::fd::FromRawFd;
    // SAFETY: duplicating and replacing this process's own descriptors;
    // the duplicate is owned by the returned file.
    unsafe {
        let answers = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if null >= 0 {
            libc::dup2(null, 1);
            libc::dup2(null, 2);
            libc::close(null);
        }
        if answers < 0 {
            std::process::exit(2);
        }
        File::from_raw_fd(answers)
    }
}

//endregion

/*┌─────────────────╮
| Windows         |
└─────────────────╯*/
//region Windows

/// No dialogs and no waiting on a crash: Windows Error Reporting shows no
/// window, a missing disk or file shows no box, the C runtime's `abort`
/// (a failed `assert` in C code) ends the process with exit code 3 at once
/// instead of offering a debugger or a message box, and any exception
/// nothing handles (a bad pointer, a stack overflow) ends the process with
/// the exception's code before Windows Error Reporting gathers a report.
/// Rust's own abort (a failed allocation) is a "fail fast" that skips all
/// of these; only the flags keep its report quiet.
#[cfg(windows)]
fn quiet_crashes() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, SetErrorMode,
        SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::ErrorReporting::{WER_FAULT_REPORTING_NO_UI, WerSetFlags};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    unsafe extern "system" fn end_now(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes a valid record for the exception.
        let code = unsafe {
            info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()).map_or(0xC000_0005u32 as i32, |r| r.ExceptionCode)
        };
        // SAFETY: ends this process.
        unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
        0
    }
    extern "C" fn aborted(_signal: libc::c_int) {
        // SAFETY: ends this process without running anything else.
        unsafe { libc::_exit(3) }
    }
    unsafe extern "C" {
        fn _set_error_mode(mode: libc::c_int) -> libc::c_int;
    }
    /// `_set_error_mode`'s choice of standard error over a message box.
    const OUT_TO_STDERR: libc::c_int = 1;
    // SAFETY: process-wide settings with valid arguments; the handlers are
    // functions that live as long as the program.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        WerSetFlags(WER_FAULT_REPORTING_NO_UI);
        SetUnhandledExceptionFilter(Some(end_now));
        libc::signal(libc::SIGABRT, aborted as *const () as libc::sighandler_t);
        _set_error_mode(OUT_TO_STDERR);
    }
}

/// The standard output as a file of its own for the answers, and the
/// standard output and error, both Windows' and the C runtime's, pointed at
/// the null device.
#[cfg(windows)]
fn take_stdout() -> File {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: duplicating this process's own standard output handle, which
    // the returned file then owns; the C runtime's descriptors 1 and 2 are
    // replaced (closing the original handle, hence the duplicate first).
    unsafe {
        let mut answers: HANDLE = std::ptr::null_mut();
        let me = GetCurrentProcess();
        let ok = DuplicateHandle(me, GetStdHandle(STD_OUTPUT_HANDLE), me, &mut answers, 0, 0, DUPLICATE_SAME_ACCESS);
        let null = libc::open(c"NUL".as_ptr(), libc::O_WRONLY);
        if null >= 0 {
            libc::dup2(null, 1);
            libc::dup2(null, 2);
            libc::close(null);
        }
        if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") {
            use std::os::windows::io::IntoRawHandle;
            let handle = nul.into_raw_handle() as HANDLE;
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
        if ok == 0 || answers.is_null() {
            std::process::exit(2);
        }
        File::from_raw_handle(answers as _)
    }
}

//endregion

#[cfg(not(any(unix, windows)))]
fn quiet_crashes() {}

#[cfg(not(any(unix, windows)))]
fn take_stdout() -> File {
    std::process::exit(2)
}
