//! NIfTI-1, NIfTI-2 and Analyze 7.5 volumes: `.nii`, `.nii.gz` and `.hdr` +
//! `.img` pairs, in either byte order.
//!
//! The header's `sform` or `qform` says which way each array axis points in
//! the body.  Slerp uses that only to name the axes (R, L, A, P, S, I) and to
//! order and mirror slices the way medical viewers do; it never resamples.

use crate::dtype::{DType, Endian};
use crate::source::{ByteSource, FileRange, FileSource, Inflater, StreamSource, Wrapper};
use crate::volume::{Axis, Dir, Role, Volume};
use slerp_scene::LoadError;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Nifti1,
    Nifti2,
    Analyze,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::Nifti1 => "NIfTI-1",
            Variant::Nifti2 => "NIfTI-2",
            Variant::Analyze => "Analyze 7.5",
        }
    }

    fn header_len(self) -> usize {
        if self == Variant::Nifti2 { 540 } else { 348 }
    }
}

/// What the header says.
#[derive(Clone, Debug)]
pub struct Header {
    pub variant: Variant,
    pub endian: Endian,
    /// True for a single-file image (`n+1`, `n+2`) whose data follows the header.
    pub single_file: bool,
    pub dims: Vec<u64>,
    pub datatype: i32,
    pub pixdim: [f64; 8],
    pub vox_offset: u64,
    pub slope: f64,
    pub inter: f64,
    pub cal_min: f64,
    pub cal_max: f64,
    pub qform_code: i32,
    pub sform_code: i32,
    pub quatern: [f64; 3],
    pub qoffset: [f64; 3],
    pub srow: [[f64; 4]; 3],
    pub xyzt_units: i32,
    pub descrip: String,
    pub aux_file: String,
    pub intent_code: i32,
    pub intent_name: String,
    pub dim_info: u8,
    pub slice_duration: f64,
}

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

struct Rd<'a> {
    b: &'a [u8],
    e: Endian,
}

impl Rd<'_> {
    fn arr<const N: usize>(&self, o: usize) -> [u8; N] {
        let mut a = [0u8; N];
        if let Some(s) = self.b.get(o..o + N) {
            a.copy_from_slice(s);
        }
        a
    }

    fn i16(&self, o: usize) -> i16 {
        let a = self.arr::<2>(o);
        if self.e == Endian::Little { i16::from_le_bytes(a) } else { i16::from_be_bytes(a) }
    }

    fn i32(&self, o: usize) -> i32 {
        let a = self.arr::<4>(o);
        if self.e == Endian::Little { i32::from_le_bytes(a) } else { i32::from_be_bytes(a) }
    }

    fn i64(&self, o: usize) -> i64 {
        let a = self.arr::<8>(o);
        if self.e == Endian::Little { i64::from_le_bytes(a) } else { i64::from_be_bytes(a) }
    }

    fn f32(&self, o: usize) -> f64 {
        let a = self.arr::<4>(o);
        (if self.e == Endian::Little { f32::from_le_bytes(a) } else { f32::from_be_bytes(a) }) as f64
    }

    fn f64(&self, o: usize) -> f64 {
        let a = self.arr::<8>(o);
        if self.e == Endian::Little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }
    }

    fn text(&self, o: usize, n: usize) -> String {
        let s = self.b.get(o..o + n).unwrap_or_default();
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).trim().to_string()
    }
}

/// The byte order of a header, from its `sizeof_hdr` field.
fn header_endian(b: &[u8]) -> Option<(Endian, i32)> {
    let a: [u8; 4] = b.get(..4)?.try_into().ok()?;
    for (e, v) in [(Endian::Little, i32::from_le_bytes(a)), (Endian::Big, i32::from_be_bytes(a))] {
        if v == 348 || v == 540 {
            return Some((e, v));
        }
    }
    None
}

/// Datatype codes this reader understands: the dtype, samples per voxel (3
/// for RGB, 4 for RGBA).
fn datatype(code: i32) -> Option<(DType, usize)> {
    Some(match code {
        2 => (DType::U8, 1),
        4 => (DType::I16, 1),
        8 => (DType::I32, 1),
        16 => (DType::F32, 1),
        32 => (DType::C64, 1),
        64 => (DType::F64, 1),
        128 => (DType::U8, 3),
        256 => (DType::I8, 1),
        512 => (DType::U16, 1),
        768 => (DType::U32, 1),
        1024 => (DType::I64, 1),
        1280 => (DType::U64, 1),
        1792 => (DType::C128, 1),
        2304 => (DType::U8, 4),
        _ => return None,
    })
}

/// Parses a NIfTI or Analyze header.  `b` holds at least the header's bytes
/// (348, or 540 for NIfTI-2).
pub fn parse_header(b: &[u8]) -> Result<Header, String> {
    let (endian, size) = header_endian(b).ok_or("this isn't a NIfTI or Analyze header")?;
    let r = Rd { b, e: endian };
    if size == 540 {
        // "n+2" marks a single file, "ni2" a header whose data is in an .img.
        let single = b.get(4..8) == Some(b"n+2\0");
        if b.len() < 540 || !(single || b.get(4..8) == Some(b"ni2\0")) {
            return Err("this isn't a NIfTI-2 header".into());
        }
        let nd = r.i64(16);
        if !(1..=7).contains(&nd) {
            return Err(format!("the header says {nd} dimensions"));
        }
        let dims = (0..nd as usize).map(|i| r.i64(24 + 8 * i).max(0) as u64).collect();
        let mut pixdim = [0f64; 8];
        for (i, p) in pixdim.iter_mut().enumerate() {
            *p = r.f64(104 + 8 * i);
        }
        let srow = |o: usize| [r.f64(o), r.f64(o + 8), r.f64(o + 16), r.f64(o + 24)];
        return Ok(Header {
            variant: Variant::Nifti2,
            endian,
            single_file: single,
            dims,
            datatype: r.i16(12) as i32,
            pixdim,
            vox_offset: r.i64(168).max(0) as u64,
            slope: r.f64(176),
            inter: r.f64(184),
            cal_max: r.f64(192),
            cal_min: r.f64(200),
            qform_code: r.i32(344),
            sform_code: r.i32(348),
            quatern: [r.f64(352), r.f64(360), r.f64(368)],
            qoffset: [r.f64(376), r.f64(384), r.f64(392)],
            srow: [srow(400), srow(432), srow(464)],
            xyzt_units: r.i32(500),
            descrip: r.text(240, 80),
            aux_file: r.text(320, 24),
            intent_code: r.i32(504),
            intent_name: r.text(508, 16),
            dim_info: b[524],
            slice_duration: r.f64(208),
        });
    }
    if b.len() < 348 {
        return Err("the header is cut short".into());
    }
    let nd = r.i16(40) as i64;
    if !(1..=7).contains(&nd) {
        return Err(format!("the header says {nd} dimensions"));
    }
    let dims: Vec<u64> = (0..nd as usize).map(|i| r.i16(42 + 2 * i).max(0) as u64).collect();
    let mut pixdim = [0f64; 8];
    for (i, p) in pixdim.iter_mut().enumerate() {
        *p = r.f32(76 + 4 * i);
    }
    let magic = b.get(344..348).unwrap_or_default();
    let nifti = magic == b"n+1\0" || magic == b"ni1\0";
    if !nifti {
        // Analyze 7.5 has no magic: accept only a header whose datatype and
        // bits per voxel agree, so other 348-byte structures aren't taken for one.
        let (dt, samples) = datatype(r.i16(70) as i32).ok_or("the datatype isn't one Analyze defines")?;
        if r.i16(72) as usize != dt.size() * 8 * samples {
            return Err("the bits per voxel don't match the datatype".into());
        }
        if dims.contains(&0) {
            return Err("a dimension is zero".into());
        }
    }
    let srow = |o: usize| [r.f32(o), r.f32(o + 4), r.f32(o + 8), r.f32(o + 12)];
    Ok(Header {
        variant: if nifti { Variant::Nifti1 } else { Variant::Analyze },
        endian,
        single_file: magic == b"n+1\0",
        dims,
        datatype: r.i16(70) as i32,
        pixdim,
        vox_offset: r.f32(108).max(0.0) as u64,
        slope: if nifti { r.f32(112) } else { 0.0 },
        inter: if nifti { r.f32(116) } else { 0.0 },
        cal_max: r.f32(124),
        cal_min: r.f32(128),
        qform_code: if nifti { r.i16(252) as i32 } else { 0 },
        sform_code: if nifti { r.i16(254) as i32 } else { 0 },
        quatern: [r.f32(256), r.f32(260), r.f32(264)],
        qoffset: [r.f32(268), r.f32(272), r.f32(276)],
        srow: [srow(280), srow(296), srow(312)],
        xyzt_units: if nifti { b[123] as i32 } else { 0 },
        descrip: r.text(148, 80),
        aux_file: r.text(228, 24),
        intent_code: if nifti { r.i16(68) as i32 } else { 0 },
        intent_name: if nifti { r.text(328, 16) } else { String::new() },
        dim_info: if nifti { b[39] } else { 0 },
        slice_duration: r.f32(132),
    })
}

//endregion

/*┌─────────────────╮
| Orientation     |
└─────────────────╯*/
//region Orientation

/// The 3x4 affine from voxel indices to RAS+ millimetres, from the sform, else
/// the qform; `None` when the header has neither.
pub fn affine(h: &Header) -> Option<[[f64; 4]; 3]> {
    if h.sform_code > 0 {
        let m = h.srow;
        if m.iter().flatten().all(|v| v.is_finite()) {
            return Some(m);
        }
    }
    if h.qform_code > 0 {
        let [b, c, d] = h.quatern;
        let a2 = 1.0 - (b * b + c * c + d * d);
        // A rotation of exactly 180 degrees can leave a slightly negative square.
        let a = if a2 > 1e-7 { a2.sqrt() } else { 0.0 };
        let (b, c, d) = if a2 < -1e-5 {
            let n = (b * b + c * c + d * d).sqrt();
            (b / n, c / n, d / n)
        } else {
            (b, c, d)
        };
        let r = [
            [a * a + b * b - c * c - d * d, 2.0 * (b * c - a * d), 2.0 * (b * d + a * c)],
            [2.0 * (b * c + a * d), a * a + c * c - b * b - d * d, 2.0 * (c * d - a * b)],
            [2.0 * (b * d - a * c), 2.0 * (c * d + a * b), a * a + d * d - c * c - b * b],
        ];
        let qfac = if h.pixdim[0] < 0.0 { -1.0 } else { 1.0 };
        let s = [h.pixdim[1], h.pixdim[2], qfac * h.pixdim[3]];
        let m: [[f64; 4]; 3] = std::array::from_fn(|i| [r[i][0] * s[0], r[i][1] * s[1], r[i][2] * s[2], h.qoffset[i]]);
        if m.iter().flatten().all(|v| v.is_finite()) {
            return Some(m);
        }
    }
    None
}

/// Which way each of the three array axes points, as body directions, and the
/// voxel size along each; `None` unless the three axes point three different
/// ways.
pub fn orientation(m: &[[f64; 4]; 3]) -> Option<([Dir; 3], [f64; 3])> {
    let mut dirs = [Dir::R; 3];
    let mut size = [0f64; 3];
    let mut used = [false; 3];
    for (axis, d) in dirs.iter_mut().enumerate() {
        let col = [m[0][axis], m[1][axis], m[2][axis]];
        size[axis] = (col[0] * col[0] + col[1] * col[1] + col[2] * col[2]).sqrt();
        let (k, v) = col.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
        if used[k] || v.abs() < 1e-9 {
            return None;
        }
        used[k] = true;
        *d = match (k, *v > 0.0) {
            (0, true) => Dir::R,
            (0, false) => Dir::L,
            (1, true) => Dir::A,
            (1, false) => Dir::P,
            (2, true) => Dir::S,
            _ => Dir::I,
        };
    }
    Some((dirs, size))
}

//endregion

/// The scale a unit code gives to millimetres, and the unit's name.
fn spatial_unit(xyzt: i32) -> (f64, &'static str) {
    match xyzt & 7 {
        1 => (1000.0, "mm"),
        3 => (1.0, "um"),
        _ => (1.0, "mm"),
    }
}

fn time_unit(xyzt: i32) -> &'static str {
    match xyzt & 0x38 {
        8 => "s",
        16 => "ms",
        24 => "us",
        32 => "Hz",
        _ => "",
    }
}

/// Builds the volume for a header whose data is `source`.
pub fn volume(h: &Header, name: &str, source: Arc<dyn ByteSource>) -> Result<Volume, String> {
    let (dtype, samples) =
        datatype(h.datatype).ok_or_else(|| format!("datatype code {} isn't supported", h.datatype))?;
    let n = h.dims.len();
    if h.dims.iter().any(|&d| d == 0 || d > (1 << 31)) {
        return Err("a dimension is zero or absurdly large".into());
    }
    // 1. Strides: the first dimension varies fastest, after the colour samples.
    let mut stride = samples as u64;
    let mut axes: Vec<Axis> = Vec::new();
    for (i, &d) in h.dims.iter().enumerate() {
        let st = usize::try_from(stride).map_err(|_| "the volume is absurdly large")?;
        let len = d as usize;
        axes.push(Axis::new(len, st, Role::Other).named(["x", "y", "z", "t", "u", "v", "w"][i]));
        stride = stride.checked_mul(d).filter(|&s| s < 1 << 50).ok_or("the volume is absurdly large")?;
    }
    if samples > 1 {
        axes.push(Axis::new(samples, 1, Role::Color).named("colour"));
    }
    // 2. Orientation: roles by body direction, else by position.
    let aff = affine(h);
    let oriented = aff.as_ref().and_then(orientation).filter(|_| n >= 3);
    let (unit_scale, unit) = spatial_unit(h.xyzt_units);
    let spatial = n.min(3);
    match oriented {
        Some((dirs, size)) => {
            for i in 0..3 {
                axes[i].dir = Some(dirs[i]);
                axes[i].role = dirs[i].role();
                axes[i].spacing = size[i] * unit_scale;
            }
        }
        None => {
            for i in 0..spatial {
                axes[i].role = [Role::X, Role::Y, Role::Z][i];
                if h.pixdim[i + 1].is_finite() && h.pixdim[i + 1] > 0.0 {
                    axes[i].spacing = h.pixdim[i + 1] * unit_scale;
                }
            }
        }
    }
    if n >= 4 {
        axes[3].role = Role::T;
    }
    let mut v = Volume::new(name, h.variant.name(), axes, dtype, h.endian, source);
    v.unit = unit.to_string();
    if h.slope.is_finite() && h.slope != 0.0 && !(h.slope == 1.0 && h.inter == 0.0) && samples == 1 {
        v.slope = h.slope;
        v.inter = if h.inter.is_finite() { h.inter } else { 0.0 };
    }
    if h.cal_max > h.cal_min && h.cal_max.is_finite() && h.cal_min.is_finite() {
        v.display_range = Some((h.cal_min, h.cal_max));
    }
    if (3000..3100).contains(&h.intent_code) {
        v.no_picture = Some("a CIFTI file (brain-ordinate data, not an image)".into());
    }
    // 3. What the inspector shows.
    v.fact("Format", h.variant.name());
    v.fact("Dimensions", h.dims.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" x "));
    v.fact(
        "Data type",
        format!(
            "{}{} (code {})",
            dtype.name(),
            if samples > 1 { format!(", {samples} samples") } else { String::new() },
            h.datatype
        ),
    );
    let sizes: Vec<String> = (1..=n.min(3)).map(|i| format!("{:.4}", h.pixdim[i] * unit_scale)).collect();
    v.fact("Voxel size", format!("{} {}", sizes.join(" x "), unit));
    if n >= 4 && h.pixdim[4] > 0.0 && !time_unit(h.xyzt_units).is_empty() {
        v.fact("Time step", format!("{} {}", h.pixdim[4], time_unit(h.xyzt_units)));
    }
    if let Some(a) = &aff {
        if let Some((dirs, _)) = orientation(a) {
            v.fact("Orientation", dirs.iter().map(|d| d.letter()).collect::<String>() + " (axis directions, i j k)");
        } else {
            v.fact("Orientation", "oblique or ambiguous; shown in array order");
        }
        v.fact(
            "Orientation source",
            if h.sform_code > 0 {
                format!("sform (code {})", h.sform_code)
            } else {
                format!("qform (code {})", h.qform_code)
            },
        );
    } else {
        v.fact("Orientation", "not given; shown in array order");
    }
    if v.slope != 1.0 || v.inter != 0.0 {
        v.fact("Scaling", format!("value = {} x stored + {}", v.slope, v.inter));
    }
    if v.display_range.is_some() {
        v.fact("Display range", format!("{} to {}", h.cal_min, h.cal_max));
    }
    v.fact("Description", &h.descrip);
    v.fact("Auxiliary file", &h.aux_file);
    if h.intent_code != 0 {
        v.fact(
            "Intent",
            format!(
                "{}{}",
                h.intent_code,
                if h.intent_name.is_empty() { String::new() } else { format!(" ({})", h.intent_name) }
            ),
        );
    }
    v.fit()?;
    Ok(v)
}

/*┌─────────────────╮
| Files           |
└─────────────────╯*/
//region Files

fn bad(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// The first bytes of a file's data, inflated when it is gzip-compressed.
fn head_bytes(file: &Arc<File>, len: u64, want: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut raw = vec![0u8; len.min(want as u64) as usize];
    crate::source::read_exact_at(file, &mut raw, 0)?;
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        Inflater::new(FileRange::new(file.clone(), 0, len), Wrapper::Gzip, want as u64)
            .take(want as u64)
            .read_to_end(&mut out)?;
        return Ok((out, true));
    }
    Ok((raw, false))
}

/// Recognises NIfTI and Analyze by content: a single file, a gzip of one, or
/// a `.hdr`/`.img` pair.
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if head.starts_with(&[0x1f, 0x8b]) {
        // The first bytes of the gzip data are inflated from what was read.
        let mut out = Vec::new();
        let _ = Inflater::new(head, Wrapper::Gzip, 1024).take(560).read_to_end(&mut out);
        return out.len() >= 348 && parse_header(&out).is_ok_and(|h| h.single_file);
    }
    // The data half of a pair has no signature of its own: its header decides.
    if ext == "img" {
        return analyze_header_for(path).is_some();
    }
    header_endian(head).is_some() && parse_header(head).is_ok_and(|h| h.variant != Variant::Analyze || ext == "hdr")
}

/// The `.hdr` beside an `.img` when it is a header.
fn analyze_header_for(img: &Path) -> Option<PathBuf> {
    let hdr = img.with_extension("hdr");
    let mut b = vec![0u8; 540];
    let n = File::open(&hdr).ok()?.read(&mut b).ok()?;
    b.truncate(n);
    parse_header(&b).ok().filter(|h| !h.single_file).map(|_| hdr)
}

/// Opens a NIfTI or Analyze volume.  `path` may be the `.img` of a pair.
pub fn open(path: &Path, reach: u64) -> Result<(Volume, Vec<PathBuf>), LoadError> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let header_path = if ext == "img" {
        analyze_header_for(path).ok_or_else(|| bad("there is no header beside this .img"))?
    } else {
        path.to_path_buf()
    };
    let file = Arc::new(File::open(&header_path)?);
    let len = file.metadata()?.len();
    let (head, gz) = head_bytes(&file, len, 560)?;
    let h = parse_header(&head).map_err(bad)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (dtype, samples) =
        datatype(h.datatype).ok_or_else(|| LoadError::Unsupported(format!("NIfTI datatype {}", h.datatype)))?;
    let mut data_bytes = samples as u64 * dtype.size() as u64;
    for &d in &h.dims {
        data_bytes = data_bytes.checked_mul(d).ok_or_else(|| bad("the volume is absurdly large"))?;
    }
    let mut neighbours = Vec::new();
    let source: Arc<dyn ByteSource> = if h.single_file {
        let offset = h.vox_offset.max(h.variant.header_len() as u64 + 4);
        if gz {
            gz_source(file.clone(), len, offset, data_bytes, reach)
        } else {
            let avail = len.saturating_sub(offset);
            Arc::new(FileSource::new(File::open(&header_path)?, offset, avail))
        }
    } else {
        // A pair: the data is in the `.img` beside the header.
        let stem = header_path.with_extension("");
        let mut found = None;
        for ext in ["img", "img.gz"] {
            let c = PathBuf::from(format!("{}.{ext}", stem.display()));
            if c.exists() {
                found = Some(c);
                break;
            }
        }
        let img = found.ok_or_else(|| bad("the .img file beside this header is missing"))?;
        let f = Arc::new(File::open(&img)?);
        let flen = f.metadata()?.len();
        neighbours.push(if header_path == path { img.clone() } else { header_path.clone() });
        let (peek, is_gz) = head_bytes(&f, flen, 4)?;
        let _ = peek;
        if is_gz {
            gz_source(f, flen, h.vox_offset, data_bytes, reach)
        } else {
            Arc::new(FileSource::new(File::open(&img)?, h.vox_offset, flen.saturating_sub(h.vox_offset)))
        }
    };
    let v = volume(&h, &name, source).map_err(bad)?;
    Ok((v, neighbours))
}

/// A gzip-compressed volume as a stream, `skip` bytes in (the header) and
/// `data` bytes long, whose real size comes from the gzip trailer when the
/// file is a single member under 4 GB.
fn gz_source(file: Arc<File>, len: u64, skip: u64, data: u64, reach: u64) -> Arc<dyn ByteSource> {
    let mut avail = data;
    let mut trailer = [0u8; 4];
    if len > 22 && crate::source::read_exact_at(&file, &mut trailer, len - 4).is_ok() {
        let isize = u32::from_le_bytes(trailer) as u64;
        let total = skip + data;
        // A short file clips the volume; otherwise the declared size holds.
        if total >> 32 == 0 && isize < total {
            avail = isize.saturating_sub(skip);
        }
    }
    let limit = skip.saturating_add(avail);
    Arc::new(StreamSource::new(avail, reach, move || {
        let mut r = Inflater::new(FileRange::new(file.clone(), 0, len), Wrapper::Gzip, limit);
        std::io::copy(&mut (&mut r).take(skip), &mut std::io::sink())?;
        Ok(Box::new(r))
    }))
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::source::MemSource;
    use crate::volume::{ChannelSel, PlaneKind, SliceView};

    /// A NIfTI-1 header and data, as a `.nii` byte vector.
    #[allow(clippy::too_many_arguments)]
    pub fn nifti1(
        dims: &[i16],
        datatype: i16,
        bitpix: i16,
        big: bool,
        data: &[u8],
        srow: Option<[[f32; 4]; 3]>,
        slope: f32,
        inter: f32,
    ) -> Vec<u8> {
        let mut h = vec![0u8; 352];
        let w16 = |h: &mut Vec<u8>, o: usize, v: i16| {
            h[o..o + 2].copy_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() })
        };
        let w32 = |h: &mut Vec<u8>, o: usize, v: f32| {
            h[o..o + 4].copy_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() })
        };
        h[0..4].copy_from_slice(&if big { 348i32.to_be_bytes() } else { 348i32.to_le_bytes() });
        w16(&mut h, 40, dims.len() as i16);
        for (i, d) in dims.iter().enumerate() {
            w16(&mut h, 42 + 2 * i, *d);
        }
        w16(&mut h, 70, datatype);
        w16(&mut h, 72, bitpix);
        for i in 0..8 {
            w32(&mut h, 76 + 4 * i, 1.0);
        }
        w32(&mut h, 108, 352.0);
        w32(&mut h, 112, slope);
        w32(&mut h, 116, inter);
        h[123] = 2;
        if let Some(m) = srow {
            w16(&mut h, 254, 1);
            for (r, row) in m.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    w32(&mut h, 280 + 16 * r + 4 * c, *v);
                }
            }
        }
        h[344..348].copy_from_slice(b"n+1\0");
        h.extend_from_slice(data);
        h
    }

    fn open_bytes(bytes: Vec<u8>) -> Result<Volume, String> {
        let h = parse_header(&bytes)?;
        let off = h.vox_offset as usize;
        volume(&h, "t", Arc::new(MemSource(bytes[off..].to_vec())))
    }

    fn i16s(v: &[i16], big: bool) -> Vec<u8> {
        v.iter().flat_map(|x| if big { x.to_be_bytes() } else { x.to_le_bytes() }).collect()
    }

    fn view(kind: PlaneKind, slice: usize) -> SliceView {
        SliceView { kind, slice, t: 0, neurological: false }
    }

    #[test]
    fn both_byte_orders_read_the_same() {
        let vals: Vec<i16> = (0..24).collect();
        for big in [false, true] {
            let b = nifti1(&[4, 3, 2], 4, 16, big, &i16s(&vals, big), None, 0.0, 0.0);
            let v = open_bytes(b).unwrap();
            assert_eq!(v.shape(), vec![4, 3, 2]);
            assert_eq!(v.value_at(&[1, 2, 1]).unwrap(), (1 + 2 * 4 + 12) as f64, "big={big}");
        }
    }

    #[test]
    fn slope_and_intercept_scale_values() {
        let vals: Vec<i16> = vec![0, 10, 20, 30];
        let b = nifti1(&[2, 2, 1], 4, 16, false, &i16s(&vals, false), None, 0.5, -3.0);
        let v = open_bytes(b).unwrap();
        assert_eq!(v.value_at(&[1, 1, 0]).unwrap(), 30.0 * 0.5 - 3.0);
        // A slope of zero means no scaling.
        let b = nifti1(&[2, 2, 1], 4, 16, false, &i16s(&vals, false), None, 0.0, 5.0);
        assert_eq!(open_bytes(b).unwrap().value_at(&[1, 1, 0]).unwrap(), 30.0);
    }

    #[test]
    fn the_sform_orients_the_axes() {
        // LPS-stored (x grows left, y back, z up), as many scanners write them.
        let m = [[-2.0, 0.0, 0.0, 0.0], [0.0, -2.0, 0.0, 0.0], [0.0, 0.0, 3.0, 0.0]];
        let vals: Vec<i16> = (0..24).collect();
        let b = nifti1(&[4, 3, 2], 4, 16, false, &i16s(&vals, false), Some(m), 0.0, 0.0);
        let v = open_bytes(b).unwrap();
        let dirs: Vec<_> = v.axes.iter().map(|a| a.dir).collect();
        assert_eq!(dirs, vec![Some(Dir::L), Some(Dir::P), Some(Dir::S)]);
        assert_eq!(v.axes[2].spacing, 3.0);
        let map = v.default_map().unwrap();
        let s = v.extract(&map, &view(PlaneKind::Axial, 0), ChannelSel::All, None).unwrap();
        // x already points left and y back: no mirroring for the axial picture.
        assert_eq!(s.edges, [Some('R'), Some('L'), Some('A'), Some('P')]);
        assert_eq!(s.value(0, 0, 0), Some(0.0));
        // Coronal slices count towards the front, so slice 0 is the back (y = 2
        // here, since y grows backwards), and the head is up (z = 1).
        let c = v.extract(&map, &view(PlaneKind::Coronal, 0), ChannelSel::All, None).unwrap();
        assert_eq!((c.width, c.height), (4, 2));
        assert_eq!(c.value(0, 0, 0), Some(20.0));
        assert_eq!(c.edges, [Some('R'), Some('L'), Some('S'), Some('I')]);
    }

    #[test]
    fn rgb_volumes_get_a_colour_axis() {
        let data: Vec<u8> = (0..24).collect();
        let b = nifti1(&[2, 2, 2], 128, 24, false, &data, None, 0.0, 0.0);
        let v = open_bytes(b).unwrap();
        assert_eq!(v.axes.iter().filter(|a| a.role == Role::Color).count(), 1);
        let map = v.default_map().unwrap();
        let s = v.extract(&map, &view(PlaneKind::Axial, 1), ChannelSel::All, None).unwrap();
        assert_eq!(s.samples, 3);
        assert_eq!(&s.data[..3], &[12.0, 13.0, 14.0]);
    }

    #[test]
    fn four_dimensional_files_have_a_time_axis() {
        let vals: Vec<i16> = (0..2 * 2 * 2 * 3).collect();
        let b = nifti1(&[2, 2, 2, 3], 4, 16, false, &i16s(&vals, false), None, 0.0, 0.0);
        let v = open_bytes(b).unwrap();
        let map = v.default_map().unwrap();
        assert_eq!(v.time_count(&map), 3);
        let s = v.extract(&map, &SliceView { t: 2, ..view(PlaneKind::Axial, 0) }, ChannelSel::All, None).unwrap();
        assert_eq!(s.value(0, 0, 0), Some(16.0));
    }

    #[test]
    fn analyze_headers_need_a_consistent_datatype() {
        let mut h = nifti1(&[4, 3, 2], 4, 16, false, &[], None, 0.0, 0.0);
        h[344..348].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(parse_header(&h).unwrap().variant, Variant::Analyze);
        h[72] = 8;
        assert!(parse_header(&h).is_err());
        assert!(parse_header(&[0u8; 348]).is_err());
    }

    #[test]
    fn nonsense_headers_are_errors() {
        let mut h = nifti1(&[4, 3, 2], 4, 16, false, &[], None, 0.0, 0.0);
        h[40] = 9;
        assert!(parse_header(&h).is_err());
        let mut h = nifti1(&[4, 3, 2], 4, 16, false, &[], None, 0.0, 0.0);
        h[42..44].copy_from_slice(&0i16.to_le_bytes());
        assert!(open_bytes(h).is_err());
        let h = nifti1(&[4, 3, 2], 999, 16, false, &[], None, 0.0, 0.0);
        assert!(open_bytes(h).is_err());
        // Huge dimensions with no data.
        let h = nifti1(&[32767, 32767, 32767, 32767], 16, 32, false, &[], None, 0.0, 0.0);
        assert!(open_bytes(h).is_err());
    }

    #[test]
    fn oblique_and_degenerate_orientations_are_left_unoriented() {
        // Two axes pointing the same way.
        let m = [[1.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        assert!(orientation(&m).is_none());
        let id = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        assert_eq!(orientation(&id).unwrap().0, [Dir::R, Dir::A, Dir::S]);
    }

    #[test]
    fn qform_gives_an_affine_with_the_flip_factor() {
        let mut h = parse_header(&nifti1(&[4, 3, 2], 4, 16, false, &[], None, 0.0, 0.0)).unwrap();
        h.qform_code = 1;
        h.quatern = [0.0, 0.0, 0.0];
        h.pixdim = [-1.0, 2.0, 2.0, 3.0, 1.0, 1.0, 1.0, 1.0];
        let m = affine(&h).unwrap();
        assert_eq!(orientation(&m).unwrap().0, [Dir::R, Dir::A, Dir::I]);
    }
}
