//! Compares two DICOM files' decoded frames (a compressed file against its
//! uncompressed reference) and prints where they first differ and by how
//! much overall.
//!
//! `cargo run -p slerp-dicom --example dicom-compare -- <reference> <other>`

use slerp_dicom::{Dicom, FramePixels};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--raw <file> <out>` writes the first frame's values as
    // little-endian f32, for comparing with other decoders.
    if let [flag, file, out] = &args[..]
        && flag == "--raw"
    {
        let d = Dicom::open(Path::new(file)).expect("file");
        let f = d.decode_frame(0, None).expect("frame");
        let bytes: Vec<u8> = match &f.pixels {
            FramePixels::Grey(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            FramePixels::Rgb(v) => v.iter().flat_map(|&x| (x as f32).to_le_bytes()).collect(),
        };
        std::fs::write(out, bytes).expect("write");
        println!("{}x{}", f.width, f.height);
        return;
    }
    let [a, b] = &args[..] else {
        eprintln!("usage: dicom-compare <reference> <other>");
        std::process::exit(2);
    };
    let (da, db) = (Dicom::open(Path::new(a)).expect("reference"), Dicom::open(Path::new(b)).expect("other"));
    println!("frames {} and {}", da.frame_count(), db.frame_count());
    for i in 0..da.frame_count().min(db.frame_count()) {
        let fa = da.decode_frame(i, None).expect("reference frame");
        let fb = match db.decode_frame(i, None) {
            Ok(f) => f,
            Err(e) => {
                println!("frame {i}: {e}");
                continue;
            }
        };
        let w = fa.width as usize;
        let values = |f: &slerp_dicom::Frame| -> Vec<f64> {
            match &f.pixels {
                FramePixels::Grey(v) => v.iter().map(|&x| x as f64).collect(),
                FramePixels::Rgb(v) => v.iter().map(|&x| x as f64).collect(),
            }
        };
        let (va, vb) = (values(&fa), values(&fb));
        let per = va.len() / (w * fa.height as usize).max(1);
        let mut first = None;
        let (mut max, mut sum) = (0f64, 0f64);
        for (k, (x, y)) in va.iter().zip(&vb).enumerate() {
            let d = (x - y).abs();
            if d > 0.0 && first.is_none() {
                first = Some((k / per % w, k / per / w, *x, *y));
            }
            max = max.max(d);
            sum += d;
        }
        println!(
            "frame {i}: {}x{} vs {}x{}, mean {:.3}, max {max}, first difference {first:?}",
            fa.width,
            fa.height,
            fb.width,
            fb.height,
            sum / va.len().max(1) as f64
        );
    }
}
