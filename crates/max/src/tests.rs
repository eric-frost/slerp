use super::*;
use std::path::PathBuf;

fn chunk(id: u16, container: bool, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    let size = (body.len() + 6) as u32 | if container { 0x8000_0000 } else { 0 };
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[test]
fn chunk_trees_nest_and_stop_at_damage() {
    let leaf = chunk(0x10, false, &[1, 2, 3, 4]);
    let inner = chunk(0x20, true, &leaf);
    let mut stream = chunk(0x30, true, &inner);
    // A chunk claiming more than is there ends the level.
    stream.extend_from_slice(&chunk(0x40, false, &[0; 8])[..10]);
    let tree = chunks::parse(&stream);
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].find(0x10).and_then(|c| c.data()), Some(&[1u8, 2, 3, 4][..]));
    // 64-bit sizes: id, zero, then the size with the container bit.
    let mut big = 0x50u16.to_le_bytes().to_vec();
    big.extend_from_slice(&0u32.to_le_bytes());
    big.extend_from_slice(&((14u64 + leaf.len() as u64) | (1 << 63)).to_le_bytes());
    big.extend_from_slice(&leaf);
    let tree = chunks::parse(&big);
    assert_eq!(tree[0].children().len(), 1);
    // Deep nesting is cut off rather than recursing for ever.
    let mut deep = leaf.clone();
    for _ in 0..10_000 {
        deep = chunk(0x60, true, &deep);
    }
    let _ = chunks::parse(&deep);
}

#[test]
fn compressed_streams_are_capped() {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&vec![0u8; 8 << 20]).unwrap();
    let packed = gz.finish().unwrap();
    assert!(packed.len() < 64 << 10);
    assert!(matches!(chunks::decompress(packed.clone(), 1 << 20), Err(LoadError::TooLarge(_))));
    assert_eq!(chunks::decompress(packed, 16 << 20).unwrap().len(), 8 << 20);
}

#[test]
fn garbage_is_refused_without_panicking() {
    let mut bytes = ole::MAGIC.to_vec();
    bytes.resize(4096, 0xFF);
    assert!(load(&bytes, None, &Limits::viewer()).is_err());
    assert!(load(b"not a max file", None, &Limits::viewer()).is_err());
}

fn samples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let mut files = Vec::new();
    for dir in ["editors-extra/max", "autodesk-extra/max"] {
        collect(&root.join(dir), &mut files);
    }
    files.sort();
    files
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("max")) {
            out.push(p);
        }
    }
}

/// Every local 3ds Max sample loads or fails cleanly.
#[test]
fn sample_corpus() {
    let files = samples();
    if files.is_empty() {
        return;
    }
    let mut built = 0;
    for f in &files {
        let start = std::time::Instant::now();
        let result = load_path(f, &Limits::viewer());
        let ms = start.elapsed().as_millis();
        match &result {
            Ok(s) => {
                built += 1;
                eprintln!(
                    "{}: {} meshes, {} triangles, {} materials, {} textures, {ms} ms",
                    f.display(),
                    s.meshes.len(),
                    s.triangle_count(),
                    s.materials.len(),
                    s.textures.len()
                );
            }
            Err(e) => eprintln!("{}: {e} ({ms} ms)", f.display()),
        }
    }
    eprintln!("{built} of {} 3ds Max samples built geometry", files.len());
    assert!(built > 0);
}

/// Truncated and mutated copies of a real sample never panic.
#[test]
fn damaged_samples_never_panic() {
    let Some(f) = samples().into_iter().find(|f| f.ends_with("babylon-SimpleTriangle.max")) else { return };
    let full = std::fs::read(&f).unwrap();
    for k in 1..40 {
        let _ = load(&full[..full.len() * k / 40], None, &Limits::viewer());
    }
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..60 {
        let mut bytes = full.clone();
        for _ in 0..32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = 512 + (state as usize) % (bytes.len() - 512);
            bytes[i] = (state >> 40) as u8;
        }
        let _ = load(&bytes, None, &Limits::viewer());
    }
}
