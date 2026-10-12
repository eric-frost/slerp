//! Measures what a request through the helper costs beyond the handler's
//! own work: a handler that answers at once, so the time is the protocol,
//! the socket and the thread hand-offs.  Run in release mode:
//!
//! ```text
//! cargo build --release -p slerp-helper --examples
//! local/target/release/examples/bench
//! ```
//!
//! It starts the crate's `fake_helper` binary, which sits in the directory
//! above the examples, on a scratch socket, and runs `probe` from there to
//! time whole client processes.  Build them first with `cargo build -p
//! slerp-helper --bins`.

use slerp_helper::{ClientError, Reply, Request, request_at, spawn_detached, stop_at};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let dir = std::env::temp_dir().join(format!("slerp-helper-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("slerp").join("socket");
    let mut helper = Command::new(bin.join("fake_helper"))
        .arg("--socket")
        .arg(&socket)
        .args(["--threads", "8"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_ready(&socket);
    let deadline = || Instant::now() + Duration::from_secs(5);
    let ask = |name: &str, size: u32, frames: u32| {
        let req = Request { path: Path::new("/bench").join(name), size, frames, deadline: deadline() };
        let reply = request_at(&socket, &req, || panic!("no helper"));
        assert!(matches!(reply, Ok(Reply::Png(_) | Reply::Frames { .. })), "{reply:?}");
    };

    // 1. One client in this process, one request at a time.
    println!("Round trips, one at a time (client call to reply):");
    println!("{:<34} {:>9} {:>9} {:>9} {:>9}", "reply", "min", "median", "p90", "p99");
    let cases: [(&str, &str, u32, u32, usize); 6] = [
        ("67 B PNG", "png", 64, 0, 5000),
        ("100 KB (a 256 px thumbnail)", "big-100000", 64, 0, 2000),
        ("1 MB (a 1024 px thumbnail)", "big-1000000", 64, 0, 500),
        ("4 MB", "big-4000000", 64, 0, 200),
        ("36 frames of 256 px (9.4 MB)", "frames", 256, 36, 100),
        ("72 frames of 512 px (75 MB)", "frames", 512, 72, 20),
    ];
    for (label, name, size, frames, n) in cases {
        for _ in 0..n.min(50) {
            ask(name, size, frames);
        }
        let times = timed(n, || ask(name, size, frames));
        print_row(label, &times);
    }
    let handler = timed(500, || drop(std::hint::black_box(vec![7u8; 1_000_000])));
    println!("(the fake handler's own work for 1 MB, allocating the reply: median {})", fmt(handler[250]));

    // 2. Many clients at once, as when a folder opens.
    for clients in [8, 16] {
        let per_client = 1000;
        let t = Instant::now();
        let mut all: Vec<Duration> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..clients).map(|_| s.spawn(|| timed(per_client, || ask("png", 64, 0)))).collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let wall = t.elapsed();
        all.sort();
        println!(
            "\n{clients} clients at once, {per_client} small requests each: {:.0} requests/s; per request median {}, p99 {}",
            (clients * per_client) as f64 / wall.as_secs_f64(),
            fmt(all[all.len() / 2]),
            fmt(all[all.len() * 99 / 100])
        );
    }

    // 3. Whole thumbnailer processes: start, one request, exit.
    let probe = bin.join("probe");
    let run = |args: &[&str]| {
        let out = Command::new(&probe).args(args).stderr(Stdio::null()).output().unwrap();
        assert!(out.status.success());
        out
    };
    let socket_arg = socket.to_str().unwrap();
    let with_request = timed(300, || drop(run(&["--socket", socket_arg, "/bench/png"])));
    let noop = timed(300, || drop(run(&["--noop"])));
    let line = String::from_utf8(run(&["--socket", socket_arg, "/bench/png"]).stdout).unwrap();
    println!("\nWhole client processes (spawn to exit), 300 each:");
    print_row("probe making one request", &with_request);
    print_row("probe doing nothing", &noop);
    println!(
        "  difference of medians: {}  (sample: {})",
        fmt(with_request[150].saturating_sub(noop[150])),
        line.trim()
    );

    // 4. Failing fast.
    println!("\nWhen the helper can't be reached:");
    let missing = Path::new("/nonexistent-slerp/slerp/socket");
    let req = |deadline: Instant| Request { path: "/bench/png".into(), size: 64, frames: 0, deadline };
    let times = timed(1000, || {
        let r = request_at(missing, &req(deadline()), || Ok(()));
        assert!(matches!(r, Err(ClientError::NoRuntimeDir)));
    });
    print_row("no runtime directory", &times);
    let empty = dir.join("empty").join("slerp").join("socket");
    std::fs::create_dir_all(empty.parent().unwrap()).unwrap();
    let times = timed(1000, || {
        let r = request_at(&empty, &req(deadline()), || Ok(()));
        assert!(matches!(r, Err(ClientError::NotRunning)));
    });
    print_row("no helper (start not counted)", &times);
    drop(std::os::unix::net::UnixListener::bind(&empty).unwrap());
    let times = timed(1000, || {
        let r = request_at(&empty, &req(deadline()), || Ok(()));
        assert!(matches!(r, Err(ClientError::NotRunning)));
    });
    print_row("stale socket (start not counted)", &times);
    std::fs::remove_file(&empty).unwrap();

    // 5. Starting a helper: what the first client pays before rendering
    // in-process, and how soon the helper answers.
    let mut start_times = Vec::new();
    let mut ready_times = Vec::new();
    for _ in 0..20 {
        let mut cmd = Command::new(bin.join("fake_helper"));
        cmd.arg("--socket").arg(&empty);
        let t = Instant::now();
        let r = request_at(&empty, &req(deadline()), || spawn_detached(&mut cmd));
        start_times.push(t.elapsed());
        assert!(matches!(r, Err(ClientError::NotRunning)), "{r:?}");
        wait_ready(&empty);
        ready_times.push(t.elapsed());
        assert!(stop_at(&empty, Duration::from_secs(1)).unwrap());
        while empty.exists() {
            std::thread::sleep(Duration::from_millis(1));
        }
        // Let the stopped helper release its lock before the next start.
        std::thread::sleep(Duration::from_millis(20));
    }
    start_times.sort();
    ready_times.sort();
    print_row("no helper, starting one", &start_times);
    print_row("  ...until the new helper listens", &ready_times);

    stop_at(&socket, Duration::from_secs(1)).unwrap();
    helper.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait_ready(socket: &PathBuf) {
    let until = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < until, "the helper didn't start");
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Runs `f` `n` times and returns the sorted times.
fn timed(n: usize, mut f: impl FnMut()) -> Vec<Duration> {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times
}

fn print_row(label: &str, sorted: &[Duration]) {
    let at = |q: usize| sorted[(sorted.len() * q / 100).min(sorted.len() - 1)];
    println!("{label:<34} {:>9} {:>9} {:>9} {:>9}", fmt(sorted[0]), fmt(at(50)), fmt(at(90)), fmt(at(99)));
}

fn fmt(d: Duration) -> String {
    let us = d.as_secs_f64() * 1e6;
    if us < 1000.0 { format!("{us:.0} us") } else { format!("{:.2} ms", us / 1000.0) }
}
