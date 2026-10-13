//! The parent's side of a job running in a newly started program (see
//! `child.rs` for the other side): how Windows isolates work, and how any
//! system does when `SLERP_ISOLATE_PROGRAM` is set.
//!
//! The program gets the job's name on its command line, the input and then
//! each request on standard input (a little-endian u64 length, then the
//! bytes), and answers each on standard output (a tag byte, a u64 length,
//! the bytes; see [`crate::encode`]).  A thread writes to the child and
//! another reads from it, so the parent only ever waits on a channel with a
//! deadline: a child that hangs, whether before reading its input or while
//! working, is killed when its time is up.

use crate::{CHILD_FLAG, Job, Limits, NO_ANSWER, TOO_LARGE, decode, say, too_slow};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

/// Memory a freshly started program needs for itself before the work's
/// own, which the memory limit adds to `Limits::memory`: the program's
/// writable data, its heap at start and the C runtime's.
const PROGRAM_MEMORY: u64 = 128 << 20;

/// What the reading thread passes on.
enum Frame {
    Answer(Result<Vec<u8>, String>),
    TooLarge,
    /// The child closed its output, so it has ended or is ending.
    Ended,
}

/// A running child program.
pub struct Child {
    job: &'static Job,
    process: std::process::Child,
    /// Holds the child in its Windows job object, which kills it when
    /// closed.
    #[cfg(windows)]
    _contained: Option<windows::JobObject>,
    to_child: Option<Sender<Vec<u8>>>,
    from_child: Receiver<Frame>,
    pub limits: Limits,
    alive: bool,
}

impl Child {
    /// Starts `program` on `job` and returns it with the job's first
    /// answer.
    pub fn start(program: &Path, job: &'static Job, input: &[u8], limits: Limits) -> Result<(Child, Vec<u8>), String> {
        // 1. The program, with nothing on its standard error and no window.
        let mut command = Command::new(program);
        command.arg(CHILD_FLAG).arg(job.name).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        prepare(&mut command, job, &limits);
        let mut process = command.spawn().map_err(|e| format!("couldn't start {}: {e}", program.display()))?;

        // 2. Limits, before the child has anything to work on.
        #[cfg(windows)]
        let contained = windows::JobObject::contain(&process, &limits);

        // 3. A thread for each direction.
        let (Some(mut stdin), Some(mut stdout)) = (process.stdin.take(), process.stdout.take()) else {
            let _ = process.kill();
            let _ = process.wait();
            return Err("couldn't talk to the started program".into());
        };
        let (to_child, requests) = channel::<Vec<u8>>();
        let (answers, from_child) = channel::<Frame>();
        let writer = std::thread::Builder::new().name("slerp-isolate-in".into()).spawn(move || {
            for message in requests {
                if stdin.write_all(&message).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let cap = limits.output;
        let reader = std::thread::Builder::new().name("slerp-isolate-out".into()).spawn(move || {
            loop {
                let frame = read_frame(&mut stdout, cap);
                let last = !matches!(frame, Frame::Answer(_));
                if answers.send(frame).is_err() || last {
                    break;
                }
            }
        });
        let mut child = Child {
            job,
            process,
            #[cfg(windows)]
            _contained: contained,
            to_child: Some(to_child),
            from_child,
            limits,
            alive: true,
        };
        if writer.is_err() || reader.is_err() {
            child.end();
            return Err("couldn't start a thread".into());
        }

        // 4. The input, and the first answer.
        child.send(input);
        match child.answer(limits.time) {
            Ok(opening) => Ok((child, opening)),
            Err(e) => {
                child.end();
                Err(e)
            }
        }
    }

    pub fn alive(&self) -> bool {
        self.alive
    }

    pub fn ask(&mut self, request: &[u8], time: Duration) -> Result<Vec<u8>, String> {
        if !self.alive {
            return Err(say(self.job, "has stopped"));
        }
        self.send(request);
        self.answer(time)
    }

    /// Hands a message to the writing thread, with its length first.
    fn send(&mut self, message: &[u8]) {
        let mut framed = Vec::with_capacity(8 + message.len());
        framed.extend((message.len() as u64).to_le_bytes());
        framed.extend_from_slice(message);
        if let Some(to_child) = &self.to_child {
            let _ = to_child.send(framed);
        }
    }

    /// Waits for the next answer for `time`.  A failure ends the child.
    fn answer(&mut self, time: Duration) -> Result<Vec<u8>, String> {
        let job = self.job;
        match self.from_child.recv_timeout(time) {
            Ok(Frame::Answer(answer)) => answer,
            Ok(Frame::TooLarge) => {
                self.end();
                Err(say(job, TOO_LARGE))
            }
            Ok(Frame::Ended) | Err(RecvTimeoutError::Disconnected) => Err(say(job, &self.how_it_ended())),
            Err(RecvTimeoutError::Timeout) => {
                self.end();
                Err(say(job, &too_slow(time)))
            }
        }
    }

    /// Waits a little for a child whose output closed to end, and says how
    /// it ended.
    fn how_it_ended(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            match self.process.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                _ => break None,
            }
        };
        self.end();
        status.and_then(crashed).unwrap_or_else(|| NO_ANSWER.into())
    }

    /// Closes the child's input and kills and reaps it.
    fn end(&mut self) {
        if !self.alive {
            return;
        }
        self.alive = false;
        self.to_child = None;
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.end();
    }
}

/// Reads one answer, or says why there is none.
fn read_frame(from: &mut impl Read, cap: usize) -> Frame {
    let mut head = [0u8; 9];
    if from.read_exact(&mut head).is_err() {
        return Frame::Ended;
    }
    let len = u64::from_le_bytes(head[1..].try_into().unwrap_or([0; 8]));
    if len > cap as u64 {
        return Frame::TooLarge;
    }
    let mut body = vec![0u8; len as usize];
    if from.read_exact(&mut body).is_err() {
        return Frame::Ended;
    }
    Frame::Answer(decode(head[0], body))
}

/*┌─────────────────╮
| Unix            |
└─────────────────╯*/
//region Unix

/// Limits the child before it runs the program, as a forked child gets
/// them: memory (Linux), and processor time for one-shot work (a worker's
/// requests are timed one by one instead).
#[cfg(unix)]
fn prepare(command: &mut Command, job: &Job, limits: &Limits) {
    use std::os::unix::process::CommandExt;
    let data = limits.memory.saturating_add(PROGRAM_MEMORY);
    let cpu: libc::rlim_t = if job.is_once() { (limits.time.as_secs() + 2) as _ } else { libc::RLIM_INFINITY };
    // SAFETY: setrlimit is async-signal-safe and takes valid structs; the
    // closure allocates nothing.
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                let d = libc::rlimit { rlim_cur: data as _, rlim_max: data as _ };
                libc::setrlimit(libc::RLIMIT_DATA, &d);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = data;
            let c = libc::rlimit { rlim_cur: cpu, rlim_max: cpu };
            libc::setrlimit(libc::RLIMIT_CPU, &c);
            Ok(())
        });
    }
}

/// What a child that didn't end normally did, as the end of a sentence.
#[cfg(unix)]
fn crashed(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    match (status.signal(), status.code()) {
        (Some(libc::SIGKILL), _) => None,
        (Some(signal), _) => Some(crate::fork::crashed(signal)),
        (None, Some(0)) => None,
        (None, Some(code)) => Some(format!("stopped with exit code {code}")),
        (None, None) => None,
    }
}

//endregion

/*┌─────────────────╮
| Windows         |
└─────────────────╯*/
//region Windows

/// No console window: a console program started from a windowed one (the
/// viewer, or Explorer's thumbnail helper) would otherwise get one, which
/// flashes up.  `DETACHED_PROCESS` gives it no console at all, which also
/// saves starting a console host for it.
#[cfg(windows)]
fn prepare(command: &mut Command, _job: &Job, _limits: &Limits) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(DETACHED_PROCESS);
}

/// What a child that didn't end normally did, from its exit code.  Windows
/// reports a crash as the exception's code; `child.rs` makes the C
/// runtime's abort exit with 3.
#[cfg(windows)]
fn crashed(status: std::process::ExitStatus) -> Option<String> {
    let code = status.code()? as u32;
    Some(match code {
        0 => return None,
        3 => "crashed (it aborted)".into(),
        0xC000_00FD => "crashed (its stack overflowed, perhaps from nesting too deep)".into(),
        0xC000_0005 => "crashed (it read or wrote memory it doesn't own)".into(),
        0xC000_0409 => "crashed or ran out of memory".into(),
        0xC000_0017 | 0xC000_012D => "ran out of memory".into(),
        0xC000_001D => "crashed (an illegal instruction)".into(),
        0xC000_0094 => "crashed (it divided by zero)".into(),
        c if c < 0x100 => format!("stopped with exit code {c}"),
        c => format!("crashed (exit code {c:#010x})"),
    })
}

#[cfg(windows)]
mod windows {
    use crate::Limits;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    /// A job object holding one child: it limits the child's committed
    /// memory, makes an unhandled exception end it without Windows'
    /// crash dialog, and kills it when the handle closes, so a child
    /// outlives neither its parent nor its `Child`.
    pub struct JobObject(HANDLE);

    // SAFETY: a job object handle may be used and closed from any thread.
    unsafe impl Send for JobObject {}

    impl JobObject {
        /// Puts `process` into a new job object with `limits`, or returns
        /// `None` when Windows refuses, in which case the child still runs
        /// and is still killed on time, only without the memory limit.
        pub fn contain(process: &std::process::Child, limits: &Limits) -> Option<JobObject> {
            // SAFETY: plain Win32 calls with valid arguments; the job handle
            // is owned by the returned value and closed once.
            unsafe {
                let job = JobObject(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
                if job.0.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
                if limits.memory != u64::MAX {
                    let bytes = limits.memory.saturating_add(super::PROGRAM_MEMORY);
                    info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
                    info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                }
                let set = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 || AssignProcessToJobObject(job.0, process.as_raw_handle() as HANDLE) == 0 {
                    return None;
                }
                Some(job)
            }
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            // SAFETY: the handle is this value's own.
            unsafe { CloseHandle(self.0) };
        }
    }
}

//endregion
