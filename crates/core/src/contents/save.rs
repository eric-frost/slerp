//! Turning the pieces of a loaded file into files: the bytes the file
//! stored when there are any (an embedded PNG comes out unchanged), else a
//! conversion that loses nothing: 8-bit pictures as PNG, floating-point ones
//! as OpenEXR, scenes, nodes, meshes, materials, skeletons and clips as GLB.
//! Also the small pictures the panel shows beside items.

use super::glb::{self, GlbOptions, Selection};
use super::pieces::Piece;
use super::{Contents, ContentsError, ExtractOptions, Format, Item, ItemId, ItemKind, Result, Source};
use slerp_scene::{Asset, Image8, Limits, Scene, Texture};
use slerp_textures::{ColorSpace, Pixels};
use std::sync::Arc;

/*┌─────────────────╮
| Original bytes  |
└─────────────────╯*/
//region Original bytes

/// Whether the loader kept the bytes a texture was decoded from: a GLB's
/// embedded PNG, a `.blend` file's packed JPEG, an FBX's embedded texture,
/// a texture file beside an OBJ.
pub(crate) fn has_original(t: &Texture) -> bool {
    t.source.original.as_ref().is_some_and(|o| !o.is_empty())
}

/// The file extension of a texture's original bytes, from their first
/// bytes, else from the container the decoder named.
pub(crate) fn original_extension(t: &Texture) -> &'static str {
    let head: Vec<u8> = match &t.source.original {
        Some(slerp_textures::Original::Memory(b)) => b.iter().take(16).copied().collect(),
        Some(slerp_textures::Original::File { path, offset, len }) => {
            use std::io::{Read, Seek, SeekFrom};
            let mut head = Vec::new();
            if let Ok(mut f) = std::fs::File::open(path)
                && f.seek(SeekFrom::Start(*offset)).is_ok()
            {
                let _ = f.take((*len).min(16)).read_to_end(&mut head);
            }
            head
        }
        None => Vec::new(),
    };
    let magic: [(&[u8], &str); 10] = [
        (b"\x89PNG", "png"),
        (b"\xff\xd8\xff", "jpg"),
        (b"\xabKTX 20", "ktx2"),
        (b"\xabKTX 11", "ktx"),
        (b"DDS ", "dds"),
        (b"GIF8", "gif"),
        (b"\x76\x2f\x31\x01", "exr"),
        (b"#?RADIANCE", "hdr"),
        (b"#?RGBE", "hdr"),
        (b"BM", "bmp"),
    ];
    if let Some((_, ext)) = magic.iter().find(|(m, _)| head.starts_with(m)) {
        return ext;
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return "webp";
    }
    match t.source.container.to_ascii_lowercase().as_str() {
        "tga" | "targa" => "tga",
        "png" => "png",
        "jpeg" | "jpg" => "jpg",
        "ktx2" => "ktx2",
        "webp" => "webp",
        "dds" => "dds",
        _ => "bin",
    }
}

/// An item's bytes as the file stored them, when it has its own.
pub(super) fn original(c: &Contents, piece: &Piece) -> Option<Vec<u8>> {
    let texture = match piece {
        Piece::Texture(i) => c.held.textures.get(*i)?,
        Piece::MaterialPreview => c.held.material_preview.as_ref()?,
        Piece::SavedPicture => c.held.saved_picture.as_ref()?,
        _ => return None,
    };
    texture.source.original.as_ref()?.read().ok().map(|b| b.into_owned())
}

//endregion

/*┌─────────────────╮
| Saving          |
└─────────────────╯*/
//region Saving

/// The file name a piece gets when saved as `format`.
pub(super) fn file_name(c: &Contents, item: &Item, piece: &Piece, format: Format) -> String {
    let stem = c.path().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match piece {
        Piece::Scene | Piece::Detail(_) | Piece::Panorama { .. } if !stem.is_empty() => match piece {
            Piece::Detail(lod) => format!("{stem} LOD {lod}"),
            Piece::Panorama { .. } => format!("{stem} {}", item.name().to_lowercase()),
            _ => stem,
        },
        _ => item.name().to_string(),
    };
    let ext = match (format, piece) {
        (Format::Raw, Piece::Texture(i)) => c.held.textures.get(*i).map_or("bin", |t| original_extension(t)),
        (Format::Raw, Piece::MaterialPreview) => c.held.material_preview.as_deref().map_or("bin", original_extension),
        (Format::Raw, Piece::SavedPicture) => c.held.saved_picture.as_deref().map_or("bin", original_extension),
        (f, _) => f.extension(),
    };
    // A name that already ends with the extension (a texture named after
    // its file) keeps it once.
    if name.to_ascii_lowercase().ends_with(&format!(".{ext}")) { name } else { format!("{name}.{ext}") }
}

/// Saves a piece as `format` (already resolved for the item).
pub(super) fn export(
    c: &Contents,
    item: &Item,
    piece: &Piece,
    format: Format,
    options: &ExtractOptions,
) -> Result<(String, Vec<u8>)> {
    let name = file_name(c, item, piece, format);
    let unsupported = || ContentsError::Unsupported(format!("{} can't be saved as {}", item.path, format.name()));
    if format == Format::Raw {
        return original(c, piece).map(|bytes| (name, bytes)).ok_or_else(unsupported);
    }
    let bytes = match piece {
        Piece::Scene
        | Piece::Node(_)
        | Piece::Collection(_)
        | Piece::Mesh(_)
        | Piece::Material(_)
        | Piece::Skeleton(_)
        | Piece::Animation(_) => {
            if format != Format::Glb {
                return Err(unsupported());
            }
            let selection = match piece {
                Piece::Scene => Selection::Scene,
                Piece::Node(i) => Selection::Node(*i),
                Piece::Collection(g) => {
                    let nodes = super::pieces::collection_nodes(&c.held.collections, *g, c.held.node_refs.len());
                    if nodes.is_empty() {
                        return Err(ContentsError::Unsupported(format!("{} holds no nodes", item.path)));
                    }
                    Selection::Nodes(nodes)
                }
                Piece::Mesh(i) => Selection::Mesh(*i),
                Piece::Material(i) => Selection::Material(*i),
                Piece::Skeleton(i) => Selection::Skeleton(*i),
                Piece::Animation(i) => Selection::Animation { clip: *i, skeleton_only: options.skeleton_only },
                _ => unreachable!(),
            };
            let scene = c.scene()?;
            glb::write(&scene, &selection, GlbOptions { centre: options.centre })?
        }
        Piece::Texture(i) => {
            let t = c.held.textures.get(*i).ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(t, 0, 0, 0, format)?
        }
        Piece::MaterialPreview => {
            let t = c.held.material_preview.as_ref().ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(t, 0, 0, 0, format)?
        }
        Piece::SavedPicture => {
            let t = c.held.saved_picture.as_ref().ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(t, 0, 0, 0, format)?
        }
        Piece::Surface { level, layer, face } => {
            let t = c.held.picture.as_ref().ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(t, *level, *layer, *face, format)?
        }
        Piece::Panorama { layer } => {
            let t = c.held.picture.as_ref().ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(&super::panorama::panorama(t, *layer, super::panorama::MAX_WIDTH)?, 0, 0, 0, format)?
        }
        Piece::Picture(i) => {
            let (_, t) = c.held.gallery.get(*i).ok_or(ContentsError::NoSuchItem)?;
            picture_bytes(t, 0, 0, 0, format)?
        }
        Piece::Layer(i) => image_bytes(&layer_image(c, *i, 1.0)?, format)?,
        Piece::Frame(i) => image_bytes(&frame_image(c, *i, 1.0)?, format)?,
        Piece::Tag(i) => image_bytes(&tag_sheet(c, *i)?, format)?,
        Piece::MaterialBall => image_bytes(&material_ball(c, 1024)?, format)?,
        Piece::Part(name) => part_bytes(c, name, format, options)?,
        Piece::Detail(lod) => {
            if format != Format::Glb {
                return Err(unsupported());
            }
            let scene = character_at(c, *lod)?;
            glb::write(&scene, &Selection::Scene, GlbOptions { centre: options.centre })?
        }
        Piece::BlendShape { .. } | Piece::Expression(_) | Piece::Listed => {
            return Err(ContentsError::Unsupported(format!("{} only means something inside its file", item.path)));
        }
    };
    Ok((name, bytes))
}

impl Contents {
    /// The scene the pieces index: the one handed over, or the file loaded
    /// again once.
    pub(crate) fn scene(&self) -> Result<Arc<Scene>> {
        let loaded = self.scene.get_or_init(|| {
            let (Some(kind), Some(path)) = (self.kind, self.path.as_deref()) else {
                return Err("there is no scene".to_string());
            };
            if kind == crate::Kind::Dna {
                let opts = slerp_dna::Options { rig: false, ..slerp_dna::Options::viewer() };
                return slerp_dna::open(path, &opts).map(|c| Arc::new(c.scene)).map_err(|e| e.to_string());
            }
            match crate::load_kind(kind, path, &Limits::viewer()) {
                Ok(Asset::Scene(scene)) => Ok(Arc::new(scene)),
                Ok(_) => Err("the file no longer holds a scene".into()),
                Err(e) => Err(e.to_string()),
            }
        });
        loaded.clone().map_err(|e| ContentsError::Convert(format!("the scene couldn't be loaded again: {e}")))
    }
}

/// A MetaHuman character at level of detail `lod`, read again, for saving.
fn character_at(c: &Contents, lod: u16) -> Result<Scene> {
    let path = c.path().ok_or(ContentsError::NoSuchItem)?;
    let opts = slerp_dna::Options { lod, rig: false, ..slerp_dna::Options::viewer() };
    slerp_dna::open(path, &opts).map(|c| c.scene).map_err(|e| ContentsError::Convert(e.to_string()))
}

/// One part of the file, loaded and saved as `format`.
fn part_bytes(c: &Contents, name: &str, format: Format, options: &ExtractOptions) -> Result<Vec<u8>> {
    let path = c.path().ok_or(ContentsError::NoSuchItem)?;
    let asset = crate::load_part(path, &Limits::viewer(), name).map_err(|e| ContentsError::Convert(e.to_string()))?;
    match asset {
        Asset::Image(t) => picture_bytes(&t, 0, 0, 0, format),
        Asset::Gallery(g) if !g.is_empty() => picture_bytes(&g[0].texture, 0, 0, 0, format),
        Asset::Canvas(canvas) => {
            let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
            image_bytes(&canvas.compose(0, &shown, 1.0), format)
        }
        Asset::Scene(scene) if format == Format::Glb => {
            glb::write(&scene, &Selection::Scene, GlbOptions { centre: options.centre })
        }
        _ => Err(ContentsError::Unsupported(format!("{name} can't be saved as {}", format.name()))),
    }
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// One surface of a texture as a PNG or EXR file.
pub(crate) fn picture_bytes(t: &Texture, level: u32, layer: u32, face: u32, format: Format) -> Result<Vec<u8>> {
    let (w, h, pixels) = surface(t, level, layer, face)?;
    match (format, pixels) {
        (Format::Exr, Pixels::Rgba32F(px)) => super::exr_bytes(w, h, &px, t.has_alpha),
        (Format::Exr, Pixels::Rgba8(px)) => {
            // Stored values become linear light, as EXR files hold.
            let srgb = t.color_space != ColorSpace::Linear;
            let f: Vec<f32> = px
                .chunks_exact(4)
                .flat_map(|p| {
                    let c = |v: u8| if srgb { srgb_to_linear(v as f32 / 255.0) } else { v as f32 / 255.0 };
                    [c(p[0]), c(p[1]), c(p[2]), p[3] as f32 / 255.0]
                })
                .collect();
            super::exr_bytes(w, h, &f, t.has_alpha)
        }
        (Format::Png, Pixels::Rgba8(px)) => image_bytes(&Image8 { width: w, height: h, pixels: px }, Format::Png),
        (Format::Png, Pixels::Rgba32F(px)) => {
            // Linear light, clipped, in sRGB: the plain view of the picture.
            let rgba: Vec<u8> = px
                .chunks_exact(4)
                .flat_map(|p| {
                    let c = |v: f32| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
                    [c(p[0]), c(p[1]), c(p[2]), (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
                })
                .collect();
            image_bytes(&Image8 { width: w, height: h, pixels: rgba }, Format::Png)
        }
        (f, _) => Err(ContentsError::Unsupported(format!("a picture can't be saved as {}", f.name()))),
    }
}

/// The pixels of one surface, GPU blocks decoded.
pub(crate) fn surface(t: &Texture, level: u32, layer: u32, face: u32) -> Result<(u32, u32, Pixels)> {
    let missing = || ContentsError::Convert(format!("the picture has no level {level}, layer {layer}, face {face}"));
    let l = t.levels.get(level as usize).ok_or_else(missing)?;
    let index = (layer as usize).checked_mul(t.faces.max(1) as usize).and_then(|i| i.checked_add(face as usize));
    let pixels = index.and_then(|i| l.surfaces.get(i)).ok_or_else(missing)?;
    let pixels = match pixels {
        Pixels::Blocks(..) => {
            let one = Texture {
                levels: vec![slerp_textures::Level { surfaces: vec![pixels.clone()], ..l.clone() }],
                layers: 1,
                faces: 1,
                kind: slerp_textures::TextureKind::D2,
                ..t.clone_meta()
            };
            slerp_textures::decompress(&one).levels.remove(0).surfaces.remove(0)
        }
        p => p.clone(),
    };
    // A 3D texture's surface holds every slice; the first is the picture.
    let texels = l.width as usize * l.height as usize;
    let pixels = match pixels {
        Pixels::Rgba8(mut v) if v.len() > texels * 4 => {
            v.truncate(texels * 4);
            Pixels::Rgba8(v)
        }
        Pixels::Rgba32F(mut v) if v.len() > texels * 4 => {
            v.truncate(texels * 4);
            Pixels::Rgba32F(v)
        }
        p => p,
    };
    Ok((l.width, l.height, pixels))
}

/// Metadata of a texture without its pixels, for building one-level copies.
trait CloneMeta {
    fn clone_meta(&self) -> Texture;
}

impl CloneMeta for Texture {
    fn clone_meta(&self) -> Texture {
        Texture {
            kind: self.kind,
            layers: self.layers,
            faces: self.faces,
            levels: Vec::new(),
            color_space: self.color_space,
            has_alpha: self.has_alpha,
            source: self.source.clone(),
        }
    }
}

/// An 8-bit picture as a PNG file (or EXR, for a picture asked for as one).
fn image_bytes(img: &Image8, format: Format) -> Result<Vec<u8>> {
    match format {
        Format::Png => super::png_bytes(&slerp_quake::convert::Image {
            width: img.width,
            height: img.height,
            rgba: img.pixels.clone(),
            offset: None,
        }),
        Format::Exr => {
            let t = Texture::from_rgba8(img.width, img.height, img.pixels.clone(), ColorSpace::Srgb);
            picture_bytes(&t, 0, 0, 0, Format::Exr)
        }
        f => Err(ContentsError::Unsupported(format!("a picture can't be saved as {}", f.name()))),
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

//endregion

/*┌─────────────────╮
| Canvases        |
└─────────────────╯*/
//region Canvases

fn canvas(c: &Contents) -> Result<&Arc<dyn slerp_scene::Canvas>> {
    c.held.canvas.as_ref().ok_or(ContentsError::NoSuchItem)
}

/// Which layers to draw to show layer `i` alone: it, the groups it sits in,
/// and, for a group, everything inside it.
pub fn layer_alone(layers: &[slerp_scene::CanvasLayer], i: usize) -> Vec<bool> {
    let mut shown = vec![false; layers.len()];
    let Some(target) = layers.get(i) else { return shown };
    shown[i] = true;
    // Groups come before what they hold: walking back, each shallower layer
    // is the next group up.
    let mut depth = target.depth;
    for j in (0..i).rev() {
        if layers[j].depth < depth && layers[j].group {
            shown[j] = true;
            depth = layers[j].depth;
        }
    }
    if target.group {
        for j in i + 1..layers.len() {
            if layers[j].depth <= target.depth {
                break;
            }
            shown[j] = true;
        }
    }
    shown
}

fn layer_image(c: &Contents, i: usize, scale: f32) -> Result<Image8> {
    let canvas = canvas(c)?;
    let shown = layer_alone(canvas.layers(), i);
    Ok(canvas.compose(0, &shown, scale))
}

fn frame_image(c: &Contents, i: usize, scale: f32) -> Result<Image8> {
    let canvas = canvas(c)?;
    let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
    Ok(canvas.compose(i.min(canvas.frames().len().saturating_sub(1)), &shown, scale))
}

/// The largest sprite sheet made, on its longer side.
const MAX_SHEET_SIDE: u32 = 16384;

/// A tag's frames side by side, wrapping into rows when a strip would be
/// too wide: the sprite sheet game engines import.
fn tag_sheet(c: &Contents, i: usize) -> Result<Image8> {
    let canvas = canvas(c)?;
    let clip = canvas.clips().get(i).ok_or(ContentsError::NoSuchItem)?;
    let frames: Vec<usize> = (clip.from..=clip.to.min(canvas.frames().len().saturating_sub(1))).collect();
    let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
    let (w, h) = canvas.size();
    let (w, h) = (w.max(1), h.max(1));
    let n = frames.len().max(1) as u32;
    let columns = (MAX_SHEET_SIDE / w).clamp(1, n);
    let rows = n.div_ceil(columns);
    if rows.saturating_mul(h) > MAX_SHEET_SIDE {
        return Err(ContentsError::Unsupported("the tag's frames don't fit in one sprite sheet".into()));
    }
    let mut sheet = Image8::new(columns * w, rows * h);
    for (k, &f) in frames.iter().enumerate() {
        let img = canvas.compose(f, &shown, 1.0);
        let (x0, y0) = ((k as u32 % columns) * w, (k as u32 / columns) * h);
        for y in 0..img.height.min(h) {
            let src = (y * img.width * 4) as usize;
            let dst = (((y0 + y) * sheet.width + x0) * 4) as usize;
            let len = (img.width.min(w) * 4) as usize;
            sheet.pixels[dst..dst + len].copy_from_slice(&img.pixels[src..src + len]);
        }
    }
    Ok(sheet)
}

//endregion

/*┌─────────────────╮
| Material ball   |
└─────────────────╯*/
//region Material ball

/// A material file's material drawn on a ball by the GPU, as thumbnails
/// draw it.
fn material_ball(c: &Contents, size: u32) -> Result<Image8> {
    let (name, material) = c.held.material.clone().ok_or(ContentsError::NoSuchItem)?;
    let asset = slerp_scene::MaterialAsset {
        name,
        material,
        textures: c.held.textures.clone(),
        preview: None,
        info: Default::default(),
    };
    let ball = crate::material_scene(&asset, &crate::MaterialState::default());
    let mut renderer = crate::ThumbRenderer::new(&crate::Config::load().gpu);
    renderer.render_thumbnail(&ball, size).map_err(|e| ContentsError::Convert(e.to_string()))
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

/// The most bytes of an archive entry read to make its row's picture.
const MAX_THUMB_ENTRY: u64 = 4 << 20;

/// A small picture of an item, when one is cheap to make.
pub(super) fn thumbnail(c: &Contents, id: ItemId, size: u32) -> Option<Image8> {
    let item = c.item(id)?;
    let fit = |t: &Texture| Some(slerp_textures::thumbnail(t, size));
    match &item.source {
        Source::Piece(p) => match p {
            Piece::Texture(i) => fit(c.held.textures.get(*i)?),
            Piece::MaterialPreview => fit(c.held.material_preview.as_ref()?),
            Piece::SavedPicture => fit(c.held.saved_picture.as_ref()?),
            Piece::Picture(i) => fit(&c.held.gallery.get(*i)?.1),
            Piece::Surface { level, layer, face } => {
                let t = c.held.picture.as_ref()?;
                let (w, h, pixels) = surface(t, *level, *layer, *face).ok()?;
                let one = Texture {
                    levels: vec![slerp_textures::Level { width: w, height: h, depth: 1, surfaces: vec![pixels] }],
                    layers: 1,
                    faces: 1,
                    kind: slerp_textures::TextureKind::D2,
                    ..t.clone_meta()
                };
                fit(&one)
            }
            Piece::Panorama { layer } => {
                let t = c.held.picture.as_ref()?;
                fit(&super::panorama::panorama(t, *layer, size.saturating_mul(2)).ok()?)
            }
            Piece::Layer(_) | Piece::Frame(_) | Piece::Tag(_) => {
                let canvas = c.held.canvas.as_ref()?;
                let (w, h) = canvas.size();
                let scale = (size as f32 / w.max(h).max(1) as f32).min(1.0);
                let img = match p {
                    Piece::Layer(i) => layer_image(c, *i, scale).ok()?,
                    Piece::Frame(i) => frame_image(c, *i, scale).ok()?,
                    Piece::Tag(i) => frame_image(c, canvas.clips().get(*i)?.from, scale).ok()?,
                    _ => unreachable!(),
                };
                fit(&Texture::from_rgba8(img.width, img.height, img.pixels, ColorSpace::Srgb))
            }
            _ => None,
        },
        Source::Entry(_) | Source::Composite { .. } => {
            let picture = matches!(
                item.kind,
                ItemKind::Picture | ItemKind::Flat | ItemKind::Sprite | ItemKind::Texture | ItemKind::Palette
            );
            if !picture || item.locked || item.size > MAX_THUMB_ENTRY {
                return None;
            }
            let preview = c.preview(id).ok()?;
            let super::PreviewKind::File { bytes, .. } = preview.kind else { return None };
            let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                slerp_textures::decode(&bytes, &slerp_textures::DecodeOptions::preview(size))
            }));
            fit(&decoded.ok()?.ok()?)
        }
    }
}

//endregion
