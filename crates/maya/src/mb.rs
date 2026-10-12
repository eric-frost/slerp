//! Maya binary: the same graph as Maya ASCII, in IFF chunks.
//!
//! The file is one `FOR4` (32-bit sizes) or `FOR8` (64-bit sizes) group of
//! type `Maya`.  Its `HEAD` group holds the version (`VERS`), units (`LUNI`,
//! `AUNI`) and `fileInfo` pairs (`FINF`).  Each node is a group whose type
//! is a four-character code for the node type (`XFRM` transform, `DMSH`
//! mesh, ...); its first chunk, `CREA`, names it and its parent, and the
//! chunks after it set attributes: a name, a flag byte and the value
//! (`DBLE`, `DBL3`, `FLT3`, `STR `, `CMP#` component lists, `MESH` whole
//! meshes, ...).  `SLCT` groups set attributes of existing nodes, and `CONN`
//! groups hold the connections (`CWFL`: a flag byte, source, destination).
//!
//! In `FOR8` files a chunk header is the tag, four bytes the reader skips,
//! and an eight-byte size; `FOR4` headers are the tag and a four-byte size.
//! A group's type follows its header.  Chunk data is padded to a multiple of
//! the size width (4 or 8), counted from the start of the group's contents;
//! groups themselves aren't padded.  Values are big-endian, lengths in
//! centimetres and angles in radians.

use crate::graph::{Attr, ComponentKind, Components, Graph, Value, canon};
use crate::mesh::parse_blob;
use slerp_scene::LoadError;

/// Whether a file starts like Maya binary.
pub fn sniff(head: &[u8]) -> bool {
    match head.get(..4) {
        Some(b"FOR4") => head.get(8..12) == Some(b"Maya"),
        Some(b"FOR8") => head.get(16..20) == Some(b"Maya"),
        _ => false,
    }
}

/// Deepest group nesting followed.  Real files nest three or four deep.
const MAX_DEPTH: u32 = 16;

struct Reader<'a> {
    d: &'a [u8],
    wide: bool,
}

/// A chunk or group header.
struct Header<'a> {
    tag: &'a [u8],
    /// Where the data (for a group, its type) starts and ends.
    body: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn header(&self, at: usize, limit: usize) -> Option<Header<'a>> {
        let (hdr, size) = if self.wide {
            let s = self.d.get(at + 8..at + 16)?;
            (16, u64::from_be_bytes(s.try_into().ok()?))
        } else {
            let s = self.d.get(at + 4..at + 8)?;
            (8, u32::from_be_bytes(s.try_into().ok()?) as u64)
        };
        let body = at.checked_add(hdr)?;
        let end = (body as u64).checked_add(size)?;
        if end > limit as u64 {
            return None;
        }
        Some(Header { tag: self.d.get(at..at + 4)?, body, end: end as usize })
    }

    fn is_group(tag: &[u8]) -> bool {
        matches!(tag, b"FOR4" | b"FOR8" | b"LIS4" | b"LIS8" | b"CAT4" | b"CAT8" | b"PRO4" | b"PRO8")
    }

    /// Calls `f` for each child of the group whose contents run from
    /// `start` to `end`.
    fn children(&self, start: usize, end: usize, mut f: impl FnMut(&Header<'a>)) {
        let align = if self.wide { 8 } else { 4 };
        let mut at = start;
        while at < end {
            let Some(h) = self.header(at, end) else { break };
            f(&h);
            at = if Self::is_group(h.tag) { h.end } else { start + (h.end - start).div_ceil(align) * align };
        }
    }
}

/// Reads a whole Maya binary file into a graph.
pub fn parse(d: &[u8]) -> Result<Graph, LoadError> {
    if !sniff(d) {
        return Err(LoadError::NotMine);
    }
    let r = Reader { d, wide: &d[..4] == b"FOR8" };
    let top = r.header(0, d.len()).or_else(|| {
        // A truncated file: read what is there.
        let hdr = if r.wide { 16 } else { 8 };
        Some(Header { tag: &d[..4], body: hdr, end: d.len() })
    });
    let top = top.ok_or_else(|| LoadError::invalid("Maya binary: no header"))?;
    let mut g = Graph::new();
    g.degrees = false;
    group(&r, &mut g, top.body + 4, top.end, 0);
    Ok(g)
}

/// Reads the groups inside a group: the header, nodes, selections and
/// connection lists.
fn group(r: &Reader, g: &mut Graph, start: usize, end: usize, depth: u32) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut kids: Vec<(Vec<u8>, usize, usize, bool)> = Vec::new();
    r.children(start, end, |h| kids.push((h.tag.to_vec(), h.body, h.end, Reader::is_group(h.tag))));
    for (tag, body, end, is_group) in kids {
        if !is_group {
            continue;
        }
        let Some(kind) = r.d.get(body..body + 4) else { continue };
        let contents = body + 4;
        match kind {
            b"HEAD" => head(r, g, contents, end),
            b"CONN" => connections(r, g, contents, end),
            _ if tag.starts_with(b"LIS") => group(r, g, contents, end, depth + 1),
            b"SLCT" => node(r, g, None, contents, end),
            _ => node(r, g, Some(kind), contents, end),
        }
    }
}

fn cstr(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|&c| c == 0) {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[]),
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(cstr(b).0).into_owned()
}

fn head(r: &Reader, g: &mut Graph, start: usize, end: usize) {
    r.children(start, end, |h| {
        let data = &r.d[h.body..h.end];
        match h.tag {
            b"VERS" => g.version = text(data),
            b"LUNI" => {
                // Binary values are always centimetres; the unit is what
                // Maya showed, kept for the inspector.
                g.file_info.push(("Working units".into(), text(data)));
            }
            b"FINF" => {
                let (k, rest) = cstr(data);
                g.file_info.push((String::from_utf8_lossy(k).into_owned(), text(rest)));
            }
            _ => {}
        }
    });
}

fn connections(r: &Reader, g: &mut Graph, start: usize, end: usize) {
    r.children(start, end, |h| {
        if h.tag != b"CWFL" {
            return;
        }
        let data = r.d.get(h.body + 1..h.end).unwrap_or(&[]);
        let (src, rest) = cstr(data);
        let (dst, _) = cstr(rest);
        let (src, dst) = (String::from_utf8_lossy(src), String::from_utf8_lossy(dst));
        g.connect(&src, &dst);
    });
}

/// The four-character codes of the node types the builder needs.
fn type_name(code: &[u8]) -> Option<&'static str> {
    Some(match code {
        b"XFRM" => "transform",
        b"JOIN" => "joint",
        b"DMSH" => "mesh",
        b"DCAM" => "camera",
        b"RLAM" => "lambert",
        b"RBLN" => "blinn",
        b"RPHO" => "phong",
        b"RPHE" => "phongE",
        b"RSSH" => "surfaceShader",
        b"RTFT" => "file",
        b"RPL2" => "place2dTexture",
        b"RBMP" => "bump2d",
        b"SHAD" => "shadingEngine",
        b"GPID" => "groupId",
        b"GRPP" => "groupParts",
        b"PCUB" => "polyCube",
        b"PSPH" => "polySphere",
        b"PCYL" => "polyCylinder",
        b"PCON" => "polyCone",
        b"PTOR" => "polyTorus",
        b"PMES" => "polyPlane",
        b"PTWK" => "polyTweak",
        b"PCPV" => "polyColorPerVertex",
        b"FSCL" => "skinCluster",
        b"FPOS" => "dagPose",
        b"OBST" => "objectSet",
        b"DECT" => "deleteComponent",
        b"PCHI" => "polyChipOff",
        b"PEXF" => "polyExtrudeFace",
        b"PSPL" => "polySplit",
        b"PSPR" => "polySplitRing",
        b"PBV3" => "polyBevel3",
        b"PSMF" => "polySmoothFace",
        b"PUNI" => "polyUnite",
        b"PCVB" => "polyCBoolOp",
        b"PCLO" => "polyCloseBorder",
        b"PPIP" => "polyPipe",
        b"PCTA" => "animCurveTA",
        b"PCTL" => "animCurveTL",
        b"PCTU" => "animCurveTU",
        b"FMPT" => "tweak",
        b"SCRP" => "script",
        b"DSPL" => "displayLayer",
        _ => return None,
    })
}

/// A node group: `CREA` and attribute chunks, or a `SLCT` and attribute
/// chunks for a node made elsewhere.
fn node(r: &Reader, g: &mut Graph, code: Option<&[u8]>, start: usize, end: usize) {
    let mut current: Option<usize> = None;
    r.children(start, end, |h| {
        let data = &r.d[h.body..h.end];
        match h.tag {
            b"CREA" => {
                let Some(code) = code else { return };
                let kind =
                    type_name(code).map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(code).into_owned());
                // A flag byte, the name, then the parent's name when the
                // node has one, and in newer files 16 bytes of UUID.
                let rest = data.get(1..).unwrap_or(&[]);
                let (name, rest) = cstr(rest);
                let parent = match rest.iter().position(|&c| c == 0) {
                    Some(p)
                        if (rest.len() - p - 1 == 0 || rest.len() - p - 1 == 16)
                            && rest[..p].iter().all(|c| (0x20..0x7F).contains(c))
                            && p > 0 =>
                    {
                        Some(String::from_utf8_lossy(&rest[..p]).into_owned())
                    }
                    _ => None,
                };
                let parent = parent.map(|p| g.find_or_add(&p));
                current = Some(g.add(&kind, &String::from_utf8_lossy(name), parent));
            }
            b"SLCT" => current = Some(g.find_or_add(&text(data))),
            b"FREF" => {
                let path = text(data);
                if !path.is_empty() && !g.references.contains(&path) {
                    g.references.push(path);
                }
            }
            _ => {
                let Some(n) = current else { return };
                if let Some(a) = attribute(h.tag, data) {
                    g.nodes[n].attrs.push(a);
                }
            }
        }
    });
}

/// An attribute chunk: the attribute's name, a flag byte, the value.
fn attribute(tag: &[u8], data: &[u8]) -> Option<Attr> {
    let (name, rest) = cstr(data);
    let value = rest.get(1..)?;
    let doubles = |b: &[u8]| -> Vec<f64> {
        b.chunks_exact(8).map(|c| f64::from_be_bytes(c.try_into().unwrap_or([0; 8]))).collect()
    };
    let floats = |b: &[u8]| -> Vec<f64> {
        b.chunks_exact(4).map(|c| f32::from_be_bytes(c.try_into().unwrap_or([0; 4])) as f64).collect()
    };
    let value = match tag {
        // A skin cluster's weight list: per vertex a count, then influence
        // and weight pairs.
        b"DBLE" if name.starts_with(b"wl[") && name.ends_with(b"].w") => Value::Weights(weights(value)?),
        b"DBLE" | b"DBL2" | b"DBL3" | b"MATR" | b"CMPD" => Value::Nums(doubles(value)),
        b"FLT2" | b"FLT3" => Value::Nums(floats(value)),
        b"STR " => Value::Text(text(value)),
        b"MESH" => Value::Mesh(Box::new(parse_blob(value)?)),
        b"CMP#" => Value::Components(component_list(value)?),
        _ => return None,
    };
    let (key, range) = canon(&String::from_utf8_lossy(name));
    Some(Attr { key, range, value })
}

/// Skin weights: per vertex a count, then that many influence indices
/// (32-bit) and weights (64-bit floats).
fn weights(b: &[u8]) -> Option<Vec<Vec<(u32, f32)>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= b.len() {
        let n = u32::from_be_bytes(b[at..at + 4].try_into().ok()?) as usize;
        at += 4;
        if n > (b.len() - at) / 12 {
            return None;
        }
        let mut list = Vec::with_capacity(n);
        for _ in 0..n {
            let j = u32::from_be_bytes(b[at..at + 4].try_into().ok()?);
            let w = f64::from_be_bytes(b[at + 4..at + 12].try_into().ok()?);
            at += 12;
            if w.is_finite() {
                list.push((j, w as f32));
            }
        }
        out.push(list);
    }
    Some(out)
}

/// A binary component list: a count of runs, each a type code (`CMDF`
/// faces, `CMDE` edges, `CMDV` vertices), a count of ranges, and the
/// ranges' first and last indices (-1 first meaning all of them).
fn component_list(b: &[u8]) -> Option<Vec<Components>> {
    let u32_at = |i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
    let runs = u32_at(0)?.min(1 << 16);
    let mut at = 4;
    let mut out = Vec::new();
    for _ in 0..runs {
        let code = b.get(at..at + 4)?;
        let kind = match code {
            b"CMDF" => ComponentKind::Face,
            b"CMDE" => ComponentKind::Edge,
            b"CMDV" => ComponentKind::Vertex,
            _ => ComponentKind::Other,
        };
        let n = u32_at(at + 4)? as usize;
        at += 8;
        if n > (b.len().saturating_sub(at)) / 8 {
            return None;
        }
        let mut ranges = Vec::with_capacity(n);
        for _ in 0..n {
            let (a, z) = (u32_at(at)?, u32_at(at + 4)?);
            at += 8;
            ranges.push(if a == u32::MAX { None } else { Some((a, z.max(a))) });
        }
        out.push(Components { kind, ranges });
    }
    Some(out)
}
