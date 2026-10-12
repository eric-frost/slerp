//! Cineon headers: Kodak's 1994 film scan format, always big-endian by
//! magic `80 2A 5F D7` (its byte-swapped twin is read too).
//!
//! - 0: file information (offset to the image data)
//! - 192: image information (channels with their bit depth and size,
//!   interleave, packing, end-of-line padding)
//! - 712: image origination (device, pitch, gamma)
//! - 1024: film information (keycode, frame position, slate)

use super::{Element, Fields, Frame, Layout, Packing, Transfer, bad, push_text, refuse, row_bytes};
use crate::util::{MAX_DIMENSION, Stream, check_size, read_fully};
use crate::{DecodeOptions, Texture, TextureError};
use std::io::SeekFrom;

const MAGIC: [u8; 4] = [0x80, 0x2a, 0x5f, 0xd7];

pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(&MAGIC) || (head.len() >= 4 && head[..4] == [0xd7, 0x5f, 0x2a, 0x80])
}

/// The most padding bytes a header may ask for at the end of a line.
const MAX_PADDING: u32 = 1 << 20;

/// The headers fit in the first 2 KB (1024 generic and the film section).
const HEADER_BYTES: usize = 2048;

pub(crate) fn decode(r: &mut dyn Stream, len: u64, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let mut head = vec![0u8; HEADER_BYTES];
    r.seek(SeekFrom::Start(0))?;
    let got = read_fully(r, &mut head)?;
    let frame = parse(&head[..got])?;
    super::decode_frame(r, len, frame, opts)
}

fn parse(head: &[u8]) -> Result<Frame, TextureError> {
    if head.len() < 1024 || !sniff(head) {
        return Err(bad("Cineon: header too short"));
    }
    let big = head.starts_with(&MAGIC);
    let f = Fields { data: head, big };
    let data_offset = f.u32(4) as u64;
    let channels = f.u8(193) as usize;
    if !(1..=8).contains(&channels) {
        return Err(bad(&format!("Cineon: {channels} channels")));
    }
    // Every channel has its own size and depth; we need them equal.
    let chan = |i: usize| 196 + 28 * i;
    let (width, height, bits) = (f.u32(chan(0) + 4), f.u32(chan(0) + 8), f.u8(chan(0) + 2) as u32);
    for i in 1..channels {
        if (f.u32(chan(i) + 4), f.u32(chan(i) + 8), f.u8(chan(i) + 2) as u32) != (width, height, bits) {
            return Err(refuse("Cineon channels of different sizes or depths"));
        }
    }
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(bad(&format!("Cineon: {width}x{height}")));
    }
    check_size(width, height, 1, 16)?;
    if !matches!(bits, 8 | 10 | 12 | 16) {
        return Err(refuse(&format!("Cineon with {bits}-bit samples")));
    }
    let (interleave, packing_code, eol, eoc) = (f.u8(680), f.u8(681), f.u32(684) as u64, f.u32(688) as u64);
    if eol > MAX_PADDING as u64 {
        return Err(bad("Cineon: absurd end-of-line padding"));
    }
    // libcineon's packing codes: 0 packed, then byte, word and long word
    // alignment each left or right justified.
    let packing = match (bits, packing_code) {
        (8 | 16, _) => Packing::Plain,
        (_, 0) => Packing::Packed,
        (_, 5) => Packing::FilledA,
        (_, 6) => Packing::FilledB,
        (12, 3) => Packing::FilledA,
        (12, 4) => Packing::FilledB,
        (_, 1 | 3) => Packing::FilledA,
        (_, 2 | 4) => Packing::FilledB,
        _ => Packing::Packed,
    };
    let (w, h) = (width as usize, height as usize);
    let mut elements = Vec::new();
    let base = Element {
        offset: data_offset,
        row_stride: 0,
        layout: Layout::Rgb,
        bits,
        packing,
        transfer: Transfer::Log,
        low: None,
        high: None,
        bt601: false,
    };
    // What each channel is, from its designator.
    let colour = |i: usize| f.u8(chan(i) + 1);
    let layout = match (channels, interleave) {
        (1, _) => Layout::Gray,
        (3, _) => Layout::Rgb,
        _ => return Err(refuse(&format!("Cineon with {channels} channels"))),
    };
    if interleave == 0 || channels == 1 {
        let row = row_bytes(bits, packing, w * layout.samples()) as u64;
        elements.push(Element { layout, row_stride: row + eol, ..base });
    } else {
        // Line- or plane-interleaved: one element per channel.
        let row = row_bytes(bits, packing, w) as u64;
        for i in 0..3 {
            let layout = match colour(i) {
                1 => Layout::Red,
                2 => Layout::Green,
                3 => Layout::Blue,
                _ => [Layout::Red, Layout::Green, Layout::Blue][i],
            };
            let (offset, stride) = if interleave == 1 {
                (data_offset + i as u64 * (row + eol), 3 * (row + eol))
            } else {
                (data_offset + i as u64 * (h as u64 * (row + eol) + eoc), row + eol)
            };
            elements.push(Element { layout, offset, row_stride: stride, ..base });
        }
    }

    // The inspector's fields.
    let mut header = Vec::new();
    push_text(&mut header, "Version", f.text(24, 8));
    push_text(&mut header, "Image file name", f.text(32, 100));
    let created = format!("{} {}", f.text(132, 12), f.text(144, 12)).trim().to_string();
    push_text(&mut header, "Created", created);
    push_text(&mut header, "Label", f.text(452, 200));
    push_text(&mut header, "Source file name", f.text(720, 100));
    push_text(&mut header, "Input device", f.text(844, 64));
    push_text(&mut header, "Input device model", f.text(908, 32));
    push_text(&mut header, "Input device serial", f.text(940, 32));
    if let Some(g) = f.f32(980) {
        header.push(("Gamma".into(), format!("{g}")));
    }
    header.push(("Bit depth".into(), bits.to_string()));
    header.push(("Channels".into(), channels.to_string()));
    if f.u8(683) == 1 {
        header.push(("Image sense".into(), "negative".into()));
    }
    // Film information, when the industry header is there.
    if head.len() >= 1024 + 288 {
        let (mfg, kind, perfs, prefix, count) =
            (f.text(1024, 2), f.text(1026, 2), f.text(1028, 2), f.text(1030, 6), f.text(1036, 4));
        if !(mfg.is_empty() && kind.is_empty() && prefix.is_empty() && count.is_empty()) {
            header.push(("Keycode".into(), format!("{mfg} {kind} {prefix} {count} {perfs}").trim().to_string()));
        }
        push_text(&mut header, "Film format", f.text(1040, 32));
        if let Some(n) = f.defined_u32(1072) {
            header.push(("Frame position".into(), n.to_string()));
        }
        if let Some(v) = f.f32(1076) {
            header.push(("Frame rate".into(), format!("{v}")));
        }
        push_text(&mut header, "Frame ID", f.text(1080, 32));
        push_text(&mut header, "Slate", f.text(1112, 200));
    }
    let orientation = f.u8(192);
    Ok(Frame {
        container: "Cineon",
        width: w,
        height: h,
        big_endian: big,
        elements,
        flip_x: matches!(orientation, 1 | 3),
        flip_y: matches!(orientation, 2 | 3),
        aspect: None,
        format: format!("{} {bits}-bit, printing density", if channels == 1 { "grey" } else { "RGB" }),
        header,
    })
}
