//! The format table (`slerp_formats`) against what sniffing finds in the
//! test corpus: every file Slerp recognises maps to a format, and when its
//! name has an extension the table knows, that format is one of the
//! extension's.  Skipped when `local/assets` hasn't been fetched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Content overrules names (an animated PNG named `.png`, an Analyze
/// header named `.hdr`, test files named for another format on purpose),
/// so single files may disagree.  A format whose files never agree with
/// their names means a wrong row in `Kind::format`.
#[test]
fn sniffed_formats_agree_with_the_table() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let mut all = Vec::new();
    files(&assets, &mut all);
    if all.is_empty() {
        eprintln!("no test assets; skipped");
        return;
    }
    // Per format: files whose name agrees, and examples that don't.
    let mut tally: BTreeMap<&str, (usize, Vec<String>)> = BTreeMap::new();
    for path in &all {
        let Ok(Some(kind)) = slerp_core::sniff_file(path) else { continue };
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let by_name = slerp_core::formats::by_file_name(&name);
        let Some(format) = kind.format(path) else {
            // Files read only when opened by name, under names no format
            // claims (Spectrum `.scr` screens, TexturePacker's `.json`).
            assert!(by_name.is_empty(), "{}: {kind:?} has no format", path.display());
            continue;
        };
        if by_name.is_empty() {
            continue;
        }
        let entry = tally.entry(format.id).or_default();
        if by_name.iter().any(|f| f.id == format.id) {
            entry.0 += 1;
        } else {
            entry.1.push(format!("{} ({kind:?})", path.display()));
        }
    }
    let never: Vec<String> =
        tally.iter().filter(|(_, (agree, _))| *agree == 0).map(|(id, (_, files))| format!("{id}: {files:?}")).collect();
    eprintln!("{} formats checked", tally.len());
    assert!(never.is_empty(), "formats whose files never match their names:\n{}", never.join("\n"));
}
