//! Niantic's SPZ (github.com/nianticlabs/spz, MIT).  Versions 1 to 3 are
//! one gzip stream: a 16-byte header (`NGSP`, version, point count,
//! spherical-harmonic degree, fractional bits, flags) followed by each
//! field for every point in turn: positions, alphas, colours, scales,
//! rotations, spherical harmonics.  Version 4 has a 32-byte plain header
//! and a table of contents, and compresses each field as its own zstd
//! stream.
//!
//! Fields are quantized: positions are 24-bit fixed point (half floats in
//! version 1), alpha the opacity after its sigmoid in a byte, colour the
//! band 0 coefficient scaled by 0.15 around 128, scales a byte of
//! logarithm (`s / 16 - 10`), rotations three bytes of x, y and z
//! (versions 1 and 2) or the smallest three components in 10 bits each
//! plus the largest's index (version 3 on), and harmonic coefficients a
//! byte each around 128.  SPZ stores x right, y up, z back, which is
//! Slerp's own frame.
//!
//! Fields are read as streams and only the splats `Limits::max_splats`
//! keeps are held, so a capture of millions costs what its thumbnail
//! needs, and a file that claims more points than it holds fails when
//! its data runs out rather than allocating up front.

use crate::{Builder, SH_C0, SplatFormat, degree_for};
use glam::{Quat, Vec3};
use half::f16;
use slerp_scene::{Info, Limits, LoadError, Scene};
use std::io::Read;

pub(crate) const MAGIC: &[u8] = b"NGSP";
const COLOR_SCALE: f32 = 0.15;
const GZIP: [u8; 2] = [0x1f, 0x8b];

/// Whether `head` is a gzip stream that starts with the SPZ magic.
pub(crate) fn is_gzipped_spz(head: &[u8]) -> bool {
    if !head.starts_with(&GZIP) {
        return false;
    }
    let mut magic = [0u8; 4];
    flate2::read::GzDecoder::new(head).read_exact(&mut magic).is_ok() && magic == MAGIC
}

/// What the header says.
struct Header {
    version: u32,
    points: u64,
    degree: u8,
    /// Coefficients per channel above band 0 in the file.
    sh_dim: usize,
    fractional_bits: u8,
    antialiased: bool,
}

impl Header {
    fn parse(b: &[u8]) -> Result<Header, LoadError> {
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        if &b[0..4] != MAGIC {
            return Err(LoadError::invalid("no NGSP magic"));
        }
        let version = u32_at(4);
        if !(1..=4).contains(&version) {
            return Err(LoadError::Unsupported(format!("SPZ version {version}")));
        }
        let degree = b[12];
        let sh_dim = match degree {
            0 => 0,
            1 => 3,
            2 => 8,
            3 => 15,
            4 => 24,
            _ => return Err(LoadError::invalid(format!("SPZ spherical-harmonic degree {degree}"))),
        };
        let points = u32_at(8) as u64;
        if points == 0 || points > i32::MAX as u64 {
            return Err(LoadError::invalid(format!("SPZ point count {points}")));
        }
        Ok(Header {
            version,
            points,
            degree: degree_for(sh_dim).unwrap_or(0),
            sh_dim,
            fractional_bits: b[13],
            antialiased: b[14] & 1 != 0,
        })
    }

    /// Bytes per point of each field, in file order.
    fn widths(&self) -> [usize; 6] {
        let position = if self.version == 1 { 6 } else { 9 };
        let rotation = if self.version >= 3 { 4 } else { 3 };
        [position, 1, 3, 3, rotation, self.sh_dim * 3]
    }
}

/// Reads one field of `width` bytes per point for `points` points from
/// `reader`, keeping the kept points' bytes.
fn read_field(reader: &mut impl Read, points: u64, width: usize, out: &Builder) -> Result<Vec<u8>, LoadError> {
    let mut kept = Vec::new();
    if width == 0 {
        return Ok(kept);
    }
    let per_block = ((256usize << 10) / width).max(1);
    let mut block = vec![0u8; per_block * width];
    let mut i = 0u64;
    while i < points {
        let n = (per_block as u64).min(points - i) as usize;
        let buf = &mut block[..n * width];
        reader.read_exact(buf).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => LoadError::invalid("SPZ data ends early"),
            _ => LoadError::invalid(format!("SPZ data: {e}")),
        })?;
        for (j, p) in buf.chunks_exact(width).enumerate() {
            if out.keeps(i + j as u64) {
                kept.extend_from_slice(p);
            }
        }
        i += n as u64;
    }
    Ok(kept)
}

fn rotation_first_three(r: &[u8]) -> Quat {
    let xyz = Vec3::new(r[0] as f32, r[1] as f32, r[2] as f32) / 127.5 - 1.0;
    Quat::from_xyzw(xyz.x, xyz.y, xyz.z, (1.0 - xyz.length_squared()).max(0.0).sqrt())
}

/// Version 3's "smallest three": the largest component's index (x, y, z,
/// w order) in the top two bits, then the other three, highest index in
/// the lowest bits, as 9 bits of magnitude and a sign bit scaled by
/// 1/sqrt(2).
fn rotation_smallest_three(r: &[u8]) -> Quat {
    let mut comp = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
    let largest = (comp >> 30) as usize;
    let mask = (1u32 << 9) - 1;
    let mut q = [0f32; 4];
    let mut sum = 0.0;
    for i in (0..4).rev() {
        if i == largest {
            continue;
        }
        let magnitude = (comp & mask) as f32 / mask as f32 * std::f32::consts::FRAC_1_SQRT_2;
        let negative = (comp >> 9) & 1 == 1;
        comp >>= 10;
        q[i] = if negative { -magnitude } else { magnitude };
        sum += q[i] * q[i];
    }
    q[largest] = (1.0 - sum).max(0.0).sqrt();
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}

/// Turns the kept points' field bytes into splats.
fn build(h: &Header, fields: [Vec<u8>; 6], mut out: Builder, info: Info) -> Result<Scene, LoadError> {
    let w = h.widths();
    let n = fields[1].len();
    for (f, &width) in fields.iter().zip(&w) {
        if f.len() != n * width {
            return Err(LoadError::invalid("SPZ fields disagree in length"));
        }
    }
    let fixed = 1.0 / (1u64 << h.fractional_bits.min(31)) as f32;
    for i in 0..n {
        let p = &fields[0][i * w[0]..][..w[0]];
        let position = if h.version == 1 {
            let half = |j: usize| f16::from_bits(u16::from_le_bytes([p[j], p[j + 1]])).to_f32();
            Vec3::new(half(0), half(2), half(4))
        } else {
            let fixed24 = |j: usize| {
                let v = p[j] as i32 | (p[j + 1] as i32) << 8 | (p[j + 2] as i32) << 16;
                ((v << 8) >> 8) as f32 * fixed
            };
            Vec3::new(fixed24(0), fixed24(3), fixed24(6))
        };
        let alpha = fields[1][i] as f32 / 255.0;
        let c = &fields[2][i * 3..][..3];
        let dc = |j: usize| (c[j] as f32 / 255.0 - 0.5) / COLOR_SCALE;
        let rgb = Vec3::new(dc(0), dc(1), dc(2)) * SH_C0 + 0.5;
        let s = &fields[3][i * 3..][..3];
        let scale = Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32).map(|v| (v / 16.0 - 10.0).exp());
        let r = &fields[4][i * w[4]..][..w[4]];
        let rotation = if h.version >= 3 { rotation_smallest_three(r) } else { rotation_first_three(r) };
        let sh = &fields[5][i * w[5]..][..w[5]];
        out.push(position, scale, rotation, rgb.extend(alpha), |k, ch| (sh[k * 3 + ch] as f32 - 128.0) / 128.0);
    }
    out.splats.antialiased = h.antialiased;
    out.finish(info)
}

pub(crate) fn load(bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.starts_with(MAGIC) {
        return load_zstd(bytes, limits);
    }
    if !bytes.starts_with(&GZIP) {
        return Err(LoadError::NotMine);
    }
    // Versions 1 to 3: one gzip stream.
    let mut stream = flate2::read::GzDecoder::new(bytes);
    let mut head = [0u8; 16];
    stream.read_exact(&mut head).map_err(|_| LoadError::invalid("SPZ header cut short"))?;
    let h = Header::parse(&head)?;
    if h.version == 4 {
        return Err(LoadError::invalid("SPZ version 4 inside gzip"));
    }
    let out = Builder::new(h.points, limits, h.degree, false);
    let info = describe(&h);
    let mut fields: [Vec<u8>; 6] = Default::default();
    for (field, width) in fields.iter_mut().zip(h.widths()) {
        *field = read_field(&mut stream, h.points, width, &out)?;
    }
    build(&h, fields, out, info)
}

/// Version 4: a plain header, an optional block of extension records, a
/// table of contents with each stream's compressed and uncompressed size,
/// and one zstd stream per non-empty field.
fn load_zstd(bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.len() < 32 {
        return Err(LoadError::invalid("SPZ header cut short"));
    }
    let h = Header::parse(&bytes[..16])?;
    if h.version != 4 {
        return Err(LoadError::invalid(format!("SPZ version {} without gzip", h.version)));
    }
    let streams = bytes[15] as usize;
    let toc = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    let widths = h.widths();
    let present: Vec<usize> = (0..6).filter(|&f| widths[f] > 0).collect();
    if streams != present.len() || toc < 32 {
        return Err(LoadError::invalid(format!("SPZ has {streams} streams, expected {}", present.len())));
    }
    let toc_end = toc
        .checked_add(streams * 16)
        .filter(|&e| e <= bytes.len())
        .ok_or_else(|| LoadError::invalid("SPZ table of contents past the end"))?;
    let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap_or([0; 8]));
    let out = Builder::new(h.points, limits, h.degree, false);
    let info = describe(&h);
    let mut fields: [Vec<u8>; 6] = Default::default();
    let mut at = toc_end;
    for (s, &f) in present.iter().enumerate() {
        let (packed, unpacked) = (u64_at(toc + s * 16), u64_at(toc + s * 16 + 8));
        if Some(unpacked) != h.points.checked_mul(widths[f] as u64) {
            return Err(LoadError::invalid("SPZ stream size disagrees with the point count"));
        }
        let end = usize::try_from(packed).ok().and_then(|p| at.checked_add(p)).filter(|&e| e <= bytes.len());
        let end = end.ok_or_else(|| LoadError::invalid("SPZ stream runs past the end"))?;
        let mut stream = zstd::stream::read::Decoder::with_buffer(&bytes[at..end])
            .map_err(|e| LoadError::invalid(format!("SPZ zstd: {e}")))?;
        fields[f] = read_field(&mut stream, h.points, widths[f], &out)?;
        at = end;
    }
    build(&h, fields, out, info)
}

fn describe(h: &Header) -> Info {
    let mut info =
        Info { format: format!("{} (version {})", SplatFormat::Spz.name(), h.version), ..Default::default() };
    if h.sh_dim == 24 {
        info.warnings.push("spherical-harmonic band 4 isn't drawn".into());
    }
    info
}
