//! What the viewer remembers between runs: recent lighting pictures and
//! animation libraries, the last folder a file was opened from, where the
//! panels are docked, where the view sat in the window and which panel
//! sections were showing, and the View menu's choices about reloading and
//! Esc.  Kept in `$XDG_STATE_HOME/slerp/viewer.json` (normally
//! `~/.local/state`), since it is history rather than settings; settings
//! live in `~/.config/slerp/config.jsonc`.  Windows and macOS use their own
//! folders (see `slerp_core::config::state_dir`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RECENT: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Saved {
    pub recent_environments: Vec<PathBuf>,
    pub recent_libraries: Vec<PathBuf>,
    pub last_folder: Option<PathBuf>,
    pub hide_panels: bool,
    /// Where the side panels were docked, as `egui_tiles` saves a layout
    /// (see `dock`).
    pub layout: Option<egui_tiles::Tree<crate::dock::Pane>>,
    /// The view's distances from the window's left, top, right and bottom
    /// edges, in points, when the last window closed: the next window's
    /// placeholder shows the view there (see `placeholder`).
    pub view_insets: Option<crate::placeholder::ViewInsets>,
    /// The lighting picture in use when the viewer closed.
    pub environment: Option<PathBuf>,
    /// Reload files that change on disk without asking.
    pub always_reload: bool,
    /// Esc closes the window once there is nothing else for it to leave.
    pub esc_closes: bool,
    /// Panel sections the user opened (`true`) or closed, by name.
    pub sections: BTreeMap<String, bool>,
    /// The display stretch last used for each linear-data format (FITS,
    /// XISF, ...), by format name.
    pub stretch: BTreeMap<String, SavedStretch>,
    /// The fill light last chosen for each format without light of its
    /// own (level-editor maps), by format name.
    pub fills: BTreeMap<String, f32>,
}

/// One format's remembered stretch: the curve, the background level and
/// how far the black and white sliders were moved from their automatic
/// places (in slider units, so they carry over to another file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedStretch {
    pub mode: String,
    pub linked: bool,
    pub target: f32,
    pub black_shift: f32,
    pub white_shift: f32,
}

impl Default for SavedStretch {
    fn default() -> Self {
        SavedStretch {
            mode: "Auto".into(),
            linked: false,
            target: slerp_scene::textures::stretch::DEFAULT_TARGET,
            black_shift: 0.0,
            white_shift: 0.0,
        }
    }
}

impl Saved {
    pub fn path() -> Option<PathBuf> {
        Some(slerp_core::config::state_dir()?.join("viewer.json"))
    }

    /// Reads the file; anything missing or broken gives the defaults.
    pub fn load() -> Saved {
        Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = slerp_core::write_atomic(&path, &json);
        }
    }

    pub fn add_environment(&mut self, path: &Path) {
        push_recent(&mut self.recent_environments, path);
    }

    pub fn add_library(&mut self, path: &Path) {
        push_recent(&mut self.recent_libraries, path);
    }
}

/// Moves `path` to the front of `list`, keeping it short.
fn push_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_lists_move_to_front_and_stay_short() {
        let mut s = Saved::default();
        for i in 0..10 {
            s.add_library(Path::new(&format!("/lib{i}.glb")));
        }
        s.add_library(Path::new("/lib5.glb"));
        assert_eq!(s.recent_libraries.len(), RECENT);
        assert_eq!(s.recent_libraries[0], PathBuf::from("/lib5.glb"));
        assert_eq!(s.recent_libraries.iter().filter(|p| p.ends_with("lib5.glb")).count(), 1);
        let json = serde_json::to_string(&s).unwrap();
        let back: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
