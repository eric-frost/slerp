//! Finding a material's preview picture next to its file.
//!
//! Material libraries ship a rendered sphere beside the material:
//! ambientCG's downloads hold `Concrete034.png` next to
//! `Concrete034_2K-JPG.mtlx`, the USD working group's assets keep
//! `thumbnails/<name>.png`, and people save Poly Haven's previews as
//! `<slug>.png` next to `<slug>_2k.mtlx`.  So the search tries the file's own
//! name, the name without a resolution suffix, and a few preview suffixes,
//! ignoring case, and never picks one of the material's own texture maps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];
const SUFFIXES: &[&str] = &["", "_preview", "-preview", ".preview", "_thumb", "_thumbnail", "_sphere"];

/// Looks for a preview picture of the material file at `path`.  `exclude`
/// lists the material's texture files.
pub(crate) fn find_neighbour(path: &Path, exclude: &[PathBuf]) -> Option<PathBuf> {
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    let stems = stems(stem);
    let excluded: Vec<PathBuf> = exclude.iter().filter_map(|p| p.canonicalize().ok()).collect();
    let usable = |p: &Path| p.is_file() && !p.canonicalize().is_ok_and(|c| excluded.contains(&c));

    for folder in [dir.to_path_buf(), dir.join("thumbnails")] {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        // Lower-case name to real name, so `Concrete034.PNG` is found too.
        let mut names: HashMap<String, PathBuf> = HashMap::new();
        for entry in entries.flatten().take(100_000) {
            if let Some(name) = entry.file_name().to_str() {
                names.insert(name.to_lowercase(), entry.path());
            }
        }
        for s in &stems {
            for suffix in SUFFIXES {
                for ext in EXTENSIONS {
                    let key = format!("{s}{suffix}.{ext}").to_lowercase();
                    if let Some(p) = names.get(&key).filter(|p| usable(p)) {
                        return Some(p.clone());
                    }
                }
            }
        }
    }
    None
}

/// The names a preview might be saved under: the stem itself, then without
/// ambientCG's `_2K-JPG` or Poly Haven's `_2k` resolution suffix.
fn stems(stem: &str) -> Vec<String> {
    let mut out = vec![stem.to_string()];
    if let Some((base, suffix)) = stem.rsplit_once('_') {
        let res = suffix.split_once('-').map_or(suffix, |(r, _)| r);
        let digits = res.trim_end_matches(['k', 'K']);
        if !base.is_empty()
            && res.len() > digits.len()
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            out.push(base.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_suffixes() {
        assert_eq!(stems("Concrete034_2K-JPG"), ["Concrete034_2K-JPG", "Concrete034"]);
        assert_eq!(stems("brick_wall_001_2k"), ["brick_wall_001_2k", "brick_wall_001"]);
        assert_eq!(stems("Plaster001_1K-PNG"), ["Plaster001_1K-PNG", "Plaster001"]);
        assert_eq!(stems("wood_k"), ["wood_k"]);
        assert_eq!(stems("Tiles_v2"), ["Tiles_v2"]);
    }
}
