//! The Contents panel: what is inside the file shown, as a tree on the
//! left with a search box, the selected item shown in the main view, and
//! three ways to get items out as ordinary files: the Extract button,
//! Ctrl+C (then paste in a file manager) and [`materialize`], which the
//! drag-out code calls.
//!
//! Two kinds of file have contents.  A game archive (a Doom WAD, a PK3, a
//! Quake PAK, a Godot PCK, a Valve VPK, a Build GRP, a Quake or Half-Life
//! texture WAD) lists its entries: selecting one prepares it on a thread
//! (converting Doom pictures to PNG, drawing sounds as waveforms), writes it
//! to a private temporary file and opens that through the ordinary loader,
//! so every format the viewer knows works as a preview; a Doom map's folder
//! opens the map itself.  Any other file lists its pieces (a model's nodes,
//! meshes, materials, textures and clips, a texture's mips and faces, a
//! sprite's layers, frames and tags, a document's pages), and selecting one
//! shows it in the file that is already open: a node or mesh alone and
//! framed, a clip playing, a mip or face picked, a layer alone.  Only a
//! piece with a picture of its own (a model's texture) opens as a preview,
//! and the model waits aside meanwhile (`parked`), so going back costs
//! nothing.  Hovering a piece outlines it in the 3D view.
//!
//! Entries that show nothing (text, music, map data) show a card of facts
//! instead.  Reading is read-only everywhere; editing archives is left to
//! SLADE and VPKEdit, with buttons that appear only when one is installed.
//!
//! This module is a child of `app`, so it can reach the viewer's own
//! fields; `app.rs` only calls the functions below at the few places it
//! needs to.

use super::{Action, Document, Purpose, Viewer, card, heading2, loader, short_name};
#[path = "contents_thumbs.rs"]
mod thumbs;
use egui::Ui;
use slerp_core::contents::{
    Contents, ExtractOptions, Format, ItemId, ItemKey, ItemKind, PreviewKind, Selected, Show, TempFolder,
};
use slerp_theme::widgets::{self, TreeEvent, TreeRow};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/*┌─────────────────╮
| Tree model      |
└─────────────────╯*/
//region Tree model

/// One row's worth of tree: a folder, an item, or an item with items
/// beneath it (a TEXTURE1 lump and the composite textures it defines).
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    /// For a folder, its path; for an item, the item's path.
    pub path: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub item: Option<ItemId>,
    pub depth: usize,
    /// A Doom map's folder, which opens as the map.
    pub map: bool,
    /// The items at or beneath this node, and their bytes.
    pub count: usize,
    pub bytes: u64,
}

/// The archive's items as a tree of folders.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    /// The nodes with items beneath them or that stand for a folder, by path.
    by_path: HashMap<String, usize>,
}

impl Tree {
    /// Builds the tree from the items' paths.  An item whose name is also a
    /// folder's (TEXTURE1 and TEXTURE1/WALL) is one node with children; a
    /// name that repeats (Doom lumps override earlier ones) is one node per
    /// repeat, in order.
    pub fn build(contents: &Contents) -> Tree {
        let mut t = Tree::default();
        // The latest node of each (parent, name), for adopting.
        let mut named: HashMap<(Option<usize>, String), usize> = HashMap::new();
        for item in contents.items() {
            let parts: Vec<&str> = item.path.split('/').filter(|p| !p.is_empty()).collect();
            let parts = if parts.is_empty() { vec!["(unnamed)"] } else { parts };
            let mut parent = None;
            for part in &parts[..parts.len() - 1] {
                let key = (parent, part.to_string());
                let at = match named.get(&key) {
                    Some(&i) => i,
                    None => t.push(parent, part, None, &mut named),
                };
                parent = Some(at);
            }
            let leaf = parts[parts.len() - 1];
            let key = (parent, leaf.to_string());
            match named.get(&key) {
                // A folder made by deeper paths takes the item.
                Some(&i) if t.nodes[i].item.is_none() => t.nodes[i].item = Some(item.id),
                _ => {
                    t.push(parent, leaf, Some(item.id), &mut named);
                }
            }
        }
        // Totals, children before parents (nodes are made parents first).
        for i in (0..t.nodes.len()).rev() {
            let (own_count, own_bytes) = match t.nodes[i].item {
                Some(id) => (1, contents.item(id).map_or(0, |it| it.size)),
                None => (0, 0),
            };
            t.nodes[i].count += own_count;
            t.nodes[i].bytes += own_bytes;
            if let Some(p) = t.nodes[i].parent {
                let (c, b) = (t.nodes[i].count, t.nodes[i].bytes);
                t.nodes[p].count += c;
                t.nodes[p].bytes += b;
            }
        }
        for (folder, _) in contents.maps() {
            if let Some(&i) = t.by_path.get(&folder) {
                t.nodes[i].map = true;
            }
        }
        t
    }

    fn push(
        &mut self,
        parent: Option<usize>,
        name: &str,
        item: Option<ItemId>,
        named: &mut HashMap<(Option<usize>, String), usize>,
    ) -> usize {
        let i = self.nodes.len();
        let (depth, path) = match parent {
            Some(p) => (self.nodes[p].depth + 1, format!("{}/{name}", self.nodes[p].path)),
            None => (0, name.to_string()),
        };
        self.nodes.push(Node {
            name: name.to_string(),
            path: path.clone(),
            parent,
            children: Vec::new(),
            item,
            depth,
            map: false,
            count: 0,
            bytes: 0,
        });
        match parent {
            Some(p) => self.nodes[p].children.push(i),
            None => self.roots.push(i),
        }
        named.insert((parent, name.to_string()), i);
        self.by_path.insert(path, i);
        i
    }

    /// Whether a node holds others.
    pub fn is_folder(&self, node: usize) -> bool {
        !self.nodes[node].children.is_empty()
    }

    /// The node at a folder or item path.
    pub fn at(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    /// The nodes shown, top to bottom: folders open in `open` show their
    /// children.  With a `filter`, only items whose path contains it (and
    /// the folders leading to them) show, all folders open.
    pub fn rows(&self, contents: &Contents, open: &HashSet<String>, filter: &str) -> Vec<usize> {
        let needle = filter.trim().to_lowercase();
        let keep: Option<Vec<bool>> = (!needle.is_empty()).then(|| {
            let mut keep = vec![false; self.nodes.len()];
            for i in (0..self.nodes.len()).rev() {
                let n = &self.nodes[i];
                let hit =
                    n.item.and_then(|id| contents.item(id)).is_some_and(|it| it.path.to_lowercase().contains(&needle));
                if hit || n.children.iter().any(|&c| keep[c]) {
                    keep[i] = true;
                }
            }
            keep
        });
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            if keep.as_ref().is_some_and(|k| !k[i]) {
                continue;
            }
            out.push(i);
            let descend = keep.is_some() || open.contains(&self.nodes[i].path);
            if descend {
                stack.extend(self.nodes[i].children.iter().rev());
            }
        }
        out
    }

    /// The ancestors of a node, nearest first.
    pub fn ancestors(&self, mut node: usize) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(p) = self.nodes[node].parent {
            out.push(p);
            node = p;
        }
        out
    }
}

//endregion

/*┌─────────────────╮
| Selection       |
└─────────────────╯*/
//region Selection

/// Which nodes are selected, the current one, and where a Shift-click range
/// starts, as a file manager's list keeps them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub set: HashSet<usize>,
    pub cursor: Option<usize>,
    pub anchor: Option<usize>,
}

impl Selection {
    /// A click or key on `node`.  `rows` are the nodes shown; `extend` is
    /// Shift (select from the anchor to `node`), `toggle` is Ctrl (flip
    /// `node`; with Shift, add the range).
    pub fn pick(&mut self, node: usize, rows: &[usize], extend: bool, toggle: bool) {
        let at = |n: usize| rows.iter().position(|&r| r == n);
        if extend {
            let from = self.anchor.and_then(at).or(self.cursor.and_then(at)).or(at(node));
            if let (Some(a), Some(b)) = (from, at(node)) {
                let (lo, hi) = (a.min(b), a.max(b));
                if !toggle {
                    self.set.clear();
                }
                self.set.extend(rows[lo..=hi].iter().copied());
            } else {
                self.set.insert(node);
            }
            self.cursor = Some(node);
            if self.anchor.is_none() {
                self.anchor = Some(node);
            }
        } else if toggle {
            if !self.set.remove(&node) {
                self.set.insert(node);
            }
            self.cursor = Some(node);
            self.anchor = Some(node);
        } else {
            self.set.clear();
            self.set.insert(node);
            self.cursor = Some(node);
            self.anchor = Some(node);
        }
    }

    pub fn select_all(&mut self, rows: &[usize]) {
        self.set = rows.iter().copied().collect();
        if self.cursor.is_none() {
            self.cursor = rows.first().copied();
        }
    }

    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    /// Forgets nodes that are no longer shown (a folder closed over them
    /// or a filter hid them) when they were selected by clicks elsewhere.
    pub fn keep_only(&mut self, shown: &HashSet<usize>) {
        self.set.retain(|n| shown.contains(n));
        if self.cursor.is_some_and(|c| !shown.contains(&c)) {
            self.cursor = None;
        }
        if self.anchor.is_some_and(|c| !shown.contains(&c)) {
            self.anchor = None;
        }
    }
}

//endregion

/*┌─────────────────╮
| State           |
└─────────────────╯*/
//region State

/// What the view shows when an entry draws no picture.
#[derive(Clone, Debug, Default)]
pub struct Card {
    pub title: String,
    pub facts: Vec<(String, String)>,
    /// A text entry's text.
    pub text: Option<String>,
    /// Why nothing could be shown.
    pub problem: Option<String>,
}

/// Where a document came from when it is an entry of the archive.
#[derive(Clone, Debug)]
pub struct Origin {
    pub archive: PathBuf,
    /// The window title.
    pub title: String,
}

/// What a thread prepared for the selected entry.
struct Prepared {
    kind: PreviewKind,
    facts: Vec<(String, String)>,
}

/// An action finished on a thread.
enum Finished {
    Extracted { files: usize, bytes: u64, skipped: Vec<(String, String)>, out: PathBuf },
    Copied(Vec<PathBuf>),
}

pub struct State {
    pub contents: Arc<Contents>,
    /// The file the panel lists, which stays the file the window is "on".
    pub archive: PathBuf,
    pub tree: Tree,
    open: HashSet<String>,
    pub filter: String,
    rows: Vec<usize>,
    pub selection: Selection,
    pub format: Format,
    /// GLB files of nodes and meshes are moved to the origin.
    pub centre: bool,
    /// GLB files of clips hold the skeleton alone.
    pub skeleton_only: bool,
    /// A new row to scroll into view.
    scroll_to: Option<usize>,
    /// The entry being shown, and the request that is preparing it.
    shown: Option<ItemKey>,
    request: u64,
    preparing: Option<(u64, loader::Task<Prepared>)>,
    /// The temporary file the viewer is loading or showing for the entry.
    pub preview_path: Option<PathBuf>,
    facts: Vec<(String, String)>,
    pub card: Option<Card>,
    working: Option<(&'static str, loader::Task<Finished>)>,
    /// A drag began on these items (for the drag-out code to take).
    drag: Option<Vec<ItemId>>,
    /// The tree had the keyboard focus at the end of the last frame.
    tree_focused: bool,
    /// The file's own document, set aside while a preview of one of its
    /// pieces (a texture) or a card is shown, so it comes back at once.
    parked: Option<Document>,
    /// Each node's visibility before a piece was shown alone.
    isolated: Option<Vec<bool>>,
    /// A blend shape or expression a piece set, to undo.
    morph: Option<Morph>,
    /// The item under the pointer in the tree, and its outline in the view
    /// (item, pose version, box).
    hovered: Option<ItemId>,
    outline: Option<(ItemId, u64, slerp_scene::Aabb)>,
    thumbs: thumbs::Thumbs,
}

/// A morph weight a piece set.
enum Morph {
    Named(String),
    Indexed(Vec<(usize, usize)>),
    Expression(usize),
}

impl State {
    pub fn new(contents: Arc<Contents>, archive: PathBuf) -> State {
        let tree = Tree::build(&contents);
        let mut open = HashSet::new();
        if contents.kind().is_some() {
            // A file's pieces open on their groups (Meshes, Textures, ...)
            // and the top of the node hierarchy, which can run deep.
            open.extend(
                tree.nodes
                    .iter()
                    .filter(|n| {
                        !n.children.is_empty() && (n.depth == 0 || (n.depth == 1 && n.path.starts_with("Nodes/")))
                    })
                    .map(|n| n.path.clone()),
            );
        } else if contents.items().len() <= 40 {
            // A small archive opens fully, a big one at its top level.
            open.extend(tree.nodes.iter().filter(|n| !n.children.is_empty()).map(|n| n.path.clone()));
        }
        let mut s = State {
            contents,
            archive,
            tree,
            open,
            filter: String::new(),
            rows: Vec::new(),
            selection: Selection::default(),
            format: Format::Auto,
            centre: false,
            skeleton_only: false,
            scroll_to: None,
            shown: None,
            request: 0,
            preparing: None,
            preview_path: None,
            facts: Vec::new(),
            card: None,
            working: None,
            drag: None,
            tree_focused: false,
            parked: None,
            isolated: None,
            morph: None,
            hovered: None,
            outline: None,
            thumbs: thumbs::Thumbs::default(),
        };
        s.refresh_rows();
        s
    }

    /// Whether the panel lists the pieces of a file rather than an
    /// archive's entries.
    pub fn pieces(&self) -> bool {
        self.contents.kind().is_some()
    }

    fn refresh_rows(&mut self) {
        self.rows = self.tree.rows(&self.contents, &self.open, &self.filter);
    }

    /// Replaces the listing after the file changed on disk, keeping what
    /// was selected and open where it still exists.  Returns the item to
    /// show again, if one was shown.
    pub fn reload(&mut self, contents: Arc<Contents>) -> Option<ItemKey> {
        let keys: Vec<ItemKey> = self
            .selected_nodes()
            .iter()
            .filter_map(|&n| self.tree.nodes[n].item)
            .filter_map(|id| self.contents.key(id))
            .collect();
        let cursor_key =
            self.selection.cursor.and_then(|n| self.tree.nodes[n].item).and_then(|id| self.contents.key(id));
        let cursor_path = self.selection.cursor.map(|n| self.tree.nodes[n].path.clone());
        let selected_paths: Vec<String> =
            self.selected_nodes().iter().map(|&n| self.tree.nodes[n].path.clone()).collect();
        self.contents = contents;
        self.tree = Tree::build(&self.contents);
        self.refresh_rows();
        self.selection.clear();
        // Item numbers change with the listing.
        self.thumbs.clear();
        self.hovered = None;
        self.outline = None;
        // Items by key (repeated names), folders by path.
        let mut node_of_item: HashMap<ItemId, usize> = HashMap::new();
        for (i, n) in self.tree.nodes.iter().enumerate() {
            if let Some(id) = n.item {
                node_of_item.insert(id, i);
            }
        }
        for k in &keys {
            if let Some(&n) = self.contents.find_key(k).and_then(|id| node_of_item.get(&id)) {
                self.selection.set.insert(n);
            }
        }
        for p in &selected_paths {
            if let Some(n) = self.tree.at(p) {
                self.selection.set.insert(n);
            }
        }
        self.selection.cursor = cursor_key
            .and_then(|k| self.contents.find_key(&k))
            .and_then(|id| node_of_item.get(&id).copied())
            .or_else(|| cursor_path.and_then(|p| self.tree.at(&p)));
        self.selection.anchor = self.selection.cursor;
        self.open.retain(|p| self.tree.at(p).is_some());
        self.refresh_rows();
        if let Some(c) = self.selection.cursor {
            self.reveal(c);
        }
        self.shown.clone().filter(|k| self.contents.find_key(k).is_some())
    }

    /// Opens the folders above a node and scrolls to it.
    fn reveal(&mut self, node: usize) {
        for a in self.tree.ancestors(node) {
            self.open.insert(self.tree.nodes[a].path.clone());
        }
        self.refresh_rows();
        self.scroll_to = self.rows.iter().position(|&r| r == node);
    }

    pub fn set_filter(&mut self, text: String) {
        if text != self.filter {
            self.filter = text;
            self.refresh_rows();
            let shown: HashSet<usize> = self.rows.iter().copied().collect();
            // Keep the selection that the filter still shows.
            self.selection.keep_only(&shown);
        }
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    /// The selected nodes in tree order.
    pub fn selected_nodes(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.selection.set.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// What the selection stands for as items and folders, for copying and
    /// extracting: a selected folder stands for everything beneath it, so
    /// nodes inside a selected folder aren't listed again.
    pub fn selected(&self) -> Vec<Selected> {
        let chosen = &self.selection.set;
        let mut out = Vec::new();
        for n in self.selected_nodes() {
            if self.tree.ancestors(n).iter().any(|a| chosen.contains(a) && self.tree.nodes[*a].item.is_none()) {
                continue;
            }
            let node = &self.tree.nodes[n];
            match node.item {
                Some(id) => out.push(Selected::Item(id)),
                None => out.push(Selected::Folder(node.path.clone())),
            }
        }
        out
    }

    /// Every item the selection covers, for counting and dragging.
    pub fn selected_items(&self) -> Vec<ItemId> {
        let mut ids = Vec::new();
        for s in self.selected() {
            match s {
                Selected::Item(id) => ids.push(id),
                Selected::Folder(p) => ids.extend(self.contents.items_in(&p)),
            }
        }
        ids
    }

    pub fn has_card(&self) -> bool {
        self.card.is_some()
    }
}

//endregion

/*┌─────────────────╮
| Hooks for app   |
└─────────────────╯*/
//region Hooks for app

/// The file that the open panel lists, for [`materialize`].
static CURRENT: Mutex<Option<Arc<Contents>>> = Mutex::new(None);
static TEMP: OnceLock<Option<TempFolder>> = OnceLock::new();

fn temp() -> Option<&'static TempFolder> {
    TEMP.get_or_init(|| {
        slerp_core::contents::clean_stale();
        TempFolder::new().ok()
    })
    .as_ref()
}

/// Removes this process's temporary files (the previews, pasted and
/// dragged entries), at exit.
pub fn shutdown() {
    if let Some(Some(t)) = TEMP.get() {
        t.clean();
    }
}

/// Writes items from the file in the panel as ordinary files in a
/// temporary folder (converted as `options` says: [`Format::Auto`] makes
/// pictures PNG, sounds WAV, music MIDI and meshes GLB, and keeps embedded
/// pictures as they were stored), and returns their paths, one per item
/// that could be written, ready for a drag or a clipboard file list.  The
/// folder is private to the user and removed when the viewer exits (and,
/// if it crashes, at the next start).  Empty when no file is open or
/// nothing could be written.
pub fn materialize(ids: &[ItemId], options: &ExtractOptions) -> Vec<PathBuf> {
    let Some(contents) = CURRENT.lock().ok().and_then(|c| c.clone()) else { return Vec::new() };
    let Some(temp) = temp() else { return Vec::new() };
    let Ok(batch) = temp.batch() else { return Vec::new() };
    let selection: Vec<Selected> = ids.iter().map(|&i| Selected::Item(i)).collect();
    contents.materialize(&selection, options, &batch).unwrap_or_default()
}

/// The items a drag out of the tree began with, once, for the drag-out
/// code, with the panel's choice of format.  Items that can't stand alone
/// (a camera, a rig control) are left out; the scene goes to the contents
/// model first when saving them needs it.
pub fn take_drag(v: &mut Viewer) -> Option<(Vec<ItemId>, ExtractOptions)> {
    let state = v.contents.as_mut()?;
    let ids: Vec<ItemId> =
        state.drag.take()?.into_iter().filter(|&id| state.contents.item(id).is_some_and(|i| i.can_save())).collect();
    let options = state.options();
    if ids.is_empty() {
        return None;
    }
    hand_over_scene(v, &ids);
    Some((ids, options))
}

impl State {
    /// How the panel's choices save items.
    fn options(&self) -> ExtractOptions {
        ExtractOptions {
            format: self.format,
            centre: self.centre,
            skeleton_only: self.skeleton_only,
            ..Default::default()
        }
    }

    /// Whether `doc` is the file the panel lists, not a preview of an item.
    fn is_own(&self, doc: &Document) -> bool {
        doc.origin.is_none() && doc.path == self.archive
    }
}

/// Gives the contents model the scene on screen when saving `ids` needs it
/// (meshes, nodes and clips as GLB), so the file isn't loaded again and the
/// numbers in the listing are the ones on screen.  Visibility is the
/// file's own, not what showing pieces alone changed.
fn hand_over_scene(v: &Viewer, ids: &[ItemId]) {
    let Some(state) = &v.contents else { return };
    if !state.contents.needs_scene(ids) {
        return;
    }
    let own = v.doc.as_ref().filter(|d| state.is_own(d)).or(state.parked.as_ref());
    if let Some(view) = own.and_then(|d| d.scene()) {
        let scene = slerp_core::contents::clone_scene(&view.scene, Some(&view.initial_visible));
        state.contents.set_scene(Arc::new(scene));
    }
}

impl Document {
    /// The file the window is about: the archive for an entry of one, else
    /// the file itself.
    pub fn source_path(&self) -> &Path {
        self.origin.as_ref().map_or(self.path.as_path(), |o| o.archive.as_path())
    }
}

/// Called with each loaded document before it is installed: marks it as an
/// archive entry when it is the temporary file the panel asked for.
pub fn tag(v: &Viewer, doc: &mut Document) {
    let Some(state) = &v.contents else { return };
    if state.preview_path.as_deref() != Some(doc.path.as_path()) {
        return;
    }
    let Some(key) = state.shown.clone() else { return };
    let name = key.path.rsplit('/').next().unwrap_or(&key.path).to_string();
    doc.neighbours = state.contents.files();
    doc.facts.splice(0..0, state.facts.iter().cloned());
    doc.origin =
        Some(Origin { title: format!("{name} — {}", short_name(&state.archive)), archive: state.archive.clone() });
}

/// Called when a document is installed: starts, keeps, refreshes or ends
/// the panel for the file the window is now on.  A preview of one of the
/// file's pieces sets the file's own document aside rather than dropping
/// it.
pub fn installed(v: &mut Viewer, doc: &Document) {
    // An entry of the archive or a piece of the file: the panel is already
    // right.
    if doc.origin.is_some() {
        park(v);
        return;
    }
    let Some(contents) = doc.contents.clone() else {
        v.contents = None;
        if let Ok(mut c) = CURRENT.lock() {
            *c = None;
        }
        return;
    };
    if let Ok(mut c) = CURRENT.lock() {
        *c = Some(contents.clone());
    }
    let again = match &mut v.contents {
        Some(state) if state.archive == doc.path => {
            // The file's own document again (reloaded, or another part):
            // whatever was set aside is out of date.
            state.parked = None;
            state.card = None;
            state.preview_path = None;
            if state.isolated.as_ref().is_some_and(|i| doc.scene().is_none_or(|s| s.scene.nodes.len() != i.len())) {
                state.isolated = None;
            }
            state.morph = None;
            if Arc::ptr_eq(&state.contents, &contents) {
                None
            } else {
                // Pieces are shown in the document itself; only an
                // archive's entry needs preparing again.
                state.reload(contents).filter(|_| doc.contents.as_ref().is_none_or(|c| c.kind().is_none()))
            }
        }
        _ => {
            v.contents = Some(State::new(contents, doc.path.clone()));
            None
        }
    };
    if let Some(key) = again {
        request_preview(v, Some(key));
    }
}

/// Called when a load failed: if it was the panel's preview, show why on
/// the card and return true.
pub fn load_failed(v: &mut Viewer, path: &Path, error: &str) -> bool {
    let Some(state) = &mut v.contents else { return false };
    if state.preview_path.as_deref() != Some(path) {
        return false;
    }
    let title = state.shown.as_ref().map(|k| k.path.clone()).unwrap_or_default();
    state.card = Some(Card {
        title,
        facts: state.facts.clone(),
        text: None,
        problem: Some(format!("This can't be shown: {error}")),
    });
    park(v);
    v.failed = None;
    true
}

/// Reloads the archive's listing in place (F5, or a change on disk) without
/// leaving the entry being shown.  Returns false when there is no panel,
/// or when the panel lists a file's pieces: that file is reloaded as a
/// whole, with its own document put back first.
pub fn reload(v: &mut Viewer) -> bool {
    let Some(state) = &v.contents else { return false };
    if state.pieces() {
        unpark(v);
        return false;
    }
    let path = state.archive.clone();
    let result = Contents::open(&path);
    match result {
        Ok(contents) => {
            let contents = Arc::new(contents);
            if let Ok(mut c) = CURRENT.lock() {
                *c = Some(contents.clone());
            }
            let again = v.contents.as_mut().and_then(|s| s.reload(contents));
            v.file_notice = Some(crate::watch::Notice::Reloaded(std::time::Instant::now()));
            let files = v.contents.as_ref().map(|s| s.contents.files()).unwrap_or_default();
            v.watch.watch(&files);
            if let Some(key) = again {
                request_preview(v, Some(key));
            }
        }
        Err(e) => v.file_notice = Some(crate::watch::Notice::Failed(e.to_string())),
    }
    true
}

//endregion

/*┌─────────────────╮
| Previews        |
└─────────────────╯*/
//region Previews

/// Takes the document out of the view for a card or a preview: the file's
/// own document of a file with pieces waits aside; anything else (an
/// archive's previous entry, an archive's gallery) is dropped, as before.
fn park(v: &mut Viewer) {
    let Some(state) = &mut v.contents else {
        v.doc = None;
        return;
    };
    match v.doc.take() {
        Some(doc) if state.pieces() && state.is_own(&doc) => state.parked = Some(doc),
        _ => {}
    }
}

/// Puts the file's own document back in the view when a preview or card
/// replaced it.  Returns whether the file's own document is in the view.
fn unpark(v: &mut Viewer) -> bool {
    let Some(state) = &mut v.contents else { return false };
    let Some(doc) = state.parked.take() else {
        return v.doc.as_ref().is_some_and(|d| state.is_own(d));
    };
    state.card = None;
    state.preview_path = None;
    state.preparing = None;
    v.doc = None;
    match v.waker.get().cloned() {
        Some(ctx) => {
            // Installing gives the view the keyboard; the tree keeps it
            // while the user steps through items.
            let focused = ctx.memory(|m| m.focused());
            v.install(doc, &ctx);
            if let Some(f) = focused {
                ctx.memory_mut(|m| m.request_focus(f));
            }
        }
        None => v.doc = Some(doc),
    }
    true
}

/// Starts showing an item: prepares it on a thread (`key` names it) and,
/// when it is ready, hands a file to the loader or sets the card.
fn request_preview(v: &mut Viewer, key: Option<ItemKey>) {
    let Some(state) = &mut v.contents else { return };
    let Some(key) = key else { return };
    let Some(id) = state.contents.find_key(&key) else { return };
    state.shown = Some(key);
    state.request += 1;
    let request = state.request;
    let contents = state.contents.clone();
    let task = loader::spawn(&v.waker, "slerp-preview", move || {
        let p = contents.preview(id).map_err(|e| e.to_string())?;
        Ok(Prepared { kind: p.kind, facts: p.facts })
    });
    state.preparing = Some((request, task));
}

/// Shows the node the panel's cursor is on: a map folder opens the map, a
/// folder shows a summary, an item is shown as `Contents::show` says.
fn show_node(v: &mut Viewer, node: usize) {
    let Some(state) = &mut v.contents else { return };
    let n = state.tree.nodes[node].clone();
    if n.map {
        // The map itself, in 3D, as when the WAD was opened.
        state.shown = None;
        state.facts.clear();
        state.card = None;
        state.preview_path = None;
        state.preparing = None;
        v.actions.push(Action::OpenPart(loader::Part::Named(n.name.clone())));
        return;
    }
    match n.item {
        Some(id) => show_item(v, id),
        None => {
            // A folder: what is in it.
            let kinds = slerp_core::contents::count_kinds(
                &state
                    .contents
                    .items_in(&n.path)
                    .iter()
                    .filter_map(|&i| state.contents.item(i).cloned())
                    .collect::<Vec<_>>(),
            );
            let mut kinds: Vec<(&'static str, usize)> = kinds.into_iter().map(|(k, c)| (k.label(), c)).collect();
            kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let mut facts = vec![("Items".into(), n.count.to_string())];
            if n.bytes > 0 {
                facts.push(("Size".into(), super::bytes(n.bytes)));
            }
            facts.push(("Kinds".into(), kinds.iter().map(|(k, c)| format!("{c} {k}")).collect::<Vec<_>>().join(", ")));
            state.shown = None;
            state.preparing = None;
            if state.pieces() {
                // The file stays in view; the facts go in the panel.
                state.facts = facts;
                return;
            }
            state.facts.clear();
            state.preview_path = None;
            state.card = Some(Card { title: format!("{}/", n.path), facts, text: None, problem: None });
            park(v);
            v.failed = None;
        }
    }
}

/// The facts the panel lists about a piece.
fn piece_facts(contents: &Contents, id: ItemId) -> Vec<(String, String)> {
    let Some(item) = contents.item(id) else { return Vec::new() };
    let mut facts = vec![("Item".to_string(), item.path.clone())];
    let mut kind = item.kind.label().to_string();
    if let Some(first) = kind.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    facts.push(("Type".into(), kind));
    if !item.detail.is_empty() {
        facts.push(("About".into(), item.detail.clone()));
    }
    let formats: Vec<&str> = item.formats.iter().map(|f| f.label()).collect();
    facts.push((
        "Saves as".into(),
        if formats.is_empty() {
            "nothing on its own: it only means something inside the file".into()
        } else {
            formats.join(", ")
        },
    ));
    facts
}

/// Shows an item: an archive's entry or a piece's own picture as a
/// preview, anything else in the file that is open.
fn show_item(v: &mut Viewer, id: ItemId) {
    let Some(state) = &mut v.contents else { return };
    let show = state.contents.show(id);
    let key = state.contents.key(id);
    if show == Show::Preview {
        request_preview(v, key);
        return;
    }
    state.shown = key;
    state.preparing = None;
    state.facts = piece_facts(&state.contents, id);
    if show == Show::Facts {
        return;
    }
    unpark(v);
    apply_show(v, show);
}

/// Shows a piece in the document of the file it belongs to.
fn apply_show(v: &mut Viewer, show: Show) {
    let aspect = v.aspect();
    let Viewer { doc, gfx, contents, actions, .. } = v;
    let (Some(state), Some(doc)) = (contents.as_mut(), doc.as_mut()) else { return };
    if let Some(view) = doc.scene_mut() {
        undo_morph(state, view);
    }
    match show {
        Show::Whole => {
            if let Some(view) = doc.scene_mut() {
                show_all(state, view, gfx);
                view.cameras.home();
            } else if let Some(c) = doc.canvas_mut() {
                c.shown = c.canvas.layers().iter().map(|l| l.visible).collect();
                c.layers_changed();
            }
        }
        Show::Nodes { nodes, subtree } => {
            if let Some(view) = doc.scene_mut() {
                isolate(state, view, gfx, &nodes, subtree, aspect);
            }
        }
        Show::BlendShape { nodes, mesh, target } => {
            if let Some(view) = doc.scene_mut() {
                isolate(state, view, gfx, &nodes, false, aspect);
                let name = view.scene.meshes.get(mesh).and_then(|m| m.target_names.get(target)).cloned();
                state.morph = Some(match name.filter(|n| !n.is_empty()) {
                    Some(name) => {
                        view.morphs.named.insert(name.clone(), 1.0);
                        Morph::Named(name)
                    }
                    None => {
                        let keys: Vec<(usize, usize)> = nodes.iter().map(|&n| (n, target)).collect();
                        for &k in &keys {
                            view.morphs.indexed.insert(k, 1.0);
                        }
                        Morph::Indexed(keys)
                    }
                });
                view.pose_dirty = true;
            }
        }
        Show::Clip(i) => {
            if let Some(view) = doc.scene_mut() {
                show_all(state, view, gfx);
                view.player.select(&view.scene, Some(i));
                view.player.playing = true;
                view.pose_dirty = true;
            }
        }
        Show::Expression(i) => {
            if let Some(view) = doc.scene_mut()
                && let Some(w) = view.morphs.expressions.get_mut(i)
            {
                *w = 1.0;
                state.morph = Some(Morph::Expression(i));
                view.pose_dirty = true;
            }
        }
        Show::Surface { level, layer, face } => {
            if let Some(t) = doc.texture_mut() {
                t.params.mip = Some(level);
                t.params.layer = layer;
                t.params.face = face;
            }
        }
        Show::Layer(i) => {
            if let Some(c) = doc.canvas_mut() {
                c.shown = slerp_core::contents::layer_alone(c.canvas.layers(), i);
                c.layers_changed();
            }
        }
        Show::Frame(i) => {
            if let Some(c) = doc.canvas_mut() {
                c.shown = c.canvas.layers().iter().map(|l| l.visible).collect();
                c.layers_changed();
                c.player.select(&*c.canvas, None);
                c.player.seek(&*c.canvas, i);
                c.player.playing = false;
            }
        }
        Show::Tag(i) => {
            if let Some(c) = doc.canvas_mut() {
                c.player.select(&*c.canvas, Some(i));
                c.player.playing = true;
            }
        }
        Show::Picture(i) => {
            if let crate::document::Content::Gallery(g) = &mut doc.content
                && let Err(e) = g.open_item(i, gfx)
            {
                state.facts.push(("Problem".into(), e));
            }
        }
        Show::Part(name) => actions.push(Action::OpenPart(loader::Part::Named(name))),
        Show::Detail(lod) => {
            if let Some(dna) = doc.scene_mut().and_then(|s| s.dna.as_mut()) {
                dna.request_lod(lod);
            }
        }
        Show::Preview | Show::Facts => {}
    }
}

/// Shows `nodes` (with what is beneath them when `subtree`) and nothing
/// else, and frames them.  Their parents stay visible, since hiding a node
/// hides everything beneath it; what is inside a shown subtree keeps the
/// visibility the file gave it.
fn isolate(
    state: &mut State,
    view: &mut crate::scene_view::SceneView,
    gfx: &mut crate::gfx::Gfx,
    nodes: &[usize],
    subtree: bool,
    aspect: f32,
) {
    let n = view.scene.nodes.len();
    let before = match &state.isolated {
        Some(b) if b.len() == n => b.clone(),
        _ => {
            let b: Vec<bool> = view.scene.nodes.iter().map(|n| n.visible).collect();
            state.isolated = Some(b.clone());
            b
        }
    };
    let shown = isolation(&view.scene, &before, nodes, subtree);
    for (i, shown) in shown.into_iter().enumerate() {
        view.set_visible(i, shown);
    }
    view.refresh_pose(gfx);
    let b = view.frame_bounds();
    view.cameras.frame(&b, aspect);
}

/// Which nodes show when `nodes` are shown alone: they and their parents
/// (hiding a node hides everything beneath it), and, with `subtree`, what
/// is beneath them as the file had it (`before`).
fn isolation(scene: &slerp_scene::Scene, before: &[bool], nodes: &[usize], subtree: bool) -> Vec<bool> {
    let n = scene.nodes.len();
    let parents = scene.parents();
    let mut path = vec![false; n];
    let mut inside = vec![false; n];
    for &t in nodes.iter().filter(|&&t| t < n) {
        // Up to the root, guarding against loops.
        let mut at = Some(t);
        let mut steps = 0;
        while let Some(i) = at.filter(|_| steps <= n) {
            path[i] = true;
            at = parents[i];
            steps += 1;
        }
        if subtree {
            let mut stack = vec![t];
            while let Some(i) = stack.pop() {
                if std::mem::replace(&mut inside[i], true) {
                    continue;
                }
                stack.extend(scene.nodes[i].children.iter().copied().filter(|&c| c < n));
            }
        }
    }
    (0..n).map(|i| path[i] || (inside[i] && before.get(i).copied().unwrap_or(true))).collect()
}

/// Shows again what showing pieces alone hid.
fn show_all(state: &mut State, view: &mut crate::scene_view::SceneView, gfx: &mut crate::gfx::Gfx) {
    if let Some(before) = state.isolated.take()
        && before.len() == view.scene.nodes.len()
    {
        for (i, shown) in before.into_iter().enumerate() {
            view.set_visible(i, shown);
        }
        view.refresh_pose(gfx);
    }
}

/// Takes back the blend shape or expression the last piece set.
fn undo_morph(state: &mut State, view: &mut crate::scene_view::SceneView) {
    match state.morph.take() {
        Some(Morph::Named(name)) => {
            view.morphs.named.remove(&name);
        }
        Some(Morph::Indexed(keys)) => {
            for k in keys {
                view.morphs.indexed.remove(&k);
            }
        }
        Some(Morph::Expression(i)) => {
            if let Some(w) = view.morphs.expressions.get_mut(i) {
                *w = 0.0;
            }
        }
        None => return,
    }
    view.pose_dirty = true;
}

/// Collects what the threads finished: a prepared preview, an extraction,
/// a clipboard copy, row pictures.  Called every frame.
pub fn poll(v: &mut Viewer, ctx: &egui::Context) {
    // 1. A prepared preview.
    let ready = match &v.contents {
        Some(State { preparing: Some((_, rx)), .. }) => loader::poll(rx),
        _ => None,
    };
    if let Some(result) = ready
        && let Some(state) = &mut v.contents
        && let Some((request, _)) = state.preparing.take()
    {
        let current = request == state.request;
        match result {
            Ok(p) if current => apply_prepared(v, p),
            Err(e) if current => {
                let title = state.shown.as_ref().map(|k| k.path.clone()).unwrap_or_default();
                state.card = Some(Card { title, facts: Vec::new(), text: None, problem: Some(e) });
                state.preview_path = None;
                park(v);
                v.failed = None;
            }
            _ => {}
        }
    }
    // 2. Extraction or copying.
    let done = match &v.contents {
        Some(State { working: Some((_, rx)), .. }) => loader::poll(rx),
        _ => None,
    };
    if let Some(result) = done {
        let what = v.contents.as_mut().and_then(|s| s.working.take()).map_or("", |(w, _)| w);
        match result {
            Ok(Finished::Extracted { files, bytes, skipped, out }) => {
                let mut text = format!(
                    "Extracted {files} file{} ({}) to {}",
                    if files == 1 { "" } else { "s" },
                    super::bytes(bytes),
                    out.display()
                );
                if !skipped.is_empty() {
                    text += &format!("; {} not written ({})", skipped.len(), skipped[0].1);
                    v.warn(text);
                } else {
                    v.notify(text);
                }
            }
            Ok(Finished::Copied(paths)) => copy_to_clipboard(v, ctx, paths),
            Err(e) => v.warn(format!("{what} failed: {e}")),
        }
    }
    // 3. Row pictures.
    if let Some(state) = &mut v.contents {
        thumbs::poll(state, ctx);
    }
}

fn apply_prepared(v: &mut Viewer, p: Prepared) {
    let Some(state) = &mut v.contents else { return };
    let title = state.shown.as_ref().map(|k| k.path.clone()).unwrap_or_default();
    state.facts = p.facts.clone();
    match p.kind {
        PreviewKind::File { name, bytes } => {
            let Some(temp) = temp() else {
                state.card = Some(Card {
                    title,
                    facts: p.facts,
                    text: None,
                    problem: Some("No folder for temporary files.".into()),
                });
                return;
            };
            let written = temp.batch().and_then(|dir| {
                let file = dir.join(slerp_core::contents::safe_component(&name));
                std::fs::write(&file, &bytes)?;
                Ok(file)
            });
            match written {
                Ok(file) => {
                    state.card = None;
                    state.preview_path = Some(file.clone());
                    v.reloading = None;
                    v.file_notice = None;
                    v.loader.request(file);
                }
                Err(e) => {
                    state.card = Some(Card { title, facts: p.facts, text: None, problem: Some(e.to_string()) });
                }
            }
        }
        PreviewKind::Text(text) => {
            state.card = Some(Card { title, facts: p.facts, text: Some(text), problem: None });
            state.preview_path = None;
            park(v);
            v.failed = None;
        }
        PreviewKind::Info => {
            state.card = Some(Card { title, facts: p.facts, text: None, problem: None });
            state.preview_path = None;
            park(v);
            v.failed = None;
        }
    }
}

//endregion

/*┌─────────────────╮
| Outline         |
└─────────────────╯*/
//region Outline

/// Outlines the item under the pointer in the tree (a node, a mesh, the
/// meshes wearing a material) in the 3D view, as a box around it.  Called
/// over the view each frame.
pub fn overlay(v: &mut Viewer, painter: &egui::Painter, rect: egui::Rect) {
    let aspect = v.aspect();
    let Viewer { doc, contents, .. } = v;
    let (Some(state), Some(doc)) = (contents.as_mut(), doc.as_ref()) else { return };
    let Some(id) = state.hovered else { return };
    if !state.is_own(doc) {
        return;
    }
    let Some(view) = doc.scene() else { return };
    let bounds = match state.outline {
        Some((i, pose, b)) if i == id && pose == view.pose_version => b,
        _ => {
            let nodes = state.contents.nodes(id);
            let b = world_bounds(view, &nodes, matches!(state.contents.show(id), Show::Nodes { subtree: true, .. }));
            state.outline = Some((id, view.pose_version, b));
            b
        }
    };
    if bounds.is_empty() {
        return;
    }
    let vp = view.cameras.view(aspect).view_proj();
    let (lo, hi) = (bounds.min, bounds.max);
    let corner = |i: u32| {
        glam::Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    let palette = slerp_theme::palette(painter.ctx());
    let stroke = egui::Stroke::new(1.5, palette.highlight);
    for i in 0..8u32 {
        for bit in [1, 2, 4] {
            if i & bit != 0 {
                continue;
            }
            let (a, b) = (corner(i), corner(i | bit));
            if let (Some(a), Some(b)) =
                (crate::scene_view::project(&vp, a, rect), crate::scene_view::project(&vp, b, rect))
            {
                painter.line_segment([a, b], stroke);
            }
        }
    }
}

/// The box around the meshes of `nodes` (and of what is beneath them, when
/// `subtree`) as posed now; skinned meshes follow their joints.
fn world_bounds(view: &crate::scene_view::SceneView, nodes: &[usize], subtree: bool) -> slerp_scene::Aabb {
    let scene = &view.scene;
    let mut all: Vec<usize> = Vec::new();
    let mut seen = vec![false; scene.nodes.len()];
    let mut stack: Vec<usize> = nodes.iter().copied().filter(|&n| n < scene.nodes.len()).collect();
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        all.push(i);
        if subtree {
            stack.extend(scene.nodes[i].children.iter().copied().filter(|&c| c < scene.nodes.len()));
        }
    }
    let mut b = slerp_scene::Aabb::EMPTY;
    for i in all {
        let node = &scene.nodes[i];
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
        let world = view.pose.world.get(i).copied().unwrap_or(glam::Mat4::IDENTITY);
        let joints = node.skin.filter(|&s| s < scene.skins.len()).map(|s| view.pose.joint_matrices(scene, s));
        for p in &mesh.primitives {
            match (&joints, p.is_skinned()) {
                (Some(joints), true) => {
                    for (k, &pos) in p.positions.iter().enumerate() {
                        let (j, w) = (
                            p.joints.get(k).copied().unwrap_or_default(),
                            p.weights.get(k).copied().unwrap_or_default(),
                        );
                        let mut m = glam::Mat4::ZERO;
                        for c in 0..4 {
                            if w[c] != 0.0 {
                                m += joints.get(j[c] as usize).copied().unwrap_or(glam::Mat4::IDENTITY) * w[c];
                            }
                        }
                        b.add(m.transform_point3(pos));
                    }
                }
                _ => {
                    let mut local = slerp_scene::Aabb::EMPTY;
                    p.positions.iter().for_each(|&q| local.add(q));
                    if local.is_empty() {
                        continue;
                    }
                    for c in 0..8u32 {
                        let q = glam::Vec3::new(
                            if c & 1 == 0 { local.min.x } else { local.max.x },
                            if c & 2 == 0 { local.min.y } else { local.max.y },
                            if c & 4 == 0 { local.min.z } else { local.max.z },
                        );
                        b.add(world.transform_point3(q));
                    }
                }
            }
        }
    }
    b
}

//endregion

/*┌─────────────────╮
| Copy and extract|
└─────────────────╯*/
//region Copy and extract

/// What the buttons and keys ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Do {
    /// Choose a folder and extract the selection (or everything).
    Extract,
    /// Put the selection on the clipboard as a file list.
    Copy,
    OpenIn(Editor),
}

/// A program that edits archives, which Slerp hands the file to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Editor {
    Slade,
    VpkEdit,
}

pub fn perform(v: &mut Viewer, what: Do, frame: &crate::shell::Frame) {
    match what {
        Do::Extract => v.file_dialog(frame, Purpose::ExtractTo),
        Do::Copy => copy_selection(v),
        Do::OpenIn(editor) => {
            let Some(state) = &v.contents else { return };
            let archive = state.archive.clone();
            match open_in(editor, &archive) {
                Ok(()) => v.notify(format!("Opened {} in {}", short_name(&archive), editor.name())),
                Err(e) => v.warn(format!("Couldn't start {}: {e}", editor.name())),
            }
        }
    }
}

/// Extracts into `out`: what is selected, or everything when nothing is.
pub fn extract_to(v: &mut Viewer, out: PathBuf) {
    let Some(state) = &mut v.contents else { return };
    if state.working.is_some() {
        v.warn("Another copy or extraction is still running");
        return;
    }
    let selection = state.selected();
    let contents = state.contents.clone();
    let options = state.options();
    let every: Vec<ItemId> =
        if selection.is_empty() { contents.items().iter().map(|i| i.id).collect() } else { state.selected_items() };
    hand_over_scene(v, &every);
    let Some(state) = &mut v.contents else { return };
    let task = loader::spawn(&v.waker, "slerp-extract", move || {
        let report = if selection.is_empty() {
            let ids: Vec<ItemId> = contents.items().iter().map(|i| i.id).collect();
            slerp_core::contents::extract(&contents, &ids, &out, &options)
        } else {
            // Folders keep their own structure, as in the tree.
            let mut report = slerp_core::contents::ExtractReport::default();
            for s in &selection {
                let part = match s {
                    Selected::Item(id) => slerp_core::contents::extract(&contents, &[*id], &out, &options),
                    Selected::Folder(p) => {
                        slerp_core::contents::extract(&contents, &contents.items_in(p), &out, &options)
                    }
                };
                let part = part.map_err(|e| e.to_string())?;
                report.written.extend(part.written);
                report.skipped.extend(part.skipped);
                report.bytes += part.bytes;
            }
            Ok(report)
        }
        .map_err(|e| e.to_string())?;
        Ok(Finished::Extracted { files: report.written.len(), bytes: report.bytes, skipped: report.skipped, out })
    });
    state.working = Some(("Extracting", task));
    v.notify("Extracting…");
}

/// Writes the selection to a temporary folder and, when that is done, puts
/// the paths on the clipboard.
pub fn copy_selection(v: &mut Viewer) {
    let Some(state) = &mut v.contents else { return };
    let selection = state.selected();
    if selection.is_empty() {
        v.notify("Select entries to copy first");
        return;
    }
    if state.working.is_some() {
        return;
    }
    let contents = state.contents.clone();
    let options = state.options();
    let ids = state.selected_items();
    hand_over_scene(v, &ids);
    let Some(state) = &mut v.contents else { return };
    let task = loader::spawn(&v.waker, "slerp-copy", move || {
        let temp = temp().ok_or("no folder for temporary files")?;
        let batch = temp.batch().map_err(|e| e.to_string())?;
        let paths = contents.materialize(&selection, &options, &batch).map_err(|e| e.to_string())?;
        if paths.is_empty() {
            return Err("nothing could be written".to_string());
        }
        Ok(Finished::Copied(paths))
    });
    state.working = Some(("Copying", task));
}

/// The clipboard, kept for the life of the process: on X11 the data is
/// served by the process that set it.
fn clipboard() -> &'static Mutex<Option<arboard::Clipboard>> {
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    CLIPBOARD.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()))
}

fn copy_to_clipboard(v: &mut Viewer, _ctx: &egui::Context, paths: Vec<PathBuf>) {
    let result = match clipboard().lock() {
        Ok(mut guard) => match guard.as_mut() {
            Some(c) => c.set().file_list(&paths).map_err(|e| e.to_string()),
            None => Err("the clipboard isn't available".to_string()),
        },
        Err(_) => Err("the clipboard is busy".to_string()),
    };
    match result {
        Ok(()) => {
            let n = paths.len();
            v.notify(format!(
                "Copied {n} file{} to the clipboard; paste them in a file manager",
                if n == 1 { "" } else { "s" }
            ));
        }
        Err(e) => v.warn(format!("Couldn't copy to the clipboard: {e}")),
    }
}

/// Ctrl+C while the panel is on screen and no text field wants the key.
pub fn shortcuts(v: &mut Viewer, ctx: &egui::Context) {
    // Not while a text field (the search box) has the keyboard, but while the
    // tree or the view has it.
    let view_keys = ctx.memory(|m| m.focused().is_none() || m.has_focus(v.view_id));
    if !v.contents.as_ref().is_some_and(|s| s.tree_focused) && !view_keys {
        return;
    }
    let copy = ctx.input_mut(|i| {
        // Ctrl+Shift+C is the view state's (`App::shortcuts`).
        if i.modifiers.shift {
            return false;
        }
        let mut hit = i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
        // egui also reports Ctrl+C as a copy event.
        let before = i.events.len();
        i.events.retain(|e| !matches!(e, egui::Event::Copy));
        hit |= i.events.len() != before;
        hit
    });
    if copy {
        copy_selection(v);
    }
}

//endregion

/*┌─────────────────╮
| Editors         |
└─────────────────╯*/
//region Editors

impl Editor {
    pub fn name(self) -> &'static str {
        match self {
            Editor::Slade => "SLADE",
            Editor::VpkEdit => "VPKEdit",
        }
    }

    /// The editor that suits an archive format: SLADE for Doom's, VPKEdit
    /// for the other game packs, none for Unity packages (the Unity editor
    /// imports them into a project; it doesn't edit them).
    pub fn for_archive(kind: slerp_core::archive::ArchiveKind) -> Option<Editor> {
        match kind {
            slerp_core::archive::ArchiveKind::UnityPackage => None,
            k if k.is_doom_format() => Some(Editor::Slade),
            _ => Some(Editor::VpkEdit),
        }
    }

    /// Executable names on the PATH and in the usual folders.
    fn names(self) -> &'static [&'static str] {
        match self {
            Editor::Slade => &["slade", "SLADE"],
            Editor::VpkEdit => &["vpkedit", "VPKEdit"],
        }
    }

    /// The Flatpak application id, if it has one.
    fn flatpak(self) -> Option<&'static str> {
        match self {
            Editor::Slade => Some("net.mancubus.SLADE"),
            // VPKEdit isn't on Flathub.
            Editor::VpkEdit => None,
        }
    }

    /// A word an AppImage's file name starts with.
    fn appimage(self) -> &'static str {
        match self {
            Editor::Slade => "slade",
            Editor::VpkEdit => "vpkedit",
        }
    }
}

/// How to start an editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    Executable(PathBuf),
    Flatpak(&'static str),
    /// A macOS application bundle, started with `open -a`.
    MacApp(PathBuf),
}

/// The places to look, so tests can point them at a scratch folder.
#[derive(Clone, Debug, Default)]
pub struct SearchPlaces {
    pub path: Vec<PathBuf>,
    pub extra: Vec<PathBuf>,
    pub flatpak_dirs: Vec<PathBuf>,
    pub appimage_dirs: Vec<PathBuf>,
    pub mac_apps: Vec<PathBuf>,
}

impl SearchPlaces {
    pub fn system() -> SearchPlaces {
        let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut extra: Vec<PathBuf> =
            vec!["/usr/bin".into(), "/usr/local/bin".into(), "/opt/slade".into(), "/opt/vpkedit".into()];
        let mut flatpak_dirs = vec![PathBuf::from("/var/lib/flatpak/app")];
        let mut appimage_dirs = Vec::new();
        let mut mac_apps = vec![PathBuf::from("/Applications")];
        if let Some(h) = &home {
            flatpak_dirs.push(h.join(".local/share/flatpak/app"));
            appimage_dirs.push(h.join("Applications"));
            appimage_dirs.push(h.join(".local/bin"));
            mac_apps.push(h.join("Applications"));
        }
        if cfg!(windows) {
            for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
                if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                    extra.push(base.clone());
                    extra.push(base.join("Programs"));
                    extra.push(base.join("SLADE"));
                    extra.push(base.join("VPKEdit"));
                    extra.push(base.join("Programs").join("SLADE"));
                    extra.push(base.join("Programs").join("VPKEdit"));
                }
            }
        }
        SearchPlaces { path, extra, flatpak_dirs, appimage_dirs, mac_apps }
    }
}

/// Finds an editor: on the PATH or in the usual install folders, as a
/// Flatpak (the app's folder exists), as an AppImage in `~/Applications`,
/// or on a Mac as an application bundle.
pub fn find_program(editor: Editor, places: &SearchPlaces) -> Option<Program> {
    let exe = |dir: &Path, name: &str| {
        let candidates = [dir.join(name), dir.join(format!("{name}.exe"))];
        candidates.into_iter().find(|p| p.is_file())
    };
    for dir in places.path.iter().chain(&places.extra) {
        for name in editor.names() {
            if let Some(p) = exe(dir, name) {
                return Some(Program::Executable(p));
            }
        }
    }
    if let Some(id) = editor.flatpak() {
        for dir in &places.flatpak_dirs {
            if dir.join(id).is_dir() {
                return Some(Program::Flatpak(id));
            }
        }
    }
    for dir in &places.appimage_dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with(editor.appimage()) && name.ends_with(".appimage") && e.path().is_file() {
                return Some(Program::Executable(e.path()));
            }
        }
    }
    for dir in &places.mac_apps {
        let app = dir.join(format!("{}.app", editor.name()));
        if app.is_dir() {
            return Some(Program::MacApp(app));
        }
    }
    None
}

/// The editors found on this machine, looked up once.
pub fn programs() -> &'static HashMap<Editor, Program> {
    static FOUND: OnceLock<HashMap<Editor, Program>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let places = SearchPlaces::system();
        [Editor::Slade, Editor::VpkEdit].into_iter().filter_map(|e| find_program(e, &places).map(|p| (e, p))).collect()
    })
}

/// The command that opens `file` in a program.
pub fn command(program: &Program, file: &Path) -> std::process::Command {
    match program {
        Program::Executable(exe) => {
            let mut c = std::process::Command::new(exe);
            c.arg(file);
            c
        }
        Program::Flatpak(id) => {
            let mut c = std::process::Command::new("flatpak");
            c.arg("run").arg(id).arg(file);
            c
        }
        Program::MacApp(app) => {
            let mut c = std::process::Command::new("open");
            c.arg("-a").arg(app).arg(file);
            c
        }
    }
}

fn open_in(editor: Editor, file: &Path) -> Result<(), String> {
    let program = programs().get(&editor).ok_or("it isn't installed")?;
    command(program, file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

//endregion

/*┌─────────────────╮
| Panel           |
└─────────────────╯*/
//region Panel

/// Draws one node as a row.
fn row_of(state: &State, node: usize, selected: bool) -> TreeRow {
    let n = &state.tree.nodes[node];
    let item = n.item.and_then(|id| state.contents.item(id));
    let folder = state.tree.is_folder(node).then(|| state.open.contains(&n.path) || !state.filter.is_empty());
    let (kind, detail) = match (item, folder) {
        (Some(it), _) => (
            it.kind.label().to_string(),
            if it.size > 0 || (it.detail.is_empty() && matches!(it.source, slerp_core::contents::Source::Entry(_))) {
                super::bytes(it.size)
            } else {
                it.detail.clone()
            },
        ),
        (None, Some(_)) => (if n.map { "map".to_string() } else { String::new() }, count_words(n.count)),
        (None, None) => (String::new(), String::new()),
    };
    let thumb = item.filter(|it| thumbs::has_thumb(it.kind)).and_then(|it| thumbs::row(state, it.id));
    TreeRow {
        depth: n.depth,
        label: n.name.clone(),
        folder,
        kind,
        detail,
        dim: item.is_some_and(|it| it.kind == ItemKind::Marker || it.locked),
        selected,
        thumb,
    }
}

/// The Contents panel, shown while a file with contents is open.  `dock`
/// frames it.
pub(crate) fn contents(v: &mut Viewer, ui: &mut Ui) {
    let mut asked: Vec<Do> = Vec::new();
    let mut shown: Option<usize> = None;
    let waker = v.waker.clone();
    let Some(state) = &mut v.contents else { return };
    state.thumbs.ensure_placeholder(ui.ctx());

    // 1. The title and the search box.
    ui.label(egui::RichText::new("Contents").text_style(heading2()));
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("{}, {}", state.contents.label(), count_words(state.contents.items().len())))
                .weak(),
        )
        .truncate(),
    );
    for w in state.contents.warnings().iter().take(2) {
        ui.add(egui::Label::new(egui::RichText::new(w).color(ui.visuals().warn_fg_color).small()).wrap());
    }
    let mut filter = state.filter.clone();
    ui.horizontal(|ui| {
        ui.label("Find:");
        ui.add(
            widgets::text_field(ui.ctx(), &mut filter).hint_text("Name or path").desired_width(ui.available_width()),
        );
    });
    state.set_filter(filter);

    // 2. The tree takes most of the panel's height; the buttons and the
    //    selection's summary follow it.
    let tree_height = (ui.available_height() * 0.62).max(160.0);
    let rows = state.rows.len();
    let cursor_row = state.selection.cursor.and_then(|c| state.rows.iter().position(|&r| r == c));
    let scroll_to = state.scroll_to.take();
    let output = ui
        .allocate_ui(egui::vec2(ui.available_width(), tree_height), |ui| {
            let snapshot: &State = state;
            let selected = snapshot.selection.set.clone();
            widgets::tree(ui, "contents-tree", rows, cursor_row, scroll_to, |i| {
                let node = snapshot.rows[i];
                row_of(snapshot, node, selected.contains(&node))
            })
        })
        .inner;
    let events = output.events;
    let dragging = output.drag_started;
    let tree_focused = output.focused;
    state.tree_focused = tree_focused;
    state.hovered = output.hovered.and_then(|r| state.rows.get(r)).and_then(|&n| state.tree.nodes[n].item);
    thumbs::want(state, v.doc.as_ref(), &v.gfx.gpu, &waker, output.visible.clone());

    // 3. Apply what the user did.
    let mut shown_node = None;
    for event in events {
        match event {
            TreeEvent::Pick { row, extend, toggle } => {
                let Some(&node) = state.rows.get(row) else { continue };
                let rows_now = state.rows.clone();
                state.selection.pick(node, &rows_now, extend, toggle);
                state.scroll_to = Some(row);
                if !extend && !toggle {
                    shown_node = Some(node);
                }
            }
            TreeEvent::SetOpen { row, open } => {
                let Some(&node) = state.rows.get(row) else { continue };
                let path = state.tree.nodes[node].path.clone();
                if open {
                    state.open.insert(path);
                } else {
                    state.open.remove(&path);
                }
                state.refresh_rows();
            }
            TreeEvent::GoToParent(row) => {
                let Some(&node) = state.rows.get(row) else { continue };
                if let Some(p) = state.tree.nodes[node].parent {
                    let rows_now = state.rows.clone();
                    state.selection.pick(p, &rows_now, false, false);
                    state.scroll_to = state.rows.iter().position(|&r| r == p);
                    shown_node = Some(p);
                }
            }
            TreeEvent::Activate(row) => {
                let Some(&node) = state.rows.get(row) else { continue };
                if state.tree.is_folder(node) && state.tree.nodes[node].item.is_none() && !state.tree.nodes[node].map {
                    let path = state.tree.nodes[node].path.clone();
                    if !state.open.remove(&path) {
                        state.open.insert(path);
                    }
                    state.refresh_rows();
                } else {
                    shown_node = Some(node);
                }
            }
            TreeEvent::SelectAll => {
                let rows_now = state.rows.clone();
                state.selection.select_all(&rows_now);
            }
            TreeEvent::Context(row) => {
                if let Some(&node) = state.rows.get(row)
                    && !state.selection.set.contains(&node)
                {
                    let rows_now = state.rows.clone();
                    state.selection.pick(node, &rows_now, false, false);
                }
            }
        }
    }
    if let Some(row) = dragging
        && let Some(&node) = state.rows.get(row)
    {
        if !state.selection.set.contains(&node) {
            let rows_now = state.rows.clone();
            state.selection.pick(node, &rows_now, false, false);
        }
        state.drag = Some(state.selected_items());
    }
    shown = shown.or(shown_node);

    // 4. What is selected, the format, and the buttons.
    ui.add_space(4.0);
    let picked = state.selected_items();
    let summary = match (state.selection.set.len(), picked.len()) {
        (0, _) => "Nothing selected: Extract takes everything".to_string(),
        (1, _) => selection_line(state),
        (_, n) => format!("{} selected, {n} item{}", state.selection.set.len(), if n == 1 { "" } else { "s" }),
    };
    ui.add(egui::Label::new(egui::RichText::new(summary).weak()).wrap());
    ui.horizontal(|ui| {
        ui.label("Save as:");
        let items: Vec<(Format, String)> = Format::CHOICES
            .iter()
            .filter(|&&f| state.pieces() || !matches!(f, Format::Exr | Format::Glb))
            .map(|&f| (f, f.label().to_string()))
            .collect();
        widgets::Choice::new("contents-format", &mut state.format, items).width(190.0).show(ui);
    });
    if state.contents.items().iter().any(|i| i.formats.contains(&Format::Glb)) {
        ui.horizontal_wrapped(|ui| {
            ui.add(widgets::CheckBox::new(&mut state.centre, "Centre"))
                .on_hover_text("Move a saved node or mesh to the origin instead of keeping its place in the scene");
            ui.add(widgets::CheckBox::new(&mut state.skeleton_only, "Clips without meshes"))
                .on_hover_text("Save an animation with its skeleton alone, not the meshes it moves");
        });
    }
    ui.horizontal_wrapped(|ui| {
        let busy = state.working.is_some();
        let label = if state.selection.set.is_empty() { "Extract All…" } else { "Extract…" };
        if ui
            .add_enabled(!busy, egui::Button::new(label))
            .on_hover_text("Choose a folder to write the files to")
            .clicked()
        {
            asked.push(Do::Extract);
        }
        if ui
            .add_enabled(!busy && !state.selection.set.is_empty(), egui::Button::new("Copy"))
            .on_hover_text(crate::keys("Copy the selection as files (Ctrl+C), then paste in a file manager"))
            .clicked()
        {
            asked.push(Do::Copy);
        }
        let archive = state.contents.archive().filter(|_| state.contents.kind().is_none());
        if let Some(editor) = archive.and_then(|a| Editor::for_archive(a.kind()))
            && programs().contains_key(&editor)
            && ui
                .button(format!("Open in {}", editor.name()))
                .on_hover_text("Edit the archive in another program")
                .clicked()
        {
            asked.push(Do::OpenIn(editor));
        }
    });
    if tree_focused {
        ui.label(egui::RichText::new(crate::keys("Arrows move, Space or Enter shows, Ctrl+C copies")).weak().small());
    }

    // 5. Facts about the entry shown.
    if !state.facts.is_empty() {
        let title = if state.pieces() { "Item" } else { "Entry" };
        crate::sections::section(ui, "Entry", title, true, |ui| {
            egui::Grid::new("entry-facts").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
                for (k, val) in state.facts.iter().filter(|(k, _)| k != "Item") {
                    ui.label(egui::RichText::new(k).weak());
                    ui.add(egui::Label::new(val).wrap());
                    ui.end_row();
                }
            });
        });
    }

    for a in asked {
        v.actions.push(Action::Contents(a));
    }
    if let Some(node) = shown {
        show_node(v, node);
    }
}

/// "1 item", "5 items".
fn count_words(n: usize) -> String {
    if n == 1 { "1 item".to_string() } else { format!("{n} items") }
}

/// One line about a single selected node.
fn selection_line(state: &State) -> String {
    let Some(&node) = state.selection.set.iter().next() else { return String::new() };
    let n = &state.tree.nodes[node];
    match n.item.and_then(|id| state.contents.item(id)) {
        Some(item) if item.formats.is_empty() => format!("{} — listed only; it can't be saved alone", item.name()),
        Some(item) => {
            let formats: Vec<&str> = item.formats.iter().map(|f| f.name()).collect();
            format!("{} — can be saved as {}", item.name(), formats.join(", "))
        }
        None => format!("{}/ — {}", n.name, count_words(n.count)),
    }
}

/// The view's card for an entry that draws no picture.
pub fn card_view(ui: &mut Ui, rect: egui::Rect, state: &State) {
    let Some(c) = &state.card else { return };
    card(ui, rect, |ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(&c.title).text_style(heading2()));
            if let Some(p) = &c.problem {
                ui.label(egui::RichText::new(p).color(ui.visuals().warn_fg_color));
            }
            egui::Grid::new("card-facts").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                for (k, val) in c.facts.iter().filter(|(k, _)| k != "Item") {
                    ui.label(egui::RichText::new(k).weak());
                    ui.add(egui::Label::new(val).wrap());
                    ui.end_row();
                }
            });
            if let Some(text) = &c.text {
                ui.add_space(6.0);
                // A fixed width, so the card doesn't take the text's longest line.
                let width = (rect.width() * 0.7).clamp(240.0, 720.0);
                ui.set_min_width(width);
                egui::ScrollArea::both()
                    .max_height((rect.height() * 0.6).max(120.0))
                    .max_width(width)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).monospace())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            }
        });
    });
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_core::archive::testutil;

    /// Colour `i` is (i, 255 - i, i / 2).
    fn palette() -> Vec<u8> {
        (0..256u32).flat_map(|i| [i as u8, 255 - i as u8, (i / 2) as u8]).collect()
    }

    /// A WAD with a sprite namespace, two maps and a composite texture.
    fn wad() -> Arc<Contents> {
        let pic = testutil::doom_picture(2, 2, 0, 0, &[1, 2, 3, 4]);
        let pal = palette();
        let mut pnames = 1u32.to_le_bytes().to_vec();
        pnames.extend_from_slice(b"TROOA1\0\0");
        let mut tex = 1u32.to_le_bytes().to_vec();
        tex.extend_from_slice(&8u32.to_le_bytes());
        tex.extend_from_slice(b"WALL\0\0\0\0");
        tex.extend_from_slice(&0u32.to_le_bytes());
        tex.extend_from_slice(&2i16.to_le_bytes());
        tex.extend_from_slice(&2i16.to_le_bytes());
        tex.extend_from_slice(&0u32.to_le_bytes());
        tex.extend_from_slice(&1i16.to_le_bytes());
        for v in [0i16, 0, 0, 1, 0] {
            tex.extend_from_slice(&v.to_le_bytes());
        }
        let lumps: &[(&str, &[u8])] = &[
            ("PLAYPAL", &pal),
            ("PNAMES", &pnames),
            ("TEXTURE1", &tex),
            ("TITLEPIC", &pic),
            ("S_START", b""),
            ("TROOA1", &pic),
            ("S_END", b""),
            ("MAP01", b""),
            ("THINGS", &[0u8; 10]),
            ("LINEDEFS", &[0u8; 14]),
            ("MAP02", b""),
            ("THINGS", &[0u8; 10]),
        ];
        Arc::new(Contents::from_bytes(testutil::wad(true, lumps)).unwrap())
    }

    /// A state with every folder closed (small archives open fully).
    fn closed(contents: Arc<Contents>) -> State {
        let mut state = State::new(contents, "x.wad".into());
        state.open.clear();
        state.refresh_rows();
        state
    }

    fn names(state: &State) -> Vec<String> {
        state
            .rows()
            .iter()
            .map(|&n| format!("{}{}", "  ".repeat(state.tree.nodes[n].depth), state.tree.nodes[n].name))
            .collect()
    }

    fn node(state: &State, path: &str) -> usize {
        state.tree.at(path).unwrap_or_else(|| panic!("no node {path}"))
    }

    #[test]
    fn the_tree_follows_the_paths() {
        let c = wad();
        let mut state = closed(c);
        // A big enough archive opens at its top level only.
        assert_eq!(names(&state), ["PLAYPAL", "PNAMES", "TEXTURE1", "TITLEPIC", "Sprites", "MAP01", "MAP02"]);
        // TEXTURE1 is one node that is both a lump and a folder of textures.
        let t = node(&state, "TEXTURE1");
        assert!(state.tree.is_folder(t) && state.tree.nodes[t].item.is_some());
        assert_eq!(state.tree.nodes[t].count, 2);
        // The map folders are recognised; the namespace folder is not.
        assert!(state.tree.nodes[node(&state, "MAP01")].map && state.tree.nodes[node(&state, "MAP02")].map);
        assert!(!state.tree.nodes[node(&state, "Sprites")].map);
        assert_eq!(state.tree.nodes[node(&state, "MAP01")].count, 3);
        assert_eq!(state.tree.nodes[node(&state, "MAP01")].bytes, 24);
        // Opening a folder shows its children.
        state.open.insert("MAP01".into());
        state.open.insert("TEXTURE1".into());
        state.refresh_rows();
        assert_eq!(
            names(&state),
            [
                "PLAYPAL",
                "PNAMES",
                "TEXTURE1",
                "  WALL",
                "TITLEPIC",
                "Sprites",
                "MAP01",
                "  MAP01",
                "  THINGS",
                "  LINEDEFS",
                "MAP02"
            ]
        );
    }

    #[test]
    fn searching_shows_matches_and_their_folders() {
        let mut state = closed(wad());
        state.set_filter("things".into());
        assert_eq!(names(&state), ["MAP01", "  THINGS", "MAP02", "  THINGS"]);
        state.set_filter("troo".into());
        assert_eq!(names(&state), ["Sprites", "  TROOA1"]);
        state.set_filter("nothing".into());
        assert!(state.rows().is_empty());
        state.set_filter(String::new());
        assert_eq!(state.rows().len(), 7);
    }

    #[test]
    fn selection_follows_file_manager_rules() {
        let rows: Vec<usize> = (0..10).collect();
        let mut s = Selection::default();
        s.pick(3, &rows, false, false);
        assert_eq!((s.set.len(), s.cursor, s.anchor), (1, Some(3), Some(3)));
        // Shift selects from the anchor, and keeps it while extending.
        s.pick(6, &rows, true, false);
        assert_eq!(s.set, (3..=6).collect());
        s.pick(1, &rows, true, false);
        assert_eq!(s.set, (1..=3).collect());
        assert_eq!(s.anchor, Some(3));
        // Ctrl flips one, and moves the anchor.
        s.pick(8, &rows, false, true);
        assert_eq!(s.set, [1, 2, 3, 8].into_iter().collect());
        s.pick(2, &rows, false, true);
        assert_eq!(s.set, [1, 3, 8].into_iter().collect());
        // Ctrl+Shift adds a range.
        s.pick(5, &rows, true, true);
        assert!(s.set.is_superset(&[1, 3, 5, 6, 7, 8].into_iter().collect()) || s.set.contains(&5));
        // A plain click resets.
        s.pick(0, &rows, false, false);
        assert_eq!(s.set, [0].into_iter().collect());
        s.select_all(&rows);
        assert_eq!(s.set.len(), 10);
        s.keep_only(&[2, 3].into_iter().collect());
        assert_eq!(s.set, [2, 3].into_iter().collect());
        assert_eq!(s.cursor, None.or(s.cursor.filter(|c| [2, 3].contains(c))));
    }

    #[test]
    fn the_selection_stands_for_items_and_folders() {
        let c = wad();
        let mut state = closed(c.clone());
        state.open.extend(["MAP01".to_string(), "Sprites".to_string()]);
        state.refresh_rows();
        let rows = state.rows().to_vec();
        // A folder and something inside it count once, as the folder.
        let (map, things) = (node(&state, "MAP01"), node(&state, "MAP01/THINGS"));
        state.selection.pick(map, &rows, false, false);
        state.selection.pick(things, &rows, false, true);
        assert_eq!(state.selected(), [Selected::Folder("MAP01".into())]);
        assert_eq!(state.selected_items().len(), 3);
        // Items alone stay items, in tree order.
        let title = node(&state, "TITLEPIC");
        let troo = node(&state, "Sprites/TROOA1");
        state.selection.clear();
        state.selection.pick(troo, &rows, false, false);
        state.selection.pick(title, &rows, false, true);
        let ids: Vec<ItemId> = state
            .selected()
            .iter()
            .map(|s| match s {
                Selected::Item(id) => *id,
                other => panic!("{other:?}"),
            })
            .collect();
        let paths: Vec<&str> = ids.iter().map(|&i| c.item(i).unwrap().path.as_str()).collect();
        assert_eq!(paths, ["TITLEPIC", "Sprites/TROOA1"]);
        // A lump with textures beneath it is itself an item (the folder has no say).
        state.selection.clear();
        state.selection.pick(node(&state, "TEXTURE1"), &rows, false, false);
        assert!(matches!(state.selected()[..], [Selected::Item(_)]));
    }

    #[test]
    fn a_reload_keeps_the_selection_and_open_folders() {
        let c = wad();
        let mut state = closed(c.clone());
        state.open.insert("MAP02".into());
        state.refresh_rows();
        let rows = state.rows().to_vec();
        state.selection.pick(node(&state, "MAP02/THINGS"), &rows, false, false);
        state.selection.pick(node(&state, "TITLEPIC"), &rows, false, true);
        state.shown = c.key(state.tree.nodes[node(&state, "MAP02/THINGS")].item.unwrap());
        // The same archive read again, as after the file changed on disk.
        let again = wad();
        let shown = state.reload(again);
        assert_eq!(state.selection.set.len(), 2);
        assert!(state.selection.set.contains(&node(&state, "MAP02/THINGS")));
        assert!(state.selection.set.contains(&node(&state, "TITLEPIC")));
        assert_eq!(state.selection.cursor, Some(node(&state, "TITLEPIC")));
        assert!(names(&state).contains(&"  THINGS".to_string()));
        // The entry that was shown is found again by its key (the second
        // THINGS has one lump with its path before it under MAP02).
        assert_eq!(shown.map(|k| k.path), Some("MAP02/THINGS".to_string()));
        // A file that lost the selected entries drops them quietly.
        let smaller = Arc::new(Contents::from_bytes(testutil::wad(false, &[("ONLY", b"x")])).unwrap());
        assert_eq!(state.reload(smaller), None);
        assert!(state.selection.set.is_empty());
    }

    #[test]
    fn rows_say_what_to_draw() {
        let state = closed(wad());
        let row = |path: &str| row_of(&state, node(&state, path), false);
        let r = row("PLAYPAL");
        assert_eq!(
            (r.label.as_str(), r.kind.as_str(), r.detail.as_str(), r.folder),
            ("PLAYPAL", "palette", "768 bytes", None)
        );
        let r = row("MAP01");
        assert_eq!((r.kind.as_str(), r.detail.as_str(), r.folder), ("map", "3 items", Some(false)));
        let r = row("Sprites");
        assert_eq!((r.kind.as_str(), r.folder), ("", Some(false)));
        // Markers are dim, and composite textures say their size.
        let r = row("Sprites/S_START");
        assert!(r.dim, "{r:?}");
    }

    #[test]
    fn materialize_writes_files_for_a_drag_or_the_clipboard() {
        let c = wad();
        let runtime = std::env::temp_dir().join(format!("slerp-contents-view-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        // SAFETY: nothing else in this test binary reads the environment
        // while the first call below sets up the temporary folder.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime) };
        let auto = ExtractOptions::default();
        assert!(materialize(&[ItemId(0)], &auto).is_empty(), "no archive open yet");
        *CURRENT.lock().unwrap() = Some(c.clone());
        let ids: Vec<ItemId> = ["TITLEPIC", "Sprites/TROOA1", "PLAYPAL"]
            .iter()
            .map(|p| c.items().iter().find(|i| i.path == *p).unwrap().id)
            .collect();
        let paths = materialize(&ids, &auto);
        let shown: Vec<String> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(shown, ["TITLEPIC.png", "TROOA1.png", "PLAYPAL.lmp"]);
        // Compared as real paths: on Windows extraction reports them as
        // `\\?\C:\...`, which Explorer takes for drops and pastes alike.
        let root = std::fs::canonicalize(temp().unwrap().root()).unwrap();
        for p in &paths {
            assert!(p.is_file() && std::fs::canonicalize(p).unwrap().starts_with(&root), "{p:?}");
        }
        let png = std::fs::read(&paths[0]).unwrap();
        assert_eq!(&png[..4], b"\x89PNG");
        // As stored.
        let raw = materialize(&ids[..1], &ExtractOptions { format: Format::Raw, ..Default::default() });
        assert_eq!(raw[0].file_name().unwrap().to_string_lossy(), "TITLEPIC.lmp");
        // Two batches never share a folder.
        assert_ne!(raw[0].parent(), paths[0].parent());
        shutdown();
        assert!(!paths[0].exists());
        *CURRENT.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(runtime);
    }

    /// A scene: a root with a body (whose child, a hat, the file hides)
    /// and a lamp beside it, each with a mesh.
    fn scene() -> slerp_scene::Scene {
        use slerp_scene::{Mesh, Node, Primitive};
        let mesh = |name: &str| Mesh {
            name: name.into(),
            primitives: vec![Primitive {
                positions: vec![glam::Vec3::ZERO, glam::Vec3::X, glam::Vec3::Y],
                indices: vec![0, 1, 2],
                ..Default::default()
            }],
            target_names: Vec::new(),
        };
        let mut root = Node::new("root");
        root.children = vec![1, 3];
        let mut body = Node { mesh: Some(0), ..Node::new("body") };
        body.children = vec![2];
        let hat = Node { mesh: Some(1), visible: false, ..Node::new("hat") };
        let lamp = Node { mesh: Some(2), ..Node::new("lamp") };
        slerp_scene::Scene {
            nodes: vec![root, body, hat, lamp],
            roots: vec![0],
            meshes: vec![mesh("body"), mesh("hat"), mesh("lamp")],
            ..Default::default()
        }
    }

    fn pieces() -> Arc<Contents> {
        let kind = slerp_core::Kind::Gltf;
        let asset = slerp_scene::Asset::Scene(scene());
        Arc::new(Contents::of_loaded(kind, Path::new("/nonexistent/x.glb"), &asset, &[], None).unwrap())
    }

    #[test]
    fn showing_a_node_alone_keeps_its_parents_and_the_files_own_hiding() {
        let s = scene();
        let before: Vec<bool> = s.nodes.iter().map(|n| n.visible).collect();
        // The body with what is beneath it: its parent shows, the lamp
        // doesn't, and the hat stays hidden as the file has it.
        assert_eq!(isolation(&s, &before, &[1], true), [true, true, false, false]);
        // The hat alone (a mesh): shown although the file hides it, with the
        // nodes above it.
        assert_eq!(isolation(&s, &before, &[2], false), [true, true, true, false]);
        // Several nodes, and indices a changed file no longer has.
        assert_eq!(isolation(&s, &before, &[3, 99], false), [true, false, false, true]);
    }

    #[test]
    fn a_files_pieces_open_on_their_groups() {
        let state = State::new(pieces(), "/nonexistent/x.glb".into());
        assert!(state.pieces());
        let shown: Vec<String> = names(&state);
        assert_eq!(&shown[..6], ["Whole scene", "Nodes", "  root", "    body", "    lamp", "Meshes"], "{shown:#?}");
        // A node with children is a folder that is also an item.
        let body = node(&state, "Nodes/root/body");
        assert!(state.tree.is_folder(body) && state.tree.nodes[body].item.is_some());
        // Pieces show in the file itself; a texture would be a preview.
        let id = state.tree.nodes[body].item.unwrap();
        assert_eq!(state.contents.show(id), Show::Nodes { nodes: vec![1], subtree: true });
        let facts = piece_facts(&state.contents, id);
        assert!(facts.iter().any(|(k, v)| k == "Saves as" && v == "GLB model"), "{facts:?}");
        // The rows give a mesh's size as its detail, not a byte count.
        let r = row_of(&state, node(&state, "Meshes/lamp"), false);
        assert_eq!((r.kind.as_str(), r.detail.as_str()), ("mesh", "1 triangle"));
    }

    #[test]
    fn the_panels_choices_make_the_save_options() {
        let c = pieces();
        let mut state = State::new(c.clone(), "/nonexistent/x.glb".into());
        let saveable = c.items().iter().find(|i| i.path == "Meshes/lamp").unwrap().id;
        state.drag = Some(vec![saveable]);
        state.format = Format::Glb;
        state.centre = true;
        assert_eq!(state.options().format, Format::Glb);
        assert!(state.options().centre);
        // Thumbnails are asked for pictures and for what is drawn alone.
        assert!(thumbs::has_thumb(ItemKind::Mip) && thumbs::has_thumb(ItemKind::Mesh));
        assert!(!thumbs::has_thumb(ItemKind::Animation) && !thumbs::has_thumb(ItemKind::Material));
    }

    #[test]
    fn editors_are_found_where_they_install() {
        let root = std::env::temp_dir().join(format!("slerp-editors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bin, flat, apps, mac) =
            (root.join("bin"), root.join("flatpak"), root.join("Applications"), root.join("mac"));
        for d in [&bin, &flat, &apps, &mac] {
            std::fs::create_dir_all(d).unwrap();
        }
        let places = SearchPlaces {
            path: vec![bin.clone()],
            extra: Vec::new(),
            flatpak_dirs: vec![flat.clone()],
            appimage_dirs: vec![apps.clone()],
            mac_apps: vec![mac.clone()],
        };
        // Nothing installed.
        assert_eq!(find_program(Editor::Slade, &places), None);
        assert_eq!(find_program(Editor::VpkEdit, &places), None);
        // A Flatpak, then an AppImage for VPKEdit, which has no Flatpak.
        std::fs::create_dir_all(flat.join("net.mancubus.SLADE")).unwrap();
        assert_eq!(find_program(Editor::Slade, &places), Some(Program::Flatpak("net.mancubus.SLADE")));
        std::fs::create_dir_all(flat.join("com.craftablescience.VPKEdit")).unwrap();
        assert_eq!(find_program(Editor::VpkEdit, &places), None);
        std::fs::write(apps.join("VPKEdit-4.0-x86_64.AppImage"), b"").unwrap();
        assert_eq!(
            find_program(Editor::VpkEdit, &places),
            Some(Program::Executable(apps.join("VPKEdit-4.0-x86_64.AppImage")))
        );
        // The PATH wins over the Flatpak.
        std::fs::write(bin.join("slade"), b"").unwrap();
        assert_eq!(find_program(Editor::Slade, &places), Some(Program::Executable(bin.join("slade"))));
        // A Mac bundle.
        std::fs::remove_file(apps.join("VPKEdit-4.0-x86_64.AppImage")).unwrap();
        std::fs::create_dir_all(mac.join("VPKEdit.app")).unwrap();
        assert_eq!(find_program(Editor::VpkEdit, &places), Some(Program::MacApp(mac.join("VPKEdit.app"))));
        // The commands that start them.
        let file = Path::new("/tmp/a.wad");
        let args = |c: &std::process::Command| -> Vec<String> {
            std::iter::once(c.get_program()).chain(c.get_args()).map(|a| a.to_string_lossy().into_owned()).collect()
        };
        assert_eq!(
            args(&command(&Program::Flatpak("net.mancubus.SLADE"), file)),
            ["flatpak", "run", "net.mancubus.SLADE", "/tmp/a.wad"]
        );
        assert_eq!(
            args(&command(&Program::MacApp("/Applications/SLADE.app".into()), file)),
            ["open", "-a", "/Applications/SLADE.app", "/tmp/a.wad"]
        );
        assert_eq!(
            args(&command(&Program::Executable("/usr/bin/slade".into()), file)),
            ["/usr/bin/slade", "/tmp/a.wad"]
        );
        // Which editor suits which archive.
        use slerp_core::archive::ArchiveKind;
        for (kind, editor) in [
            (ArchiveKind::DoomWad, Editor::Slade),
            (ArchiveKind::Zip, Editor::Slade),
            (ArchiveKind::SevenZip, Editor::Slade),
            (ArchiveKind::Pak, Editor::VpkEdit),
            (ArchiveKind::Pck, Editor::VpkEdit),
            (ArchiveKind::Vpk, Editor::VpkEdit),
            (ArchiveKind::Grp, Editor::VpkEdit),
        ] {
            assert_eq!(Editor::for_archive(kind), Some(editor), "{kind:?}");
        }
        assert_eq!(Editor::for_archive(ArchiveKind::UnityPackage), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
