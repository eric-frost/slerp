//! The thumbnail helper: a per-user process that loads files and draws
//! thumbnails for the handler inside Explorer, over a named pipe.
//!
//! Explorer has to load the handler in its own process to hand it real
//! paths (see `thumbnail.rs`), but decoders and a GPU device don't belong
//! in Explorer: a crash there takes the desktop with it, and every program
//! that shows a file dialog would start its own GPU.  So the handler only
//! sends the path to `slerp.exe helper`, started on demand beside the
//! DLL, which keeps one GPU renderer for every Explorer window and dialog
//! and exits after `helperIdleSeconds` without requests, like the Linux
//! helper.
//!
//! The pipe is named per user and session, only that user may open it,
//! remote clients are refused, and the first helper to create it owns it,
//! so racing starts end with one helper.  All I/O is overlapped with
//! deadlines, so a stuck helper can't hang Explorer's thumbnail threads.

use crate::log::log;
use crate::protocol::{self, Reply, Request};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, CreateProcessW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
    PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};

/// How long the handler waits for a helper to start and accept.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the handler waits for a thumbnail.  The first 3D thumbnail on
/// Direct3D's software renderer takes about two seconds; huge files more.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the helper waits for a request, and for the handler to hang up
/// once it has the reply.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/*┌─────────────────╮
| Client          |
└─────────────────╯*/
//region Client

/// Asks the helper for a thumbnail of `path`, starting it if it isn't
/// running.
pub fn request(path: &Path, size: u32) -> Result<Reply, String> {
    let name = pipe_name().map_err(|e| format!("pipe name: {e}"))?;
    let mut pipe = connect(&name, Instant::now() + CONNECT_TIMEOUT)?;
    pipe.deadline = Instant::now() + REPLY_TIMEOUT;
    let req = Request { size, path: path.to_string_lossy().into_owned() };
    protocol::write_request(&mut pipe, &req).map_err(|e| format!("sending the request: {e}"))?;
    protocol::read_reply(&mut pipe).map_err(|e| format!("reading the reply: {e}"))
}

/// Opens the helper's pipe, starting the helper the first time it's
/// missing.
fn connect(name: &HSTRING, deadline: Instant) -> Result<Pipe, String> {
    let mut started = false;
    loop {
        // SAFETY: plain CreateFileW on a pipe name; the handle is owned
        // by the returned Pipe.
        let opened = unsafe {
            CreateFileW(
                name,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        let error = match opened {
            Ok(handle) => return Pipe::new(handle, deadline).map_err(|e| e.to_string()),
            Err(e) => e,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("the thumbnail helper didn't answer: {error}"));
        }
        if error.code() == ERROR_PIPE_BUSY.to_hresult() {
            // SAFETY: valid name; waits for a free pipe instance.
            let _ = unsafe { WaitNamedPipeW(name, remaining.as_millis().min(1000) as u32) };
        } else if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() {
            if !started {
                start_helper()?;
                started = true;
            }
            std::thread::sleep(Duration::from_millis(15));
        } else {
            return Err(format!("opening the thumbnail helper's pipe: {error}"));
        }
    }
}

/// Starts `slerp.exe helper` from the DLL's folder, with no console
/// window and none of the host's handles: Explorer's open files mustn't be
/// held by the helper.  Its current folder is Windows' own, since a running
/// process's current folder can't be deleted: uninstalling Slerp must be
/// able to remove its folder while a helper is still idling, and the host's
/// current folder may be one the user wants to delete.
fn start_helper() -> Result<(), String> {
    let dll = crate::this_dll().map_err(|e| e.to_string())?;
    let exe = dll.with_file_name("slerp.exe");
    if !exe.is_file() {
        return Err(format!("{} is missing", exe.display()));
    }
    let windows = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = HSTRING::from(windows.as_os_str());
    let mut command: Vec<u16> = format!("\"{}\" helper", exe.display()).encode_utf16().chain([0]).collect();
    let startup = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: valid NUL-terminated strings; the command line buffer is
    // writable as CreateProcessW requires.
    unsafe {
        CreateProcessW(
            &HSTRING::from(exe.as_os_str()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            &dir,
            &startup,
            &mut info,
        )
        .map_err(|e| format!("starting {}: {e}", exe.display()))?;
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    log!("started the thumbnail helper, pid {}", info.dwProcessId);
    Ok(())
}

//endregion

/*┌─────────────────╮
| Server          |
└─────────────────╯*/
//region Server

/// Runs the helper until it has been idle for `idle`.  Returns at once
/// when another helper already serves this user and session.
pub fn serve(idle: Duration) -> Result<(), String> {
    let name = pipe_name().map_err(|e| format!("pipe name: {e}"))?;
    let sid = user_sid().map_err(|e| format!("user: {e}"))?;
    // Only this user (and the system) may open the pipe.
    let security = SecurityDescriptor::new(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    let active = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(std::sync::Mutex::new(Instant::now()));
    let mut first = true;
    log!("thumbnail helper serving");
    loop {
        // 1. A fresh pipe instance for the next client.
        let mut flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: valid name and attributes, which outlive the call.
        let handle = unsafe {
            CreateNamedPipeW(
                &name,
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                Some(&attributes),
            )
        };
        if handle.is_invalid() {
            let error = io::Error::last_os_error();
            if first {
                // Another helper got there first.
                log!("another thumbnail helper is running ({error})");
                return Ok(());
            }
            return Err(format!("creating a pipe instance: {error}"));
        }
        first = false;
        let mut pipe = Pipe::new(handle, Instant::now()).map_err(|e| e.to_string())?;
        // 2. Wait for a client, or exit once idle.
        if !pipe.accept(|| active.load(Ordering::SeqCst) == 0 && last_elapsed(&last) >= idle)? {
            log!("thumbnail helper idle for {} s, exiting", idle.as_secs());
            return Ok(());
        }
        // 3. Serve it on its own thread: loading runs in parallel, drawing
        //    takes turns on the shared renderer.
        active.fetch_add(1, Ordering::SeqCst);
        let (active, last) = (active.clone(), last.clone());
        std::thread::spawn(move || {
            pipe.deadline = Instant::now() + REQUEST_TIMEOUT;
            if let Err(e) = answer(&mut pipe) {
                log!("helper request failed: {e}");
            }
            *last.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn last_elapsed(last: &std::sync::Mutex<Instant>) -> Duration {
    last.lock().unwrap_or_else(|p| p.into_inner()).elapsed()
}

fn answer(pipe: &mut Pipe) -> io::Result<()> {
    let req = protocol::read_request(pipe)?;
    let start = Instant::now();
    let reply = crate::render::thumbnail(Path::new(&req.path), req.size);
    match &reply {
        Reply::Image { width, height, .. } => {
            log!("helper drew {} at {}: {width}x{height} in {:.1} ms", req.path, req.size, ms(start))
        }
        Reply::Unknown => log!("helper: {} isn't a format Slerp reads", req.path),
        Reply::Failed(e) => log!("helper failed on {}: {e}", req.path),
    }
    pipe.deadline = Instant::now() + REQUEST_TIMEOUT;
    protocol::write_reply(pipe, &reply)?;
    // Wait for the handler to hang up, so closing doesn't throw away a
    // reply it hasn't read yet.
    let _ = pipe.read(&mut [0u8; 1]);
    Ok(())
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

//endregion

/*┌─────────────────╮
| Pipes           |
└─────────────────╯*/
//region Pipes

/// An overlapped pipe handle whose reads and writes give up at `deadline`.
struct Pipe {
    handle: HANDLE,
    event: HANDLE,
    deadline: Instant,
}

// SAFETY: the handles are plain kernel handles, usable from any thread.
unsafe impl Send for Pipe {}

impl Pipe {
    fn new(handle: HANDLE, deadline: Instant) -> io::Result<Pipe> {
        // SAFETY: an unnamed manual-reset event.
        match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => Ok(Pipe { handle, event, deadline }),
            Err(e) => {
                // SAFETY: the handle is ours to close.
                let _ = unsafe { CloseHandle(handle) };
                Err(e.into())
            }
        }
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED { hEvent: self.event, ..Default::default() }
    }

    /// Waits for a client to connect.  Returns false when `give_up` says
    /// so while nobody is connecting; it's asked every second.
    fn accept(&mut self, give_up: impl Fn() -> bool) -> Result<bool, String> {
        let mut ov = self.overlapped();
        // SAFETY: ov and its event outlive the operation: it completes or
        // is cancelled and waited for before this returns.
        match unsafe { ConnectNamedPipe(self.handle, Some(&mut ov)) } {
            Ok(()) => return Ok(true),
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => return Ok(true),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
            Err(e) => return Err(format!("waiting for a client: {e}")),
        }
        loop {
            // SAFETY: a valid event handle.
            let waited = unsafe { WaitForSingleObject(self.event, 1000) };
            if waited == WAIT_OBJECT_0 {
                let mut n = 0;
                // SAFETY: the operation has completed.
                return unsafe { GetOverlappedResult(self.handle, &ov, &mut n, false) }
                    .map(|_| true)
                    .map_err(|e| format!("accepting a client: {e}"));
            }
            if waited != WAIT_TIMEOUT {
                return Err(format!("waiting for a client: {}", io::Error::last_os_error()));
            }
            if give_up() {
                self.cancel(&mut ov);
                return Ok(false);
            }
        }
    }

    /// Waits for an overlapped operation until the deadline, cancelling it
    /// if the deadline passes.  Returns the bytes transferred.
    fn finish(&self, ov: &mut OVERLAPPED, started: windows::core::Result<()>) -> io::Result<u32> {
        if let Err(e) = started {
            if e.code() != ERROR_IO_PENDING.to_hresult() {
                return Err(e.into());
            }
            let wait = self.deadline.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128 - 1);
            // SAFETY: a valid event handle.
            if unsafe { WaitForSingleObject(self.event, wait as u32) } != WAIT_OBJECT_0 {
                self.cancel(ov);
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the other side took too long"));
            }
        }
        let mut n = 0;
        // SAFETY: the operation has completed.
        unsafe { GetOverlappedResult(self.handle, ov, &mut n, false) }?;
        Ok(n)
    }

    /// Cancels the operation and waits until the system lets go of `ov`.
    fn cancel(&self, ov: &mut OVERLAPPED) {
        let mut n = 0;
        // SAFETY: ov belongs to an operation on this handle.
        unsafe {
            let _ = CancelIoEx(self.handle, Some(ov));
            let _ = GetOverlappedResult(self.handle, ov, &mut n, true);
        }
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(1 << 20);
        let mut ov = self.overlapped();
        // SAFETY: buf and ov outlive the operation, which finish() waits
        // for or cancels.
        let started = unsafe { ReadFile(self.handle, Some(&mut buf[..len]), None, Some(&mut ov)) };
        match self.finish(&mut ov, started) {
            Ok(n) => Ok(n as usize),
            // The other side hung up: end of file.
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE.0 as i32) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(1 << 20);
        let mut ov = self.overlapped();
        // SAFETY: as for read.
        let started = unsafe { WriteFile(self.handle, Some(&buf[..len]), None, Some(&mut ov)) };
        self.finish(&mut ov, started).map(|n| n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: both handles are ours; no operation is pending.
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

/// A security descriptor made from SDDL, freed on drop.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn new(sddl: &str) -> Result<Self, String> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid SDDL string; sd receives a LocalAlloc'd descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None)
        }
        .map_err(|e| format!("pipe security: {e}"))?;
        Ok(SecurityDescriptor(sd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// `\\.\pipe\slerp-thumbnails-<session>-<user SID>`: one helper per
/// user and session, since sessions see different GPUs and desktops.
fn pipe_name() -> windows::core::Result<HSTRING> {
    let mut session = 0;
    // SAFETY: plain query for this process.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session)? };
    Ok(HSTRING::from(format!(r"\\.\pipe\slerp-thumbnails-{session}-{}", user_sid()?)))
}

/// The current user's SID as a string such as `S-1-5-21-...-1001`.
fn user_sid() -> windows::core::Result<String> {
    // SAFETY: the token handle is closed below; the buffer is sized by the
    // first call and holds a TOKEN_USER whose SID points into it.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        got?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
        let sid = text.to_string().unwrap_or_default();
        LocalFree(Some(HLOCAL(text.0.cast())));
        Ok(sid)
    }
}

//endregion
