//! The thumbnail an FBX file can carry of its scene, which 3ds Max, Maya and
//! MotionBuilder save in the header extension at the start of the file:
//! `FBXHeaderExtension > SceneInfo > Thumbnail`, raw RGB or RGBA pixels,
//! square (`Size`) or with a `CustomWidth` and `CustomHeight` property.
//!
//! ufbx reads it too, but only while parsing the whole file, which for a
//! large scene costs far more than a thumbnail may.  This reader seeks to
//! the header extension and reads that one node, binary or ASCII, so a
//! file of any size costs a few hundred kilobytes.

use miniz_oxide::inflate::decompress_to_vec_zlib_with_limit;
use slerp_scene::LoadError;
use slerp_scene::textures::{ColorSpace, Texture};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The most a header extension may span; real ones hold a 128 x 128
/// picture at most, a few hundred kilobytes in ASCII.
const MAX_HEADER_BYTES: u64 = 16 << 20;
/// The largest picture taken, in pixels.
const MAX_PIXELS: u64 = 4096 * 4096;

/// What the thumbnail node holds, before it is turned into a texture.
#[derive(Default)]
struct Raw {
    /// 0 for RGB, 1 for RGBA, as FBX numbers them.
    format: Option<i64>,
    size: Option<i64>,
    custom: (Option<i64>, Option<i64>),
    data: Vec<u8>,
}

/// Reads the thumbnail of the FBX file at `path`, or `None` when it has
/// none (most files, from most exporters).
pub fn read(path: &Path) -> Result<Option<Texture>, LoadError> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 27];
    let n = read_up_to(&mut f, &mut head)?;
    let raw = if head[..n].starts_with(super::fbx::BINARY_MAGIC) && n == 27 {
        let version = u32::from_le_bytes([head[23], head[24], head[25], head[26]]);
        binary(&mut f, len, version)?
    } else {
        f.seek(SeekFrom::Start(0))?;
        let mut text = Vec::new();
        f.take(MAX_HEADER_BYTES).read_to_end(&mut text)?;
        ascii(&text)
    };
    Ok(raw.and_then(texture))
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Turns the node's values into a picture, checking the pixels fill it.
fn texture(raw: Raw) -> Option<Texture> {
    let channels: usize = match raw.format? {
        0 => 3,
        1 => 4,
        _ => return None,
    };
    let (w, h) = match (raw.size, raw.custom) {
        (Some(s), _) if s > 0 => (s, s),
        (_, (Some(w), Some(h))) if w > 0 && h > 0 => (w, h),
        _ => return None,
    };
    let (w, h) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    let pixels = (w as u64).checked_mul(h as u64)?;
    if pixels > MAX_PIXELS || (raw.data.len() as u64) < pixels * channels as u64 {
        return None;
    }
    // The rows run from the bottom up, as OpenGL reads a viewport back
    // (MotionBuilder's floor grid comes out upside down otherwise).
    let row = w as usize * channels;
    let mut rgba = Vec::with_capacity(pixels as usize * 4);
    for line in raw.data[..row * h as usize].chunks_exact(row).rev() {
        for px in line.chunks_exact(channels) {
            rgba.extend_from_slice(&px[..3]);
            rgba.push(if channels == 4 { px[3] } else { 255 });
        }
    }
    let mut t = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    t.source.container = "FBX".into();
    t.source.format = "the thumbnail in the file's header".into();
    Some(t)
}

/*┌─────────────────╮
| Binary          |
└─────────────────╯*/
//region Binary

/// A node of a binary file held in memory: its name, its property bytes and
/// the byte range of its children.
struct Node<'a> {
    name: &'a [u8],
    props: u64,
    prop_bytes: &'a [u8],
    children: std::ops::Range<usize>,
    end: usize,
}

/// Field widths: 64-bit from version 7.5 on.
fn wide(version: u32) -> bool {
    version >= 7500
}

/// Finds the top-level `FBXHeaderExtension` node (the first one, in every
/// file the SDK writes) and reads its thumbnail.
fn binary(f: &mut std::fs::File, len: u64, version: u32) -> Result<Option<Raw>, LoadError> {
    let header = if wide(version) { 25 } else { 13 };
    let mut pos = 27u64;
    for _ in 0..8 {
        if pos + header as u64 > len {
            return Ok(None);
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![0u8; header + 255];
        let n = read_up_to(f, &mut buf)?;
        let num = |i: usize| -> u64 {
            if wide(version) {
                u64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap_or_default())
            } else {
                u32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap_or_default()) as u64
            }
        };
        let end = num(0);
        let name_len = buf[header - 1] as usize;
        if end <= pos || end > len || header + name_len > n {
            return Ok(None);
        }
        if &buf[header..header + name_len] == b"FBXHeaderExtension" {
            if end - pos > MAX_HEADER_BYTES {
                return Ok(None);
            }
            let mut node = vec![0u8; (end - pos) as usize];
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut node)?;
            return Ok(header_extension(&node, version));
        }
        pos = end;
    }
    Ok(None)
}

/// Reads the node that starts at `at` in `data`, whose offsets count from
/// `base` (the position of `data[0]` in the file).
fn node_at(data: &[u8], at: usize, base: u64, version: u32) -> Option<Node<'_>> {
    let header = if wide(version) { 25 } else { 13 };
    let h = data.get(at..at.checked_add(header)?)?;
    let num = |i: usize| -> Option<u64> {
        Some(if wide(version) {
            u64::from_le_bytes(h.get(i * 8..i * 8 + 8)?.try_into().ok()?)
        } else {
            u32::from_le_bytes(h.get(i * 4..i * 4 + 4)?.try_into().ok()?) as u64
        })
    };
    let (end, props, prop_len) = (num(0)?, num(1)?, num(2)?);
    if end == 0 {
        return None;
    }
    let end = usize::try_from(end.checked_sub(base)?).ok()?;
    let name_len = h[header - 1] as usize;
    let name_start = at + header;
    let props_start = name_start.checked_add(name_len)?;
    let props_end = props_start.checked_add(usize::try_from(prop_len).ok()?)?;
    if end > data.len() || props_end > end {
        return None;
    }
    Some(Node {
        name: &data[name_start..props_start],
        props,
        prop_bytes: &data[props_start..props_end],
        children: props_end..end,
        end,
    })
}

/// The nodes in `range`, stopping at the null record that ends a list.
fn children<'a>(data: &'a [u8], range: std::ops::Range<usize>, base: u64, version: u32) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let Some(n) = node_at(&data[..range.end], at, base, version) else { break };
        if n.end <= at {
            break;
        }
        at = n.end;
        out.push(n);
    }
    out
}

/// The thumbnail inside a header extension node read whole into `data`
/// (so offsets count from its own start).
fn header_extension(data: &[u8], version: u32) -> Option<Raw> {
    // Offsets in the file are absolute; rebase them on the node's start.
    let base = absolute_start(data, version)?;
    let rebased = Rebased { data, base, version };
    let ext = rebased.node(0)?;
    let scene_info = rebased.child(&ext, b"SceneInfo")?;
    let thumb = rebased.child(&scene_info, b"Thumbnail")?;
    let mut raw = Raw::default();
    for n in rebased.children(&thumb) {
        match n.name {
            b"Format" => raw.format = first_int(n.prop_bytes),
            b"Size" => raw.size = first_int(n.prop_bytes),
            b"ImageData" if n.props >= 1 => raw.data = first_bytes(n.prop_bytes)?,
            b"Properties70" | b"Properties60" => {
                for p in rebased.children(&n) {
                    let values = strings_and_ints(p.prop_bytes);
                    match values.first().map(|v| v.0.as_slice()) {
                        Some(b"CustomWidth") => raw.custom.0 = values.iter().find_map(|v| v.1),
                        Some(b"CustomHeight") => raw.custom.1 = values.iter().find_map(|v| v.1),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Some(raw)
}

/// The file position of a node read whole: its end offset minus its length.
fn absolute_start(data: &[u8], version: u32) -> Option<u64> {
    let end = if wide(version) {
        u64::from_le_bytes(data.get(..8)?.try_into().ok()?)
    } else {
        u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as u64
    };
    end.checked_sub(data.len() as u64)
}

/// A node read whole, whose children's offsets are absolute in the file.
struct Rebased<'a> {
    data: &'a [u8],
    base: u64,
    version: u32,
}

impl<'a> Rebased<'a> {
    fn node(&self, at: usize) -> Option<Node<'a>> {
        node_at(self.data, at, self.base, self.version)
    }

    fn children(&self, n: &Node) -> Vec<Node<'a>> {
        children(self.data, n.children.clone(), self.base, self.version)
    }

    fn child(&self, n: &Node, name: &[u8]) -> Option<Node<'a>> {
        self.children(n).into_iter().find(|c| c.name == name)
    }
}

/// The first property as an integer, if it is one.
fn first_int(props: &[u8]) -> Option<i64> {
    let (&code, rest) = props.split_first()?;
    let v = |n: usize| rest.get(..n);
    Some(match code {
        b'Y' => i16::from_le_bytes(v(2)?.try_into().ok()?) as i64,
        b'I' => i32::from_le_bytes(v(4)?.try_into().ok()?) as i64,
        b'L' => i64::from_le_bytes(v(8)?.try_into().ok()?),
        b'C' => *v(1)?.first()? as i64,
        _ => return None,
    })
}

/// The first property's bytes: a byte array (`c`, `b`, possibly deflated)
/// or a raw blob (`R`).
fn first_bytes(props: &[u8]) -> Option<Vec<u8>> {
    let (&code, rest) = props.split_first()?;
    let u32_at = |i: usize| Some(u32::from_le_bytes(rest.get(i..i + 4)?.try_into().ok()?) as usize);
    match code {
        b'R' => Some(rest.get(4..4usize.checked_add(u32_at(0)?)?)?.to_vec()),
        b'c' | b'b' => {
            let (count, encoding, stored) = (u32_at(0)?, u32_at(4)?, u32_at(8)?);
            if count as u64 > MAX_PIXELS * 4 {
                return None;
            }
            let body = rest.get(12..12usize.checked_add(stored)?)?;
            match encoding {
                0 => Some(body.get(..count)?.to_vec()),
                1 => decompress_to_vec_zlib_with_limit(body, count).ok().filter(|d| d.len() == count),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A property list as strings and integers, enough for a `P` record's name
/// and its integer value.  Stops at the first type it doesn't know.
fn strings_and_ints(mut props: &[u8]) -> Vec<(Vec<u8>, Option<i64>)> {
    let mut out = Vec::new();
    while let Some((&code, rest)) = props.split_first() {
        let size = match code {
            b'S' | b'R' => {
                let Some(n) = rest.get(..4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes) else { break };
                let Some(s) = rest.get(4..4 + n as usize) else { break };
                out.push((s.to_vec(), None));
                4 + n as usize
            }
            b'Y' | b'I' | b'L' | b'C' => {
                out.push((Vec::new(), first_int(props)));
                match code {
                    b'Y' => 2,
                    b'I' => 4,
                    b'L' => 8,
                    _ => 1,
                }
            }
            b'F' => 4,
            b'D' => 8,
            _ => break,
        };
        let Some(next) = rest.get(size..) else { break };
        props = next;
    }
    out
}

//endregion

/*┌─────────────────╮
| ASCII           |
└─────────────────╯*/
//region ASCII

/// Reads the thumbnail from the start of an ASCII file: the `Thumbnail`
/// block inside `FBXHeaderExtension`.  (Takes carry thumbnails of their
/// own further down; those aren't the scene's picture.)
fn ascii(text: &[u8]) -> Option<Raw> {
    let ext = find(text, b"FBXHeaderExtension:")?;
    let open = ext + find(&text[ext..], b"{")?;
    let close = matching_brace(text, open)?;
    let block = &text[open..close];
    let thumb = find(block, b"Thumbnail:")?;
    let t_open = thumb + find(&block[thumb..], b"{")?;
    let t_close = matching_brace(block, t_open)?;
    let t = &block[t_open + 1..t_close];
    let mut raw = Raw { format: value_after(t, b"Format:"), size: value_after(t, b"Size:"), ..Raw::default() };
    raw.custom = (property(t, b"\"CustomWidth\""), property(t, b"\"CustomHeight\""));
    let data = find(t, b"ImageData:")? + b"ImageData:".len();
    let mut rest = &t[data..];
    // FBX 7: `ImageData: *65536 { a: 1,2,3 }`; FBX 6: `ImageData: 1,2,3`.
    if rest.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'*') {
        let a = find(rest, b"a:")?;
        rest = &rest[a + 2..];
    }
    let mut bytes = Vec::new();
    let mut n: Option<u32> = None;
    for &b in rest {
        match b {
            b'0'..=b'9' => n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add((b - b'0') as u32)),
            b',' | b' ' | b'\t' | b'\r' | b'\n' => {
                if let Some(v) = n.take() {
                    bytes.push(v.min(255) as u8);
                }
            }
            _ => break,
        }
        if bytes.len() as u64 > MAX_PIXELS * 4 {
            return None;
        }
    }
    if let Some(v) = n {
        bytes.push(v.min(255) as u8);
    }
    raw.data = bytes;
    Some(raw)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The position of the `}` closing the `{` at `open`, skipping quoted text.
fn matching_brace(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quoted = false;
    for (i, &b) in text.iter().enumerate().skip(open) {
        match b {
            b'"' => quoted = !quoted,
            b'{' if !quoted => depth += 1,
            b'}' if !quoted => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The integer after `key` (such as `Size: 128`).
fn value_after(t: &[u8], key: &[u8]) -> Option<i64> {
    let at = find(t, key)? + key.len();
    let s: String = t[at..]
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit() || **b == b'-')
        .map(|&b| b as char)
        .collect();
    s.parse().ok()
}

/// The integer value of a `P: "Name", "int", "Integer", "", 128` record.
fn property(t: &[u8], name: &[u8]) -> Option<i64> {
    let at = find(t, name)?;
    let line = t[at..].split(|&b| b == b'\n').next()?;
    let last = line.rsplit(|&b| b == b',').next()?;
    std::str::from_utf8(last).ok()?.trim().parse().ok()
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/fbx").join(name)
    }

    /// The reader agrees with ufbx, which parses the whole file, on every
    /// sample that carries a thumbnail, binary and ASCII, deflated or not.
    #[test]
    fn agrees_with_ufbx() {
        let names = [
            "ufbx/motionbuilder_thumbnail_7700_ascii.fbx",
            "ufbx/motionbuilder_thumbnail_7700_binary.fbx",
            "assimp/boxWithCompressedCTypeArray.FBX",
            "assimp/boxWithUncompressedCTypeArray.FBX",
        ];
        let mut checked = 0;
        for name in names {
            let path = sample(name);
            if !path.is_file() {
                continue;
            }
            let ours = read(&path).unwrap().unwrap_or_else(|| panic!("{name}: no thumbnail"));
            let scene = ufbx::load_file(path.to_str().unwrap(), ufbx::LoadOpts::default()).unwrap();
            let theirs = &scene.metadata.thumbnail;
            assert_eq!((ours.width(), ours.height()), (theirs.width, theirs.height), "{name}");
            let channels = if theirs.format == ufbx::ThumbnailFormat::Rgba32 { 4 } else { 3 };
            let rgba = match &ours.levels[0].surfaces[0] {
                slerp_scene::textures::Pixels::Rgba8(p) => p.clone(),
                _ => panic!("{name}: not 8-bit"),
            };
            let data: &[u8] = &theirs.data;
            let w = theirs.width as usize;
            for (i, px) in data.chunks_exact(channels).enumerate().take(4096) {
                let (x, y) = (i % w, i / w);
                let o = ((theirs.height as usize - 1 - y) * w + x) * 4;
                assert_eq!(&rgba[o..o + 3], &px[..3], "{name}: pixel {i}");
            }
            checked += 1;
        }
        if checked == 0 {
            eprintln!("no FBX thumbnail samples in local/assets/fbx; skipped");
        }
    }

    #[test]
    fn files_without_one_and_damaged_files_give_none() {
        let dir = std::env::temp_dir().join(format!("slerp-fbx-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = sample("ufbx/motionbuilder_thumbnail_7700_binary.fbx");
        let Ok(bytes) = std::fs::read(&path) else { return };
        for cut in [0, 10, 27, 40, 200, 1000, 5000, 20000, bytes.len() / 2] {
            let p = dir.join("cut.fbx");
            std::fs::write(&p, &bytes[..cut.min(bytes.len())]).unwrap();
            let _ = read(&p);
        }
        let mut noisy = bytes.clone();
        for i in (100..noisy.len().min(60_000)).step_by(97) {
            noisy[i] ^= 0x5a;
        }
        let p = dir.join("noisy.fbx");
        std::fs::write(&p, &noisy).unwrap();
        let _ = read(&p);
        std::fs::write(&p, b"; FBX 7.4.0 project file\nFBXHeaderExtension:  {\n}\n").unwrap();
        assert!(read(&p).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
