//! Every file of the local test corpus (`local/assets/cad-extra`, fetched
//! by `tools/fetch-test-assets.ts`) loads or is refused without a panic, in
//! reasonable time, and mutated copies of a sample of them do too.  Skipped
//! quietly when the corpus is absent.

use slerp_cad::Opened;
use slerp_scene::Limits;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/cad-extra")
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if matches!(ext.as_str(), "dxf" | "dxfb" | "3dm") && e.metadata().is_ok_and(|m| m.len() < 64 << 20) {
                out.push(p);
            }
        }
    }
}

/// Opens `data` as the file `name` (written to a scratch folder, since the
/// reader takes paths); `None` when it isn't recognised.
fn open_bytes(data: &[u8], name: &str) -> Option<Duration> {
    let head = &data[..data.len().min(4096)];
    let format = slerp_cad::sniff(Path::new(name), head)?;
    let dir = std::env::temp_dir().join(format!("slerp-cad-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, data).unwrap();
    let start = Instant::now();
    let result = std::panic::catch_unwind(|| slerp_cad::open(format, &path, &Limits::thumbnail()));
    let took = start.elapsed();
    std::fs::remove_file(&path).ok();
    match result {
        Ok(Ok(Opened::Flat(d))) => {
            // Drawing what was read must not panic either.
            let _ = d.drawing.render_fit(256, 256, &d.drawing.saved_visibility(), &slerp_cad::Paint::LIGHT);
        }
        Ok(_) => {}
        Err(_) => panic!("{name}: panicked"),
    }
    Some(took)
}

#[test]
fn every_corpus_file_loads_or_is_refused_cleanly() {
    let mut all = Vec::new();
    files(&corpus(), &mut all);
    all.sort();
    if all.is_empty() {
        eprintln!("no corpus at {}", corpus().display());
        return;
    }
    let mut slow = Vec::new();
    let mut loaded = 0;
    for p in &all {
        let data = std::fs::read(p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if let Some(took) = open_bytes(&data, &name) {
            loaded += 1;
            if took > Duration::from_secs(3) {
                slow.push((p.clone(), took));
            }
        }
    }
    eprintln!("{loaded} of {} files recognised", all.len());
    assert!(slow.is_empty(), "slow files: {slow:?}");
    assert!(loaded > all.len() / 2, "most files should be recognised");
}

#[test]
fn mutated_corpus_files_never_panic() {
    let mut all = Vec::new();
    files(&corpus(), &mut all);
    all.sort();
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    // Every 17th file below 600 KB, a few mutations each.
    for p in all.iter().step_by(17).filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 600_000)) {
        let data = std::fs::read(p).unwrap();
        if data.len() < 64 {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        for round in 0..6 {
            let mut d = data.clone();
            for _ in 0..(1 + round * 3) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let i = (x >> 8) as usize % d.len();
                d[i] = (x >> 3) as u8;
            }
            if round == 5 {
                d.truncate((x >> 11) as usize % d.len());
            }
            if d.len() >= 32 {
                // A mutated file may no longer be recognised; that is fine.
                let _ = open_bytes(&d, &name);
            }
        }
    }
}
