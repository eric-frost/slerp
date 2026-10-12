//! Times decoding KTX2 files to RGBA and for a GPU that samples BC blocks
//! (`decode_for_gpu`), file by file:
//! `cargo run --release -p slerp-textures --example blocks_bench <files>`.
//! `RAYON_NUM_THREADS=1` shows the CPU work of each.

use slerp_textures::{DecodeOptions, GpuBlocks, decode, decode_for_gpu};
use std::time::Instant;

fn main() {
    let files: Vec<_> = std::env::args().skip(1).collect();
    let bc = GpuBlocks { bc: true, astc: false };
    let (mut rgba_total, mut bc_total) = (0.0, 0.0);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let t = Instant::now();
        let a = decode(&bytes, &DecodeOptions::full()).unwrap();
        let rgba = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let b = decode_for_gpu(&bytes, &DecodeOptions::full(), bc).unwrap();
        let blocks = t.elapsed().as_secs_f64() * 1e3;
        rgba_total += rgba;
        bc_total += blocks;
        println!(
            "{:40} {:>5}x{:<5} {:28} rgba {rgba:6.1} ms  blocks {blocks:6.1} ms {}",
            std::path::Path::new(f).file_name().unwrap().to_string_lossy(),
            a.width(),
            a.height(),
            a.source.format,
            if slerp_textures::is_compressed(&b) { "" } else { "(not kept)" }
        );
    }
    println!("total rgba {rgba_total:.0} ms, blocks {bc_total:.0} ms");
}
