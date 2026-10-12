//! Tiny creative-app documents built here: iWork and Pixelmator Pro zips,
//! Paint Shop Pro files of several versions, depths and compressions, and
//! an encrypted PaintTool SAI 1 document.  SAI 2's thumbnails and canvases
//! are checked against real files in `tests.rs`, since building their JPEG
//! and delta streams here would only test the builder.

use super::*;
use crate::synthetic_cad::{load, png, refused, shown, sniffed, survives_damage};
use crate::zip::tests::stored_zip;

/*┌─────────────────╮
| iWork, Pixelmator |
└─────────────────╯*/
//region iWork, Pixelmator

#[test]
fn iwork_documents_of_each_generation() {
    let big = png(8, 6, [1, 2, 3, 255]);
    let small = png(2, 2, [4, 5, 6, 255]);
    // iWork 2013 and later.
    let modern = stored_zip(&[("Index/Document.iwa", b"x"), ("preview-micro.jpg", &small), ("preview.jpg", &big)]);
    for name in ["a.key", "a.pages", "a.numbers"] {
        assert_eq!(sniffed(name, &modern), Some(PreviewFormat::Iwork), "{name}");
    }
    assert_eq!(shown(PreviewFormat::Iwork, modern).0, (8, 6));
    // iWork '09.
    let old = stored_zip(&[("index.apxl", b"<x/>"), ("QuickLook/Thumbnail.jpg", &small)]);
    assert_eq!(shown(PreviewFormat::Iwork, old).0, (2, 2));
    // A package folder zipped by Finder.
    let zipped = stored_zip(&[("Talk.key/Index.zip", b"x"), ("Talk.key/preview.jpg", &big)]);
    assert_eq!(shown(PreviewFormat::Iwork, zipped.clone()).0, (8, 6));
    survives_damage(PreviewFormat::Iwork, &zipped);
    // Without a picture; and a TLS key named .key isn't a zip.
    let none = stored_zip(&[("index.xml", b"<x/>")]);
    assert!(matches!(load(PreviewFormat::Iwork, none, &Limits::viewer()), Err(LoadError::Unsupported(_))));
    assert_eq!(sniffed("server.key", b"-----BEGIN PRIVATE KEY-----\nMIIB\n"), None);
}

#[test]
fn pixelmator_documents_show_their_quicklook_thumbnail() {
    let p = png(3, 3, [9, 8, 7, 255]);
    let bytes = stored_zip(&[
        ("metadata.info", b"SQLite format 3\0"),
        ("QuickLook/Thumbnail.webp", &p),
        ("QuickLook/Icon.webp", b"x"),
    ]);
    assert_eq!(sniffed("art.pxd", &bytes), Some(PreviewFormat::Pixelmator));
    assert_eq!(shown(PreviewFormat::Pixelmator, bytes).0, (3, 3));
    // Cython declaration files are text.
    assert_eq!(sniffed("numpy.pxd", b"cdef extern from \"numpy/arrayobject.h\":\n"), None);
}

//endregion

/*┌─────────────────╮
| Paint Shop Pro  |
└─────────────────╯*/
//region Paint Shop Pro

/// A block of major version 4 or later: `~BK\0`, id, length, body.
fn block(id: u16, body: &[u8]) -> Vec<u8> {
    let mut b = b"~BK\0".to_vec();
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&(body.len() as u32).to_le_bytes());
    b.extend_from_slice(body);
    b
}

/// A chunk that starts with its own size.
fn chunk(fields: &[u8]) -> Vec<u8> {
    let mut c = ((fields.len() + 4) as u32).to_le_bytes().to_vec();
    c.extend_from_slice(fields);
    c
}

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

/// The general image attributes: size, compression, depth, layers.
fn psp_image(w: u32, h: u32, compression: u16, bits: u16, layers: u16) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&le32(w));
    f.extend_from_slice(&le32(h));
    f.extend_from_slice(&72f64.to_le_bytes());
    f.push(1);
    f.extend_from_slice(&le16(compression));
    f.extend_from_slice(&le16(bits));
    f.extend_from_slice(&le16(1));
    f.extend_from_slice(&le32(1 << bits.min(24)));
    f.push(0);
    f.extend_from_slice(&le32(0));
    f.extend_from_slice(&le32(0));
    f.extend_from_slice(&le16(layers));
    f.extend_from_slice(&le32(0));
    block(0, &chunk(&f))
}

/// One channel: its data compressed as `compression` says.
fn channel(bitmap: u16, kind: u16, raw: &[u8], compression: u16) -> Vec<u8> {
    let data = match compression {
        1 => raw.chunks(100).flat_map(|c| [&[c.len() as u8][..], c].concat()).collect(),
        2 => miniz_oxide::deflate::compress_to_vec_zlib(raw, 6),
        _ => raw.to_vec(),
    };
    let mut f = Vec::new();
    f.extend_from_slice(&le32(data.len() as u32));
    f.extend_from_slice(&le32(raw.len() as u32));
    f.extend_from_slice(&le16(bitmap));
    f.extend_from_slice(&le16(kind));
    let mut body = chunk(&f);
    body.extend(data);
    block(5, &body)
}

/// A file: signature, version, then the blocks.
fn psp(major: u16, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut f = crate::psp::SIGNATURE.to_vec();
    f.extend_from_slice(&le16(major));
    f.extend_from_slice(&le16(0));
    for b in blocks {
        f.extend_from_slice(b);
    }
    f
}

/// A picture of a composite bank: thumbnail?, width, height, depth,
/// compression, and its palette and channel blocks.
type BankPicture = (bool, u32, u32, u16, u16, Vec<u8>);

/// A composite bank of these pictures.
fn bank(pictures: &[BankPicture]) -> Vec<u8> {
    let mut body = chunk(&le32(pictures.len() as u32));
    for (thumb, w, h, bits, compression, _) in pictures {
        let mut f = Vec::new();
        f.extend_from_slice(&le32(*w));
        f.extend_from_slice(&le32(*h));
        f.extend_from_slice(&le16(*bits));
        f.extend_from_slice(&le16(*compression));
        f.extend_from_slice(&le16(1));
        f.extend_from_slice(&le32(1 << (*bits).min(24)));
        f.extend_from_slice(&le16(*thumb as u16));
        body.extend(block(17, &chunk(&f)));
    }
    for (.., inner) in pictures {
        let mut b = chunk(&[le16(1), le16(3)].concat());
        b.extend_from_slice(inner);
        body.extend(block(9, &b));
    }
    block(16, &body)
}

/// Three colour channels of one colour each, `w` x `h`.
fn rgb_channels(w: u32, h: u32, rgb: [u8; 3], bitmap: u16, compression: u16) -> Vec<u8> {
    let n = (w * h) as usize;
    (0..3).flat_map(|c| channel(bitmap, c as u16 + 1, &vec![rgb[c]; n], compression)).collect()
}

#[test]
fn psp_composites_and_thumbnails() {
    for compression in [0, 1, 2] {
        let thumb = rgb_channels(4, 2, [10, 20, 30], 5, compression);
        let mut full = rgb_channels(8, 4, [200, 100, 50], 8, compression);
        // Half transparent.
        full.extend(channel(9, 0, &[128u8; 32], compression));
        let bytes = psp(
            6,
            &[
                psp_image(8, 4, compression, 24, 2),
                bank(&[(true, 4, 2, 24, compression, thumb), (false, 8, 4, 24, compression, full)]),
            ],
        );
        assert_eq!(sniffed("a.pspimage", &bytes), Some(PreviewFormat::PaintShopPro));
        assert_eq!(sniffed("tube.tub", &bytes), Some(PreviewFormat::PaintShopPro));
        // The viewer shows the composite with its transparency.
        assert_eq!(shown(PreviewFormat::PaintShopPro, bytes.clone()), ((8, 4), [200, 100, 50, 128]), "{compression}");
        // A thumbnail of 4 or less takes the stored thumbnail.
        let mut small = Limits::thumbnail();
        small.max_texture_size = Some(4);
        match load(PreviewFormat::PaintShopPro, bytes.clone(), &small) {
            Ok(Stored::Image(t)) => assert_eq!((t.width(), t.height()), (4, 2)),
            _ => panic!("no thumbnail"),
        }
        survives_damage(PreviewFormat::PaintShopPro, &bytes);
    }
}

#[test]
fn psp_paletted_and_single_layer_documents() {
    // An 8-bit composite with its own palette: index 1 is blue.
    let mut pal = chunk(&le32(2));
    pal.extend_from_slice(&[0, 0, 0, 0, 255, 0, 0, 0]);
    let mut inner = block(2, &pal);
    inner.extend(channel(8, 0, &[1u8; 4], 0));
    let bytes = psp(5, &[psp_image(2, 2, 0, 8, 2), bank(&[(false, 2, 2, 8, 0, inner)])]);
    assert_eq!(shown(PreviewFormat::PaintShopPro, bytes), ((2, 2), [0, 0, 255, 255]));
    // A single raster layer and only a thumbnail: the viewer reads the layer.
    let mut info = Vec::new();
    info.extend_from_slice(&le16(10));
    info.extend_from_slice(b"Background");
    info.push(1);
    for rect in [[0u32, 0, 3, 2], [0, 0, 3, 2]] {
        for v in rect {
            info.extend_from_slice(&le32(v));
        }
    }
    info.extend_from_slice(&[255, 0, 1, 0, 0]);
    let mut layer = chunk(&info);
    // An extension block newer versions write, then the bitmap chunk.
    layer.extend(block(33, &[0u8; 8]));
    layer.extend(chunk(&[le16(1), le16(3)].concat()));
    layer.extend(rgb_channels(3, 2, [7, 77, 177], 0, 2));
    let layers = block(3, &block(4, &layer));
    let thumb = rgb_channels(1, 1, [1, 1, 1], 5, 2);
    let bytes = psp(12, &[psp_image(3, 2, 2, 24, 1), bank(&[(true, 1, 1, 24, 2, thumb)]), layers]);
    assert_eq!(shown(PreviewFormat::PaintShopPro, bytes.clone()), ((3, 2), [7, 77, 177, 255]));
    survives_damage(PreviewFormat::PaintShopPro, &bytes);
}

#[test]
fn psp5_thumbnail_blocks() {
    // Paint Shop Pro 5: 14-byte headers with the first chunk's length, and
    // chunks without their own size.
    let v3 = |id: u16, first: &[u8], rest: &[u8]| {
        let mut b = b"~BK\0".to_vec();
        b.extend_from_slice(&le16(id));
        b.extend_from_slice(&le32(first.len() as u32));
        b.extend_from_slice(&le32((first.len() + rest.len()) as u32));
        b.extend_from_slice(first);
        b.extend_from_slice(rest);
        b
    };
    let mut attrs = Vec::new();
    attrs.extend_from_slice(&le32(2));
    attrs.extend_from_slice(&le32(1));
    attrs.extend_from_slice(&72f64.to_le_bytes());
    // Metric, compression, depth, planes, colours, grey, total, active, layers.
    attrs.extend_from_slice(&[1, 2, 0, 24, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
    let image = v3(0, &attrs, &[]);
    let channels: Vec<u8> = (0..3u16)
        .flat_map(|c| {
            let raw = vec![[250u8, 5, 9][c as usize]; 2];
            let mut f = Vec::new();
            f.extend_from_slice(&le32(2));
            f.extend_from_slice(&le32(2));
            f.extend_from_slice(&le16(5));
            f.extend_from_slice(&le16(c + 1));
            v3(5, &f, &raw)
        })
        .collect();
    let mut t = Vec::new();
    t.extend_from_slice(&le32(2));
    t.extend_from_slice(&le32(1));
    t.extend_from_slice(&le16(24));
    t.extend_from_slice(&le16(0));
    t.extend_from_slice(&le16(1));
    t.extend_from_slice(&le32(1 << 24));
    t.extend_from_slice(&le32(0));
    t.extend_from_slice(&le16(3));
    let thumb = v3(9, &t, &channels);
    let bytes = psp(3, &[image, thumb]);
    assert_eq!(shown(PreviewFormat::PaintShopPro, bytes.clone()), ((2, 1), [250, 5, 9, 255]));
    survives_damage(PreviewFormat::PaintShopPro, &bytes);
}

#[test]
fn hostile_psp_files_are_bounded() {
    // A composite of 2^31 pixels, a channel claiming 4 GB, a zlib bomb.
    refused(
        PreviewFormat::PaintShopPro,
        psp(
            6,
            &[
                psp_image(1 << 16, 1 << 15, 2, 24, 2),
                bank(&[(false, 1 << 16, 1 << 15, 24, 2, rgb_channels(1, 1, [1, 1, 1], 8, 2))]),
            ],
        ),
    );
    let mut huge = channel(8, 1, &[1u8; 4], 0);
    huge[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    refused(PreviewFormat::PaintShopPro, psp(6, &[psp_image(2, 2, 0, 24, 2), bank(&[(false, 2, 2, 24, 0, huge)])]));
    let bomb = channel(8, 1, &vec![0u8; 64 << 20], 2);
    let start = std::time::Instant::now();
    let bytes = psp(6, &[psp_image(2, 2, 2, 24, 2), bank(&[(false, 2, 2, 24, 2, bomb)])]);
    let _ = load(PreviewFormat::PaintShopPro, bytes, &Limits::viewer());
    assert!(start.elapsed().as_millis() < 1000, "{:?}", start.elapsed());
    // Text named .psp isn't claimed.
    assert_eq!(sniffed("Makefile.psp", b"all:\n\tcc -o x x.c\n"), None);
}

//endregion

/*┌─────────────────╮
| SAI 1           |
└─────────────────╯*/
//region SAI 1

fn key_sum(v: u32) -> u32 {
    let k = crate::sai::USER_KEY;
    k[(v >> 24) as usize]
        .wrapping_add(k[(v >> 16 & 255) as usize])
        .wrapping_add(k[(v >> 8 & 255) as usize])
        .wrapping_add(k[(v & 255) as usize])
}

fn checksum(words: &[u32]) -> u32 {
    words.iter().fold(0u32, |s, &w| s.rotate_left(1) ^ w) | 1
}

/// A SAI 1 document: a table page, an empty page 1, the root folder in page
/// 2 with one `thumbnail` entry, and the thumbnail's pages after it, all
/// encrypted as SAI does.
fn sai1(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
    let mut thumb = Vec::new();
    thumb.extend_from_slice(&le32(w));
    thumb.extend_from_slice(&le32(h));
    thumb.extend_from_slice(b"BM32");
    for _ in 0..w * h {
        thumb.extend_from_slice(&bgra);
    }
    let pages_needed = thumb.len().div_ceil(4096).max(1);
    let mut plain: Vec<Vec<u32>> = vec![vec![0; 1024]; 3 + pages_needed];
    // The root folder: flags, name, two pads, type, pad, first page, size.
    let mut root = vec![0u8; 4096];
    root[..4].copy_from_slice(&le32(1));
    root[4..13].copy_from_slice(b"thumbnail");
    root[38] = 0x80;
    root[40..44].copy_from_slice(&le32(3));
    root[44..48].copy_from_slice(&le32(thumb.len() as u32));
    plain[2] = root.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    for (i, piece) in thumb.chunks(4096).enumerate() {
        let mut p = piece.to_vec();
        p.resize(4096, 0);
        plain[3 + i] = p.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    }
    // The table: each data page's checksum and the next page of its file.
    let mut table = vec![0u32; 1024];
    for n in 1..plain.len() {
        table[n * 2] = checksum(&plain[n]);
        table[n * 2 + 1] = if n >= 3 && n + 1 < plain.len() { n as u32 + 1 } else { 0 };
    }
    table[0] = checksum(&table[1..]);
    let mut out = Vec::new();
    // Encrypting is decrypting backwards: each cipher word chains to the next.
    let mut prev = 0u32;
    for &p in &table {
        let c = p.rotate_right(16) ^ prev ^ key_sum(prev);
        out.extend_from_slice(&le32(c));
        prev = c;
    }
    for (n, page) in plain.iter().enumerate().skip(1) {
        let mut vector = table[n * 2];
        for &p in page {
            let c = p.wrapping_add(vector ^ key_sum(vector));
            out.extend_from_slice(&le32(c));
            vector = c;
        }
    }
    out
}

#[test]
fn sai1_thumbnails_decrypt() {
    let bytes = sai1(40, 30, [30, 20, 10, 255]);
    assert_eq!(sniffed("art.sai", &bytes), Some(PreviewFormat::Sai));
    assert_eq!(sniffed("anything", &bytes), Some(PreviewFormat::Sai));
    assert_eq!(shown(PreviewFormat::Sai, bytes.clone()), ((40, 30), [10, 20, 30, 255]));
    survives_damage(PreviewFormat::Sai, &bytes);
    // A file cut short fails.
    let mut odd = sai1(2, 2, [0, 0, 0, 255]);
    odd.truncate(odd.len() - 1);
    assert!(load(PreviewFormat::Sai, odd, &Limits::viewer()).is_err());
    // Random pages aren't SAI.
    let noise: Vec<u8> = (0..8192u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    assert_eq!(sniffed("noise.sai", &noise), None);
}

//endregion
