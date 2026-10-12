//! Video formats in DDS files: DXGI 1.2's YUV formats and Direct3D 9's
//! YUY2 and UYVY.  They store luma (Y) and two chroma differences (U is
//! Cb, V is Cr), often with chroma at half or quarter resolution, in one of
//! three arrangements: one texel per word (AYUV, Y410, Y416), pairs of
//! texels sharing chroma (YUY2, UYVY, Y210, Y216), or a luma plane followed
//! by a plane of interleaved chroma (NV12, P010, P016 at half resolution
//! both ways; NV11 at quarter width).
//!
//! Microsoft leaves the colour matrix to the application.  Slerp follows
//! texconv (DirectXTex), the tool that writes most of these files: BT.601
//! with studio range at every bit depth, in DirectXTex's own integer
//! arithmetic, so wherever chroma is flat Slerp gives exactly what texconv
//! gives.
//!
//! texconv repeats each chroma sample over the texels it covers; Slerp
//! blends neighbouring samples by where they sit instead, which avoids
//! blocky colour edges.  texconv's encoder averages each pair's chroma, so
//! 4:2:2 chroma sits between the pair (centred): blending it that way
//! matches the original picture better than repeating it (39.1 against
//! 38.4 dB on a test photo), while the co-sited placement Microsoft's
//! video documentation draws does worse (37.7 dB).  texconv can't write the
//! planar layouts, which come from video tools, so they follow Microsoft's
//! documentation and video's default: 4:2:0 chroma on the first column of
//! the pair and halfway between its two rows (MPEG-2 siting), 4:1:1 chroma
//! on the first of its four columns.  `Chroma::Nearest` reproduces texconv
//! for the tests.

use crate::Pixels;
use crate::formats::Yuv;
use rayon::prelude::*;

/// How chroma is brought to full resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chroma {
    /// Blended between neighbouring samples by their documented positions.
    Sited,
    /// Each sample repeated over the texels it covers, as texconv does;
    /// only the tests use it, to compare with texconv exactly.
    #[cfg_attr(not(test), allow(dead_code))]
    Nearest,
}

/// Decodes `depth` slices of a video format to 8-bit RGBA.  `row` is the
/// distance between rows (of luma, for planar layouts) and `slice` the size
/// of one slice; the caller has checked that `data` holds them.
pub(crate) fn decode(layout: Yuv, w: u32, h: u32, depth: u32, data: &[u8], row: usize, slice: usize) -> Pixels {
    decode_with(layout, Chroma::Sited, w, h, depth, data, row, slice)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_with(
    layout: Yuv,
    chroma: Chroma,
    w: u32,
    h: u32,
    depth: u32,
    data: &[u8],
    row: usize,
    slice: usize,
) -> Pixels {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 4 * depth as usize];
    for (z, dst) in out.chunks_exact_mut(w * h * 4).enumerate() {
        let src = Source { layout, data: &data[z * slice..(z + 1) * slice], w, h, row };
        dst.par_chunks_mut(w * 4).enumerate().for_each(|(y, out)| src.decode_row(y, chroma, out));
    }
    Pixels::Rgba8(out)
}

/*┌─────────────────╮
| Reading samples |
└─────────────────╯*/
//region Reading samples

/// One slice of a video format.
struct Source<'a> {
    layout: Yuv,
    data: &'a [u8],
    w: usize,
    h: usize,
    row: usize,
}

impl Source<'_> {
    /// A component of `bytes` bytes at byte `at`, little-endian.  P010 and
    /// Y210 keep their 10 bits at the top of 16.
    fn sample(&self, at: usize) -> i32 {
        match self.layout {
            Yuv::Ayuv | Yuv::Yuy2 | Yuv::Uyvy | Yuv::Nv12 | Yuv::Nv11 => self.data[at] as i32,
            Yuv::Y210 | Yuv::P010 => (u16::from_le_bytes([self.data[at], self.data[at + 1]]) >> 6) as i32,
            _ => u16::from_le_bytes([self.data[at], self.data[at + 1]]) as i32,
        }
    }

    /// How many texels share one chroma sample across, and whether chroma
    /// rows are halved.
    fn subsampling(&self) -> (usize, bool) {
        match self.layout {
            Yuv::Ayuv | Yuv::Y410 | Yuv::Y416 => (1, false),
            Yuv::Nv12 | Yuv::P010 | Yuv::P016 => (2, true),
            Yuv::Nv11 => (4, false),
            _ => (2, false),
        }
    }

    /// Luma and alpha (8-bit) of row `y`, with the chroma samples of row `y`
    /// for formats that keep chroma in the same words.
    fn luma_row(&self, y: usize) -> (Vec<i32>, Vec<u8>) {
        let at = y * self.row;
        let w = self.w;
        let word = |x: usize| u32::from_le_bytes(self.data[at + x * 4..at + x * 4 + 4].try_into().unwrap());
        match self.layout {
            Yuv::Ayuv => (
                (0..w).map(|x| self.data[at + x * 4 + 2] as i32).collect(),
                (0..w).map(|x| self.data[at + x * 4 + 3]).collect(),
            ),
            Yuv::Y410 => (
                (0..w).map(|x| ((word(x) >> 10) & 0x3FF) as i32).collect(),
                (0..w).map(|x| (word(x) >> 30) as u8 * 85).collect(),
            ),
            Yuv::Y416 => {
                let a = |x: usize| ((self.sample(at + x * 8 + 6) * 255 + 32767) / 65535) as u8;
                ((0..w).map(|x| self.sample(at + x * 8 + 2)).collect(), (0..w).map(a).collect())
            }
            Yuv::Yuy2 | Yuv::Uyvy | Yuv::Y210 | Yuv::Y216 | Yuv::Uyvy16 => {
                let bytes = if matches!(self.layout, Yuv::Yuy2 | Yuv::Uyvy) { 1 } else { 2 };
                // Where Y0 and Y1 sit among each pair's four components.
                let first = if matches!(self.layout, Yuv::Uyvy | Yuv::Uyvy16) { 1 } else { 0 };
                let luma =
                    (0..w).map(|x| self.sample(at + (x / 2) * 4 * bytes + (first + 2 * (x % 2)) * bytes)).collect();
                (luma, vec![255; w])
            }
            Yuv::Nv12 | Yuv::Nv11 => ((0..w).map(|x| self.data[at + x] as i32).collect(), vec![255; w]),
            Yuv::P010 | Yuv::P016 => ((0..w).map(|x| self.sample(at + x * 2)).collect(), vec![255; w]),
        }
    }

    /// The chroma samples (U, V) of chroma row `j`.
    fn chroma_row(&self, j: usize) -> (Vec<i32>, Vec<i32>) {
        let (step, _) = self.subsampling();
        let n = self.w.div_ceil(step);
        let at = j * self.row;
        let pick = |f: &dyn Fn(usize) -> i32| (0..n).map(f).collect::<Vec<i32>>();
        match self.layout {
            Yuv::Ayuv => (pick(&|x| self.data[at + x * 4 + 1] as i32), pick(&|x| self.data[at + x * 4] as i32)),
            Yuv::Y410 => {
                let word = |x: usize| u32::from_le_bytes(self.data[at + x * 4..at + x * 4 + 4].try_into().unwrap());
                (pick(&|x| (word(x) & 0x3FF) as i32), pick(&|x| ((word(x) >> 20) & 0x3FF) as i32))
            }
            Yuv::Y416 => (pick(&|x| self.sample(at + x * 8)), pick(&|x| self.sample(at + x * 8 + 4))),
            Yuv::Yuy2 | Yuv::Y210 | Yuv::Y216 => {
                let b = if self.layout == Yuv::Yuy2 { 1 } else { 2 };
                (pick(&|k| self.sample(at + k * 4 * b + b)), pick(&|k| self.sample(at + k * 4 * b + 3 * b)))
            }
            Yuv::Uyvy => (pick(&|k| self.data[at + k * 4] as i32), pick(&|k| self.data[at + k * 4 + 2] as i32)),
            Yuv::Uyvy16 => (pick(&|k| self.sample(at + k * 8)), pick(&|k| self.sample(at + k * 8 + 4))),
            // The chroma plane follows the luma plane's rows, with the
            // same pitch (half of it for NV11).
            Yuv::Nv12 | Yuv::P010 | Yuv::P016 => {
                let at = (self.h + j) * self.row;
                let b = if self.layout == Yuv::Nv12 { 1 } else { 2 };
                (pick(&|k| self.sample(at + k * 2 * b)), pick(&|k| self.sample(at + k * 2 * b + b)))
            }
            Yuv::Nv11 => {
                let at = self.h * self.row + j * (self.row / 2);
                (pick(&|k| self.data[at + k * 2] as i32), pick(&|k| self.data[at + k * 2 + 1] as i32))
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Upsampling      |
└─────────────────╯*/
//region Upsampling

impl Source<'_> {
    /// Decodes row `y` into `out`.
    fn decode_row(&self, y: usize, chroma: Chroma, out: &mut [u8]) {
        let (luma, alpha) = self.luma_row(y);
        let (step, halved) = self.subsampling();
        // 1. Chroma rows and their weights: 4:2:0 rows sit between two luma
        //    rows, so each luma row takes 3/4 of the nearer chroma row and
        //    1/4 of the next nearest.
        let rows = if halved { self.h.div_ceil(2) } else { self.h };
        let (rows, vertical): (Vec<(usize, i32)>, i32) = match (halved, chroma) {
            (false, _) => (vec![(y, 1)], 1),
            (true, Chroma::Nearest) => (vec![(y / 2, 1)], 1),
            (true, Chroma::Sited) => {
                let j = y / 2;
                let other = if y.is_multiple_of(2) { j.saturating_sub(1) } else { (j + 1).min(rows - 1) };
                (vec![(j, 3), (other, 1)], 4)
            }
        };
        let mut u = vec![0i32; self.w.div_ceil(step)];
        let mut v = u.clone();
        for (j, weight) in rows {
            let (cu, cv) = self.chroma_row(j);
            for k in 0..u.len() {
                u[k] += weight * cu[k];
                v[k] += weight * cv[k];
            }
        }
        // 2. Across: a texel blends the two samples whose positions it lies
        //    between.  Co-sited samples sit on the first texel they cover;
        //    centred ones between the texels, weighed in 2*step-ths.
        let centred = matches!(self.layout, Yuv::Yuy2 | Yuv::Uyvy | Yuv::Y210 | Yuv::Y216 | Yuv::Uyvy16);
        let horizontal = match chroma {
            Chroma::Nearest => 1,
            Chroma::Sited if centred => 2 * step as i32,
            Chroma::Sited => step as i32,
        };
        let at = |c: &[i32], x: usize| -> i32 {
            let (k, i, step) = (x / step, (x % step) as i32, step as i32);
            let next = |k: usize| c[(k + 1).min(c.len() - 1)];
            match chroma {
                _ if step == 1 => c[k],
                Chroma::Nearest => c[k],
                Chroma::Sited if centred => {
                    let d = 2 * i + 1 - step;
                    let other = if d < 0 { c[k.saturating_sub(1)] } else { next(k) };
                    (2 * step - d.abs()) * c[k] + d.abs() * other
                }
                Chroma::Sited => (step - i) * c[k] + i * next(k),
            }
        };
        let m = Matrix::of(self.layout, vertical * horizontal);
        for (x, o) in out.chunks_exact_mut(4).enumerate() {
            let [r, g, b] = m.rgb(luma[x], at(&u, x), at(&v, x));
            o.copy_from_slice(&[r, g, b, alpha[x]]);
        }
    }
}

//endregion

/*┌─────────────────╮
| Colour matrix   |
└─────────────────╯*/
//region Colour matrix

/// BT.601 studio-range YUV to RGB at one bit depth, as DirectXTex's integer
/// arithmetic does it, for chroma multiplied by `scale` (a power of two:
/// the sum of the upsampling weights).
struct Matrix {
    /// Coefficients for Y, V into R, U and V into G, and U into B, in
    /// fixed point with `shift` fractional bits.
    k: [i64; 5],
    shift: u32,
    y_offset: i64,
    c_offset: i64,
    max: i64,
    scale: i64,
}

impl Matrix {
    fn of(layout: Yuv, scale: i32) -> Matrix {
        let (k, shift, y_offset, c_offset, max) = match layout {
            Yuv::Ayuv | Yuv::Yuy2 | Yuv::Uyvy | Yuv::Nv12 | Yuv::Nv11 => ([298, 409, 100, 208, 516], 8, 16, 128, 255),
            Yuv::Y410 | Yuv::Y210 | Yuv::P010 => ([76533, 104905, 25747, 53425, 132590], 16, 64, 512, 1023),
            _ => ([76607, 105006, 25772, 53477, 132718], 16, 4096, 32768, 65535),
        };
        Matrix { k, shift, y_offset, c_offset, max, scale: scale as i64 }
    }

    /// Converts luma and `scale`-times chroma to 8-bit RGB.
    fn rgb(&self, y: i32, u: i32, v: i32) -> [u8; 3] {
        let s = self.scale;
        let bits = self.shift + s.trailing_zeros();
        let y = (y as i64 - self.y_offset) * self.k[0] * s;
        let (u, v) = (u as i64 - self.c_offset * s, v as i64 - self.c_offset * s);
        let round = 1i64 << (bits - 1);
        let to8 = |c: i64| {
            let c = c.clamp(0, self.max);
            if self.max == 255 { c as u8 } else { ((c * 255 + self.max / 2) / self.max) as u8 }
        };
        [
            to8((y + self.k[1] * v + round) >> bits),
            to8((y - self.k[2] * u - self.k[3] * v + round) >> bits),
            to8((y + self.k[4] * u + round) >> bits),
        ]
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat colours decode exactly as DirectXTex's formulas give them,
    /// whatever the upsampling.
    #[test]
    fn matrix_matches_directxtex() {
        // 8-bit: Y 81, U 90, V 240 is BT.601's pure red.
        let m = Matrix::of(Yuv::Ayuv, 1);
        assert_eq!(m.rgb(81, 90, 240), [255, 0, 0]);
        assert_eq!(Matrix::of(Yuv::Nv12, 8).rgb(81, 90 * 8, 240 * 8), [255, 0, 0]);
        assert_eq!(m.rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(m.rgb(235, 128, 128), [255, 255, 255]);
        // 10-bit and 16-bit greys: (76533 * 438 + 32768) >> 16 is 511 of
        // 1023, and the 16-bit one 32767 of 65535: 127 of 255 both.
        assert_eq!(Matrix::of(Yuv::Y410, 1).rgb(64 + 438, 512, 512), [127, 127, 127]);
        assert_eq!(Matrix::of(Yuv::Y416, 4).rgb(4096 + 28032, 32768 * 4, 32768 * 4), [127, 127, 127]);
    }

    fn nv12(w: usize, h: usize, luma: u8, chroma: impl Fn(usize, usize) -> (u8, u8)) -> Vec<u8> {
        let mut d = vec![luma; w * h];
        for j in 0..h.div_ceil(2) {
            for k in 0..w.div_ceil(2) {
                let (u, v) = chroma(k, j);
                d.extend([u, v]);
            }
        }
        d
    }

    /// 4:2:0 chroma blends 3:1 between rows and 1:1 between co-sited
    /// columns; texconv's nearest repeats it.
    #[test]
    fn nv12_siting() {
        // V rises by 16 per chroma column and 64 per chroma row.
        let data = nv12(4, 4, 128, |k, j| (128, 64 + 16 * k as u8 + 64 * j as u8));
        let red = |p: &Pixels, x: usize, y: usize| match p {
            Pixels::Rgba8(v) => v[(y * 4 + x) * 4] as i32,
            _ => unreachable!(),
        };
        let sited = decode_with(Yuv::Nv12, Chroma::Sited, 4, 4, 1, &data, 4, data.len());
        let nearest = decode_with(Yuv::Nv12, Chroma::Nearest, 4, 4, 1, &data, 4, data.len());
        // Texel (1, 1): V = 3/4 of row 0 and 1/4 of row 1, halfway between
        // columns 0 and 1: 64 + 8 + 16 = 88.  Texel (0, 0): row 0 only at
        // the top edge: 64.
        let r = |v: i32| Matrix::of(Yuv::Nv12, 1).rgb(128, 128, v)[0] as i32;
        assert_eq!(red(&sited, 1, 1), r(88));
        assert_eq!(red(&sited, 0, 0), r(64));
        assert_eq!(red(&nearest, 1, 1), r(64));
        assert_eq!(red(&nearest, 1, 2), r(128));
        // The last column has no right neighbour: it keeps its own.
        assert_eq!(red(&sited, 3, 0), r(80));
    }

    /// 4:2:2 chroma, which texconv centres between each pair, blends 3:1
    /// with the neighbour on the texel's side.
    #[test]
    fn yuy2_siting() {
        let data = [128, 128, 128, 64, 128, 128, 128, 128];
        let Pixels::Rgba8(v) = decode(Yuv::Yuy2, 4, 1, 1, &data, 8, 8) else { unreachable!() };
        let r = |v: i32| Matrix::of(Yuv::Yuy2, 1).rgb(128, 128, v)[0];
        assert_eq!([v[0], v[4], v[8], v[12]], [r(64), r(80), r(112), r(128)]);
    }

    /// Every layout reads the right bytes: a pure red texel in each.
    #[test]
    fn layouts() {
        let (y8, u8_, v8) = (81u8, 90u8, 240u8);
        let ten = |c: u8| (c as u16) << 2;
        let sixteen = |c: u8| (c as u16) << 8;
        let check = |layout: Yuv, w: u32, data: Vec<u8>, row: usize| {
            let p = decode(layout, w, 1, 1, &data, row, data.len());
            let Pixels::Rgba8(v) = p else { unreachable!() };
            for t in v.chunks_exact(4) {
                assert!(t[0] >= 254 && t[1] <= 1 && t[2] <= 1, "{layout:?}: {t:?}");
            }
        };
        check(Yuv::Ayuv, 1, vec![v8, u8_, y8, 255], 4);
        let y410 = (ten(u8_) as u32) | (ten(y8) as u32) << 10 | (ten(v8) as u32) << 20 | 3 << 30;
        check(Yuv::Y410, 1, y410.to_le_bytes().to_vec(), 4);
        let words = |w: [u16; 4]| w.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        check(Yuv::Y416, 1, words([sixteen(u8_), sixteen(y8), sixteen(v8), 0xFFFF]), 8);
        check(Yuv::Yuy2, 2, vec![y8, u8_, y8, v8], 4);
        check(Yuv::Uyvy, 2, vec![u8_, y8, v8, y8], 4);
        check(Yuv::Y210, 2, words([ten(y8) << 6, ten(u8_) << 6, ten(y8) << 6, ten(v8) << 6]), 8);
        check(Yuv::Y216, 2, words([sixteen(y8), sixteen(u8_), sixteen(y8), sixteen(v8)]), 8);
        let planar = |b: Vec<u8>, c: Vec<u8>| [b, c].concat();
        check(Yuv::Nv12, 2, planar(vec![y8, y8], vec![u8_, v8]), 2);
        let p = |c: u16| c.to_le_bytes().to_vec();
        check(Yuv::P010, 2, [p(ten(y8) << 6), p(ten(y8) << 6), p(ten(u8_) << 6), p(ten(v8) << 6)].concat(), 4);
        check(Yuv::P016, 2, [p(sixteen(y8)), p(sixteen(y8)), p(sixteen(u8_)), p(sixteen(v8))].concat(), 4);
        check(Yuv::Nv11, 4, planar(vec![y8; 4], vec![u8_, v8, 0, 0]), 4);
    }

    /// The layout a sample's name ends in.
    fn layout_of(stem: &str) -> Option<Yuv> {
        let all = [
            ("AYUV", Yuv::Ayuv),
            ("Y410", Yuv::Y410),
            ("Y416", Yuv::Y416),
            ("YUY2", Yuv::Yuy2),
            ("UYVY", Yuv::Uyvy),
            ("Y210", Yuv::Y210),
            ("Y216", Yuv::Y216),
            ("NV12", Yuv::Nv12),
            ("420_OPAQUE", Yuv::Nv12),
            ("P010", Yuv::P010),
            ("P016", Yuv::P016),
            ("NV11", Yuv::Nv11),
        ];
        all.iter().find(|(s, _)| stem.ends_with(s)).map(|&(_, y)| y)
    }

    /// A sample's level 0 decoded with `chroma`, straight from its bytes.
    fn decode_sample(path: &std::path::Path, chroma: Chroma) -> (u32, u32, Vec<u8>) {
        let bytes = std::fs::read(path).unwrap();
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let (h, w) = (word(12), word(16));
        let start = if &bytes[84..88] == b"DX10" { 148 } else { 128 };
        let stem = path.file_stem().unwrap().to_string_lossy();
        let layout = layout_of(&stem).unwrap();
        let codec = crate::formats::Codec::Yuv(layout);
        let (gx, gy) = codec.grid(w, h);
        let row = gx as usize * codec.block_bytes();
        let Pixels::Rgba8(v) = decode_with(layout, chroma, w, h, 1, &bytes[start..], row, row * gy as usize) else {
            unreachable!()
        };
        (w, h, v)
    }

    /// KTX2's Vulkan 4:2:2 conformance files, which declare the YUV colour
    /// model, decode through these layouts and say so.  (Their contents
    /// differ from file to file, so they aren't compared with each other;
    /// `tests/containers.rs` checks a known colour in every format.)
    #[test]
    fn ktx2_422_samples_are_ycbcr() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/textures/ktx-cts/ktx2");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skipped: no {}", dir.display());
            return;
        };
        let mut n = 0;
        for path in entries.flatten().map(|e| e.path()) {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.contains("_422_") || name.contains("_1D") {
                continue;
            }
            let tex = crate::decode(&std::fs::read(&path).unwrap(), &crate::DecodeOptions::full()).unwrap();
            assert!(tex.source.format.contains("(YCbCr)"), "{name}: {}", tex.source.format);
            n += 1;
        }
        assert!(n >= 8, "only {n} samples");
    }

    /// texconv's decodes of every sample, from `textures-extra/dds-yuv`
    /// (made with texconv in the Windows VM, see `tools/assets.jsonc`) and
    /// image-dds's texconv-made files in `textures/dds`.  With texconv's
    /// nearest chroma Slerp matches exactly, or within one step where
    /// texconv rounds 10- and 16-bit values to 8 bits; with sited chroma it
    /// differs only around colour edges.
    #[test]
    fn samples_match_texconv() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
        let ours = assets.join("textures-extra/dds-yuv");
        let Ok(entries) = std::fs::read_dir(&ours) else {
            eprintln!("skipped: no {}", ours.display());
            return;
        };
        let mut pairs: Vec<(std::path::PathBuf, std::path::PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "dds"))
            .map(|p| {
                let png = ours.join("ref").join(p.with_extension("png").file_name().unwrap());
                (p, png)
            })
            .collect();
        for f in [
            "bi-planar/DX10 NV12",
            "bi-planar/DX10 P010",
            "bi-planar/DX10 P016",
            "sub-sampled/DX10 Y210",
            "sub-sampled/DX10 Y216",
            "sub-sampled/DX9 UYVY",
            "sub-sampled/DX9 YUY2",
            "uncompressed/DX10 AYUV",
            "uncompressed/DX10 Y410",
            "uncompressed/DX10 Y416",
        ] {
            let png =
                ours.join("ref-image-dds").join(format!("{}.png", f.rsplit('/').next().unwrap().replace(' ', "_")));
            pairs.push((assets.join(format!("textures/dds/{f}.dds")), png));
        }
        pairs.sort();
        let mut checked = 0;
        for (dds, png) in &pairs {
            let stem = dds.file_stem().unwrap().to_string_lossy();
            if !png.exists() {
                // 420_OPAQUE: texconv can't read it; it must read as NV12.
                let nv12 = dds.with_file_name(stem.replace("420_OPAQUE", "NV12") + ".dds");
                assert_eq!(decode_sample(dds, Chroma::Sited), decode_sample(&nv12, Chroma::Sited), "{stem}");
                continue;
            }
            let reference = crate::decode_file(png, &crate::DecodeOptions::full()).unwrap();
            let Pixels::Rgba8(theirs) = &reference.levels[0].surfaces[0] else { panic!() };
            let (w, h, nearest) = decode_sample(dds, Chroma::Nearest);
            assert_eq!((w, h), (reference.width(), reference.height()), "{stem}");
            let diff = |a: &[u8]| -> (u8, f64) {
                let d: Vec<u8> = a.iter().zip(theirs.iter()).map(|(x, y)| x.abs_diff(*y)).collect();
                (*d.iter().max().unwrap(), d.iter().map(|&v| v as f64).sum::<f64>() / d.len() as f64)
            };
            let (worst, _) = diff(&nearest);
            let eight_bit =
                matches!(layout_of(&stem).unwrap(), Yuv::Ayuv | Yuv::Yuy2 | Yuv::Uyvy | Yuv::Nv12 | Yuv::Nv11);
            assert!(worst <= if eight_bit { 0 } else { 1 }, "{stem}: nearest chroma differs from texconv by {worst}");
            // The file as Slerp opens it, with sited chroma.
            let tex = crate::decode_file(dds, &crate::DecodeOptions::full()).unwrap();
            let Pixels::Rgba8(sited) = &tex.levels[0].surfaces[0] else { panic!() };
            let (worst_sited, mean_sited) = diff(sited);
            eprintln!(
                "{stem}: {}, nearest chroma within {worst}; sited chroma within {worst_sited}, mean {mean_sited:.3}",
                tex.source.format
            );
            assert!(mean_sited < 5.0, "{stem}: sited chroma strays from texconv by {mean_sited} on average");
            checked += 1;
        }
        assert!(checked >= 28, "only {checked} samples");
    }
}
