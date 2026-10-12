//! Mechanical and building CAD files, shown by the preview picture each
//! program saves; their geometry is closed (Parasolid, ACIS and the
//! programs' own formats).  Where the pictures live follows cad-preview
//! (github.com/cinderblock/cad-preview, MIT), checked against real files.
//!
//! - **SolidWorks** parts, assemblies and drawings (`.sldprt`, `.sldasm`,
//!   `.slddrw`).  Up to SolidWorks 2014 they are OLE compound files with a
//!   `PreviewPNG` stream (a 640x480 PNG) and a `Preview` stream (a 32-bit
//!   length, then an RLE8 DIB); some files have only the latter.  From 2015
//!   on, the file is SolidWorks' own run of records shaped like zip local
//!   headers (`14 00 06 00 08 00`, time, CRC-32, packed and unpacked sizes,
//!   name and extra lengths), each a raw-deflated former stream whose name
//!   is stored with the two halves of every byte swapped.  The `PreviewPNG`
//!   record lies anywhere from a third of the way through to near the end,
//!   and the same bytes also appear in index-like records near the start,
//!   so a record counts only when it inflates to exactly the size it
//!   states.
//! - **Autodesk Inventor** (`.ipt`, `.iam`, `.idw`, `.ipn`): an OLE file
//!   whose "Inventor Summary Information" property set
//!   (`\x05Zrxrt4arFafyu34gYa3l3ohgHg`) holds the thumbnail as clipboard data
//!   marked as a metafile picture but holding a PNG.
//! - **Autodesk Revit** (`.rvt`, `.rfa`, `.rte`): an OLE file whose
//!   `RevitPreview4.0` stream holds a PNG after a short header.
//! - **Solid Edge** (`.par`, `.psm`, `.asm`, `.dft`): an OLE file whose own
//!   property set (`\x05C3teagxwOttdbfkuIaamtae3Ie`) holds the thumbnail as
//!   clipboard data, a DIB right after the clipboard tag.

use crate::ole::Ole;
use crate::source::Source;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The largest stream or record read for a picture.
const MAX_PICTURE: u64 = 64 << 20;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The programs, by extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    SolidWorks,
    Inventor,
    Revit,
    SolidEdge,
}

impl Program {
    pub fn of_extension(ext: &str) -> Option<Program> {
        Some(match ext {
            "sldprt" | "sldasm" | "slddrw" => Program::SolidWorks,
            "ipt" | "iam" | "idw" | "ipn" => Program::Inventor,
            "rvt" | "rfa" | "rte" => Program::Revit,
            "par" | "psm" | "asm" | "dft" => Program::SolidEdge,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Program::SolidWorks => "SolidWorks file",
            Program::Inventor => "Inventor file",
            Program::Revit => "Revit file",
            Program::SolidEdge => "Solid Edge file",
        }
    }
}

/// The class ids of Solid Edge's documents, which they give their root
/// storage: part, sheet metal part, assembly, draft.
const SOLID_EDGE_CLASSES: [[u8; 16]; 4] = [
    [0x80, 0x2e, 0xc5, 0x23, 0x98, 0x46, 0xce, 0x11, 0xb3, 0x07, 0x08, 0x00, 0x36, 0x3a, 0x1e, 0x02],
    [0xe0, 0x22, 0x85, 0xdd, 0x75, 0x23, 0xd0, 0x11, 0xac, 0x05, 0x08, 0x00, 0x36, 0xfd, 0x18, 0x02],
    [0x00, 0xbf, 0xc6, 0x00, 0x3b, 0x48, 0xce, 0x11, 0x95, 0x1a, 0x08, 0x00, 0x36, 0x01, 0xbe, 0x52],
    [0xfb, 0x11, 0x6b, 0x01, 0xc0, 0xcd, 0xce, 0x11, 0xa0, 0x35, 0x08, 0x00, 0x36, 0x01, 0xe5, 0x3b],
];

/// Solid Edge, recognised in an OLE file under any name by its root
/// storage's class id, when the first directory sector lies within `head`
/// (it does in every sample; `.par` and `.asm` are claimed this way, since
/// parity files and assembler source share them).
pub fn solid_edge_by_class(head: &[u8]) -> Option<Program> {
    let shift = u16::from_le_bytes(head.get(30..32)?.try_into().ok()?);
    let first = u32::from_le_bytes(head.get(48..52)?.try_into().ok()?) as usize;
    if !(shift == 9 || shift == 12) {
        return None;
    }
    let root = first.checked_add(1)?.checked_mul(1 << shift)?.checked_add(80)?;
    let class = head.get(root..root + 16)?;
    SOLID_EDGE_CLASSES.iter().any(|c| c == class).then_some(Program::SolidEdge)
}

/// Whether `head` starts a SolidWorks 2015+ file: four bytes that differ
/// per file, `00 00 00 04`, and soon a record header.
pub fn looks_like_modern_solidworks(head: &[u8]) -> bool {
    head.get(4..8) == Some(&[0, 0, 0, 4]) && head.get(..96).is_some_and(|h| h.windows(6).any(|w| w == RECORD))
}

pub fn preview(program: Program, src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let head = src.head(8)?;
    if !head.starts_with(crate::ole::MAGIC) {
        if program == Program::SolidWorks {
            return modern_solidworks(src, limits);
        }
        return Err(LoadError::NotMine);
    }
    let mut ole = Ole::open(src)?;
    let mut stream = |name: &str| -> Result<Option<Vec<u8>>, LoadError> {
        match ole.find(name)? {
            Some((entry, root)) => ole.read(&entry, &root, MAX_PICTURE).map(Some),
            None => Ok(None),
        }
    };
    let picture = match program {
        Program::SolidWorks => {
            if let Some(png) = stream("PreviewPNG")?.filter(|s| s.starts_with(PNG)) {
                Some(png)
            } else {
                // A 32-bit length, then the DIB.
                stream("Preview")?.and_then(|s| s.get(4..).and_then(crate::bmp_from_dib))
            }
        }
        Program::Revit => stream("RevitPreview4.0")?.and_then(|s| png_within(&s, 4096).map(<[u8]>::to_vec)),
        Program::Inventor => stream("\u{5}Zrxrt4arFafyu34gYa3l3ohgHg")?.and_then(|s| clip_picture(&s)),
        Program::SolidEdge => stream("\u{5}C3teagxwOttdbfkuIaamtae3Ie")?.and_then(|s| clip_picture(&s)),
    };
    let picture = picture.ok_or_else(|| crate::no_preview(program.name()))?;
    crate::decode_image(&picture, limits)
}

/// A PNG starting within the first `within` bytes, to the end.
fn png_within(s: &[u8], within: usize) -> Option<&[u8]> {
    let at = s.get(..within.min(s.len()))?.windows(PNG.len()).position(|w| w == PNG)?;
    s.get(at..)
}

/// The picture in a property set's first clipboard property: a PNG
/// (Inventor's, behind a metafile picture's header) or a DIB (Solid Edge's,
/// right after the clipboard tag), as a file a decoder reads.
fn clip_picture(stream: &[u8]) -> Option<Vec<u8>> {
    let clip = crate::summary::clip_property(stream, None)?;
    if let Some(png) = png_within(clip, 64) {
        return Some(png.to_vec());
    }
    // After the tag: a BITMAPINFOHEADER, or a clipboard format and then one.
    let data = clip.get(4..)?;
    crate::bmp_from_dib(data).or_else(|| crate::bmp_from_dib(data.get(4..)?))
}

/*┌─────────────────╮
| SolidWorks 2015+ |
└─────────────────╯*/
//region SolidWorks 2015+

/// The start of each record: zip's "version 2.0, flags 6, deflated".
const RECORD: [u8; 6] = [0x14, 0x00, 0x06, 0x00, 0x08, 0x00];

/// Bytes read at a time while looking for a record.
const SCAN: u64 = 4 << 20;

/// A name as SolidWorks stores it, the halves of each byte swapped.
fn swapped(name: &str) -> Vec<u8> {
    name.bytes().map(|b| b.rotate_left(4)).collect()
}

fn modern_solidworks(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    if let Some(png) = record(src, "PreviewPNG", |d| d.starts_with(PNG))? {
        return crate::decode_image(&png, limits);
    }
    if let Some(dib) = record(src, "Preview", |d| d.len() > 44 && d[4..8] == [40, 0, 0, 0])?
        && let Some(bmp) = dib.get(4..).and_then(crate::bmp_from_dib)
    {
        return crate::decode_image(&bmp, limits);
    }
    Err(crate::no_preview("SolidWorks file"))
}

/// The inflated data of the first record called `name` that inflates to
/// exactly its stated size and passes `check`.
fn record(src: &mut Source, name: &str, check: impl Fn(&[u8]) -> bool) -> Result<Option<Vec<u8>>, LoadError> {
    let want = swapped(name);
    // The header (26 bytes) comes before the name, so chunks overlap by
    // that and the name.
    let overlap = 26 + want.len() as u64;
    let mut start = 0u64;
    while start < src.len() {
        let n = SCAN.min(src.len() - start);
        let chunk = src.read_at(start, n)?;
        let mut i = 26;
        while i + want.len() <= chunk.len() {
            let Some(found) = chunk[i..].windows(want.len()).position(|w| w == want.as_slice()) else { break };
            let at = i + found;
            i = at + 1;
            let h = &chunk[at - 26..at];
            let u32_at = |k: usize| u32::from_le_bytes([h[k], h[k + 1], h[k + 2], h[k + 3]]) as u64;
            let name_len = u16::from_le_bytes([h[22], h[23]]) as usize;
            let extra = u16::from_le_bytes([h[24], h[25]]) as u64;
            let (packed, size) = (u32_at(14), u32_at(18));
            if h[..6] != RECORD || name_len != want.len() || packed == 0 || packed > MAX_PICTURE || size > MAX_PICTURE {
                continue;
            }
            let data_at = start + at as u64 + want.len() as u64 + extra;
            let Ok(data) = src.exact(data_at, packed) else { continue };
            let Ok(out) = miniz_oxide::inflate::decompress_to_vec_with_limit(&data, size as usize) else { continue };
            if out.len() as u64 == size && check(&out) {
                return Ok(Some(out));
            }
        }
        if start + n >= src.len() {
            break;
        }
        start += n - overlap;
    }
    Ok(None)
}

//endregion
