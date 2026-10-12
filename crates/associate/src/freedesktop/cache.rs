//! Clearing cached thumbnails after the thumbnailer of some types changes.
//! File managers keep thumbnails in `~/.cache/thumbnails/<size>/` and mark
//! files that failed in `~/.cache/thumbnails/fail/<program>/`, by the MD5
//! of the file's URI (the freedesktop thumbnail specification).  A cached
//! picture stays valid while the file is unchanged, so without clearing, a
//! type switched off keeps showing Slerp's thumbnails and a type switched
//! on keeps its failure marks.
//!
//! Each cached PNG says which file it shows (`Thumb::URI`) and, from KDE,
//! the file's type (`Thumb::Mimetype`), in text chunks before the pixels;
//! the type comes from the format table by name where it isn't written.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

/// Deletes the cached thumbnails and failure marks of files whose type is
/// one of `types`, and returns how many.
pub fn clear(cache_home: &Path, types: &BTreeSet<String>) -> usize {
    let root = cache_home.join("thumbnails");
    let mut dirs: Vec<std::path::PathBuf> =
        ["normal", "large", "x-large", "xx-large"].iter().map(|d| root.join(d)).collect();
    if let Ok(entries) = std::fs::read_dir(root.join("fail")) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    let mut removed = 0;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "png") {
                continue;
            }
            if matches(&path, types) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Whether a cached thumbnail shows a file of one of `types`.
fn matches(path: &Path, types: &BTreeSet<String>) -> bool {
    let mut head = Vec::with_capacity(8192);
    if std::fs::File::open(path).and_then(|f| f.take(8192).read_to_end(&mut head)).is_err() {
        return false;
    }
    let text = text_chunks(&head);
    let get = |k: &str| text.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    if let Some(mime) = get("Thumb::Mimetype") {
        return types.contains(mime);
    }
    let Some(uri) = get("Thumb::URI") else { return false };
    let name = uri.rsplit('/').next().unwrap_or(uri);
    let name = percent_decode(name);
    slerp_formats::by_file_name(&name).iter().any(|f| f.listed_mime().any(|m| types.contains(m)))
}

/// The `tEXt` chunks of a PNG's first bytes, as keyword and text.
fn text_chunks(png: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return out;
    }
    let mut at = 8usize;
    while let Some(head) = png.get(at..at + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind = &head[4..8];
        if kind == b"IDAT" || kind == b"IEND" {
            break;
        }
        let Some(data) = at.checked_add(8).and_then(|s| png.get(s..s.checked_add(len)?)) else { break };
        if kind == b"tEXt"
            && let Some(nul) = data.iter().position(|&b| b == 0)
        {
            let key = String::from_utf8_lossy(&data[..nul]).into_owned();
            // tEXt is Latin-1; URIs and type names are ASCII.
            let value = data[nul + 1..].iter().map(|&b| b as char).collect();
            out.push((key, value));
        }
        at += 12 + len;
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal PNG carrying the text chunks a file manager writes.
    pub fn cached_png(texts: &[(&str, &str)]) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            png.extend((data.len() as u32).to_be_bytes());
            png.extend(kind);
            png.extend(data);
            png.extend([0, 0, 0, 0]);
        };
        chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        for (k, v) in texts {
            chunk(b"tEXt", &[k.as_bytes(), &[0], v.as_bytes()].concat());
        }
        chunk(b"IDAT", &[]);
        chunk(b"IEND", &[]);
        png
    }

    #[test]
    fn clears_only_the_changed_types() {
        let root = std::env::temp_dir().join(format!("slerp-cache-{}", std::process::id()));
        let normal = root.join("thumbnails/normal");
        let fail = root.join("thumbnails/fail/gnome-thumbnail-factory");
        std::fs::create_dir_all(&normal).unwrap();
        std::fs::create_dir_all(&fail).unwrap();
        // KDE writes the type; GNOME only the URI.
        std::fs::write(
            normal.join("a.png"),
            cached_png(&[("Thumb::URI", "file:///x/tex.dds"), ("Thumb::Mimetype", "image/x-dds")]),
        )
        .unwrap();
        std::fs::write(
            normal.join("b.png"),
            cached_png(&[("Thumb::URI", "file:///x/m.glb"), ("Thumb::Mimetype", "model/gltf-binary")]),
        )
        .unwrap();
        std::fs::write(fail.join("c.png"), cached_png(&[("Thumb::URI", "file:///x/My%20Tex.dds")])).unwrap();
        std::fs::write(fail.join("d.png"), b"not a png").unwrap();
        let types: BTreeSet<String> = ["image/x-dds".to_string()].into();
        assert_eq!(clear(&root, &types), 2);
        assert!(normal.join("b.png").exists() && fail.join("d.png").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
