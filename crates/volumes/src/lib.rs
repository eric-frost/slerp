//! Scientific arrays, medical volumes and microscopy images.
//!
//! One model, [`Volume`], holds an n-dimensional array that stays in its file
//! (or compressed stream) until a plane is wanted.  Format modules read the
//! headers of NumPy, NIfTI, MetaImage, NRRD, TIFF stacks and the rest into
//! that model; thumbnails cut the middle slice and the viewer cuts whichever
//! plane is asked for.

#[rustfmt::skip]
mod colormap_data;
pub mod dtype;
pub mod hdf5;
pub mod layout;
pub mod metaimage;
pub mod netcdf;
pub mod nifti;
pub mod npy;
pub mod nrrd;
pub mod render;
pub mod source;
pub mod tiff;
pub mod volume;
pub mod zip;

pub use dtype::{DType, Endian};
pub use render::{ChannelStyle, ColorMap, Layout, Style};
pub use volume::{Axis, AxisMap, ChannelInfo, ChannelSel, Dir, PlaneKind, Role, Slice, SliceView, Stats, Volume};

use slerp_scene::textures::{ColorSpace, SourceInfo, Texture};
use slerp_scene::{Asset, Limits, LoadError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VolumeFormat {
    Npy,
    Npz,
    Nifti,
    MetaImage,
    Nrrd,
    Tiff,
    NetCdf,
    Hdf5,
}

impl VolumeFormat {
    pub fn name(self) -> &'static str {
        match self {
            VolumeFormat::Npy => "NumPy array",
            VolumeFormat::Npz => "NumPy archive",
            VolumeFormat::Nifti => "NIfTI volume",
            VolumeFormat::MetaImage => "MetaImage volume",
            VolumeFormat::Nrrd => "NRRD volume",
            VolumeFormat::Tiff => "TIFF image",
            VolumeFormat::NetCdf => "NetCDF file",
            VolumeFormat::Hdf5 => "HDF5 file",
        }
    }
}

/// How much to read.
#[derive(Clone, Debug)]
pub struct OpenOptions {
    /// Volumes of at most this many bytes are copied into memory, so every
    /// plane after the first costs nothing; larger ones are read from their
    /// file plane by plane.
    pub memory_limit: u64,
    /// How far into a compressed stream a read may reach.  Thumbnails of
    /// gzip-compressed volumes stop here instead of inflating gigabytes.
    pub reach: u64,
    /// The thumbnail's size in pixels, which lets slides pick a small level.
    pub thumb_size: Option<u32>,
    /// Read neighbouring files (a detached `.raw`, an Analyze `.img`).
    pub neighbours: bool,
}

impl OpenOptions {
    /// For thumbnails: nothing is copied, and compressed data is inflated
    /// only 192 MB deep.
    pub fn thumbnail() -> OpenOptions {
        OpenOptions { memory_limit: 0, reach: 192 << 20, thumb_size: None, neighbours: true }
    }

    /// For the viewer: volumes up to `memory_limit` bytes go into memory.
    pub fn viewer(memory_limit: u64) -> OpenOptions {
        OpenOptions { memory_limit, reach: u64::MAX, thumb_size: None, neighbours: true }
    }
}

/// A finished picture: a slide's level, a page of a file whose pages differ.
pub struct Picture {
    pub texture: Texture,
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// What a file opens as.
pub enum Content {
    /// An array to cut planes from.
    Volume(Box<Volume>),
    /// A picture to show as it is.
    Picture(Box<Picture>),
}

impl Content {
    /// The volume, for formats that only ever give one.
    pub fn volume(self) -> Option<Box<Volume>> {
        match self {
            Content::Volume(v) => Some(v),
            Content::Picture(_) => None,
        }
    }
}

/// An opened file: what it shows, and the parts it can show instead.
pub struct Opened {
    pub content: Content,
    /// Names of the file's other parts (an `.npz`'s arrays); empty for files
    /// that are one thing.
    pub parts: Vec<String>,
    /// The part shown.
    pub part: Option<String>,
    /// Files beside this one that were read.
    pub neighbours: Vec<PathBuf>,
}

fn invalid(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// Recognises a file from its first bytes and name.
pub fn sniff(path: &Path, head: &[u8]) -> Option<VolumeFormat> {
    if npy::sniff_npy(head) {
        return Some(VolumeFormat::Npy);
    }
    // A zip is only taken for an archive of arrays when it is named like one.
    let ext = path.extension().map(|e| e.to_ascii_lowercase());
    if ext.as_deref() == Some(std::ffi::OsStr::new("npz")) && zip::looks_like_zip(head) {
        return Some(VolumeFormat::Npz);
    }
    if nrrd::sniff(head) {
        return Some(VolumeFormat::Nrrd);
    }
    if metaimage::sniff(path, head) {
        return Some(VolumeFormat::MetaImage);
    }
    if nifti::sniff(path, head) {
        return Some(VolumeFormat::Nifti);
    }
    if tiff::sniff(path, head) {
        return Some(VolumeFormat::Tiff);
    }
    if netcdf::sniff(head) {
        return Some(VolumeFormat::NetCdf);
    }
    if hdf5::sniff(head) {
        return Some(VolumeFormat::Hdf5);
    }
    None
}

/// The names of a file's parts.
pub fn parts_path(f: VolumeFormat, path: &Path) -> Vec<String> {
    match f {
        VolumeFormat::Npz => {
            npy::npz_members(path).map(|m| m.iter().map(npy::member_name).collect()).unwrap_or_default()
        }
        VolumeFormat::Tiff => tiff::parts(path),
        VolumeFormat::NetCdf => netcdf::open(path, None).map(|(_, names, _)| names).unwrap_or_default(),
        VolumeFormat::Hdf5 => hdf5::list(path).map(|l| l.part_names()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Opens a file, on `part` when it has several.
pub fn open(f: VolumeFormat, path: &Path, part: Option<&str>, opts: &OpenOptions) -> Result<Opened, LoadError> {
    let mut parts = Vec::new();
    let mut shown = None;
    let mut neighbours = Vec::new();
    let mut volume = match f {
        VolumeFormat::Tiff => {
            let (content, parts, part) = tiff::open(path, part, opts)?;
            let content = match content {
                Content::Volume(v) if opts.memory_limit > 0 => {
                    Content::Volume(Box::new(v.into_memory(opts.memory_limit)?))
                }
                c => c,
            };
            return Ok(Opened { content, parts, part, neighbours: Vec::new() });
        }
        VolumeFormat::Npy => npy::open_npy(path)?,
        VolumeFormat::Nifti => {
            let (v, n) = nifti::open(path, opts.reach)?;
            neighbours = n;
            v
        }
        VolumeFormat::MetaImage => {
            let (v, n) = metaimage::open(path, opts.reach)?;
            neighbours = n;
            v
        }
        VolumeFormat::Nrrd => {
            let (v, n) = nrrd::open(path, opts.reach)?;
            neighbours = n;
            v
        }
        VolumeFormat::Hdf5 => {
            let listing = hdf5::list(path)?;
            parts = listing.part_names();
            let (v, shown_name) = listing.open(part)?;
            shown = Some(shown_name);
            v
        }
        VolumeFormat::NetCdf => {
            let (v, names, shown_name) = netcdf::open(path, part)?;
            parts = names;
            shown = Some(shown_name);
            v
        }
        VolumeFormat::Npz => {
            let members = npy::npz_members(path)?;
            parts = members.iter().map(npy::member_name).collect();
            // 1. The part asked for, else the first array that can be pictured.
            let pick = match part {
                Some(name) => members.iter().find(|m| npy::member_name(m) == name),
                None => members
                    .iter()
                    .take(64)
                    .find(|m| npy::open_member(path, m, opts.reach).is_ok_and(|v| v.no_picture.is_none()))
                    .or(members.first()),
            };
            let entry = pick.ok_or_else(|| LoadError::Unsupported("the archive holds no arrays".into()))?;
            shown = Some(npy::member_name(entry));
            npy::open_member(path, entry, opts.reach)?
        }
    };
    if opts.memory_limit > 0 {
        volume = volume.into_memory(opts.memory_limit)?;
    }
    Ok(Opened { content: Content::Volume(Box::new(volume)), parts, part: shown, neighbours })
}

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// The colour map a volume's first picture uses: scientific measurements
/// (floating-point or scaled data from NumPy, NetCDF, HDF5) in Viridis, which shows structure better than
/// grey, and everything else, medical and microscopy images included, in
/// grey.
pub fn default_colormap(v: &Volume) -> ColorMap {
    let scientific = ["NumPy", "NetCDF", "HDF5"].iter().any(|p| v.format.starts_with(p));
    // Packed integers (a NetCDF short with a scale_factor) are measurements too.
    let measured = v.dtype.is_float() || v.slope != 1.0 || v.inter != 0.0;
    if scientific && measured { ColorMap::Viridis } else { ColorMap::Gray }
}

/// The style for a plane: windowed by the 1st to 99th percentile, in the
/// layout the volume's axes give.
pub fn auto_style(v: &Volume, slice: &Slice) -> Style {
    let window = render::auto_window(v, slice);
    let layout = if !v.channels.is_empty() && slice.samples == v.channels.len() {
        let chans = (0..slice.samples)
            .map(|c| {
                let vals: Vec<f32> = slice.data.iter().skip(c).step_by(slice.samples).copied().collect();
                let color = v.channels.get(c).map_or([255; 3], |i| i.color);
                ChannelStyle { color, window: render::percentile_window(&vals, 1.0, 99.5), visible: true }
            })
            .collect();
        Layout::Channels(chans)
    } else if slice.samples >= 3 {
        Layout::Rgb
    } else {
        Layout::Single { map: default_colormap(v), invert: v.invert }
    };
    Style { window, layout }
}

/// Wraps rendered RGBA as a texture carrying the volume's facts.
pub fn texture_of(v: &Volume, w: usize, h: usize, rgba: Vec<u8>, slices: usize) -> Texture {
    let mut tex = Texture::from_rgba8(w as u32, h as u32, rgba, ColorSpace::Srgb);
    tex.source = SourceInfo {
        container: v.format.clone(),
        format: format!("{} {:?}", v.dtype.name(), v.shape()),
        width: w as u32,
        height: h as u32,
        depth: slices as u32,
        layers: 1,
        faces: 1,
        mip_count: 1,
        header: v.facts.clone(),
        ..Default::default()
    };
    tex
}

/// The picture a thumbnail shows: the middle slice of the first time point,
/// with the 1st to 99th percentile as its window.  `max_side` shrinks big
/// planes while they are read.
pub fn middle_picture(v: &Volume, max_side: Option<usize>) -> Result<Texture, LoadError> {
    if let Some(why) = &v.no_picture {
        return Err(LoadError::Unsupported(format!("{why}: nothing to draw (the viewer shows its statistics)")));
    }
    let map = v.default_map().ok_or_else(|| LoadError::Unsupported("the array has no rows and columns".into()))?;
    let kind = PlaneKind::Axial;
    let n = v.slice_count(&map, kind);
    let mut view = SliceView { kind, slice: n / 2, t: 0, neurological: false };
    // A compressed stream may not reach the middle: take the deepest slice that it does.
    let slice = loop {
        match v.extract(&map, &view, ChannelSel::All, max_side) {
            Ok(s) => break s,
            Err(e) if view.slice > 0 && e.to_string().contains("too far") => view.slice /= 2,
            Err(e) => return Err(invalid(e)),
        }
    };
    let style = auto_style(v, &slice);
    let (rgba, alpha) = render::render(&slice, &style);
    let (w, h, rgba) =
        render::square_pixels(slice.width, slice.height, rgba, slice.col_spacing, slice.row_spacing, 4096);
    let mut tex = texture_of(v, w, h, rgba, n);
    tex.has_alpha = alpha;
    Ok(tex)
}

/// Loads a file as Slerp's other loaders do: the thumbnail picture.
pub fn load_path(f: VolumeFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    picture_of(f, path, limits, None)
}

fn picture_of(f: VolumeFormat, path: &Path, limits: &Limits, part: Option<&str>) -> Result<Asset, LoadError> {
    let opts = OpenOptions { thumb_size: limits.max_texture_size, ..OpenOptions::thumbnail() };
    let opened = open(f, path, part, &opts)?;
    let max_side = limits.max_texture_size.map(|n| n as usize * 2);
    Ok(Asset::Image(Arc::new(match opened.content {
        Content::Volume(v) => middle_picture(&v, max_side)?,
        Content::Picture(p) => p.texture,
    })))
}

/// Loads one part of a file as a picture.
pub fn load_part_path(f: VolumeFormat, path: &Path, limits: &Limits, name: &str) -> Result<Asset, LoadError> {
    picture_of(f, path, limits, Some(name))
}

//endregion
