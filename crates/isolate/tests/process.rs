//! Jobs run in a started program, as Windows runs them (and any system
//! does with `SLERP_ISOLATE_PROGRAM` set): answers, the job's own errors,
//! every kind of crash, hangs, too large answers, what libraries print,
//! memory limits and serving jobs.  The program is `isolate_child`, which
//! serves the crate's test jobs.

use slerp_isolate::testing::{self, run_in, start_in};
use slerp_isolate::{Job, Limits};
use std::path::Path;
use std::time::{Duration, Instant};

fn child() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_isolate_child"))
}

fn limits() -> Limits {
    Limits { time: Duration::from_secs(10), memory: 256 << 20, output: 1 << 20 }
}

fn run(job: &'static Job, input: &[u8]) -> Result<Vec<u8>, String> {
    run_in(child(), job, input, limits())
}

#[test]
fn answers_and_errors_come_back() {
    assert_eq!(run(&testing::ECHO, b"hello").unwrap(), b"hello");
    assert_eq!(run(&testing::ECHO, b"").unwrap(), b"");
    assert_eq!(run(&testing::FAIL, b"nope").unwrap_err(), "nope");
    let big: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
    assert_eq!(run_in(child(), &testing::ECHO, &big, Limits { output: 4 << 20, ..limits() }).unwrap(), big);
}

#[test]
fn a_panic_is_an_error() {
    let e = run(&testing::PANIC, b"boom").unwrap_err();
    assert!(e.contains("failed inside") && e.contains("boom"), "{e}");
}

#[test]
fn crashes_are_survived_and_described() {
    for (job, words) in [
        (&testing::ABORT, "crashed"),
        (&testing::RUST_ABORT, "crashed"),
        (&testing::BAD_POINTER, "crashed"),
        (&testing::OVERFLOW, "crashed"),
    ] {
        let started = Instant::now();
        let e = run(job, b"").unwrap_err();
        assert!(e.starts_with("the test job ") && e.contains(words), "{}: {e}", job.name());
        eprintln!("{}: {e} ({:.0} ms)", job.name(), started.elapsed().as_secs_f64() * 1000.0);
    }
    assert!(run(&testing::ECHO, b"1").is_ok());
}

#[test]
fn a_hang_is_cut_off() {
    let started = Instant::now();
    let e = run_in(child(), &testing::HANG, b"", Limits { time: Duration::from_millis(500), ..limits() }).unwrap_err();
    assert!(e.contains("took longer than"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn too_large_an_answer_is_refused() {
    let l = Limits { output: 1000, ..limits() };
    let e = run_in(child(), &testing::BIG, &10_000_000u64.to_le_bytes(), l).unwrap_err();
    assert!(e.contains("too large"), "{e}");
}

#[test]
fn printing_doesnt_reach_the_answer() {
    assert_eq!(run(&testing::NOISY, b"quiet").unwrap(), b"quiet");
}

#[test]
fn an_unknown_job_is_an_error() {
    // A job this program doesn't serve: the name is all the child sees.
    static ELSEWHERE: Job = Job::once("test.nowhere", "the test job", |_| Ok(Vec::new()));
    let e = run(&ELSEWHERE, b"").unwrap_err();
    assert!(e.contains("no job called"), "{e}");
}

/// Linux and Windows limit a started program's memory; macOS enforces
/// no limit on malloc's memory.
#[test]
#[cfg(any(target_os = "linux", windows))]
fn greed_is_stopped_by_the_memory_limit() {
    let l = Limits { memory: 64 << 20, ..limits() };
    for job in [&testing::GREEDY, &testing::HUGE] {
        let e = run_in(child(), job, b"", l).unwrap_err();
        assert!(e.contains("ran out of memory"), "{}: {e}", job.name());
    }
}

#[test]
fn a_worker_keeps_its_state_and_survives_a_crash() {
    let (mut w, opening) = start_in(child(), &testing::COUNTER, &[10], limits()).unwrap();
    assert_eq!(opening, b"open");
    assert_eq!(w.ask(b"a").unwrap(), [11]);
    assert_eq!(w.ask(b"b").unwrap(), [12]);
    assert_eq!(w.ask(b"fail").unwrap_err(), "no");
    assert!(w.alive());
    let e = w.ask(b"crash").unwrap_err();
    assert!(e.contains("crashed"), "{e}");
    assert!(!w.alive());
    assert!(w.ask(b"a").is_err());
}

#[test]
fn a_failed_opening_is_an_error() {
    let e = start_in(child(), &testing::COUNTER, b"", limits()).err().unwrap();
    assert_eq!(e, "no starting count");
}

#[test]
fn a_hung_worker_is_cut_off() {
    let (mut w, _) = start_in(child(), &testing::COUNTER, &[0], limits()).unwrap();
    let started = Instant::now();
    let e = w.ask_within(b"hang", Duration::from_millis(500)).unwrap_err();
    assert!(e.contains("took longer than"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!w.alive());
}

/// What starting the program costs, printed for a look
/// (`cargo test -p slerp-isolate --test process -- --nocapture start_up`).
#[test]
fn start_up_cost() {
    let started = Instant::now();
    let n = 20;
    for _ in 0..n {
        run(&testing::ECHO, b"x").unwrap();
    }
    eprintln!("start, answer and end: {:.1} ms a job", started.elapsed().as_secs_f64() * 1000.0 / n as f64);
}
