//! PCD, the Point Cloud Library's format: a text header naming the fields,
//! then the points as text, packed binary rows, or LZF-compressed binary
//! columns (`binary_compressed`).
//!
//! Positions come from the `x`, `y` and `z` fields, colour from `rgb` or
//! `rgba` (packed into one 32-bit field, written as a float or an integer)
//! or separate `r`, `g`, `b` fields, intensity from `intensity`.  PCL keeps
//! clouds in whatever frame the sensor used, so they are shown as they are
//! (Y up).

use crate::common::{parse_f64, tokens};
use crate::points::Cloud;
use glam::DVec3;
use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

/// True when the first lines are PCD header lines.
pub fn looks_like_pcd(head: &[u8]) -> bool {
    let t = crate::common::trim(head);
    (t.starts_with(b"# .PCD") || t.starts_with(b"VERSION")) && crate::common::find(head, b"FIELDS").is_some()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Unsigned,
    Float,
}

struct Field {
    name: String,
    size: usize,
    kind: Kind,
    count: usize,
    /// Byte offset within a binary row.
    offset: usize,
}

/// Reads a value of a field from little-endian bytes.
fn value(f: &Field, b: &[u8]) -> f64 {
    match (f.kind, f.size) {
        (Kind::Float, 4) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        (Kind::Float, 8) => f64::from_le_bytes(b[..8].try_into().unwrap_or_default()),
        (Kind::Unsigned, 1) => b[0] as f64,
        (Kind::Int, 1) => b[0] as i8 as f64,
        (Kind::Unsigned, 2) => u16::from_le_bytes([b[0], b[1]]) as f64,
        (Kind::Int, 2) => i16::from_le_bytes([b[0], b[1]]) as f64,
        (Kind::Unsigned, 4) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        (Kind::Int, 4) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        (Kind::Unsigned, 8) => u64::from_le_bytes(b[..8].try_into().unwrap_or_default()) as f64,
        (Kind::Int, 8) => i64::from_le_bytes(b[..8].try_into().unwrap_or_default()) as f64,
        _ => f64::NAN,
    }
}

/// Where each wanted quantity is: field indices.
struct Layout {
    xyz: [usize; 3],
    packed: Option<usize>,
    rgb: Option<[usize; 3]>,
    intensity: Option<usize>,
}

/// The colour bytes of a packed `rgb` field: a float's or an integer's
/// bits, 0x00RRGGBB either way.
fn unpack(b: &[u8]) -> [u16; 3] {
    let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    [((bits >> 16) & 255) as u16, ((bits >> 8) & 255) as u16, (bits & 255) as u16]
}

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Header lines up to DATA.
    let mut fields: Vec<Field> = Vec::new();
    let (mut sizes, mut kinds, mut counts) = (Vec::new(), Vec::new(), Vec::new());
    let (mut width, mut height, mut points) = (0u64, 1u64, None);
    let mut data = None;
    let mut at = 0usize;
    while at < bytes.len() {
        let end = bytes[at..].iter().position(|&c| c == b'\n').map_or(bytes.len(), |e| at + e + 1);
        let line = &bytes[at..end];
        at = end;
        let mut words = tokens(line);
        let Some(key) = words.next() else { continue };
        let rest: Vec<&[u8]> = words.collect();
        let num = |w: &&[u8]| std::str::from_utf8(w).ok().and_then(|s| s.parse::<u64>().ok());
        match key {
            b"FIELDS" | b"COLUMNS" => {
                fields = rest
                    .iter()
                    .map(|w| Field {
                        name: String::from_utf8_lossy(w).into_owned(),
                        size: 4,
                        kind: Kind::Float,
                        count: 1,
                        offset: 0,
                    })
                    .collect()
            }
            b"SIZE" => sizes = rest.iter().map(|w| num(w).unwrap_or(0) as usize).collect(),
            b"TYPE" => {
                kinds = rest
                    .iter()
                    .map(|w| match w.first() {
                        Some(b'I') => Kind::Int,
                        Some(b'U') => Kind::Unsigned,
                        _ => Kind::Float,
                    })
                    .collect()
            }
            b"COUNT" => counts = rest.iter().map(|w| num(w).unwrap_or(1) as usize).collect(),
            b"WIDTH" => width = rest.first().and_then(num).unwrap_or(0),
            b"HEIGHT" => height = rest.first().and_then(num).unwrap_or(1),
            b"POINTS" => points = rest.first().and_then(num),
            b"DATA" => {
                data = Some(rest.first().map(|w| w.to_ascii_lowercase()).unwrap_or_default());
                break;
            }
            _ => {}
        }
    }
    let data = data.ok_or_else(|| LoadError::invalid("PCD header has no DATA line"))?;
    let mut row = 0usize;
    for (i, f) in fields.iter_mut().enumerate() {
        f.size = sizes.get(i).copied().unwrap_or(4);
        f.kind = kinds.get(i).copied().unwrap_or(Kind::Float);
        f.count = counts.get(i).copied().unwrap_or(1).clamp(1, 4096);
        if !matches!(f.size, 1 | 2 | 4 | 8) {
            return Err(LoadError::invalid(format!("PCD field {} has size {}", f.name, f.size)));
        }
        f.offset = row;
        row += f.size * f.count;
    }
    let index = |name: &str| fields.iter().position(|f| f.name.eq_ignore_ascii_case(name));
    let (Some(x), Some(y), Some(z)) = (index("x"), index("y"), index("z")) else {
        return Err(LoadError::invalid("PCD has no x, y and z fields"));
    };
    let layout = Layout {
        xyz: [x, y, z],
        packed: index("rgb").or_else(|| index("rgba")).filter(|&i| fields[i].size == 4),
        rgb: match (index("r").or(index("red")), index("g").or(index("green")), index("b").or(index("blue"))) {
            (Some(r), Some(g), Some(b)) => Some([r, g, b]),
            _ => None,
        },
        intensity: index("intensity"),
    };
    let stated = points.unwrap_or(width.saturating_mul(height));
    let body = &bytes[at.min(bytes.len())..];

    // 2. The points.
    let mut warnings = Vec::new();
    let format = match data.as_slice() {
        b"ascii" => {
            let mut cloud = Cloud::new(Some(stated.min(body.len() as u64 / 2)), limits);
            let starts: Vec<usize> = {
                let mut s = Vec::with_capacity(fields.len());
                let mut k = 0;
                for f in &fields {
                    s.push(k);
                    k += f.count;
                }
                s
            };
            for line in body.split(|&c| c == b'\n') {
                let words: Vec<&[u8]> = tokens(line).collect();
                if words.len() < 3 {
                    continue;
                }
                if !cloud.wants() {
                    continue;
                }
                let get = |field: usize| words.get(starts[field]).and_then(|w| parse_f64(w)).unwrap_or(f64::NAN);
                let p = DVec3::new(get(layout.xyz[0]), get(layout.xyz[1]), get(layout.xyz[2]));
                let color = if let Some(c) = layout.packed {
                    // Written as a float whose bits are the colour, or as an
                    // integer.
                    words.get(starts[c]).and_then(|w| {
                        let bits = if fields[c].kind == Kind::Float {
                            parse_f64(w).map(|v| (v as f32).to_bits())
                        } else {
                            parse_f64(w).map(|v| v as u32)
                        }?;
                        Some([((bits >> 16) & 255) as u16, ((bits >> 8) & 255) as u16, (bits & 255) as u16])
                    })
                } else {
                    layout.rgb.map(|rgb| rgb.map(|f| get(f).clamp(0.0, 65535.0) as u16))
                };
                let intensity = layout.intensity.map(|f| intensity_u16(get(f), fields[f].kind));
                cloud.push(p, color, intensity);
            }
            finish(cloud, "PCD (text)")
        }
        b"binary" => {
            if row == 0 {
                return Err(LoadError::invalid("PCD rows have no fields"));
            }
            let present = (body.len() / row) as u64;
            if present < stated {
                warnings.push(format!("header says {stated} points, file holds {present}"));
            }
            let n = stated.min(present);
            let mut cloud = Cloud::new(Some(n), limits);
            for r in body.chunks_exact(row).take(n as usize) {
                if cloud.wants() {
                    let get = |i: usize| &r[fields[i].offset..];
                    add(&mut cloud, &fields, &layout, &get);
                }
            }
            finish(cloud, "PCD (binary)")
        }
        b"binary_compressed" => {
            let head = body.get(..8).ok_or_else(|| LoadError::invalid("PCD compressed block is truncated"))?;
            let packed = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            if row == 0 || size > limits.max_file_bytes || size > stated.saturating_mul(row as u64) + 64 {
                return Err(LoadError::invalid("PCD compressed size doesn't match its points"));
            }
            let input = &body[8..(8 + packed).min(body.len())];
            let columns = lzf(input, size as usize);
            if (columns.len() as u64) < size {
                warnings.push("compressed data ends early".into());
            }
            let n = (columns.len() / row) as u64;
            let n = n.min(stated);
            // Columns: each field's values for every point, one field
            // after another.
            let mut column_at = Vec::with_capacity(fields.len());
            let mut k = 0usize;
            for f in &fields {
                column_at.push(k);
                k += f.size * f.count * n as usize;
            }
            let mut cloud = Cloud::new(Some(n), limits);
            for i in 0..n as usize {
                if cloud.wants() {
                    let get = |f: usize| {
                        let o = column_at[f] + i * fields[f].size * fields[f].count;
                        &columns[o.min(columns.len())..]
                    };
                    add(&mut cloud, &fields, &layout, &get);
                }
            }
            finish(cloud, "PCD (compressed binary)")
        }
        other => return Err(LoadError::Unsupported(format!("PCD data {}", String::from_utf8_lossy(other)))),
    };
    let mut scene = format?;
    scene.info.warnings.extend(warnings);
    Ok(scene)
}

fn finish(cloud: Cloud, format: &str) -> Result<Scene, LoadError> {
    cloud.finish(format, false)
}

/// An intensity as a 16-bit value: floats of 0 to 1 are scaled up, others
/// are taken as counts.
fn intensity_u16(v: f64, kind: Kind) -> u16 {
    if !v.is_finite() {
        0
    } else if kind == Kind::Float && v <= 1.0 {
        (v.max(0.0) * 65535.0) as u16
    } else {
        v.clamp(0.0, 65535.0) as u16
    }
}

/// Adds one binary point, reading each field's bytes through `get`.
fn add<'a>(cloud: &mut Cloud, fields: &[Field], l: &Layout, get: &dyn Fn(usize) -> &'a [u8]) {
    let read = |i: usize| {
        let b = get(i);
        if b.len() < fields[i].size { f64::NAN } else { value(&fields[i], b) }
    };
    let p = DVec3::new(read(l.xyz[0]), read(l.xyz[1]), read(l.xyz[2]));
    let color = if let Some(c) = l.packed {
        let b = get(c);
        (b.len() >= 4).then(|| unpack(b))
    } else {
        l.rgb.map(|rgb| rgb.map(|f| read(f).clamp(0.0, 65535.0) as u16))
    };
    let intensity = l.intensity.map(|f| intensity_u16(read(f), fields[f].kind));
    cloud.push(p, color, intensity);
}

/// LZF decompression, at most `size` bytes.  Damaged input ends early.
fn lzf(input: &[u8], size: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(size.min(1 << 28));
    let mut i = 0usize;
    while i < input.len() && out.len() < size {
        let ctrl = input[i] as usize;
        i += 1;
        if ctrl < 32 {
            let len = ctrl + 1;
            let Some(literal) = input.get(i..i + len) else { break };
            out.extend_from_slice(literal);
            i += len;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                let Some(&extra) = input.get(i) else { break };
                len += extra as usize;
                i += 1;
            }
            let Some(&low) = input.get(i) else { break };
            i += 1;
            let back = ((ctrl & 0x1f) << 8) + low as usize + 1;
            let Some(start) = out.len().checked_sub(back) else { break };
            for k in 0..len + 2 {
                let b = out[start + k];
                out.push(b);
            }
        }
    }
    out.truncate(size);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII: &str = "# .PCD v0.7 - Point Cloud Data file format\nVERSION 0.7\nFIELDS x y z rgb\nSIZE 4 4 4 4\nTYPE F F F U\nCOUNT 1 1 1 1\nWIDTH 3\nHEIGHT 1\nVIEWPOINT 0 0 0 1 0 0 0\nPOINTS 3\nDATA ascii\n0 0 0 16711680\n1 0 0 65280\nnan nan nan 255\n";

    #[test]
    fn reads_ascii_binary_and_compressed() {
        assert!(looks_like_pcd(ASCII.as_bytes()));
        let s = load(Path::new("a.pcd"), ASCII.as_bytes(), &Limits::viewer()).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 2);
        assert_eq!(p.colors[0], glam::Vec4::new(1.0, 0.0, 0.0, 1.0));

        let header = "VERSION .7\nFIELDS x y z intensity\nSIZE 4 4 4 2\nTYPE F F F U\nCOUNT 1 1 1 1\nWIDTH 2\nHEIGHT 1\nPOINTS 2\n";
        let mut b = format!("{header}DATA binary\n").into_bytes();
        for (x, i) in [(0f32, 10u16), (2.0, 200)] {
            for v in [x, 0.0, 0.0] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&i.to_le_bytes());
        }
        let s = load(Path::new("b.pcd"), &b, &Limits::viewer()).unwrap();
        assert_eq!(s.meshes[0].primitives[0].positions.len(), 2);
        for cut in 0..b.len() {
            let _ = load(Path::new("b.pcd"), &b[..cut], &Limits::viewer());
        }

        // Columns, LZF-compressed as literals only.
        let mut columns = Vec::new();
        for v in [0f32, 2.0, 0.0, 0.0, 0.0, 0.0] {
            columns.extend_from_slice(&v.to_le_bytes());
        }
        columns.extend_from_slice(&[10, 0, 200, 0]);
        let mut packed = vec![(columns.len() - 1) as u8];
        packed.extend_from_slice(&columns);
        let mut c = format!("{header}DATA binary_compressed\n").into_bytes();
        c.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        c.extend_from_slice(&(columns.len() as u32).to_le_bytes());
        c.extend_from_slice(&packed);
        let s = load(Path::new("c.pcd"), &c, &Limits::viewer()).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 2);
        assert!((p.positions[1].x - p.positions[0].x - 2.0).abs() < 1e-6);
        for cut in 0..c.len() {
            let _ = load(Path::new("c.pcd"), &c[..cut], &Limits::viewer());
        }
    }

    #[test]
    fn lzf_back_references() {
        // "abcabcabc": three literals, then a back-reference of 6 bytes
        // three back.
        let input = [2, b'a', b'b', b'c', (4 << 5) as u8, 2];
        assert_eq!(lzf(&input, 100), b"abcabcabc");
        assert_eq!(lzf(&[0xff, 0xff, 0xff], 100), b"");
    }
}
