//! Files built to hurt: absurd sizes, frame counts and compressed sizes.  A
//! viewer opens whatever a download contains, so each must fail or open in
//! bounded time and memory, never panic.

use slerp_images::ImageFormat;
use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-images-hostile-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The most memory the process has held, in KiB (Linux; 0 elsewhere).
fn peak_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn open(path: &Path) -> Result<Asset, String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path).unwrap().take(4096).read_to_end(&mut head).unwrap();
    let f: ImageFormat = slerp_images::sniff(path, &head).ok_or("not recognised")?;
    slerp_images::load(f, path, &Limits::viewer()).map_err(|e| e.to_string())
}

/// Opens `bytes` as `name`, and returns the result with the time taken,
/// failing the test when it takes long or grows the process by gigabytes.
fn opened(dir: &Path, name: &str, bytes: &[u8]) -> (Result<Asset, String>, Duration) {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    let before = peak_kib();
    let t = Instant::now();
    let r = open(&p);
    let took = t.elapsed();
    let grew = peak_kib().saturating_sub(before) / 1024;
    assert!(took < Duration::from_secs(20), "{name} took {took:?}");
    assert!(grew < 3000, "{name} grew the process by {grew} MiB");
    (r, took)
}

fn gif_header(w: u16, h: u16, loops: bool) -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend(w.to_le_bytes());
    g.extend(h.to_le_bytes());
    g.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    if loops {
        g.extend([0x21, 0xff, 11]);
        g.extend(b"NETSCAPE2.0");
        g.extend([3, 1, 0, 0, 0]);
    }
    g
}

/// An image block of `w` x `h` with `data` as its (already compressed) pixels.
fn gif_frame(x: u16, y: u16, w: u16, h: u16, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x2c];
    for v in [x, y, w, h] {
        f.extend(v.to_le_bytes());
    }
    f.extend([0, 2]);
    for chunk in data.chunks(255) {
        f.push(chunk.len() as u8);
        f.extend(chunk);
    }
    f.push(0);
    f
}

#[test]
fn a_gif_screen_of_65535_squared_with_many_frames() {
    let dir = scratch("gif-screen");
    let mut g = gif_header(65535, 65535, true);
    for _ in 0..2000 {
        g.extend(gif_frame(0, 0, 1, 1, &[0x44, 0x01]));
    }
    g.push(0x3b);
    let (r, took) = opened(&dir, "screen.gif", &g);
    let e = r.err().expect("a 65535 x 65535 canvas is refused");
    assert!(e.contains("megapixels"), "{e}");
    assert!(took < Duration::from_secs(1));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_gif_with_a_million_frames() {
    let dir = scratch("gif-frames");
    let mut g = gif_header(2, 2, true);
    let frame = gif_frame(0, 0, 1, 1, &[0x44, 0x01]);
    for _ in 0..250_000 {
        g.extend(&frame);
    }
    g.push(0x3b);
    let (r, _) = opened(&dir, "many.gif", &g);
    // Indexing stops at the cap with a warning; the picture still plays.
    let Ok(Asset::Canvas(c)) = r else { panic!("a long animation opens") };
    assert!(c.frames().len() <= slerp_images::MAX_FRAMES);
    assert!(c.info().warnings.iter().any(|w| w.contains("frames")));
    // Frames deep inside decode without decoding the ones before them again and again.
    let t = Instant::now();
    let img = c.compose(60_000, &[], 1.0);
    assert_eq!((img.width, img.height), (2, 2));
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_gif_frame_that_claims_more_than_it_holds() {
    let dir = scratch("gif-bomb");
    // A 4000 x 4000 frame with a few bytes of LZW data: the rest stays transparent.
    let mut g = gif_header(4000, 4000, false);
    g.extend(gif_frame(0, 0, 4000, 4000, &[0x44, 0x01]));
    g.push(0x3b);
    let (r, took) = opened(&dir, "bomb.gif", &g);
    let Ok(Asset::Image(t)) = r else { panic!("opens as a picture") };
    assert_eq!((t.width(), t.height()), (4000, 4000));
    assert!(took < Duration::from_secs(5), "{took:?}");
    std::fs::remove_dir_all(&dir).ok();
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = (body.len() as u32).to_be_bytes().to_vec();
    c.extend(kind);
    c.extend(body);
    c.extend([0, 0, 0, 0]);
    c
}

#[test]
fn apng_with_a_huge_canvas_and_with_endless_frames() {
    let dir = scratch("apng");
    // A canvas of 100000 x 100000.
    let mut huge = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = 100_000u32.to_be_bytes().to_vec();
    ihdr.extend(100_000u32.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    huge.extend(chunk(b"IHDR", &ihdr));
    huge.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
    huge.extend(chunk(b"IEND", &[]));
    let (r, took) = opened(&dir, "huge.png", &huge);
    assert!(r.is_err());
    assert!(took < Duration::from_secs(1));
    // Fifty thousand frames of 1 x 1, announced as four billion.
    let mut many = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = 4u32.to_be_bytes().to_vec();
    ihdr.extend(4u32.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    many.extend(chunk(b"IHDR", &ihdr));
    many.extend(chunk(b"acTL", &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]));
    let z = miniz_for_test(&[0, 1, 2, 3, 255]);
    for i in 0..50_000u32 {
        let mut f = i.to_be_bytes().to_vec();
        f.extend([0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 100, 0, 1]);
        f.truncate(26);
        many.extend(chunk(b"fcTL", &f));
        if i == 0 {
            many.extend(chunk(b"IDAT", &z));
        } else {
            let mut d = i.to_be_bytes().to_vec();
            d.extend(&z);
            many.extend(chunk(b"fdAT", &d));
        }
    }
    many.extend(chunk(b"IEND", &[]));
    let (r, took) = opened(&dir, "many.png", &many);
    assert!(r.is_ok(), "{:?}", r.err());
    assert!(took < Duration::from_secs(5), "{took:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A zlib stream for `raw`, stored without compression.
fn miniz_for_test(raw: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(raw);
    let (mut a, mut b) = (1u32, 0u32);
    for &v in raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    z
}

#[test]
fn webp_with_a_huge_canvas() {
    let dir = scratch("webp");
    let mut w = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
    w.extend(10u32.to_le_bytes());
    w.extend([0x12, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    let (r, took) = opened(&dir, "huge.webp", &w);
    assert!(r.is_err());
    assert!(took < Duration::from_secs(1));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn compressed_size_bombs() {
    let dir = scratch("bombs");
    // A PNM whose header asks for 20 gigapixels.
    let (r, _) = opened(&dir, "big.ppm", b"P6\n100000 200000\n255\nabc");
    assert!(r.is_err());
    // An icon whose bitmap claims 60000 x 60000.
    let mut ico = vec![0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 24, 0, 60, 0, 0, 0, 22, 0, 0, 0];
    let mut dib = 40u32.to_le_bytes().to_vec();
    dib.extend(60_000u32.to_le_bytes());
    dib.extend(120_000u32.to_le_bytes());
    dib.extend([1, 0, 24, 0]);
    dib.extend([0u8; 24]);
    ico.extend(dib);
    let (r, _) = opened(&dir, "big.ico", &ico);
    assert!(r.is_err());
    std::fs::remove_dir_all(&dir).ok();
}
