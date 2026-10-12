//! The window's layout: the view and the side panels as tiles of an
//! `egui_tiles` tree.  The panels are tabs that can be dragged by their
//! titles and dropped beside, above or below the view or each other, or
//! into another panel's tab bar; the borders between tiles drag to resize.
//! By default the three panels share one tab bar on the left, with
//! Controls (camera, display, lighting and whatever the file offers) in
//! front, so the view keeps as much of the window as it can.
//!
//! The view itself never gets a tab bar and can't be dragged: a panel
//! dropped onto the middle of it docks to its right instead of becoming a
//! tab beside it, which would let the view be hidden behind a panel.  When
//! the window is resized, the panels in the view's row keep their widths
//! and the view takes the difference.  The layout is saved with the rest of
//! the viewer's state; a saved layout that lacks a pane (from a damaged
//! file, say) gives way to the default one.

use crate::app::Viewer;
use egui::Ui;
use egui_tiles::{
    Behavior, Container, LinearDir, SimplificationOptions, TabState, Tile, TileId, Tiles, Tree, UiResponse,
};
use serde::{Deserialize, Serialize};
use slerp_theme::{Palette, widgets};
use std::sync::Arc;

/// One tile's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    /// The 3D view, picture or page: whatever the file shows.
    View,
    /// Camera, display, lighting, animation, material and the other
    /// controls for what is open.
    Controls,
    /// Facts about the file: its format, sizes, statistics.
    Inspector,
    /// The pieces inside the file; hidden while the file has none.
    Contents,
}

impl Pane {
    const ALL: [Pane; 4] = [Pane::View, Pane::Controls, Pane::Inspector, Pane::Contents];

    fn title(self) -> &'static str {
        match self {
            Pane::View => "View",
            Pane::Controls => "Controls",
            Pane::Inspector => "Inspector",
            Pane::Contents => "Contents",
        }
    }
}

/// The side panels' starting width, in points.
const SIDE_WIDTH: f32 = 345.0;

/// The layout of the view and the side panels.
pub struct Dock {
    tree: Tree<Pane>,
    /// The root row's tiles at the last frame, to tell which arrived since.
    row: Vec<TileId>,
}

impl Dock {
    /// The saved layout, or the default when there is none or it is
    /// incomplete.
    pub fn new(saved: Option<Tree<Pane>>) -> Dock {
        let tree = saved.filter(complete).unwrap_or_else(default_tree);
        let row = root_row(&tree);
        Dock { tree, row }
    }

    /// Puts the panels back where they start.
    pub fn reset(&mut self) {
        *self = Dock::new(None);
    }
}

/// The root's children, when the root is a row.
fn root_row(tree: &Tree<Pane>) -> Vec<TileId> {
    match tree.root.and_then(|r| tree.tiles.get(r)) {
        Some(Tile::Container(Container::Linear(row))) if row.dir == LinearDir::Horizontal => row.children.clone(),
        _ => Vec::new(),
    }
}

/// The three panels in one tab bar on the left of the view, Controls in
/// front.
fn default_tree() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let view = tiles.insert_pane(Pane::View);
    let panels: Vec<TileId> = [Pane::Controls, Pane::Inspector, Pane::Contents].map(|p| tiles.insert_pane(p)).into();
    let tabs = tiles.insert_tab_tile(panels);
    let row = tiles.insert_horizontal_tile(vec![tabs, view]);
    if let Some(Tile::Container(Container::Linear(linear))) = tiles.get_mut(row) {
        linear.shares.set_share(view, 1000.0);
        linear.shares.set_share(tabs, SIDE_WIDTH);
    }
    Tree::new("slerp-panels", row, tiles)
}

/// Whether every pane is in the tree exactly once, reachable from the root.
fn complete(tree: &Tree<Pane>) -> bool {
    let mut seen = Vec::new();
    let mut stack: Vec<TileId> = tree.root.into_iter().collect();
    while let Some(id) = stack.pop() {
        match tree.tiles.get(id) {
            Some(Tile::Pane(p)) => seen.push(*p),
            Some(Tile::Container(c)) => stack.extend(c.children()),
            None => return false,
        }
    }
    Pane::ALL.iter().all(|p| seen.iter().filter(|s| *s == p).count() == 1) && seen.len() == Pane::ALL.len()
}

/// Shows the view and the panels in what is left of the window, and
/// returns whether the view needs another frame.
pub fn show(v: &mut Viewer, ui: &mut Ui, dt: f32) -> bool {
    // 1. The tree is taken out of the viewer while its panes borrow it.
    let mut tree = std::mem::replace(&mut v.dock.tree, Tree::empty("slerp-panels-taken"));
    let before = std::mem::take(&mut v.dock.row);
    set_visible(&mut tree, Pane::Contents, v.contents.is_some());
    keep_widths(&mut tree, ui.available_width());

    let mut panes = Panes { v, dt, moving: false, dropped: false, p: slerp_theme::palette(ui.ctx()) };
    tree.ui(&mut panes, ui);
    let (moving, dropped) = (panes.moving, panes.dropped);

    // 2. What a drop may have made is put right before the next frame,
    //    and a panel newly docked in the root row starts at the panels'
    //    width.  egui_tiles joins a dropped tile's row into the root row
    //    at its next frame, handing the newcomer half of the tile it was
    //    dropped beside, so the join is made here, and any panel left
    //    wider than two panels by a drop is narrowed too.
    tree.simplify(&simplification());
    tidy(&mut tree);
    let row = root_row(&tree);
    let view = tree.root.and_then(|r| view_branch(&tree, r));
    if let Some(Tile::Container(Container::Linear(line))) = tree.root.and_then(|r| tree.tiles.get_mut(r)) {
        for &c in row.iter().filter(|&&c| Some(c) != view) {
            if !before.contains(&c) || (dropped && line.shares[c] > 2.0 * SIDE_WIDTH) {
                line.shares.set_share(c, SIDE_WIDTH);
            }
        }
    }
    v.dock.row = row;

    // 3. Saved once a drag or resize ends.
    if ui.input(|i| i.pointer.any_released()) && v.saved.layout.as_ref() != Some(&tree) {
        v.saved.layout = Some(tree.clone());
        v.saved.save();
    }
    v.dock.tree = tree;
    moving
}

/// Shows or hides a pane, and with it any container that would be left
/// with nothing to show (an empty tab bar).
fn set_visible(tree: &mut Tree<Pane>, pane: Pane, visible: bool) {
    let Some(id) = tree.tiles.find_pane(&pane) else { return };
    if tree.tiles.is_visible(id) == visible {
        return;
    }
    tree.tiles.set_visible(id, visible);
    let mut child = id;
    while let Some(parent) = tree.tiles.parent_of(child) {
        if tree.root == Some(parent) {
            break;
        }
        let any = match tree.tiles.get(parent) {
            Some(Tile::Container(c)) => c.children().any(|&c| tree.tiles.is_visible(c)),
            _ => true,
        };
        tree.tiles.set_visible(parent, any);
        child = parent;
    }
}

/// The child of `container` that is or holds the view.
fn view_branch(tree: &Tree<Pane>, container: TileId) -> Option<TileId> {
    let mut id = tree.tiles.find_pane(&Pane::View)?;
    loop {
        let parent = tree.tiles.parent_of(id)?;
        if parent == container {
            return Some(id);
        }
        id = parent;
    }
}

/// Keeps the panels beside the view at their widths: when the root is a
/// row, its shares are kept in points that add up to the window's width,
/// the view's branch taking whatever the others leave.  So resizing the
/// window resizes only the view, and a border dragged between two tiles
/// (which moves share from one to the other) stays where it was put.
fn keep_widths(tree: &mut Tree<Pane>, width: f32) {
    let Some(root) = tree.root else { return };
    let Some(view) = view_branch(tree, root) else { return };
    let others: Vec<TileId> = match tree.tiles.get(root) {
        Some(Tile::Container(Container::Linear(row))) if row.dir == LinearDir::Horizontal => {
            row.children.iter().copied().filter(|&c| c != view && tree.tiles.is_visible(c)).collect()
        }
        _ => return,
    };
    let Some(Tile::Container(Container::Linear(row))) = tree.tiles.get_mut(root) else { return };
    let taken: f32 = others.iter().map(|&c| row.shares[c]).sum();
    let gaps = Panes::GAP * others.len() as f32;
    row.shares.set_share(view, (width - gaps - taken).max(Panes::MIN_SIZE));
}

/// Puts right what dropping a tab or a tab bar can make, so that every
/// panel keeps a tab to drag it by and the view never gets one:
///
/// 1. A panel dropped onto the middle of the view wraps both in a tab
///    bar; the view moves out of it, to its left.
/// 2. A tab bar dropped into another tab bar is merged into it.
/// 3. A panel dropped beside a tile arrives without a tab bar; it gets one.
/// 4. A tile new to a row or column gets a share fit for a panel: the
///    side panels' width in the root row, whose shares are points, a third
///    of the view's branch beside the view, an even share elsewhere.
fn tidy(tree: &mut Tree<Pane>) {
    // 1. The view out of tabs.
    if let Some(view) = tree.tiles.find_pane(&Pane::View)
        && let Some(tabs) = tree.tiles.parent_of(view)
        && let Some(Tile::Container(Container::Tabs(t))) = tree.tiles.get_mut(tabs)
    {
        t.children.retain(|&c| c != view);
        t.active = t.children.first().copied();
        let outer = tree.tiles.parent_of(tabs);
        match outer.and_then(|o| tree.tiles.get_mut(o)) {
            Some(Tile::Container(Container::Linear(row))) if row.dir == LinearDir::Horizontal => {
                let at = row.children.iter().position(|&c| c == tabs).unwrap_or(0);
                row.children.insert(at, view);
                // The view keeps the room it had; the panel gets a share in
                // step 4.
                let share = row.shares[tabs];
                row.shares.set_share(view, share);
                row.shares.retain(|c| c != tabs);
            }
            _ => {
                let row = tree.tiles.insert_horizontal_tile(vec![view, tabs]);
                replace_child(tree, outer, tabs, row);
            }
        }
    }

    // 2. Tabs in tabs merged.
    let nested: Vec<(TileId, TileId)> = tree
        .tiles
        .iter()
        .filter_map(|(&id, tile)| match tile {
            Tile::Container(Container::Tabs(t)) => t
                .children
                .iter()
                .find(|&&c| matches!(tree.tiles.get(c), Some(Tile::Container(Container::Tabs(_)))))
                .map(|&inner| (id, inner)),
            _ => None,
        })
        .collect();
    for (outer, inner) in nested {
        let Some(Tile::Container(Container::Tabs(inner_tabs))) = tree.tiles.remove(inner) else { continue };
        if let Some(Tile::Container(Container::Tabs(t))) = tree.tiles.get_mut(outer)
            && let Some(at) = t.children.iter().position(|&c| c == inner)
        {
            t.children.splice(at..=at, inner_tabs.children.iter().copied());
            if t.active == Some(inner) || t.active.is_none() {
                t.active = inner_tabs.active.or_else(|| t.children.first().copied());
            }
        }
    }

    // 3. Every panel in a tab bar.
    for pane in [Pane::Controls, Pane::Inspector, Pane::Contents] {
        let Some(id) = tree.tiles.find_pane(&pane) else { continue };
        let parent = tree.tiles.parent_of(id);
        if matches!(parent.and_then(|p| tree.tiles.get(p)), Some(Tile::Container(Container::Tabs(_)))) {
            continue;
        }
        let visible = tree.tiles.is_visible(id);
        let tabs = tree.tiles.insert_tab_tile(vec![id]);
        tree.tiles.set_visible(tabs, visible);
        replace_child(tree, parent, id, tabs);
    }

    // 4. Shares for new tiles.
    let root = tree.root;
    let lines: Vec<TileId> = tree
        .tiles
        .iter()
        .filter(|(_, tile)| matches!(tile, Tile::Container(Container::Linear(_))))
        .map(|(&id, _)| id)
        .collect();
    for id in lines {
        let view = view_branch(tree, id);
        let Some(Tile::Container(Container::Linear(line))) = tree.tiles.get_mut(id) else { continue };
        let known: Vec<(TileId, f32)> = line.shares.iter().map(|(&c, &s)| (c, s)).collect();
        let has = |c: TileId| known.iter().any(|&(k, _)| k == c);
        let missing: Vec<TileId> = line.children.iter().copied().filter(|&c| !has(c)).collect();
        if missing.is_empty() {
            continue;
        }
        let present: Vec<f32> = line.children.iter().filter(|&&c| has(c)).map(|&c| line.shares[c]).collect();
        let even = if present.is_empty() { 1.0 } else { present.iter().sum::<f32>() / present.len() as f32 };
        for c in missing {
            let share = match view {
                Some(v) if v == c => even,
                _ if root == Some(id) && line.dir == LinearDir::Horizontal => SIDE_WIDTH,
                Some(v) if has(v) => line.shares[v] / 3.0,
                _ => even,
            };
            line.shares.set_share(c, share);
        }
    }
}

/// Puts `new` where `old` was among `parent`'s children, keeping its
/// share, or makes it the root.  `parent` is `old`'s parent from before
/// `new` was made (once `new` holds `old`, `old` has two).
fn replace_child(tree: &mut Tree<Pane>, parent: Option<TileId>, old: TileId, new: TileId) {
    match parent.and_then(|p| tree.tiles.get_mut(p)) {
        Some(Tile::Container(Container::Linear(line))) => {
            for c in &mut line.children {
                if *c == old {
                    *c = new;
                }
            }
            line.shares.replace_with(old, new);
        }
        Some(Tile::Container(Container::Tabs(tabs))) => {
            for c in &mut tabs.children {
                if *c == old {
                    *c = new;
                }
            }
            if tabs.active == Some(old) {
                tabs.active = Some(new);
            }
        }
        // The panels never make grids.
        Some(_) => *tree = default_tree(),
        None => tree.root = Some(new),
    }
}

/// How egui_tiles tidies the tree: a panel alone keeps its tab, so it can
/// still be dragged by its title; the view never has one (`tidy`).
fn simplification() -> SimplificationOptions {
    SimplificationOptions { prune_single_child_tabs: false, all_panes_must_have_tabs: false, ..Default::default() }
}

/*┌─────────────────╮
| Panes           |
└─────────────────╯*/
//region Panes

/// Draws the panes for `egui_tiles`, in the desktop theme's colours.
struct Panes<'a> {
    v: &'a mut Viewer,
    dt: f32,
    /// Whether the view needs another frame.
    moving: bool,
    /// Whether a tab or tab bar was dropped this frame.
    dropped: bool,
    p: Arc<Palette>,
}

impl Panes<'_> {
    const GAP: f32 = 1.0;
    const MIN_SIZE: f32 = 160.0;
}

impl Behavior<Pane> for Panes<'_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile_id: TileId, pane: &mut Pane) -> UiResponse {
        if *pane == Pane::View {
            self.moving |= self.v.view_ui(ui, self.dt);
            return UiResponse::None;
        }
        // A side panel: window colour with Breeze's child margin, scrolling
        // when its sections are taller than the window.
        let frame = widgets::side_panel_frame(ui.ctx());
        ui.painter().rect_filled(ui.max_rect(), 0.0, frame.fill);
        frame.show(ui, |ui| {
            let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
            match pane {
                Pane::Controls => {
                    scroll.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        crate::panels::controls_contents(self.v, ui);
                    });
                }
                Pane::Inspector => {
                    scroll.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        crate::panels::inspector_contents(self.v, ui);
                    });
                }
                // The tree has its own scrolling; what is below it (the
                // buttons and the entry's facts) scrolls with the panel.
                Pane::Contents => {
                    scroll.show(ui, |ui| crate::app::contents_view::contents(self.v, ui));
                }
                Pane::View => {}
            }
        });
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    /// Names the tab for screen readers, with whether it is the one shown.
    fn on_tab_button(&mut self, tiles: &mut Tiles<Pane>, tile_id: TileId, response: egui::Response) -> egui::Response {
        let shown = match tiles.parent_of(tile_id).and_then(|t| tiles.get(t)) {
            Some(Tile::Container(Container::Tabs(tabs))) => tabs.is_active(tile_id),
            _ => true,
        };
        let title = tiles.get_pane(&tile_id).map_or("", |p| p.title());
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, shown, title));
        response
    }

    fn is_tile_draggable(&self, tiles: &Tiles<Pane>, tile_id: TileId) -> bool {
        tiles.get_pane(&tile_id) != Some(&Pane::View)
    }

    fn simplification_options(&self) -> SimplificationOptions {
        simplification()
    }

    fn on_edit(&mut self, action: egui_tiles::EditAction) {
        self.dropped |= action == egui_tiles::EditAction::TileDropped;
    }

    fn tab_bar_height(&self, style: &egui::Style) -> f32 {
        style.spacing.interact_size.y + 8.0
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        Self::GAP
    }

    fn min_size(&self) -> f32 {
        Self::MIN_SIZE
    }

    fn tab_title_spacing(&self, _visuals: &egui::Visuals) -> f32 {
        12.0
    }

    fn tab_bar_color(&self, _visuals: &egui::Visuals) -> egui::Color32 {
        self.p.header
    }

    fn tab_bg_color(
        &self,
        _visuals: &egui::Visuals,
        _tiles: &Tiles<Pane>,
        _id: TileId,
        state: &TabState,
    ) -> egui::Color32 {
        if state.active { self.p.window } else { egui::Color32::TRANSPARENT }
    }

    fn tab_outline_stroke(
        &self,
        _visuals: &egui::Visuals,
        _tiles: &Tiles<Pane>,
        _id: TileId,
        state: &TabState,
    ) -> egui::Stroke {
        if state.active { egui::Stroke::new(1.0, self.p.separator) } else { egui::Stroke::NONE }
    }

    fn tab_bar_hline_stroke(&self, _visuals: &egui::Visuals) -> egui::Stroke {
        egui::Stroke::new(1.0, self.p.separator)
    }

    fn tab_text_color(
        &self,
        _visuals: &egui::Visuals,
        _tiles: &Tiles<Pane>,
        _id: TileId,
        state: &TabState,
    ) -> egui::Color32 {
        if state.active { self.p.window_text } else { self.p.inactive_text }
    }

    /// The borders between tiles: a separator line, in the focus colour
    /// while the pointer is on one or drags it.
    fn resize_stroke(&self, _style: &egui::Style, resize_state: egui_tiles::ResizeState) -> egui::Stroke {
        match resize_state {
            egui_tiles::ResizeState::Idle => egui::Stroke::new(Self::GAP, self.p.separator),
            _ => egui::Stroke::new(2.0, self.p.focus),
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn parent_kind(tree: &Tree<Pane>, pane: Pane) -> Option<egui_tiles::ContainerKind> {
        let id = tree.tiles.find_pane(&pane)?;
        tree.tiles.get(tree.tiles.parent_of(id)?)?.kind()
    }

    /// A row of the view and one tab bar holding `tabbed`, with the other
    /// panels in a second tab bar.
    fn row_with(first: Vec<Pane>, second: Vec<Pane>) -> Tree<Pane> {
        let mut tiles = Tiles::default();
        let a: Vec<TileId> = first.into_iter().map(|p| tiles.insert_pane(p)).collect();
        let b: Vec<TileId> = second.into_iter().map(|p| tiles.insert_pane(p)).collect();
        let a = tiles.insert_tab_tile(a);
        let b = tiles.insert_tab_tile(b);
        let row = tiles.insert_horizontal_tile(vec![a, b]);
        Tree::new("test", row, tiles)
    }

    #[test]
    fn the_default_has_every_pane_with_the_view_bare() {
        let tree = default_tree();
        assert!(complete(&tree));
        assert_eq!(parent_kind(&tree, Pane::View), Some(egui_tiles::ContainerKind::Horizontal));
        assert_eq!(parent_kind(&tree, Pane::Controls), Some(egui_tiles::ContainerKind::Tabs));
        let tabs = tree.tiles.parent_of(tree.tiles.find_pane(&Pane::Controls).unwrap()).unwrap();
        let Some(Tile::Container(Container::Tabs(t))) = tree.tiles.get(tabs) else { panic!() };
        assert_eq!(t.active, tree.tiles.find_pane(&Pane::Controls), "Controls in front");
        let Some(Tile::Container(Container::Linear(row))) = tree.tiles.get(tree.root.unwrap()) else { panic!() };
        assert_eq!(row.children, vec![tabs, tree.tiles.find_pane(&Pane::View).unwrap()], "panels on the left");
    }

    #[test]
    fn a_layout_missing_a_pane_is_not_complete() {
        let tree = row_with(vec![Pane::View], vec![Pane::Controls, Pane::Inspector]);
        assert!(!complete(&tree));
        assert!(complete(&row_with(vec![Pane::View, Pane::Contents], vec![Pane::Controls, Pane::Inspector])));
    }

    #[test]
    fn a_panel_dropped_on_the_view_docks_beside_it() {
        let mut tree = row_with(vec![Pane::View, Pane::Inspector], vec![Pane::Controls, Pane::Contents]);
        tidy(&mut tree);
        assert!(complete(&tree));
        assert_eq!(parent_kind(&tree, Pane::View), Some(egui_tiles::ContainerKind::Horizontal));
        assert_eq!(parent_kind(&tree, Pane::Inspector), Some(egui_tiles::ContainerKind::Tabs));
        // The view is left of the panel that was dropped on it.
        let root = tree.root.unwrap();
        let Some(Tile::Container(Container::Linear(row))) = tree.tiles.get(root) else { panic!() };
        let view = tree.tiles.find_pane(&Pane::View).unwrap();
        let inspector_tabs = tree.tiles.parent_of(tree.tiles.find_pane(&Pane::Inspector).unwrap()).unwrap();
        let at = |id| row.children.iter().position(|&c| c == id).unwrap();
        assert!(at(view) < at(inspector_tabs));
        assert_eq!(row.shares[inspector_tabs], SIDE_WIDTH, "a new panel in the root row gets the panels' width");
    }

    #[test]
    fn a_panel_dropped_beside_a_tile_gets_a_tab_bar() {
        let mut tiles = Tiles::default();
        let view = tiles.insert_pane(Pane::View);
        let contents = tiles.insert_pane(Pane::Contents);
        let others = [Pane::Controls, Pane::Inspector].map(|p| tiles.insert_pane(p)).into();
        let tabs = tiles.insert_tab_tile(others);
        let row = tiles.insert_horizontal_tile(vec![contents, view, tabs]);
        let mut tree = Tree::new("test", row, tiles);
        tidy(&mut tree);
        assert!(complete(&tree));
        assert_eq!(parent_kind(&tree, Pane::Contents), Some(egui_tiles::ContainerKind::Tabs));
        assert_eq!(parent_kind(&tree, Pane::View), Some(egui_tiles::ContainerKind::Horizontal));
    }

    #[test]
    fn a_tab_bar_dropped_into_another_is_merged() {
        let mut tiles = Tiles::default();
        let view = tiles.insert_pane(Pane::View);
        let inner: Vec<TileId> = [Pane::Controls, Pane::Inspector].map(|p| tiles.insert_pane(p)).into();
        let inner = tiles.insert_tab_tile(inner);
        let contents = tiles.insert_pane(Pane::Contents);
        let outer = tiles.insert_tab_tile(vec![contents, inner]);
        let row = tiles.insert_horizontal_tile(vec![view, outer]);
        let mut tree = Tree::new("test", row, tiles);
        tidy(&mut tree);
        assert!(complete(&tree));
        let Some(Tile::Container(Container::Tabs(t))) = tree.tiles.get(outer) else { panic!() };
        let names: Vec<Pane> = t.children.iter().map(|c| *tree.tiles.get_pane(c).unwrap()).collect();
        assert_eq!(names, [Pane::Contents, Pane::Controls, Pane::Inspector]);
        assert!(tree.tiles.get(inner).is_none());
    }

    #[test]
    fn the_view_takes_what_the_panels_leave() {
        let mut tree = default_tree();
        keep_widths(&mut tree, 1600.0);
        let root = tree.root.unwrap();
        let view = tree.tiles.find_pane(&Pane::View).unwrap();
        let Some(Tile::Container(Container::Linear(row))) = tree.tiles.get(root) else { panic!() };
        assert_eq!(row.shares[view], 1600.0 - SIDE_WIDTH - Panes::GAP);
        // A hidden panel leaves its room to the view.
        let mut tree = row_with(vec![Pane::View], vec![Pane::Contents]);
        set_visible(&mut tree, Pane::Contents, false);
        let tabs = tree.tiles.parent_of(tree.tiles.find_pane(&Pane::Contents).unwrap()).unwrap();
        assert!(!tree.tiles.is_visible(tabs), "an empty tab bar hides too");
    }
}
