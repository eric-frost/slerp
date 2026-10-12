//! Times loading splat files: `cargo run --release -p slerp-splats --example
//! load_time -- [--thumbnail] FILE...`.

use slerp_scene::Limits;
use std::path::Path;
use std::time::Instant;

fn main() {
    let mut limits = Limits::viewer();
    for arg in std::env::args().skip(1) {
        if arg == "--thumbnail" {
            limits = Limits::thumbnail();
            continue;
        }
        let path = Path::new(&arg);
        let head: Vec<u8> = std::fs::read(path).map(|b| b.into_iter().take(4096).collect()).unwrap_or_default();
        let Some(format) = slerp_splats::sniff(path, &head) else {
            println!("{arg}: not a splat file");
            continue;
        };
        let mut times = Vec::new();
        let mut count = 0;
        for _ in 0..3 {
            let start = Instant::now();
            match slerp_splats::load(format, path, &limits) {
                Ok(scene) => count = scene.splats.map_or(0, |s| s.len()),
                Err(e) => println!("{arg}: {e}"),
            }
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{arg}: {count} splats, {:.0} {:.0} {:.0} ms", times[0], times[1], times[2]);
    }
}
