//! The half of drag-out that doesn't depend on a window system: the items
//! being dragged and how their files come into being, the names those files
//! get, the `file://` URIs a drop target is told about, the temporary
//! folder, and the one-drag-at-a-time rule.  The Wayland, Windows and macOS
//! code only moves these bytes and paths across their own protocol, so
//! everything here is tested without a compositor.

// Each platform uses a different part of this shared code (Wayland the
// session and the URI lists, Windows its own paths and the session's
// payload, macOS the per-item export), so on the others the rest is unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashSet;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

//region Items

/// Produces an item's file somewhere of its own choosing and returns where.
type PathProducer = Box<dyn FnOnce() -> io::Result<PathBuf> + Send>;

/// Writes an item's file to the path it is given.
type WriteProducer = Box<dyn FnOnce(&Path) -> io::Result<()> + Send>;

/// How an item's file comes into being, once a drop target needs it.
enum Producer {
    /// The producer makes the file itself (the Contents panel's per-user
    /// temporary folder) and says where.  The file is not deleted afterwards:
    /// its maker owns it.
    Path(PathProducer),
    /// Drag-out owns the file: the producer writes it to the path drag-out
    /// picks (a unique name in drag-out's own temporary folder, or the place
    /// a drop target asked for), and the folder is deleted after the drag.
    Write(WriteProducer),
}

/// One thing being dragged out of the window, which will land in the drop
/// target as an ordinary file.  Nothing is written until a drop target asks
/// for the data, since a drag that is cancelled, or that enters a few
/// windows on its way, shouldn't cost the time of extracting a large entry.
pub struct DragItem {
    name: String,
    producer: Producer,
}

impl DragItem {
    /// An item whose producer makes the file itself and returns its path.
    /// `name` is only a label (and the file name if the drag has to be
    /// described to the user); the dropped file keeps the name of the path
    /// returned.
    pub fn from_path(name: impl Into<String>, produce: impl FnOnce() -> io::Result<PathBuf> + Send + 'static) -> Self {
        DragItem { name: name.into(), producer: Producer::Path(Box::new(produce)) }
    }

    /// An item whose producer writes the file to the path it is given.
    /// `name` becomes the dropped file's name (made safe, and given a
    /// suffix like " (2)" if another item of the drag has the same name).
    pub fn from_write(name: impl Into<String>, write: impl FnOnce(&Path) -> io::Result<()> + Send + 'static) -> Self {
        DragItem { name: name.into(), producer: Producer::Write(Box::new(write)) }
    }

    /// An item that already is a file on disk, which is dropped in place.
    pub fn from_existing(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
        DragItem::from_path(name, move || Ok(path))
    }

    /// Items whose files are all made by one call, such as the Contents
    /// panel's `materialize(&ids, format)`, which writes the whole selection
    /// to its temporary folder and returns the paths in order.  The call
    /// happens once, when the first of the items is needed.  A result with
    /// fewer paths than names, or an error, fails the items it doesn't
    /// cover (they are skipped with a log line, the others still drop).
    pub fn batch(
        names: Vec<String>,
        produce: impl FnOnce() -> io::Result<Vec<PathBuf>> + Send + 'static,
    ) -> Vec<DragItem> {
        enum Batch {
            Pending(Box<dyn FnOnce() -> io::Result<Vec<PathBuf>> + Send>),
            Done(Vec<Option<io::Result<PathBuf>>>),
        }
        let count = names.len();
        let shared = Arc::new(Mutex::new(Batch::Pending(Box::new(produce))));
        names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let shared = shared.clone();
                DragItem::from_path(name, move || {
                    let mut batch = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if let Batch::Pending(_) = &*batch {
                        let Batch::Pending(produce) = std::mem::replace(&mut *batch, Batch::Done(Vec::new())) else {
                            unreachable!()
                        };
                        let results = match produce() {
                            Ok(paths) => paths.into_iter().map(|p| Some(Ok(p))).collect(),
                            Err(e) => {
                                let kind = e.kind();
                                (0..count).map(|_| Some(Err(io::Error::new(kind, e.to_string())))).collect()
                            }
                        };
                        *batch = Batch::Done(results);
                    }
                    match &mut *batch {
                        Batch::Done(results) => results
                            .get_mut(index)
                            .and_then(Option::take)
                            .unwrap_or_else(|| Err(io::Error::other("the batch produced no file for this item"))),
                        Batch::Pending(_) => unreachable!(),
                    }
                })
            })
            .collect()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A small picture shown beside the pointer while dragging: 8-bit RGBA,
/// straight alpha, `width * height * 4` bytes, no more than 128 pixels
/// either way.  Without one the drag shows only the cursor.
#[derive(Clone, Debug, Default)]
pub struct DragIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DragIcon {
    pub const MAX_SIDE: u32 = 128;

    /// Whether the buffer matches the size and the size is sensible.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= Self::MAX_SIDE
            && self.height <= Self::MAX_SIDE
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}

/// What to drag: the items, in the order the drop target receives them, and
/// optionally a picture to drag with the pointer.
pub struct Drag {
    pub items: Vec<DragItem>,
    pub icon: Option<DragIcon>,
}

impl Drag {
    pub fn new(items: Vec<DragItem>) -> Drag {
        Drag { items, icon: None }
    }

    pub fn with_icon(mut self, icon: DragIcon) -> Drag {
        self.icon = Some(icon);
        self
    }
}

impl From<Vec<DragItem>> for Drag {
    fn from(items: Vec<DragItem>) -> Drag {
        Drag::new(items)
    }
}

//endregion

//region Errors and outcomes

/// Why a drag couldn't start.
#[derive(Debug, thiserror::Error)]
pub enum DragError {
    /// This window system or session can't start a drag out of a window of
    /// ours (an X11 session, a Linux desktop without Wayland, a platform
    /// without code for it).  The caller should say "Use Ctrl+C and paste
    /// instead".
    #[error("dragging files out isn't supported here ({0}); use Ctrl+C and paste instead")]
    Unsupported(String),
    /// A drag is already under way.
    #[error("a drag is already in progress")]
    Busy,
    /// No mouse button is held in the window, so the window system would
    /// refuse a drag (Wayland starts one only from a button press it
    /// reported to us).
    #[error("no mouse button is pressed in the window, so a drag can't start")]
    NoGesture,
    /// There was nothing to drag.
    #[error("nothing to drag")]
    NoItems,
    /// The window system's own call failed.
    #[error("starting the drag failed: {0}")]
    Platform(String),
}

/// How a drag ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A drop target accepted the drop.
    Dropped,
    /// The user cancelled (Esc, or dropped somewhere that takes nothing).
    Cancelled,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Held by the platform code for as long as a drag is under way; there is
/// one pointer, so there is one drag.
#[derive(Debug)]
pub(crate) struct ActiveGuard(());

impl ActiveGuard {
    pub fn acquire() -> Result<ActiveGuard, DragError> {
        match ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(ActiveGuard(())),
            Err(_) => Err(DragError::Busy),
        }
    }

    pub fn is_held() -> bool {
        ACTIVE.load(Ordering::Acquire)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

//endregion

//region Names and URIs

/// Makes an item name safe to use as a file name on this system: no path
/// separators, control characters or (on Windows) characters or names
/// Windows forbids, not empty, and no longer than 200 bytes, with the
/// extension kept.  `strict` is the Windows rule set; tests use both.
pub(crate) fn sanitize_name(name: &str, strict: bool) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            c if c.is_control() => '_',
            '<' | '>' | ':' | '"' | '|' | '?' | '*' if strict => '_',
            c => c,
        })
        .collect();
    if strict {
        out = out.trim_end_matches(['.', ' ']).to_string();
    }
    let out = out.trim_start_matches(' ').to_string();
    let mut out = if out.is_empty() || out == "." || out == ".." { "item".to_string() } else { out };
    if strict {
        let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            out.insert(0, '_');
        }
    }
    shorten_name(&out, 200)
}

/// Cuts a name to `max` bytes at a character boundary, cutting the stem and
/// keeping a short extension.
fn shorten_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let ext_start = name.rfind('.').filter(|&i| i > 0 && name.len() - i <= 16).unwrap_or(name.len());
    let (stem, ext) = name.split_at(ext_start);
    let mut cut = max.saturating_sub(ext.len());
    while cut > 0 && !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{}", &stem[..cut], ext)
}

/// Makes every name safe and distinct.  Names that collide, ignoring case
/// (the folder may be copied onto a Windows or macOS volume), get " (2)",
/// " (3)" and so on before the extension, so the second `a.png` is
/// `a (2).png`.
pub(crate) fn unique_names(names: &[&str], strict: bool) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    names
        .iter()
        .map(|name| {
            let base = sanitize_name(name, strict);
            let mut candidate = base.clone();
            let mut n = 2;
            while !taken.insert(candidate.to_lowercase()) {
                let dot = base.rfind('.').filter(|&i| i > 0).unwrap_or(base.len());
                candidate = format!("{} ({}){}", &base[..dot], n, &base[dot..]);
                n += 1;
            }
            candidate
        })
        .collect()
}

/// Percent-encodes bytes for a URI path: everything except the unreserved
/// characters and `/`, which is what every file manager decodes.
fn encode_path_bytes(bytes: &[u8], out: &mut String) {
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 15) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
}

/// `file:///home/user/a%20b.png` for an absolute Unix path.
pub(crate) fn file_uri_unix(path: &[u8]) -> String {
    let mut uri = String::from("file://");
    encode_path_bytes(path, &mut uri);
    uri
}

/// `file:///C:/Users/a%20b.png` for a Windows drive path and
/// `file://server/share/a.png` for a UNC path (`\\server\share\a.png`).
#[cfg(any(windows, test))]
pub(crate) fn file_uri_windows(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut uri = String::from("file://");
    if let Some(unc) = path.strip_prefix(r"\\") {
        let unc = unc.replace('\\', "/");
        let (host, rest) = unc.split_once('/').unwrap_or((&unc, ""));
        uri.push_str(host);
        uri.push('/');
        encode_path_bytes(rest.as_bytes(), &mut uri);
    } else {
        uri.push('/');
        // Keep the drive's colon readable: `C:` is what every reader expects.
        let slashed = path.replace('\\', "/");
        match slashed.split_once(':') {
            Some((drive, rest)) if drive.len() == 1 => {
                uri.push_str(drive);
                uri.push(':');
                encode_path_bytes(rest.as_bytes(), &mut uri);
            }
            _ => encode_path_bytes(slashed.as_bytes(), &mut uri),
        }
    }
    uri
}

/// The `file://` URI of a path on this system.
pub(crate) fn file_uri(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        file_uri_unix(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        file_uri_windows(&path.to_string_lossy())
    }
}

/// The `text/uri-list` body: one URI per line, each ended with CRLF as RFC
/// 2483 says.
pub(crate) fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| file_uri(p) + "\r\n").collect()
}

/// The `text/plain` body: the paths themselves, one per line.
pub(crate) fn plain_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n")
}

//endregion

//region Temporary folder

const STAGING_PREFIX: &str = "slerp-drag-";

/// How long a finished drag's files stay: the drop target may still be
/// copying them after it said it was done.
pub(crate) const KEEP_AFTER_DROP: Duration = Duration::from_secs(10 * 60);

static STAGING_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A private folder in the system's temporary folder, created for one drag
/// and deleted after it.  Its name is made with an exclusive `mkdir` (mode
/// 0700 on Unix), so nobody can pre-create it in a shared `/tmp` to
/// redirect where the files go.
#[derive(Debug)]
pub(crate) struct Staging {
    dir: PathBuf,
}

impl Staging {
    pub fn create() -> io::Result<Staging> {
        Staging::create_in(&std::env::temp_dir())
    }

    pub fn create_in(parent: &Path) -> io::Result<Staging> {
        sweep(parent, Duration::from_secs(60 * 60));
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let mut last = io::Error::other("no free name for a temporary folder");
        for _ in 0..100 {
            let n = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = parent.join(format!("{STAGING_PREFIX}{}-{n}-{nanos:09}", std::process::id()));
            let builder = {
                #[cfg(unix)]
                {
                    let mut builder = std::fs::DirBuilder::new();
                    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
                    builder
                }
                #[cfg(not(unix))]
                std::fs::DirBuilder::new()
            };
            match builder.create(&dir) {
                Ok(()) => return Ok(Staging { dir }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Deletes the folder and what is in it now.
    pub fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Deletes the folder after `delay`, on a thread that exits with the
    /// process (the next run's `sweep` collects what an early exit left).
    pub fn remove_later(self, delay: Duration) {
        let spawned = std::thread::Builder::new().name("dragout-cleanup".into()).spawn({
            let dir = self.dir.clone();
            move || {
                std::thread::sleep(delay);
                let _ = std::fs::remove_dir_all(dir);
            }
        });
        if spawned.is_err() {
            self.remove();
        }
    }
}

/// Deletes drag folders in `parent` that were last changed more than `older`
/// ago, left by runs that ended before their delayed cleanup.  Other users'
/// folders in a shared `/tmp` are theirs: deleting one fails (mode 0700 and
/// the sticky bit) and that's ignored.
pub(crate) fn sweep(parent: &Path, older: Duration) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let age = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
        if meta.is_dir() && age > older {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

//endregion

//region Payload

enum Slot {
    Pending(Producer),
    Ready(PathBuf),
    Failed,
}

/// The items of one drag and their files, made when first needed and kept,
/// so that several drop targets, or one that asks for `text/uri-list` and
/// then `text/plain`, all see the same files and nothing is written twice.
pub(crate) struct Payload {
    names: Vec<String>,
    slots: Vec<Slot>,
    staging: Option<Staging>,
    /// Where drag-out's own folder goes, for tests.
    parent: Option<PathBuf>,
    resolved: bool,
}

impl Payload {
    pub fn new(items: Vec<DragItem>) -> Payload {
        let raw: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        let names = unique_names(&raw, cfg!(windows));
        let slots = items.into_iter().map(|i| Slot::Pending(i.producer)).collect();
        Payload { names, slots, staging: None, parent: None, resolved: false }
    }

    #[cfg(test)]
    pub fn new_in(items: Vec<DragItem>, parent: &Path) -> Payload {
        let mut payload = Payload::new(items);
        payload.parent = Some(parent.to_path_buf());
        payload
    }

    /// The names the dropped files get (for the items that write their own
    /// files), in item order.
    #[cfg(target_os = "macos")]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Makes item `index`'s file if that hasn't happened yet and returns its
    /// path, or `None` if it couldn't be made (logged).
    pub fn resolve_one(&mut self, index: usize) -> Option<PathBuf> {
        let producer = match self.slots.get_mut(index)? {
            Slot::Ready(path) => return Some(path.clone()),
            Slot::Failed => return None,
            slot @ Slot::Pending(_) => match std::mem::replace(slot, Slot::Failed) {
                Slot::Pending(producer) => producer,
                _ => unreachable!(),
            },
        };
        let name = self.names[index].clone();
        let result = match producer {
            Producer::Path(produce) => run(&name, produce).and_then(|path| {
                // A producer that reports success without a file would hand
                // the drop target a dead link.
                std::fs::metadata(&path).map(|_| path)
            }),
            Producer::Write(write) => self.staged_path(&name).and_then(|dest| match run(&name, || write(&dest)) {
                Ok(()) => Ok(dest),
                Err(e) => {
                    let _ = std::fs::remove_file(&dest);
                    Err(e)
                }
            }),
        };
        match result {
            Ok(path) => {
                self.slots[index] = Slot::Ready(path.clone());
                Some(path)
            }
            Err(e) => {
                eprintln!("slerp dragout: skipping {name}: {e}");
                None
            }
        }
    }

    /// Writes item `index` to `dest` (a place a drop target chose) instead
    /// of drag-out's folder: macOS file promises name their own destination.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn export_one(&mut self, index: usize, dest: &Path) -> io::Result<()> {
        let slot = self.slots.get_mut(index).ok_or_else(|| io::Error::other("no such item"))?;
        let name = self.names[index].clone();
        match std::mem::replace(slot, Slot::Failed) {
            Slot::Pending(Producer::Write(write)) => {
                run(&name, || write(dest))?;
                self.slots[index] = Slot::Ready(dest.to_path_buf());
                Ok(())
            }
            Slot::Pending(Producer::Path(produce)) => {
                let made = run(&name, produce)?;
                std::fs::copy(&made, dest)?;
                self.slots[index] = Slot::Ready(made);
                Ok(())
            }
            Slot::Ready(path) => {
                std::fs::copy(&path, dest)?;
                self.slots[index] = Slot::Ready(path);
                Ok(())
            }
            Slot::Failed => Err(io::Error::other("this item's file couldn't be made")),
        }
    }

    fn staged_path(&mut self, name: &str) -> io::Result<PathBuf> {
        if self.staging.is_none() {
            self.staging = Some(match &self.parent {
                Some(parent) => Staging::create_in(parent)?,
                None => Staging::create()?,
            });
        }
        Ok(self.staging.as_ref().expect("just created").dir().join(name))
    }

    /// Makes every item's file, in order, the first time and returns the
    /// paths of those that could be made.
    pub fn paths(&mut self) -> Vec<PathBuf> {
        if !self.resolved {
            self.resolved = true;
            for index in 0..self.slots.len() {
                self.resolve_one(index);
            }
        }
        self.slots
            .iter()
            .filter_map(|s| match s {
                Slot::Ready(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    #[cfg(test)]
    pub fn is_resolved(&self) -> bool {
        self.resolved
    }

    /// The bytes to give for a MIME type a drop target asked for, making the
    /// files first; `None` for a type that wasn't offered.
    pub fn data(&mut self, mime: &str) -> Option<Vec<u8>> {
        match mime {
            URI_LIST => Some(uri_list(&self.paths()).into_bytes()),
            m if PLAIN_TEXT.contains(&m) => Some(plain_list(&self.paths()).into_bytes()),
            _ => None,
        }
    }

    /// The drag was cancelled: whatever drag-out wrote goes now.
    pub fn cancel(&mut self) {
        if let Some(staging) = self.staging.take() {
            staging.remove();
        }
        self.slots.clear();
    }

    /// The drag ended with a drop: the files stay for a while, since the
    /// target may still be copying them, then go.
    pub fn finish(&mut self) {
        if let Some(staging) = self.staging.take() {
            staging.remove_later(KEEP_AFTER_DROP);
        }
        self.slots.clear();
    }
}

impl Drop for Payload {
    fn drop(&mut self) {
        // A payload dropped without being finished (the window system went
        // away) still must not leave files behind.
        self.cancel();
    }
}

/// Runs a producer, turning a panic into an error so one bad entry can't
/// take down the thread that serves the drag.
fn run<T>(name: &str, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(_) => Err(io::Error::other(format!("the code that makes {name} panicked"))),
    }
}

//endregion

//region Session

/// The MIME type of a list of file URIs.
pub(crate) const URI_LIST: &str = "text/uri-list";

/// The plain-text types that get the paths one per line: what terminals and
/// editors ask for.
pub(crate) const PLAIN_TEXT: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// Every type the drag offers, in order of preference.
pub(crate) const OFFERED: [&str; 4] = [URI_LIST, PLAIN_TEXT[0], PLAIN_TEXT[1], PLAIN_TEXT[2]];

/// Where a drag is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Offered; the pointer is still dragging.
    Dragging,
    /// The user released over a target; its data requests may still come.
    Dropped,
    /// Over: the target said it was done, or the drag was cancelled.
    Over(Outcome),
}

/// The source side of one drag, in terms every platform shares: the data to
/// hand out, the phases, and the cleanup.  The Wayland code feeds it the
/// data source's events; tests feed it a mock.
pub(crate) struct Session {
    payload: Payload,
    phase: Phase,
    _guard: ActiveGuard,
}

impl Session {
    pub fn new(payload: Payload, guard: ActiveGuard) -> Session {
        Session { payload, phase: Phase::Dragging, _guard: guard }
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(any(windows, test))]
    pub fn payload(&mut self) -> &mut Payload {
        &mut self.payload
    }

    /// A drop target asked for `mime`.  Returns what to write, or `None`
    /// when the type isn't offered or the drag is already over.
    pub fn data(&mut self, mime: &str) -> Option<Vec<u8>> {
        if matches!(self.phase, Phase::Over(_)) {
            return None;
        }
        self.payload.data(mime)
    }

    /// The user released the button over a target.
    pub fn drop_performed(&mut self) {
        if self.phase == Phase::Dragging {
            self.phase = Phase::Dropped;
        }
    }

    /// The target finished with the drop.  Files stay for a grace period.
    pub fn finished(&mut self) -> Outcome {
        if let Phase::Over(outcome) = self.phase {
            return outcome;
        }
        self.phase = Phase::Over(Outcome::Dropped);
        self.payload.finish();
        Outcome::Dropped
    }

    /// The drag was cancelled, or the drop was refused.  KWin reports a drop
    /// on empty desktop as `dnd_drop_performed` followed at once by
    /// `cancelled`, so a cancel after the drop is still a failed drag.  Files
    /// already made then stay for the grace period instead of going at
    /// once, in case a target that never said it was finished is still
    /// copying them.
    pub fn cancelled(&mut self) -> Outcome {
        match self.phase {
            Phase::Over(outcome) => outcome,
            Phase::Dropped => {
                self.phase = Phase::Over(Outcome::Cancelled);
                self.payload.finish();
                Outcome::Cancelled
            }
            Phase::Dragging => {
                self.phase = Phase::Over(Outcome::Cancelled);
                self.payload.cancel();
                Outcome::Cancelled
            }
        }
    }

    #[cfg(test)]
    pub fn outcome(&self) -> Option<Outcome> {
        match self.phase {
            Phase::Over(o) => Some(o),
            _ => None,
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Tests that take the one-drag guard must not overlap.
    static GUARD_LOCK: Mutex<()> = Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slerp-dragout-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text_item(name: &str, body: &'static str, count: &Arc<AtomicUsize>) -> DragItem {
        let count = count.clone();
        DragItem::from_write(name, move |dest| {
            count.fetch_add(1, Ordering::SeqCst);
            std::fs::write(dest, body)
        })
    }

    #[test]
    fn uris_encode_awkward_names() {
        assert_eq!(file_uri_unix(b"/tmp/a b.png"), "file:///tmp/a%20b.png");
        assert_eq!(file_uri_unix(b"/tmp/50%#1?.txt"), "file:///tmp/50%25%231%3F.txt");
        assert_eq!(file_uri_unix("/tmp/größe ☃.png".as_bytes()), "file:///tmp/gr%C3%B6%C3%9Fe%20%E2%98%83.png");
        assert_eq!(file_uri_unix(b"/tmp/a+b&c=d;e,f'g(h)"), "file:///tmp/a%2Bb%26c%3Dd%3Be%2Cf%27g%28h%29");
        // Bytes that aren't UTF-8 still round-trip as percent escapes.
        assert_eq!(file_uri_unix(b"/tmp/\xff\xfe"), "file:///tmp/%FF%FE");
    }

    #[test]
    fn windows_uris() {
        assert_eq!(file_uri_windows(r"C:\Users\Ann\a b#.png"), "file:///C:/Users/Ann/a%20b%23.png");
        assert_eq!(file_uri_windows(r"\\?\C:\Temp\x.txt"), "file:///C:/Temp/x.txt");
        assert_eq!(file_uri_windows(r"\\server\share\dir\a b.txt"), "file://server/share/dir/a%20b.txt");
    }

    #[cfg(unix)]
    #[test]
    fn uri_list_is_crlf_terminated() {
        let list = uri_list(&[PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b c")]);
        assert_eq!(list, "file:///tmp/a\r\nfile:///tmp/b%20c\r\n");
        assert_eq!(plain_list(&[PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b c")]), "/tmp/a\n/tmp/b c");
        assert_eq!(uri_list(&[]), "");
    }

    #[cfg(windows)]
    #[test]
    fn uri_list_is_crlf_terminated_on_windows() {
        let list = uri_list(&[PathBuf::from(r"C:\tmp\a"), PathBuf::from(r"C:\tmp\b c")]);
        assert_eq!(list, "file:///C:/tmp/a\r\nfile:///C:/tmp/b%20c\r\n");
        assert_eq!(uri_list(&[]), "");
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(sanitize_name("a/b\\c.png", false), "a_b_c.png");
        assert_eq!(sanitize_name("", false), "item");
        assert_eq!(sanitize_name("..", false), "item");
        assert_eq!(sanitize_name("a:b?.txt", false), "a:b?.txt");
        assert_eq!(sanitize_name("a:b?.txt", true), "a_b_.txt");
        assert_eq!(sanitize_name("CON.txt", true), "_CON.txt");
        assert_eq!(sanitize_name("com1", true), "_com1");
        assert_eq!(sanitize_name("name. . ", true), "name");
        assert_eq!(sanitize_name("tab\there", false), "tab_here");
        let long = format!("{}.png", "é".repeat(300));
        let short = sanitize_name(&long, false);
        assert!(short.len() <= 200 && short.ends_with(".png"), "{}", short.len());
        assert!(short.is_char_boundary(short.len()));
    }

    #[test]
    fn colliding_names_get_suffixes() {
        let names = unique_names(&["a.png", "A.PNG", "a.png", "b", "b", "a (2).png", ".hidden", ".hidden"], false);
        assert_eq!(names, ["a.png", "A (2).PNG", "a (3).png", "b", "b (2)", "a (2) (2).png", ".hidden", ".hidden (2)"]);
        let all: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
        assert_eq!(all.len(), names.len());
    }

    #[test]
    fn producers_run_once_and_only_when_asked() {
        let parent = scratch("lazy");
        let count = Arc::new(AtomicUsize::new(0));
        let mut payload =
            Payload::new_in(vec![text_item("a.txt", "A", &count), text_item("b.txt", "B", &count)], &parent);
        assert_eq!(count.load(Ordering::SeqCst), 0, "nothing is written before the first request");
        assert!(!payload.is_resolved());
        let uris = String::from_utf8(payload.data(URI_LIST).unwrap()).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(uris.lines().count(), 2);
        assert!(uris.ends_with("b.txt\r\n"));
        // A second type, and a second ask for the first, reuse the files.
        let plain = String::from_utf8(payload.data("text/plain").unwrap()).unwrap();
        assert_eq!(plain.lines().count(), 2);
        payload.data(URI_LIST).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        for path in payload.paths() {
            assert!(path.starts_with(&parent));
            assert!(path.exists());
        }
        assert!(payload.data("image/png").is_none());
        payload.cancel();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn many_items_keep_their_order() {
        let parent = scratch("many");
        let order = Arc::new(Mutex::new(Vec::new()));
        let items: Vec<DragItem> = (0..400)
            .map(|i| {
                let order = order.clone();
                DragItem::from_write(format!("entry {i:03}.txt"), move |dest| {
                    order.lock().unwrap().push(i);
                    std::fs::write(dest, i.to_string())
                })
            })
            .collect();
        let mut payload = Payload::new_in(items, &parent);
        let paths = payload.paths();
        assert_eq!(paths.len(), 400);
        assert_eq!(*order.lock().unwrap(), (0..400).collect::<Vec<_>>());
        assert!(paths[399].ends_with("entry 399.txt"));
        payload.cancel();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn colliding_items_land_side_by_side() {
        let parent = scratch("collide");
        let count = Arc::new(AtomicUsize::new(0));
        let mut payload = Payload::new_in(
            vec![text_item("x.txt", "one", &count), text_item("x.txt", "two", &count), text_item("X.txt", "3", &count)],
            &parent,
        );
        let paths = payload.paths();
        let contents: Vec<String> = paths.iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
        assert_eq!(contents, ["one", "two", "3"]);
        let names: HashSet<_> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase()).collect();
        assert_eq!(names.len(), 3);
        payload.cancel();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn failing_items_are_skipped() {
        let parent = scratch("fail");
        let count = Arc::new(AtomicUsize::new(0));
        let mut payload = Payload::new_in(
            vec![
                text_item("ok1.txt", "1", &count),
                DragItem::from_write("bad.txt", |dest| {
                    std::fs::write(dest, "partial")?;
                    Err(io::Error::other("corrupt entry"))
                }),
                DragItem::from_write("panics.txt", |_| panic!("decoder bug")),
                DragItem::from_path("ghost.txt", || Ok(PathBuf::from("/nonexistent/ghost.txt"))),
                text_item("ok2.txt", "2", &count),
            ],
            &parent,
        );
        let paths = payload.paths();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("ok1.txt") && paths[1].ends_with("ok2.txt"));
        // The failed write left no partial file.
        let staging = paths[0].parent().unwrap();
        assert_eq!(std::fs::read_dir(staging).unwrap().count(), 2);
        payload.cancel();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn path_items_keep_their_files() {
        let parent = scratch("paths");
        let file = parent.join("made elsewhere.txt");
        std::fs::write(&file, "x").unwrap();
        let made = file.clone();
        let mut payload = Payload::new_in(vec![DragItem::from_path("label", move || Ok(made))], &parent);
        assert_eq!(payload.paths(), std::slice::from_ref(&file));
        payload.cancel();
        assert!(file.exists(), "files made by the producer are its own");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn batch_calls_once_and_maps_results() {
        let parent = scratch("batch");
        let calls = Arc::new(AtomicUsize::new(0));
        let files: Vec<PathBuf> = (0..3)
            .map(|i| {
                let f = parent.join(format!("f{i}.bin"));
                std::fs::write(&f, [i]).unwrap();
                f
            })
            .collect();
        let counter = calls.clone();
        // The batch returns one path too few: the last item is skipped.
        let result = files[..2].to_vec();
        let items = DragItem::batch(vec!["f0.bin".into(), "f1.bin".into(), "f2.bin".into()], move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(result)
        });
        assert_eq!(items.len(), 3);
        let mut payload = Payload::new_in(items, &parent);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(payload.paths(), files[..2].to_vec());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let failing = DragItem::batch(vec!["a".into(), "b".into()], || Err(io::Error::other("no space")));
        assert!(Payload::new_in(failing, &parent).paths().is_empty());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn cancel_removes_files_and_finish_keeps_them_for_a_while() {
        let parent = scratch("cleanup");
        let count = Arc::new(AtomicUsize::new(0));
        let mut cancelled = Payload::new_in(vec![text_item("a.txt", "A", &count)], &parent);
        let path = cancelled.paths().remove(0);
        let folder = path.parent().unwrap().to_path_buf();
        assert!(path.exists());
        cancelled.cancel();
        assert!(!folder.exists());

        let mut finished = Payload::new_in(vec![text_item("b.txt", "B", &count)], &parent);
        let path = finished.paths().remove(0);
        finished.finish();
        assert!(path.exists(), "a dropped file stays while the target copies it");
        // The next run's sweep removes it once it is old enough.
        sweep(&parent, Duration::from_secs(3600));
        assert!(path.exists());
        sweep(&parent, Duration::ZERO);
        std::thread::sleep(Duration::from_millis(5));
        sweep(&parent, Duration::ZERO);
        assert!(!path.exists());

        // Dropping a payload without finishing it also cleans up.
        let mut dropped = Payload::new_in(vec![text_item("c.txt", "C", &count)], &parent);
        let path = dropped.paths().remove(0);
        drop(dropped);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn sweep_leaves_other_folders_alone() {
        let parent = scratch("sweep");
        let ours = parent.join("slerp-drag-1-1-000000000");
        let theirs = parent.join("documents");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        sweep(&parent, Duration::ZERO);
        assert!(!ours.exists());
        assert!(theirs.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[cfg(unix)]
    #[test]
    fn staging_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let parent = scratch("private");
        let staging = Staging::create_in(&parent).unwrap();
        let mode = std::fs::metadata(staging.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        staging.remove();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn export_goes_where_a_target_asks() {
        let parent = scratch("export");
        let count = Arc::new(AtomicUsize::new(0));
        let mut payload = Payload::new_in(vec![text_item("a.txt", "AAA", &count)], &parent);
        let dest = parent.join("chosen by the target.txt");
        payload.export_one(0, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "AAA");
        assert!(payload.staging.is_none(), "no folder of our own is needed");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn one_drag_at_a_time() {
        let _lock = GUARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let first = ActiveGuard::acquire().unwrap();
        assert!(ActiveGuard::is_held());
        assert!(matches!(ActiveGuard::acquire(), Err(DragError::Busy)));
        drop(first);
        assert!(!ActiveGuard::is_held());
        drop(ActiveGuard::acquire().unwrap());
    }

    fn session(items: Vec<DragItem>, parent: &Path) -> Session {
        Session::new(Payload::new_in(items, parent), ActiveGuard::acquire().unwrap())
    }

    #[test]
    fn session_drop_path() {
        let _lock = GUARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = scratch("session-drop");
        let count = Arc::new(AtomicUsize::new(0));
        let mut s = session(vec![text_item("a.txt", "A", &count)], &parent);
        assert_eq!(s.phase(), Phase::Dragging);
        assert!(s.data(URI_LIST).is_some());
        s.drop_performed();
        assert_eq!(s.phase(), Phase::Dropped);
        // Data requests still work after the drop.
        let path = s.payload().paths().remove(0);
        assert!(s.data("text/plain").is_some());
        assert_eq!(s.finished(), Outcome::Dropped);
        assert!(path.exists());
        assert!(s.data(URI_LIST).is_none(), "nothing is served after the end");
        assert_eq!(s.cancelled(), Outcome::Dropped, "late events change nothing");
        assert_eq!(s.outcome(), Some(Outcome::Dropped));
        assert!(ActiveGuard::is_held());
        drop(s);
        assert!(!ActiveGuard::is_held());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn session_cancel_after_drop() {
        let _lock = GUARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = scratch("session-cancel-after-drop");
        let count = Arc::new(AtomicUsize::new(0));
        let mut s = session(vec![text_item("a.txt", "A", &count)], &parent);
        // KWin on a drop onto the empty desktop: drop performed, then cancelled.
        s.drop_performed();
        assert_eq!(s.cancelled(), Outcome::Cancelled);
        assert_eq!(count.load(Ordering::SeqCst), 0, "no target asked, so nothing was written");
        drop(s);
        // A target that asked for the files and then never finished.
        let mut s = session(vec![text_item("a.txt", "A", &count)], &parent);
        let path = s.payload().paths().remove(0);
        s.drop_performed();
        assert_eq!(s.cancelled(), Outcome::Cancelled);
        assert!(path.exists(), "files a target may still be copying stay for the grace period");
        drop(s);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn session_cancel_path() {
        let _lock = GUARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let parent = scratch("session-cancel");
        let count = Arc::new(AtomicUsize::new(0));
        let mut s = session(vec![text_item("a.txt", "A", &count)], &parent);
        // A target looked at the list while hovering, then the user pressed Esc.
        let path = s.payload().paths().remove(0);
        assert_eq!(s.cancelled(), Outcome::Cancelled);
        assert!(!path.exists(), "a cancelled drag leaves nothing behind");
        assert!(s.data(URI_LIST).is_none());
        assert_eq!(s.finished(), Outcome::Cancelled, "late events change nothing");
        drop(s);
        // Cancelling before anything was asked writes nothing at all.
        let count = Arc::new(AtomicUsize::new(0));
        let mut s = session(vec![text_item("a.txt", "A", &count)], &parent);
        s.cancelled();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn icons_are_checked() {
        assert!(DragIcon { width: 2, height: 2, rgba: vec![0; 16] }.is_valid());
        assert!(!DragIcon { width: 2, height: 2, rgba: vec![0; 15] }.is_valid());
        assert!(!DragIcon { width: 0, height: 2, rgba: vec![] }.is_valid());
        assert!(!DragIcon { width: 4096, height: 1, rgba: vec![0; 4096 * 4] }.is_valid());
    }
}
