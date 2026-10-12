//! Texture mode (one picture with pan, zoom and mip, layer, face and
//! channel pickers) and gallery mode (a grid of a texture pack's or Doom
//! WAD's named pictures, each opening in texture mode).

use crate::gfx::{Gfx, next_id};
use crate::scene_view::SceneView;
use slerp_render::{GpuTexture, TextureDrawParams};
use slerp_scene::textures::stretch::{self, DataStats, Points, Stretch};
use slerp_scene::textures::{Pixels, TextureKind};
use slerp_scene::{ColorSpace, Image8, Material, Mesh, NamedImage, Scene, Shape, Texture};
use std::sync::Arc;

/// Screen pixels per texel at the extremes of zooming.
const MIN_ZOOM: f32 = 1.0 / 64.0;
const MAX_ZOOM: f32 = 256.0;

pub struct TextureView {
    pub id: u64,
    pub name: String,
    pub texture: Arc<Texture>,
    pub gpu: GpuTexture,
    pub params: TextureDrawParams,
    /// Keeps the picture fitted to the view until the user pans or zooms.
    pub fit: bool,
    /// Looking around inside the picture, for panoramas and cubemaps.
    pub inside: Option<crate::document::Panorama>,
    /// For data such as height maps: the lowest and highest value, which
    /// the picture is stretched between so its detail shows.
    pub data_range: Option<(f32, f32)>,
    /// A height map shown as a shaded 3D relief.
    pub relief: Option<Box<SceneView>>,
    /// The relief's height as a fraction of its width.
    pub relief_height: f32,
    /// For linear measurements (astronomy frames, film scans): the display
    /// stretch the picture is shown through.
    pub stretch: Option<StretchView>,
    /// The last target size, in physical pixels.
    size: (u32, u32),
}

impl TextureView {
    pub fn new(name: String, texture: Arc<Texture>, gfx: &mut Gfx) -> Result<TextureView, String> {
        // Data (16-bit greyscale PNGs, usually height maps) would show as a
        // nearly flat grey; a copy stretched from its lowest to its highest
        // value shows the detail, while reading values under the pointer
        // still uses the original.
        let data_range = data_range(&texture);
        let mut stretch = StretchView::for_texture(&texture);
        let gpu = match (&mut stretch, data_range) {
            (Some(s), _) => gfx.renderer.upload_texture(&gfx.gpu, &s.render(&texture))?,
            (None, Some(range)) => gfx.renderer.upload_texture(&gfx.gpu, &stretched(&texture, range))?,
            (None, None) => gfx.renderer.upload_texture(&gfx.gpu, &texture)?,
        };
        let mut params = TextureDrawParams::fit(&gpu, (1, 1));
        // Exposure for HDR starts where the thumbnail's automatic exposure
        // would put it, so a dim panorama doesn't open black.
        if gpu.is_hdr() {
            params.exposure = auto_exposure(&texture);
        }
        Ok(TextureView {
            id: next_id(),
            name,
            texture,
            gpu,
            params,
            fit: true,
            inside: None,
            data_range,
            relief: None,
            relief_height: 0.15,
            stretch,
            size: (1, 1),
        })
    }

    /// Shows the picture again through the stretch's current settings, when
    /// they changed since it was last drawn: the stretched copy is made
    /// on the CPU and uploaded in place of the old one.
    pub fn restretch(&mut self, gfx: &mut Gfx) -> Result<(), String> {
        let Some(s) = &mut self.stretch else { return Ok(()) };
        if s.is_current() {
            return Ok(());
        }
        let image = s.render(&self.texture);
        self.gpu = gfx.renderer.upload_texture(&gfx.gpu, &image)?;
        // A new number tells the frame cache the picture changed.
        self.id = next_id();
        Ok(())
    }

    /// Shows or hides the relief view of a height map.
    pub fn set_relief(&mut self, on: bool, gfx: &mut Gfx, aspect: f32) {
        self.relief = match (on, self.data_range) {
            (true, Some(range)) => {
                let scene = relief_scene(&self.texture, range, self.relief_height);
                // Terrain reads best from well above.
                let mut view = SceneView::with_pitch(scene, gfx, aspect, 45.0);
                if let Some(old) = self.relief.take() {
                    view.cameras = old.cameras;
                }
                Some(Box::new(view))
            }
            _ => None,
        };
    }

    /// Keeps the fit when the view changes size.
    pub fn set_size(&mut self, size: (u32, u32)) {
        if size != self.size {
            self.size = size;
            if self.fit {
                self.fit_now();
            }
        }
    }

    pub fn fit_now(&mut self) {
        let fitted = TextureDrawParams::fit(&self.gpu, self.size);
        self.params.center = fitted.center;
        self.params.zoom = fitted.zoom;
        self.fit = true;
    }

    /// One texel per screen pixel, centred on what is in the middle now.
    pub fn actual_size(&mut self) {
        self.params.zoom = 1.0;
        self.fit = false;
    }

    /// Pans by `delta` physical pixels.
    pub fn pan(&mut self, delta: egui::Vec2) {
        self.params.center[0] -= delta.x / self.params.zoom;
        self.params.center[1] -= delta.y / self.params.zoom;
        self.fit = false;
    }

    /// Zooms by `factor` about `at`, a point in physical pixels from the
    /// target's top left, so the texel under it stays put.
    pub fn zoom_about(&mut self, factor: f32, at: egui::Vec2) {
        let half = egui::vec2(self.size.0 as f32, self.size.1 as f32) * 0.5;
        let p = &mut self.params;
        let texel = [p.center[0] + (at.x - half.x) / p.zoom, p.center[1] + (at.y - half.y) / p.zoom];
        p.zoom = (p.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        p.center = [texel[0] - (at.x - half.x) / p.zoom, texel[1] - (at.y - half.y) / p.zoom];
        self.fit = false;
    }

    /// Zooms by `factor` about the middle of the view.
    pub fn zoom(&mut self, factor: f32) {
        let mid = egui::vec2(self.size.0 as f32, self.size.1 as f32) * 0.5;
        self.zoom_about(factor, mid);
    }

    pub fn draw(&self, gfx: &mut Gfx) {
        let key = crate::gfx::FrameKey::Texture { texture: self.id, params: self.params, size: self.size };
        gfx.draw_texture(key, &self.gpu, &self.params);
    }

    /// The texel under `at` (physical pixels from the target's top left)
    /// and its value, for the status bar.
    pub fn texel_at(&self, at: egui::Vec2) -> Option<(u32, u32, [f32; 4], bool)> {
        let half = egui::vec2(self.size.0 as f32, self.size.1 as f32) * 0.5;
        let p = &self.params;
        let x = p.center[0] + (at.x - half.x) / p.zoom;
        let y = p.center[1] + (at.y - half.y) / p.zoom;
        let (w, h) = (self.texture.width() as f32, self.texture.height() as f32);
        if x < 0.0 || y < 0.0 || x >= w || y >= h {
            return None;
        }
        // Read from the level shown, if the file stored it; generated mips
        // aren't kept on the CPU, so those read from the closest stored one.
        let mip = p.mip.unwrap_or(0).min(self.texture.levels.len() as u32 - 1) as usize;
        let level = &self.texture.levels[mip];
        let (lx, ly) = (
            ((x / w) * level.width as f32).min(level.width as f32 - 1.0) as usize,
            ((y / h) * level.height as f32).min(level.height as f32 - 1.0) as usize,
        );
        let faces = self.texture.faces.max(1);
        let layer_count = if self.texture.kind == TextureKind::D3 { 1 } else { self.texture.layers.max(1) };
        // A stretched picture shows one plane at a time, which the stretch
        // tracks, not the draw parameters.
        let layer = self.stretch.as_ref().map_or(p.layer, |s| s.layer);
        let surface = if self.texture.kind == TextureKind::D3 {
            0
        } else {
            (layer.min(layer_count - 1) * faces + p.face.min(faces - 1)) as usize
        };
        let pixels = level.surfaces.get(surface)?;
        let slice = if self.texture.kind == TextureKind::D3 { p.layer.min(level.depth.max(1) - 1) as usize } else { 0 };
        let index = ((slice * level.height as usize + ly) * level.width as usize + lx) * 4;
        let value = match pixels {
            Pixels::Rgba8(v) => {
                let px = v.get(index..index + 4)?;
                [px[0] as f32, px[1] as f32, px[2] as f32, px[3] as f32]
            }
            Pixels::Rgba32F(v) => {
                let px = v.get(index..index + 4)?;
                [px[0], px[1], px[2], px[3]]
            }
            // Texture mode shows files decoded to texels.
            Pixels::Blocks(..) => return None,
        };
        Some((x as u32, y as u32, value, pixels.is_hdr()))
    }

    /// Layers to pick from: array layers, or a 3D texture's slices.
    pub fn layer_count(&self) -> u32 {
        // The GPU holds the one plane of a stretched picture that is shown.
        if self.stretch.is_some() {
            1
        } else if self.texture.kind == TextureKind::D3 {
            self.texture.levels[0].depth.max(1)
        } else {
            self.texture.layers.max(1)
        }
    }

    /// An equirectangular panorama (twice as wide as tall) or a cubemap,
    /// which can be looked around in and lit with.
    pub fn is_panorama(&self) -> bool {
        let t = &self.texture;
        t.kind == TextureKind::Cube || (t.kind == TextureKind::D2 && t.width() == t.height() * 2)
    }
}

/*┌─────────────────╮
| Stretched data  |
└─────────────────╯*/
//region Stretched data

/// A picture of linear measurements, shown through a display stretch (see
/// `slerp_textures::stretch`).  The texture keeps the file's own values for
/// the status bar's readout; the GPU holds an 8-bit copy of the plane shown,
/// remade when the stretch or the plane changes.
pub struct StretchView {
    pub settings: Stretch,
    /// The plane shown, for files with several.
    pub layer: u32,
    pub layers: u32,
    /// The file format's name, under which the settings are remembered.
    pub container: String,
    /// Statistics of each plane, worked out when it is first shown.
    stats: Vec<Option<DataStats>>,
    /// What the GPU copy shows now.
    shown: Option<(Stretch, u32)>,
}

/// Whether the decoder asked for a display stretch.
pub fn is_stretched(t: &Texture) -> bool {
    t.source.format.ends_with("(stretch)") && matches!(t.base(), Pixels::Rgba32F(_))
}

impl StretchView {
    pub fn for_texture(t: &Texture) -> Option<StretchView> {
        if !is_stretched(t) {
            return None;
        }
        let layers = t.levels[0].surfaces.len() as u32;
        Some(StretchView {
            settings: remembered_stretch(&t.source.container),
            layer: 0,
            layers,
            container: t.source.container.clone(),
            stats: vec![None; layers as usize],
            shown: None,
        })
    }

    fn pixels<'a>(&self, t: &'a Texture) -> &'a [f32] {
        match t.levels[0].surfaces.get(self.layer as usize) {
            Some(Pixels::Rgba32F(v)) => v,
            _ => &[],
        }
    }

    /// Statistics of the plane shown.
    pub fn stats(&mut self, t: &Texture) -> DataStats {
        let layer = self.layer as usize;
        if self.stats[layer].is_none() {
            self.stats[layer] = Some(stretch::analyse(self.pixels(t)));
        }
        self.stats[layer].expect("just filled")
    }

    /// The black and white points in use, for each colour channel.
    pub fn points(&mut self, t: &Texture) -> [Points; 3] {
        let stats = self.stats(t);
        stretch::resolve(&stats, &self.settings)
    }

    /// The points the settings start from: none moved.
    pub fn default_points(&mut self, t: &Texture) -> [Points; 3] {
        let stats = self.stats(t);
        stretch::resolve(&stats, &self.settings.reset_points())
    }

    pub fn is_current(&self) -> bool {
        self.shown == Some((self.settings, self.layer))
    }

    /// The plane shown, stretched to 8-bit sRGB display colour.
    pub fn render(&mut self, t: &Texture) -> Texture {
        let points = self.points(t);
        let bytes = stretch::apply(self.pixels(t), &points);
        self.shown = Some((self.settings, self.layer));
        Texture::from_rgba8(t.width(), t.height(), bytes, ColorSpace::Srgb)
    }

    /// Takes over the settings and plane of the stretch shown before a
    /// reload, within what this picture has.
    pub fn carry_over(&mut self, old: &StretchView) {
        self.settings = old.settings;
        self.layer = old.layer.min(self.layers.saturating_sub(1));
    }
}

/// The stretch settings last used for each file format, shared by every
/// picture opened, and saved in `viewer.json`.
static REMEMBERED: std::sync::Mutex<Vec<(String, Stretch)>> = std::sync::Mutex::new(Vec::new());

/// The settings to open a picture of `format` with: the ones last used for
/// that format, including how far the black and white sliders were moved
/// (in slider units, relative to each file's own range).
pub fn remembered_stretch(format: &str) -> Stretch {
    let list = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    list.iter().find(|(f, _)| f == format).map(|(_, s)| *s).unwrap_or_default()
}

/// Keeps `settings` as the ones for `format`.
pub fn remember_stretch(format: &str, settings: &Stretch) {
    let mut list = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    match list.iter_mut().find(|(f, _)| f == format) {
        Some(entry) => entry.1 = *settings,
        None => list.push((format.to_string(), *settings)),
    }
}

//endregion

/*┌─────────────────╮
| Data and relief |
└─────────────────╯*/
//region Data and relief

/// Whether the decoder marked the texture as data rather than a picture.
pub fn is_data(t: &Texture) -> bool {
    t.source.format.ends_with("(data)")
}

/// For data textures, the range the picture is stretched over.
pub fn data_range(t: &Texture) -> Option<(f32, f32)> {
    if is_data(t) { value_range(t) } else { None }
}

/// The lowest and highest finite value of the first channel, or `None`
/// for a flat or empty image.
fn value_range(t: &Texture) -> Option<(f32, f32)> {
    let Pixels::Rgba32F(v) = t.base() else { return None };
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for p in v.chunks_exact(4).filter(|p| p[0].is_finite()) {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    (hi > lo).then_some((lo, hi))
}

/// A displayable grey copy of the first surface, stretched so the range
/// fills black to white.  The bytes are display values, as in the
/// thumbnail, so it is marked sRGB.
fn stretched(t: &Texture, (lo, hi): (f32, f32)) -> Texture {
    let Pixels::Rgba32F(v) = t.base() else { return t.clone() };
    let scale = 1.0 / (hi - lo);
    let pixels = v
        .chunks_exact(4)
        .flat_map(|p| {
            let g = if p[0].is_finite() { ((p[0] - lo) * scale).clamp(0.0, 1.0) } else { 0.0 };
            let g = (g * 255.0 + 0.5) as u8;
            [g, g, g, (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
        })
        .collect();
    Texture::from_rgba8(t.width(), t.height(), pixels, ColorSpace::Srgb)
}

/// Most vertices along a relief's longer side.  512 is about half a
/// million triangles, which every GPU draws without effort.
const RELIEF_DETAIL: u32 = 512;

/// A height map as terrain: a grid one metre across in the image's
/// proportions, raised by its stretched values times `height` metres and
/// coloured by height from low blue through green and brown to white.
pub fn relief_scene(t: &Texture, (lo, hi): (f32, f32), height: f32) -> Scene {
    let (w, h) = (t.width().max(1), t.height().max(1));
    let Pixels::Rgba32F(v) = t.base() else { return Scene::default() };
    let detail = RELIEF_DETAIL.min(w.max(h).saturating_sub(1)).max(2);
    let mut prim = Shape::Plane.primitive(detail);
    // The plane is a unit square in x and z; squash it to the picture's
    // proportions.
    let (sx, sz) = if w >= h { (1.0, h as f32 / w as f32) } else { (w as f32 / h as f32, 1.0) };
    let scale = 1.0 / (hi - lo);
    let sample = |u: f32, vv: f32| {
        let x = ((u * (w - 1) as f32).round() as usize).min(w as usize - 1);
        let y = ((vv * (h - 1) as f32).round() as usize).min(h as usize - 1);
        let p = v[(y * w as usize + x) * 4];
        if p.is_finite() { ((p - lo) * scale).clamp(0.0, 1.0) } else { 0.0 }
    };
    let mut colors = Vec::with_capacity(prim.positions.len());
    for (pos, uv) in prim.positions.iter_mut().zip(&prim.uv0) {
        let k = sample(uv.x, uv.y);
        pos.x *= sx;
        pos.z *= sz;
        pos.y = k * height;
        colors.push(terrain_color(k));
    }
    prim.colors = colors;
    // Normals and tangents follow the new surface.
    prim.normals.clear();
    prim.tangents.clear();
    prim.ensure_normals();
    prim.material = Some(0);
    let mut scene = Scene {
        name: "relief".into(),
        materials: vec![Material {
            name: "terrain".into(),
            base_color: glam::Vec4::ONE,
            roughness: 0.85,
            ..Default::default()
        }],
        ..Default::default()
    };
    scene.add_mesh_node(Mesh { name: "relief".into(), primitives: vec![prim], target_names: vec![] });
    scene
}

/// A hypsometric tint: linear RGBA for a height from 0 to 1.
fn terrain_color(k: f32) -> glam::Vec4 {
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.02, 0.07, 0.25]),
        (0.2, [0.05, 0.25, 0.35]),
        (0.35, [0.1, 0.35, 0.08]),
        (0.6, [0.35, 0.3, 0.1]),
        (0.8, [0.3, 0.2, 0.13]),
        (1.0, [0.9, 0.9, 0.92]),
    ];
    let k = k.clamp(0.0, 1.0);
    let i = STOPS.iter().position(|s| s.0 >= k).unwrap_or(STOPS.len() - 1).max(1);
    let (a, b) = (STOPS[i - 1], STOPS[i]);
    let t = ((k - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    let c = |j: usize| a.1[j] + (b.1[j] - a.1[j]) * t;
    glam::Vec4::new(c(0), c(1), c(2), 1.0)
}

//endregion

/// An exposure that puts the image's average brightness at middle grey,
/// as the texture thumbnails do.
fn auto_exposure(tex: &Texture) -> f32 {
    let Pixels::Rgba32F(v) = tex.base() else { return 0.0 };
    let step = (v.len() / 4 / 65536).max(1);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for px in v.chunks_exact(4).step_by(step) {
        let l = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
        if l.is_finite() && l > 0.0 {
            sum += (l as f64).ln();
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let mean = (sum / n as f64).exp() as f32;
    (0.18 / mean).log2().clamp(-10.0, 10.0)
}

/*┌─────────────────╮
| Gallery         |
└─────────────────╯*/
//region Gallery

pub struct GalleryView {
    pub items: Vec<NamedImage>,
    /// Small pictures made while loading, uploaded to egui on first show.
    thumbs: Vec<Option<Image8>>,
    handles: Vec<Option<egui::TextureHandle>>,
    /// The picture opened from the grid, if any.
    pub open: Option<(usize, TextureView)>,
    pub filter: String,
    /// Edge of a grid cell in points.
    pub cell: f32,
}

impl GalleryView {
    pub fn new(items: Vec<NamedImage>, thumbs: Vec<Option<Image8>>) -> GalleryView {
        let n = items.len();
        GalleryView {
            items,
            thumbs,
            handles: (0..n).map(|_| None).collect(),
            open: None,
            filter: String::new(),
            cell: 128.0,
        }
    }

    /// The egui texture for item `i`'s thumbnail.
    pub fn thumb(&mut self, ctx: &egui::Context, i: usize) -> Option<egui::TextureHandle> {
        if self.handles.get(i)?.is_none() {
            let img = self.thumbs.get_mut(i)?.take()?;
            let color =
                egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
            // Game textures are pixel art more often than not: hard edges
            // when magnified.
            let options = egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                minification: egui::TextureFilter::Linear,
                ..Default::default()
            };
            self.handles[i] = Some(ctx.load_texture(format!("gallery-{i}"), color, options));
        }
        self.handles[i].clone()
    }

    pub fn open_item(&mut self, i: usize, gfx: &mut Gfx) -> Result<(), String> {
        let item = self.items.get(i).ok_or("no such picture")?;
        let view = TextureView::new(item.name.clone(), item.texture.clone(), gfx)?;
        self.open = Some((i, view));
        Ok(())
    }

    /// Steps the open picture to the previous or next one.
    pub fn step(&mut self, delta: isize, gfx: &mut Gfx) -> Result<(), String> {
        let Some((i, _)) = &self.open else { return Ok(()) };
        let n = self.items.len() as isize;
        if n == 0 {
            return Ok(());
        }
        let next = (*i as isize + delta).rem_euclid(n) as usize;
        self.open_item(next, gfx)
    }
}

/// Makes the gallery's thumbnails, on the loading thread.
pub fn gallery_thumbs(items: &[NamedImage], size: u32) -> Vec<Option<Image8>> {
    items.iter().map(|item| Some(slerp_scene::textures::thumbnail(&item.texture, size))).collect()
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_stretches_and_raises() {
        let mut tex = Texture::from_rgba32f(
            2,
            2,
            vec![0.25, 0.25, 0.25, 1.0, 0.5, 0.5, 0.5, 1.0, 0.75, 0.75, 0.75, 1.0, 0.25, 0.25, 0.25, 1.0],
        );
        tex.source.format = "Grayscale 16-bit (data)".into();
        assert!(is_data(&tex));
        let range = value_range(&tex).unwrap();
        assert_eq!(range, (0.25, 0.75));
        let shown = stretched(&tex, range);
        let Pixels::Rgba8(px) = shown.base() else { panic!() };
        assert_eq!([px[0], px[4], px[8]], [0, 128, 255]);
        let scene = relief_scene(&tex, range, 0.2);
        let prim = &scene.meshes[0].primitives[0];
        let top = prim.positions.iter().map(|p| p.y).fold(0.0, f32::max);
        assert!((top - 0.2).abs() < 1e-6);
        assert_eq!(prim.normals.len(), prim.positions.len());
        assert_eq!(prim.colors.len(), prim.positions.len());
        // A flat image has no range to stretch.
        let flat = Texture::from_rgba32f(1, 1, vec![0.5, 0.5, 0.5, 1.0]);
        assert_eq!(value_range(&flat), None);
    }

    #[test]
    fn exposure_maps_mean_to_grey() {
        let tex = Texture::from_rgba32f(2, 1, vec![0.72, 0.72, 0.72, 1.0, 0.72, 0.72, 0.72, 1.0]);
        assert!((auto_exposure(&tex) - (-2.0)).abs() < 1e-4);
        let ldr = Texture::from_rgba8(1, 1, vec![1, 2, 3, 255], slerp_scene::ColorSpace::Srgb);
        assert_eq!(auto_exposure(&ldr), 0.0);
    }
}
