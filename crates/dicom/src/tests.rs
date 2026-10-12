//! Tests on data sets written here: every VR encoding, transfer syntaxes,
//! rescale and windowing, MONOCHROME1, YBR, palettes, multi-frame files,
//! DICOMDIRs, and damaged files that must fail cleanly.

use crate::data::{Tag, Value, Vr, tag, tags};
use crate::image::FramePixels;
use crate::parse::{Extent, parse_bytes};
use crate::window::{Voi, Window};
use crate::{Dicom, Photometric};

/*┌─────────────────╮
| Writer          |
└─────────────────╯*/
//region Writer

/// Writes data sets the way files encode them, for tests.
#[derive(Clone, Copy)]
pub struct Enc {
    pub explicit: bool,
    pub big: bool,
}

pub const ELE: Enc = Enc { explicit: true, big: false };
pub const ILE: Enc = Enc { explicit: false, big: false };
pub const EBE: Enc = Enc { explicit: true, big: true };

impl Enc {
    fn u16(self, v: u16) -> [u8; 2] {
        if self.big { v.to_be_bytes() } else { v.to_le_bytes() }
    }

    fn u32(self, v: u32) -> [u8; 4] {
        if self.big { v.to_be_bytes() } else { v.to_le_bytes() }
    }

    /// One element with a defined length.
    pub fn element(self, t: Tag, vr: Vr, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.u16((t >> 16) as u16));
        out.extend(self.u16(t as u16));
        let mut value = value.to_vec();
        if value.len() % 2 == 1 {
            value.push(if vr.is_text() && vr != Vr::UI { b' ' } else { 0 });
        }
        if self.explicit {
            out.extend(vr.0);
            if vr.long_length() {
                out.extend([0, 0]);
                out.extend(self.u32(value.len() as u32));
            } else {
                out.extend(self.u16(value.len() as u16));
            }
        } else {
            out.extend(self.u32(value.len() as u32));
        }
        out.extend(value);
        out
    }

    pub fn text(self, t: Tag, vr: Vr, s: &str) -> Vec<u8> {
        self.element(t, vr, s.as_bytes())
    }

    pub fn us(self, t: Tag, v: &[u16]) -> Vec<u8> {
        let b: Vec<u8> = v.iter().flat_map(|&x| self.u16(x)).collect();
        self.element(t, Vr::US, &b)
    }

    pub fn ow(self, t: Tag, v: &[u16]) -> Vec<u8> {
        let b: Vec<u8> = v.iter().flat_map(|&x| self.u16(x)).collect();
        self.element(t, Vr::OW, &b)
    }

    /// A sequence of undefined length with undefined-length items.
    pub fn sequence(self, t: Tag, items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.u16((t >> 16) as u16));
        out.extend(self.u16(t as u16));
        if self.explicit {
            out.extend(b"SQ");
            out.extend([0, 0]);
        }
        out.extend(self.u32(u32::MAX));
        for item in items {
            out.extend(self.u16(0xFFFE));
            out.extend(self.u16(0xE000));
            out.extend(self.u32(u32::MAX));
            out.extend(item);
            out.extend(self.u16(0xFFFE));
            out.extend(self.u16(0xE00D));
            out.extend(self.u32(0));
        }
        out.extend(self.u16(0xFFFE));
        out.extend(self.u16(0xE0DD));
        out.extend(self.u32(0));
        out
    }

    /// Encapsulated pixel data: an empty offset table, then `fragments`.
    pub fn encapsulated(self, fragments: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(self.u16(0x7FE0));
        out.extend(self.u16(0x0010));
        out.extend(b"OB");
        out.extend([0, 0]);
        out.extend(self.u32(u32::MAX));
        let mut item = |data: &[u8]| {
            out.extend(self.u16(0xFFFE));
            out.extend(self.u16(0xE000));
            out.extend(self.u32(data.len() as u32));
            out.extend(data);
        };
        item(&[]);
        for f in fragments {
            let mut f = f.clone();
            if f.len() % 2 == 1 {
                f.push(0);
            }
            item(&f);
        }
        out.extend(self.u16(0xFFFE));
        out.extend(self.u16(0xE0DD));
        out.extend(self.u32(0));
        out
    }
}

/// A Part 10 file: preamble, `DICM`, meta information naming `ts`, then
/// the data set's elements (which must be in tag order).
pub fn part10(ts: &str, body: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out.extend(b"DICM");
    let mut meta = Vec::new();
    meta.extend(ELE.element(tag(0x0002, 0x0001), Vr::OB, &[0, 1]));
    meta.extend(ELE.text(tags::MEDIA_STORAGE_SOP_CLASS, Vr::UI, "1.2.840.10008.5.1.4.1.1.7"));
    meta.extend(ELE.text(tags::TRANSFER_SYNTAX, Vr::UI, ts));
    out.extend(ELE.element(tag(0x0002, 0x0000), Vr::UL, &(meta.len() as u32).to_le_bytes()));
    out.extend(meta);
    for e in body {
        out.extend(e);
    }
    out
}

/// The elements of a small greyscale image in `enc`, pixel data last.
pub fn grey_image(
    enc: Enc,
    rows: u16,
    cols: u16,
    bits: u16,
    signed: bool,
    extra: &[Vec<u8>],
    pixels: &[u8],
) -> Vec<Vec<u8>> {
    let mut v = vec![
        enc.text(tags::SOP_CLASS, Vr::UI, "1.2.840.10008.5.1.4.1.1.7"),
        enc.text(tags::MODALITY, Vr::CS, "OT"),
        enc.text(tags::PATIENT_NAME, Vr::PN, "Doe^Jane"),
        enc.text(tags::SERIES_INSTANCE_UID, Vr::UI, "1.2.3.4"),
        enc.us(tags::SAMPLES_PER_PIXEL, &[1]),
        enc.text(tags::PHOTOMETRIC, Vr::CS, "MONOCHROME2"),
        enc.us(tags::ROWS, &[rows]),
        enc.us(tags::COLUMNS, &[cols]),
        enc.us(tags::BITS_ALLOCATED, &[if bits > 8 { 16 } else { 8 }]),
        enc.us(tags::BITS_STORED, &[bits]),
        enc.us(tags::HIGH_BIT, &[bits - 1]),
        enc.us(tags::PIXEL_REPRESENTATION, &[signed as u16]),
    ];
    v.extend(extra.iter().cloned());
    v.sort_by_key(|e| {
        let rd = |i: usize| u16::from_le_bytes([e[i], e[i + 1]]);
        let rb = |i: usize| u16::from_be_bytes([e[i], e[i + 1]]);
        if enc.big { (rb(0), rb(2)) } else { (rd(0), rd(2)) }
    });
    let vr = if bits > 8 || !enc.explicit { Vr::OW } else { Vr::OB };
    v.push(enc.element(tags::PIXEL_DATA, vr, pixels));
    v
}

fn words(enc: Enc, v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|&x| enc.u16(x as u16)).collect()
}

fn grey(d: &Dicom, i: u32) -> Vec<f32> {
    match d.decode_frame(i, None).unwrap().pixels {
        FramePixels::Grey(v) => v,
        FramePixels::Rgb(_) => panic!("expected greyscale"),
    }
}

fn rgb(d: &Dicom, i: u32) -> Vec<u8> {
    match d.decode_frame(i, None).unwrap().pixels {
        FramePixels::Rgb(v) => v,
        FramePixels::Grey(_) => panic!("expected colour"),
    }
}

//endregion

/*┌─────────────────╮
| Encodings       |
└─────────────────╯*/
//region Encodings

/// The same signed 12-bit image in each of the three uncompressed
/// encodings reads to the same values, with rescale applied.
#[test]
fn explicit_implicit_and_big_endian() {
    let values = [-1000, 0, 40, 2047];
    for (ts, enc) in [("1.2.840.10008.1.2.1", ELE), ("1.2.840.10008.1.2", ILE), ("1.2.840.10008.1.2.2", EBE)] {
        let extra = [enc.text(tags::RESCALE_INTERCEPT, Vr::DS, "-24"), enc.text(tags::RESCALE_SLOPE, Vr::DS, "1.0")];
        let file = part10(ts, &grey_image(enc, 2, 2, 12, true, &extra, &words(enc, &values)));
        let d = Dicom::from_bytes(file).unwrap();
        assert_eq!(d.parsed.transfer.explicit_vr, enc.explicit, "{ts}");
        assert_eq!(grey(&d, 0), vec![-1024.0, -24.0, 16.0, 2023.0], "{ts}");
        assert_eq!(d.data().string(tags::PATIENT_NAME).as_deref(), Some("Doe^Jane"));
        assert_eq!(d.data().get(tags::ROWS).unwrap().vr, Vr::US);
    }
}

/// Files without the preamble are recognised and read, in either VR.
#[test]
fn bare_data_sets() {
    for enc in [ELE, ILE, EBE] {
        let body: Vec<u8> = grey_image(enc, 1, 2, 8, false, &[], &[3, 200]).concat();
        assert!(crate::parse::looks_like_dicom(&body));
        let d = Dicom::from_bytes(body).unwrap();
        assert!(!d.parsed.part10);
        assert_eq!(grey(&d, 0), vec![3.0, 200.0]);
    }
    // Text that happens to start with zeros is not DICOM.
    assert!(!crate::parse::looks_like_dicom(b"\x08\x00\x05\x00hello world, this is not a data set at all"));
    assert!(!crate::parse::looks_like_dicom(&[0u8; 300]));
}

/// A file whose meta information says explicit VR but whose data set is
/// implicit still reads.
#[test]
fn mislabelled_vr() {
    let body = grey_image(ILE, 1, 1, 8, false, &[], &[9, 0]);
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert!(!d.parsed.transfer.explicit_vr);
    assert_eq!(grey(&d, 0), vec![9.0]);
}

/// Deflated data sets inflate before parsing.
#[test]
fn deflated() {
    let body: Vec<u8> = grey_image(ELE, 1, 3, 8, false, &[], &[1, 2, 3]).concat();
    let packed = miniz_oxide::deflate::compress_to_vec(&body, 6);
    let file = part10("1.2.840.10008.1.2.1.99", &[packed]);
    let d = Dicom::from_bytes(file).unwrap();
    assert_eq!(grey(&d, 0), vec![1.0, 2.0, 3.0]);
}

/// Nested sequences of defined and undefined length, and an undefined
/// length UN element holding an implicit VR sequence (CP-246).
#[test]
fn sequences() {
    let inner = ELE.sequence(tag(0x0040, 0xA730), &[ELE.text(tag(0x0040, 0xA160), Vr::UT, "deep")]);
    let item = [ELE.text(tag(0x0008, 0x0100), Vr::SH, "CODE"), inner].concat();
    let seq = ELE.sequence(tag(0x0008, 0x1140), &[item.clone(), item]);
    // A private sequence written as UN with undefined length.
    let mut un = Vec::new();
    un.extend(0x0029u16.to_le_bytes());
    un.extend(0x1010u16.to_le_bytes());
    un.extend(b"UN\0\0");
    un.extend(u32::MAX.to_le_bytes());
    un.extend(ILE.sequence(0, &[ILE.text(tag(0x0008, 0x0100), Vr::SH, "X1")])[8..].iter());
    let body = vec![ELE.text(tags::MODALITY, Vr::CS, "SR"), seq, un];
    let p = parse_bytes(part10("1.2.840.10008.1.2.1", &body), Extent::All).unwrap();
    let items = p.data.items(tag(0x0008, 0x1140));
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].items(tag(0x0040, 0xA730))[0].string(tag(0x0040, 0xA160)).as_deref(), Some("deep"));
    let private = p.data.items(tag(0x0029, 0x1010));
    assert_eq!(private.len(), 1);
    assert_eq!(private[0].string(tag(0x0008, 0x0100)).as_deref(), Some("X1"));
    // The inspector nests them.
    let rows = crate::text::rows(&p.data);
    let seq_row = rows.iter().find(|r| r.tag == tag(0x0008, 0x1140)).unwrap();
    assert_eq!(seq_row.items.len(), 2);
    assert_eq!(seq_row.value, "2 items");
}

//endregion

/*┌─────────────────╮
| Pixels          |
└─────────────────╯*/
//region Pixels

/// High bit below the top, unused bits set (overlay bits in old files)
/// and a sign: masked and sign-extended per Bits Stored and High Bit.
#[test]
fn bits_stored_and_high_bit() {
    let enc = ELE;
    // 10 bits stored at bits 2..11, high bit 11, signed; garbage in the
    // top bits and the low two bits.
    let raw: [u16; 2] = [0xF000 | (0x3FF << 2) | 0b11, (0x001 << 2) | 0xC000];
    let mut body = grey_image(enc, 1, 2, 10, true, &[], &words(enc, &raw.map(|v| v as i32)));
    // Override the high bit to 11.
    let hb = body.iter().position(|e| e[..4] == [0x28, 0, 0x02, 0x01]).unwrap();
    body[hb] = enc.us(tags::HIGH_BIT, &[11]);
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert_eq!(grey(&d, 0), vec![-1.0, 1.0]);
}

/// MONOCHROME1 inverts, and with no stored window the image's own range
/// is used.
#[test]
fn monochrome1_and_auto_window() {
    let mut body = grey_image(ELE, 1, 2, 8, false, &[], &[10, 20]);
    let p = body.iter().position(|e| e[..4] == [0x28, 0, 0x04, 0]).unwrap();
    body[p] = ELE.text(tags::PHOTOMETRIC, Vr::CS, "MONOCHROME1");
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert!(d.inverted());
    let frame = d.decode_frame(0, None).unwrap();
    let voi = d.default_voi(0, &frame);
    assert_eq!(voi, Voi::Window(Window::spanning(10.0, 20.0)));
    let tex = d.picture(&frame, &voi, d.inverted(), 1.0);
    let slerp_scene::textures::Pixels::Rgba8(px) = tex.base() else { panic!() };
    // Low values bright.
    assert!(px[0] > 250 && px[4] < 5, "{px:?}");
}

/// The first of several stored windows is the default; others are listed
/// with their explanations.
#[test]
fn stored_windows() {
    let extra = [
        ELE.text(tags::WINDOW_CENTER, Vr::DS, "40\\400"),
        ELE.text(tags::WINDOW_WIDTH, Vr::DS, "80\\1800"),
        ELE.text(tags::WINDOW_EXPLANATION, Vr::LO, "BRAIN\\BONE"),
    ];
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 1, 2, 8, false, &extra, &[30, 50]))).unwrap();
    let stored = d.stored_vois(0);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].name, "BRAIN");
    assert_eq!(stored[1].voi, Voi::Window(Window::new(400.0, 1800.0)));
    let frame = d.decode_frame(0, None).unwrap();
    assert_eq!(d.default_voi(0, &frame), Voi::Window(Window::new(40.0, 80.0)));
}

/// A stored window that leaves the whole image white is set aside for the
/// image's own range, but stays in the list of windows.
#[test]
fn unusable_stored_window_gives_way() {
    let extra = [ELE.text(tags::WINDOW_CENTER, Vr::DS, "-600"), ELE.text(tags::WINDOW_WIDTH, Vr::DS, "1600")];
    let pixels: Vec<u8> = (0..16u16).flat_map(|i| (3000 + i * 200).to_le_bytes()).collect();
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 4, 4, 14, false, &extra, &pixels))).unwrap();
    let frame = d.decode_frame(0, None).unwrap();
    assert_eq!(d.stored_vois(0).len(), 1);
    assert_eq!(d.default_voi(0, &frame), Voi::Window(Window::spanning(3000.0, 6000.0)));
}

/// A VOI LUT Sequence maps values through its table.
#[test]
fn voi_lut_sequence() {
    let lut = [
        ELE.us(tags::LUT_DESCRIPTOR, &[4, 10, 8]),
        ELE.element(tags::LUT_DATA, Vr::OW, &[0, 0, 85, 0, 170, 0, 255, 0]),
    ]
    .concat();
    let extra = [ELE.sequence(tags::VOI_LUT_SEQUENCE, &[lut])];
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 1, 2, 8, false, &extra, &[11, 13]))).unwrap();
    let frame = d.decode_frame(0, None).unwrap();
    let voi = d.default_voi(0, &frame);
    assert!(matches!(voi, Voi::Lut(_)));
    assert!((voi.apply(11.0) - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(voi.apply(100.0), 1.0);
}

/// A Modality LUT Sequence replaces rescale.
#[test]
fn modality_lut() {
    let lut = [ELE.us(tags::LUT_DESCRIPTOR, &[3, 0, 16]), ELE.ow(tags::LUT_DATA, &[100, 200, 300])].concat();
    let extra = [ELE.sequence(tags::MODALITY_LUT_SEQUENCE, &[lut])];
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 1, 3, 8, false, &extra, &[0, 2, 9, 0])))
        .unwrap();
    assert_eq!(grey(&d, 0), vec![100.0, 300.0, 300.0]);
}

fn colour_image(photometric: &str, planar: u16, pixels: &[u8], cols: u16, extra: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut v = vec![
        ELE.us(tags::SAMPLES_PER_PIXEL, &[3]),
        ELE.text(tags::PHOTOMETRIC, Vr::CS, photometric),
        ELE.us(tags::PLANAR_CONFIGURATION, &[planar]),
        ELE.us(tags::ROWS, &[1]),
        ELE.us(tags::COLUMNS, &[cols]),
        ELE.us(tags::BITS_ALLOCATED, &[8]),
        ELE.us(tags::BITS_STORED, &[8]),
        ELE.us(tags::HIGH_BIT, &[7]),
        ELE.us(tags::PIXEL_REPRESENTATION, &[0]),
    ];
    v.extend(extra.iter().cloned());
    v.push(ELE.element(tags::PIXEL_DATA, Vr::OB, pixels));
    v
}

/// RGB interleaved and planar, YBR_FULL and YBR_FULL_422.
#[test]
fn colour() {
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &colour_image("RGB", 0, &[255, 0, 0, 0, 0, 255], 2, &[])))
        .unwrap();
    assert_eq!(rgb(&d, 0), vec![255, 0, 0, 0, 0, 255]);
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &colour_image("RGB", 1, &[255, 0, 0, 0, 0, 255], 2, &[])))
        .unwrap();
    assert_eq!(rgb(&d, 0), vec![255, 0, 0, 0, 0, 255]);
    // Pure red in full-range YCbCr is (76, 85, 255).
    let d = Dicom::from_bytes(part10(
        "1.2.840.10008.1.2.1",
        &colour_image("YBR_FULL", 0, &[76, 85, 255, 255, 128, 128], 2, &[]),
    ))
    .unwrap();
    let px = rgb(&d, 0);
    assert!(px[0] >= 253 && px[1] <= 2 && px[2] <= 2, "{px:?}");
    assert_eq!(&px[3..], &[255, 255, 255]);
    // 422: Y Y Cb Cr per pair; red then a darker red.
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &colour_image("YBR_FULL_422", 0, &[76, 38, 85, 255], 2, &[])))
            .unwrap();
    let px = rgb(&d, 0);
    assert!(px[0] >= 253 && px[1] <= 2, "{px:?}");
    assert!(px[3] > 150 && px[3] < 225 && px[4] == 0, "{px:?}");
}

/// PALETTE COLOR with 16-bit entries.
#[test]
fn palette_colour() {
    let extra = [
        ELE.us(tags::RED_PALETTE_DESCRIPTOR, &[3, 5, 16]),
        ELE.us(tags::GREEN_PALETTE_DESCRIPTOR, &[3, 5, 16]),
        ELE.us(tags::BLUE_PALETTE_DESCRIPTOR, &[3, 5, 16]),
        ELE.ow(tags::RED_PALETTE_DATA, &[0xFFFF, 0, 0]),
        ELE.ow(tags::GREEN_PALETTE_DATA, &[0, 0xFFFF, 0]),
        ELE.ow(tags::BLUE_PALETTE_DATA, &[0, 0, 0xFFFF]),
    ];
    let mut body = grey_image(ELE, 1, 4, 8, false, &extra, &[5, 6, 7, 200]);
    let p = body.iter().position(|e| e[..4] == [0x28, 0, 0x04, 0]).unwrap();
    body[p] = ELE.text(tags::PHOTOMETRIC, Vr::CS, "PALETTE COLOR");
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert_eq!(d.info.as_ref().unwrap().photometric, Photometric::Palette);
    assert_eq!(rgb(&d, 0), vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255]);
}

/// Native multi-frame data: each frame from its own offset, frame count
/// capped by the data, the middle frame in thumbnails.
#[test]
fn multi_frame() {
    let extra = [ELE.text(tags::NUMBER_OF_FRAMES, Vr::IS, "3")];
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 1, 2, 8, false, &extra, &[1, 2, 3, 4, 5, 6])))
            .unwrap();
    assert_eq!(d.frame_count(), 3);
    assert_eq!(grey(&d, 1), vec![3.0, 4.0]);
    assert_eq!(grey(&d, 2), vec![5.0, 6.0]);
    // An absurd frame count is capped by the data present.
    let extra = [ELE.text(tags::NUMBER_OF_FRAMES, Vr::IS, "4000000000")];
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 1, 2, 8, false, &extra, &[1, 2, 3, 4])))
        .unwrap();
    assert_eq!(d.frame_count(), 2);
    let pic = d.middle_picture(None).unwrap();
    assert_eq!(pic.width(), 2);
}

/// One-bit data (segmentations) unpacks least significant bit first,
/// including frames that start mid-byte.
#[test]
fn one_bit_frames() {
    let extra = [ELE.text(tags::NUMBER_OF_FRAMES, Vr::IS, "2")];
    let mut body = grey_image(ELE, 1, 3, 1, false, &extra, &[0b0010_1101, 0]);
    let p = body.iter().position(|e| e[..4] == [0x28, 0, 0x00, 0x01]).unwrap();
    body[p] = ELE.us(tags::BITS_ALLOCATED, &[1]);
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert_eq!(d.frame_count(), 2);
    assert_eq!(grey(&d, 0), vec![1.0, 0.0, 1.0]);
    assert_eq!(grey(&d, 1), vec![1.0, 0.0, 1.0]);
}

/// Pixel Spacing (row, column) stretches thumbnails to square pixels.
#[test]
fn pixel_aspect() {
    let extra = [ELE.text(tags::PIXEL_SPACING, Vr::DS, "1.0\\0.5")];
    let d =
        Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &grey_image(ELE, 2, 4, 8, false, &extra, &[0; 8]))).unwrap();
    assert_eq!(d.pixel_aspect(0), 2.0);
    let pic = d.middle_picture(None).unwrap();
    assert_eq!((pic.width(), pic.height()), (4, 4));
}

/// RLE Lossless, two frames of 16-bit samples.
#[test]
fn rle_frames() {
    let frame = |a: u8, b: u8| {
        let mut f = vec![0u8; 64];
        f[0] = 2;
        f[4] = 64;
        f[8] = 67;
        f.extend([1, 0, 1, 1, a, b]);
        f
    };
    let extra = [ELE.text(tags::NUMBER_OF_FRAMES, Vr::IS, "2")];
    let mut body = grey_image(ELE, 1, 2, 16, false, &extra, &[]);
    body.pop();
    body.push(ELE.encapsulated(&[frame(1, 2), frame(3, 4)]));
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.5", &body)).unwrap();
    assert_eq!(d.frame_count(), 2);
    assert_eq!(grey(&d, 0), vec![1.0, 258.0]);
    assert_eq!(grey(&d, 1), vec![3.0, 260.0]);
}

//endregion

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

/// A small, valid file to damage.
fn sample() -> Vec<u8> {
    let extra = [
        ELE.text(tags::NUMBER_OF_FRAMES, Vr::IS, "2"),
        ELE.sequence(tag(0x0008, 0x1140), &[ELE.text(tag(0x0008, 0x0100), Vr::SH, "A")]),
    ];
    let mut body = grey_image(ELE, 4, 4, 16, true, &extra, &[0x55; 64]);
    body.sort_by_key(|e| (u16::from_le_bytes([e[0], e[1]]), u16::from_le_bytes([e[2], e[3]])));
    part10("1.2.840.10008.1.2.1", &body)
}

fn try_everything(bytes: Vec<u8>) {
    if let Ok(d) = Dicom::from_bytes(bytes) {
        let _ = crate::text::rows(d.data());
        for i in 0..d.frame_count().min(4) {
            if let Ok(f) = d.decode_frame(i, None) {
                let voi = d.default_voi(i, &f);
                let _ = d.picture(&f, &voi, d.inverted(), d.pixel_aspect(i));
            }
        }
        let _ = d.facts();
    }
}

/// Every truncation of a file fails cleanly or reads what is there.
#[test]
fn truncated_files() {
    let file = sample();
    for len in 0..file.len() {
        try_everything(file[..len].to_vec());
    }
}

/// Mutated bytes, including lengths and frame counts made absurd, never
/// panic.
#[test]
fn mutated_files() {
    let file = sample();
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let mut f = file.clone();
        for _ in 0..1 + rand() % 4 {
            let i = (rand() as usize) % f.len();
            f[i] = match rand() % 4 {
                0 => 0xFF,
                1 => 0,
                2 => f[i] ^ (1 << (rand() % 8)),
                _ => rand() as u8,
            };
        }
        try_everything(f);
    }
    // Lengths of every element set to the largest values.
    for at in (132..file.len() - 4).step_by(2) {
        let mut f = file.clone();
        f[at..at + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0x7F]);
        try_everything(f);
    }
}

/// Millions of empty items, or sequences nested thousands deep, stop at
/// the caps instead of eating memory or the stack.
#[test]
fn sequence_bombs() {
    let mut wide = Vec::new();
    wide.extend(0x0008u16.to_le_bytes());
    wide.extend(0x1140u16.to_le_bytes());
    wide.extend(b"SQ\0\0");
    wide.extend(u32::MAX.to_le_bytes());
    for _ in 0..1_000_000 {
        wide.extend([0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0]);
    }
    let r = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &[ELE.text(tags::MODALITY, Vr::CS, "OT"), wide]));
    assert!(r.is_err());

    let mut deep = Vec::new();
    for _ in 0..10_000 {
        deep.extend(0x0008u16.to_le_bytes());
        deep.extend(0x1140u16.to_le_bytes());
        deep.extend(b"SQ\0\0");
        deep.extend(u32::MAX.to_le_bytes());
        deep.extend([0xFE, 0xFF, 0x00, 0xE0, 0xFF, 0xFF, 0xFF, 0xFF]);
    }
    // Reading stops at the nesting cap and keeps the header before it.
    let r = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &[ELE.text(tags::MODALITY, Vr::CS, "OT"), deep]));
    if let Ok(d) = r {
        assert!(!d.data().contains(tag(0x0008, 0x1140)));
        assert_eq!(d.modality(), "OT");
    }
}

/// Huge Rows and Columns with little data are refused, not allocated.
#[test]
fn absurd_dimensions() {
    let mut body = grey_image(ELE, 1, 1, 16, false, &[], &[0, 0]);
    let r = body.iter().position(|e| e[..4] == [0x28, 0, 0x10, 0]).unwrap();
    body[r] = ELE.us(tags::ROWS, &[65535]);
    body[r + 1] = ELE.us(tags::COLUMNS, &[65535]);
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.1", &body)).unwrap();
    assert!(d.decode_frame(0, None).is_err());
    // Compressed: the codec's own header claims a huge size.
    let mut body = grey_image(ELE, 1, 1, 8, false, &[], &[]);
    body.pop();
    let jls = [0xFF, 0xD8, 0xFF, 0xF7, 0, 11, 8, 0xFF, 0xFF, 0xFF, 0xFF, 1, 1, 0x11, 0, 0xFF, 0xD9];
    body.push(ELE.encapsulated(&[jls.to_vec()]));
    let d = Dicom::from_bytes(part10("1.2.840.10008.1.2.4.80", &body)).unwrap();
    assert!(d.decode_frame(0, None).is_err());
}

/// Values in the inspector: binary and deferred values are summarised,
/// person names flagged.
#[test]
fn inspector_rows() {
    let d = Dicom::from_bytes(sample()).unwrap();
    let rows = crate::text::rows(d.data());
    let name = rows.iter().find(|r| r.tag == tags::PATIENT_NAME).unwrap();
    assert!(name.identifying);
    assert_eq!(name.value, "Doe^Jane");
    let pixels = rows.iter().find(|r| r.tag == tags::PIXEL_DATA).unwrap();
    assert_eq!(pixels.value, "64 bytes");
    assert!(!rows.iter().find(|r| r.tag == tags::ROWS).unwrap().identifying);
    assert!(matches!(d.data().get(tags::PIXEL_DATA).unwrap().value, Value::Deferred(_)));
}

//endregion

/*┌─────────────────╮
| JPEG cross-check|
└─────────────────╯*/
//region JPEG cross-check

/// The 12-bit JPEG decoder is precision-agnostic, so on 8-bit baseline
/// files it must agree with `jpeg-decoder`: within rounding where nothing
/// is subsampled, and closely where chroma is (the two upsample chroma
/// differently).  Skips files that aren't downloaded.
#[test]
fn own_jpeg_decoder_agrees_with_jpeg_decoder() {
    use crate::codec::{Samples, jpeg};
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/dicom");
    for (file, max_mean) in [
        ("pydicom-test-files/SC_rgb_dcmtk_+eb+cy+s4.dcm", 0.05),
        ("pydicom-test-files/SC_rgb_dcmtk_+eb+cr.dcm", 0.05),
        ("pydicom-test-files/SC_rgb_jpeg_dcmtk.dcm", 0.05),
        ("pydicom-test-files/SC_rgb_jpeg_app14_dcmd.dcm", 0.05),
        ("pydicom-test-files/SC_rgb_dcmtk_+eb+cy+n2.dcm", 3.0),
        ("pydicom-test-files/examples_ybr_color.dcm", 3.0),
        ("pydicom-data/color3d_jpeg_baseline.dcm", 3.0),
    ] {
        let path = base.join(file);
        if !path.exists() {
            eprintln!("skipping {file}");
            continue;
        }
        let d = Dicom::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        let bytes = d.parsed.source.read_joined(&d.frames[0], 1 << 30).unwrap();
        let photometric = &d.info.as_ref().unwrap().photometric;
        let h = jpeg::header(&bytes).unwrap();
        let ours = jpeg::decode12(&bytes, &h, photometric, None).unwrap();
        let theirs = jpeg::with_jpeg_decoder(&bytes, &h, photometric, None).unwrap();
        assert_eq!((ours.width, ours.height, ours.comps), (theirs.width, theirs.height, theirs.comps), "{file}");
        let (Samples::U16(a), Samples::U8(b)) = (&ours.data, &theirs.data) else { panic!("{file}: sample types") };
        let total: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).abs()).sum();
        let signed: f64 = a.iter().zip(b).map(|(&x, &y)| x as f64 - y as f64).sum::<f64>() / a.len() as f64;
        let max = a.iter().zip(b).map(|(&x, &y)| (x as i32 - y as i32).abs()).max().unwrap_or(0);
        let first = a.iter().zip(b).position(|(&x, &y)| x as i32 != y as i32);
        let mean = total / a.len() as f64;
        eprintln!(
            "{file}: mean difference {mean:.3}, signed {signed:.3}, max {max}, first at {first:?} of {}",
            a.len()
        );
        assert!(mean <= max_mean, "{file}: mean difference {mean}");
    }
}

//endregion
