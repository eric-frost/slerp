//! The preview picture AutoCAD saves in a DXF file: the `THUMBNAILIMAGE`
//! section at the end of the file, a Windows bitmap without its file header
//! (a DIB) written as hexadecimal text in group 310 lines (raw bytes in
//! binary files).  Only the end of the file is read, since the section
//! comes last, after the drawing.

use super::pairs::{Reader, Value};
use slerp_scene::LoadError;
use slerp_scene::textures::{DecodeOptions, Texture};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of the file's end is searched for the section.  AutoCAD's
/// pictures are a few tens of kilobytes; the largest seen in the corpus
/// started 0.9 MB from the end.
const TAIL_BYTES: u64 = 8 << 20;
/// The most picture bytes taken.
const MAX_PICTURE_BYTES: usize = 4 << 20;

/// Reads the preview picture of the DXF file at `path`, or `None` when it
/// has none (most files written by programs other than AutoCAD).
pub fn read(path: &Path, max_side: Option<u32>) -> Result<Option<Texture>, LoadError> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; 4096.min(len as usize)];
    f.read_exact(&mut head)?;
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut tail)?;
    let Some(bytes) = section_bytes(&Reader::new(&head), &tail) else { return Ok(None) };
    let opts = max_side.map_or_else(DecodeOptions::full, DecodeOptions::preview);
    let picture = if bytes.starts_with(b"\x89PNG") { Some(bytes) } else { bmp_from_dib(&bytes) };
    let Some(Ok(mut t)) = picture.map(|p| slerp_scene::textures::decode(&p, &opts)) else { return Ok(None) };
    t.source.container = "DXF".into();
    t.source.format = "AutoCAD's preview picture (THUMBNAILIMAGE)".into();
    Ok(Some(t))
}

/// The picture bytes of the last `THUMBNAILIMAGE` section in `tail`, read
/// with the encoding `head` (the file's start) has.
fn section_bytes(head: &Reader, tail: &[u8]) -> Option<Vec<u8>> {
    let at = memchr::memmem::rfind(tail, b"THUMBNAILIMAGE")?;
    // Back up to the section name's group code: the line before the name's
    // line in text files, two bytes (a 16-bit code) in binary ones.
    let start = if head.is_binary() {
        at.checked_sub(2)?
    } else {
        let line = tail[..at].iter().rposition(|&b| b == b'\n' || b == b'\r').map_or(0, |i| i + 1);
        let code_end = line.checked_sub(1)?;
        tail[..code_end].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
    };
    let mut r = head.like(tail, start);
    let first = r.next()?;
    if !(first.code == 2 && first.bytes().trim_ascii().eq_ignore_ascii_case(b"THUMBNAILIMAGE")) {
        return None;
    }
    let mut out = Vec::new();
    while let Some(p) = r.next() {
        match (p.code, p.value) {
            (0, _) => break,
            (310, Value::Bin(b)) => out.extend_from_slice(b),
            (310, Value::Text(t)) => hex_into(t.trim_ascii(), &mut out)?,
            _ => {}
        }
        if out.len() > MAX_PICTURE_BYTES {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn hex_into(text: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    for pair in text.chunks(2) {
        let [a, b] = pair else { return None };
        out.push(digit(*a)? << 4 | digit(*b)?);
    }
    Some(())
}

/// Puts a BMP file header in front of a DIB, pointing past the header,
/// colour masks and palette to the pixels; `None` when it doesn't start
/// with a bitmap header (libredwg writes other data there).
fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |i: usize| Some(u32::from_le_bytes(dib.get(i..i + 4)?.try_into().ok()?));
    let header = u32_at(0)?;
    if !matches!(header, 40 | 52 | 56 | 108 | 124) {
        return None;
    }
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?) as u32;
    let compression = u32_at(16)?;
    let used = u32_at(32)?;
    let palette = if bits <= 8 { if used == 0 { 1u32 << bits } else { used.min(256) } } else { used.min(256) };
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let offset = 14 + header + masks + palette * 4;
    let total = 14u32.checked_add(u32::try_from(dib.len()).ok()?)?;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_dxf(hex_lines: &[&str]) -> Vec<u8> {
        let mut s =
            String::from("  0\nSECTION\n  2\nENTITIES\n  0\nENDSEC\n  0\nSECTION\n  2\nTHUMBNAILIMAGE\n 90\n0\n");
        for l in hex_lines {
            s.push_str("310\n");
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("  0\nENDSEC\n  0\nEOF\n");
        s.into_bytes()
    }

    /// A 2 x 2 24-bit DIB, bottom row first.
    fn dib() -> Vec<u8> {
        let mut d = Vec::new();
        for v in [40u32, 2, 2] {
            d.extend(v.to_le_bytes());
        }
        d.extend(1u16.to_le_bytes());
        d.extend(24u16.to_le_bytes());
        d.extend([0u8; 24]);
        // Rows are padded to four bytes.
        d.extend([0, 0, 255, 0, 255, 0, 0, 0]);
        d.extend([255, 0, 0, 255, 255, 255, 0, 0]);
        d
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }

    fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("slerp-dxf-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn reads_a_dib_split_over_lines() {
        let h = hex(&dib());
        let (a, b) = h.split_at(40);
        let p = write("two.dxf", &text_dxf(&[a, b]));
        let t = read(&p, None).unwrap().expect("a picture");
        assert_eq!((t.width(), t.height()), (2, 2));
    }

    #[test]
    fn other_data_and_damage_give_none() {
        // libredwg writes a DWG preview header where the DIB belongs.
        let p = write("libredwg.dxf", &text_dxf(&["6B4200000201030100005000"]));
        assert!(read(&p, None).unwrap().is_none());
        let p = write("odd.dxf", &text_dxf(&["ABC"]));
        assert!(read(&p, None).unwrap().is_none());
        let p = write("none.dxf", b"  0\nSECTION\n  2\nENTITIES\n  0\nENDSEC\n  0\nEOF\n");
        assert!(read(&p, None).unwrap().is_none());
        let full = text_dxf(&[&hex(&dib())]);
        for cut in [0, 5, 30, 60, full.len() - 20] {
            let p = write("cut.dxf", &full[..cut]);
            let _ = read(&p, None);
        }
    }

    /// Every DXF in the corpus with a `THUMBNAILIMAGE` section either gives
    /// a picture or is one of libredwg's conversions, whose section holds
    /// DWG data rather than a bitmap.
    #[test]
    fn corpus_pictures() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/cad-extra/dxf");
        let Ok(walk) = walk(&root) else { return };
        let (mut found, mut read_ok) = (0, 0);
        for p in walk {
            let bytes = std::fs::read(&p).unwrap();
            if memchr::memmem::find(&bytes, b"THUMBNAILIMAGE").is_none() {
                continue;
            }
            found += 1;
            match read(&p, Some(256)).unwrap() {
                Some(t) => {
                    assert!(t.width() >= 16 && t.height() >= 16, "{}", p.display());
                    read_ok += 1;
                }
                None => assert!(p.to_string_lossy().contains("libredwg"), "{}: no picture", p.display()),
            }
        }
        eprintln!("{read_ok} of {found} DXF files with a THUMBNAILIMAGE section gave a picture");
    }

    fn walk(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                out.extend(walk(&p)?);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dxf")) {
                out.push(p);
            }
        }
        Ok(out)
    }
}
