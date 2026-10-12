//! An open file and how it is shown: a 3D scene, a material on a shape, a
//! texture, or a gallery of named pictures.

use crate::cad_view::CadView;
use crate::canvas_view::CanvasView;
use crate::dicom_view::DicomView;
use crate::gfx::Gfx;
use crate::loader::{Loaded, Waker};
use crate::lut_view::LutView;
use crate::pages_view::PagesView;
use crate::palette_view::PaletteView;
use crate::scene_view::SceneView;
use crate::texture_view::{GalleryView, TextureView, data_range};
use crate::volume_view::VolumeView;
use slerp_core::MaterialState;
use slerp_render::GpuScene;
use slerp_scene::{Asset, FlyCamera, Image8, MaterialAsset, Scene, Texture};
use std::path::PathBuf;
use std::time::Duration;

pub struct Document {
    pub path: PathBuf,
    pub file_bytes: u64,
    pub load_time: Duration,
    pub upload_time: Duration,
    pub parts: Vec<(String, u64)>,
    /// Facts from the loader: format, generator, extensions and so on.
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
    /// Files beside this one that the loader read, which watch mode
    /// watches too.
    pub neighbours: Vec<PathBuf>,
    /// The file's parts when it holds several shown one at a time (a Doom
    /// WAD's maps), and the one shown; `None` when the whole file is.
    pub part_names: Vec<String>,
    pub part: Option<String>,
    /// A picture the file's app saved in it (a 3ds Max or Procreate
    /// file's preview), shown because Slerp can't read the file itself.
    pub stored_preview: bool,
    /// A vector metafile (WMF, EMF, PICT), whose picture is a replay of its
    /// drawing records onto a canvas rather than stored pixels.
    pub replayed: bool,
    /// The format the loader recognised.
    pub kind: slerp_core::Kind,
    /// The listing of an archive file, which the Contents panel shows.
    pub contents: Option<std::sync::Arc<slerp_core::contents::Contents>>,
    /// Set when this document is an entry of an archive, not a file.
    pub origin: Option<crate::app::contents_view::Origin>,
    pub content: Content,
}

pub enum Content {
    Scene(Box<SceneView>),
    Material(Box<MaterialView>),
    Texture(Box<TextureView>),
    Gallery(Box<GalleryView>),
    /// An Aseprite sprite or a Tiled map.
    Canvas(Box<CanvasView>),
    /// A colour lookup table.
    Lut(Box<LutView>),
    /// A colour palette.
    Palette(Box<PaletteView>),
    /// A DICOM medical image, which re-windows as it is dragged.
    Dicom(Box<DicomView>),
    /// A flat CAD drawing, drawn again at every zoom.
    Cad(Box<CadView>),
    /// A scientific array, medical volume or microscope stack, one plane at
    /// a time.
    Volume(Box<VolumeView>),
    /// A PDF's pages, drawn again at every zoom.
    Pages(Box<PagesView>),
}

impl Document {
    /// Uploads a loaded file to the GPU.  `aspect` is the view's, for the
    /// first camera framing.
    pub fn new(loaded: Loaded, gfx: &mut Gfx, aspect: f32, waker: &Waker) -> Result<Document, String> {
        let start = std::time::Instant::now();
        let mut facts = Vec::new();
        let mut warnings = Vec::new();
        let mut neighbours = Vec::new();
        let name = loaded.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut info = |i: &slerp_scene::Info| {
            if !i.format.is_empty() {
                facts.push(("Format".to_string(), i.format.clone()));
            }
            if let Some(g) = &i.generator {
                facts.push(("Made with".to_string(), g.clone()));
            }
            facts.extend(i.details.iter().cloned());
            warnings.extend(i.warnings.iter().cloned());
            neighbours.extend(i.neighbours.iter().cloned());
        };
        // A Rhino file without render meshes shows the picture it stores.
        // An EPS shows the preview it stores unless Ghostscript drew it.
        let eps_preview = match (&loaded.kind, &loaded.asset) {
            (slerp_core::Kind::Document(_), Asset::Image(t)) => {
                t.source.header.iter().any(|(k, v)| k == "Shown" && v.starts_with("The preview picture"))
            }
            _ => false,
        };
        // Which of the file's pictures a stored preview is (a thumbnail, a
        // merged canvas, a layer), when the reader says.
        let own_note = match &loaded.asset {
            Asset::Image(t) => t.source.header.iter().find(|(k, _)| k == "Shown").map(|(_, v)| v.clone()),
            _ => None,
        };
        let stored_preview = matches!(loaded.kind, slerp_core::Kind::Preview(_))
            || (matches!(loaded.kind, slerp_core::Kind::Cad(_)) && matches!(loaded.asset, Asset::Image(_)))
            || eps_preview;
        let mut palette_facts = (Vec::new(), Vec::new());
        let content = match loaded.asset {
            Asset::Image(first) if loaded.pages.is_some() => {
                let opened = loaded.pages.expect("checked above");
                let view = PagesView::new(name, *opened, first, gfx, waker.clone())?;
                let (f, w) = view.facts();
                facts.extend(f);
                warnings.extend(w);
                Content::Pages(Box::new(view))
            }
            Asset::Image(picture) if loaded.volume.is_some() => {
                let opened = loaded.volume.expect("checked above");
                let view = VolumeView::new(name, picture, *opened, gfx, waker)?;
                facts.extend(view.volume.facts.iter().cloned());
                warnings.extend(view.volume.warnings.iter().cloned());
                Content::Volume(Box::new(view))
            }
            Asset::Image(picture) if loaded.dicom.is_some() => {
                let opened = loaded.dicom.expect("checked above");
                let view = DicomView::new(name, picture, *opened, gfx, waker)?;
                facts.extend(view.dicom.facts());
                warnings.extend(view.dicom.parsed.warnings.iter().cloned());
                Content::Dicom(Box::new(view))
            }
            Asset::Canvas(canvas) if loaded.cad.is_some() => {
                info(canvas.info());
                let drawing = loaded.cad.expect("checked above");
                let first = loaded.first_frame.unwrap_or_else(|| crate::cad_view::first_picture(&drawing));
                Content::Cad(Box::new(CadView::new(name, drawing, first, gfx, waker.clone())?))
            }
            _ if loaded.palette.is_some() => {
                let palette = loaded.palette.expect("checked above");
                palette_facts = (palette.facts(), palette.warnings.clone());
                Content::Palette(Box::new(PaletteView::new(palette)))
            }
            Asset::Scene(scene) => {
                info(&scene.info);
                // The first frame shows the model while its textures go up.
                let mut view = SceneView::streaming(scene, loaded.geometry, gfx, aspect);
                view.dna = loaded.dna.map(|c| Box::new(crate::dna::DnaState::new(c)));
                view.paths = loaded.paths;
                warnings.extend(view.gpu.warnings.iter().cloned());
                Content::Scene(Box::new(view))
            }
            Asset::Material(asset) => {
                info(&asset.info);
                Content::Material(Box::new(MaterialView::new(asset, gfx, aspect)))
            }
            Asset::Image(texture) => {
                texture_facts(&texture, &mut facts);
                // An EPS or PostScript file's facts: what is shown, the
                // bounding box, the creator.
                if let slerp_core::Kind::Document(_) = loaded.kind {
                    facts.splice(0..0, texture.source.header.iter().cloned());
                }
                Content::Texture(Box::new(TextureView::new(name, texture, gfx)?))
            }
            Asset::Gallery(items) => {
                facts.push(("Pictures".into(), items.len().to_string()));
                Content::Gallery(Box::new(GalleryView::new(items, loaded.thumbs)))
            }
            Asset::Canvas(canvas) => {
                info(canvas.info());
                let first = match loaded.first_frame {
                    Some(f) => f,
                    None => {
                        let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
                        canvas.compose(0, &shown, crate::canvas_view::viewer_scale(&*canvas))
                    }
                };
                Content::Canvas(Box::new(CanvasView::new(name, canvas, first, gfx, waker.clone())?))
            }
            Asset::Lut(lut) => {
                info(lut.info());
                Content::Lut(Box::new(LutView::new(lut)))
            }
        };
        facts.extend(palette_facts.0);
        warnings.extend(palette_facts.1);
        if stored_preview && !eps_preview {
            let note = own_note.unwrap_or_else(|| "The preview picture the app saved in the file".into());
            facts.insert(0, ("Shown".into(), note));
        }
        // Thumbnails of a file that carries a picture of itself show that
        // picture; the view shows the content.
        if let Some((w, h)) = loaded.saved_picture.filter(|_| !stored_preview && !eps_preview) {
            // Beside the format, where it is seen without scrolling.
            let at = facts.iter().position(|(k, _)| k == "Format").map_or(0, |i| i + 1);
            facts.insert(at, ("Thumbnail".into(), format!("The picture saved in the file ({w} × {h})")));
        }
        neighbours.extend(loaded.neighbours.iter().cloned());
        let replayed = matches!(loaded.kind, slerp_core::Kind::Metafile(_));
        if replayed {
            let note = "A replay of the file's drawing records onto a canvas, drawn at twice the file's own size (384 to 3072 pixels across), so lines stay sharp when zoomed; the fonts are this machine's, not necessarily the file's";
            facts.insert(0, ("Shown".into(), note.into()));
        }
        Ok(Document {
            path: loaded.path,
            file_bytes: loaded.file_bytes,
            load_time: loaded.elapsed,
            upload_time: start.elapsed(),
            parts: loaded.parts,
            facts,
            warnings,
            neighbours,
            part_names: loaded.part_names,
            part: loaded.part,
            stored_preview,
            replayed,
            kind: loaded.kind,
            contents: loaded.contents,
            origin: None,
            content,
        })
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The 3D scene shown: a scene, a material's shape, or a height map's
    /// relief.
    pub fn scene(&self) -> Option<&SceneView> {
        match &self.content {
            Content::Scene(s) => Some(s),
            Content::Material(m) => Some(&m.view),
            Content::Texture(t) => t.relief.as_deref(),
            Content::Gallery(_)
            | Content::Canvas(_)
            | Content::Lut(_)
            | Content::Palette(_)
            | Content::Dicom(_)
            | Content::Volume(_)
            | Content::Pages(_)
            | Content::Cad(_) => None,
        }
    }

    pub fn scene_mut(&mut self) -> Option<&mut SceneView> {
        match &mut self.content {
            Content::Scene(s) => Some(s),
            Content::Material(m) => Some(&mut m.view),
            Content::Texture(t) => t.relief.as_deref_mut(),
            Content::Gallery(_)
            | Content::Canvas(_)
            | Content::Lut(_)
            | Content::Palette(_)
            | Content::Dicom(_)
            | Content::Volume(_)
            | Content::Pages(_)
            | Content::Cad(_) => None,
        }
    }

    /// The sprite or map shown.
    pub fn canvas_mut(&mut self) -> Option<&mut CanvasView> {
        match &mut self.content {
            Content::Canvas(c) => Some(c),
            _ => None,
        }
    }

    /// The texture shown, alone or opened from a gallery.
    pub fn texture_mut(&mut self) -> Option<&mut TextureView> {
        match &mut self.content {
            Content::Texture(t) => Some(t),
            Content::Gallery(g) => g.open.as_mut().map(|(_, t)| t),
            Content::Canvas(c) => Some(&mut c.texture),
            Content::Dicom(d) => Some(&mut d.texture),
            Content::Volume(v) if v.has_picture() => Some(&mut v.texture),
            _ => None,
        }
    }

    pub fn texture(&self) -> Option<&TextureView> {
        match &self.content {
            Content::Texture(t) => Some(t),
            Content::Gallery(g) => g.open.as_ref().map(|(_, t)| t),
            Content::Canvas(c) => Some(&c.texture),
            Content::Dicom(d) => Some(&d.texture),
            Content::Volume(v) if v.has_picture() => Some(&v.texture),
            _ => None,
        }
    }

    /// What the mode is called in the status bar and title.
    pub fn mode_name(&self) -> &'static str {
        match &self.content {
            Content::Scene(s) if s.flying() => "First person",
            Content::Scene(_) => "Orbit",
            Content::Material(_) => "Material",
            Content::Texture(t) if t.relief.is_some() => "Relief",
            Content::Texture(t) if t.inside.is_some() => "Panorama",
            Content::Texture(_) if self.stored_preview => "Stored preview",
            Content::Texture(_) if self.replayed => "Vector drawing",
            Content::Texture(_) => "Texture",
            Content::Gallery(g) if g.open.is_some() => "Gallery picture",
            Content::Gallery(_) => "Gallery",
            Content::Canvas(c) if c.is_map() => "Map",
            Content::Canvas(_) if matches!(self.kind, slerp_core::Kind::Image(_)) => "Animation",
            Content::Canvas(_) if matches!(self.kind, slerp_core::Kind::Paint(_)) => "Layers",
            Content::Canvas(_) => "Sprite",
            Content::Lut(_) => "Colour table",
            Content::Palette(_) => "Palette",
            Content::Dicom(d) if d.playing => "Cine",
            Content::Dicom(_) => "DICOM",
            Content::Volume(v) if !v.has_picture() => "Array",
            Content::Volume(v) if v.playing => "Playing",
            Content::Volume(_) => "Volume",
            Content::Cad(_) => "Drawing",
            Content::Pages(_) if matches!(self.kind, slerp_core::Kind::Document(_)) => "Document",
            Content::Pages(_) => "Drawing",
        }
    }
}

/// Facts about a texture for the inspector.
pub fn texture_facts(t: &Texture, facts: &mut Vec<(String, String)>) {
    let s = &t.source;
    if !s.container.is_empty() {
        facts.push(("Container".into(), s.container.clone()));
    }
    if !s.format.is_empty() {
        facts.push(("Pixel format".into(), s.format.clone()));
    }
    if let Some(c) = &s.supercompression {
        facts.push(("Supercompression".into(), c.clone()));
    }
    let depth = if s.depth > 1 { format!(" × {}", s.depth) } else { String::new() };
    facts.push(("Size".into(), format!("{} × {}{depth}", s.width.max(t.width()), s.height.max(t.height()))));
    if let Some(range) = data_range(t) {
        facts.push(("Values".into(), format!("{:.4} to {:.4}, shown stretched", range.0, range.1)));
    }
    if s.layers > 1 {
        facts.push(("Layers".into(), s.layers.to_string()));
    }
    if s.faces > 1 {
        facts.push(("Faces".into(), s.faces.to_string()));
    }
    facts.push(("Mip levels".into(), format!("{} stored, {} decoded", s.mip_count.max(1), t.levels.len())));
    facts.push(("Decoded as".into(), if t.is_hdr() { "32-bit float RGBA".into() } else { "8-bit RGBA".into() }));
    if crate::texture_view::is_stretched(t) {
        facts.push(("Shown as".into(), "stretched; the readout is in the file's own units".into()));
    }
    facts.push(("Has alpha".into(), if t.has_alpha { "yes".into() } else { "no".into() }));
    // What was done to a photograph to show it right (turned upright, colour
    // converted to sRGB) is worth seeing without opening the header list.
    for (k, v) in &s.header {
        if matches!(k.as_str(), "Orientation" | "Rotation" | "Mirror" | "Cropped" | "Colour profile" | "Colour") {
            facts.push((k.clone(), v.clone()));
        }
    }
}

/*┌─────────────────╮
| Material mode   |
└─────────────────╯*/
//region Material mode

/// A material file shown on a shape whose kind, tiling and displacement
/// the user picks.
pub struct MaterialView {
    pub asset: MaterialAsset,
    pub state: MaterialState,
    pub view: SceneView,
    /// The file's own preview picture, uploaded to egui on first show.
    preview: Option<Image8>,
    preview_handle: Option<egui::TextureHandle>,
}

impl MaterialView {
    pub fn new(asset: MaterialAsset, gfx: &mut Gfx, aspect: f32) -> MaterialView {
        let state = MaterialState::default();
        let view = SceneView::new(slerp_core::material_scene(&asset, &state), gfx, aspect);
        let preview = asset.preview.as_ref().map(|p| slerp_scene::textures::thumbnail(p, 256));
        MaterialView { asset, state, view, preview, preview_handle: None }
    }

    /// Shows the state's tiling and displacement without rebuilding the
    /// shape: both live in the material's uniform on the GPU (the vertex
    /// shader displaces the shape by the height texture), so rewriting it
    /// takes microseconds and the sliders update every frame.  A new shape
    /// needs `rebuild`.
    pub fn retune(&mut self, gfx: &mut Gfx) {
        // A material ball's scene holds the file's material alone.
        let Some(m) = self.view.scene.materials.first_mut() else { return };
        *m = self.asset.material.clone();
        slerp_core::tune_material(m, &self.state);
        gfx.renderer.update_material(&gfx.gpu, &mut self.view.gpu, &self.view.scene, 0);
        // Displacement moves the vertices as a new pose would, and the
        // frame key notices poses, so the view draws again.
        self.view.pose_version += 1;
    }

    /// Rebuilds the shape after the state changed, keeping the camera.
    pub fn rebuild(&mut self, gfx: &mut Gfx, aspect: f32) {
        let scene = slerp_core::material_scene(&self.asset, &self.state);
        let mut view = SceneView::new(scene, gfx, aspect);
        std::mem::swap(&mut view.cameras, &mut self.view.cameras);
        view.cameras.set_bounds(view.gpu.bounds);
        view.selected_material = self.view.selected_material;
        self.view = view;
    }

    /// The file's preview picture as an egui texture.
    pub fn preview(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.preview_handle.is_none() {
            let img = self.preview.take()?;
            let color =
                egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
            self.preview_handle = Some(ctx.load_texture("material-preview", color, egui::TextureOptions::LINEAR));
        }
        self.preview_handle.clone()
    }

    pub fn displacement(&self) -> f32 {
        self.state
            .displacement
            .unwrap_or_else(|| self.asset.material.height_scale.clamp(0.0, slerp_core::DEFAULT_MAX_DISPLACEMENT))
    }
}

//endregion

/*┌─────────────────╮
| Panorama        |
└─────────────────╯*/
//region Panorama

/// Looking around inside an HDR panorama or cubemap: the picture becomes
/// the lighting environment and the background of an empty scene.
pub struct Panorama {
    pub id: u64,
    pub scene: GpuScene,
    pub camera: FlyCamera,
}

impl Panorama {
    pub fn new(gfx: &mut Gfx) -> Panorama {
        let scene = gfx.renderer.prepare(&gfx.gpu, &Scene::default());
        let camera = FlyCamera {
            position: glam::Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            fov_y: 75f32.to_radians(),
            near: 0.1,
            far: 10.0,
        };
        Panorama { id: crate::gfx::next_id(), scene, camera }
    }
}

//endregion
