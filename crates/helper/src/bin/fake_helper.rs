//! A stand-in helper for the integration tests and the benchmark.  It serves
//! with a handler whose behaviour the requested file's name picks, so tests
//! can make it sleep, panic or hang without a renderer.
//!
//! ```text
//! fake_helper --socket PATH [--idle-ms N] [--threads N] [--hang-ms N]
//!             [--max-connections N] [--record DIR]
//! ```
//!
//! File names the handler understands (the directory part is ignored):
//!
//! - `png`: a 1x1 PNG.  `pid`: this process's id as text, in place of a PNG.
//! - `none`, `fail`, `panic`, `hang` (sleeps forever), `bad-frames`.
//! - `sleep-MS`: sleeps, then a PNG.  `big-BYTES`: a PNG-sized reply of
//!   that many bytes.  `frames`: `frames` frames of `size` squared.
//! - `echo`: the request's path, size and frames as text.  `left`: the
//!   milliseconds left until the deadline, as text.
//! - `touch-NAME`: creates `NAME` in the record directory, so tests can see
//!   whether a job ran.
//!
//! With `--record DIR` it appends its pid to `DIR/winners` when it gets the
//! lock or `DIR/losers` when another helper has it, and the reason it exited
//! to `DIR/exits`.

use slerp_helper::{Reply, Request, ServeError, ServeOptions, Server};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The smallest valid PNG: one transparent pixel.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn main() {
    // Behave like a C or C++ host, where a write to a closed socket raises
    // SIGPIPE and kills the process, to prove the helper never does that.
    // SAFETY: called before any other thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

    let mut opts = ServeOptions::default();
    let mut record: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| usage(&format!("{flag} needs a value")));
        let ms = || Duration::from_millis(value.parse().unwrap_or_else(|_| usage(&value)));
        match flag.as_str() {
            "--socket" => opts.socket = Some(value.clone().into()),
            "--idle-ms" => opts.idle = ms(),
            "--hang-ms" => opts.hang_margin = ms(),
            "--threads" => opts.threads = value.parse().unwrap_or_else(|_| usage(&value)),
            "--max-connections" => opts.max_connections = value.parse().unwrap_or_else(|_| usage(&value)),
            "--record" => record = Some(value.into()),
            _ => usage(&flag),
        }
    }

    let server = match Server::bind(opts) {
        Ok(server) => server,
        Err(ServeError::AlreadyRunning) => {
            append(record.as_deref(), "losers", &std::process::id().to_string());
            return;
        }
        Err(err) => {
            eprintln!("fake_helper: {err}");
            std::process::exit(1);
        }
    };
    append(record.as_deref(), "winners", &std::process::id().to_string());
    let exit = server.run(|req| handle(req, record.as_deref()));
    match exit {
        Ok(exit) => append(record.as_deref(), "exits", &format!("{exit:?}")),
        Err(err) => {
            eprintln!("fake_helper: {err}");
            std::process::exit(1);
        }
    }
}

fn handle(req: &Request, record: Option<&Path>) -> Reply {
    let name = req.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let number = |prefix: &str| name.strip_prefix(prefix).and_then(|n| n.parse::<u64>().ok());
    match name.as_str() {
        "png" => Reply::Png(PNG.to_vec()),
        "pid" => Reply::Png(std::process::id().to_string().into_bytes()),
        "none" => Reply::NoThumbnail,
        "fail" => Reply::Failed("this file is broken".into()),
        "panic" => panic!("the fake handler panics on request"),
        "hang" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        "echo" => Reply::Png(format!("{} {} {}", req.path.display(), req.size, req.frames).into_bytes()),
        "left" => {
            let left = req.deadline.saturating_duration_since(Instant::now()).as_millis();
            Reply::Png(left.to_string().into_bytes())
        }
        "frames" => {
            let len = (req.size * req.size * 4) as usize;
            Reply::Frames {
                width: req.size,
                height: req.size,
                rgba: (0..req.frames).map(|i| vec![i as u8; len]).collect(),
            }
        }
        "bad-frames" => Reply::Frames { width: 4, height: 4, rgba: vec![vec![0; 3]] },
        _ if number("sleep-").is_some() => {
            std::thread::sleep(Duration::from_millis(number("sleep-").unwrap_or(0)));
            Reply::Png(PNG.to_vec())
        }
        _ if number("big-").is_some() => Reply::Png(vec![7; number("big-").unwrap_or(0) as usize]),
        _ if name.starts_with("touch-") => {
            if let Some(dir) = record {
                let _ = std::fs::write(dir.join(&name["touch-".len()..]), b"");
            }
            Reply::Png(PNG.to_vec())
        }
        _ => Reply::NoThumbnail,
    }
}

fn append(dir: Option<&Path>, file: &str, line: &str) {
    if let Some(dir) = dir
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join(file))
    {
        let _ = writeln!(f, "{line}");
    }
}

fn usage(problem: &str) -> ! {
    eprintln!("fake_helper: bad argument: {problem}");
    std::process::exit(2);
}
