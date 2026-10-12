//! Parser tests that need no sample files: tiny documents built here for
//! each format, so every reader is checked wherever the crate is built
//! (the tests in `tests.rs` use real files and skip when they're missing).
//! The SQLite reader gets hand-built databases, including ones with
//! overflow pages, interior pages and a table that points back at itself.

use super::*;
use crate::sqlite::{Db, Value};
use crate::zip::tests::stored_zip;
use base64::Engine;
use slerp_scene::textures::Pixels;

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

/// An RGBA PNG of one colour, or of noise that doesn't compress when
/// `noisy`, for pictures that must be bigger than a database page.
fn png(w: u32, h: u32, color: [u8; 4], noisy: bool) -> Vec<u8> {
    let mut seed = 0x1234_5678u32;
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        if noisy {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            pixels.extend_from_slice(&[(seed >> 8) as u8, (seed >> 16) as u8, color[2], 255]);
        } else {
            pixels.extend_from_slice(&color);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("header").write_image_data(&pixels).expect("pixels");
    out
}

const RED: [u8; 4] = [200, 20, 10, 255];
const BLUE: [u8; 4] = [10, 20, 200, 255];

fn load(format: PreviewFormat, bytes: Vec<u8>) -> Result<Texture, LoadError> {
    load_source(format, &mut Source::memory(bytes), &Limits::viewer())
}

/// The picture a file shows: its size and first texel.
fn shown(format: PreviewFormat, bytes: Vec<u8>) -> ((u32, u32), [u8; 4]) {
    let t = load(format, bytes).unwrap_or_else(|e| panic!("{format:?}: {e}"));
    let Pixels::Rgba8(px) = t.base() else { panic!("{format:?}: not 8-bit") };
    ((t.width(), t.height()), [px[0], px[1], px[2], px[3]])
}

fn assert_no_preview(format: PreviewFormat, bytes: Vec<u8>) {
    match load(format, bytes) {
        Err(LoadError::Unsupported(_)) => {}
        other => panic!("{format:?}: expected Unsupported, got {:?}", other.map(|_| ())),
    }
}

fn sniffed(name: &str, bytes: &[u8]) -> Option<PreviewFormat> {
    sniff(Path::new(name), &bytes[..bytes.len().min(4096)])
}

//endregion

/*┌─────────────────╮
| CorelDRAW, Visio |
└─────────────────╯*/
//region CorelDRAW and Visio

/// A 2x2 DIB with a 256-entry colour table: blue (0) and red (1).
fn small_dib() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&2i32.to_le_bytes());
    d.extend_from_slice(&2i32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&8u16.to_le_bytes());
    // Compression, image size, resolutions, then colours used and important.
    d.extend_from_slice(&[0; 16]);
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    let mut table = vec![0u8; 1024];
    table[0..4].copy_from_slice(&[200, 20, 10, 0]);
    table[4..8].copy_from_slice(&[10, 20, 200, 0]);
    d.extend_from_slice(&table);
    // Bottom row first, rows padded to four bytes.
    d.extend_from_slice(&[0, 1, 0, 0, 1, 0, 0, 0]);
    d
}

fn bmp_of(dib: &[u8]) -> Vec<u8> {
    let mut b = b"BM".to_vec();
    b.extend_from_slice(&(14 + dib.len() as u32).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&(14 + 40 + 1024u32).to_le_bytes());
    b.extend_from_slice(dib);
    b
}

fn riff_cdr(form: &[u8; 4], disp_first: bool) -> Vec<u8> {
    let mut disp = Vec::new();
    disp.extend_from_slice(&8u32.to_le_bytes());
    disp.extend_from_slice(&small_dib());
    let mut body = form.to_vec();
    let chunk = |id: &[u8; 4], data: &[u8]| {
        let mut c = id.to_vec();
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    if disp_first {
        body.extend(chunk(b"DISP", &disp));
        body.extend(chunk(b"vrsn", &[0xB0, 0x04]));
    } else {
        body.extend(chunk(b"vrsn", &[0xB0, 0x04]));
        body.extend(chunk(b"DISP", &disp));
    }
    body.extend(chunk(b"LIST", b"INFO"));
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

#[test]
fn coreldraw_riff_files_carry_a_dib_in_the_disp_chunk() {
    for form in [b"CDR7", b"CDR9", b"CDRC", b"CDRD", b"CMX1"] {
        for disp_first in [false, true] {
            let file = riff_cdr(form, disp_first);
            assert_eq!(sniffed("a.cdr", &file), Some(PreviewFormat::CorelDraw), "{form:?}");
            // Index 0 is blue and index 1 red in the colour table; the DIB's
            // bottom row comes first, so the top left is index 1.
            let ((w, h), first) = shown(PreviewFormat::CorelDraw, file);
            assert_eq!((w, h), (2, 2), "{form:?}");
            assert_eq!(first, RED, "{form:?}");
        }
    }
    // Other RIFF files aren't CorelDRAW's.
    let mut wav = riff_cdr(b"WAVE", false);
    wav[8..12].copy_from_slice(b"WAVE");
    assert_eq!(sniffed("a.wav", &wav), None);
    // A RIFF without a DISP chunk has no picture.
    let mut bare = b"RIFF".to_vec();
    bare.extend_from_slice(&4u32.to_le_bytes());
    bare.extend_from_slice(b"CDRC");
    assert_no_preview(PreviewFormat::CorelDraw, bare);
}

#[test]
fn coreldraw_zips_hold_a_bmp_or_a_png() {
    let bmp = bmp_of(&small_dib());
    let x4 = stored_zip(&[("content/riffData.cdr", b"RIFF"), ("metadata/thumbnails/thumbnail.bmp", &bmp)]);
    assert_eq!(sniffed("a.cdr", &x4), Some(PreviewFormat::CorelDraw));
    assert_eq!(sniffed("a.cdt", &x4), Some(PreviewFormat::CorelDraw));
    assert_eq!(shown(PreviewFormat::CorelDraw, x4).0, (2, 2));
    let x7 = stored_zip(&[
        ("mimetype", b"application/x-vnd.corel.zcf.draw.document+zip"),
        ("previews/thumbnail.png", &png(7, 4, RED, false)),
    ]);
    assert_eq!(shown(PreviewFormat::CorelDraw, x7), ((7, 4), RED));
    assert_eq!(sniffed("a.zip", &stored_zip(&[("a", b"b")])), None);
    assert_no_preview(PreviewFormat::CorelDraw, stored_zip(&[("content/riffData.cdr", b"RIFF")]));
}

#[test]
fn visio_zips_give_their_metafile_or_bitmap() {
    let emf = b"\x01\0\0\0\x6c\0\0\0 EMF fake".to_vec();
    let vsdx = stored_zip(&[("visio/document.xml", b"<x/>"), ("docProps/thumbnail.emf", &emf)]);
    for name in ["a.vsdx", "a.vsdm", "a.vstx", "a.vssx"] {
        assert_eq!(sniffed(name, &vsdx), Some(PreviewFormat::Visio), "{name}");
    }
    let stored = read_stored(PreviewFormat::Visio, &mut Source::memory(vsdx.clone()), &Limits::viewer()).unwrap();
    assert!(matches!(stored, Stored::Metafile(ref b) if *b == emf));
    // `load_source` is for pictures only.
    assert!(matches!(load(PreviewFormat::Visio, vsdx), Err(LoadError::Unsupported(_))));
    // A PNG thumbnail is decoded here and wins over a metafile.
    let both = stored_zip(&[("docProps/thumbnail.emf", &emf), ("docProps/thumbnail.png", &png(3, 3, BLUE, false))]);
    assert_eq!(shown(PreviewFormat::Visio, both), ((3, 3), BLUE));
    assert_no_preview(PreviewFormat::Visio, stored_zip(&[("visio/document.xml", b"<x/>")]));
}

/// A property-set stream whose thumbnail property holds clipboard data.
fn property_set(cf: u32, data: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; 48];
    s[0] = 0xFE;
    s[1] = 0xFF;
    s[24] = 1;
    s[44] = 48;
    let mut set = Vec::new();
    set.extend_from_slice(&0u32.to_le_bytes());
    set.extend_from_slice(&1u32.to_le_bytes());
    set.extend_from_slice(&17u32.to_le_bytes());
    set.extend_from_slice(&16u32.to_le_bytes());
    set.extend_from_slice(&71u32.to_le_bytes());
    set.extend_from_slice(&((8 + data.len()) as u32).to_le_bytes());
    set.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    set.extend_from_slice(&cf.to_le_bytes());
    set.extend_from_slice(data);
    s.extend(set);
    s
}

#[test]
fn summary_information_thumbnails_are_clipboard_data() {
    let stream = property_set(14, b"EMFDATA");
    let clip = crate::summary::thumbnail_property(&stream).expect("the thumbnail property");
    let (format, data) = crate::summary::windows_format(clip).unwrap();
    assert_eq!((format, data), (14, &b"EMFDATA"[..]));
    // Truncated or foreign streams give nothing, and never panic.
    for cut in 0..stream.len() {
        let _ = crate::summary::thumbnail_property(&stream[..cut]);
    }
    assert!(crate::summary::thumbnail_property(&[0u8; 100]).is_none());
    assert!(crate::summary::windows_format(&[1, 2, 3, 4, 5, 6, 7, 8]).is_none());
}

//endregion

/*┌─────────────────╮
| Zips            |
└─────────────────╯*/
//region Zips

#[test]
fn zipped_formats_read_their_preview_member() {
    let cases = [
        (PreviewFormat::Procreate, "a.procreate", "QuickLook/Thumbnail.png"),
        (PreviewFormat::Sketch, "a.sketch", "previews/preview.png"),
        (PreviewFormat::Figma, "a.fig", "thumbnail.png"),
        (PreviewFormat::AdobeXd, "a.xd", "thumbnail.png"),
        (PreviewFormat::FreeCad, "a.FCStd", "thumbnails/Thumbnail.png"),
    ];
    for (format, file, member) in cases {
        // The preview sits among other members, as in real files.
        let zip = stored_zip(&[("document.json", b"{}"), (member, &png(5, 3, RED, false)), ("z.bin", &[1, 2, 3])]);
        assert_eq!(sniffed(file, &zip), Some(format), "{file}");
        assert_eq!(shown(format, zip.clone()), ((5, 3), RED), "{file}");
        // The extension decides among zips: any other name isn't claimed.
        assert_eq!(sniffed("a.zip", &zip), None);
        // Without the member there is no preview.
        assert_no_preview(format, stored_zip(&[("document.json", b"{}")]));
    }
}

/// Adobe XD keeps a 512-pixel `thumbnail.png` and a larger `preview.png`:
/// thumbnails take the small one, the viewer the large one.
#[test]
fn xd_picks_the_member_by_the_size_wanted() {
    let zip = stored_zip(&[
        ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
        ("preview.png", &png(40, 20, RED, false)),
        ("thumbnail.png", &png(8, 4, BLUE, false)),
    ]);
    let wanted = |limits: &Limits| {
        let t = load_source(PreviewFormat::AdobeXd, &mut Source::memory(zip.clone()), limits).unwrap();
        (t.width(), t.height())
    };
    assert_eq!(wanted(&Limits::thumbnail()), (8, 4));
    assert_eq!(wanted(&Limits::viewer()), (40, 20));
}

#[test]
fn sketchup_2021_zips_follow_the_header() {
    let magic = b"\xFF\xFE\xFF\x0ES\0k\0e\0t\0c\0h\0U\0p\0 \0M\0o\0d\0e\0l\0";
    let mut file = magic.to_vec();
    file.extend_from_slice(b"{\0\x32\0\x31\0}\0VFF\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09");
    file.extend(stored_zip(&[("meta/model_thumbnail.png", &png(6, 6, BLUE, false)), ("model", &[0; 50])]));
    assert_eq!(sniffed("a.skp", &file), Some(PreviewFormat::SketchUp));
    assert_eq!(shown(PreviewFormat::SketchUp, file), ((6, 6), BLUE));
}

#[test]
fn sketchup_before_2021_stores_a_png_near_the_start() {
    let magic = b"\xFF\xFE\xFF\x0ES\0k\0e\0t\0c\0h\0U\0p\0 \0M\0o\0d\0e\0l\0";
    let mut file = magic.to_vec();
    file.extend_from_slice(&[7u8; 300]);
    file.extend(png(4, 9, RED, false));
    file.extend_from_slice(&[9u8; 300]);
    assert_eq!(shown(PreviewFormat::SketchUp, file), ((4, 9), RED));
}

//endregion

/*┌─────────────────╮
| Affinity, Figma |
└─────────────────╯*/
//region Affinity, Figma

/// An Affinity document: a header, some body, then a trailer of records.
fn affinity(records: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut b = b"\x00\xFFKA".to_vec();
    b.extend_from_slice(&[11, 0, 0, 0]);
    b.extend_from_slice(b"nsrP#Inf");
    b.resize(0x40, 0);
    b.extend_from_slice(&[0xAA; 100]);
    let trailer = b.len() as u64;
    b[0x18..0x20].copy_from_slice(&trailer.to_le_bytes());
    for (tag, data) in records {
        b.extend_from_slice(&[0xFF; 4]);
        b.extend_from_slice(*tag);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(data.len() as u32 + 13).to_le_bytes());
        b.extend_from_slice(&29u64.to_le_bytes());
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.push(1);
        b.extend_from_slice(data);
    }
    b
}

#[test]
fn affinity_trailer_records() {
    let file = affinity(&[(b"Thmb", png(7, 5, RED, false)), (b"Meta", br#"{"clientVersion":"3.0"}"#.to_vec())]);
    assert_eq!(sniffed("a.afphoto", &file), Some(PreviewFormat::Affinity));
    assert_eq!(sniffed("a.af", &file), Some(PreviewFormat::Affinity));
    assert_eq!(shown(PreviewFormat::Affinity, file), ((7, 5), RED));
    // Another record first: the trailer is walked.
    let later = affinity(&[(b"Meta", vec![b'x'; 300]), (b"Thmb", png(2, 2, BLUE, false))]);
    assert_eq!(shown(PreviewFormat::Affinity, later), ((2, 2), BLUE));
    assert_no_preview(PreviewFormat::Affinity, affinity(&[(b"Meta", b"{}".to_vec())]));
}

fn chunk(data: &[u8]) -> Vec<u8> {
    [&(data.len() as u32).to_le_bytes()[..], data].concat()
}

#[test]
fn bare_figma_files_end_in_a_png_chunk() {
    let mut file = b"fig-kiwi\x0f\x00\x00\x00".to_vec();
    file.extend(chunk(&[0x11; 200]));
    file.extend(chunk(&[0x22; 500]));
    let without = file.clone();
    file.extend(chunk(&png(9, 4, BLUE, false)));
    // Recognised by content, whatever the file is called.
    assert_eq!(sniffed("design.fig", &file), Some(PreviewFormat::Figma));
    assert_eq!(sniffed("design", &file), Some(PreviewFormat::Figma));
    assert_eq!(shown(PreviewFormat::Figma, file), ((9, 4), BLUE));
    // A `canvas.fig` taken out of a zip has no thumbnail chunk.
    assert_no_preview(PreviewFormat::Figma, without);
}

//endregion

/*┌─────────────────╮
| Paint.NET       |
└─────────────────╯*/
//region Paint.NET

#[test]
fn paintnet_thumbnail_in_the_xml_header() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png(3, 2, RED, false));
    let xml = format!(r#"<pdnImage width="3" height="2" layers="1"><custom><thumb png="{b64}" /></custom></pdnImage>"#);
    let mut file = b"PDN3".to_vec();
    file.extend_from_slice(&(xml.len() as u32).to_le_bytes()[..3]);
    file.extend_from_slice(xml.as_bytes());
    file.extend_from_slice(&[0u8; 64]);
    assert_eq!(sniffed("a.pdn", &file), Some(PreviewFormat::PaintNet));
    assert_eq!(shown(PreviewFormat::PaintNet, file.clone()), ((3, 2), RED));
    // A header without a thumbnail, and one with bad base64.
    let bare = |xml: &str| {
        let mut f = b"PDN3".to_vec();
        f.extend_from_slice(&(xml.len() as u32).to_le_bytes()[..3]);
        f.extend_from_slice(xml.as_bytes());
        f
    };
    assert_no_preview(PreviewFormat::PaintNet, bare("<pdnImage><custom/></pdnImage>"));
    assert!(load(PreviewFormat::PaintNet, bare(r#"<pdnImage><thumb png="not base64!!"/></pdnImage>"#)).is_err());
}

//endregion

/*┌─────────────────╮
| InDesign        |
└─────────────────╯*/
//region InDesign

const MASTER: &[u8] = b"\x06\x06\xED\xF5\xD8\x1D\x46\xE5\xBD\x31\xEF\xE7\xFE\x74\xB7\x1D";
const OBJECT: &[u8] = b"\xDE\x39\x39\x79\x51\x88\x4B\x6C\x8E\x63\xEE\xF8\xAE\xE0\xDD\x38";

fn xmp(png: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    // XMP writers break the base64 into lines escaped as `&#xA;`.
    let broken: Vec<&str> = b64.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect();
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta><rdf:li><xmpGImg:format>PNG</xmpGImg:format><xmpGImg:image>{}</xmpGImg:image></rdf:li></x:xmpmeta><?xpacket end="w"?>"#,
        broken.join("&#xA;")
    )
    .into_bytes()
}

/// An InDesign document: two master pages (the second newer), database
/// pages holding a stale XMP packet, and then contiguous objects, the XMP
/// packet being the second.
fn indd(stale: &[u8], current: &[u8]) -> Vec<u8> {
    const PAGE: usize = 4096;
    let mut b = vec![0u8; 4 * PAGE];
    for page in [0, 1] {
        b[page * PAGE..page * PAGE + 16].copy_from_slice(MASTER);
        b[page * PAGE + 16..page * PAGE + 24].copy_from_slice(b"DOCUMENT");
    }
    // The older master page claims two pages, the newer four.
    b[264..272].copy_from_slice(&1u64.to_le_bytes());
    b[280..284].copy_from_slice(&2u32.to_le_bytes());
    b[PAGE + 264..PAGE + 272].copy_from_slice(&2u64.to_le_bytes());
    b[PAGE + 280..PAGE + 284].copy_from_slice(&4u32.to_le_bytes());
    b[2 * PAGE + 100..2 * PAGE + 100 + stale.len()].copy_from_slice(stale);
    let object = |data: &[u8]| {
        let mut o = OBJECT.to_vec();
        o.resize(24, 0);
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o.resize(32, 0);
        o.extend_from_slice(data);
        o.extend_from_slice(&[0u8; 32]);
        o
    };
    b.extend(object(&[0x55; 100]));
    b.extend(object(&[&(current.len() as u32).to_le_bytes()[..], current].concat()));
    b
}

#[test]
fn indd_finds_the_current_xmp_packet_not_a_stale_one() {
    let file = indd(&xmp(&png(5, 5, RED, false)), &xmp(&png(8, 6, BLUE, true)));
    assert_eq!(sniffed("a.indd", &file), Some(PreviewFormat::InDesign));
    let ((w, h), px) = shown(PreviewFormat::InDesign, file);
    assert_eq!((w, h, px[2]), (8, 6, 200), "the packet after the database pages, not the stale red one");
    // An XMP packet without a thumbnail.
    let bare = indd(b"", br#"<?xpacket begin=""?><x:xmpmeta/><?xpacket end="w"?>"#);
    assert_no_preview(PreviewFormat::InDesign, bare);
}

//endregion

/*┌─────────────────╮
| SQLite          |
└─────────────────╯*/
//region SQLite

const PAGE: usize = 512;

enum Val<'a> {
    Null,
    Int(i64),
    Text(&'a str),
    Blob(&'a [u8]),
}

/// A variable-length integer (values below 2^56).
fn varint(v: u64) -> Vec<u8> {
    let mut groups = vec![(v & 0x7F) as u8];
    let mut rest = v >> 7;
    while rest > 0 {
        groups.push((rest & 0x7F) as u8 | 0x80);
        rest >>= 7;
    }
    groups.reverse();
    groups
}

fn record(vals: &[Val]) -> Vec<u8> {
    let (mut types, mut body) = (Vec::new(), Vec::new());
    for v in vals {
        match v {
            Val::Null => types.push(0),
            Val::Int(i) if (-128..128).contains(i) => {
                types.push(1);
                body.push(*i as i8 as u8);
            }
            Val::Int(i) => {
                types.push(4);
                body.extend_from_slice(&(*i as i32).to_be_bytes());
            }
            Val::Text(s) => {
                types.extend(varint(13 + 2 * s.len() as u64));
                body.extend_from_slice(s.as_bytes());
            }
            Val::Blob(b) => {
                types.extend(varint(12 + 2 * b.len() as u64));
                body.extend_from_slice(b);
            }
        }
    }
    let mut out = varint(types.len() as u64 + 1);
    out.extend(types);
    out.extend(body);
    out
}

/// A table-leaf cell whose payload may spill onto overflow pages.  They
/// are appended to `pages`, which holds the database's pages from number 2
/// on (page 1 is the schema), so a new page's number is its index plus 2.
fn cell(rowid: u64, payload: &[u8], pages: &mut Vec<Vec<u8>>) -> Vec<u8> {
    let size = payload.len();
    let (usable, x) = (PAGE, PAGE - 35);
    let local = if size <= x {
        size
    } else {
        let m = (usable - 12) * 32 / 255 - 23;
        let k = m + (size - m) % (usable - 4);
        if k <= x { k } else { m }
    };
    let mut out = varint(size as u64);
    out.extend(varint(rowid));
    out.extend_from_slice(&payload[..local]);
    if local < size {
        let chunks: Vec<&[u8]> = payload[local..].chunks(usable - 4).collect();
        // The first overflow page is the next one to be added.
        let first = pages.len() as u32 + 2;
        out.extend_from_slice(&first.to_be_bytes());
        for (i, c) in chunks.iter().enumerate() {
            let next = if i + 1 < chunks.len() { first + i as u32 + 1 } else { 0 };
            let mut page = next.to_be_bytes().to_vec();
            page.extend_from_slice(c);
            page.resize(PAGE, 0);
            pages.push(page);
        }
    }
    out
}

/// A table b-tree page: leaf cells, or an interior page's (child, key)
/// cells and right-most child.  Page 1 keeps its header in the first 100
/// bytes.
fn leaf(first_page: bool, cells: &[Vec<u8>]) -> Vec<u8> {
    let h = if first_page { 100 } else { 0 };
    let mut page = vec![0u8; PAGE];
    let mut at = PAGE;
    page[h] = 0x0D;
    page[h + 3..h + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    for (i, c) in cells.iter().enumerate() {
        at -= c.len();
        page[at..at + c.len()].copy_from_slice(c);
        page[h + 8 + i * 2..h + 10 + i * 2].copy_from_slice(&(at as u16).to_be_bytes());
    }
    page[h + 5..h + 7].copy_from_slice(&(at as u16).to_be_bytes());
    page
}

fn interior(children: &[(u32, u64)], right: u32) -> Vec<u8> {
    let mut page = vec![0u8; PAGE];
    let mut at = PAGE;
    page[0] = 0x05;
    page[3..5].copy_from_slice(&(children.len() as u16).to_be_bytes());
    page[8..12].copy_from_slice(&right.to_be_bytes());
    for (i, (child, key)) in children.iter().enumerate() {
        let mut c = child.to_be_bytes().to_vec();
        c.extend(varint(*key));
        at -= c.len();
        page[at..at + c.len()].copy_from_slice(&c);
        page[12 + i * 2..14 + i * 2].copy_from_slice(&(at as u16).to_be_bytes());
    }
    page[5..7].copy_from_slice(&(at as u16).to_be_bytes());
    page
}

/// Page 1 of a database: the header and a schema table holding `tables`
/// as (name, root page, SQL).
fn schema(tables: &[(&str, u32, &str)]) -> Vec<u8> {
    let mut none = Vec::new();
    let cells: Vec<Vec<u8>> = tables
        .iter()
        .enumerate()
        .map(|(i, (name, root, sql))| {
            let r =
                record(&[Val::Text("table"), Val::Text(name), Val::Text(name), Val::Int(*root as i64), Val::Text(sql)]);
            cell(i as u64 + 1, &r, &mut none)
        })
        .collect();
    let mut page = leaf(true, &cells);
    page[..16].copy_from_slice(b"SQLite format 3\0");
    page[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
    page
}

const PREVIEW_SQL: &str =
    "CREATE TABLE CanvasPreview(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, CanvasId INTEGER DEFAULT(0), ImageData BLOB)";

/// A database whose `CanvasPreview` table has one row holding `blob`,
/// spilling onto overflow pages when it is big, with `stale` (an older
/// preview) in a free page nothing refers to.
fn preview_db(blob: &[u8], stale: &[u8]) -> Vec<u8> {
    let mut pages = vec![Vec::new(); 1];
    let row = cell(1, &record(&[Val::Null, Val::Int(1), Val::Blob(blob)]), &mut pages);
    // `pages` held a placeholder for page 2, the table's leaf, so the
    // overflow pages were numbered from 3.
    pages[0] = leaf(false, &[row]);
    let mut free = stale.to_vec();
    free.resize(free.len().div_ceil(PAGE) * PAGE, 0);
    let mut db = schema(&[("CanvasPreview", 2, PREVIEW_SQL)]);
    for p in &pages {
        db.extend_from_slice(p);
    }
    db.extend(free);
    db
}

/// A `.clip` file: the header, a head chunk (which names where the
/// database chunk is), a layer chunk, the database and a footer.
fn clip(db: &[u8], head_points_right: bool) -> Vec<u8> {
    let chunk = |tag: &[u8; 8], body: &[u8]| [&tag[..], &(body.len() as u64).to_be_bytes(), body].concat();
    let mut b = b"CSFCHUNK".to_vec();
    b.extend_from_slice(&0u64.to_be_bytes());
    b.extend_from_slice(&24u64.to_be_bytes());
    let layers = chunk(b"CHNKExta", &[0x77; 1000]);
    let head_len = 16 + 40;
    let sqli_at = 24 + head_len + layers.len();
    let mut head = vec![0u8; 40];
    if head_points_right {
        head[8..16].copy_from_slice(&(sqli_at as u64).to_be_bytes());
    }
    b.extend(chunk(b"CHNKHead", &head));
    b.extend(layers);
    b.extend(chunk(b"CHNKSQLi", db));
    b.extend(chunk(b"CHNKFoot", &[]));
    let len = b.len() as u64;
    b[8..16].copy_from_slice(&len.to_be_bytes());
    b
}

#[test]
fn clip_studio_reads_the_preview_from_its_database() {
    // A picture of 64 x 64 noise takes far more than a page, so the row's
    // blob lies on a chain of overflow pages.
    let real = png(64, 64, BLUE, true);
    assert!(real.len() > 4 * PAGE, "{}", real.len());
    let stale = png(16, 16, RED, true);
    // The stale PNG sits in a free page after the live ones, where a
    // search for a PNG signature would find it first.
    let db = preview_db(&real, &stale);
    for head_points_right in [true, false] {
        let file = clip(&db, head_points_right);
        assert_eq!(sniffed("a.clip", &file), Some(PreviewFormat::ClipStudio));
        let ((w, h), px) = shown(PreviewFormat::ClipStudio, file);
        assert_eq!((w, h, px[2]), (64, 64, 200), "head_points_right: {head_points_right}");
    }
    // A small preview fits in the page.
    let small = clip(&preview_db(&png(2, 2, RED, false), &[]), true);
    assert_eq!(shown(PreviewFormat::ClipStudio, small), ((2, 2), RED));
    // A database without the table, and an empty table.
    let other = {
        let mut db = schema(&[("Other", 2, "CREATE TABLE Other(a)")]);
        db.extend(leaf(false, &[]));
        db
    };
    assert_no_preview(PreviewFormat::ClipStudio, clip(&other, true));
    let empty = {
        let mut db = schema(&[("CanvasPreview", 2, PREVIEW_SQL)]);
        db.extend(leaf(false, &[]));
        db
    };
    assert_no_preview(PreviewFormat::ClipStudio, clip(&empty, true));
}

#[test]
fn sqlite_interior_pages_are_walked_in_order() {
    // Table t: root page 4 (interior) over leaves 2 (rows 1, 2) and 3
    // (rows 3, 4).
    let mut none = Vec::new();
    let mut row = |id: u64| cell(id, &record(&[Val::Int(id as i64), Val::Text(&format!("row {id}"))]), &mut none);
    let mut db = schema(&[("t", 4, "CREATE TABLE t(id, name)")]);
    db.extend(leaf(false, &[row(1), row(2)]));
    db.extend(leaf(false, &[row(3), row(4)]));
    db.extend(interior(&[(2, 2)], 3));
    let len = db.len() as u64;
    let mut src = Source::memory(db);
    let mut db = Db::open(&mut src, 0, len).unwrap();
    let (root, columns) = db.table("t").unwrap().unwrap();
    assert_eq!((root, columns), (4, vec!["id".to_string(), "name".to_string()]));
    let first = db.find_row(root, |_| true).unwrap().unwrap();
    assert_eq!(first[0], Value::Int(1));
    let fourth = db.find_row(root, |v| v[0] == Value::Int(4)).unwrap().unwrap();
    assert_eq!(fourth[1], Value::Text("row 4".into()));
    assert_eq!(db.find_row(root, |v| v[0] == Value::Int(9)).unwrap(), None);
    assert!(db.table("missing").unwrap().is_none());
}

/// Pages that point back at themselves end the walk instead of looping,
/// and numbers past the end of the file are errors.
#[test]
fn sqlite_damaged_trees_fail_cleanly() {
    let mut db = schema(&[("loop", 2, "CREATE TABLE loop(a)"), ("far", 3, "CREATE TABLE far(a)")]);
    db.extend(interior(&[(2, 1)], 2));
    db.extend(interior(&[(99, 1)], 98));
    let len = db.len() as u64;
    let mut src = Source::memory(db);
    let mut db = Db::open(&mut src, 0, len).unwrap();
    let start = std::time::Instant::now();
    assert_eq!(db.find_row(2, |_| false).unwrap(), None);
    assert!(db.find_row(3, |_| false).is_err());
    assert!(db.find_row(0, |_| false).is_err());
    assert!(start.elapsed().as_millis() < 500);
    // Page sizes that aren't a power of two, or too small a usable size.
    for (size, reserved) in [(1000u16, 0u8), (100, 0), (512, 100)] {
        let mut bad = vec![0u8; 1024];
        bad[..16].copy_from_slice(b"SQLite format 3\0");
        bad[16..18].copy_from_slice(&size.to_be_bytes());
        bad[20] = reserved;
        assert!(Db::open(&mut Source::memory(bad), 0, 1024).is_err(), "{size} {reserved}");
    }
}

/// A record claiming a 2 GB value, and a huge overflow chain, are refused
/// before anything that size is allocated.
#[test]
fn sqlite_huge_values_are_refused() {
    let mut db = schema(&[("t", 2, "CREATE TABLE t(a)")]);
    // 39 + 508 * 3_937_007 + 1 bytes: SQLite keeps 40 of them on the page
    // (see `cell`), as many as the cell below holds, and the rest on a
    // chain of pages that doesn't exist.
    let mut c = varint(39 + 508 * 3_937_007 + 1);
    c.extend(varint(1));
    c.extend_from_slice(&[0x0D; 40]);
    c.extend_from_slice(&3u32.to_be_bytes());
    db.extend(leaf(false, &[c]));
    let len = db.len() as u64;
    let mut src = Source::memory(db);
    let mut db = Db::open(&mut src, 0, len).unwrap();
    assert!(matches!(db.find_row(2, |_| true), Err(LoadError::TooLarge(_))));
}

//endregion

/*┌─────────────────╮
| Damage          |
└─────────────────╯*/
//region Damage

/// Every document built above, truncated at each byte and then with
/// bytes overwritten: errors or pictures, never panics or hangs.
#[test]
fn damaged_synthetic_documents_fail_cleanly() {
    let magic = b"\xFF\xFE\xFF\x0ES\0k\0e\0t\0c\0h\0U\0p\0 \0M\0o\0d\0e\0l\0";
    let zip = |member: &str| stored_zip(&[("a", b"x"), (member, &png(5, 3, RED, false))]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(png(3, 2, RED, false));
    let pdn_xml = format!(r#"<pdnImage><thumb png="{b64}" /></pdnImage>"#);
    let mut pdn = b"PDN3".to_vec();
    pdn.extend_from_slice(&(pdn_xml.len() as u32).to_le_bytes()[..3]);
    pdn.extend_from_slice(pdn_xml.as_bytes());
    let mut kiwi = b"fig-kiwi\x0f\x00\x00\x00".to_vec();
    kiwi.extend(chunk(&[0x11; 40]));
    kiwi.extend(chunk(&png(9, 4, BLUE, false)));
    let mut skp = magic.to_vec();
    skp.extend_from_slice(b"VFF\x00\x01\x02");
    skp.extend(zip("meta/model_thumbnail.png"));
    let documents: Vec<(PreviewFormat, Vec<u8>)> = vec![
        (PreviewFormat::Procreate, zip("QuickLook/Thumbnail.png")),
        (PreviewFormat::Sketch, zip("previews/preview.png")),
        (PreviewFormat::AdobeXd, zip("thumbnail.png")),
        (PreviewFormat::FreeCad, zip("thumbnails/Thumbnail.png")),
        (PreviewFormat::SketchUp, skp),
        (PreviewFormat::Figma, kiwi),
        (PreviewFormat::Affinity, affinity(&[(b"Thmb", png(7, 5, RED, false)), (b"Meta", vec![b'x'; 40])])),
        (PreviewFormat::PaintNet, pdn),
        (PreviewFormat::InDesign, indd(&xmp(&png(5, 5, RED, false)), &xmp(&png(8, 6, BLUE, true)))),
        (PreviewFormat::ClipStudio, clip(&preview_db(&png(32, 32, BLUE, true), &png(8, 8, RED, true)), true)),
    ];
    let limits = Limits::thumbnail();
    for (format, bytes) in documents {
        // 1. Cut off at every offset (every 7th for the big ones).
        let step = if bytes.len() > 8000 { 7 } else { 1 };
        for cut in (0..bytes.len()).step_by(step) {
            let _ = load_source(format, &mut Source::memory(bytes[..cut].to_vec()), &limits);
        }
        // 2. Bytes overwritten, half of them in the first kilobyte and half
        //    in the last, where headers and directories are.
        let mut seed = 0x9E37_79B9u32 ^ bytes.len() as u32;
        for round in 0..3000 {
            let mut copy = bytes.clone();
            for _ in 0..1 + round % 6 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let near = copy.len().min(1024);
                let i = match seed % 3 {
                    0 => seed as usize % copy.len(),
                    1 => seed as usize % near,
                    _ => copy.len() - 1 - seed as usize % near,
                };
                copy[i] = (seed >> 8) as u8;
            }
            let _ = load_source(format, &mut Source::memory(copy), &limits);
        }
    }
}

//endregion
