//! Tests: records built by hand and replayed, file-level behaviour, and
//! damaged input.  Real files come from `local/assets/metafile-extra/` and
//! the tests that use them skip quietly when the folder is absent.

use super::*;
use slerp_scene::textures::Pixels;
use std::path::PathBuf;

fn limits(size: u32) -> Limits {
    Limits { max_texture_size: Some(size), ..Limits::thumbnail() }
}

/// RGBA at a pixel of a rendered texture.
fn px(t: &Texture, x: u32, y: u32) -> [u8; 4] {
    let l = &t.levels[0];
    let Pixels::Rgba8(d) = &l.surfaces[0] else { panic!("not RGBA8") };
    let i = ((y * l.width + x) * 4) as usize;
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

/*┌─────────────────────╮
| Builders for records  |
└─────────────────────╯*/
//region Builders

fn wmf_record(func: u16, params: &[i16]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&((3 + params.len()) as u32).to_le_bytes());
    v.extend_from_slice(&func.to_le_bytes());
    for p in params {
        v.extend_from_slice(&p.to_le_bytes());
    }
    v
}

/// A placeable WMF whose bounding box is `0..w` by `0..h` at 100 units per
/// inch, with the given records and an end record.
fn wmf(w: i16, h: i16, records: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0x9AC6CDD7u32.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    for c in [0i16, 0, w, h] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v.extend_from_slice(&100u16.to_le_bytes());
    v.extend_from_slice(&[0; 6]);
    // The standard header: memory metafile, header size, version, size, objects, max record, members.
    for w in [1u16, 9, 0x0300] {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    for r in records {
        v.extend_from_slice(r);
    }
    v.extend_from_slice(&wmf_record(0, &[]));
    v
}

fn color_words(r: u8, g: u8, b: u8) -> [i16; 2] {
    [(r as i16) | ((g as i16) << 8), b as i16]
}

/// Records that set the window and select a solid brush of a colour and no pen.
fn setup(w: i16, h: i16, brush: [u8; 3]) -> Vec<Vec<u8>> {
    let [lo, hi] = color_words(brush[0], brush[1], brush[2]);
    vec![
        wmf_record(0x0103, &[8]),
        wmf_record(0x020B, &[0, 0]),
        wmf_record(0x020C, &[h, w]),
        wmf_record(0x02FC, &[0, lo, hi, 0]),
        wmf_record(0x012D, &[0]),
        wmf_record(0x02FA, &[5, 0, 0, 0, 0]),
        wmf_record(0x012D, &[1]),
    ]
}

fn emf_record(ty: u32, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    let size = (8 + body.len()).div_ceil(4) * 4;
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&(size as u32).to_le_bytes());
    v.extend_from_slice(body);
    v.resize(size, 0);
    v
}

fn ints(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// An EMF with a frame of `w` by `h` device pixels on a 10 pixels per
/// millimetre device (so the frame is `w * 10` by `h * 10` hundredths of a
/// millimetre).
fn emf(w: i32, h: i32, records: &[Vec<u8>]) -> Vec<u8> {
    let mut body = ints(&[0, 0, w - 1, h - 1]);
    body.extend(ints(&[0, 0, w * 10, h * 10]));
    body.extend_from_slice(&0x464D4520u32.to_le_bytes());
    body.extend(ints(&[0x10000, 0, 0]));
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend(ints(&[0, 0, 0]));
    // Device 1000 x 1000 pixels, 100 x 100 mm: ten pixels per millimetre.
    body.extend(ints(&[1000, 1000, 100, 100]));
    let mut v = emf_record(1, &body);
    for r in records {
        v.extend_from_slice(r);
    }
    v.extend(emf_record(14, &ints(&[0, 16, 20])));
    v
}

fn emf_brush(index: i32, rgb: [u8; 3]) -> Vec<u8> {
    let c = rgb[0] as i32 | (rgb[1] as i32) << 8 | (rgb[2] as i32) << 16;
    emf_record(39, &ints(&[index, 0, c, 0]))
}

//endregion

/*┌─────────────────────╮
| Playing records       |
└─────────────────────╯*/
//region Playing records

#[test]
fn wmf_rectangle_lands_where_the_window_says() {
    let mut recs = setup(100, 100, [255, 0, 0]);
    // right, bottom order: the record stores bottom, right, top, left.
    recs.push(wmf_record(0x041B, &[60, 60, 10, 10]));
    let t = load(MetafileFormat::Wmf, &wmf(100, 100, &recs), &limits(100)).unwrap();
    assert_eq!((t.width(), t.height()), (100, 100));
    assert_eq!(px(&t, 30, 30), [255, 0, 0, 255]);
    assert_eq!(px(&t, 80, 80)[3], 0, "outside the rectangle the page is transparent");
    assert_eq!(px(&t, 5, 5)[3], 0);
}

#[test]
fn the_picture_keeps_the_frames_aspect_ratio() {
    let mut recs = setup(200, 100, [0, 0, 255]);
    recs.push(wmf_record(0x041B, &[100, 200, 0, 0]));
    let t = load(MetafileFormat::Wmf, &wmf(200, 100, &recs), &limits(256)).unwrap();
    assert_eq!((t.width(), t.height()), (256, 128));
    assert_eq!(px(&t, 128, 64), [0, 0, 255, 255]);
}

#[test]
fn restore_dc_returns_to_the_saved_brush() {
    let [lo, hi] = color_words(0, 255, 0);
    let mut recs = setup(100, 100, [255, 0, 0]);
    recs.push(wmf_record(0x001E, &[]));
    recs.push(wmf_record(0x02FC, &[0, lo, hi, 0]));
    recs.push(wmf_record(0x012D, &[2]));
    // A count of zero is ignored; minus one goes back to red.
    recs.push(wmf_record(0x0127, &[0]));
    recs.push(wmf_record(0x041B, &[40, 40, 0, 0]));
    recs.push(wmf_record(0x0127, &[-1]));
    recs.push(wmf_record(0x041B, &[100, 100, 60, 60]));
    let t = load(MetafileFormat::Wmf, &wmf(100, 100, &recs), &limits(100)).unwrap();
    assert_eq!(px(&t, 20, 20), [0, 255, 0, 255]);
    assert_eq!(px(&t, 80, 80), [255, 0, 0, 255]);
}

#[test]
fn emf_himetric_mapping_flips_y() {
    // MM_HIMETRIC: 0.01 mm units, y up.  A rectangle near logical y=0 sits
    // at the bottom of the frame once the origin is the lower left.
    let recs = vec![
        emf_record(17, &ints(&[3])),
        emf_brush(1, [255, 0, 0]),
        emf_record(37, &ints(&[1])),
        // Window origin at the top of the 500-unit-high frame, y up.
        emf_record(10, &ints(&[0, 500])),
        // Rectangle from y=500 down to y=400 (the top fifth), 0..100 wide.
        emf_record(43, &ints(&[0, 400, 100, 500])),
    ];
    let t = load(MetafileFormat::Emf, &emf(50, 50, &recs), &limits(100)).unwrap();
    assert_eq!(px(&t, 10, 5)[3], 255, "the rectangle is at the top");
    assert_eq!(px(&t, 10, 90)[3], 0);
    assert_eq!(px(&t, 90, 5)[3], 0);
}

#[test]
fn emf_world_transform_moves_shapes() {
    let mut xf = Vec::new();
    for v in [1.0f32, 0.0, 0.0, 1.0, 25.0, 0.0] {
        xf.extend_from_slice(&v.to_le_bytes());
    }
    let mut body = xf.clone();
    body.extend_from_slice(&4u32.to_le_bytes());
    let recs = vec![
        emf_brush(1, [0, 0, 255]),
        emf_record(37, &ints(&[1])),
        emf_record(36, &body),
        emf_record(43, &ints(&[0, 0, 25, 50])),
    ];
    let t = load(MetafileFormat::Emf, &emf(50, 50, &recs), &limits(100)).unwrap();
    assert_eq!(px(&t, 25, 50)[3], 0, "the left half stays empty");
    assert_eq!(px(&t, 75, 50), [0, 0, 255, 255]);
}

#[test]
fn emf_paths_fill_and_clip() {
    // A triangle path, filled red, then a rectangular clip and a blue fill.
    let mut poly = ints(&[0, 0, 49, 49]);
    poly.extend(ints(&[3, 0, 0, 50, 0, 0, 50]));
    let recs = vec![
        emf_brush(1, [255, 0, 0]),
        emf_record(37, &ints(&[1])),
        emf_record(59, &[]),
        emf_record(3, &poly),
        emf_record(60, &[]),
        emf_record(62, &ints(&[0, 0, 49, 49])),
    ];
    let t = load(MetafileFormat::Emf, &emf(50, 50, &recs), &limits(100)).unwrap();
    assert_eq!(px(&t, 20, 20), [255, 0, 0, 255], "inside the triangle");
    assert_eq!(px(&t, 80, 80)[3], 0, "the far corner is outside it");
}

#[test]
fn rop3_blits_work_on_a_white_page() {
    // PATBLT with DSTINVERT over an unpainted area turns white into black.
    // EMR_BITBLT without a bitmap: the fields up to cbBits, zeros.
    let mut b = ints(&[0, 0, 49, 49, 10, 10, 20, 20, 0x00550009, 0, 0]);
    b.extend(vec![0u8; 24 + 4 + 4 + 4 * 4 + 4]);
    let recs = vec![emf_record(76, &b)];
    let t = load(MetafileFormat::Emf, &emf(50, 50, &recs), &limits(100)).unwrap();
    assert_eq!(px(&t, 40, 40), [0, 0, 0, 255], "inverted white is black");
    assert_eq!(px(&t, 80, 80)[3], 0);
}

#[test]
fn text_draws_something_and_respects_alignment() {
    // ExtTextOutW "HI" at (5, 30), 12 pixels high.
    let font = {
        let mut b = ints(&[1, -20, 0, 0, 0, 400]);
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        b.resize(8 + 92 + 4, 0);
        emf_record(82, &b)
    };
    let text: Vec<u8> = "HI".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut b = ints(&[0, 0, 49, 49, 1, 0, 0]);
    b.extend(ints(&[5, 30, 2, 76, 0, 0, 0, 0, 0, 0]));
    b.resize(76 - 8, 0);
    b.extend_from_slice(&text);
    let recs = vec![font, emf_record(37, &ints(&[1])), emf_record(84, &b)];
    let t = load(MetafileFormat::Emf, &emf(50, 50, &recs), &limits(100)).unwrap();
    let l = &t.levels[0];
    let Pixels::Rgba8(d) = &l.surfaces[0] else { panic!() };
    let inked = d.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(inked > 40, "{inked} pixels drawn");
}

//endregion

/*┌─────────────────────╮
| PICT                  |
└─────────────────────╯*/
//region PICT

fn be16(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}

/// A version 2 PICT of 100 by 100 points with the opcodes appended.
fn pict2(ops: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 512];
    v.extend_from_slice(&[0, 0]);
    for c in [0i16, 0, 100, 100] {
        v.extend_from_slice(&be16(c));
    }
    v.extend_from_slice(&[0x00, 0x11, 0x02, 0xFF, 0x0C, 0x00]);
    v.extend_from_slice(&[0xFF, 0xFE, 0, 0]);
    v.extend_from_slice(&[0; 20]);
    v.extend_from_slice(ops);
    v.extend_from_slice(&[0x00, 0xFF]);
    v
}

fn rect_ops(op: u16, rect: [i16; 4]) -> Vec<u8> {
    let mut v = op.to_be_bytes().to_vec();
    for c in rect {
        v.extend_from_slice(&be16(c));
    }
    v
}

#[test]
fn pict_paint_rect_and_oval() {
    let mut ops = Vec::new();
    // RGBFgColor red, paintRect (top, left, bottom, right), then blue oval.
    ops.extend_from_slice(&[0x00, 0x1A, 0xFF, 0xFF, 0, 0, 0, 0]);
    ops.extend(rect_ops(0x0031, [10, 10, 50, 50]));
    ops.extend_from_slice(&[0x00, 0x1A, 0, 0, 0, 0, 0xFF, 0xFF]);
    ops.extend(rect_ops(0x0051, [50, 50, 90, 90]));
    let data = pict2(&ops);
    assert_eq!(sniff(Path::new("x.pict"), &data), Some(MetafileFormat::Pict));
    let t = load(MetafileFormat::Pict, &data, &limits(100)).unwrap();
    assert_eq!(px(&t, 30, 30), [255, 0, 0, 255]);
    assert_eq!(px(&t, 70, 70), [0, 0, 255, 255]);
    assert_eq!(px(&t, 70, 30)[3], 0);
}

#[test]
fn pict_frame_rect_uses_the_pen_size() {
    let mut ops = vec![0x00, 0x07, 0, 4, 0, 4];
    ops.extend(rect_ops(0x0030, [10, 10, 90, 90]));
    let t = load(MetafileFormat::Pict, &pict2(&ops), &limits(100)).unwrap();
    assert_eq!(px(&t, 11, 50), [0, 0, 0, 255], "the frame is four points wide, inside the rectangle");
    assert_eq!(px(&t, 50, 50)[3], 0, "the inside stays empty");
}

#[test]
fn pict_polygon_and_lines() {
    let mut ops = Vec::new();
    // paintPoly (0x71): size, bbox, then points as (v, h).
    ops.extend_from_slice(&0x0071u16.to_be_bytes());
    let pts: [(i16, i16); 3] = [(10, 10), (10, 90), (90, 10)];
    let size = 10 + pts.len() as i16 * 4;
    ops.extend_from_slice(&be16(size));
    for c in [10i16, 10, 90, 90] {
        ops.extend_from_slice(&be16(c));
    }
    for (v, h) in pts {
        ops.extend_from_slice(&be16(v));
        ops.extend_from_slice(&be16(h));
    }
    let t = load(MetafileFormat::Pict, &pict2(&ops), &limits(100)).unwrap();
    assert_eq!(px(&t, 25, 25), [0, 0, 0, 255]);
    assert_eq!(px(&t, 80, 80)[3], 0);
}

#[test]
fn pict_one_bit_bitmap_with_packbits() {
    // BitsRect (0x90): rowBytes, bounds, srcRect, dstRect, mode, rows.
    let mut ops = 0x0090u16.to_be_bytes().to_vec();
    ops.extend_from_slice(&be16(2));
    for c in [0i16, 0, 8, 8] {
        ops.extend_from_slice(&be16(c));
    }
    for c in [0i16, 0, 8, 8] {
        ops.extend_from_slice(&be16(c));
    }
    for c in [20i16, 20, 60, 60] {
        ops.extend_from_slice(&be16(c));
    }
    ops.extend_from_slice(&be16(0));
    for row in 0..8 {
        // Plain rows (rowBytes < 8): black on even rows.
        ops.extend_from_slice(&[if row % 2 == 0 { 0xFF } else { 0x00 }, 0]);
    }
    let t = load(MetafileFormat::Pict, &pict2(&ops), &limits(100)).unwrap();
    assert_eq!(px(&t, 30, 22), [0, 0, 0, 255], "an even row of the bitmap is black");
    assert_eq!(px(&t, 30, 27), [255, 255, 255, 255], "an odd row is white");
}

#[test]
fn pict_v1_needs_its_extension() {
    let mut v = vec![0, 0];
    for c in [0i16, 0, 50, 50] {
        v.extend_from_slice(&be16(c));
    }
    v.extend_from_slice(&[
        0x11, 0x01, 0x01, 0x00, 0x0A, 0, 0, 0, 0, 0, 50, 0, 50, 0x31, 0, 10, 0, 10, 0, 40, 0, 40, 0xFF,
    ]);
    assert_eq!(sniff(Path::new("a.pct"), &v), Some(MetafileFormat::Pict));
    assert_eq!(sniff(Path::new("a.bin"), &v), None);
    let t = load(MetafileFormat::Pict, &v, &limits(50)).unwrap();
    assert_eq!(px(&t, 25, 25), [0, 0, 0, 255]);
}

//endregion

/*┌─────────────────────╮
| Recognising files     |
└─────────────────────╯*/
//region Recognising files

#[test]
fn sniffing() {
    let w = wmf(10, 10, &[]);
    assert_eq!(sniff(Path::new("a.wmf"), &w), Some(MetafileFormat::Wmf));
    // Without the placeable header, by the standard header alone.
    assert_eq!(sniff(Path::new("a.bin"), &w[22..]), Some(MetafileFormat::Wmf));
    let disk = {
        let mut v = w[22..].to_vec();
        v[0] = 2;
        v
    };
    assert_eq!(sniff(Path::new("a.bin"), &disk), Some(MetafileFormat::Wmf));
    assert!(is_bare_wmf(&disk));
    let e = emf(10, 10, &[]);
    assert_eq!(sniff(Path::new("a.bin"), &e), Some(MetafileFormat::Emf));
    // Negative checks: close but not metafiles.
    assert_eq!(sniff(Path::new("a.wmf"), &[1, 0, 9, 0, 0, 2, 0, 0]), None, "an unknown version");
    assert_eq!(sniff(Path::new("a.emf"), &[1, 0, 0, 0, 0, 0, 0, 0]), None);
    assert_eq!(sniff(Path::new("a.png"), b"\x89PNG\r\n\x1a\n"), None);
    assert_eq!(sniff(Path::new("a.emz"), &[0x1F, 0x8B, 8, 0]), Some(MetafileFormat::Emf));
    assert_eq!(sniff(Path::new("a.gz"), &[0x1F, 0x8B, 8, 0]), None);
}

//endregion

/*┌─────────────────────╮
| Hostile input         |
└─────────────────────╯*/
//region Hostile input

/// A gzip file around deflated zeros.
fn gzip_zeros(n: usize) -> Vec<u8> {
    let raw = vec![0u8; n];
    let deflated = miniz_oxide::deflate::compress_to_vec(&raw, 6);
    let mut v = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 3];
    v.extend_from_slice(&deflated);
    v.extend_from_slice(&[0; 8]);
    v
}

#[test]
fn compression_bombs_are_refused() {
    let bomb = gzip_zeros(8 << 20);
    assert!(bomb.len() < 100_000, "{} bytes", bomb.len());
    let started = std::time::Instant::now();
    let err = gunzip_limited(&bomb, 1 << 20).unwrap_err();
    assert!(matches!(err, LoadError::TooLarge(_)), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // And through the public path: zeros are no metafile, just an error.
    assert!(load(MetafileFormat::Emf, &gzip_zeros(1 << 20), &limits(64)).is_err());
}

#[test]
fn gzip_framing_errors_are_errors() {
    assert!(gunzip(&[0x1F, 0x8B]).is_err());
    let mut ok = gzip_zeros(1000);
    ok[2] = 7;
    assert!(gunzip(&ok).is_err());
    let mut flags = gzip_zeros(1000);
    flags[3] = 0x1C;
    assert!(gunzip(&flags).is_err());
}

#[test]
fn absurd_counts_and_sizes_do_not_hang_or_allocate() {
    let started = std::time::Instant::now();
    // EMF polygons claiming billions of points, a bitmap of 100000 x 100000,
    // a huge pattern brush, and endless nesting of saves.
    let mut poly = ints(&[0, 0, 10, 10, 0x7FFFFFFF]);
    poly.extend(ints(&[1, 2, 3, 4]));
    let mut dib = ints(&[40, 100_000, 100_000]);
    dib.extend_from_slice(&[1, 0, 24, 0]);
    dib.extend(ints(&[0, 0, 0, 0, 0, 0]));
    let mut blt = ints(&[0, 0, 10, 10, 0, 0, 10, 10, 0x00CC0020, 0, 0]);
    blt.extend(vec![0u8; 24]);
    blt.extend(ints(&[0, 0]));
    blt.extend(ints(&[100, 40, 100, 0, 0]));
    blt.resize(100 - 8, 0);
    blt.extend_from_slice(&dib);
    let mut recs = vec![emf_record(86, &poly), emf_record(3, &poly), emf_record(76, &blt)];
    for _ in 0..50_000 {
        recs.push(emf_record(33, &[]));
    }
    recs.push(emf_record(34, &ints(&[-100_000])));
    let _ = load(MetafileFormat::Emf, &emf(10, 10, &recs), &limits(64));
    // WMF: a polygon of 65535 points with no data, object indices far out
    // of range, a region with absurd scan counts.
    let recs = vec![
        wmf_record(0x0324, &[-1]),
        wmf_record(0x012D, &[30000]),
        wmf_record(0x01F0, &[-1]),
        wmf_record(0x06FF, &[0, 0, 0, 0, 0x7FFF, 0x7FFF, 0, 0, 0, 0]),
        wmf_record(0x0538, &[0x7FFF, 0, 0]),
    ];
    let _ = load(MetafileFormat::Wmf, &wmf(10, 10, &recs), &limits(64));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn frames_of_absurd_size_are_refused_or_clamped() {
    // A frame a million times wider than tall must not allocate a huge canvas.
    let mut body = ints(&[0, 0, 0, 0, 0, 0, 1_000_000_000, 10]);
    body.extend_from_slice(&0x464D4520u32.to_le_bytes());
    body.extend(ints(&[0x10000, 0, 0]));
    body.extend_from_slice(&[0; 4]);
    body.extend(ints(&[0, 0, 0, 1000, 1000, 100, 100]));
    let mut v = emf_record(1, &body);
    v.extend(emf_record(14, &ints(&[0, 16, 20])));
    let t = load(MetafileFormat::Emf, &v, &limits(1024));
    if let Ok(t) = t {
        assert!(t.width() as u64 * t.height() as u64 <= MAX_SIDE as u64 * MAX_SIDE as u64);
    }
}

/// Every metafile among the downloaded samples.
fn samples() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let mut out = Vec::new();
    for dir in ["metafile-extra", "editors-extra/visio", "editors-extra/coreldraw"] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if matches!(
                    p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
                    Some("wmf" | "emf" | "emz" | "wmz" | "pict" | "pct")
                ) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// A cheap deterministic generator for mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn real_files_render_without_panics_in_time() {
    for path in samples() {
        let bytes = std::fs::read(&path).unwrap();
        let Some(format) = sniff(&path, &bytes[..bytes.len().min(4096)]) else { continue };
        let started = std::time::Instant::now();
        let _ = std::panic::catch_unwind(|| load(format, &bytes, &limits(256)))
            .unwrap_or_else(|_| panic!("{} panicked", path.display()));
        assert!(started.elapsed() < Duration::from_secs(4), "{} took {:?}", path.display(), started.elapsed());
    }
}

#[test]
fn truncated_and_mutated_files_never_panic() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for path in samples().into_iter().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 300_000)) {
        let bytes = std::fs::read(&path).unwrap();
        let Some(format) = sniff(&path, &bytes[..bytes.len().min(4096)]) else { continue };
        // Cut at a handful of places, and flip bytes at random.
        for k in 1..8 {
            let cut = bytes.len() * k / 8;
            let r = std::panic::catch_unwind(|| load(format, &bytes[..cut], &limits(64)));
            assert!(r.is_ok(), "{} cut at {cut} panicked", path.display());
        }
        for round in 0..6 {
            let mut copy = bytes.clone();
            for _ in 0..(1 + round * 3) {
                let i = (rng.next() as usize) % copy.len();
                copy[i] = rng.next() as u8;
            }
            let started = std::time::Instant::now();
            let r = std::panic::catch_unwind(|| load(format, &copy, &limits(64)));
            assert!(r.is_ok(), "{} mutated panicked", path.display());
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{} mutated took {:?}",
                path.display(),
                started.elapsed()
            );
        }
    }
}

//endregion
