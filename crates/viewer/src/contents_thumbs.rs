//! The small pictures beside the Contents panel's rows, made lazily for
//! the rows in view, a few at a time on a thread, and uploaded as they
//! arrive.  Pictures, mips, layers and frames are shrunk from their pixels
//! (`Contents::thumbnail`).  Meshes, nodes and collections are drawn on
//! the GPU: the window's thread copies each one's geometry out of the
//! scene on screen (`Contents::piece_scene`, a few milliseconds' worth per
//! job), and the job draws it with a thumbnail renderer of its own on the
//! viewer's GPU, kept for later jobs.  A file with very many meshes gets no
//! drawn pictures at all, so scrolling through a CAD assembly's thousands
//! of parts costs nothing.
//!
//! A child of `contents_view`, so it reaches the panel's state.

use super::{State, loader};
use slerp_core::contents::ItemId;
use slerp_core::contents::ItemKind;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The size, in pixels, the row pictures are made at.
const THUMB_PIXELS: u32 = 48;
/// Pictures made per job.
const THUMBS_PER_JOB: usize = 16;
/// Scenes with more meshes or nodes than these get no drawn row pictures.
const MAX_DRAWN_MESHES: usize = 2_000;
const MAX_DRAWN_NODES: usize = 20_000;
/// How long the window's thread may spend copying pieces for one job;
/// the rest wait for the next.
const COPY_BUDGET: Duration = Duration::from_millis(4);

/// An item's row picture as a thread made it, `None` when it has none.
type RowPicture = (ItemId, Option<slerp_scene::Image8>);

/// What a job is asked to make: an item's picture from the contents, or a
/// drawing of a piece's own scene.
enum Wanted {
    Picture(ItemId),
    Drawn(ItemId, Box<slerp_scene::Scene>),
}

#[derive(Default)]
pub(super) struct Thumbs {
    ready: HashMap<ItemId, Option<(egui::TextureHandle, egui::Vec2)>>,
    /// The job making pictures, and the items it was asked for, which get
    /// none if it fails (else they would be asked for again every frame).
    job: Option<(Vec<ItemId>, loader::Task<Vec<RowPicture>>)>,
    /// A clear square that holds a row's place while its picture is made,
    /// so labels don't jump when it arrives.
    placeholder: Option<egui::TextureHandle>,
    /// The renderer jobs draw pieces with, made by the first job that
    /// needs it.
    renderer: Arc<Mutex<Option<slerp_core::ThumbRenderer>>>,
}

impl Thumbs {
    /// Forgets the pictures, whose item numbers a new listing changes.
    /// The renderer stays.
    pub(super) fn clear(&mut self) {
        self.ready.clear();
        self.job = None;
    }

    /// Makes the placeholder square, once.
    pub(super) fn ensure_placeholder(&mut self, ctx: &egui::Context) {
        if self.placeholder.is_none() {
            let clear = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 0]);
            self.placeholder =
                Some(ctx.load_texture("contents-thumb-placeholder", clear, egui::TextureOptions::LINEAR));
        }
    }
}

/// Whether an item's row gets a small picture.
pub(super) fn has_thumb(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Picture
            | ItemKind::Flat
            | ItemKind::Sprite
            | ItemKind::Texture
            | ItemKind::Palette
            | ItemKind::Mip
            | ItemKind::Face
            | ItemKind::Layer
            | ItemKind::Frame
            | ItemKind::Tag
            | ItemKind::Mesh
            | ItemKind::Node
            | ItemKind::Collection
    )
}

/// Starts making the pictures of rows in view that have none yet.  `doc`
/// is the window's document, whose scene drawn pieces are copied from
/// when it is the file listed (else the file's own document waiting aside
/// is used); `gpu` is the GPU to draw them on.
pub(super) fn want(
    state: &mut State,
    doc: Option<&crate::document::Document>,
    gpu: &Arc<slerp_render::Gpu>,
    waker: &loader::Waker,
    visible: std::ops::Range<usize>,
) {
    if state.thumbs.job.is_some() {
        return;
    }
    let ids: Vec<ItemId> = state.rows[visible.start.min(state.rows.len())..visible.end.min(state.rows.len())]
        .iter()
        .filter_map(|&n| state.tree.nodes[n].item)
        .filter(|id| !state.thumbs.ready.contains_key(id))
        .filter(|&id| state.contents.item(id).is_some_and(|i| has_thumb(i.kind)))
        .take(THUMBS_PER_JOB)
        .collect();
    if ids.is_empty() {
        return;
    }
    // 1. The scene on screen, for drawn pieces: the file's own document,
    //    or the one set aside while a texture is previewed.
    let own = doc.filter(|d| state.is_own(d)).or(state.parked.as_ref()).and_then(|d| d.scene());
    let drawable = own.filter(|v| v.scene.meshes.len() <= MAX_DRAWN_MESHES && v.scene.nodes.len() <= MAX_DRAWN_NODES);
    let start = Instant::now();
    let mut wanted = Vec::with_capacity(ids.len());
    for id in ids {
        if !state.contents.is_scene_piece(id) {
            wanted.push(Wanted::Picture(id));
            continue;
        }
        match (own, drawable) {
            // No scene to copy from just now: try again later.
            (None, _) => continue,
            // Too many meshes to draw them all.
            (Some(_), None) => {
                state.thumbs.ready.insert(id, None);
            }
            (_, Some(view)) => {
                if start.elapsed() > COPY_BUDGET {
                    break;
                }
                match state.contents.piece_scene(id, &view.scene, &view.initial_visible) {
                    Some(scene) => wanted.push(Wanted::Drawn(id, Box::new(scene))),
                    None => {
                        state.thumbs.ready.insert(id, None);
                    }
                }
            }
        }
    }
    if wanted.is_empty() {
        return;
    }
    // 2. The job: pictures from the contents, drawings with the renderer,
    //    which the first drawing makes.
    let asked: Vec<ItemId> = wanted
        .iter()
        .map(|w| match w {
            Wanted::Picture(id) | Wanted::Drawn(id, _) => *id,
        })
        .collect();
    let contents = state.contents.clone();
    let renderer = state.thumbs.renderer.clone();
    let gpu = gpu.clone();
    let task = loader::spawn(waker, "slerp-thumbs", move || {
        let mut out = Vec::with_capacity(wanted.len());
        for w in wanted {
            match w {
                Wanted::Picture(id) => out.push((id, contents.thumbnail(id, THUMB_PIXELS))),
                Wanted::Drawn(id, scene) => {
                    // A renderer that panicked earlier draws nothing more.
                    let Ok(mut slot) = renderer.lock() else {
                        out.push((id, None));
                        continue;
                    };
                    let r = slot.get_or_insert_with(|| slerp_core::ThumbRenderer::with_gpu(gpu.clone()));
                    out.push((id, contents.draw_piece(*scene, r, THUMB_PIXELS)));
                }
            }
        }
        Ok(out)
    });
    state.thumbs.job = Some((asked, task));
}

/// Uploads the row pictures a job finished.
pub(super) fn poll(state: &mut State, ctx: &egui::Context) {
    let Some((_, rx)) = &state.thumbs.job else { return };
    let Some(result) = loader::poll(rx) else { return };
    let Some((asked, _)) = state.thumbs.job.take() else { return };
    for id in asked {
        state.thumbs.ready.insert(id, None);
    }
    for (id, img) in result.unwrap_or_default() {
        let handle = img.filter(|i| i.width > 0 && i.height > 0).map(|img| {
            let size = egui::vec2(img.width as f32, img.height as f32);
            let color =
                egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
            (ctx.load_texture(format!("contents-thumb-{}", id.0), color, egui::TextureOptions::LINEAR), size)
        });
        state.thumbs.ready.insert(id, handle);
    }
}

/// The picture a row shows: its own, or a clear square while it is made.
pub(super) fn row(state: &State, id: ItemId) -> Option<(egui::TextureId, egui::Vec2)> {
    match state.thumbs.ready.get(&id) {
        Some(Some((handle, size))) => Some((handle.id(), *size)),
        Some(None) => None,
        None => state.thumbs.placeholder.as_ref().map(|p| (p.id(), egui::vec2(1.0, 1.0))),
    }
}
