//! Mutates the sample metafiles and replays them, looking for panics and
//! slow cases: `fuzz <rounds> <file>...`.  A failing input is written to
//! `/tmp/metafile-fuzz-<n>.bin` for reproducing.

use slerp_metafile::{load, sniff};
use slerp_scene::Limits;
use std::path::Path;
use std::time::Instant;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rounds: usize = args[1].parse().unwrap();
    let limits = Limits { max_texture_size: Some(64), ..Limits::thumbnail() };
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let (mut runs, mut bad, mut slowest) = (0usize, 0usize, 0.0f64);
    for file in &args[2..] {
        let path = Path::new(file);
        let Ok(bytes) = std::fs::read(path) else { continue };
        if bytes.len() > 400_000 {
            continue;
        }
        let Some(format) = sniff(path, &bytes[..bytes.len().min(4096)]) else { continue };
        for round in 0..rounds {
            let mut copy = bytes.clone();
            match round % 4 {
                // A few random bytes.
                0 | 1 => {
                    for _ in 0..(1 + rng.next() % 8) {
                        let i = (rng.next() as usize) % copy.len();
                        copy[i] = rng.next() as u8;
                    }
                }
                // Cut short.
                2 => copy.truncate((rng.next() as usize) % copy.len().max(1)),
                // Extreme values in the 32-bit words of the first records.
                _ => {
                    for _ in 0..4 {
                        let i = (rng.next() as usize) % copy.len().saturating_sub(4).max(1);
                        let v =
                            [0u32, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, 0x0100_0000][(rng.next() % 6) as usize];
                        if i + 4 <= copy.len() {
                            copy[i..i + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
            }
            let t = Instant::now();
            let r = std::panic::catch_unwind(|| load(format, &copy, &limits));
            let secs = t.elapsed().as_secs_f64();
            slowest = slowest.max(secs);
            runs += 1;
            if r.is_err() || secs > 3.0 {
                bad += 1;
                let out = format!("/tmp/metafile-fuzz-{bad}.bin");
                std::fs::write(&out, &copy).unwrap();
                println!(
                    "{} round {round}: {} in {secs:.2} s -> {out}",
                    file,
                    if r.is_err() { "PANIC" } else { "slow" }
                );
            }
        }
    }
    println!("{runs} runs, {bad} bad, slowest {slowest:.2} s");
}
