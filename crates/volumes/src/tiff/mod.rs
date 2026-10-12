//! TIFF files as volumes and pictures: multi-page stacks (every page a
//! slice), OME-TIFF and ImageJ hyperstacks ([`stack`]), and whole-slide and
//! other pyramidal files whose levels, label and macro picture are parts
//! ([`slide`]).  The file format itself (directories, compression, assembling
//! a page) is the `slerp-tiff` crate, which anything may use.

pub mod ome;
pub mod slide;
pub mod stack;

use slerp_tiff::raster::Options;
pub use slerp_tiff::{MAX_PAGES, Page, Tiff, tag};
use std::path::Path;
use std::sync::Arc;

use crate::volume::Volume;
use crate::{Content, OpenOptions, Picture};
use slerp_scene::LoadError;

/// How many pages are read in full to classify a file.
const CLASSIFY_PAGES: usize = 64;

/// Raw camera formats built on TIFF, which Slerp doesn't develop.
const RAW_EXTENSIONS: [&str; 18] = [
    "dng", "cr2", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "pef", "srw", "3fr", "iiq", "erf", "mef", "mos",
    "mrw", "kdc",
];

/// Recognises a TIFF by its header, leaving camera raw files to their own
/// tools.
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    let ok = matches!(head.get(..4), Some(b"II*\0" | b"MM\0*" | b"II+\0" | b"MM\0+"));
    if !ok {
        return false;
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    !RAW_EXTENSIONS.contains(&ext.as_str())
}

/// Whether the TIFF at `path` is one this crate must open rather than a plain
/// picture viewer: a whole-slide pyramid (its levels, label and macro picture
/// are parts, and its thumbnail comes from the smallest level) or an OME or
/// ImageJ file (axes beyond the page).  A slide's label and macro pages are
/// flagged as reduced copies, so a viewer that looks only at that takes the
/// file for a photograph with thumbnails.
pub fn is_slide_or_hyperstack(path: &Path) -> bool {
    match read(path) {
        Ok((r, layout)) => {
            matches!(layout, Layout::Slide(_))
                || ome::looks_like_ome(&r.pages[0].description)
                || stack::looks_like_imagej(&r.pages[0].description)
        }
        Err(_) => false,
    }
}

/// A TIFF file read far enough to know what it is.
struct Read {
    tiff: Arc<Tiff>,
    offsets: Vec<u64>,
    pages: Vec<Page>,
}

enum Layout {
    Slide(slide::Slide),
    /// Pages of one size: a stack, an OME or ImageJ hyperstack.
    Stack,
    /// Pages of different sizes, one picture each.
    Many,
}

fn bad(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

fn read(path: &Path) -> Result<(Read, Layout), LoadError> {
    let tiff = Arc::new(Tiff::open(path).map_err(bad)?);
    let offsets = tiff.ifd_offsets(MAX_PAGES).map_err(bad)?;
    let mut pages = Vec::new();
    for &o in offsets.iter().take(CLASSIFY_PAGES) {
        match tiff.page(o) {
            Ok(p) => pages.push(p),
            Err(e) if pages.is_empty() => return Err(bad(e)),
            Err(_) => break,
        }
    }
    let first = &pages[0];
    if first.ifd.get(tag::NDPI_FORMAT_FLAG).is_some() && tiff.len > u32::MAX as u64 {
        return Err(LoadError::Unsupported(
            "Hamamatsu NDPI files over 4 GB (their directories use 64-bit offsets)".into(),
        ));
    }
    let layout = if ome::looks_like_ome(&first.description) || stack::looks_like_imagej(&first.description) {
        Layout::Stack
    } else if offsets.len() <= CLASSIFY_PAGES {
        match slide::classify(&tiff, &pages).map_err(bad)? {
            Some(s) => Layout::Slide(s),
            None => {
                let (w, h) = (first.width, first.height);
                if pages.iter().all(|p| (p.width, p.height) == (w, h)) { Layout::Stack } else { Layout::Many }
            }
        }
    } else {
        Layout::Stack
    };
    Ok((Read { tiff, offsets, pages }, layout))
}

/// The names of a TIFF's parts: a slide's levels, label and macro picture, or
/// the pages of a file whose pages differ in size.
pub fn parts(path: &Path) -> Vec<String> {
    match read(path) {
        Ok((_, Layout::Slide(s))) => s.parts().into_iter().map(|p| p.name).collect(),
        Ok((r, Layout::Many)) => {
            r.pages.iter().enumerate().map(|(i, p)| format!("Page {} ({} x {})", i + 1, p.width, p.height)).collect()
        }
        _ => Vec::new(),
    }
}

/// Opens a TIFF: a volume for stacks, a picture for slides and for files with
/// pages of several sizes.
pub fn open(
    path: &Path,
    part: Option<&str>,
    opts: &OpenOptions,
) -> Result<(Content, Vec<String>, Option<String>), LoadError> {
    let (r, layout) = read(path)?;
    let name = stack::file_name(path);
    let first = r.pages[0].clone();
    match layout {
        Layout::Stack => {
            let plan = if ome::looks_like_ome(&first.description) {
                stack::plan_ome(&r.tiff, &r.offsets, &first, &name).map_err(bad)?
            } else if stack::looks_like_imagej(&first.description) {
                stack::plan_imagej(&r.tiff, &r.offsets, &first).map_err(bad)?
            } else {
                stack::plan_pages(&r.offsets)
            };
            let v = stack::volume(&r.tiff, &first, plan, &name).map_err(bad)?;
            Ok((Content::Volume(Box::new(v)), Vec::new(), None))
        }
        Layout::Many => {
            let names: Vec<String> = r
                .pages
                .iter()
                .enumerate()
                .map(|(i, p)| format!("Page {} ({} x {})", i + 1, p.width, p.height))
                .collect();
            let idx = part.and_then(|n| names.iter().position(|x| x == n)).unwrap_or(0);
            let plan = stack::plan_pages(&r.offsets[idx..idx + 1]);
            let v = stack::volume(&r.tiff, &r.pages[idx], plan, &name).map_err(bad)?;
            Ok((Content::Volume(Box::new(v)), names.clone(), Some(names[idx].clone())))
        }
        Layout::Slide(s) => {
            let parts = s.parts();
            let names: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
            let (image, level, shown) = match (part, opts.thumb_size) {
                (Some(n), _) => {
                    let p = parts
                        .iter()
                        .find(|p| p.name == n)
                        .ok_or_else(|| LoadError::Unsupported(format!("no part called {n}")))?;
                    (p.image, p.level, p.name.clone())
                }
                (None, Some(size)) => {
                    let (i, l) = s.thumbnail_level(size as u64).ok_or_else(|| bad("the slide has no levels"))?;
                    let n =
                        parts.iter().find(|p| p.image == i && p.level == l).map(|p| p.name.clone()).unwrap_or_default();
                    (i, l, n)
                }
                (None, None) => {
                    let p = parts.first().ok_or_else(|| bad("the slide has no parts"))?;
                    (p.image, p.level, p.name.clone())
                }
            };
            let lv = &s.images[image].levels[level];
            let o = match opts.thumb_size {
                Some(size) => Options { min_long_side: size as usize * 2, max_pixels: slide::MAX_THUMB_PIXELS },
                None => Options { min_long_side: 0, max_pixels: slide::MAX_VIEW_PIXELS },
            };
            if (lv.width.saturating_mul(lv.height) > o.max_pixels || lv.width.max(lv.height) > slide::MAX_VIEW_SIDE)
                && opts.thumb_size.is_none()
            {
                return Err(LoadError::TooLarge(format!(
                    "{} is {} x {} pixels; pick a smaller level",
                    shown, lv.width, lv.height
                )));
            }
            // Pages beyond the classified ones are never levels.
            let tex = slide::level_picture(&r.tiff, &r.pages, &s, image, level, &o).map_err(bad)?;
            let facts = tex.source.header.clone();
            let warnings = Vec::new();
            Ok((Content::Picture(Box::new(Picture { texture: tex, facts, warnings })), names, Some(shown)))
        }
    }
}

/// The volume of a TIFF stack, for callers that need only that.
pub fn open_volume(path: &Path, opts: &OpenOptions) -> Result<Volume, LoadError> {
    match open(path, None, opts)?.0 {
        Content::Volume(v) => Ok(*v),
        Content::Picture(_) => Err(LoadError::Unsupported("this TIFF is a slide, not a stack".into())),
    }
}
