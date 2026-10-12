//! ZDoom's extended node formats, which UDMF maps (ZNODES) and big
//! binary maps (NODES) use instead of vanilla Doom's 16-bit nodes:
//!
//! - `XNOD`: 32-bit indices and extra vertices the node builder added.
//! - `XGLN`, `XGL2`, `XGL3`: GL nodes, whose subsectors are closed convex
//!   polygons (minisegs close them where no linedef does); `XGL3` stores
//!   partition lines in fixed point.
//! - The `Z` variants are the same, zlib-compressed after the signature.
//!
//! Only what the level builder uses is read: nodes, subsectors and segs.

use super::map::{Bsp, Node, SUBSECTOR, Seg};
use crate::read::Bytes;

/// The largest decompressed node lump: a huge map's are tens of megabytes.
const MAX_BYTES: usize = 512 << 20;
/// The most records of each kind.
const MAX_RECORDS: usize = 1 << 26;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    Gl,
    Gl2,
    Gl3,
}

/// Reads a node lump in one of ZDoom's extended formats.  `map_vertices`
/// is the map's own vertex count, after which the lump's new vertices are
/// numbered.  `None` when the lump is something else or damaged.
pub(crate) fn read_extended(data: &[u8], map_vertices: usize) -> Option<Bsp> {
    let sig = data.get(..4)?;
    let kind = match &sig[1..4] {
        b"NOD" => Kind::Plain,
        b"GLN" => Kind::Gl,
        b"GL2" => Kind::Gl2,
        b"GL3" => Kind::Gl3,
        _ => return None,
    };
    let body: std::borrow::Cow<[u8]> = match sig[0] {
        b'X' => std::borrow::Cow::Borrowed(&data[4..]),
        b'Z' => std::borrow::Cow::Owned(
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data[4..], MAX_BYTES).ok()?,
        ),
        _ => return None,
    };
    let run = std::panic::catch_unwind(|| parse(&body, kind, map_vertices));
    run.ok().flatten()
}

fn parse(d: &[u8], kind: Kind, map_vertices: usize) -> Option<Bsp> {
    let b = Bytes::new(d);
    let count = |at: usize| -> Option<usize> {
        let n = b.u32(at).ok()? as usize;
        (n <= MAX_RECORDS).then_some(n)
    };
    let fixed = |at: usize| -> Option<f64> { Some(b.i32(at).ok()? as f64 / 65536.0) };
    let mut at = 0;

    // 1. Vertices: the map's count, then the new ones.
    let org = count(at)?;
    let new = count(at + 4)?;
    at += 8;
    let mut extra = Vec::with_capacity(new.min(d.len() / 8));
    for _ in 0..new {
        extra.push([fixed(at)?, fixed(at + 4)?]);
        at += 8;
    }
    // Vertex numbers past the map's own index the new vertices, which
    // `MapData::vertex` numbers after the map's.
    let vertex = |v: usize| if v >= org { map_vertices + (v - org) } else { v };

    // 2. Subsectors: each one's seg count; its segs follow on.
    let n_sub = count(at)?;
    at += 4;
    let mut subsectors = Vec::with_capacity(n_sub.min(d.len() / 4));
    let mut first = 0usize;
    for _ in 0..n_sub {
        let n = count(at)?;
        subsectors.push((first, n));
        first = first.checked_add(n)?;
        at += 4;
    }

    // 3. Segs.  GL segs store only their first vertex; the second is the
    //    next seg's first, around the subsector.
    let n_seg = count(at)?;
    at += 4;
    let stride = match kind {
        Kind::Plain | Kind::Gl => 11,
        Kind::Gl2 | Kind::Gl3 => 13,
    };
    b.array(at, n_seg, stride).ok()?;
    let mut segs = Vec::with_capacity(n_seg);
    for _ in 0..n_seg {
        let v1 = b.u32(at).ok()? as usize;
        let (v2, line, side) = match kind {
            Kind::Plain => (b.u32(at + 4).ok()? as usize, b.u16(at + 8).ok()? as usize, b.u8(at + 10).ok()?),
            Kind::Gl => {
                let l = b.u16(at + 8).ok()?;
                (usize::MAX, if l == 0xffff { usize::MAX } else { l as usize }, b.u8(at + 10).ok()?)
            }
            Kind::Gl2 | Kind::Gl3 => {
                let l = b.u32(at + 8).ok()?;
                (usize::MAX, if l == 0xffff_ffff { usize::MAX } else { l as usize }, b.u8(at + 12).ok()?)
            }
        };
        segs.push(Seg { v1: vertex(v1), v2: if v2 == usize::MAX { usize::MAX } else { vertex(v2) }, line, side });
        at += stride;
    }
    if kind != Kind::Plain {
        for &(first, n) in &subsectors {
            if n == 0 || first + n > segs.len() {
                continue;
            }
            for k in 0..n {
                segs[first + k].v2 = segs[first + (k + 1) % n].v1;
            }
        }
    }

    // 4. Nodes.
    let n_node = count(at)?;
    at += 4;
    let stride = if kind == Kind::Gl3 { 40 } else { 32 };
    b.array(at, n_node, stride).ok()?;
    let mut nodes = Vec::with_capacity(n_node);
    for _ in 0..n_node {
        let (x, y, dx, dy, children_at) = if kind == Kind::Gl3 {
            (fixed(at)?, fixed(at + 4)?, fixed(at + 8)?, fixed(at + 12)?, at + 32)
        } else {
            let s = |o: usize| b.i16(at + o).ok().map(|v| v as f64);
            (s(0)?, s(2)?, s(4)?, s(6)?, at + 24)
        };
        let child = |o: usize| -> Option<u32> {
            let c = b.u32(children_at + o).ok()?;
            Some(if c & 0x8000_0000 != 0 { SUBSECTOR | (c & 0x7fff_ffff) } else { c })
        };
        nodes.push(Node { x, y, dx, dy, children: [child(0)?, child(4)?] });
        at += stride;
    }
    Some(Bsp { nodes, subsectors, segs, extra_vertices: extra })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square split by one node into two GL subsectors, in each format,
    /// compressed and not.
    #[test]
    fn extended_nodes() {
        // Map vertices 0-3 (a 64-unit square); the builder adds (32, 0)
        // and (32, 64) as new vertices 4 and 5.
        let mut gl = Vec::new();
        let push32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
        push32(&mut gl, 4);
        push32(&mut gl, 2);
        for (x, y) in [(32, 0), (32, 64)] {
            push32(&mut gl, (x << 16) as u32);
            push32(&mut gl, (y << 16) as u32);
        }
        push32(&mut gl, 2);
        push32(&mut gl, 4);
        push32(&mut gl, 4);
        push32(&mut gl, 8);
        // West half: 0 -> 4 -> 5 -> 3; east half: 4 -> 1 -> 2 -> 5.
        let seg = |v: &mut Vec<u8>, v1: u32, line: u16| {
            v.extend_from_slice(&v1.to_le_bytes());
            v.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
            v.extend_from_slice(&line.to_le_bytes());
            v.push(0);
        };
        for (v1, line) in [(0, 0), (4, 0xffff), (5, 2), (3, 3), (4, 0), (1, 1), (2, 2), (5, 0xffff)] {
            seg(&mut gl, v1, line);
        }
        push32(&mut gl, 1);
        for v in [32i16, 0, 0, 64] {
            gl.extend_from_slice(&v.to_le_bytes());
        }
        gl.extend_from_slice(&[0; 16]);
        push32(&mut gl, 0x8000_0001);
        push32(&mut gl, 0x8000_0000);

        let mut xgln = b"XGLN".to_vec();
        xgln.extend_from_slice(&gl);
        let mut zgln = b"ZGLN".to_vec();
        zgln.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&gl, 6));
        for lump in [xgln, zgln] {
            let bsp = read_extended(&lump, 4).expect("nodes");
            assert_eq!(bsp.extra_vertices, [[32.0, 0.0], [32.0, 64.0]]);
            assert_eq!(bsp.subsectors, [(0, 4), (4, 4)]);
            assert_eq!((bsp.segs[0].v1, bsp.segs[0].v2), (0, 4));
            assert_eq!((bsp.segs[3].v1, bsp.segs[3].v2), (3, 0), "closes the loop");
            assert_eq!(bsp.segs[1].line, usize::MAX, "miniseg");
            assert_eq!(bsp.nodes[0].children, [SUBSECTOR | 1, SUBSECTOR]);
            // Truncated and damaged copies never panic.
            for cut in 0..lump.len() {
                let _ = read_extended(&lump[..cut], 4);
            }
        }
        assert!(read_extended(b"ZGLN\x01\x02\x03", 4).is_none());
        assert!(read_extended(b"XYZW", 4).is_none());
    }
}
