//! Damaged and hostile FITS, XISF, DPX, Cineon, PAM and farbfeld files must
//! give an error or a picture, never a panic, a hang or an allocation out of
//! proportion to the file.  Real samples are cut at many lengths and mutated
//! with a fixed-seed generator; hand-made headers claim absurd sizes and
//! compression ratios.

use slerp_textures::{DecodeOptions, Format, TextureError, decode_as, thumbnail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn sample(sub: &str, name: &str) -> Vec<u8> {
    // JPEG XR's samples live with its crate.
    let base = if sub == "jxr" { "../jxr/tests/data/files" } else { "tests/data" };
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(base).join(if sub == "jxr" { "" } else { sub }).join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Decodes both ways, failing on a caught panic or a slow decode.
fn survive(name: &str, format: Format, bytes: &[u8]) -> Result<(), TextureError> {
    let mut last = Ok(());
    for opts in [DecodeOptions::preview(16), DecodeOptions::full()] {
        let start = Instant::now();
        match decode_as(format, bytes, &opts) {
            Ok(tex) => {
                let t = thumbnail(&tex, 32);
                assert!(t.width > 0 && t.height > 0 && t.width <= 32 && t.height <= 32, "{name}: bad thumbnail");
                last = Ok(());
            }
            Err(TextureError::Invalid(m)) if m.contains("internal decoder error") => panic!("{name}: decoder panicked"),
            Err(e) => last = Err(e),
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{name}: took {:?}", start.elapsed());
    }
    last
}

fn torture(name: &str, format: Format, original: &[u8], seed: u64) {
    // 1. Truncations: every length for small files, a spread for larger.
    let step = (original.len() / 150).max(1);
    for len in (0..original.len()).step_by(step) {
        let _ = survive(&format!("{name} cut at {len}"), format, &original[..len]);
    }
    // 2. Random bytes, half of them in the header where sizes live.
    let mut rng = XorShift(seed);
    let iters = std::env::var("SLERP_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(250);
    let head = if format == Format::Fits { 5760 } else { 2300 };
    for i in 0..iters {
        let mut b = original.to_vec();
        for _ in 0..1 + rng.below(4) {
            let at = if i % 2 == 0 { rng.below(b.len().min(head)) } else { rng.below(b.len()) };
            if let Some(byte) = b.get_mut(at) {
                *byte = match rng.below(4) {
                    0 => 0,
                    1 => 0xff,
                    _ => rng.next() as u8,
                };
            }
        }
        let _ = survive(&format!("{name} mutation {i}"), format, &b);
    }
}

#[test]
fn fits_samples() {
    for (i, name) in [
        "u16.fits",
        "i16_blank.fits",
        "f64_nan.fits",
        "rgb_f32.fits",
        "bayer_rggb.fits",
        "two_images.fits",
        "rice_i16.fits",
        "rice_f32_dither1.fits",
        "gzip2_f32_lossless.fits",
        "gzip1_i16.fits",
        "plio_mask.fits",
        "hcomp_i16.fits",
        "hcomp_f32.fits",
        "hcomp_smooth8.fits",
    ]
    .into_iter()
    .enumerate()
    {
        torture(name, Format::Fits, &sample("fits", name), 1 + i as u64);
    }
}

#[test]
fn xisf_samples() {
    for (i, name) in [
        "u16_zlib_sh.xisf",
        "u16_lz4_sh.xisf",
        "u16_zstd_sh.xisf",
        "u16_be_planar_rgb.xisf",
        "cfa_rggb.xisf",
        "u8_embedded.xisf",
        "u16_zlib_subblocks.xisf",
    ]
    .into_iter()
    .enumerate()
    {
        torture(name, Format::Xisf, &sample("xisf", name), 100 + i as u64);
    }
}

#[test]
fn film_samples() {
    for (i, name) in [
        "dpx_10_be_a.dpx",
        "dpx_10_le_packed.dpx",
        "dpx_12_be_b.dpx",
        "dpx_16_rgba.dpx",
        "dpx_8_be.dpx",
        "dpx_10_fields.dpx",
    ]
    .into_iter()
    .enumerate()
    {
        torture(name, Format::Dpx, &sample("film", name), 200 + i as u64);
    }
    for (i, name) in
        ["cin_10_rgb.cin", "cin_10_planes.cin", "cin_16_rgb.cin", "cin_10_gray.cin"].into_iter().enumerate()
    {
        torture(name, Format::Cineon, &sample("film", name), 300 + i as u64);
    }
    for (i, name) in ["pam_rgba8.pam", "pam_rgb16.pam", "pam_gray16.pam"].into_iter().enumerate() {
        torture(name, Format::Pam, &sample("film", name), 400 + i as u64);
    }
    torture("farbfeld", Format::Farbfeld, &sample("film", "farbfeld.ff"), 500);
}

#[test]
fn jpeg_xr_samples() {
    // Each decode forks a child, so a smaller run than the others.
    for (i, name) in ["rgb24_tiled.jxr", "rgba8_planar.jxr", "rgba_half.jxr"].into_iter().enumerate() {
        let original = sample("jxr", name);
        let step = (original.len() / 40).max(1);
        for len in (0..original.len()).step_by(step) {
            let _ = survive(&format!("{name} cut at {len}"), Format::Jxr, &original[..len]);
        }
        let mut rng = XorShift(900 + i as u64);
        for k in 0..60 {
            let mut b = original.clone();
            for _ in 0..1 + rng.below(4) {
                let at = if k % 2 == 0 { rng.below(b.len().min(400)) } else { rng.below(b.len()) };
                b[at] = rng.next() as u8;
            }
            let _ = survive(&format!("{name} mutation {k}"), Format::Jxr, &b);
        }
    }
}

/*┌─────────────────╮
| Hostile headers |
└─────────────────╯*/

fn card(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(80, b' ');
    v
}

/// A header of the cards given and `data`, both padded to blocks.
fn fits(cards: &[&str], data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = cards.iter().flat_map(|c| card(c)).collect();
    out.extend(card("END"));
    out.resize(out.len().div_ceil(2880) * 2880, b' ');
    out.extend(data);
    out.resize(out.len().div_ceil(2880) * 2880, 0);
    out
}

fn refuses_quickly(name: &str, format: Format, bytes: &[u8]) {
    let start = Instant::now();
    let r = survive(name, format, bytes);
    assert!(r.is_err(), "{name}: should be refused");
    assert!(start.elapsed() < Duration::from_secs(2), "{name}: took {:?}", start.elapsed());
}

#[test]
fn fits_absurd_sizes() {
    let build = |bitpix: i64, axes: &[u64]| {
        let mut cards = vec![
            "SIMPLE  =                    T".to_string(),
            format!("BITPIX  = {bitpix}"),
            format!("NAXIS   = {}", axes.len()),
        ];
        cards.extend(axes.iter().enumerate().map(|(i, a)| format!("NAXIS{} = {a}", i + 1)));
        let refs: Vec<&str> = cards.iter().map(|s| s.as_str()).collect();
        fits(&refs, &[7u8; 64])
    };
    refuses_quickly("huge plane", Format::Fits, &build(16, &[1 << 31, 1 << 31]));
    refuses_quickly("huge width", Format::Fits, &build(-64, &[1 << 40, 2]));
    refuses_quickly("overflowing axes", Format::Fits, &build(32, &[u64::MAX / 2, u64::MAX / 2]));
    refuses_quickly("huge cube", Format::Fits, &build(8, &[100, 100, 1_000_000_000]));
    refuses_quickly("many axes", Format::Fits, &build(8, &[2; 40]));
    refuses_quickly("odd bitpix", Format::Fits, &build(12, &[4, 4]));
    // Not FITS at all, and a header that never ends.
    refuses_quickly("no end", Format::Fits, &[b' '; 2880 * 3]);
    let mut longer = card("SIMPLE  =                    T");
    longer.extend(vec![b'X'; 2880 * 40]);
    refuses_quickly("endless header", Format::Fits, &longer);
}

#[test]
fn fits_compression_bombs() {
    // A tile-compressed image whose single tile claims 60000 x 60000
    // pixels, stored in a few bytes.
    let table_header = |zcmp: &str, extra: &[&str]| {
        let mut cards = vec![
            "XTENSION= 'BINTABLE'".to_string(),
            "BITPIX  =                    8".to_string(),
            "NAXIS   =                    2".to_string(),
            "NAXIS1  =                    8".to_string(),
            "NAXIS2  =                    1".to_string(),
            "PCOUNT  =                   64".to_string(),
            "GCOUNT  =                    1".to_string(),
            "TFIELDS =                    1".to_string(),
            "TTYPE1  = 'COMPRESSED_DATA'".to_string(),
            "TFORM1  = '1PB(64)'".to_string(),
            "ZIMAGE  =                    T".to_string(),
            format!("ZCMPTYPE = '{zcmp}'"),
            "ZBITPIX =                   16".to_string(),
            "ZNAXIS  =                    2".to_string(),
            "ZNAXIS1 =                60000".to_string(),
            "ZNAXIS2 =                60000".to_string(),
            "ZTILE1  =                60000".to_string(),
            "ZTILE2  =                60000".to_string(),
        ];
        cards.extend(extra.iter().map(|s| s.to_string()));
        cards
    };
    for zcmp in ["RICE_1", "GZIP_1", "GZIP_2", "PLIO_1", "HCOMPRESS_1"] {
        let cards = table_header(zcmp, &[]);
        let refs: Vec<&str> = cards.iter().map(|s| s.as_str()).collect();
        // The table row: 4-byte count 32, offset 0, then 64 bytes of heap.
        let mut data = vec![0, 0, 0, 32, 0, 0, 0, 0];
        data.extend([0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]);
        data.extend(vec![0xffu8; 54]);
        let mut file = fits(
            &[
                "SIMPLE  =                    T",
                "BITPIX  =                    8",
                "NAXIS   =                    0",
                "EXTEND  =                    T",
            ],
            &[],
        );
        file.extend(fits(&refs, &data));
        refuses_quickly(&format!("{zcmp} bomb"), Format::Fits, &file);
    }
}

#[test]
fn xisf_hostile_headers() {
    let build = |attrs: &str, block: &[u8]| {
        let header = |pos: usize| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><xisf version=\"1.0\"><Image {attrs} location=\"attachment:{pos:010}:{:010}\"/></xisf>",
                block.len()
            )
        };
        let pos = 16 + header(0).len();
        let h = header(pos);
        let mut out = b"XISF0100".to_vec();
        out.extend((h.len() as u32).to_le_bytes());
        out.extend([0; 4]);
        out.extend(h.as_bytes());
        out.extend(block);
        out
    };
    // 100k x 100k of 16-bit stored in a handful of bytes, plain and packed.
    refuses_quickly(
        "huge plain",
        Format::Xisf,
        &build("geometry=\"100000:100000:1\" sampleFormat=\"UInt16\"", &[1; 64]),
    );
    refuses_quickly(
        "zlib bomb",
        Format::Xisf,
        &build(
            "geometry=\"30000:30000:1\" sampleFormat=\"UInt16\" compression=\"zlib:1800000000\"",
            &[0x78, 0x9c, 3, 0, 0, 0, 0, 1],
        ),
    );
    refuses_quickly(
        "zstd bomb",
        Format::Xisf,
        &build(
            "geometry=\"30000:30000:1\" sampleFormat=\"UInt16\" compression=\"zstd:1800000000\"",
            &[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0],
        ),
    );
    refuses_quickly(
        "lz4 bomb",
        Format::Xisf,
        &build("geometry=\"30000:30000:1\" sampleFormat=\"UInt16\" compression=\"lz4:1800000000\"", &[0xf0; 64]),
    );
    refuses_quickly(
        "unknown codec",
        Format::Xisf,
        &build("geometry=\"4:4:1\" sampleFormat=\"UInt8\" compression=\"bzip2:16\"", &[0; 16]),
    );
    refuses_quickly(
        "complex samples",
        Format::Xisf,
        &build("geometry=\"4:4:1\" sampleFormat=\"Complex32\"", &[0; 128]),
    );
    refuses_quickly(
        "subblock sizes lie",
        Format::Xisf,
        &build(
            "geometry=\"4:4:1\" sampleFormat=\"UInt8\" compression=\"zlib:16\" subblocks=\"8,1000:8,1000\"",
            &[0; 16],
        ),
    );
    refuses_quickly("distributed unit", Format::Xisf, b"XISF0100\x40\0\0\0\0\0\0\0<xisf><Image geometry=\"4:4:1\" sampleFormat=\"UInt8\" location=\"url(file:///etc/passwd)\"/></xisf>");
    // An entity bomb in the XML.
    let bomb = "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;\"><!ENTITY c \"&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;\">]><xisf><Image geometry=\"&c;\"/></xisf>";
    let mut f = b"XISF0100".to_vec();
    f.extend((bomb.len() as u32).to_le_bytes());
    f.extend([0; 4]);
    f.extend(bomb.as_bytes());
    refuses_quickly("entity bomb", Format::Xisf, &f);
}

#[test]
fn film_and_netpbm_hostile_headers() {
    // A DPX claiming 60000 x 60000 in a 2 KB file.
    let mut dpx = sample("film", "dpx_10_be_a.dpx");
    dpx.truncate(8192);
    dpx[0x304..0x308].copy_from_slice(&60000u32.to_be_bytes());
    dpx[0x308..0x30c].copy_from_slice(&60000u32.to_be_bytes());
    refuses_quickly("huge dpx", Format::Dpx, &dpx);
    let mut dpx = sample("film", "dpx_10_be_a.dpx");
    dpx[0x302..0x304].copy_from_slice(&200u16.to_be_bytes());
    refuses_quickly("too many elements", Format::Dpx, &dpx);
    let mut dpx = sample("film", "dpx_10_be_a.dpx");
    dpx[0x30C + 32..0x30C + 36].copy_from_slice(&0xfff0_0000u32.to_be_bytes());
    refuses_quickly("absurd line padding", Format::Dpx, &dpx);
    let mut cin = sample("film", "cin_10_rgb.cin");
    cin.truncate(2048);
    cin[196 + 4..196 + 8].copy_from_slice(&60000u32.to_be_bytes());
    cin[196 + 32..196 + 36].copy_from_slice(&60000u32.to_be_bytes());
    cin[196 + 60..196 + 64].copy_from_slice(&60000u32.to_be_bytes());
    refuses_quickly("huge cineon", Format::Cineon, &cin);
    refuses_quickly(
        "huge pam",
        Format::Pam,
        b"P7\nWIDTH 60000\nHEIGHT 60000\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\nabcd",
    );
    refuses_quickly("pam depth", Format::Pam, b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 99999\nMAXVAL 255\nENDHDR\nabcd");
    let mut ff = b"farbfeld".to_vec();
    ff.extend(60000u32.to_be_bytes());
    ff.extend(60000u32.to_be_bytes());
    ff.extend([1; 32]);
    refuses_quickly("huge farbfeld", Format::Farbfeld, &ff);
}
