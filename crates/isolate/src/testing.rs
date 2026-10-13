//! Jobs that succeed, fail and crash on purpose, for this crate's tests and
//! the `isolate_child` program that serves them to the tests of starting a
//! program.  Not part of the interface.

use crate::{Job, Jobs, Limits, Opened, Session, Worker};
use std::path::Path;
use std::time::Duration;

/// Every test job, for [`crate::serve_jobs`].
pub static JOBS: Jobs = &[
    &ECHO,
    &FAIL,
    &PANIC,
    &ABORT,
    &RUST_ABORT,
    &BAD_POINTER,
    &OVERFLOW,
    &HANG,
    &BIG,
    &NOISY,
    &GREEDY,
    &HUGE,
    &COUNTER,
];

const WHAT: &str = "the test job";

/// Answers with its input.
pub static ECHO: Job = Job::once("test.echo", WHAT, |input| Ok(input.to_vec()));
/// Fails with its input as the message.
pub static FAIL: Job = Job::once("test.fail", WHAT, |input| Err(String::from_utf8_lossy(input).into_owned()));
/// Panics with its input as the message.
pub static PANIC: Job = Job::once("test.panic", WHAT, |input| panic!("{}", String::from_utf8_lossy(input)));
/// Calls the C runtime's `abort`, as a failed C `assert` does.
pub static ABORT: Job = Job::once("test.abort", WHAT, |_| {
    // SAFETY: ends the process on purpose.
    unsafe { libc::abort() }
});
/// Rust's own abort, which a failed allocation ends in.
pub static RUST_ABORT: Job = Job::once("test.rust-abort", WHAT, |_| std::process::abort());
/// Writes through a null pointer.
pub static BAD_POINTER: Job = Job::once("test.bad-pointer", WHAT, |_| {
    // SAFETY: deliberately not: the write faults, ending the process.
    unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
    Ok(Vec::new())
});
/// Recurses until the stack runs out.
pub static OVERFLOW: Job =
    Job::once("test.overflow", WHAT, |_| Ok(deep(std::hint::black_box(u64::MAX)).to_le_bytes().to_vec()));
/// Sleeps far longer than any test waits.
pub static HANG: Job = Job::once("test.hang", WHAT, |_| {
    std::thread::sleep(Duration::from_secs(60));
    Ok(Vec::new())
});
/// Answers with as many bytes as its input (a little-endian u64) asks.
pub static BIG: Job = Job::once("test.big", WHAT, |input| {
    let n = u64::from_le_bytes(input.try_into().map_err(|_| "no size")?);
    Ok(vec![7u8; n as usize])
});
/// Prints through Rust and the C runtime before answering with its input,
/// as libraries that complain do.
pub static NOISY: Job = Job::once("test.noisy", WHAT, |input| {
    println!("noise on standard output");
    eprintln!("noise on standard error");
    let text = b"noise from C\n";
    // SAFETY: writes a valid buffer to the standard descriptors.
    unsafe {
        libc::write(1, text.as_ptr().cast(), text.len() as _);
        libc::write(2, text.as_ptr().cast(), text.len() as _);
    }
    Ok(input.to_vec())
});
/// Takes memory a megabyte at a time, up to 4 GB.
pub static GREEDY: Job = Job::once("test.greedy", WHAT, |_| {
    let mut v: Vec<Vec<u8>> = Vec::new();
    for _ in 0..4096 {
        v.push(vec![1u8; 1 << 20]);
    }
    Ok(vec![v.len() as u8])
});
/// Asks for 10 GB at once, as damaged files make decoders do.
pub static HUGE: Job = Job::once("test.huge", WHAT, |_| {
    let v = vec![1u8; std::hint::black_box(10_000_000_000usize)];
    Ok(vec![v[v.len() / 2]])
});
/// Counts requests from the input's first byte: "fail" fails, "crash"
/// aborts, "hang" hangs, anything else answers with the count.
pub static COUNTER: Job = Job::serving("test.counter", WHAT, open_counter);

fn open_counter(input: &[u8]) -> Result<Opened, String> {
    let mut count = *input.first().ok_or("no starting count")?;
    let session: Session = Box::new(move |request: &[u8]| {
        count = count.wrapping_add(1);
        match request {
            b"fail" => Err("no".into()),
            // SAFETY: ends the process on purpose.
            b"crash" => unsafe { libc::abort() },
            b"hang" => {
                std::thread::sleep(Duration::from_secs(60));
                Ok(Vec::new())
            }
            _ => Ok(vec![count]),
        }
    });
    Ok((session, b"open".to_vec()))
}

fn deep(n: u64) -> u64 {
    let pad = [n; 64];
    if n == 0 { 0 } else { deep(std::hint::black_box(n - 1)) + pad[3] }
}

/// Runs `job` in `program`, whatever this process would do otherwise.
pub fn run_in(program: &Path, job: &'static Job, input: &[u8], limits: Limits) -> Result<Vec<u8>, String> {
    crate::process::Child::start(program, job, input, limits).map(|(_, answer)| answer)
}

/// Starts a worker for `job` in `program`.
pub fn start_in(program: &Path, job: &'static Job, input: &[u8], limits: Limits) -> Result<(Worker, Vec<u8>), String> {
    crate::process::Child::start(program, job, input, limits).map(|(c, a)| (Worker(crate::Backend::Program(c)), a))
}
