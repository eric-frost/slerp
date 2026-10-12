//! The rule for thumbnails: when a file carries a picture its authoring
//! program saved of it (a thumbnail, a preview, a merged image), file
//! managers show that picture, and only files without one get Slerp's own
//! render.  The viewer still shows the real content, noting in its
//! inspector that the thumbnail is the saved picture.
//!
//! The rule lives here, in [`saved_picture`]: one arm per format that can
//! carry such a picture, each calling a cheap reader in the format's own
//! crate that reads only what the picture needs (a header, a zip member, a
//! chunk near the start or end of the file), never the model or drawing.
//! [`crate::prepare_thumbnail`] asks it before loading anything, and
//! `slerp render` without a viewer state asks it too, so the two agree.
//!
//! A poor picture doesn't count, since Slerp's render shows more
//! ([`SavedPicture::stands_for_file`]):
//!
//! - one too small for the thumbnail, which would show it blurred: under
//!   half the thumbnail's size on its longer side, and never under 96
//!   pixels.  ImageMagick writes 1 x 1 PDF page thumbnails, slicers 16 px
//!   icons next to their real pictures, ezdxf 64 x 33 smudges.  128 pixels
//!   is always enough, so Blender's 128 px previews stand at every size.
//! - one flat colour, as some FBX and 3ds Max exporters write.
//! - a view of a scene or drawing whose subject is a speck in an empty
//!   frame, as Rhino's and SketchUp's captures of a small object often
//!   are; Slerp's render frames the subject to fill the thumbnail.
//!
//! Such a file is rendered instead, and the poor picture is shown only when
//! its content can't be.
//!
//! Formats whose files are only their saved picture (`Kind::Preview`: 3ds
//! Max, Cinema 4D, DWG and the rest) load that picture as their content and
//! don't need this step.  A material's preview picture beside or inside the
//! file stays with the material loader (`MaterialAsset::preview`), since
//! the Qt image plugin draws the material on a ball even when it has one.

use crate::{Kind, Prepared, on_white, with_alpha};
use slerp_scene::{Limits, LoadError};
use slerp_textures::Texture;
use std::path::Path;
use std::sync::Arc;

/// The least a saved picture's longer side may be, in pixels, for any
/// thumbnail size; larger thumbnails want half their size.
const MIN_SAVED_SIDE: u32 = 96;
/// A saved picture's longer side that is enough for any thumbnail size,
/// in pixels: Blender's previews are this size, and give way to a render
/// on no icon.
const ENOUGH_SAVED_SIDE: u32 = 128;

/// The least share of a saved view its subject must fill: the box around
/// the middle of everything that isn't background (see [`subject_share`])
/// must cover this much of the picture.  Measured on the sample files,
/// views under it are small objects in empty viewports.
const MIN_SUBJECT_SHARE: f32 = 0.1;

/// The percentage of the subject's pixels left outside its box on each
/// side, so a sliver of another object at the frame's edge doesn't stretch
/// the box over empty space.
const SUBJECT_TRIM: usize = 5;

/// How far, in 8-bit steps on any channel, a pixel may differ from the
/// background and still be background: enough for JPEG noise and gentle
/// gradients, not for a dim object on a dark viewport.
const BACKGROUND_TOLERANCE: u8 = 12;

/// The most of a Litematica schematic read to find its picture; the rest
/// of such a file is its blocks.
const MAX_SCHEMATIC_BYTES: u64 = 256 << 20;

/// A picture a file carries of itself.
pub struct SavedPicture {
    pub texture: Arc<Texture>,
    /// Drawn on a white page: a document's picture and an EPS's metafile
    /// preview assume one, as Windows shows them.
    pub on_paper: bool,
    /// A view of a scene or drawing, which Slerp's render frames to fill
    /// the thumbnail; not a copy of the content itself (a painting's
    /// composite, an image's or page's small copy), whose empty space is
    /// part of the picture.
    pub view: bool,
}

impl SavedPicture {
    /// The picture's own size, before any reduction for the thumbnail.
    pub fn size(&self) -> (u32, u32) {
        let t = &*self.texture;
        (t.source.width.max(t.width()), t.source.height.max(t.height()))
    }

    /// Whether it is good enough to stand for the file in a thumbnail of
    /// `size` pixels: large enough not to blur, not one flat colour, and
    /// for a view, not a speck of a subject in an empty frame.
    pub fn stands_for_file(&self, size: u32) -> bool {
        let (w, h) = self.size();
        let t = &*self.texture;
        w.max(h) >= (size / 2).clamp(MIN_SAVED_SIDE, ENOUGH_SAVED_SIDE)
            && !is_flat(t)
            && (!self.view || subject_share(t) >= MIN_SUBJECT_SHARE)
    }

    /// The file's thumbnail, `size` pixels at most: transparency kept as
    /// the program shows it, or drawn on paper.
    pub fn thumbnail(&self, size: u32) -> Prepared {
        let tex = &*self.texture;
        if self.on_paper {
            return Prepared::Done(slerp_textures::thumbnail(&on_white(tex), size));
        }
        Prepared::Done(with_alpha(tex, slerp_textures::thumbnail(tex, size)))
    }
}

/// Whether a picture is one flat colour, as some exporters write in place
/// of a real thumbnail (an all-white FBX thumbnail, an empty slicer plate).
fn is_flat(t: &Texture) -> bool {
    let Some(slerp_textures::Pixels::Rgba8(px)) = t.levels.first().and_then(|l| l.surfaces.first()) else {
        return false;
    };
    let Some(first) = px.get(..4) else { return true };
    px.chunks_exact(4).all(|p| p.iter().zip(first).all(|(a, b)| a.abs_diff(*b) <= 3))
}

/// The share of a picture its subject fills: the area of the box around the
/// pixels that aren't background, less the outermost [`SUBJECT_TRIM`]
/// percent on each side, over the picture's area.  The background is the
/// corner colour most of the picture's edge has; a picture whose edge has
/// no one colour (a photograph, a subject cut by the frame) is all
/// subject.
fn subject_share(t: &Texture) -> f32 {
    let Some(slerp_textures::Pixels::Rgba8(px)) = t.levels.first().and_then(|l| l.surfaces.first()) else {
        return 1.0;
    };
    let (w, h) = (t.width() as usize, t.height() as usize);
    if w < 3 || h < 3 || px.len() < w * h * 4 {
        return 1.0;
    }
    let at = |x: usize, y: usize| &px[(y * w + x) * 4..][..4];
    let same = |a: &[u8], b: &[u8]| {
        a[3] < 8 && b[3] < 8 || a.iter().zip(b).all(|(a, b)| a.abs_diff(*b) <= BACKGROUND_TOLERANCE)
    };
    // 1. The edge, and the corner colour most of it matches.
    let edge: Vec<&[u8]> = (0..w)
        .flat_map(|x| [at(x, 0), at(x, h - 1)])
        .chain((1..h - 1).flat_map(|y| [at(0, y), at(w - 1, y)]))
        .collect();
    let corners = [at(0, 0), at(w - 1, 0), at(0, h - 1), at(w - 1, h - 1)];
    let (matches, background) = corners
        .into_iter()
        .map(|c| (edge.iter().filter(|p| same(p, c)).count(), c))
        .max_by_key(|(n, _)| *n)
        .unwrap_or((0, corners[0]));
    // 2. Most of the edge must be background for the picture to have one.
    if matches * 10 < edge.len() * 6 {
        return 1.0;
    }
    // 3. How many subject pixels each column and row holds.
    let (mut columns, mut rows) = (vec![0; w], vec![0; h]);
    for (i, p) in px[..w * h * 4].chunks_exact(4).enumerate() {
        if !same(p, background) {
            columns[i % w] += 1;
            rows[i / w] += 1;
        }
    }
    let total: usize = rows.iter().sum();
    if total == 0 {
        return 0.0;
    }
    // 4. The box around their middle.
    (middle_span(&columns, total) * middle_span(&rows, total)) as f32 / (w * h) as f32
}

/// How many columns (or rows) hold the middle of the subject: from where
/// the first [`SUBJECT_TRIM`] percent of its `total` pixels end to where
/// the last begin.
fn middle_span(counts: &[usize], total: usize) -> usize {
    let trim = total * SUBJECT_TRIM / 100;
    // The first index, walking `indices`, by which more than `trim` pixels
    // have been passed.
    let past_trim = |indices: &mut dyn Iterator<Item = usize>| {
        let mut seen = 0;
        for i in indices {
            seen += counts[i];
            if seen > trim {
                return Some(i);
            }
        }
        None
    };
    let first = past_trim(&mut (0..counts.len())).unwrap_or(0);
    let last = past_trim(&mut (0..counts.len()).rev()).unwrap_or(counts.len() - 1);
    last.saturating_sub(first) + 1
}

/// The picture the file at `path` carries of itself, when it has one good
/// enough to stand for it in a thumbnail of `size` pixels (see
/// [`SavedPicture::stands_for_file`]).  Reads only what the picture needs,
/// decoded no larger than the thumbnail where the format allows.  Damage
/// and read errors count as no picture: the content loader reports them,
/// if it fails too.
pub fn saved_picture(kind: Kind, path: &Path, size: u32) -> Option<SavedPicture> {
    carried_picture(kind, path, &crate::thumbnail_limits(size)).filter(|s| s.stands_for_file(size))
}

/// Whether files of `kind` can carry a saved picture that thumbnails show
/// in place of content the viewer draws, so the viewer notes it.  Layered
/// paintings aren't among them, since the viewer shows the same picture.
pub fn shows_saved_picture(kind: Kind) -> bool {
    !matches!(kind, Kind::Paint(_)) && can_carry(kind)
}

/// Whether files of `kind` can carry a saved picture at all.
fn can_carry(kind: Kind) -> bool {
    use slerp_models::ModelFormat;
    matches!(
        kind,
        Kind::Blend
            | Kind::SketchUp
            | Kind::Max
            | Kind::Gltf
            | Kind::Model(ModelFormat::ThreeMf | ModelFormat::Fbx)
            | Kind::Cad(_)
            | Kind::Path(_)
            | Kind::Document(_)
            | Kind::Paint(_)
            | Kind::Image(slerp_images::ImageFormat::Exr)
            | Kind::Minecraft(slerp_minecraft::MinecraftFormat::Litematica)
    )
}

/// [`saved_picture`] without the quality check: whatever picture the file
/// carries, for the fallback when its content can't be shown either.
pub fn carried_picture(kind: Kind, path: &Path, limits: &Limits) -> Option<SavedPicture> {
    if !can_carry(kind) {
        return None;
    }
    let image = |t: Result<Option<Texture>, LoadError>| t.ok().flatten();
    let texture = match kind {
        // Blender's preview of the scene, in most files it writes.
        Kind::Blend => image(slerp_blend::preview_path(path)),
        // SketchUp's picture of the model as it was saved.
        Kind::SketchUp => {
            match slerp_previews::read_stored_path(slerp_previews::PreviewFormat::SketchUp, path, limits) {
                Ok(slerp_previews::Stored::Image(t)) => Some(t),
                _ => None,
            }
        }
        // 3ds Max's picture of the scene, in its summary information.
        Kind::Max => match slerp_previews::read_stored_path(slerp_previews::PreviewFormat::Max, path, limits) {
            Ok(slerp_previews::Stored::Image(t)) => Some(t),
            _ => None,
        },
        // A 3MF package's thumbnail or a slicer's plate picture; an FBX
        // file's header thumbnail.
        Kind::Model(f) => image(slerp_models::saved_picture(f, path, limits)),
        // A VRM avatar's portrait.
        Kind::Gltf => image(slerp_gltf::saved_picture(path, limits)),
        // AutoCAD's DXF thumbnail; Rhino's preview.
        Kind::Cad(f) => image(slerp_cad::saved_picture(f, path, limits)),
        // A slicer's picture in G-code.
        Kind::Path(f) => image(slerp_paths::saved_picture(f, path)),
        // PDF and Illustrator thumbnails (XMP, the first page's /Thumb);
        // EPS previews.
        Kind::Document(f) => {
            return document(slerp_documents::saved_picture(f, path).ok().flatten()?, limits);
        }
        // Photoshop's composite, OpenRaster's and Krita's merged picture.
        Kind::Paint(f) => image(slerp_paint::saved_picture(f, path, limits)),
        // An OpenEXR header preview.
        Kind::Image(f) => image(slerp_images::saved_picture(f, path)),
        // Litematica's picture taken in the game.
        Kind::Minecraft(f) => {
            let size = std::fs::metadata(path).ok()?.len();
            if size > MAX_SCHEMATIC_BYTES.min(limits.max_file_bytes) {
                return None;
            }
            slerp_minecraft::stored_preview(f, &std::fs::read(path).ok()?)
        }
        _ => None,
    }?;
    // Paintings' composites and images' previews are the content itself.
    let view = !matches!(kind, Kind::Paint(_) | Kind::Image(_));
    Some(SavedPicture { texture: Arc::new(texture), on_paper: false, view })
}

/// A document's saved picture: an image, or an EPS's metafile drawn here,
/// since loaders don't depend on each other.
fn document(picture: slerp_documents::Picture, limits: &Limits) -> Option<SavedPicture> {
    let texture = match picture {
        slerp_documents::Picture::Image(t) => t,
        slerp_documents::Picture::Metafile { bytes, facts } => {
            let mut t = slerp_metafile::load(slerp_metafile::MetafileFormat::Wmf, &bytes, limits).ok()?;
            t.source.container = "EPS with a Windows Metafile preview".into();
            t.source.header.splice(0..0, facts);
            t
        }
    };
    Some(SavedPicture { texture: Arc::new(texture), on_paper: true, view: false })
}

/// The size of the saved picture thumbnails show for the file at `path`,
/// for the viewer's inspector; `None` when they show Slerp's own drawing.
/// Answers for thumbnails of 256 pixels and up, which treat a picture
/// alike.
pub fn saved_picture_size(kind: Kind, path: &Path) -> Option<(u32, u32)> {
    if !shows_saved_picture(kind) {
        return None;
    }
    Some(saved_picture(kind, path, 2 * ENOUGH_SAVED_SIDE)?.size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_textures::ColorSpace;

    /// A `w` x `h` picture of `back` with a `box_w` x `box_h` box of
    /// `front` at (10, 10).
    fn picture(w: u32, h: u32, back: [u8; 4], front: [u8; 4], box_w: u32, box_h: u32) -> SavedPicture {
        let px = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| if (10..10 + box_w).contains(&x) && (10..10 + box_h).contains(&y) { front } else { back })
            .collect();
        SavedPicture { texture: Arc::new(Texture::from_rgba8(w, h, px, ColorSpace::Srgb)), on_paper: false, view: true }
    }

    #[test]
    fn a_picture_stands_for_its_file_when_large_and_full_enough() {
        let dark = [40, 44, 48, 255];
        let object = [140, 60, 200, 255];
        // 1. A filled frame stands at every size.
        let full = picture(200, 120, dark, object, 170, 100);
        assert!(full.stands_for_file(128) && full.stands_for_file(256) && full.stands_for_file(1024));
        // 2. A view of a speck in an empty frame doesn't, nor does one on a
        //    transparent background, whatever colour that background has;
        //    a painting of one does.
        assert!(!picture(200, 120, dark, object, 20, 15).stands_for_file(128));
        assert!(!picture(200, 120, [9, 200, 9, 0], object, 20, 15).stands_for_file(128));
        assert!(SavedPicture { view: false, ..picture(200, 120, dark, object, 20, 15) }.stands_for_file(128));
        // 2a. A sliver of another object at the edge doesn't make a speck
        //     fill the frame.
        let speck = picture(200, 120, dark, object, 30, 30);
        let mut px = match speck.texture.base() {
            slerp_textures::Pixels::Rgba8(px) => px.clone(),
            _ => unreachable!(),
        };
        for (x, y) in (196..199).flat_map(|x| (100..110).map(move |y| (x, y))) {
            px[(y * 200 + x) * 4..][..4].copy_from_slice(&object);
        }
        let texture = Arc::new(Texture::from_rgba8(200, 120, px, ColorSpace::Srgb));
        assert!(!SavedPicture { texture, ..speck }.stands_for_file(128));
        // 3. A wide 128 px picture stands at every size; a 100 px one only
        //    for small thumbnails, and a 64 px one for none.
        let blender = picture(128, 67, dark, object, 100, 50);
        assert!(blender.stands_for_file(128) && blender.stands_for_file(256) && blender.stands_for_file(1024));
        let small = picture(100, 100, dark, object, 80, 80);
        assert!(small.stands_for_file(128) && !small.stands_for_file(256));
        assert!(!picture(64, 64, dark, object, 50, 50).stands_for_file(64));
        // 4. One flat colour isn't a picture of anything.
        assert!(!picture(200, 120, dark, dark, 0, 0).stands_for_file(128));
    }

    #[test]
    fn a_picture_without_a_plain_edge_is_all_subject() {
        let px = (0..64u32 * 64).flat_map(|i| [(i % 64 * 4) as u8, (i / 64 * 4) as u8, 90, 255]).collect();
        let t = Texture::from_rgba8(64, 64, px, ColorSpace::Srgb);
        assert_eq!(subject_share(&t), 1.0);
    }
}
