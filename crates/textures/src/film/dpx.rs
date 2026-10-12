//! DPX headers (SMPTE 268M): 2048 bytes in sections, in the byte order the
//! magic number tells (`SDPX` big-endian, `XPDS` little).
//!
//! - 0: file information (offset to the image data, text fields)
//! - 768: image information (size, and up to eight elements of 72 bytes)
//! - 1408: image source information (the picture's aspect ratio)
//! - 1664: motion-picture film information (keycode, frame number, slate)
//! - 1920: television information (time code, frame rate)

use super::{Element, Fields, Frame, Layout, Packing, Transfer, bad, push_text, refuse, row_bytes};
use crate::util::{MAX_DIMENSION, Stream, check_size, read_fully};
use crate::{DecodeOptions, Texture, TextureError};
use std::io::SeekFrom;

pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"SDPX") || head.starts_with(b"XPDS")
}

/// The most padding bytes a header may ask for at the end of a line.
const MAX_PADDING: u32 = 1 << 20;

/// How much of the start of the file holds the headers we read.
const HEADER_BYTES: usize = 2048;

pub(crate) fn decode(r: &mut dyn Stream, len: u64, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let mut head = vec![0u8; HEADER_BYTES];
    r.seek(SeekFrom::Start(0))?;
    let got = read_fully(r, &mut head)?;
    let frame = parse(&head[..got], len)?;
    super::decode_frame(r, len, frame, opts)
}

fn descriptor_name(d: u8) -> &'static str {
    match d {
        1 => "red",
        2 => "green",
        3 => "blue",
        4 => "alpha",
        6 => "luma",
        7 => "chroma",
        8 => "depth",
        9 => "composite video",
        50 => "RGB",
        51 => "RGBA",
        52 => "ABGR",
        100 => "CbYCrY 4:2:2",
        101 => "CbYACrYA 4:2:2:4",
        102 => "CbYCr 4:4:4",
        103 => "CbYCrA 4:4:4:4",
        _ => "user defined",
    }
}

fn transfer_name(t: u8) -> &'static str {
    match t {
        1 => "printing density",
        2 => "linear",
        3 => "logarithmic",
        4 => "unspecified video",
        5 => "SMPTE 274M",
        6 => "Rec. 709",
        7 => "Rec. 601 625-line",
        8 => "Rec. 601 525-line",
        9 => "NTSC composite",
        10 => "PAL composite",
        11 => "Z linear",
        12 => "Z homogeneous",
        13 => "ADX",
        _ => "user defined",
    }
}

struct RawElement {
    low: Option<f64>,
    high: Option<f64>,
    descriptor: u8,
    transfer: u8,
    colorimetric: u8,
    bits: u8,
    packing: u16,
    encoding: u16,
    offset: u32,
    eol: u32,
}

fn parse(head: &[u8], len: u64) -> Result<Frame, TextureError> {
    if head.len() < 0x30C + 72 || !super::dpx::sniff(head) {
        return Err(bad("DPX: header too short"));
    }
    let big = head.starts_with(b"SDPX");
    let f = Fields { data: head, big };
    let data_offset = f.u32(4);
    let (width, height) = (f.u32(0x304), f.u32(0x308));
    let count = f.u16(0x302) as usize;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || !(1..=8).contains(&count) {
        return Err(bad(&format!("DPX: {width}x{height} with {count} elements")));
    }
    check_size(width, height, 1, 16)?;
    let mut raw = Vec::new();
    for i in 0..count {
        let at = 0x30C + i * 72;
        if head.len() < at + 72 {
            return Err(bad("DPX: header ends inside the element list"));
        }
        raw.push(RawElement {
            low: f.defined_u32(at + 4).map(|v| v as f64),
            high: f.defined_u32(at + 12).map(|v| v as f64),
            descriptor: f.u8(at + 20),
            transfer: f.u8(at + 21),
            colorimetric: f.u8(at + 22),
            bits: f.u8(at + 23),
            packing: f.u16(at + 24),
            encoding: f.u16(at + 26),
            offset: f.u32(at + 28),
            eol: f.u32(at + 32),
        });
    }

    // 1. Pick the elements that make the picture: a colour element, else
    //    separate red, green and blue (and alpha) elements, else luma or
    //    depth.
    let find = |d: u8| raw.iter().position(|e| e.descriptor == d);
    let composite = raw.iter().position(|e| matches!(e.descriptor, 50 | 51 | 52 | 100 | 102 | 103));
    let chosen: Vec<(usize, Layout)> = if let Some(i) = composite {
        let layout = match raw[i].descriptor {
            50 => Layout::Rgb,
            51 => Layout::Rgba,
            52 => Layout::Abgr,
            100 => Layout::YCbCr422,
            102 => Layout::YCbCr,
            _ => Layout::YCbCrA,
        };
        vec![(i, layout)]
    } else if let (Some(r), Some(g), Some(b)) = (find(1), find(2), find(3)) {
        let mut v = vec![(r, Layout::Red), (g, Layout::Green), (b, Layout::Blue)];
        if let Some(a) = find(4) {
            v.push((a, Layout::Alpha));
        }
        v
    } else if let Some(i) = find(6).or(find(8)).or(find(9)) {
        vec![(i, Layout::Gray)]
    } else if raw.iter().any(|e| e.descriptor == 101) {
        return Err(refuse("DPX 4:2:2:4 chroma-luma-alpha"));
    } else {
        return Err(refuse(&format!("DPX element type {}", raw[0].descriptor)));
    };

    // 2. Describe how each is stored.
    let mut elements = Vec::new();
    for &(i, layout) in &chosen {
        let e = &raw[i];
        if e.encoding != 0 {
            return Err(refuse("run-length encoded DPX"));
        }
        // Padding is a few bytes at most; a huge value would make the row
        // buffer huge.
        if e.eol > MAX_PADDING {
            return Err(bad("DPX: absurd end-of-line padding"));
        }
        let bits = e.bits as u32;
        if !matches!(bits, 8 | 10 | 12 | 16 | 32 | 64) {
            return Err(refuse(&format!("DPX with {bits}-bit samples")));
        }
        let packing = match (bits, e.packing) {
            (8 | 16 | 32 | 64, _) => Packing::Plain,
            (_, 1) => Packing::FilledA,
            (_, 2) => Packing::FilledB,
            _ => Packing::Packed,
        };
        let transfer = match e.transfer {
            1 | 3 | 13 => Transfer::Log,
            2 | 11 | 12 => Transfer::Linear,
            _ => Transfer::Display,
        };
        let samples = match layout {
            Layout::YCbCr422 => (width as usize).div_ceil(2) * 4,
            l => width as usize * l.samples(),
        };
        let row = row_bytes(bits, packing, samples) as u64;
        let offset = if e.offset != 0 { e.offset } else { data_offset } as u64;
        elements.push(Element {
            offset,
            row_stride: row + e.eol as u64,
            layout,
            bits,
            packing,
            transfer,
            low: e.low,
            high: e.high,
            bt601: matches!(e.colorimetric, 5..=8),
        });
        let _ = len;
    }

    // 3. What the header says about the frame, for the inspector.
    let orientation = f.u16(0x300);
    let first = &raw[chosen[0].0];
    let mut header = Vec::new();
    push_text(&mut header, "Version", f.text(8, 8));
    push_text(&mut header, "Image file name", f.text(36, 100));
    push_text(&mut header, "Created", f.text(136, 24));
    push_text(&mut header, "Creator", f.text(160, 100));
    push_text(&mut header, "Project", f.text(260, 200));
    push_text(&mut header, "Copyright", f.text(460, 200));
    push_text(&mut header, "Description", f.text(0x30C + 40, 32));
    header.push(("Element".into(), descriptor_name(first.descriptor).into()));
    header.push(("Bit depth".into(), first.bits.to_string()));
    header.push(("Transfer".into(), transfer_name(first.transfer).into()));
    if first.encoding != 0 {
        header.push(("Encoding".into(), "run-length".into()));
    }
    if count > 1 {
        header.push(("Elements".into(), count.to_string()));
    }
    header.push(("Orientation".into(), orientation_name(orientation).into()));
    push_text(&mut header, "Source file name", f.text(1432, 100));
    push_text(&mut header, "Source date", f.text(1532, 24));
    push_text(&mut header, "Input device", f.text(1556, 32));
    push_text(&mut header, "Input device serial", f.text(1588, 32));
    let (ah, av) = (f.u32(1628), f.u32(1632));
    if ah != 0 && av != 0 && ah != u32::MAX && av != u32::MAX {
        header.push(("Aspect ratio".into(), format!("{ah}:{av}")));
    }
    film_and_television(&f, &mut header);

    // OpenImageIO, the de facto reference, reads these two numbers as the
    // pixel's shape; headers that mean the picture's shape say 16:9 or the
    // like on pictures that are, or aren't, already that shape.  1:1 is the
    // usual "unspecified" and changes nothing either way.
    let aspect = (ah != 0 && av != 0 && ah != u32::MAX && av != u32::MAX && ah != av && ah < 1 << 20 && av < 1 << 20)
        .then_some(ah as f64 / av as f64);
    let name = match chosen.len() {
        1 => descriptor_name(first.descriptor).to_string(),
        _ => "RGB elements".to_string(),
    };
    Ok(Frame {
        container: "DPX",
        width: width as usize,
        height: height as usize,
        big_endian: big,
        elements,
        flip_x: matches!(orientation, 1 | 3),
        flip_y: matches!(orientation, 2 | 3),
        aspect,
        format: format!("{name} {}-bit, {}", first.bits, transfer_name(first.transfer)),
        header,
    })
}

fn orientation_name(o: u16) -> &'static str {
    match o {
        0 => "left to right, top to bottom",
        1 => "right to left, top to bottom",
        2 => "left to right, bottom to top",
        3 => "right to left, bottom to top",
        4 => "top to bottom, left to right",
        5 => "top to bottom, right to left",
        6 => "bottom to top, left to right",
        7 => "bottom to top, right to left",
        _ => "unknown",
    }
}

/// The film and television sections, when they hold anything.
fn film_and_television(f: &Fields, out: &mut Vec<(String, String)>) {
    // Film: the edge code printed on the stock.
    let (mfg, kind, perfs, prefix, count) =
        (f.text(1664, 2), f.text(1666, 2), f.text(1668, 2), f.text(1670, 6), f.text(1676, 4));
    if !(mfg.is_empty() && kind.is_empty() && prefix.is_empty() && count.is_empty()) {
        out.push(("Keycode".into(), format!("{mfg} {kind} {prefix} {count} {perfs}").trim().to_string()));
    }
    push_text(out, "Film format", f.text(1680, 32));
    if let Some(n) = f.defined_u32(1712) {
        out.push(("Frame position".into(), n.to_string()));
    }
    if let Some(n) = f.defined_u32(1716) {
        out.push(("Sequence length".into(), n.to_string()));
    }
    if let Some(n) = f.defined_u32(1720) {
        out.push(("Held count".into(), n.to_string()));
    }
    if let Some(v) = f.f32(1724) {
        out.push(("Film frame rate".into(), format!("{v}")));
    }
    if let Some(v) = f.f32(1728) {
        out.push(("Shutter angle".into(), format!("{v}")));
    }
    push_text(out, "Frame ID", f.text(1732, 32));
    push_text(out, "Slate", f.text(1764, 100));
    // Television.
    if let Some(tc) = f.defined_u32(1920) {
        out.push((
            "Time code".into(),
            format!("{:02x}:{:02x}:{:02x}:{:02x}", (tc >> 24) & 0x3f, (tc >> 16) & 0x7f, (tc >> 8) & 0x7f, tc & 0x3f),
        ));
    }
    if let Some(v) = f.f32(1944) {
        out.push(("Frame rate".into(), format!("{v}")));
    }
    if let Some(v) = f.f32(1952) {
        out.push(("Gamma".into(), format!("{v}")));
    }
}
