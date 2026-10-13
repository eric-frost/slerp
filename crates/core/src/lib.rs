//! Ties Slerp together: recognises a file's format from its content,
//! hands it to the right loader, and turns the result into a thumbnail,
//! drawing 3D content with the GPU renderer.
//!
//! Thumbnails follow one rule (see [`saved_picture`]): a file that carries
//! a picture its authoring program saved of it (Blender's and SketchUp's
//! previews, a 3MF or FBX thumbnail, Rhino's and AutoCAD's previews, a VRM
//! portrait, a slicer's picture, a PDF's or EPS's stored thumbnail, a
//! Photoshop composite) gets that picture, read without the rest of the
//! file; only files without one are loaded and drawn.  Poor pictures (too
//! small for the thumbnail, one flat colour, or a speck of a subject in an
//! empty frame) don't count, except when the content can't be shown.  The
//! viewer always shows the content.

pub mod choices;
pub mod config;
pub mod contents;
pub mod pages;
mod picture;
mod render;
mod saved;
mod sniff;

pub use config::Config;
pub use picture::*;
pub use render::*;
pub use saved::*;
pub use slerp_archive as archive;
pub use slerp_cad as cad;
pub use slerp_dicom as dicom;
pub use slerp_documents as documents;
pub use slerp_formats as formats;
pub use slerp_gltf as gltf;
pub use slerp_images as images;
pub use slerp_paths as paths;
pub use slerp_previews::palette;
pub use slerp_render as gpu;
pub use slerp_scene as scene;
pub use slerp_textures as textures;
pub use slerp_volumes as volumes;
pub use sniff::*;

use rayon::prelude::*;
use slerp_scene::{Asset, Image8, Limits, LoadError, NamedImage};
use slerp_textures::DecodeOptions;
use std::path::Path;
use std::sync::Arc;

/// Every decoder that runs isolated, as the crates that own them list them
/// (see `slerp-isolate`): the program that serves them gives this to
/// `slerp_isolate::serve_jobs`, so it can run any of them in a new copy of
/// itself, which Windows needs for lack of fork.
pub static ISOLATED_JOBS: &[slerp_isolate::Jobs] =
    &[slerp_svg::JOBS, slerp_documents::JOBS, slerp_images::JOBS, slerp_jxr::JOBS, slerp_dna::JOBS];

#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    #[error("not a format Slerp reads")]
    Unknown,
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("no GPU available: {0}")]
    NoGpu(String),
    #[error("nothing to draw")]
    Empty,
}

/// Loads any supported file into the shared asset model.
pub fn load(path: &Path, limits: &Limits) -> Result<Asset, ThumbError> {
    let kind = sniff_file(path).map_err(LoadError::Io)?.ok_or(ThumbError::Unknown)?;
    Ok(load_kind(kind, path, limits)?)
}

/// Loads a file whose kind is already known.
pub fn load_kind(kind: Kind, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    if let Kind::Dicom = kind {
        // DICOM files run to gigabytes (cine loops, whole-slide images), but
        // the reader seeks to the header and the one frame it shows, so the
        // size limit doesn't apply.  A DICOMDIR shows its first image.
        return slerp_dicom::load_path(path, limits);
    }
    if let Kind::Volume(f) = kind {
        // Volumes run to gigabytes (and slides to tens of them), but the
        // readers seek to the slice or level they show.
        return slerp_volumes::load_path(f, path, limits);
    }
    if let Kind::Image(f) = kind {
        // Animations and icons are read by their own crate, which keeps
        // within its memory budget however many frames the file claims.
        return slerp_images::load(f, path, limits);
    }
    if let Kind::Paint(f) = kind {
        // Layered documents keep their own caps; thumbnails take the picture
        // the application saved in the file whenever there is one.
        return slerp_paint::load(f, path, limits);
    }
    if let Kind::Svg = kind {
        // Drawn in a child process with time and memory limits; the viewer
        // draws again at each zoom through `pages`.
        return Ok(Asset::Image(Arc::new(slerp_svg::picture(path, limits)?)));
    }
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    if let Kind::Texture(format) = kind {
        // Textures go through decode_file, which streams large HDR files.
        let tex = slerp_textures::decode_file(path, &texture_options(limits))?;
        // Photographs are shown upright and in sRGB; the decoder keeps the
        // pixels as stored, which is what a game wants of a texture.
        let tex = slerp_images::photo::refine(path, format, tex);
        return Ok(Asset::Image(Arc::new(tex)));
    }
    if let Kind::Dna = kind {
        // openriglogic reads DNA files itself and skips the sections (the
        // other levels of detail, the blend shapes) a thumbnail doesn't use.
        return Ok(Asset::Scene(slerp_dna::load(path, limits)?));
    }
    if let Kind::Splat(f) = kind {
        // Captures run to gigabytes, so the splat loader streams them.
        return Ok(Asset::Scene(slerp_splats::load(f, path, limits)?));
    }
    if let Kind::SketchUp = kind {
        return sketchup_asset(path, limits);
    }
    if let Kind::Max = kind {
        return max_asset(path, limits);
    }
    if let Kind::Preview(f) = kind {
        // Scenes run to gigabytes; their previews sit near the start.  A
        // Visio thumbnail is a metafile, which the metafile crate draws.
        return match slerp_previews::read_stored_path(f, path, limits)? {
            slerp_previews::Stored::Image(tex) => Ok(Asset::Image(Arc::new(tex))),
            slerp_previews::Stored::Metafile(bytes) => {
                Ok(Asset::Image(Arc::new(slerp_metafile::load_sniffed(&bytes, limits)?)))
            }
        };
    }
    if let Kind::Metafile(f) = kind {
        return slerp_metafile::load_path(f, path, limits);
    }
    if let Kind::Document(f) = kind {
        // PDF pages are drawn in a child process; an EPS shows its stored
        // preview or Ghostscript's drawing.
        return document_asset(slerp_documents::load_path(f, path, limits)?, limits);
    }
    if let Kind::Palette(f) = kind {
        // As a picture of its swatches; the viewer reads the palette itself
        // through `load_palette`.
        let p = palette::read_path(f, path, limits)?;
        return Ok(Asset::Image(Arc::new(palette::picture(&p, limits.max_texture_size.unwrap_or(512)))));
    }
    if let Kind::Archive(_) = kind {
        // PAK, PCK, VPK and GRP files run to gigabytes: a contact sheet of
        // the pictures inside, read from the file's own directory.
        return contents::gallery(path, limits).map(Asset::Gallery);
    }
    if let Kind::Quake(f) = kind
        && slerp_quake::reads_path(f)
    {
        // PK3 archives run to gigabytes; the loader reads what it needs.
        return slerp_quake::load_path(f, path, limits);
    }
    if let Kind::Path(f) = kind {
        // G-code runs to hundreds of megabytes and streams through.
        return Ok(Asset::Scene(slerp_paths::load(f, path, limits)?));
    }
    if let Kind::Cad(f) = kind {
        // DXF drawings run to tens of megabytes; Rhino files are read by
        // chunks, with the preview picture near the start.
        return slerp_cad::load_path(f, path, limits);
    }
    let bytes = std::fs::read(path)?;
    let mut asset = match kind {
        Kind::Texture(_)
        | Kind::Dna
        | Kind::Splat(_)
        | Kind::Preview(_)
        | Kind::Palette(_)
        | Kind::Dicom
        | Kind::Cad(_)
        | Kind::Metafile(_)
        | Kind::Volume(_)
        | Kind::Archive(_)
        | Kind::Image(_)
        | Kind::Document(_)
        | Kind::Paint(_)
        | Kind::Svg
        | Kind::SketchUp
        | Kind::Max
        | Kind::Path(_) => {
            unreachable!()
        }
        Kind::Gltf => slerp_gltf::load(path, &bytes, limits),
        Kind::Model(f) => slerp_models::load(f, path, &bytes, limits),
        Kind::Quake(f) => slerp_quake::load(f, path, &bytes, limits),
        Kind::Material(f) => slerp_materials::load(f, path, &bytes, limits),
        Kind::Sprite(f) => slerp_sprites::load(f, path, &bytes, limits),
        Kind::Lut(f) => slerp_lut::load(f, path, &bytes, limits),
        Kind::Engine(f) => slerp_engines::load(f, path, &bytes, limits),
        Kind::Blend => slerp_blend::load(path, &bytes, limits).map(Asset::Scene),
        Kind::Retro(f) => slerp_retro::load(f, path, &bytes, limits),
        Kind::GameImage(f) => slerp_gameimages::load(f, path, &bytes, limits),
        Kind::Minecraft(f) => {
            slerp_minecraft::load(f, path, &bytes, limits, &Config::load().minecraft_textures()).map(Asset::Scene)
        }
        Kind::Draco => slerp_gltf::load_drc(path, &bytes, limits),
        Kind::Rigged(f) => slerp_rigged::load(f, path, &bytes, limits),
        Kind::Roblox(f) => slerp_roblox::load(f, path, &bytes, limits).map(Asset::Scene),
        Kind::Maya(_) => slerp_maya::load(&bytes, Some(path), limits).map(Asset::Scene),
    }?;
    if let Asset::Scene(scene) = &mut asset {
        show_if_all_hidden(scene);
        hide_helper_meshes(scene);
        color_groom_cards(scene);
    }
    Ok(asset)
}

/// A SketchUp model as a scene, or, when the model can't be read (damage,
/// or a version the reader doesn't cope with), the picture SketchUp saved
/// in the file, saying so.
fn sketchup_asset(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    match slerp_sketchup::load_path(path, limits) {
        Ok(scene) => Ok(Asset::Scene(scene)),
        Err(e) => {
            let stored = slerp_previews::read_stored_path(slerp_previews::PreviewFormat::SketchUp, path, limits);
            let Ok(slerp_previews::Stored::Image(mut tex)) = stored else { return Err(e) };
            tex.source.container = format!("SketchUp's saved picture; the model couldn't be read ({e})");
            Ok(Asset::Image(Arc::new(tex)))
        }
    }
}

/// A 3ds Max scene, or, when nothing in it can be built (only lights,
/// cameras or object types Slerp can't rebuild, or damage), the picture Max
/// saved in the file, saying why.
fn max_asset(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    match slerp_max::load_path(path, limits) {
        Ok(scene) => Ok(Asset::Scene(scene)),
        Err(e) => {
            let stored = slerp_previews::read_stored_path(slerp_previews::PreviewFormat::Max, path, limits);
            let Ok(slerp_previews::Stored::Image(mut tex)) = stored else { return Err(e) };
            tex.source.container = format!("3ds Max's saved picture; the scene couldn't be built ({e})");
            Ok(Asset::Image(Arc::new(tex)))
        }
    }
}

/// The view a SketchUp model was saved with, as an orbit camera for its
/// scene whose visible parts fill `bounds`; `None` for other scenes.
pub fn saved_view(scene: &scene::Scene, bounds: &scene::Aabb) -> Option<scene::OrbitCamera> {
    slerp_sketchup::saved_view(scene, bounds)
}

/// A document's picture as an asset.  An EPS's Windows Metafile preview is
/// drawn here, since loaders don't depend on each other.
fn document_asset(picture: slerp_documents::Picture, limits: &Limits) -> Result<Asset, LoadError> {
    let texture = match picture {
        slerp_documents::Picture::Image(t) => t,
        slerp_documents::Picture::Metafile { bytes, facts } => {
            let mut t = slerp_metafile::load(slerp_metafile::MetafileFormat::Wmf, &bytes, limits)?;
            t.source.container = "EPS with a Windows Metafile preview".into();
            t.source.header.splice(0..0, facts);
            t
        }
    };
    Ok(Asset::Image(Arc::new(texture)))
}

/// How much of a texture to decode under these limits: a thumbnail-sized
/// preview, or everything.
fn texture_options(limits: &Limits) -> DecodeOptions {
    match limits.max_texture_size {
        Some(n) => DecodeOptions::preview(n),
        None => DecodeOptions::full(),
    }
}

/// Reads a colour palette with its names and original colour values, for
/// the viewer's palette list.
pub fn load_palette(path: &Path) -> Result<palette::Palette, ThumbError> {
    match sniff_file(path).map_err(LoadError::Io)? {
        Some(Kind::Palette(f)) => Ok(palette::read_path(f, path, &Limits::viewer())?),
        _ => Err(ThumbError::Unknown),
    }
}

/// The names of a file's parts, in file order, when it holds several things
/// that are shown one at a time: a Doom WAD's maps (MAP01, E1M1, ...), an
/// LDtk project's levels.
/// Empty for files that are one thing, and for unreadable ones.  `load`
/// keeps giving the whole file (a WAD's gallery of title screen and
/// automaps, which thumbnails show); `load_part` loads one part.
pub fn parts(path: &Path) -> Vec<String> {
    // Only Doom WADs and archives and LDtk projects have parts; other
    // files aren't read.
    match sniff_file(path) {
        // A Doom WAD's maps, a Source model's skin families.
        Ok(Some(Kind::Quake(
            f @ (slerp_quake::QuakeFormat::DoomWad
            | slerp_quake::QuakeFormat::DoomArchive
            | slerp_quake::QuakeFormat::SourceMdl),
        ))) => slerp_quake::parts_path(f, path),
        Ok(Some(Kind::Sprite(f))) => slerp_sprites::parts(f, path),
        // An .npz's arrays, an HDF5 file's datasets, a slide's levels.
        Ok(Some(Kind::Volume(f))) => slerp_volumes::parts_path(f, path),
        // A FITS file with several image extensions.
        Ok(Some(Kind::Texture(slerp_textures::Format::Fits | slerp_textures::Format::Xisf))) => {
            slerp_textures::parts_file(path)
        }
        // A PDF's or PostScript document's pages; an EPS's stored preview
        // and Ghostscript's drawing of it.
        Ok(Some(Kind::Document(f))) => slerp_documents::page_names(f, path),
        // A DWF's sheets and models, each with its own stored picture.
        Ok(Some(Kind::Preview(f))) => slerp_previews::parts(f, path),
        // An OpenEXR file's layers and parts.
        Ok(Some(Kind::Image(slerp_images::ImageFormat::Exr))) => slerp_images::exr_parts(path),
        _ => Vec::new(),
    }
}

/// Loads the part of a file called `name`, one of those `parts` lists: a
/// Doom WAD's map becomes a 3D level, an LDtk level a picture of it.
pub fn load_part(path: &Path, limits: &Limits, name: &str) -> Result<Asset, ThumbError> {
    let kind = sniff_file(path).map_err(LoadError::Io)?.ok_or(ThumbError::Unknown)?;
    if let Kind::Volume(f) = kind {
        return Ok(slerp_volumes::load_part_path(f, path, limits, name)?);
    }
    let size = std::fs::metadata(path).map_err(LoadError::Io)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")).into());
    }
    let mut asset = match kind {
        Kind::Quake(f) => slerp_quake::load_part_path(f, path, limits, name)?,
        Kind::Sprite(f) => slerp_sprites::load_part(f, path, limits, name)?,
        Kind::Texture(slerp_textures::Format::Fits | slerp_textures::Format::Xisf) => {
            let tex = slerp_textures::decode_file_part(path, Some(name), &texture_options(limits))
                .map_err(LoadError::from)?;
            Asset::Image(Arc::new(tex))
        }
        Kind::Document(f) => document_asset(slerp_documents::load_page(f, path, limits, name)?, limits)?,
        Kind::Preview(f) => slerp_previews::load_part(f, path, limits, name)?,
        Kind::Image(slerp_images::ImageFormat::Exr) => slerp_images::load_exr_part(path, limits, name)?,
        _ => return Err(LoadError::Unsupported("this file has no parts".into()).into()),
    };
    if let Asset::Scene(scene) = &mut asset {
        show_if_all_hidden(scene);
    }
    Ok(asset)
}

/// Hair colours by groom kind, linear, as chalk's `html/src/render/humans/
/// hair.ts` gives them: near black-brown hair and brows, a greying beard.
const GROOM_COLORS: [(&str, [f32; 3]); 4] = [
    ("Hair", [0.028, 0.02, 0.015]),
    ("Eyebrows", [0.02, 0.015, 0.012]),
    ("Beard", [0.06, 0.052, 0.046]),
    ("Mustache", [0.05, 0.043, 0.038]),
];

/// Gives MetaHuman hair cards a hair colour.  Their texture atlases, as
/// chalk's `tools/humans/grooms.py` bakes them, aren't colours: alpha is each
/// strand's coverage and green and blue vary strand by strand, while chalk's
/// own shader supplies the colour.  Drawn as plain PBR they come out cyan.
/// Cards are recognised by node names `hair_<Kind>_...` with an alpha-masked
/// material, and their atlas is turned into a coloured one the way chalk's
/// shader colours it.
pub fn color_groom_cards(scene: &mut scene::Scene) {
    use slerp_textures::{Pixels, Texture};
    use std::collections::HashMap;
    // 1. Which cards wear which atlas: one job per material and groom kind,
    //    each card's primitive pointing at its job.
    let mut jobs: Vec<(usize, [f32; 3], scene::TexRef)> = Vec::new();
    let mut job_of: HashMap<(usize, &str), usize> = HashMap::new();
    let mut cards: Vec<(usize, usize, usize)> = Vec::new();
    for ni in 0..scene.nodes.len() {
        let name = &scene.nodes[ni].name;
        let Some(rest) = name.strip_prefix("hair_") else { continue };
        let Some(&(kind, color)) = GROOM_COLORS.iter().find(|(k, _)| rest.starts_with(&format!("{k}_"))) else {
            continue;
        };
        let Some(mesh) = scene.nodes[ni].mesh else { continue };
        for pi in 0..scene.meshes.get(mesh).map_or(0, |m| m.primitives.len()) {
            let Some(mi) = scene.meshes[mesh].primitives[pi].material else { continue };
            let job = match job_of.get(&(mi, kind)) {
                Some(&j) => j,
                None => {
                    let Some(material) = scene.materials.get(mi) else { continue };
                    let (scene::AlphaMode::Mask(_), Some(tex_ref)) = (material.alpha_mode, material.base_color_texture)
                    else {
                        continue;
                    };
                    if tex_ref.texture >= scene.textures.len() {
                        continue;
                    }
                    jobs.push((mi, color, tex_ref));
                    job_of.insert((mi, kind), jobs.len() - 1);
                    jobs.len() - 1
                }
            };
            cards.push((mesh, pi, job));
        }
    }

    // 2. Colour each atlas as chalk does: a base colour varied by green and
    //    blue, opacity a smoothstep of coverage.  Tables for every green and
    //    blue byte pair, and every alpha byte, keep this to lookups: a
    //    character's atlases with all their mips hold tens of millions of
    //    texels.  The atlases are coloured side by side.  Atlases the viewer
    //    kept in GPU blocks are coloured as texels, which then upload as
    //    RGBA.
    let srgb = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0 + 0.5) as u8
    };
    let alphas: Vec<u8> = (0..256u32)
        .map(|a| {
            let t = ((a as f32 / 255.0 - 0.04) / (0.35 - 0.04)).clamp(0.0, 1.0);
            ((t * t * (3.0 - 2.0 * t)) * 255.0 + 0.5) as u8
        })
        .collect();
    let colored: Vec<Texture> = jobs
        .par_iter()
        .map(|&(_, color, tex_ref)| {
            let shades: Vec<[u8; 3]> = (0..65536u32)
                .into_par_iter()
                .map(|gb| {
                    let (g, b) = ((gb >> 8) as f32 / 255.0, (gb & 255) as f32 / 255.0);
                    let shade = (0.7 + 0.65 * g) * (0.85 + 0.25 * b);
                    [srgb(color[0] * shade), srgb(color[1] * shade), srgb(color[2] * shade)]
                })
                .collect();
            let mut colored = slerp_textures::decompress(&scene.textures[tex_ref.texture]);
            for level in &mut colored.levels {
                for surface in &mut level.surfaces {
                    let Pixels::Rgba8(px) = surface else { continue };
                    px.par_chunks_mut(4 * 4096).for_each(|chunk| {
                        for p in chunk.chunks_exact_mut(4) {
                            let [r, g, b] = shades[(p[1] as usize) << 8 | p[2] as usize];
                            p[3] = alphas[p[3] as usize];
                            (p[0], p[1], p[2]) = (r, g, b);
                        }
                    });
                }
            }
            colored.color_space = slerp_textures::ColorSpace::Srgb;
            colored
        })
        .collect();

    // 3. A new material for each job, using its coloured atlas, shared by
    //    every card of the same kind wearing the same material.
    let mut replacement = Vec::with_capacity(jobs.len());
    for ((mi, _, tex_ref), texture) in jobs.into_iter().zip(colored) {
        scene.textures.push(Arc::new(texture));
        let mut m = scene.materials[mi].clone();
        m.base_color = glam::Vec4::ONE;
        m.base_color_texture = Some(scene::TexRef { texture: scene.textures.len() - 1, ..tex_ref });
        // chalk's depth pass cuts the smoothed coverage at 0.2, which keeps
        // strands from vanishing in small mips.
        m.alpha_mode = scene::AlphaMode::Mask(0.2);
        m.roughness = 0.5;
        m.metallic = 0.0;
        m.double_sided = true;
        scene.materials.push(m);
        replacement.push(scene.materials.len() - 1);
    }
    for (mesh, pi, job) in cards {
        scene.meshes[mesh].primitives[pi].material = Some(replacement[job]);
    }
}

/// Shows every mesh when the file hides all of them.  Old FBX files (3ds
/// Max 6 and 7 exports) mark every node invisible, which can only be an
/// exporter quirk: nobody saves a model to show nothing.
pub fn show_if_all_hidden(scene: &mut scene::Scene) {
    let has_mesh = |n: &scene::Node| n.mesh.is_some();
    if scene.nodes.iter().any(has_mesh) && !scene.nodes.iter().any(|n| has_mesh(n) && n.visible) {
        for node in &mut scene.nodes {
            node.visible = true;
        }
    }
}

/// Hides meshes that only make sense with their engine's special shaders.
/// MetaHuman eyes carry an `eyeshell` (a wet film over the eyeball) and an
/// `eyeEdge` (a blur ring at the lids); drawn with ordinary PBR they come
/// out as opaque white shells over the eyes, so chalk's own viewer hides
/// them too.
pub fn hide_helper_meshes(scene: &mut scene::Scene) {
    let helper = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.contains("eyeshell") || n.contains("eyeedge")
    };
    let meshes: Vec<bool> = scene.meshes.iter().map(|m| helper(&m.name)).collect();
    for node in &mut scene.nodes {
        if helper(&node.name) || node.mesh.is_some_and(|m| meshes.get(m).copied().unwrap_or(false)) {
            node.visible = false;
        }
    }
}

/// Limits for loading a file to make a thumbnail of `size` pixels.
pub fn thumbnail_limits(size: u32) -> Limits {
    Limits { max_texture_size: Some(size.clamp(64, 1024)), ..Limits::thumbnail() }
}

/// The result of the CPU half of making a thumbnail.
pub enum Prepared {
    /// A finished picture: textures, galleries, stored previews.
    Done(Image8),
    /// A scene the GPU must draw.
    Render(Box<scene::Scene>),
}

/// Loads `path` and makes its thumbnail if that needs no GPU.  Safe to call
/// from many threads at once.
///
/// A picture the file carries of itself comes first ([`saved_picture`]);
/// only files without one are loaded and drawn.  When the content can't be
/// shown, a saved picture too poor to stand for the file still beats no
/// thumbnail.
pub fn prepare_thumbnail(path: &Path, size: u32) -> Result<Prepared, ThumbError> {
    let kind = sniff_file(path).map_err(LoadError::Io)?.ok_or(ThumbError::Unknown)?;
    let saved = carried_picture(kind, path, &thumbnail_limits(size));
    if let Some(s) = &saved
        && s.stands_for_file(size)
    {
        return Ok(s.thumbnail(size));
    }
    match (prepare_content(kind, path, size), saved) {
        (Err(_), Some(s)) => Ok(s.thumbnail(size)),
        (result, _) => result,
    }
}

/// The thumbnail of a file's content: its picture, a drawing, or a scene
/// to render.
fn prepare_content(kind: Kind, path: &Path, size: u32) -> Result<Prepared, ThumbError> {
    // Palettes are drawn at the size asked for, so their swatches stay
    // crisp squares.
    if let Kind::Palette(f) = kind {
        let p = palette::read_path(f, path, &thumbnail_limits(size))?;
        return Ok(Prepared::Done(palette::thumbnail(&p, size)));
    }
    // Tracks and flat laser jobs are drawn from above; other toolpaths
    // rendered.
    if let Kind::Path(f) = kind {
        return Ok(match slerp_paths::thumbnail(f, path, size)? {
            slerp_paths::Thumb::Drawn(image) => Prepared::Done(image),
            slerp_paths::Thumb::Scene(scene) => Prepared::Render(scene),
        });
    }
    // CAD: flat drawings are drawn on paper on the CPU, Rhino files without
    // render meshes give their stored picture, anything else is rendered.
    if let Kind::Cad(f) = kind {
        return Ok(match slerp_cad::thumbnail(f, path, size, &thumbnail_limits(size))? {
            slerp_cad::Thumb::Picture(tex) => Prepared::Done(slerp_textures::thumbnail(&tex, size)),
            slerp_cad::Thumb::Drawn(image) => Prepared::Done(image),
            slerp_cad::Thumb::Scene(scene) => Prepared::Render(scene),
        });
    }
    let asset = load_kind(kind, path, &thumbnail_limits(size))?;
    let mut prepared = prepare_asset(kind, asset, size)?;
    if let Prepared::Render(scene) = &mut prepared
        && scene.kind == scene::SceneKind::Level
    {
        strip_sky(scene);
    }
    Ok(prepared)
}

/// Removes sky surfaces (Quake's and Doom's).  Seen from outside a map
/// they are its far walls and ceilings painted with clouds, which hide the
/// layout a level thumbnail is meant to show.
pub fn strip_sky(scene: &mut scene::Scene) {
    let sky: Vec<bool> = scene.materials.iter().map(|m| m.effect.is_sky()).collect();
    for mesh in &mut scene.meshes {
        mesh.primitives.retain(|p| !p.material.is_some_and(|m| sky.get(m).copied().unwrap_or(false)));
    }
}

/// Makes a thumbnail from an already loaded asset when no GPU is needed.
pub fn prepare_asset(kind: Kind, asset: Asset, size: u32) -> Result<Prepared, ThumbError> {
    Ok(match asset {
        // A stored preview keeps its transparency, as its app shows it
        // (round icons, cut-out sprites), rather than turning black.
        // Metafiles, Visio's thumbnails (metafiles) and documents draw on a
        // white page, as Windows shows them, so their black lines stay
        // visible.
        Asset::Image(tex)
            if matches!(
                kind,
                Kind::Metafile(_) | Kind::Document(_) | Kind::Preview(slerp_previews::PreviewFormat::Visio)
            ) =>
        {
            Prepared::Done(slerp_textures::thumbnail(&on_white(&tex), size))
        }
        Asset::Image(tex) if matches!(kind, Kind::Preview(_)) => {
            Prepared::Done(with_alpha(&tex, slerp_textures::thumbnail(&tex, size)))
        }
        Asset::Image(tex) => Prepared::Done(slerp_textures::thumbnail(&tex, size)),
        Asset::Gallery(items) => {
            if items.is_empty() {
                return Err(ThumbError::Empty);
            }
            // A Doom WAD or archive shows its title screen; texture packs
            // show a grid.
            let single =
                matches!(kind, Kind::Quake(slerp_quake::QuakeFormat::DoomWad | slerp_quake::QuakeFormat::DoomArchive));
            Prepared::Done(gallery_thumbnail(&items, size, single))
        }
        Asset::Material(mat) => match &mat.preview {
            Some(preview) => Prepared::Done(slerp_textures::thumbnail(preview, size)),
            None => {
                let mut ball = scene::Scene::material_ball(&mat, scene::Shape::Sphere, 128);
                // Material files give displacement in their own units:
                // ambientCG's say 1.0, which on a 1 m ball would turn it into
                // a hedgehog.  A few centimetres reads as relief.
                for m in &mut ball.materials {
                    m.height_scale = m.height_scale.clamp(0.0, 0.03);
                }
                Prepared::Render(Box::new(ball))
            }
        },
        Asset::Scene(scene) => Prepared::Render(Box::new(scene)),
        Asset::Canvas(canvas) => Prepared::Done(slerp_sprites::thumbnail(&*canvas, size)),
        Asset::Lut(lut) => Prepared::Done(slerp_lut::thumbnail(&*lut, size)),
    })
}

/// A copy of a picture composited over white, for drawings that assume a
/// white page.  Pictures without transparency come back unchanged.
pub fn on_white(tex: &slerp_textures::Texture) -> slerp_textures::Texture {
    use slerp_textures::Pixels;
    let mut out = tex.clone();
    if !tex.has_alpha {
        return out;
    }
    if let Some(level) = out.levels.first_mut() {
        if let Some(Pixels::Rgba8(px)) = level.surfaces.first_mut() {
            for p in px.chunks_exact_mut(4) {
                let a = p[3] as u32;
                for c in p.iter_mut().take(3) {
                    *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
                }
                p[3] = 255;
            }
        }
    }
    out.has_alpha = false;
    out
}

/// Lays out the first few pictures of a gallery in a square grid: 3x3 when
/// there are at least nine, 2x2 for four to eight, otherwise the first.
pub fn gallery_thumbnail(items: &[NamedImage], size: u32, single: bool) -> Image8 {
    let n = if single { 1 } else { items.len() };
    let cols: u32 = if n >= 9 {
        3
    } else if n >= 4 {
        2
    } else {
        1
    };
    if cols == 1 {
        return slerp_textures::thumbnail(&items[0].texture, size);
    }
    let gap = (size / 64).max(1);
    let cell = (size - gap * (cols - 1)) / cols;
    let mut out = Image8::new(size, size);
    for (i, item) in items.iter().take((cols * cols) as usize).enumerate() {
        let tile = slerp_textures::thumbnail(&item.texture, cell);
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        // Centre each tile in its cell.
        let x0 = cx * (cell + gap) + (cell - tile.width.min(cell)) / 2;
        let y0 = cy * (cell + gap) + (cell - tile.height.min(cell)) / 2;
        for y in 0..tile.height.min(cell) {
            let src = (y * tile.width * 4) as usize;
            let dst = (((y0 + y) * size + x0) * 4) as usize;
            let len = (tile.width.min(cell) * 4) as usize;
            out.pixels[dst..dst + len].copy_from_slice(&tile.pixels[src..src + len]);
        }
    }
    out
}

/// Encodes a thumbnail as PNG, quickly rather than small, since the file
/// manager caches it anyway.
pub fn encode_png(img: &Image8) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().expect("writing to a Vec cannot fail");
        writer.write_image_data(&img.pixels).expect("writing to a Vec cannot fail");
    }
    out
}

/// Writes `data` to `path` through a temporary file in the same folder, so
/// a reader never sees half a file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A plain-text summary of what a file holds, for `slerp info`.
pub fn describe(asset: &Asset) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let texture = |s: &mut String, t: &slerp_textures::Texture| {
        let src = &t.source;
        let _ = writeln!(
            s,
            "texture: {} {} {}x{}x{} layers {} faces {} mips {}{}{}",
            src.container,
            src.format,
            src.width,
            src.height,
            src.depth,
            src.layers,
            src.faces,
            src.mip_count,
            src.supercompression.as_deref().map(|c| format!(" ({c})")).unwrap_or_default(),
            if t.has_alpha { ", alpha" } else { "" }
        );
        // Header cards and fields (FITS, XISF, DPX, Cineon).
        for (k, v) in src.header.iter().take(60) {
            let _ = writeln!(s, "  {k}: {v}");
        }
        if src.header.len() > 60 {
            let _ = writeln!(s, "  ... {} more", src.header.len() - 60);
        }
    };
    let info = |s: &mut String, i: &scene::Info| {
        let _ = writeln!(s, "format: {}", i.format);
        if let Some(g) = &i.generator {
            let _ = writeln!(s, "generator: {g}");
        }
        for (k, v) in &i.details {
            let _ = writeln!(s, "{k}: {v}");
        }
        for w in &i.warnings {
            let _ = writeln!(s, "warning: {w}");
        }
    };
    match asset {
        Asset::Image(t) => texture(&mut s, t),
        Asset::Gallery(items) => {
            let _ = writeln!(s, "gallery: {} images", items.len());
            for item in items.iter().take(50) {
                let _ = writeln!(s, "  {} {}x{}", item.name, item.texture.width(), item.texture.height());
            }
        }
        Asset::Canvas(c) => {
            info(&mut s, c.info());
            let (w, h) = c.size();
            let _ = writeln!(s, "size: {w} x {h}\nframes: {}\nlayers: {}", c.frames().len(), c.layers().len());
            for l in c.layers().iter().take(50) {
                let indent = "  ".repeat(l.depth as usize + 1);
                let hidden = if l.visible { "" } else { " (hidden)" };
                let note = if l.note.is_empty() { String::new() } else { format!(" [{}]", l.note) };
                let _ = writeln!(s, "{indent}layer {}{note}{hidden}", l.name);
            }
            for c in c.clips().iter().take(50) {
                let _ = writeln!(s, "  clip {} frames {}-{} {:?} x{}", c.name, c.from, c.to, c.direction, c.repeat);
            }
        }
        Asset::Lut(l) => info(&mut s, l.info()),
        Asset::Material(m) => {
            info(&mut s, &m.info);
            let _ =
                writeln!(s, "material: {} ({} textures, preview: {})", m.name, m.textures.len(), m.preview.is_some());
        }
        Asset::Scene(sc) => {
            info(&mut s, &sc.info);
            let joints: usize = sc.skins.iter().map(|k| k.joints.len()).sum();
            let morphs: usize =
                sc.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.targets.len()).max().unwrap_or(0);
            let _ = writeln!(
                s,
                "kind: {:?}\nnodes: {}\nmeshes: {}\nprimitives: {}\ntriangles: {}\nvertices: {}\nmaterials: {}\ntextures: {}\nskins: {} ({} joints)\nmorph targets: {}\nclips: {}",
                sc.kind,
                sc.nodes.len(),
                sc.meshes.len(),
                sc.meshes.iter().map(|m| m.primitives.len()).sum::<usize>(),
                sc.triangle_count(),
                sc.vertex_count(),
                sc.materials.len(),
                sc.textures.len(),
                sc.skins.len(),
                joints,
                morphs,
                sc.animations.len()
            );
            for a in sc.animations.iter().take(50) {
                let _ = writeln!(s, "  clip {} {:.2}s", a.name, a.duration);
            }
            if let Some(sp) = &sc.splats {
                let full = sp.full_bounds();
                let _ = writeln!(s, "splats: {} (spherical harmonics degree {})", sp.len(), sp.sh_degree);
                if !full.is_empty() {
                    let _ = writeln!(s, "all splat centres: {} .. {}", full.min, full.max);
                }
            }
            let bounds = sc.bounds(&scene::Pose::rest(sc));
            if !bounds.is_empty() {
                let _ = writeln!(s, "bounds: {} .. {}", bounds.min, bounds.max);
            }
            if let Some(sp) = sc.spawn {
                let _ = writeln!(s, "spawn: {} yaw {:.0}", sp.position, sp.yaw.to_degrees());
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    /// A program finds a job by its name alone, so no two may share one.
    #[test]
    fn isolated_jobs_have_unique_names() {
        let mut names: Vec<&str> = super::ISOLATED_JOBS.iter().flat_map(|g| g.iter()).map(|j| j.name()).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "{names:?}");
        assert!(n >= 9, "{names:?}");
    }
}
