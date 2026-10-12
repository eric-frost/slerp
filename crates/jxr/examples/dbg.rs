//! Decodes a file and prints its facts and the first pixels: `dbg FILE [MIN_SIZE]`.
use slerp_jxr::*;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let min: u32 = std::env::args().nth(2).map_or(0, |s| s.parse().unwrap());
    let d = decode(&std::fs::read(&path).unwrap(), min, 1 << 28).unwrap();
    println!("{}x{} {:?} {} alpha {}", d.width, d.height, d.layout, d.native, d.has_alpha);
    let f: Vec<f32> = d.pixels.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    println!("{:?}", &f[..24.min(f.len())]);
}
