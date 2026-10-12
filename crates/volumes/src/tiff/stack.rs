//! TIFF files as volumes: multi-page stacks (every page a slice), OME-TIFF
//! hyperstacks (Z, C and T from the OME-XML), and ImageJ hyperstacks, which
//! may store their pages back to back after a single directory.

use super::ome;
use crate::dtype::{DType, Endian};
use crate::source::ByteSource;
use crate::volume::{Axis, ChannelInfo, Role, Volume};
use slerp_tiff::raster::{self, Options, Sample, compression_name, photometric_name};
use slerp_tiff::{Page, Tiff, tag};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Where a plane's page comes from.
#[derive(Clone, Debug)]
pub enum PageSpec {
    /// The directory at this offset.
    Ifd(u64),
    /// The page of a directory shifted forward by this many bytes (ImageJ's
    /// contiguous hyperstacks).
    Shifted(u64, u64),
    /// Not in this file (an OME plane stored in a companion file).
    Missing,
}

/// The pages of a stack as one flat run of samples: page after page, each in
/// rows of interleaved samples.  Pages are decoded when a read needs them and
/// the last few stay in memory.
pub struct PagedSource {
    tiff: Arc<Tiff>,
    pages: Vec<PageSpec>,
    template: Page,
    dtype: DType,
    samples: usize,
    page_bytes: u64,
    cache: Mutex<Vec<(usize, Arc<Vec<u8>>)>>,
    cache_pages: usize,
}

/// The volume model's name for a TIFF sample type.
pub fn dtype_of(s: Sample) -> DType {
    match s {
        Sample::U8 => DType::U8,
        Sample::I8 => DType::I8,
        Sample::U16 => DType::U16,
        Sample::I16 => DType::I16,
        Sample::U32 => DType::U32,
        Sample::I32 => DType::I32,
        Sample::U64 => DType::U64,
        Sample::I64 => DType::I64,
        Sample::F16 => DType::F16,
        Sample::F32 => DType::F32,
        Sample::F64 => DType::F64,
    }
}

/// The sample type and count of a decoded page of this layout, without
/// decoding it.
pub fn layout(p: &Page) -> Result<(DType, usize), String> {
    raster::check(p)?;
    let lossy = matches!(
        p.compression,
        raster::comp::JPEG | raster::comp::APERIO_J2K_YCC | raster::comp::APERIO_J2K_RGB | raster::comp::J2K
    );
    Ok(match p.photometric {
        3 | 5 | 6 => (DType::U8, 3),
        _ => {
            let samples = if p.samples == 2 && matches!(p.photometric, 0 | 1) { 1 } else { p.samples };
            let dtype = if lossy {
                if p.bits.first().copied().unwrap_or(8) > 8 { DType::U16 } else { DType::U8 }
            } else {
                dtype_of(raster::native_sample(p)?)
            };
            (dtype, samples)
        }
    })
}

impl PagedSource {
    pub fn new(tiff: Arc<Tiff>, pages: Vec<PageSpec>, template: Page) -> Result<PagedSource, String> {
        let (dtype, samples) = layout(&template)?;
        let page_bytes = (template.width)
            .checked_mul(template.height)
            .and_then(|n| n.checked_mul(samples as u64 * dtype.size() as u64))
            .ok_or("a page is absurdly large")?;
        if page_bytes > 3 << 30 {
            return Err("a page is too large to decode at once".into());
        }
        let cache_pages = ((256u64 << 20) / page_bytes.max(1)).clamp(2, 8) as usize;
        Ok(PagedSource {
            tiff,
            pages,
            template,
            dtype,
            samples,
            page_bytes,
            cache: Mutex::new(Vec::new()),
            cache_pages,
        })
    }

    pub fn page_bytes(&self) -> u64 {
        self.page_bytes
    }

    fn page_data(&self, i: usize) -> io::Result<Arc<Vec<u8>>> {
        let other = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
        let mut cache = self.cache.lock().map_err(|_| io::Error::other("the page cache is poisoned"))?;
        if let Some(pos) = cache.iter().position(|(k, _)| *k == i) {
            let hit = cache.remove(pos);
            let data = hit.1.clone();
            cache.push(hit);
            return Ok(data);
        }
        let spec = self.pages.get(i).ok_or_else(crate::source::eof)?;
        let data = match spec {
            PageSpec::Missing => Arc::new(vec![0u8; self.page_bytes as usize]),
            PageSpec::Ifd(_) | PageSpec::Shifted(..) => {
                let (off, shift) = match spec {
                    PageSpec::Ifd(o) => (*o, 0),
                    PageSpec::Shifted(o, s) => (*o, *s),
                    PageSpec::Missing => unreachable!(),
                };
                let mut p = self.tiff.page(off).map_err(other)?;
                p.offset_shift = shift;
                if (p.width, p.height) != (self.template.width, self.template.height) {
                    return Err(other(format!("page {} has a different size from the first", i + 1)));
                }
                let r = p.decode(&self.tiff, &Options::full()).map_err(other)?;
                if dtype_of(r.sample) != self.dtype || r.samples != self.samples {
                    return Err(other(format!("page {} has a different sample layout from the first", i + 1)));
                }
                Arc::new(r.bytes)
            }
        };
        cache.push((i, data.clone()));
        while cache.len() > self.cache_pages {
            cache.remove(0);
        }
        Ok(data)
    }
}

impl ByteSource for PagedSource {
    fn len(&self) -> u64 {
        self.page_bytes * self.pages.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(crate::source::eof)?;
        if end > self.len() {
            return Err(crate::source::eof());
        }
        let mut at = offset;
        let mut done = 0usize;
        while done < buf.len() {
            let page = (at / self.page_bytes) as usize;
            let inside = (at % self.page_bytes) as usize;
            let n = (buf.len() - done).min(self.page_bytes as usize - inside);
            let data = self.page_data(page)?;
            buf[done..done + n].copy_from_slice(&data[inside..inside + n]);
            at += n as u64;
            done += n;
        }
        Ok(())
    }
}

/*┌─────────────────╮
| Hyperstacks     |
└─────────────────╯*/
//region Hyperstacks

/// The order planes are stored in: for "XYCZT", C varies fastest, then Z,
/// then T.  Returns the three letters fastest first.
fn plane_order(order: &str) -> Vec<char> {
    order.chars().filter(|c| matches!(c, 'Z' | 'C' | 'T')).collect()
}

/// An ImageJ description's `key=value` lines.
fn imagej_fields(desc: &str) -> Vec<(String, String)> {
    desc.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

pub fn looks_like_imagej(desc: &str) -> bool {
    desc.starts_with("ImageJ=")
}

/// The default colours of channels: red, green, blue, then cyan, magenta,
/// yellow and grey.
pub fn default_channel_colors(n: usize) -> Vec<[u8; 3]> {
    const C: [[u8; 3]; 7] =
        [[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 255], [255, 255, 0], [255, 255, 255]];
    if n == 1 {
        return vec![[255, 255, 255]];
    }
    (0..n).map(|i| C[i % C.len()]).collect()
}

/// Pixel size in micrometres from a page's resolution tags, or an ImageJ
/// unit.
fn resolution_um(p: &Page, unit: Option<&str>) -> Option<[f64; 2]> {
    let (x, y, u) = p.resolution?;
    let per = match (unit, u) {
        (Some("micron" | "um" | "µm" | "μm"), _) => 1.0,
        (Some("nm"), _) => 1e-3,
        (Some("mm"), _) => 1e3,
        (Some("cm"), _) | (None, 3) => 1e4,
        (Some("inch" | "in"), _) | (None, 2) => 25400.0,
        _ => return None,
    };
    Some([per / x, per / y])
}

/// What the volume's axes are, and how its pages are arranged.
pub struct Plan {
    pub pages: Vec<PageSpec>,
    /// Axes outside the page, slowest first: (role, length).
    pub dims: Vec<(Role, usize)>,
    pub channels: Vec<ChannelInfo>,
    pub z_spacing: f64,
    pub xy_spacing: Option<[f64; 2]>,
    pub kind: String,
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// Plans an OME-TIFF.
pub fn plan_ome(t: &Tiff, offsets: &[u64], first: &Page, this_file: &str) -> Result<Plan, String> {
    let o = ome::parse(&first.description)?;
    let (_, spp) = layout(first)?;
    let c_planes = if o.channels.is_empty() { (o.size_c / spp.max(1)).max(1) } else { o.channels.len() };
    let (nz, nt) = (o.size_z, o.size_t);
    let total = nz
        .checked_mul(c_planes)
        .and_then(|n| n.checked_mul(nt))
        .filter(|&n| n <= super::MAX_PAGES)
        .ok_or("the OME stack is absurdly large")?;
    let order = plane_order(&o.order);
    let len_of = |d: char| match d {
        'Z' => nz,
        'C' => c_planes,
        _ => nt,
    };
    // The plane number of (z, c, t): the first letter of the order is fastest.
    let ordinal = |z: usize, c: usize, tt: usize| -> usize {
        let idx = |d: char| match d {
            'Z' => z,
            'C' => c,
            _ => tt,
        };
        let mut n = 0usize;
        let mut mul = 1usize;
        for &d in &order {
            n += idx(d).min(len_of(d).saturating_sub(1)) * mul;
            mul *= len_of(d);
        }
        n
    };
    let mut map: Vec<PageSpec> = vec![PageSpec::Missing; total];
    let mut elsewhere = 0usize;
    if o.tiff_data.is_empty() {
        for (i, slot) in map.iter_mut().enumerate().take(offsets.len()) {
            *slot = PageSpec::Ifd(offsets[i]);
        }
    } else {
        for td in &o.tiff_data {
            if !td.is_in(this_file, o.uuid.as_deref()) {
                elsewhere += td.plane_count.unwrap_or(1);
                continue;
            }
            let start = ordinal(td.first_z, td.first_c, td.first_t);
            let ifd0 = td.ifd.unwrap_or(0);
            let count = td.plane_count.unwrap_or(if td.ifd.is_none() { total } else { 1 });
            for i in 0..count.min(total) {
                if let (Some(slot), Some(&off)) = (map.get_mut(start + i), offsets.get(ifd0 + i)) {
                    *slot = PageSpec::Ifd(off);
                }
            }
        }
    }
    // Drop Z, C or T indices that no plane in this file covers (the other
    // channels of a series split over several files).
    let mut keep: Vec<Vec<usize>> = Vec::new();
    for d in ['Z', 'C', 'T'] {
        let n = len_of(d);
        let mut k = Vec::new();
        for i in 0..n {
            let any = (0..total).any(|p| {
                let (mut rem, mut idx_d) = (p, 0usize);
                for &dd in &order {
                    let l = len_of(dd);
                    if dd == d {
                        idx_d = rem % l;
                    }
                    rem /= l;
                }
                idx_d == i && !matches!(map[p], PageSpec::Missing)
            });
            if any {
                k.push(i);
            }
        }
        keep.push(k);
    }
    if keep.iter().any(|k| k.is_empty()) {
        return Err("none of the planes the OME-XML lists are in this file".into());
    }
    let (kz, kc, kt) = (&keep[0], &keep[1], &keep[2]);
    let mut pages = Vec::new();
    // New plane order: the same dimension order over the kept indices.
    let new_len = |d: char| match d {
        'Z' => kz.len(),
        'C' => kc.len(),
        _ => kt.len(),
    };
    let new_total = kz.len() * kc.len() * kt.len();
    for p in 0..new_total {
        let (mut rem, mut z, mut c, mut tt) = (p, 0, 0, 0);
        for &d in &order {
            let l = new_len(d);
            let i = rem % l;
            rem /= l;
            match d {
                'Z' => z = kz[i],
                'C' => c = kc[i],
                _ => tt = kt[i],
            }
        }
        pages.push(map[ordinal(z, c, tt)].clone());
    }
    let mut dims: Vec<(Role, usize)> = Vec::new();
    for &d in order.iter().rev() {
        let (role, n) = match d {
            'Z' => (Role::Z, kz.len()),
            'C' => (Role::Channel, kc.len()),
            _ => (Role::T, kt.len()),
        };
        if n > 1 {
            dims.push((role, n));
        }
    }
    let defaults = default_channel_colors(kc.len());
    let channels: Vec<ChannelInfo> = kc
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let ch = o.channels.get(c);
            ChannelInfo {
                name: ch
                    .map(|c| c.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("Channel {}", c + 1)),
                color: ch
                    .and_then(|c| c.color)
                    .filter(|c| *c != [255, 255, 255] || kc.len() == 1)
                    .unwrap_or(defaults[i]),
            }
        })
        .collect();
    let mut facts = vec![
        ("OME image".to_string(), o.name.clone()),
        ("Dimension order".into(), o.order.clone()),
        ("OME pixel type".into(), o.pixel_type.clone()),
    ];
    facts.push(("Z, C, T sizes".into(), format!("{} x {} x {}", o.size_z, c_planes, o.size_t)));
    if o.images > 1 {
        facts.push(("Images in the file".into(), format!("{} (the first is shown)", o.images)));
    }
    if !o.acquired.is_empty() {
        facts.push(("Acquired".into(), o.acquired.clone()));
    }
    if let Some(ti) = o.time_increment {
        facts.push(("Time step".into(), format!("{ti} {}", o.time_unit)));
    }
    let mut warnings = Vec::new();
    if elsewhere > 0 {
        warnings.push(format!(
            "{elsewhere} planes of this series are in other files; only the planes in this file are shown."
        ));
    }
    let _ = t;
    Ok(Plan {
        pages,
        dims,
        channels,
        z_spacing: o.physical[2],
        xy_spacing: (o.physical[0] > 0.0 && o.physical[1] > 0.0).then_some([o.physical[0], o.physical[1]]),
        kind: "OME-TIFF".into(),
        facts,
        warnings,
    })
}

/// Plans an ImageJ hyperstack.
pub fn plan_imagej(t: &Tiff, offsets: &[u64], first: &Page) -> Result<Plan, String> {
    let f = imagej_fields(&first.description);
    let get = |k: &str| -> Option<usize> { f.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.parse().ok()) };
    let text = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let images = get("images").unwrap_or(offsets.len()).clamp(1, super::MAX_PAGES);
    let (mut nc, mut nz, mut nt) =
        (get("channels").unwrap_or(1).max(1), get("slices").unwrap_or(1).max(1), get("frames").unwrap_or(1).max(1));
    if nc.saturating_mul(nz).saturating_mul(nt) != images {
        (nc, nz, nt) = (1, images, 1);
    }
    // Pages: one directory each, or all after the first directory's data.
    let mut pages = Vec::new();
    if offsets.len() >= images {
        pages.extend(offsets.iter().take(images).map(|&o| PageSpec::Ifd(o)));
    } else if offsets.len() == 1 && first.compression == raster::comp::NONE && first.planar != 2 {
        let step = first.raw_bytes();
        for i in 0..images as u64 {
            pages.push(PageSpec::Shifted(offsets[0], i * step));
        }
    } else {
        pages.extend(offsets.iter().map(|&o| PageSpec::Ifd(o)));
        nz = pages.len();
        (nc, nt) = (1, 1);
    }
    let mut dims = Vec::new();
    // XYCZT: T slowest.
    for (role, n) in [(Role::T, nt), (Role::Z, nz), (Role::Channel, nc)] {
        if n > 1 {
            dims.push((role, n));
        }
    }
    let channels = if nc > 1 {
        default_channel_colors(nc)
            .into_iter()
            .enumerate()
            .map(|(i, color)| ChannelInfo { name: format!("Channel {}", i + 1), color })
            .collect()
    } else {
        Vec::new()
    };
    let unit = text("unit");
    let xy = resolution_um(first, unit.as_deref());
    let spacing = text("spacing").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let z_um = match unit.as_deref() {
        Some("nm") => spacing * 1e-3,
        Some("mm") => spacing * 1e3,
        Some("cm") => spacing * 1e4,
        _ => spacing,
    };
    let mut facts = vec![("ImageJ version".to_string(), text("ImageJ").unwrap_or_default())];
    facts.push(("Channels, slices, frames".into(), format!("{nc} x {nz} x {nt}")));
    for k in ["mode", "unit", "spacing", "finterval", "min", "max"] {
        if let Some(v) = text(k) {
            facts.push((k.to_string(), v));
        }
    }
    let _ = t;
    Ok(Plan {
        pages,
        dims,
        channels,
        z_spacing: z_um,
        xy_spacing: xy,
        kind: "ImageJ TIFF".into(),
        facts,
        warnings: Vec::new(),
    })
}

/// Plans an ordinary multi-page file: every page a slice.
pub fn plan_pages(offsets: &[u64]) -> Plan {
    let n = offsets.len();
    Plan {
        pages: offsets.iter().map(|&o| PageSpec::Ifd(o)).collect(),
        dims: if n > 1 { vec![(Role::Z, n)] } else { Vec::new() },
        channels: Vec::new(),
        z_spacing: 0.0,
        xy_spacing: None,
        kind: "TIFF".into(),
        facts: vec![("Pages".into(), n.to_string())],
        warnings: Vec::new(),
    }
}

//endregion

/// Facts every TIFF volume shows.
pub fn page_facts(p: &Page, t: &Tiff) -> Vec<(String, String)> {
    let mut f = vec![
        ("Container".to_string(), if t.big { "BigTIFF".into() } else { "TIFF".into() }),
        ("Size".into(), format!("{} x {}", p.width, p.height)),
        (
            "Samples".into(),
            format!(
                "{} x {}-bit{}",
                p.samples,
                p.bits.first().copied().unwrap_or(0),
                match p.sample_format {
                    2 => ", signed",
                    3 => ", float",
                    _ => "",
                }
            ),
        ),
        ("Photometric".into(), photometric_name(p.photometric).to_string()),
        ("Compression".into(), compression_name(p.compression)),
    ];
    if let Some((tw, th)) = p.tile {
        f.push(("Tiles".into(), format!("{tw} x {th}")));
    } else {
        f.push(("Rows per strip".into(), p.rows_per_strip.to_string()));
    }
    if p.predictor > 1 {
        f.push(("Predictor".into(), p.predictor.to_string()));
    }
    if p.planar == 2 {
        f.push(("Planar".into(), "separate planes".into()));
    }
    if !p.software.is_empty() {
        f.push(("Software".into(), p.software.clone()));
    }
    if let Some((x, y, u)) = p.resolution {
        f.push((
            "Resolution".into(),
            format!(
                "{x:.4} x {y:.4} per {}",
                match u {
                    2 => "inch",
                    3 => "centimetre",
                    _ => "unit",
                }
            ),
        ));
    }
    if !p.description.is_empty() && !ome::looks_like_ome(&p.description) {
        f.push(("Description".into(), p.description.chars().take(2000).collect()));
    }
    let _ = tag::DESCRIPTION;
    f
}

/// Opens the planes of a plan as a volume.
pub fn volume(t: &Arc<Tiff>, first: &Page, plan: Plan, name: &str) -> Result<Volume, String> {
    let (dtype, samples) = layout(first)?;
    let source = PagedSource::new(t.clone(), plan.pages.clone(), first.clone())?;
    let (w, h) = (first.width as usize, first.height as usize);
    let page_samples = w.checked_mul(h).and_then(|n| n.checked_mul(samples)).ok_or("a page is absurdly large")?;
    let mut axes = Vec::new();
    // Page axes: the last of `dims` varies fastest.
    let mut stride = page_samples;
    let mut page_axes = Vec::new();
    for &(role, n) in plan.dims.iter().rev() {
        page_axes.push(Axis::new(n, stride, role).named(match role {
            Role::Z => "z",
            Role::T => "t",
            Role::Channel => "channel",
            _ => "page",
        }));
        stride = stride.checked_mul(n).ok_or("the stack is absurdly large")?;
    }
    page_axes.reverse();
    axes.extend(page_axes);
    let xy = plan.xy_spacing.or_else(|| resolution_um(first, None));
    let mut y = Axis::new(h, w * samples, Role::Y).named("y");
    let mut x = Axis::new(w, samples, Role::X).named("x");
    if let Some([sx, sy]) = xy {
        x = x.with_spacing(sx);
        y = y.with_spacing(sy);
    }
    axes.push(y);
    axes.push(x);
    if samples > 1 {
        let colour = matches!(first.photometric, 2 | 3 | 5 | 6) && samples <= 4;
        axes.push(Axis::new(samples, 1, if colour { Role::Color } else { Role::Channel }).named("samples"));
    }
    if plan.z_spacing > 0.0
        && let Some(z) = axes.iter_mut().find(|a| a.role == Role::Z)
    {
        z.spacing = plan.z_spacing;
    }
    let mut v = Volume::new(name, plan.kind.clone(), axes, dtype, Endian::Little, Arc::new(source));
    v.unit = if xy.is_some() { "um".into() } else { String::new() };
    v.invert = first.photometric == 0 && samples == 1;
    v.channels = plan.channels;
    if samples > 1 && v.channels.is_empty() && !matches!(first.photometric, 2 | 3 | 5 | 6) {
        v.channels = default_channel_colors(samples)
            .into_iter()
            .enumerate()
            .map(|(i, color)| ChannelInfo { name: format!("Sample {}", i + 1), color })
            .collect();
    }
    v.facts = plan.facts;
    v.facts.extend(page_facts(first, t));
    v.warnings = plan.warnings;
    v.fit()?;
    Ok(v)
}

/// A name for a stack's file.
pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
