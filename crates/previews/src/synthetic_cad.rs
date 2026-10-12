//! Tiny CAD files built here: AutoCAD drawings, DWF and DWFx packages, OLE
//! documents of SolidWorks, Inventor, Revit and Solid Edge, SolidWorks 2015+
//! records and Fusion zips (Zstandard and deflate).  They check each reader
//! wherever the crate is built, and that damaged and hostile files (loops in
//! an OLE allocation table, sizes of gigabytes, zip bombs) fail quickly.

use super::*;
use crate::zip::tests::stored_zip;
use slerp_scene::textures::Pixels;

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

/// An RGBA PNG of one colour.
pub(crate) fn png(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&color.repeat((w * h) as usize)).unwrap();
    out
}

/// A 2x2 8-bit DIB (header, palette, rows padded to four bytes) of palette
/// entry 1, which is pure green.
pub(crate) fn dib() -> Vec<u8> {
    let mut d = Vec::new();
    for v in [40u32, 2, 2] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&8u16.to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&8u32.to_le_bytes());
    d.extend_from_slice(&[0; 8]);
    d.extend_from_slice(&2u32.to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&[0, 0, 0, 0, 0, 255, 0, 0]);
    d.extend_from_slice(&[1, 1, 0, 0, 1, 1, 0, 0]);
    d
}

pub(crate) fn sniffed(name: &str, bytes: &[u8]) -> Option<PreviewFormat> {
    sniff(Path::new(name), &bytes[..bytes.len().min(4096)])
}

pub(crate) fn load(format: PreviewFormat, bytes: Vec<u8>, limits: &Limits) -> Result<Stored, LoadError> {
    read_stored(format, &mut Source::memory(bytes), limits)
}

/// The picture a file shows: its size and first texel.
pub(crate) fn shown(format: PreviewFormat, bytes: Vec<u8>) -> ((u32, u32), [u8; 4]) {
    match load(format, bytes, &Limits::viewer()) {
        Ok(Stored::Image(t)) => {
            let Pixels::Rgba8(px) = t.base() else { panic!("{format:?}: not 8-bit") };
            ((t.width(), t.height()), [px[0], px[1], px[2], px[3]])
        }
        Ok(Stored::Metafile(_)) => panic!("{format:?}: a metafile"),
        Err(e) => panic!("{format:?}: {e}"),
    }
}

pub(crate) fn refused(format: PreviewFormat, bytes: Vec<u8>) {
    let start = std::time::Instant::now();
    assert!(load(format, bytes, &Limits::viewer()).is_err(), "{format:?}");
    assert!(start.elapsed().as_millis() < 500, "{format:?}: {:?}", start.elapsed());
}

/// Truncated copies and copies with bytes overwritten: errors or pictures,
/// never panics.
pub(crate) fn survives_damage(format: PreviewFormat, bytes: &[u8]) {
    for cut in (0..bytes.len()).step_by((bytes.len() / 200).max(1)) {
        let _ = load(format, bytes[..cut].to_vec(), &Limits::thumbnail());
    }
    let mut seed = 0x9E37_79B9u32;
    for round in 0..400 {
        let mut copy = bytes.to_vec();
        for _ in 0..1 + round % 6 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let i = seed as usize % copy.len();
            copy[i] = (seed >> 8) as u8;
        }
        let _ = load(format, copy, &Limits::thumbnail());
    }
}

/// An OLE compound file (512-byte sectors) holding `streams` at the top
/// level.  Every stream is a regular one: the mini stream cutoff is zero.
/// `fat_loop` makes the first stream's chain point back at itself.
pub(crate) fn ole(streams: &[(&str, &[u8])], fat_loop: bool) -> Vec<u8> {
    const END: u32 = 0xFFFF_FFFE;
    let dir_sectors = (streams.len() + 1).div_ceil(4) as u32;
    let mut fat: Vec<u32> = vec![0xFFFF_FFFD];
    // Directory sectors, chained.
    for i in 0..dir_sectors {
        fat.push(if i + 1 < dir_sectors { 2 + i } else { END });
    }
    let mut starts = Vec::new();
    for (_, data) in streams {
        let n = data.len().div_ceil(512).max(1) as u32;
        let first = fat.len() as u32;
        starts.push(first);
        for i in 0..n {
            fat.push(if i + 1 < n { first + i + 1 } else { END });
        }
    }
    if fat_loop && !starts.is_empty() {
        fat[starts[0] as usize] = starts[0];
    }
    assert!(fat.len() <= 128, "one FAT sector");
    fat.resize(128, 0xFFFF_FFFF);
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(MAGIC_OLE);
    h[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
    h[26..28].copy_from_slice(&3u16.to_le_bytes());
    h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
    h[30..32].copy_from_slice(&9u16.to_le_bytes());
    h[32..34].copy_from_slice(&6u16.to_le_bytes());
    h[44..48].copy_from_slice(&1u32.to_le_bytes());
    h[48..52].copy_from_slice(&1u32.to_le_bytes());
    h[60..64].copy_from_slice(&END.to_le_bytes());
    h[68..72].copy_from_slice(&END.to_le_bytes());
    for i in 0..109 {
        let v: u32 = if i == 0 { 0 } else { 0xFFFF_FFFF };
        h[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut out = h;
    out.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
    // The directory: the root, then the streams.
    let entry = |name: &str, kind: u8, start: u32, size: u32| {
        let mut e = vec![0u8; 128];
        let units: Vec<u16> = name.encode_utf16().collect();
        for (i, u) in units.iter().enumerate() {
            e[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        e[64..66].copy_from_slice(&((units.len() as u16 + 1) * 2).to_le_bytes());
        e[66] = kind;
        e[68..80].copy_from_slice(&[0xFF; 12]);
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..124].copy_from_slice(&size.to_le_bytes());
        e
    };
    let mut dir = entry("Root Entry", 5, END, 0);
    for ((name, data), start) in streams.iter().zip(&starts) {
        dir.extend(entry(name, 2, *start, data.len() as u32));
    }
    dir.resize(dir_sectors as usize * 512, 0);
    out.extend(dir);
    for (_, data) in streams {
        let mut d = data.to_vec();
        d.resize(data.len().div_ceil(512).max(1) * 512, 0);
        out.extend(d);
    }
    out
}

const MAGIC_OLE: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// A property set whose one clipboard property (number `pid`) holds `clip`
/// (from the tag on).
fn clip_set(pid: u32, clip: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; 48];
    s[0] = 0xFE;
    s[1] = 0xFF;
    s[24] = 1;
    s[44] = 48;
    let mut set = Vec::new();
    set.extend_from_slice(&0u32.to_le_bytes());
    set.extend_from_slice(&1u32.to_le_bytes());
    set.extend_from_slice(&pid.to_le_bytes());
    set.extend_from_slice(&16u32.to_le_bytes());
    set.extend_from_slice(&71u32.to_le_bytes());
    set.extend_from_slice(&(clip.len() as u32).to_le_bytes());
    set.extend_from_slice(clip);
    s.extend(set);
    s
}

//endregion

/*┌─────────────────╮
| AutoCAD         |
└─────────────────╯*/
//region AutoCAD

const SENTINEL: [u8; 16] =
    [0x1F, 0x25, 0x6D, 0x07, 0xD4, 0x36, 0x28, 0x28, 0x9D, 0x57, 0xCA, 0x3F, 0x9D, 0x44, 0x10, 0x2B];

/// A drawing whose preview section sits at 0x40 and holds `entries` (type
/// and data), with the header's pointer at 0x0D aimed at `pointer`.
fn dwg(version: &[u8; 6], entries: &[(u8, &[u8])], pointer: u32) -> Vec<u8> {
    let mut d = vec![0u8; 0x40];
    d[..6].copy_from_slice(version);
    d[0x0D..0x11].copy_from_slice(&pointer.to_le_bytes());
    d.extend_from_slice(&SENTINEL);
    d.extend_from_slice(&0u32.to_le_bytes());
    d.push(entries.len() as u8);
    let mut at = 0x40 + 16 + 5 + entries.len() * 9;
    let mut data = Vec::new();
    for (kind, bytes) in entries {
        d.push(*kind);
        d.extend_from_slice(&(at as u32).to_le_bytes());
        d.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        at += bytes.len();
        data.extend_from_slice(bytes);
    }
    d.extend(data);
    d
}

#[test]
fn dwg_previews_prefer_png_then_bmp_then_wmf() {
    let header = [0u8; 80];
    let p = png(3, 2, [10, 20, 30, 255]);
    let bytes = dwg(b"AC1032", &[(1, &header), (2, &dib()), (6, &p)], 0x40);
    assert_eq!(sniffed("a.dwg", &bytes), Some(PreviewFormat::Dwg));
    assert_eq!(shown(PreviewFormat::Dwg, bytes), ((3, 2), [10, 20, 30, 255]));
    // A BMP without its file header, as R13 to 2010 write.
    let bytes = dwg(b"AC1015", &[(1, &header), (2, &dib())], 0x40);
    assert_eq!(shown(PreviewFormat::Dwg, bytes), ((2, 2), [0, 255, 0, 255]));
    // A metafile goes to the caller, which draws it.
    let wmf = b"\xD7\xCD\xC6\x9Ametafile".to_vec();
    match load(PreviewFormat::Dwg, dwg(b"AC1018", &[(3, &wmf)], 0x40), &Limits::viewer()) {
        Ok(Stored::Metafile(m)) => assert_eq!(m, wmf),
        _ => panic!("expected the metafile"),
    }
    // A pointer that misses: the section is found by its sentinel.
    let bytes = dwg(b"AC1024", &[(6, &p)], 0x7777);
    assert_eq!(shown(PreviewFormat::Dwg, bytes).0, (3, 2));
}

#[test]
fn dwg_files_without_a_picture_say_so() {
    let no = |bytes: Vec<u8>| {
        assert!(matches!(load(PreviewFormat::Dwg, bytes, &Limits::viewer()), Err(LoadError::Unsupported(_))))
    };
    // Only header data, as LibreDWG writes.
    no(dwg(b"AC1032", &[(1, &[0u8; 80])], 0x40));
    no(dwg(b"AC1032", &[], 0x40));
    // R12 and older have no preview section.
    let mut old = vec![0u8; 256];
    old[..6].copy_from_slice(b"AC1009");
    no(old);
    // Text named .dwg (DXF saved under the wrong name) isn't claimed here.
    assert_eq!(sniffed("x.dwg", b"  0\nSECTION\n"), None);
}

#[test]
fn hostile_dwg_sections_are_bounded() {
    // An entry of 4 GB, an entry past the end, 255 entries.
    let mut huge = dwg(b"AC1032", &[(6, b"\x89PNG\r\n\x1a\n")], 0x40);
    let at = 0x40 + 16 + 5 + 5;
    huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    refused(PreviewFormat::Dwg, huge);
    let mut past = dwg(b"AC1032", &[(6, b"\x89PNG\r\n\x1a\n")], 0x40);
    past[0x40 + 16 + 5 + 1..0x40 + 16 + 5 + 5].copy_from_slice(&0x7FFF_FFF0u32.to_le_bytes());
    refused(PreviewFormat::Dwg, past);
    let mut many = dwg(b"AC1032", &[], 0x40);
    many[0x40 + 20] = 255;
    refused(PreviewFormat::Dwg, many);
    survives_damage(PreviewFormat::Dwg, &dwg(b"AC1032", &[(1, &[0u8; 80]), (6, &png(4, 4, [1, 2, 3, 255]))], 0x40));
    survives_damage(PreviewFormat::Dwg, &dwg(b"AC1015", &[(2, &dib())], 0x40));
}

//endregion

/*┌─────────────────╮
| DWF             |
└─────────────────╯*/
//region DWF

fn dwf_manifest(sections: &[(&str, &str)]) -> String {
    let mut m = String::from(r#"<dwf:Manifest xmlns:dwf="DWF-Manifest:6.0" version="6.0"><dwf:Sections>"#);
    for (title, href) in sections {
        m.push_str(&format!(
            r#"<dwf:Section type="com.autodesk.dwf.ePlot" title="{title}"><dwf:Toc><dwf:Resource dwf:role="2d streaming graphics" dwf:href="x.w2d"/><dwf:Resource dwf:role="thumbnail" dwf:mime="image/png" dwf:href="{href}"/></dwf:Toc></dwf:Section>"#
        ));
    }
    m.push_str("</dwf:Sections></dwf:Manifest>\0");
    m
}

#[test]
fn dwf_sheets_are_parts_with_their_thumbnails() {
    let (red, blue) = (png(2, 1, [255, 0, 0, 255]), png(1, 3, [0, 0, 255, 255]));
    let manifest = dwf_manifest(&[("Model", "s1\\a.png"), ("Layout", "s2\\b.png")]);
    let zip = stored_zip(&[("manifest.xml", manifest.as_bytes()), ("s1\\a.png", &red), ("s2\\b.png", &blue)]);
    let mut bytes = b"(DWF V06.00)".to_vec();
    bytes.extend(zip);
    assert_eq!(sniffed("a.dwf", &bytes), Some(PreviewFormat::Dwf));
    assert_eq!(shown(PreviewFormat::Dwf, bytes.clone()), ((2, 1), [255, 0, 0, 255]));
    let sheets = dwf::sheets(&mut Source::memory(bytes.clone())).unwrap();
    assert_eq!(sheets.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Model", "Layout"]);
    let t = dwf::preview(&mut Source::memory(bytes.clone()), &Limits::viewer(), Some("Layout")).unwrap();
    assert_eq!((t.width(), t.height()), (1, 3));
    survives_damage(PreviewFormat::Dwf, &bytes);
    // DWF before version 6: a vector stream without a picture.
    let old = b"(DWF V00.55)\x00\x01\x02 lots of W2D opcodes".to_vec();
    assert!(matches!(load(PreviewFormat::Dwf, old, &Limits::viewer()), Err(LoadError::Unsupported(_))));
}

#[test]
fn dwfx_falls_back_to_the_package_thumbnail() {
    let p = png(2, 2, [0, 128, 0, 255]);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/dwf/thumb.png" Id="r1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail"/></Relationships>"#;
    let bytes = stored_zip(&[("_rels/.rels", rels.as_bytes()), ("dwf/thumb.png", &p)]);
    assert_eq!(sniffed("a.dwfx", &bytes), Some(PreviewFormat::Dwf));
    assert_eq!(shown(PreviewFormat::Dwf, bytes).0, (2, 2));
    // A plain zip named .dwfx has no picture.
    let plain = stored_zip(&[("readme.txt", b"hi")]);
    assert!(matches!(load(PreviewFormat::Dwf, plain, &Limits::viewer()), Err(LoadError::Unsupported(_))));
}

//endregion

/*┌─────────────────╮
| OLE CAD files   |
└─────────────────╯*/
//region OLE CAD files

#[test]
fn solidworks_ole_files_prefer_the_png_stream() {
    let p = png(4, 3, [1, 2, 3, 255]);
    let mut preview = (dib().len() as u32).to_le_bytes().to_vec();
    preview.extend(dib());
    let both = ole(&[("Preview", &preview), ("PreviewPNG", &p)], false);
    assert_eq!(sniffed("part.SLDPRT", &both), Some(PreviewFormat::SolidWorks));
    assert_eq!(shown(PreviewFormat::SolidWorks, both).0, (4, 3));
    // Only the DIB, as some 2012 files have.
    let dib_only = ole(&[("Preview", &preview)], false);
    assert_eq!(shown(PreviewFormat::SolidWorks, dib_only), ((2, 2), [0, 255, 0, 255]));
    // Neither.
    let none = ole(&[("Contents", b"x")], false);
    assert!(matches!(load(PreviewFormat::SolidWorks, none, &Limits::viewer()), Err(LoadError::Unsupported(_))));
    // Other OLE files aren't claimed by these extensions' neighbours.
    assert_eq!(sniffed("letter.doc", &ole(&[("WordDocument", b"x")], false)), None);
}

#[test]
fn inventor_revit_and_solid_edge_keep_pictures_in_their_own_streams() {
    let p = png(5, 5, [9, 9, 9, 255]);
    // Inventor: a metafile picture's header, then a PNG.
    let mut clip = 0xFFFF_FFFFu32.to_le_bytes().to_vec();
    clip.extend_from_slice(&3u32.to_le_bytes());
    clip.extend_from_slice(&[8, 0, 180, 0, 180, 0, 0, 0]);
    clip.extend_from_slice(&p);
    let inventor = ole(&[("\u{5}Zrxrt4arFafyu34gYa3l3ohgHg", &clip_set(17, &clip))], false);
    assert_eq!(sniffed("a.ipt", &inventor), Some(PreviewFormat::Inventor));
    assert_eq!(shown(PreviewFormat::Inventor, inventor.clone()).0, (5, 5));
    survives_damage(PreviewFormat::Inventor, &inventor);
    // Revit: a header, then a PNG.
    let mut stream = b"\x62\x19\x22\x05 a Revit preview header".to_vec();
    stream.extend_from_slice(&p);
    let revit = ole(&[("BasicFileInfo", b"x"), ("RevitPreview4.0", &stream)], false);
    assert_eq!(sniffed("a.rfa", &revit), Some(PreviewFormat::Revit));
    assert_eq!(shown(PreviewFormat::Revit, revit).0, (5, 5));
    // Solid Edge: the tag, then the DIB.
    let mut clip = 0xFFFF_FFFFu32.to_le_bytes().to_vec();
    clip.extend(dib());
    let edge = ole(&[("\u{5}C3teagxwOttdbfkuIaamtae3Ie", &clip_set(10, &clip))], false);
    for name in ["a.par", "a.psm", "a.asm", "a.dft"] {
        assert_eq!(sniffed(name, &edge), Some(PreviewFormat::SolidEdge), "{name}");
    }
    assert_eq!(shown(PreviewFormat::SolidEdge, edge.clone()), ((2, 2), [0, 255, 0, 255]));
    survives_damage(PreviewFormat::SolidEdge, &edge);
    // Assembler source and parity files named like Solid Edge's aren't OLE.
    assert_eq!(sniffed("boot.asm", b"section .text\nglobal _start\n"), None);
    assert_eq!(sniffed("x.par", b"PAR2\0PKT"), None);
}

#[test]
fn ole_chains_that_loop_end() {
    let p = png(2, 2, [1, 1, 1, 255]);
    let mut big = p.clone();
    big.resize(4000, 0);
    let looped = ole(&[("PreviewPNG", &big)], true);
    let start = std::time::Instant::now();
    let _ = load(PreviewFormat::SolidWorks, looped, &Limits::viewer());
    assert!(start.elapsed().as_millis() < 500, "{:?}", start.elapsed());
    survives_damage(PreviewFormat::SolidWorks, &ole(&[("PreviewPNG", &p)], false));
}

//endregion

/*┌─────────────────╮
| SolidWorks 2015+ |
└─────────────────╯*/
//region SolidWorks 2015+

/// A record as SolidWorks 2015+ writes it: the zip-like header, the name
/// with its nibbles swapped, raw deflate, and four more bytes.
fn sw_record(name: &str, data: &[u8], lie: bool) -> Vec<u8> {
    let packed = miniz_oxide::deflate::compress_to_vec(data, 6);
    let mut r = vec![0x14, 0x00, 0x06, 0x00, 0x08, 0x00];
    r.extend_from_slice(&[0; 8]);
    r.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    r.extend_from_slice(&((data.len() as u32) + lie as u32).to_le_bytes());
    r.extend_from_slice(&(name.len() as u16).to_le_bytes());
    r.extend_from_slice(&0u16.to_le_bytes());
    r.extend(name.bytes().map(|b| b.rotate_left(4)));
    r.extend(packed);
    r.extend_from_slice(&[0; 4]);
    r
}

fn sw_modern(records: &[Vec<u8>]) -> Vec<u8> {
    let mut f = vec![0x44, 0xA0, 0xE2, 0x49, 0, 0, 0, 4, 0x16, 0x00, 0x9F, 0x5C, 0x45];
    for r in records {
        f.extend_from_slice(r);
    }
    f
}

#[test]
fn modern_solidworks_records_are_found_and_checked() {
    let p = png(6, 4, [7, 8, 9, 255]);
    // A decoy record whose stated size is wrong comes first, as the index
    // records near the start of real files do.
    let bytes = sw_modern(&[
        sw_record("Header2", b"some header", false),
        sw_record("PreviewPNG", &p, true),
        sw_record("Contents/Config-0", &[3u8; 5000], false),
        sw_record("PreviewPNG", &p, false),
    ]);
    assert_eq!(sniffed("x.sldasm", &bytes), Some(PreviewFormat::SolidWorks));
    assert_eq!(sniffed("x.bin", &bytes), None);
    assert_eq!(shown(PreviewFormat::SolidWorks, bytes.clone()).0, (6, 4));
    survives_damage(PreviewFormat::SolidWorks, &bytes);
    // Only the DIB record.
    let mut preview = (dib().len() as u32).to_le_bytes().to_vec();
    preview.extend(dib());
    let dib_only = sw_modern(&[sw_record("Preview", &preview, false)]);
    assert_eq!(shown(PreviewFormat::SolidWorks, dib_only).0, (2, 2));
    // Neither: no picture.
    let none = sw_modern(&[sw_record("Header2", b"x", false)]);
    assert!(matches!(load(PreviewFormat::SolidWorks, none, &Limits::viewer()), Err(LoadError::Unsupported(_))));
    // A record claiming a gigabyte is skipped without allocating it.
    let mut huge = sw_record("PreviewPNG", &p, false);
    huge[18..22].copy_from_slice(&0x4000_0000u32.to_le_bytes());
    refused(PreviewFormat::SolidWorks, sw_modern(&[huge]));
}

//endregion

/*┌─────────────────╮
| Fusion          |
└─────────────────╯*/
//region Fusion

/// A zip of one member compressed with `method` (8 deflate, 93 Zstandard),
/// declaring `size` as its inflated size.
fn zip_one(name: &str, data: &[u8], method: u16, size: u32) -> Vec<u8> {
    let packed = match method {
        93 => zstd::bulk::compress(data, 3).unwrap(),
        8 => miniz_oxide::deflate::compress_to_vec(data, 6),
        _ => data.to_vec(),
    };
    let mut zip = stored_zip(&[(name, &packed)]);
    // Patch the method and the inflated size in both headers.
    let local = 0;
    zip[local + 8..local + 10].copy_from_slice(&method.to_le_bytes());
    zip[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
    let dir = zip.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    zip[dir + 10..dir + 12].copy_from_slice(&method.to_le_bytes());
    zip[dir + 24..dir + 28].copy_from_slice(&size.to_le_bytes());
    zip
}

#[test]
fn fusion_previews_inflate_deflate_and_zstandard() {
    let p = png(8, 8, [200, 100, 50, 255]);
    for method in [8, 93] {
        let bytes = zip_one("FusionAssetName[Active]/Previews/small.png", &p, method, p.len() as u32);
        assert_eq!(sniffed("design.f3d", &bytes), Some(PreviewFormat::Fusion));
        assert_eq!(shown(PreviewFormat::Fusion, bytes.clone()).0, (8, 8), "method {method}");
        survives_damage(PreviewFormat::Fusion, &bytes);
    }
    // A Zstandard bomb: 64 MB of zeros declared as 1000 bytes stops there.
    let zeros = vec![0u8; 64 << 20];
    let bomb = zip_one("FusionAssetName[Active]/Previews/small.png", &zeros, 93, 1000);
    assert!(bomb.len() < 1 << 20);
    refused(PreviewFormat::Fusion, bomb);
    // Bazis-Mebelshchik's .f3d files aren't zips and aren't claimed.
    assert_eq!(sniffed("chair.f3d", b"BZ85\x01\x00\x00\xff\x00\x0b"), None);
}

//endregion

/*┌─────────────────╮
| Under any name  |
└─────────────────╯*/
//region Under any name

/// Files the MIME database claims by content alone are recognised here the
/// same way: Solid Edge by its root storage's class id, Fusion and
/// Pixelmator Pro by their zips' first members.
#[test]
fn content_claims_match_the_mime_database() {
    let mut edge = ole(&[("JSitesList", b"x")], false);
    assert_eq!(sniffed("bucket", &edge), None);
    // The root entry is the directory's first, in sector 1 (byte 1024).
    edge[1024 + 80..1024 + 96].copy_from_slice(&[
        0x00, 0xbf, 0xc6, 0x00, 0x3b, 0x48, 0xce, 0x11, 0x95, 0x1a, 0x08, 0x00, 0x36, 0x01, 0xbe, 0x52,
    ]);
    assert_eq!(sniffed("bucket", &edge), Some(PreviewFormat::SolidEdge));
    assert_eq!(sniffed("bucket.bin", &edge), Some(PreviewFormat::SolidEdge));
    let fusion = stored_zip(&[
        ("FusionAssetName[Active]/", b""),
        ("FusionAssetName[Active]/Previews/small.png", &png(1, 1, [0; 4])),
    ]);
    assert_eq!(sniffed("design", &fusion), Some(PreviewFormat::Fusion));
    let pixelmator = stored_zip(&[("metadata.info", b"SQLite format 3\0")]);
    assert_eq!(sniffed("art", &pixelmator), Some(PreviewFormat::Pixelmator));
    assert_eq!(sniffed("other.zip", &stored_zip(&[("readme.txt", b"hi")])), None);
}

//endregion
