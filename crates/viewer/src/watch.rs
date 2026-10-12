//! Watch mode: noticing when the open file, or a file it loaded from
//! beside it (the loader's `Info::neighbours`: a glTF's `.bin` and
//! textures, an OBJ's `.mtl`, a map's WADs), changes on disk, and the slim
//! bar over the view that offers to reload it.
//!
//! Folders are watched rather than files, through the system's own
//! notifications (inotify on Linux, FSEvents on macOS,
//! ReadDirectoryChangesW on Windows), because editors and exporters often
//! save by writing a new file and renaming it over the old one, which a
//! watch on the old file itself would lose.  Nothing polls, so an idle
//! window watching a file uses no CPU.
//!
//! Exporters write in pieces (a glTF's JSON, then its `.bin`, then its
//! textures; a large GLB in many chunks), so a change only counts once the
//! files have been quiet for [`QUIET`].  Notifications also come for
//! reading a file, changing its permissions or touching another file in
//! the same folder, so a quiet burst is then confirmed by comparing each
//! file's size, modification time and identity with what was seen before.

use crate::loader::{Waker, wake};
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// How long the files must stay unchanged before a change counts.
pub const QUIET: Duration = Duration::from_millis(300);

/// How long the "Reloaded" note shows, the last part of it fading out.
const RELOADED_SHOWS: Duration = Duration::from_millis(2000);
const RELOADED_FADES: Duration = Duration::from_millis(500);

/*┌─────────────────╮
| Debouncing      |
└─────────────────╯*/
//region Debouncing

/// Waits for a burst of changes to end: each change restarts the quiet
/// period, and the burst is over once a whole period passes without one.
#[derive(Clone, Debug)]
pub struct Debounce {
    quiet: Duration,
    last: Option<Instant>,
}

impl Debounce {
    pub fn new(quiet: Duration) -> Debounce {
        Debounce { quiet, last: None }
    }

    /// Notes a change at `at`.
    pub fn change(&mut self, at: Instant) {
        self.last = Some(self.last.map_or(at, |l| l.max(at)));
    }

    /// How long until the burst counts as over: `None` when there is none,
    /// zero when it is over.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.last.map(|l| (l + self.quiet).saturating_duration_since(now))
    }

    /// Ends the burst if it has been quiet long enough, saying whether it
    /// did.
    pub fn settle(&mut self, now: Instant) -> bool {
        let over = self.wait(now) == Some(Duration::ZERO);
        if over {
            self.last = None;
        }
        over
    }
}

//endregion

/*┌─────────────────╮
| Watching        |
└─────────────────╯*/
//region Watching

/// What a file looked like: enough to tell a real change from a
/// notification about reading it or changing its permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    /// The file's identity (the inode on Unix), so a file replaced by a
    /// rename counts as changed even when its size and time match.
    id: u64,
}

/// The file's stamp, or `None` when it doesn't exist.
pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let id = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let id = 0;
    Some(Stamp { len: meta.len(), modified: meta.modified().ok(), id })
}

/// Files that changed on disk and have since been quiet.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub files: Vec<PathBuf>,
    /// The main file is gone (deleted, or renamed away).
    pub main_missing: bool,
}

/// Watches the open file and its neighbours.  The system's watcher starts
/// with the first file, on a thread of its own, plus one more thread that
/// waits for each burst of changes to end; both sleep while nothing
/// happens.  Dropping this stops them.
pub struct FileWatch {
    waker: Waker,
    backend: Option<Backend>,
    /// The system's watcher couldn't start (too many watches, say), so
    /// changes go unnoticed.
    failed: bool,
    /// The files watched, the main one first.
    files: Vec<PathBuf>,
    /// What each file looked like when last checked; `None` when missing.
    stamps: HashMap<PathBuf, Option<Stamp>>,
}

struct Backend {
    watcher: notify::RecommendedWatcher,
    /// The paths events may name (a file, or the file a link points to),
    /// each with the watched file it stands for.
    wanted: Arc<RwLock<HashMap<PathBuf, PathBuf>>>,
    /// Folders being watched.
    dirs: HashSet<PathBuf>,
    /// Watched files touched by a burst of changes that has ended.
    settled: Receiver<HashSet<PathBuf>>,
}

impl FileWatch {
    pub fn new(waker: Waker) -> FileWatch {
        FileWatch { waker, backend: None, failed: false, files: Vec::new(), stamps: HashMap::new() }
    }

    /// Watches exactly `files`, the main one first, instead of what was
    /// watched before.  A file that stays watched keeps what it looked
    /// like when last checked, so a change made while it was being
    /// reloaded still counts.
    pub fn watch(&mut self, files: &[PathBuf]) {
        let mut list: Vec<PathBuf> = Vec::new();
        for f in files {
            let f = std::path::absolute(f).unwrap_or_else(|_| f.clone());
            if !list.contains(&f) {
                list.push(f);
            }
        }
        if list == self.files {
            return;
        }
        self.stamps.retain(|p, _| list.contains(p));
        for f in &list {
            self.stamps.entry(f.clone()).or_insert_with(|| stamp(f));
        }
        self.files = list;
        if self.backend.is_none() && !self.failed && !self.files.is_empty() {
            match Backend::start(self.waker.clone()) {
                Ok(b) => self.backend = Some(b),
                Err(e) => {
                    eprintln!("slerp view: can't watch files for changes: {e}");
                    self.failed = true;
                }
            }
        }
        if let Some(b) = &mut self.backend {
            b.set(&self.files);
        }
    }

    /// Stops watching, as when the file is closed.
    pub fn clear(&mut self) {
        self.watch(&[]);
    }

    /// The watched files that changed since they were last checked, once
    /// they have been quiet for [`QUIET`].
    pub fn poll(&mut self) -> Option<Change> {
        let backend = self.backend.as_ref()?;
        let mut touched = HashSet::new();
        while let Ok(t) = backend.settled.try_recv() {
            touched.extend(t);
        }
        if touched.is_empty() {
            return None;
        }
        let mut changed = Vec::new();
        for f in &self.files {
            if !touched.contains(f) {
                continue;
            }
            let now = stamp(f);
            if self.stamps.insert(f.clone(), now) != Some(now) {
                changed.push(f.clone());
            }
        }
        if changed.is_empty() {
            return None;
        }
        let main_missing = self.files.first().is_some_and(|m| matches!(self.stamps.get(m), Some(None)));
        Some(Change { files: changed, main_missing })
    }
}

impl Backend {
    fn start(waker: Waker) -> Result<Backend, String> {
        let wanted: Arc<RwLock<HashMap<PathBuf, PathBuf>>> = Arc::default();
        let (raw_tx, raw_rx) = channel::<Vec<PathBuf>>();
        let (settled_tx, settled) = channel();
        let filter = wanted.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            // Reading a file (the viewer's own reloads included) is no
            // change.
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let wanted = filter.read().unwrap_or_else(|e| e.into_inner());
            let hits: Vec<PathBuf> = if event.need_rescan() {
                wanted.values().cloned().collect()
            } else {
                event.paths.iter().filter_map(|p| wanted.get(p).cloned()).collect()
            };
            if !hits.is_empty() {
                let _ = raw_tx.send(hits);
            }
        })
        .map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("slerp-watch".into())
            .spawn(move || settle_bursts(raw_rx, settled_tx, waker))
            .map_err(|e| e.to_string())?;
        Ok(Backend { watcher, wanted, dirs: HashSet::new(), settled })
    }

    /// Points the watches at the folders of `files`.
    fn set(&mut self, files: &[PathBuf]) {
        let mut wanted = HashMap::new();
        for f in files {
            wanted.insert(f.clone(), f.clone());
            // A link's target changes in the target's folder.
            if let Ok(real) = std::fs::canonicalize(f)
                && real != *f
            {
                wanted.insert(real, f.clone());
            }
        }
        let dirs: HashSet<PathBuf> = wanted.keys().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
        for gone in self.dirs.difference(&dirs) {
            let _ = self.watcher.unwatch(gone);
        }
        let new: Vec<PathBuf> = dirs.difference(&self.dirs).cloned().collect();
        // A folder that can't be watched (it doesn't exist) is tried again
        // next time.
        self.dirs.retain(|d| dirs.contains(d));
        for d in new {
            if self.watcher.watch(&d, RecursiveMode::NonRecursive).is_ok() {
                self.dirs.insert(d);
            }
        }
        *self.wanted.write().unwrap_or_else(|e| e.into_inner()) = wanted;
    }
}

/// The thread that turns notifications into ended bursts: it sleeps until
/// one arrives, then until the files have been quiet for [`QUIET`].
fn settle_bursts(raw: Receiver<Vec<PathBuf>>, settled: Sender<HashSet<PathBuf>>, waker: Waker) {
    let mut debounce = Debounce::new(QUIET);
    let mut touched = HashSet::new();
    loop {
        let received = match debounce.wait(Instant::now()) {
            None => raw.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(left) => raw.recv_timeout(left),
        };
        match received {
            Ok(paths) => {
                debounce.change(Instant::now());
                touched.extend(paths);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if debounce.settle(Instant::now()) {
            if settled.send(std::mem::take(&mut touched)).is_err() {
                return;
            }
            wake(&waker);
        }
    }
}

//endregion

/*┌─────────────────╮
| The bar         |
└─────────────────╯*/
//region The bar

/// What the view shows about its file on disk.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// "This file changed on disk.", with Reload and Always Reload.
    Changed,
    /// The file is gone; what was loaded stays on screen.
    Deleted,
    /// Reloading failed (a half-written file, say), so what was loaded
    /// before stays on screen until the next change.
    Failed(String),
    /// A reload that kept the view finished at this time; shown briefly.
    Reloaded(Instant),
}

impl Notice {
    /// What to do about a change: reload by itself, or ask.
    pub fn for_change(change: &Change, always_reload: bool) -> Option<Notice> {
        if change.main_missing {
            Some(Notice::Deleted)
        } else if always_reload {
            None
        } else {
            Some(Notice::Changed)
        }
    }

    /// Whether it has finished showing.
    pub fn expired(&self, now: Instant) -> bool {
        matches!(self, Notice::Reloaded(at) if now.saturating_duration_since(*at) >= RELOADED_SHOWS)
    }
}

/// What the bar's buttons ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAction {
    Reload,
    AlwaysReload,
    Dismiss,
}

/// The kinds of KDE's `KMessageWidget`, with the colours it uses for them.
#[derive(Clone, Copy)]
enum Kind {
    Positive,
    Information,
    Warning,
    Error,
}

impl Kind {
    fn color(self, p: &slerp_theme::Palette) -> egui::Color32 {
        match self {
            Kind::Positive => p.positive,
            // KMessageWidget hard-codes Breeze's #3daee9 here, which is the
            // scheme's focus colour.
            Kind::Information => p.focus,
            Kind::Warning => p.neutral,
            Kind::Error => p.negative,
        }
    }
}

/// `KMessageWidget::paintEvent`'s fill: a fifth of the kind's colour over
/// the window colour.
fn tint(color: egui::Color32, window: egui::Color32) -> egui::Color32 {
    let mix = |a: u8, b: u8| (a as f32 * 0.2 + b as f32 * 0.8) as u8;
    egui::Color32::from_rgb(mix(color.r(), window.r()), mix(color.g(), window.g()), mix(color.b(), window.b()))
}

/// Draws `notice` over the top of the view at `rect` and returns what its
/// buttons asked for.
pub fn notice_ui(ui: &mut egui::Ui, rect: egui::Rect, notice: &Notice) -> Option<BarAction> {
    let now = Instant::now();
    match notice {
        Notice::Changed => header_bar(ui, rect, Kind::Information, "This file changed on disk.", true),
        Notice::Deleted => header_bar(ui, rect, Kind::Warning, "This file was deleted from disk.", false),
        Notice::Failed(e) => {
            let first = e.lines().next().unwrap_or_default();
            note(ui, rect, Kind::Error, &format!("Couldn't reload: {first}"), 1.0, true)
        }
        Notice::Reloaded(at) => {
            let shown = now.saturating_duration_since(*at);
            let left = RELOADED_SHOWS.saturating_sub(shown);
            let alpha = (left.as_secs_f32() / RELOADED_FADES.as_secs_f32()).clamp(0.0, 1.0);
            // Sleep until the fade starts, then draw it smoothly.
            if left > RELOADED_FADES {
                ui.ctx().request_repaint_after(left - RELOADED_FADES);
            } else if !left.is_zero() {
                ui.ctx().request_repaint();
            }
            note(ui, rect, Kind::Positive, "Reloaded", alpha, false)
        }
    }
}

/// Padding inside the bar and its buttons, in points: slimmer than a
/// dialog's, as KDE's message bars above a view are.
const BAR_MARGIN: f32 = 5.0;
const BUTTON_PADDING: egui::Vec2 = egui::vec2(8.0, 4.0);

/// The height of the bar's buttons: the text and padding, but never less
/// than the theme's minimum control height (Breeze makes buttons about 32
/// px tall), which egui enforces whatever the padding.
fn button_height(ui: &egui::Ui) -> f32 {
    let text = ui.text_style_height(&egui::TextStyle::Button).max(16.0) + BUTTON_PADDING.y * 2.0;
    text.max(ui.spacing().interact_size.y)
}

/// How tall the bar along the view's top edge is for `notice`, so the
/// view's own labels can sit below it; zero for the floating notes.
pub fn bar_height(ui: &egui::Ui, notice: Option<&Notice>) -> f32 {
    match notice {
        Some(Notice::Changed | Notice::Deleted) => (button_height(ui) + BAR_MARGIN * 2.0).round(),
        _ => 0.0,
    }
}

/// A note that a reload is under way, where "Reloaded" will show.
pub fn progress_note(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    note(ui, rect, Kind::Information, text, 1.0, false);
}

/// A bar across the top of the view, like `KMessageWidget` in its header
/// position (Kate's "modified on disk" bar): the tinted fill with a line
/// of the kind's colour along its bottom, the text on the left, buttons
/// and a close button on the right.
fn header_bar(ui: &mut egui::Ui, rect: egui::Rect, kind: Kind, text: &str, reload: bool) -> Option<BarAction> {
    let p = slerp_theme::palette(ui.ctx());
    let color = kind.color(&p);
    let button_h = button_height(ui);
    let height = (button_h + BAR_MARGIN * 2.0).round();
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), height));
    // The bar takes clicks and drags that miss its buttons, so they don't
    // turn the model underneath.
    let id = ui.id().with("watch-bar");
    ui.interact(bar, id, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(bar, 0.0, tint(color, p.window));
    painter.hline(bar.x_range(), bar.bottom() - 0.5, egui::Stroke::new(1.0, color));
    let mut action = None;
    let inner = bar.shrink2(egui::vec2(BAR_MARGIN + 3.0, BAR_MARGIN));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("watch-bar-contents")
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().button_padding = BUTTON_PADDING;
    child.spacing_mut().item_spacing.x = 6.0;
    child.add(egui::Label::new(egui::RichText::new(text).color(p.window_text)).truncate());
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if close_button(ui, button_h).on_hover_text("Close message").clicked() {
            action = Some(BarAction::Dismiss);
        }
        if reload {
            let always = ui
                .button("Always Reload")
                .on_hover_text("Reloads now, and from now on reloads by itself whenever the file changes (View menu)");
            if always.clicked() {
                action = Some(BarAction::AlwaysReload);
            }
            let button = match slerp_theme::widgets::icon_image("view-refresh", 16) {
                Some(icon) => egui::Button::image_and_text(icon, "Reload"),
                None => egui::Button::new("Reload"),
            };
            if ui.add(button).on_hover_text("Reloads the file, keeping the view (F5)").clicked() {
                action = Some(BarAction::Reload);
            }
        }
    });
    action
}

/// A small note floating at the top middle of the view, like
/// `KMessageWidget` inline: rounded, outlined in the kind's colour.
fn note(ui: &mut egui::Ui, rect: egui::Rect, kind: Kind, text: &str, alpha: f32, closable: bool) -> Option<BarAction> {
    if alpha <= 0.0 {
        return None;
    }
    let p = slerp_theme::palette(ui.ctx());
    let color = kind.color(&p);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let close = if closable { 22.0 } else { 0.0 };
    let max_text = (rect.width() - 80.0 - close).max(60.0);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, p.window_text.gamma_multiply(alpha));
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_text);
    let galley = ui.painter().layout_job(job);
    let size = egui::vec2(galley.size().x + 24.0 + close, galley.size().y + 12.0);
    let center = egui::pos2(rect.center().x, rect.top() + 10.0 + size.y / 2.0);
    let note = egui::Rect::from_min_size((center - size / 2.0).round(), size.round());
    let painter = ui.painter_at(rect);
    painter.rect(
        note.shrink(1.0),
        3.0,
        tint(color, p.window).gamma_multiply(alpha),
        egui::Stroke::new(2.0, color.gamma_multiply(alpha)),
        egui::StrokeKind::Middle,
    );
    painter.galley(egui::pos2(note.left() + 12.0, note.center().y - galley.size().y / 2.0), galley, p.window_text);
    if !closable {
        return None;
    }
    let button = egui::Rect::from_center_size(egui::pos2(note.right() - 16.0, note.center().y), egui::vec2(20.0, 20.0));
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("watch-note-close").max_rect(button));
    close_button(&mut child, 20.0).on_hover_text("Close message").clicked().then_some(BarAction::Dismiss)
}

/// A flat close button with the theme's `dialog-close` icon (a cross drawn
/// by hand where no icon theme has one), outlined on hover like Breeze's
/// auto-raise tool buttons.
fn close_button(ui: &mut egui::Ui, side: f32) -> egui::Response {
    let p = slerp_theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = p.metrics.frame_radius;
        if response.hovered() || response.is_pointer_button_down_on() {
            let fill = if response.is_pointer_button_down_on() {
                p.highlight.gamma_multiply(0.3)
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect(rect, radius, fill, egui::Stroke::new(1.0, p.highlight), egui::StrokeKind::Inside);
        }
        let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0));
        match slerp_theme::icon("dialog-close", 16).or_else(|| slerp_theme::icon("window-close", 16)) {
            Some(source) => {
                egui::Image::new(source).fit_to_exact_size(icon.size()).paint_at(ui, icon);
            }
            None => {
                let s = egui::Stroke::new(1.5, p.window_text);
                let r = icon.shrink(3.5);
                ui.painter().line_segment([r.left_top(), r.right_bottom()], s);
                ui.painter().line_segment([r.right_top(), r.left_bottom()], s);
            }
        }
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close"));
    response
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_ends_after_a_quiet_period_counted_from_its_last_change() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut d = Debounce::new(QUIET);
        assert_eq!(d.wait(t0), None);
        assert!(!d.settle(ms(1000)));
        // An exporter writes a chunk, then another 200 ms later.
        d.change(ms(0));
        assert_eq!(d.wait(ms(100)), Some(Duration::from_millis(200)));
        d.change(ms(200));
        assert!(!d.settle(ms(400)), "quiet only 200 ms after the second chunk");
        assert_eq!(d.wait(ms(400)), Some(Duration::from_millis(100)));
        // A change reported late (with an earlier time) doesn't shorten
        // the wait.
        d.change(ms(150));
        assert_eq!(d.wait(ms(400)), Some(Duration::from_millis(100)));
        assert!(d.settle(ms(500)));
        assert_eq!(d.wait(ms(500)), None, "settling ends the burst");
        assert!(!d.settle(ms(900)));
    }

    #[test]
    fn changes_ask_or_reload_by_themselves() {
        let changed = Change { files: vec![PathBuf::from("/a.glb")], main_missing: false };
        let gone = Change { main_missing: true, ..changed.clone() };
        assert_eq!(Notice::for_change(&changed, false), Some(Notice::Changed));
        assert_eq!(Notice::for_change(&changed, true), None);
        assert_eq!(Notice::for_change(&gone, true), Some(Notice::Deleted));
        let at = Instant::now();
        assert!(!Notice::Reloaded(at).expired(at + Duration::from_millis(1500)));
        assert!(Notice::Reloaded(at).expired(at + RELOADED_SHOWS));
        assert!(!Notice::Changed.expired(at + Duration::from_secs(60)));
    }

    #[test]
    fn stamps_tell_a_replaced_file_from_a_read_one() {
        let dir = temp_dir("stamps");
        let a = dir.join("a.bin");
        std::fs::write(&a, b"one").unwrap();
        let before = stamp(&a);
        let _ = std::fs::read(&a).unwrap();
        assert_eq!(stamp(&a), before, "reading changes nothing");
        // Saved by writing a new file and renaming it over the old one,
        // with the same size.
        let tmp = dir.join("a.bin.tmp");
        std::fs::write(&tmp, b"two").unwrap();
        std::fs::rename(&tmp, &a).unwrap();
        assert_ne!(stamp(&a), before);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(stamp(&a), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slerp-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Polls `watch` for up to `limit`, returning each change with the time
    /// it came, measured from `start`.
    fn changes(watch: &mut FileWatch, start: Instant, limit: Duration) -> Vec<(Duration, Change)> {
        let mut out = Vec::new();
        while start.elapsed() < limit {
            if let Some(c) = watch.poll() {
                out.push((start.elapsed(), c));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        out
    }

    #[test]
    fn a_file_written_in_two_chunks_changes_once_after_it_goes_quiet() {
        let dir = temp_dir("chunks");
        let main = dir.join("model.glb");
        std::fs::write(&main, b"first version").unwrap();
        let mut watch = FileWatch::new(Waker::default());
        watch.watch(std::slice::from_ref(&main));
        let start = Instant::now();
        // 1. Reading the file and writing a file nobody watches are no
        //    changes.
        let _ = std::fs::read(&main).unwrap();
        std::fs::write(dir.join("notes.txt"), b"unrelated").unwrap();
        assert!(changes(&mut watch, start, Duration::from_millis(500)).is_empty());
        // 2. Two chunks 150 ms apart: one change, at least 300 ms after the
        //    second.
        let start = Instant::now();
        let writer = {
            let main = main.clone();
            std::thread::spawn(move || {
                use std::io::Write;
                let mut f = std::fs::File::create(&main).unwrap();
                f.write_all(b"second version, part one").unwrap();
                f.sync_all().unwrap();
                std::thread::sleep(Duration::from_millis(150));
                f.write_all(b", part two").unwrap();
                Instant::now()
            })
        };
        let seen = changes(&mut watch, start, Duration::from_millis(1200));
        let last_write = writer.join().unwrap() - start;
        assert_eq!(seen.len(), 1, "{seen:?}");
        let (at, change) = &seen[0];
        assert!(*at >= last_write + QUIET, "came {at:?} after the start, last write at {last_write:?}");
        assert_eq!(change, &Change { files: vec![main.clone()], main_missing: false });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renames_deletions_and_neighbours_count() {
        let dir = temp_dir("renames");
        let gltf = dir.join("scene.gltf");
        let bin = dir.join("scene.bin");
        std::fs::write(&gltf, b"{}").unwrap();
        std::fs::write(&bin, b"geometry").unwrap();
        let mut watch = FileWatch::new(Waker::default());
        watch.watch(&[gltf.clone(), bin.clone()]);
        let wait = Duration::from_millis(800);

        // 1. Only the `.bin` changes.
        let start = Instant::now();
        std::fs::write(&bin, b"new geometry").unwrap();
        let seen = changes(&mut watch, start, wait);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].1.files, vec![bin.clone()]);

        // 2. Saved by writing elsewhere and renaming over it.
        let start = Instant::now();
        std::fs::write(dir.join(".scene.gltf.part"), b"{\"asset\":{}}").unwrap();
        std::fs::rename(dir.join(".scene.gltf.part"), &gltf).unwrap();
        let seen = changes(&mut watch, start, wait);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].1, Change { files: vec![gltf.clone()], main_missing: false });

        // 3. Deleted, then written again.
        let start = Instant::now();
        std::fs::remove_file(&gltf).unwrap();
        let seen = changes(&mut watch, start, wait);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].1.main_missing);
        let start = Instant::now();
        std::fs::write(&gltf, b"{}").unwrap();
        let seen = changes(&mut watch, start, wait);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(!seen[0].1.main_missing);

        // 4. Stepping to another file stops watching these.
        watch.watch(&[dir.join("other.glb")]);
        let start = Instant::now();
        std::fs::write(&bin, b"ignored").unwrap();
        assert!(changes(&mut watch, start, wait).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
