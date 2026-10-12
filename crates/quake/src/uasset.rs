//! The thumbnail Unreal Engine's editor saves inside `.uasset` and `.umap`
//! packages.
//!
//! The package summary at the start of the file holds the offset of a
//! thumbnail table; each entry names an object and points at its picture: a
//! width, a height (negative when the picture is a JPEG), and the PNG or
//! JPEG bytes, or raw BGRA in old packages.  The summary's layout changes
//! with engine versions, so when it can't be followed (or leads somewhere
//! implausible) the file is scanned for a picture preceded by a matching
//! size instead.

use crate::read::{Bytes, Result};
use crate::{decode_image, pixels};
use slerp_scene::{Limits, LoadError, Texture};
use std::sync::Arc;

/// The package tag, 0x9E2A83C1 little-endian.
pub const TAG: [u8; 4] = [0xc1, 0x83, 0x2a, 0x9e];

/// The largest thumbnail side believed; the editor saves 256.
const MAX_SIDE: i32 = 4096;

/// Engine object versions that changed the summary.
const UE4_STRING_ASSET_REFERENCES: i32 = 384;
const UE4_TEXT_IN_PACKAGES: i32 = 459;
const UE4_SEARCHABLE_NAMES: i32 = 510;
const UE4_LOCALIZATION_ID: i32 = 516;
const UE5_SOFT_OBJECT_PATHS: i32 = 1008;
const UE5_METADATA_OFFSET: i32 = 1014;
const UE5_VERSE_CELLS: i32 = 1015;
const UE5_SAVED_HASH: i32 = 1016;

const PKG_FILTER_EDITOR_ONLY: u32 = 0x8000_0000;

pub fn load(data: &[u8], limits: &Limits) -> Result<Arc<Texture>> {
    let b = Bytes::new(data);
    if !b.starts_with(&TAG) {
        return Err(LoadError::NotMine);
    }
    let found = table_offset(b).ok().and_then(|at| from_table(b, at)).or_else(|| scan(b));
    let Some(thumb) = found else {
        return Err(LoadError::Unsupported("the package has no thumbnail".into()));
    };
    let mut tex = match thumb.kind {
        Kind::Png | Kind::Jpeg => decode_image(thumb.data, limits.max_texture_size).map_err(LoadError::Invalid)?,
        Kind::Bgra => {
            let rgba: Vec<u8> = thumb.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
            pixels::texture(thumb.width, thumb.height, rgba, limits.max_texture_size, "", "")
        }
    };
    tex.source.container = "Unreal package".into();
    let kind = match thumb.kind {
        Kind::Png => "PNG",
        Kind::Jpeg => "JPEG",
        Kind::Bgra => "BGRA",
    };
    tex.source.format = match thumb.object {
        Some(o) => format!("{kind} thumbnail of {o}"),
        None => format!("{kind} thumbnail"),
    };
    Ok(Arc::new(tex))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Png,
    Jpeg,
    Bgra,
}

struct Thumb<'a> {
    width: u32,
    height: u32,
    kind: Kind,
    data: &'a [u8],
    /// The object's class and name, such as "StaticMesh 1M_Cube".
    object: Option<String>,
}

/// Reads an FString: a length (negative for UTF-16) then the characters
/// with a terminating NUL.  Returns the text and the offset after it.
fn fstring(b: Bytes, at: usize) -> Result<(String, usize)> {
    let len = b.i32(at)?;
    if len == 0 {
        return Ok((String::new(), at + 4));
    }
    if len > 0 {
        let s = b.slice(at + 4, len as usize)?;
        return Ok((crate::read::cstr(s), at + 4 + len as usize));
    }
    let n = len.unsigned_abs() as usize;
    let s = b.array(at + 4, n, 2)?;
    let units: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    Ok((String::from_utf16_lossy(&units), at + 4 + n * 2))
}

/// Follows the package summary to the thumbnail table's offset.
fn table_offset(b: Bytes) -> Result<usize> {
    let legacy = b.i32(4)?;
    // Positive versions are Unreal Engine 3; below -9 is newer than this
    // reader knows.
    if !(-9..0).contains(&legacy) {
        return Err(LoadError::Unsupported(format!("package version {legacy}")));
    }
    let mut at = 8;
    if legacy != -4 {
        at += 4;
    }
    let ue4 = b.i32(at)?;
    at += 4;
    let ue5 = if legacy <= -8 {
        at += 4;
        b.i32(at - 4)?
    } else {
        0
    };
    at += 4; // Licensee version.
    if legacy <= -2 {
        let count = b.index(at)?;
        at += 4;
        if legacy == -2 {
            b.array(at, count, 8)?;
            at += count * 8;
        } else if legacy >= -5 {
            for _ in 0..count.min(1 << 16) {
                at = fstring(b, at + 20)?.1;
            }
        } else {
            b.array(at, count, 20)?;
            at += count * 20;
        }
    }
    if ue5 >= UE5_SAVED_HASH {
        at += 20;
    }
    at += 4; // Total header size.
    at = fstring(b, at)?.1; // Package name.
    let flags = b.u32(at)?;
    at += 4 + 8; // Flags, then name count and offset.
    if ue5 >= UE5_SOFT_OBJECT_PATHS {
        at += 8;
    }
    if flags & PKG_FILTER_EDITOR_ONLY == 0 && ue4 >= UE4_LOCALIZATION_ID {
        at = fstring(b, at)?.1;
    }
    if ue4 >= UE4_TEXT_IN_PACKAGES {
        at += 8;
    }
    at += 16; // Exports and imports.
    if ue5 >= UE5_VERSE_CELLS {
        at += 16;
    }
    if ue5 >= UE5_METADATA_OFFSET {
        at += 4;
    }
    at += 4; // Depends offset.
    if ue4 >= UE4_STRING_ASSET_REFERENCES {
        at += 8;
    }
    if ue4 >= UE4_SEARCHABLE_NAMES {
        at += 4;
    }
    b.index(at)
}

/// Reads the thumbnail table and returns the first entry whose picture is
/// plausible.
fn from_table(b: Bytes<'_>, at: usize) -> Option<Thumb<'_>> {
    if at == 0 {
        return None;
    }
    let count = b.index(at).ok()?.min(1024);
    let mut p = at + 4;
    for _ in 0..count {
        let (class, next) = fstring(b, p).ok()?;
        let (object, next) = fstring(b, next).ok()?;
        let offset = b.index(next).ok()?;
        p = next + 4;
        if let Some(mut t) = picture_at(b, offset) {
            t.object = Some(format!("{class} {object}").trim().to_string());
            return Some(t);
        }
    }
    None
}

/// Reads a picture record: width, height, byte count, bytes.
fn picture_at(b: Bytes<'_>, at: usize) -> Option<Thumb<'_>> {
    let (w, h, n) = (b.i32(at).ok()?, b.i32(at + 4).ok()?, b.index(at + 8).ok()?);
    if w <= 0 || w > MAX_SIDE || h == 0 || h.abs() > MAX_SIDE {
        return None;
    }
    let data = b.slice(at + 12, n).ok()?;
    let (width, height) = (w as u32, h.unsigned_abs());
    let kind = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Kind::Png
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Kind::Jpeg
    } else if n == width as usize * height as usize * 4 {
        Kind::Bgra
    } else {
        return None;
    };
    Some(Thumb { width, height, kind, data, object: None })
}

/// Looks for a PNG or JPEG that is preceded by a width, height and byte
/// count agreeing with it, for packages whose summary this reader can't
/// follow.
fn scan(b: Bytes<'_>) -> Option<Thumb<'_>> {
    let d = b.data;
    let mut i = 12;
    while i + 8 <= d.len() {
        let png = d[i..].starts_with(b"\x89PNG\r\n\x1a\n");
        let jpeg = d[i..].starts_with(&[0xff, 0xd8, 0xff]);
        if (png || jpeg)
            && let Some(t) = picture_at(b, i - 12)
        {
            // A PNG's own header must agree with the record's size.
            let agrees = !png
                || (b.slice(i + 16, 8).ok()).is_some_and(|s| {
                    u32::from_be_bytes([s[0], s[1], s[2], s[3]]) == t.width
                        && u32::from_be_bytes([s[4], s[5], s[6], s[7]]) == t.height
                });
            if agrees {
                return Some(t);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, asset};

    fn files() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for dir in ["quake-extra/uasset/ue4", "quake-extra/uasset/ue5"] {
            let Some(d) = asset(dir) else { continue };
            let mut list: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
            list.sort();
            out.extend(list);
        }
        out
    }

    #[test]
    fn summary_parse_agrees_with_scan() {
        for p in files() {
            let bytes = std::fs::read(&p).unwrap();
            let b = Bytes::new(&bytes);
            let table = table_offset(b).ok().and_then(|at| from_table(b, at));
            let scanned = scan(b);
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            eprintln!(
                "{name}: version {} table {:?} scan {:?}",
                b.i32(4).unwrap(),
                table.as_ref().map(|t| (t.width, t.height, t.kind, t.object.clone())),
                scanned.as_ref().map(|t| (t.width, t.height, t.kind))
            );
            match (&table, &scanned) {
                (Some(t), Some(s)) => assert_eq!(t.data.as_ptr(), s.data.as_ptr(), "{name}"),
                (None, None) => {}
                _ => panic!("{name}: the summary and the scan disagree"),
            }
        }
    }

    #[test]
    fn thumbnails_decode() {
        let mut decoded = 0;
        for p in files() {
            let bytes = std::fs::read(&p).unwrap();
            match load(&bytes, &Limits::thumbnail()) {
                Ok(t) => {
                    decoded += 1;
                    let name = p.file_stem().unwrap().to_string_lossy().to_string();
                    eprintln!("{name}: {}x{} {}", t.width(), t.height(), t.source.format);
                    if let slerp_scene::textures::Pixels::Rgba8(px) = t.base() {
                        testutil::write_png(
                            std::path::Path::new(&format!("/tmp/slerp-quake/uasset-{name}.png")),
                            t.width(),
                            t.height(),
                            px,
                        );
                    }
                }
                Err(LoadError::Unsupported(_)) => {}
                Err(e) => panic!("{}: {e}", p.display()),
            }
        }
        if !files().is_empty() {
            assert!(decoded >= 2);
        }
    }

    #[test]
    fn corrupt_packages_never_panic() {
        let Some(p) = files().into_iter().find(|p| p.ends_with("Slippery.uasset")) else { return };
        let bytes = std::fs::read(&p).unwrap();
        for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
            let r = std::panic::catch_unwind(|| load(m, &Limits::viewer()).map(|_| ()));
            assert!(r.is_ok(), "mutation {i} panicked");
        }
        assert!(matches!(load(&TAG, &Limits::viewer()), Err(LoadError::Unsupported(_))));
    }
}
