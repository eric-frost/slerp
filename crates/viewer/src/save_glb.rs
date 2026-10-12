//! File > Save as GLB…: writes the scene the viewer shows (with any clips
//! added from animation libraries) as a GLB file, on a thread of its own,
//! and says how it went in the status bar.
//!
//! For tests, `SLERP_SAVE_GLB=out.glb` saves the first scene the window
//! shows there, without the file dialog.

use super::Viewer;
use crate::document::{Content, Document};
use crate::loader::{Task, poll, spawn};
use slerp_core::gltf::{GlbOptions, Selection, write_glb};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// A save in progress: where to, and the thread writing it, which gives
/// back the writer's warnings.
pub struct Saving {
    path: PathBuf,
    started: Instant,
    task: Task<Vec<String>>,
}

/// True when the document is a 3D scene that can be saved: a model, level
/// or anything else that opens as a scene, but not a material's ball or a
/// height map's relief.
pub fn can_save(doc: Option<&Document>) -> bool {
    doc.is_some_and(|d| matches!(d.content, Content::Scene(_)))
}

/// The file dialog's title, filter and suggested name: the document's name
/// with `.glb`.
pub fn configure(dialog: rfd::FileDialog, current: Option<&Path>, folder: Option<PathBuf>) -> rfd::FileDialog {
    let stem = current.and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned());
    let mut dialog = dialog
        .set_title("Save as GLB")
        .add_filter("glTF binary", &["glb"])
        .set_file_name(format!("{}.glb", stem.unwrap_or_else(|| "scene".into())));
    if let Some(f) = folder {
        dialog = dialog.set_directory(f);
    }
    dialog
}

/// Starts writing the shown scene to `path`.  The scene is copied first
/// (pictures are shared, not copied), so the view goes on while it is
/// written.
pub fn start(v: &mut Viewer, path: PathBuf) {
    if v.glb.is_some() {
        v.warn("A GLB file is still being saved");
        return;
    }
    let Some(view) = v.doc.as_ref().filter(|d| can_save(Some(d))).and_then(|d| d.scene()) else {
        v.warn("Only 3D scenes can be saved as GLB");
        return;
    };
    let scene = view.scene.clone();
    let target = path.clone();
    let task = spawn(&v.waker, "slerp-save-glb", move || {
        let glb = write_glb(&scene, &Selection::Scene, &GlbOptions::default()).map_err(|e| e.to_string())?;
        slerp_core::write_atomic(&target, &glb.bytes).map_err(|e| e.to_string())?;
        Ok(glb.warnings)
    });
    v.glb = Some(Saving { path, started: Instant::now(), task });
    v.notify("Saving as GLB…");
}

/// Reports a finished save, and starts the `SLERP_SAVE_GLB` one once a
/// scene is shown.
pub fn poll_save(v: &mut Viewer) {
    static HOOK_USED: AtomicBool = AtomicBool::new(false);
    if can_save(v.doc.as_ref())
        && let Some(path) = crate::env::var("SLERP_SAVE_GLB")
        && !HOOK_USED.swap(true, Ordering::Relaxed)
    {
        start(v, PathBuf::from(path));
    }
    let Some(saving) = &v.glb else { return };
    let Some(result) = poll(&saving.task) else { return };
    let Saving { path, started, .. } = v.glb.take().expect("checked above");
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Saved over the file shown: its pictures' byte ranges now point into
    // a different file, so the scene is read again.
    if result.is_ok() && v.current.as_deref() == Some(path.as_path()) {
        v.reload();
    }
    match result {
        Ok(warnings) if warnings.is_empty() => {
            v.notify(format!("Saved {name} in {:.1} s", started.elapsed().as_secs_f32()));
        }
        Ok(warnings) => {
            let more =
                if warnings.len() > 1 { format!(" (and {} more notes)", warnings.len() - 1) } else { String::new() };
            v.notify(format!("Saved {name}; {}{more}", warnings[0]));
        }
        Err(e) => v.warn(format!("Couldn't save {name}: {e}")),
    }
}
