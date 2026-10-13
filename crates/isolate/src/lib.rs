//! Running code that reads untrusted files where a fault in it costs one
//! picture, not the program.
//!
//! Decoders run on whatever a user browses past, downloads included.  Some
//! are C libraries, some are Rust whose authors could not bound everything
//! (a PDF can nest, loop and inflate in more ways than any renderer
//! checks), and a crafted file can make them crash, abort on a failed
//! allocation, overflow the stack or run for ever.  So that work runs in a
//! separate process with a time and a memory limit, and a crash, a hang or
//! a runaway allocation there becomes an error message here.
//!
//! The work is a [`Job`]: a named function from bytes to bytes.  A name
//! rather than a closure, because on Windows the work runs in a newly
//! started program, which can be told which function to run but can't be
//! handed a closure.  Each crate with risky decoders defines its jobs as
//! statics and packs what the work needs (file bytes, a path, sizes) into
//! the job's input with [`Pack`].  [`run`] does one piece of work; a
//! [`Worker`] keeps a process for a series of requests that share state,
//! such as the pages of one open document, which is read once and then
//! drawn many times.  Callers use the same calls on every system; how the
//! work is kept apart differs:
//!
//! - On Unix the process forks, and the child calls the job's function
//!   directly with the parent's memory as it was.  The child gets a memory
//!   limit (`RLIMIT_DATA` over what the parent holds, Linux only, since
//!   macOS doesn't enforce it for malloc's memory) and a processor-time
//!   limit, and is killed when its time is up.
//! - Windows has no fork, so it starts a program that serves jobs: a
//!   program calls [`serve_jobs`] first thing in `main` with the jobs it
//!   knows, which makes it answer `--isolated-job NAME` (input on standard
//!   input, the answer on standard output) and makes it the program its own
//!   jobs run in.  `slerp.exe` is that program for all of Slerp's jobs, and
//!   Explorer's thumbnail DLL names the `slerp.exe` beside it with
//!   [`use_program`].  The child runs in a Windows job object, which limits
//!   its memory and kills it when the parent lets go of it, without a
//!   console window and without crash dialogs.
//! - Where no program serves a job (test programs, other programs using
//!   Slerp's crates on Windows), the work runs on a thread of its own
//!   behind a panic guard, and a thread that overruns its time is
//!   abandoned: it goes on until it ends, but the caller no longer waits
//!   for it.  A crash there still ends the whole process.
//!
//! The environment variable `SLERP_ISOLATE_PROGRAM` names a program that
//! serves jobs (`slerp` itself), and when set, every job runs in it, on
//! every system: that is how the tests of the decoder crates reach a real
//! child process on Windows, and how the child-process code is tested on
//! Linux.
//!
//! This crate depends on nothing of Slerp's, so any loader may use it.

mod child;
#[cfg(unix)]
mod fork;
mod pack;
mod process;
#[doc(hidden)]
pub mod testing;
#[cfg(not(unix))]
mod thread;

pub use pack::{Pack, Unpack};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/*┌─────────────────╮
| Jobs            |
└─────────────────╯*/
//region Jobs

/// What the work may use.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Wall-clock time the work may take: all of it for [`run`], each
    /// request (and the opening) for a [`Worker`].
    pub time: Duration,
    /// Bytes of memory the work may use beyond what its process starts
    /// with: on Unix beyond what the forking parent holds (Linux only),
    /// on Windows beyond what a freshly started program needs.
    /// `u64::MAX` sets no limit.
    pub memory: u64,
    /// The most bytes one answer may have.
    pub output: usize,
}

/// The state of a serving job, which answers one request at a time.
pub type Session = Box<dyn FnMut(&[u8]) -> Result<Vec<u8>, String>>;

/// What opening a serving job gives: the state that answers requests, and
/// the first answer.
pub type Opened = (Session, Vec<u8>);

/// The jobs one crate defines, as it exports them.
pub type Jobs = &'static [&'static Job];

/// A piece of work that runs isolated, known by its name in every process.
pub struct Job {
    name: &'static str,
    what: &'static str,
    work: Work,
}

#[derive(Clone, Copy)]
enum Work {
    Once(fn(&[u8]) -> Result<Vec<u8>, String>),
    Serving(fn(&[u8]) -> Result<Opened, String>),
}

impl Job {
    /// One-shot work for [`run`]: `work` turns the input into the answer.
    /// `name` must be unique among all jobs of a program (crates prefix
    /// theirs, as in "svg.picture"); `what` names whoever does the work in
    /// error messages ("the SVG renderer"), which read like "the SVG
    /// renderer crashed".
    pub const fn once(name: &'static str, what: &'static str, work: fn(&[u8]) -> Result<Vec<u8>, String>) -> Job {
        Job { name, what, work: Work::Once(work) }
    }

    /// Work for a [`Worker`]: `open` makes the state from the input (opening
    /// a document, say) and returns it with an opening answer, and the
    /// state then answers each request.
    pub const fn serving(name: &'static str, what: &'static str, open: fn(&[u8]) -> Result<Opened, String>) -> Job {
        Job { name, what, work: Work::Serving(open) }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Whether this is one-shot work rather than a serving job.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn is_once(&self) -> bool {
        matches!(self.work, Work::Once(_))
    }

    /// Runs the work's first step: the answer, and the state that serves
    /// requests when the job has any.
    fn open(&self, input: &[u8]) -> Result<(Option<Session>, Vec<u8>), String> {
        match self.work {
            Work::Once(work) => work(input).map(|answer| (None, answer)),
            Work::Serving(open) => open(input).map(|(session, answer)| (Some(session), answer)),
        }
    }
}

/// The job called `name` among `groups`.
fn find(groups: &[Jobs], name: &str) -> Option<&'static Job> {
    groups.iter().flat_map(|g| g.iter()).find(|j| j.name == name).copied()
}

//endregion

/*┌─────────────────╮
| Running         |
└─────────────────╯*/
//region Running

/// Runs `job` on `input` where a fault in it cannot take the caller down,
/// and returns the bytes it produced or why it failed.  The job's own
/// errors come back as they are; failures of the process read like "the
/// SVG renderer crashed (...)".
pub fn run(job: &'static Job, input: &[u8], limits: Limits) -> Result<Vec<u8>, String> {
    match way(job) {
        Way::Program(program) => process::Child::start(&program, job, input, limits).map(|(_, answer)| answer),
        #[cfg(unix)]
        Way::Here => fork::run(job, input, limits),
        #[cfg(not(unix))]
        Way::Here => thread::run(job, input, limits),
    }
}

/// A serving job kept running between requests.  A crash, a hang or a too
/// large answer ends it, and that request and every later one fail; the
/// caller may start a new one.
pub struct Worker(Backend);

enum Backend {
    Program(process::Child),
    #[cfg(unix)]
    Here(fork::Worker),
    #[cfg(not(unix))]
    Here(thread::Worker),
}

impl Worker {
    /// Starts `job` on `input` and returns the worker with the job's
    /// opening answer, or the job's error (or why it never answered within
    /// `limits.time`).
    pub fn start(job: &'static Job, input: &[u8], limits: Limits) -> Result<(Worker, Vec<u8>), String> {
        match way(job) {
            Way::Program(program) => {
                process::Child::start(&program, job, input, limits).map(|(c, a)| (Worker(Backend::Program(c)), a))
            }
            #[cfg(unix)]
            Way::Here => fork::Worker::start(job, input, limits).map(|(w, a)| (Worker(Backend::Here(w)), a)),
            #[cfg(not(unix))]
            Way::Here => thread::Worker::start(job, input, limits).map(|(w, a)| (Worker(Backend::Here(w)), a)),
        }
    }

    /// Sends a request and waits for its answer for the worker's time limit.
    pub fn ask(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
        let time = match &self.0 {
            Backend::Program(c) => c.limits.time,
            Backend::Here(w) => w.limits.time,
        };
        self.ask_within(request, time)
    }

    /// Sends a request and waits for its answer for `time`.
    pub fn ask_within(&mut self, request: &[u8], time: Duration) -> Result<Vec<u8>, String> {
        match &mut self.0 {
            Backend::Program(c) => c.ask(request, time),
            Backend::Here(w) => w.ask(request, time),
        }
    }

    /// Whether the worker is still there to ask.
    pub fn alive(&self) -> bool {
        match &self.0 {
            Backend::Program(c) => c.alive(),
            Backend::Here(w) => w.alive(),
        }
    }
}

//endregion

/*┌─────────────────╮
| Programs        |
└─────────────────╯*/
//region Programs

/// The argument that makes a program serving jobs run one.
const CHILD_FLAG: &str = "--isolated-job";

/// The program that serves this process's jobs on Windows, and which.
struct Program {
    /// `None` for this program itself.
    path: Option<PathBuf>,
    jobs: &'static [Jobs],
}

static PROGRAM: OnceLock<Program> = OnceLock::new();

/// Makes this program serve `jobs`; call it first thing in `main`, before
/// parsing arguments or starting anything.  When the program was started
/// to run a job (`--isolated-job NAME`), this runs it and exits the
/// process.  Otherwise it records that the program serves `jobs`, so that
/// on Windows they run in a new copy of it rather than on a thread.
pub fn serve_jobs(jobs: &'static [Jobs]) {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == CHILD_FLAG) {
        child::serve(jobs, args.next().unwrap_or_default());
    }
    let _ = PROGRAM.set(Program { path: None, jobs });
}

/// Has `jobs` run in `program`, which serves them (calls [`serve_jobs`]
/// with the same jobs).  For code that runs inside someone else's process,
/// such as Explorer's thumbnail DLL, which uses the `slerp.exe` beside it.
/// Only the first call (of this and [`serve_jobs`]) counts.
pub fn use_program(program: PathBuf, jobs: &'static [Jobs]) {
    let _ = PROGRAM.set(Program { path: Some(program), jobs });
}

/// How a job runs.
enum Way {
    /// In a newly started program.
    Program(PathBuf),
    /// Forked on Unix, on a thread elsewhere.
    Here,
}

fn way(job: &Job) -> Way {
    if let Some(program) = std::env::var_os("SLERP_ISOLATE_PROGRAM").filter(|p| !p.is_empty()) {
        return Way::Program(program.into());
    }
    if cfg!(unix) {
        return Way::Here;
    }
    let Some(program) = PROGRAM.get().filter(|p| find(p.jobs, job.name).is_some()) else { return Way::Here };
    match &program.path {
        Some(path) => Way::Program(path.clone()),
        None => {
            static THIS: OnceLock<Option<PathBuf>> = OnceLock::new();
            match THIS.get_or_init(|| std::env::current_exe().ok()) {
                Some(path) => Way::Program(path.clone()),
                None => Way::Here,
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Answers         |
└─────────────────╯*/
//region Answers

/// How work that gave no answer ended, as the rest of a sentence whose
/// subject is the job's `what`.
fn too_slow(time: Duration) -> String {
    format!("took longer than {:.1} s", time.as_secs_f32())
}

const NO_ANSWER: &str = "stopped without an answer";
const TOO_LARGE: &str = "sent too large an answer";

/// A sentence about the job: "the SVG renderer crashed".
fn say(job: &Job, happened: &str) -> String {
    format!("{} {happened}", job.what)
}

/// A panic's message, for the error it becomes.
fn panic_text(job: &Job, p: &(dyn std::any::Any + Send)) -> String {
    let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
    match text {
        Some(t) => say(job, &format!("failed inside ({})", t.chars().take(200).collect::<String>())),
        None => say(job, "failed inside"),
    }
}

/// Runs `f` and turns a panic into an error.
fn guarded<T>(job: &Job, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| Err(panic_text(job, &*p)))
}

/// Encodes an answer: a tag (0 for a result, 1 for an error message), its
/// length and its bytes.
fn encode(result: Result<Vec<u8>, String>) -> (Vec<u8>, Vec<u8>) {
    let (tag, body) = match result {
        Ok(bytes) => (0u8, bytes),
        Err(message) => (1u8, message.into_bytes()),
    };
    let mut head = vec![tag];
    head.extend((body.len() as u64).to_le_bytes());
    (head, body)
}

/// Decodes the body of an answer by its tag.
fn decode(tag: u8, body: Vec<u8>) -> Result<Vec<u8>, String> {
    match tag {
        0 => Ok(body),
        _ => Err(String::from_utf8_lossy(&body).into_owned()),
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names_are_found_in_groups() {
        let groups: &[Jobs] = &[testing::JOBS];
        assert_eq!(find(groups, "test.echo").map(|j| j.name()), Some("test.echo"));
        assert!(find(groups, "nothing").is_none());
    }

    #[test]
    fn test_jobs_have_unique_names() {
        let mut names: Vec<&str> = testing::JOBS.iter().map(|j| j.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), testing::JOBS.len());
    }
}
