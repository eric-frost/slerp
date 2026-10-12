//! KTX2, KTX1, DDS and raw ASTC files built byte by byte, with blocks whose
//! decoded values are known, plus the layout rules: orientation, cubemaps,
//! arrays, volumes, mip choice and supercompression.

mod common;

use common::*;
use slerp_textures::{ColorSpace, DecodeOptions, Texture, TextureKind, decode, thumbnail};

/*┌─────────────────╮
| KTX2            |
└─────────────────╯*/
//region KTX2

#[test]
fn ktx2_rgba8_orientation() {
    let texels = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 128];
    let mut k = Ktx2::new(37, 2, 2, texels.clone());
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(rgba8(&tex, 0, 0), texels.as_slice());
    assert!(tex.has_alpha);
    assert_eq!(tex.color_space, ColorSpace::Linear);
    assert_eq!(tex.source.format, "R8G8B8A8_UNORM");
    // "ru": rows run bottom to top, so they come back swapped.
    k.kv.push(("KTXorientation", b"ru\0"));
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..8], &texels[8..]);
}

#[test]
fn ktx2_srgb_and_swizzle() {
    let mut k = Ktx2::new(15, 2, 1, vec![10, 200]);
    k.kv.push(("KTXswizzle", b"rrr1\0"));
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(tex.color_space, ColorSpace::Srgb);
    assert_eq!(rgba8(&tex, 0, 0), &[10, 10, 10, 255, 200, 200, 200, 255]);
}

#[test]
fn ktx2_bc1_known_colours() {
    // Endpoint 0 red, endpoint 1 blue; texel 0 uses index 1, the rest 0.
    let tex = decode(&Ktx2::new(131, 4, 4, bc1(0xf800, 0x001f, 1)).build(), &DecodeOptions::full()).unwrap();
    let v = rgba8(&tex, 0, 0);
    assert_eq!(&v[..8], &[0, 0, 255, 255, 255, 0, 0, 255]);
    assert!(!tex.has_alpha);
}

#[test]
fn ktx2_odd_size_crops_blocks() {
    // 5x3 BC1 needs 2x1 blocks; the texels outside the image are dropped.
    let data = [bc1(0xf800, 0, 0), bc1(0x07e0, 0, 0)].concat();
    let tex = decode(&Ktx2::new(131, 5, 3, data).build(), &DecodeOptions::full()).unwrap();
    let v = rgba8(&tex, 0, 0);
    assert_eq!(v.len(), 5 * 3 * 4);
    assert_eq!(&v[3 * 4..5 * 4], &[255, 0, 0, 255, 0, 255, 0, 255]);
}

#[test]
fn ktx2_bc4_bc5_channels() {
    // BC4: endpoints 200 and 100, every index 0.
    let bc4 = vec![200, 100, 0, 0, 0, 0, 0, 0];
    let tex = decode(&Ktx2::new(139, 4, 4, bc4.clone()).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[200, 0, 0, 255]);
    assert_eq!(tex.source.format, "BC4_UNORM_BLOCK (R)");
    let thumb = thumbnail(&tex, 4);
    assert_eq!(&thumb.pixels[..4], &[200, 200, 200, 255], "one channel shows as grey");

    // BC5 holding a flat normal (128, 128): blue is rebuilt as 255.
    let bc5 = [vec![128, 128, 0, 0, 0, 0, 0, 0], vec![128, 128, 0, 0, 0, 0, 0, 0]].concat();
    let tex = decode(&Ktx2::new(141, 4, 4, bc5).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[128, 128, 0, 255]);
    let thumb = thumbnail(&tex, 4);
    assert_eq!(&thumb.pixels[..4], &[128, 128, 255, 255], "two channels show as a normal map");
}

#[test]
fn ktx2_bc7_mode6() {
    // Mode 6: endpoints are seven bits plus a p-bit.  Red runs from 255 to 0;
    // green, blue and alpha stay 255.  Texel 0 uses index 0 (255), texel 1
    // index 15 (0).
    let mut bits = vec![0u8; 16];
    let mut at = 0;
    let mut put = |v: u32, n: u32| {
        for i in 0..n {
            if v >> i & 1 != 0 {
                bits[(at / 8) as usize] |= 1 << (at % 8);
            }
            at += 1;
        }
    };
    put(1 << 6, 7);
    for (e0, e1) in [(127, 0), (127, 127), (127, 127), (127, 127)] {
        put(e0, 7);
        put(e1, 7);
    }
    put(1, 1);
    put(0, 1);
    put(0, 3); // texel 0's index has an implied leading zero
    put(15, 4);
    let tex = decode(&Ktx2::new(145, 4, 4, bits).build(), &DecodeOptions::full()).unwrap();
    let v = rgba8(&tex, 0, 0);
    assert_eq!(&v[..4], &[255, 255, 255, 255]);
    assert_eq!(v[4], 0);
}

#[test]
fn ktx2_etc1() {
    // Differential mode, base 16 (132 in 8 bits), table 0, all indices 0:
    // every texel is 132 + 2.
    let block = vec![16 << 3, 16 << 3, 16 << 3, 0b10, 0, 0, 0, 0];
    let tex = decode(&Ktx2::new(147, 4, 4, block).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[134, 134, 134, 255]);
}

#[test]
fn ktx2_astc_ldr_and_hdr() {
    let ldr = astc_solid([0x1234, 0x8000, 0xffff, 0xff00]);
    let tex = decode(&Ktx2::new(157, 4, 4, ldr.clone()).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[0x12, 0x80, 0xff, 0xff]);

    // HDR solid colour: halves 2.0, 0.5, 1.0, 1.0.
    let hdr = astc_solid_hdr([0x4000, 0x3800, 0x3c00, 0x3c00]);
    let tex = decode(&Ktx2::new(1000066000, 4, 4, hdr.clone()).build(), &DecodeOptions::full()).unwrap();
    assert!(tex.is_hdr());
    assert_eq!(&rgba32f(&tex)[..4], &[2.0, 0.5, 1.0, 1.0]);
    // An UNORM format holding HDR blocks still decodes as HDR.
    let tex = decode(&Ktx2::new(157, 4, 4, hdr).build(), &DecodeOptions::full()).unwrap();
    assert!(tex.is_hdr());
}

#[test]
fn ktx2_float_formats() {
    // R16_SFLOAT 1.5, B10G11R11 (1.0, 1.0, 1.0), E5B9G9R9 (0.5, 0.5, 0.5).
    let tex = decode(&Ktx2::new(76, 1, 1, 0x3e00u16.to_le_bytes().to_vec()).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba32f(&tex)[..4], &[1.5, 0.0, 0.0, 1.0]);
    let one11 = 15u32 << 6;
    let packed = one11 | one11 << 11 | (15 << 5) << 22;
    let tex = decode(&Ktx2::new(122, 1, 1, packed.to_le_bytes().to_vec()).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba32f(&tex)[..4], &[1.0, 1.0, 1.0, 1.0]);
    let e5 = (15u32 << 27) | (256 << 18) | (256 << 9) | 256;
    let tex = decode(&Ktx2::new(123, 1, 1, e5.to_le_bytes().to_vec()).build(), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba32f(&tex)[..3], &[0.5, 0.5, 0.5]);
}

#[test]
fn ktx2_supercompression() {
    let texels: Vec<u8> = (0..64u8).collect();
    let mut k = Ktx2::new(37, 4, 4, vec![]);
    k.scheme = 2;
    k.levels = vec![(zstd::bulk::compress(&texels, 3).unwrap(), 64)];
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(rgba8(&tex, 0, 0), texels.as_slice());
    assert_eq!(tex.source.supercompression.as_deref(), Some("zstd"));

    k.scheme = 3;
    k.levels = vec![(miniz_oxide::deflate::compress_to_vec_zlib(&texels, 6), 64)];
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(rgba8(&tex, 0, 0), texels.as_slice());

    // A level that claims to unpack to more than it should is refused.
    k.levels = vec![(miniz_oxide::deflate::compress_to_vec_zlib(&texels, 6), 1 << 40)];
    assert!(decode(&k.build(), &DecodeOptions::full()).is_err());
}

#[test]
fn ktx2_mips_and_preview() {
    let level = |size: u32, v: u8| vec![v; (size * size * 4) as usize];
    let mut k = Ktx2::new(37, 8, 8, vec![]);
    k.levels = [8, 4, 2, 1].iter().enumerate().map(|(i, &s)| (level(s, i as u8 * 10), (s * s * 4) as u64)).collect();
    let file = k.build();
    let tex = decode(&file, &DecodeOptions::full()).unwrap();
    assert_eq!(tex.levels.len(), 4);
    assert_eq!(tex.source.mip_count, 4);
    // The smallest level still at least 3 wide is the 4x4 one.
    let tex = decode(&file, &DecodeOptions::preview(3)).unwrap();
    assert_eq!((tex.width(), tex.levels.len()), (3, 1), "4x4 level, then shrunk to 3");
    let tex = decode(&file, &DecodeOptions::preview(4)).unwrap();
    assert_eq!(rgba8(&tex, 0, 0)[0], 10);
    assert_eq!(tex.source.width, 8);
}

#[test]
fn ktx2_cubemap_and_cross() {
    let colours: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 255, 255],
    ];
    let mut k = Ktx2::new(37, 1, 1, colours.concat());
    k.faces = 6;
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!(tex.kind, TextureKind::Cube);
    assert_eq!(tex.levels[0].surfaces.len(), 6);
    let t = thumbnail(&tex, 8);
    assert_eq!((t.width, t.height), (8, 6));
    let at = |x: u32, y: u32| &t.pixels[((y * 8 + x) * 4) as usize..((y * 8 + x) * 4 + 4) as usize];
    // Faces are 2x2 here; +Y sits above +Z, -X left of it, -Z far right.
    assert_eq!(at(2, 0), &colours[2]);
    assert_eq!(at(0, 2), &colours[1]);
    assert_eq!(at(2, 2), &colours[4]);
    assert_eq!(at(4, 2), &colours[0]);
    assert_eq!(at(6, 2), &colours[5]);
    assert_eq!(at(2, 4), &colours[3]);
    assert_eq!(at(0, 0)[3], 0, "outside the cross is transparent");
}

#[test]
fn ktx2_array_and_volume() {
    let mut k = Ktx2::new(37, 1, 1, [[1, 1, 1, 255], [2, 2, 2, 255], [3, 3, 3, 255]].concat());
    k.layers = 3;
    let file = k.build();
    let tex = decode(&file, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.layers, tex.levels[0].surfaces.len()), (3, 3));
    assert_eq!(rgba8(&tex, 0, 2)[0], 3);
    let tex = decode(&file, &DecodeOptions::preview(1)).unwrap();
    assert_eq!((tex.layers, tex.source.layers), (1, 3));

    let mut k = Ktx2::new(37, 1, 1, [[9, 9, 9, 255], [7, 7, 7, 255]].concat());
    k.depth = 2;
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.levels[0].depth), (TextureKind::D3, 2));
    assert_eq!(rgba8(&tex, 0, 0), &[9, 9, 9, 255, 7, 7, 7, 255]);
    assert_eq!(&thumbnail(&tex, 1).pixels, &[9, 9, 9, 255], "first slice");
}

#[test]
fn ktx2_rejects_bad_headers() {
    let good = Ktx2::new(37, 2, 2, vec![0; 16]);
    let mut k = Ktx2::new(37, 2, 2, vec![0; 15]);
    assert!(decode(&k.build(), &DecodeOptions::full()).is_err(), "short level");
    k = Ktx2::new(999_999, 2, 2, vec![0; 16]);
    assert!(decode(&k.build(), &DecodeOptions::full()).is_err(), "unknown format");
    let mut f = good.build();
    f[12 + 8..12 + 12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    assert!(decode(&f, &DecodeOptions::full()).is_err(), "huge width");
    let mut f = good.build();
    f[12 + 24..12 + 28].copy_from_slice(&5u32.to_le_bytes());
    assert!(decode(&f, &DecodeOptions::full()).is_err(), "five faces");
}

//endregion

/*┌─────────────────╮
| KTX1            |
└─────────────────╯*/
//region KTX1

#[test]
fn ktx1_rgb_rows_are_padded() {
    // 3x2 RGB8: each 9-byte row is padded to 12.
    let rows = [[1u8, 2, 3, 4, 5, 6, 7, 8, 9, 0, 0, 0], [10, 11, 12, 13, 14, 15, 16, 17, 18, 0, 0, 0]].concat();
    let f = ktx1([0x1401, 0x1907, 0x8051, 0x1907], 3, 2, 1, &[], &[rows]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(tex.source.format, "GL_RGB8");
    assert_eq!(&rgba8(&tex, 0, 0)[12..16], &[10, 11, 12, 255]);
}

#[test]
fn ktx1_compressed_cube_and_orientation() {
    let faces: Vec<Vec<u8>> = (0..6u16).map(|i| bc1(0xf800 >> i, 0, 0)).collect();
    let f = ktx1([0, 0, 0x83f0, 0x1907], 4, 4, 6, &[], &faces);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.levels[0].surfaces.len()), (TextureKind::Cube, 6));

    let mut kv = Vec::new();
    let entry = b"KTXorientation\0S=r,T=u\0";
    kv.extend((entry.len() as u32).to_le_bytes());
    kv.extend(entry);
    kv.resize(kv.len().div_ceil(4) * 4, 0);
    let img = [vec![1, 1, 1, 255], vec![2, 2, 2, 255]].concat();
    let f = ktx1([0x1401, 0x1908, 0x8058, 0x1908], 1, 2, 1, &kv, &[img]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(rgba8(&tex, 0, 0)[0], 2, "T=u: bottom row first");
}

//endregion

#[test]
fn ktx1_pvrtc_and_dds_atc_decode() {
    // PVRTC1 4 bpp needs at least 2x2 blocks; all-zero data is a valid
    // black image.
    let f = ktx1([0, 0, 0x8c00, 0x1907], 8, 8, 1, &[], &[vec![0; 32]]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.width(), tex.height(), rgba8(&tex, 0, 0).len()), (8, 8, 256));
    // A 2x2 PVRTC texture still reads the two-by-two block minimum.
    let f = ktx1([0, 0, 0x8c00, 0x1907], 2, 2, 1, &[], &[vec![0; 32]]);
    assert_eq!(decode(&f, &DecodeOptions::full()).unwrap().width(), 2);

    // ATC: endpoint 0 is RGB555-ish white in its first word.
    let mut f = dds(4, 4, 1, (FOURCC, b"ATC ", 0, [0; 4]), 0, None);
    f.extend([0xff, 0x7f, 0xff, 0xff, 0, 0, 0, 0]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(tex.source.format, "ATC");
    assert_eq!(rgba8(&tex, 0, 0).len(), 64);
}

/*┌─────────────────╮
| DDS             |
└─────────────────╯*/
//region DDS

#[test]
fn dds_legacy_dxt1_and_masks() {
    let mut f = dds(4, 4, 1, (FOURCC, b"DXT1", 0, [0; 4]), 0, None);
    f.extend(bc1(0x07e0, 0, 0));
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[0, 255, 0, 255]);

    // A8R8G8B8 by masks: memory order B G R A.
    let mut f = dds(1, 1, 1, (0x41, &[0; 4], 32, [0xff0000, 0xff00, 0xff, 0xff00_0000]), 0, None);
    f.extend([30, 20, 10, 40]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(rgba8(&tex, 0, 0), &[10, 20, 30, 40]);
}

#[test]
fn dds_wrong_mip_count_is_forgiven() {
    // Claims 5 mips but holds one.
    let mut f = dds(4, 4, 5, (FOURCC, b"DXT1", 0, [0; 4]), 0, None);
    f.extend(bc1(0xf800, 0, 0));
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(tex.levels.len(), 1);
}

#[test]
fn dds_dx10_array_cube_and_bc6h() {
    // A cube array of two cubes, BC1, one 4x4 mip each.
    let mut f = dds(4, 4, 1, (FOURCC, b"DX10", 0, [0; 4]), 0, Some((71, 2, 4)));
    for i in 0..12u16 {
        f.extend(bc1(i, 0, 0));
    }
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.layers, tex.levels[0].surfaces.len()), (TextureKind::Cube, 2, 12));

    // BC6H mode 11 (0x03) with all-zero endpoints decodes to black floats.
    let mut f = dds(4, 4, 1, (FOURCC, b"DX10", 0, [0; 4]), 0, Some((95, 1, 0)));
    f.extend([0x03; 1]);
    f.extend([0; 15]);
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert!(tex.is_hdr());
    assert_eq!(&rgba32f(&tex)[..4], &[0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn dds_legacy_cubemap_and_volume() {
    let mut f = dds(1, 1, 1, (0x41, &[0; 4], 32, [0xff0000, 0xff00, 0xff, 0xff00_0000]), 0xfe00, None);
    for i in 0..6u8 {
        f.extend([i, i, i, 255]);
    }
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!(tex.kind, TextureKind::Cube);
    assert_eq!(rgba8(&tex, 0, 5)[0], 5);

    let mut f = dds(1, 1, 1, (0x41, &[0; 4], 32, [0xff0000, 0xff00, 0xff, 0xff00_0000]), 0x20_0000, None);
    f[24..28].copy_from_slice(&3u32.to_le_bytes());
    f.extend([[1, 1, 1, 255], [2, 2, 2, 255], [3, 3, 3, 255]].concat());
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.levels[0].depth), (TextureKind::D3, 3));
}

//endregion

/*┌─────────────────╮
| Raw ASTC        |
└─────────────────╯*/
//region Raw ASTC

#[test]
fn astc_raw_files() {
    let blocks = [astc_solid([0xff00, 0, 0, 0xffff]), astc_solid([0, 0xff00, 0, 0xffff])].concat();
    let tex = decode(&astc_file(6, 6, 1, 7, 3, &blocks), &DecodeOptions::full()).unwrap();
    assert_eq!((tex.width(), tex.height()), (7, 3));
    let v = rgba8(&tex, 0, 0);
    assert_eq!(&v[20..28], &[255, 0, 0, 255, 0, 255, 0, 255]);
    assert_eq!(tex.source.format, "ASTC 6x6");

    let hdr = astc_solid_hdr([0x4400, 0x4400, 0x4400, 0x3c00]);
    let tex = decode(&astc_file(4, 4, 1, 4, 4, &hdr), &DecodeOptions::full()).unwrap();
    assert_eq!(rgba32f(&tex)[0], 4.0);
    assert_eq!(tex.source.format, "ASTC 4x4 HDR");

    assert!(decode(&astc_file(7, 7, 1, 7, 7, &[0; 16]), &DecodeOptions::full()).is_err(), "no 7x7 footprint");
    assert!(decode(&astc_file(3, 3, 2, 3, 3, &[0; 16]), &DecodeOptions::full()).is_err(), "no 3x3x2 footprint");
    assert!(decode(&astc_file(4, 4, 1, 8, 8, &[0; 16]), &DecodeOptions::full()).is_err(), "too few blocks");
}

#[test]
fn astc_3d_raw_and_ktx() {
    // Two 4x4x4 blocks stacked in z: a red volume 3 deep over a green one.
    let blocks = [astc_solid([0xff00, 0, 0, 0xffff]), astc_solid([0, 0xff00, 0, 0xffff])].concat();
    let mut file = astc_file(4, 4, 4, 3, 2, &blocks);
    file[13..16].copy_from_slice(&[6, 0, 0]);
    let tex = decode(&file, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.width(), tex.height(), tex.levels[0].depth), (TextureKind::D3, 3, 2, 6));
    assert_eq!(tex.source.format, "ASTC 4x4x4");
    let v = rgba8(&tex, 0, 0);
    assert_eq!(v.len(), 3 * 2 * 6 * 4);
    assert_eq!(&v[..4], &[255, 0, 0, 255], "slice 0");
    assert_eq!(&v[3 * 2 * 3 * 4..][..4], &[255, 0, 0, 255], "slice 3 is still the first block");
    assert_eq!(&v[3 * 2 * 4 * 4..][..4], &[0, 255, 0, 255], "slice 4 starts the second");
    file.truncate(16 + 16);
    assert!(decode(&file, &DecodeOptions::full()).is_err(), "one block for two slices of blocks");

    // KTX2 with VK_FORMAT_ASTC_3x3x3_SFLOAT_BLOCK_EXT and a mip: 4x4x4 needs
    // 2x2x2 blocks, its 2x2x2 mip one.
    let hdr = astc_solid_hdr([0x4400, 0x3c00, 0x3800, 0x3c00]);
    let mut k = Ktx2::new(1000288002, 4, 4, hdr.repeat(8));
    k.depth = 4;
    k.levels.push((hdr.clone(), 16));
    let tex = decode(&k.build(), &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.levels.len(), tex.levels[1].depth), (TextureKind::D3, 2, 2));
    assert_eq!(&rgba32f(&tex)[..4], &[4.0, 1.0, 0.5, 1.0]);
    assert_eq!(tex.source.format, "ASTC_3x3x3_SFLOAT_BLOCK_EXT");

    // KTX1 with GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x6x6_OES.
    let mut file = ktx1([0, 0, 0x93E9, 0x1908], 6, 6, 1, &[], &[astc_solid([0x8000, 0x4000, 0x2000, 0xffff])]);
    file[0x2C..0x30].copy_from_slice(&3u32.to_le_bytes());
    let tex = decode(&file, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.kind, tex.levels[0].depth, tex.color_space), (TextureKind::D3, 3, ColorSpace::Srgb));
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[0x80, 0x40, 0x20, 255]);
}

#[test]
fn astc_malformed_block_is_magenta() {
    // An all-zero block uses a reserved encoding.
    let tex = decode(&astc_file(4, 4, 1, 4, 4, &[0; 16]), &DecodeOptions::full()).unwrap();
    assert_eq!(&rgba8(&tex, 0, 0)[..4], &[255, 0, 255, 255]);
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

#[test]
fn thumbnails_tone_map_hdr_and_drop_alpha() {
    // A float texture of middle grey at any brightness lands on the same
    // exposed grey.
    let shades: Vec<f32> = [0.01f32, 100.0].iter().flat_map(|&v| [v, v, v, 0.5]).collect();
    let dim = Texture::from_rgba32f(1, 1, shades[..4].to_vec());
    let bright = Texture::from_rgba32f(1, 1, shades[4..].to_vec());
    let (a, b) = (thumbnail(&dim, 4), thumbnail(&bright, 4));
    assert_eq!((a.width, a.height), (4, 4));
    assert_eq!(a.pixels, b.pixels);
    assert_eq!(a.pixels[3], 255);
    assert!((100..140).contains(&a.pixels[0]), "{}", a.pixels[0]);

    // Small textures grow by whole factors with hard edges.
    let t = Texture::from_rgba8(2, 1, vec![0, 0, 0, 0, 255, 255, 255, 255], ColorSpace::Srgb);
    let th = thumbnail(&t, 9);
    assert_eq!((th.width, th.height), (8, 4));
    assert_eq!(&th.pixels[3 * 4..4 * 4], &[0, 0, 0, 255]);
    assert_eq!(&th.pixels[4 * 4..5 * 4], &[255, 255, 255, 255]);
}

//endregion

/*┌─────────────────╮
| Basis 2.50      |
└─────────────────╯*/
//region Basis 2.50

/// Files from Basis Universal 2.50's encoder in `local/assets/textures-extra`
/// (when present) against the encoder's own unpacked images: XUASTC to the
/// ASTC blocks it carries, XUBC7 to its BC7 blocks.
#[test]
fn basisu_250_codecs_match_reference() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/textures-extra/basisu-2.50");
    for (name, tolerance) in [("ldr_6x6i", 0), ("ldr_12x12i", 0), ("xubc7", 0), ("etc1s", 0), ("uastc", 1)] {
        let Ok(file) = std::fs::read(dir.join(format!("{name}.ktx2"))) else { return };
        let reference =
            decode(&std::fs::read(dir.join(format!("ref/{name}.png"))).unwrap(), &DecodeOptions::full()).unwrap();
        let ours = decode(&file, &DecodeOptions::full()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (a, b) = (rgba8(&ours, 0, 0), rgba8(&reference, 0, 0));
        assert_eq!(a.len(), b.len(), "{name}");
        // Opaque references are stored without alpha, so compare colour.
        let worst =
            a.chunks(4).zip(b.chunks(4)).flat_map(|(p, q)| (0..3).map(move |c| p[c].abs_diff(q[c]))).max().unwrap();
        assert!(worst <= tolerance, "{name}: off by {worst}");
    }
    for name in ["hdr_4x4", "hdr_6x6", "hdr_6x6i"] {
        let Ok(file) = std::fs::read(dir.join(format!("{name}.ktx2"))) else { return };
        let tex = decode(&file, &DecodeOptions::full()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(tex.is_hdr(), "{name}");
        assert!(rgba32f(&tex).iter().all(|v| v.is_finite()), "{name}");
    }
}

//endregion

/// Vulkan's 4:2:2 formats are YCbCr when the descriptor's colour model says
/// so, as video files do, in every bit depth and component order: a pair of
/// BT.601 studio-range red texels comes out red.  With the RGB model they
/// stay RGB with shared red and blue.
#[test]
fn ktx2_422_ycbcr() {
    // Red: Y 81, Cb 90, Cr 240 in 8 bits; deeper samples sit at the top of
    // 16 bits, so each is the 8-bit value shifted up by 8.
    let (y, cb, cr) = (81u16, 90u16, 240u16);
    let formats = [
        (1000156000, true, false),
        (1000156001, false, false),
        (1000156010, true, true),
        (1000156011, false, true),
        (1000156020, true, true),
        (1000156021, false, true),
        (1000156027, true, true),
        (1000156028, false, true),
    ];
    for (vk, g_first, wide) in formats {
        let order = if g_first { [y, cb, y, cr] } else { [cb, y, cr, y] };
        let data: Vec<u8> =
            order.iter().flat_map(|&v| if wide { (v << 8).to_le_bytes().to_vec() } else { vec![v as u8] }).collect();
        let mut file = Ktx2::new(vk, 2, 1, data);
        file.model = 2;
        let tex = slerp_textures::decode(&file.build(), &DecodeOptions::full()).unwrap();
        assert!(tex.source.format.contains("(YCbCr)"), "{vk}: {}", tex.source.format);
        for texel in rgba8(&tex, 0, 0).chunks_exact(4) {
            let red = [255u8, 0, 0];
            assert!(texel[..3].iter().zip(red).all(|(a, b)| a.abs_diff(b) <= 2), "{vk}: {texel:?}");
        }
        file.model = 1;
        let tex = slerp_textures::decode(&file.build(), &DecodeOptions::full()).unwrap();
        assert!(!tex.source.format.contains("YCbCr"), "{vk}: {}", tex.source.format);
    }
}
