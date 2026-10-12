//! Writing items out as files, and listing them as text: the code behind
//! `slerp contents extract` and `slerp contents list`, and behind the
//! panel's Extract button.
//!
//! Extraction never leaves its output folder: every name is made safe
//! ([`safe_relative_path`](super::safe_relative_path)), folders are made one
//! level at a time and refused if they turn out to be symbolic links, files
//! are created with `create_new` so existing files are kept (unless `force`
//! replaces them, by deleting them first, so a link at the destination is
//! never followed), and the real location of every file is checked against
//! the output folder once it exists.

use super::{Contents, ContentsError, Format, Item, ItemId, Result, Source, names};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// How to extract.
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    /// The format asked for ([`Format::Auto`] converts pictures, sounds and
    /// music and keeps the rest as stored).
    pub format: Format,
    /// Replace files that exist.
    pub force: bool,
    /// Stop after writing this many bytes in all.  `None`: a thousand times
    /// the archive's own size, at least 1 GiB, which no honest archive
    /// reaches and a zip bomb does at once.
    pub max_total: Option<u64>,
    /// Write empty marker items too (`S_START` and the like).
    pub markers: bool,
    /// Put every file directly in the output folder, ignoring the items'
    /// folders (what pasting into a file manager wants for single items).
    pub flat: bool,
    /// Leave out this many leading parts of every item's folder (to extract
    /// a folder as itself, without the folders above it).
    pub strip: usize,
    /// GLB files of nodes and meshes: move them to the origin instead of
    /// keeping their place in the scene.
    pub centre: bool,
    /// GLB files of clips: the skeleton alone, without the meshes it moves.
    pub skeleton_only: bool,
}

impl Default for ExtractOptions {
    fn default() -> ExtractOptions {
        ExtractOptions {
            format: Format::Auto,
            force: false,
            max_total: None,
            markers: false,
            flat: false,
            strip: 0,
            centre: false,
            skeleton_only: false,
        }
    }
}

/// What extraction did.
#[derive(Debug, Default)]
pub struct ExtractReport {
    /// The files written, in order.
    pub written: Vec<PathBuf>,
    /// Items not written, and why (a file that exists, an unreadable item).
    pub skipped: Vec<(String, String)>,
    pub bytes: u64,
}

/// Selects items by what the user typed: an exact path, a glob (`*.png`,
/// `Sprites/TROO*`, `**/THINGS`), a name without folders, or a folder (all
/// the items beneath it).  Comparison ignores case.  Returns the items in
/// archive order, each once; a pattern that matches nothing is an error.
pub fn select(contents: &Contents, patterns: &[String]) -> Result<Vec<ItemId>> {
    let mut picked = vec![false; contents.items().len()];
    for pattern in patterns {
        let pat = pattern.replace('\\', "/");
        let pat = pat.trim_start_matches('/').trim_end_matches('/');
        let mut found = false;
        let folder_prefix = format!("{}/", pat.to_ascii_lowercase());
        for item in contents.items() {
            let lower = item.path.to_ascii_lowercase();
            let hit = names::glob_match(pat, &item.path)
                || (!pat.contains('/') && names::glob_match(pat, item.name()))
                || lower.starts_with(&folder_prefix);
            if hit {
                picked[item.id.0] = true;
                found = true;
            }
        }
        if !found {
            return Err(ContentsError::Unsupported(format!("no item matches {pattern}")));
        }
    }
    Ok(contents.items().iter().filter(|i| picked[i.id.0]).map(|i| i.id).collect())
}

/// Extracts `ids` under `out` (created if missing).
pub fn extract(contents: &Contents, ids: &[ItemId], out: &Path, options: &ExtractOptions) -> Result<ExtractReport> {
    std::fs::create_dir_all(out)?;
    let root = std::fs::canonicalize(out)?;
    let cap = options.max_total.unwrap_or_else(|| {
        let size = contents.path().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        size.saturating_mul(1000).max(1 << 30)
    });
    let mut report = ExtractReport::default();

    // 1. Plan the files: each item's format and a unique, safe path.
    struct Job<'a> {
        item: &'a Item,
        format: Format,
        relative: PathBuf,
    }
    let mut taken: HashSet<String> = HashSet::new();
    let mut jobs: Vec<Job> = Vec::new();
    for &id in ids {
        let Some(item) = contents.item(id) else { continue };
        if item.locked {
            report.skipped.push((item.path.clone(), "it can't be read (encrypted, or outside the file)".into()));
            continue;
        }
        if item.size == 0 && matches!(item.source, Source::Entry(_)) && !options.markers {
            // Markers and empty lumps hold nothing to save.
            continue;
        }
        if options.format == Format::Raw && !item.formats.contains(&Format::Raw) {
            // Derived items have no stored bytes.
            continue;
        }
        if item.formats.is_empty() {
            // Cameras, blend shapes, rig controls: nothing to save alone.
            continue;
        }
        let format = item.resolve(options.format);
        let Some(file) = contents.file_name(id, format) else { continue };
        let folder: String = item.folder().split('/').skip(options.strip).collect::<Vec<_>>().join("/");
        let mut relative =
            if options.flat || folder.is_empty() { PathBuf::new() } else { names::safe_relative_path(&folder) };
        relative.push(names::safe_component(&file));
        let relative = unique(relative, &mut taken);
        jobs.push(Job { item, format, relative });
    }

    // 2. Composite textures in one batch, since composing needs the whole
    //    file's resources.
    let mut composed: std::collections::HashMap<String, Option<slerp_quake::convert::Image>> = Default::default();
    let names: Vec<&str> = jobs
        .iter()
        .filter_map(|j| match &j.item.source {
            Source::Composite { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if !names.is_empty() {
        match contents.render_composites(&names) {
            Ok(images) => composed.extend(names.iter().map(|n| n.to_string()).zip(images)),
            Err(e) => {
                for j in jobs.iter().filter(|j| matches!(j.item.source, Source::Composite { .. })) {
                    report.skipped.push((j.item.path.clone(), e.to_string()));
                }
            }
        }
    }

    // 3. Write them.
    for job in jobs {
        let path = match &job.item.source {
            Source::Composite { name, .. } => {
                let Some(Some(image)) = composed.remove(name) else {
                    if !report.skipped.iter().any(|(p, _)| *p == job.item.path) {
                        report.skipped.push((job.item.path.clone(), "its patches couldn't be read".into()));
                    }
                    continue;
                };
                let bytes = super::export::png_bytes(&image)?;
                write_file(&root, &job.relative, options.force, |f| {
                    Ok(f.write_all(&bytes).map(|_| bytes.len() as u64)?)
                })
            }
            Source::Entry(i) if job.format == Format::Raw => {
                let archive = contents.archive().ok_or(ContentsError::NoSuchItem)?;
                let entry = archive.entry(*i).ok_or(ContentsError::NoSuchItem)?;
                write_file(&root, &job.relative, options.force, |f| Ok(archive.read_to(entry, f)?))
            }
            Source::Entry(_) | Source::Piece(_) => {
                let one = ExtractOptions { format: job.format, ..options.clone() };
                let (_, bytes) = match contents.export_with(job.item.id, &one) {
                    Ok(x) => x,
                    Err(e) => {
                        report.skipped.push((job.item.path.clone(), e.to_string()));
                        continue;
                    }
                };
                write_file(&root, &job.relative, options.force, |f| {
                    Ok(f.write_all(&bytes).map(|_| bytes.len() as u64)?)
                })
            }
        };
        match path {
            Ok(Written::Done(path, n)) => {
                report.bytes += n;
                report.written.push(path);
                if report.bytes > cap {
                    return Err(ContentsError::TooLargeOutput(report.bytes, cap));
                }
            }
            Ok(Written::Exists) => report.skipped.push((job.item.path.clone(), "the file exists (use --force)".into())),
            Err(ContentsError::Unsafe(m)) => return Err(ContentsError::Unsafe(m)),
            Err(e) => report.skipped.push((job.item.path.clone(), e.to_string())),
        }
    }
    Ok(report)
}

enum Written {
    Done(PathBuf, u64),
    Exists,
}

/// Makes a relative path unique among those already taken, comparing
/// without case (macOS and Windows file systems do), by putting `~2`, `~3`
/// before the extension.
fn unique(relative: PathBuf, taken: &mut HashSet<String>) -> PathBuf {
    let key = |p: &Path| p.to_string_lossy().to_ascii_lowercase();
    if taken.insert(key(&relative)) {
        return relative;
    }
    let name = relative.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for n in 2.. {
        let candidate = relative.with_file_name(format!("{stem}~{n}{ext}"));
        if taken.insert(key(&candidate)) {
            return candidate;
        }
    }
    unreachable!()
}

/// Creates `relative` under `root` and fills it.  Folders are created one at
/// a time and must not be links; the file is created new.
fn write_file(
    root: &Path,
    relative: &Path,
    force: bool,
    fill: impl FnOnce(&mut std::fs::File) -> Result<u64>,
) -> Result<Written> {
    // Only plain names can be in the path.
    let parts: Vec<&std::ffi::OsStr> = relative
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(ContentsError::Unsafe(format!("{} isn't a plain relative path", relative.display()))),
        })
        .collect::<Result<_>>()?;
    let Some((file, folders)) = parts.split_last() else {
        return Err(ContentsError::Unsafe("an empty path".into()));
    };
    let mut dir = root.to_path_buf();
    for part in folders {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(ContentsError::Unsafe(format!("{} is a symbolic link", dir.display())));
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(ContentsError::Unsupported(format!("{} is a file, not a folder", dir.display()))),
            Err(_) => std::fs::create_dir(&dir)?,
        }
    }
    let path = dir.join(file);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut handle = match options.open(&path) {
        Ok(h) => h,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !force {
                return Ok(Written::Exists);
            }
            // Delete rather than truncate, so a link there is replaced, not
            // followed.  A folder in the way stays an error.
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                return Err(ContentsError::Unsupported(format!("{} is a folder", path.display())));
            }
            std::fs::remove_file(&path)?;
            options.open(&path)?
        }
        Err(e) => return Err(e.into()),
    };
    // The folder's real place must still be inside the root.
    let inside = std::fs::canonicalize(&dir).is_ok_and(|d| d.starts_with(root));
    if !inside {
        drop(handle);
        let _ = std::fs::remove_file(&path);
        return Err(ContentsError::Unsafe(format!("{} leaves the output folder", path.display())));
    }
    match fill(&mut handle) {
        Ok(n) => Ok(Written::Done(path, n)),
        Err(e) => {
            drop(handle);
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// The listing `slerp contents list` prints: the folders as a tree with
/// each item's size and kind, or with `flat` one tab-separated line per
/// item (path, size, kind, formats) for scripts.
pub fn list_text(contents: &Contents, flat: bool) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let total: u64 = contents.items().iter().map(|i| i.size).sum();
    if !flat {
        let count = contents.items().len();
        let _ = match total {
            0 => writeln!(out, "{}: {count} items", contents.label()),
            _ => writeln!(out, "{}: {count} items, {total} bytes", contents.label()),
        };
        for w in contents.warnings() {
            let _ = writeln!(out, "warning: {w}");
        }
    }
    // Items that are folders too (a node and its children) head their
    // folder themselves.
    let item_paths: std::collections::HashSet<&str> = contents.items().iter().map(|i| i.path.as_str()).collect();
    let mut shown: Vec<&str> = Vec::new();
    for item in contents.items() {
        if flat {
            let formats: Vec<&str> = item.formats.iter().map(|f| f.name()).collect();
            let _ = writeln!(out, "{}\t{}\t{}\t{}", item.path, item.size, item.kind.label(), formats.join(","));
            continue;
        }
        // Folder headers for the parts of the folder path not yet shown.
        let folders: Vec<&str> = item.folder().split('/').filter(|p| !p.is_empty()).collect();
        let common = shown.iter().zip(&folders).take_while(|(a, b)| a == b).count();
        for (depth, part) in folders.iter().enumerate().skip(common) {
            let folder = folders[..=depth].join("/");
            if !item_paths.contains(folder.as_str()) {
                let _ = writeln!(out, "{}{part}/", "  ".repeat(depth));
            }
        }
        shown = folders.clone();
        let indent = "  ".repeat(folders.len());
        let name = if item.name().len() > 36 {
            format!("{}...", &item.name()[..item.name().floor_char_boundary(33)])
        } else {
            item.name().to_string()
        };
        let size = if item.size == 0 && !matches!(item.source, Source::Entry(_)) {
            "-".to_string()
        } else {
            item.size.to_string()
        };
        let _ = writeln!(
            out,
            "{indent}{name:<width$} {size:>11}  {kind:<8}{detail}",
            width = 36usize.saturating_sub(indent.len()).max(8),
            kind = item.kind.label(),
            detail = if item.detail.is_empty() { String::new() } else { format!(" {}", item.detail) },
        );
    }
    out
}

/*┌─────────────────╮
| Materialising   |
└─────────────────╯*/
//region Materialising

/// What is selected in the panel: single items, and whole folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selected {
    Item(ItemId),
    /// A folder's path (`Sprites`, `MAP01`, `textures/walls`): everything
    /// beneath it, with its own structure.
    Folder(String),
}

impl Contents {
    /// Writes a selection into the existing folder `batch`, converted as
    /// `format` says, and returns the paths to hand to a file manager (a
    /// clipboard file list, a drag): one per selected item, which sit
    /// directly in `batch`, and one per selected folder, which holds its
    /// items as the archive does.  Items that can't be read are left out.
    /// `options` gives the format and the GLB choices.
    pub fn materialize(&self, selection: &[Selected], options: &ExtractOptions, batch: &Path) -> Result<Vec<PathBuf>> {
        let format = options.format;
        let shared = ExtractOptions { flat: false, strip: 0, ..options.clone() };
        let options = ExtractOptions { flat: true, ..shared.clone() };
        let mut paths = Vec::new();
        // Single items in one pass, so same-named ones are told apart.
        let loose: Vec<ItemId> = selection
            .iter()
            .filter_map(|s| match s {
                Selected::Item(id) => Some(*id),
                Selected::Folder(_) => None,
            })
            .collect();
        let report = extract(self, &loose, batch, &options)?;
        paths.extend(report.written);
        for s in selection {
            let Selected::Folder(folder) = s else { continue };
            let strip = folder.trim_end_matches('/').split('/').count().saturating_sub(1);
            let ids = self.items_in(folder);
            let report = extract(self, &ids, batch, &ExtractOptions { format, strip, ..shared.clone() })?;
            let name = names::safe_component(folder.trim_end_matches('/').rsplit('/').next().unwrap_or(folder));
            if !report.written.is_empty() {
                paths.push(batch.join(name));
            }
        }
        Ok(paths)
    }
}

//endregion

/*┌─────────────────╮
| Command line    |
└─────────────────╯*/
//region Command line

/// What `slerp contents list FILE` prints.
pub fn list_file(path: &Path, flat: bool) -> Result<String> {
    Ok(list_text(&Contents::open(path)?, flat))
}

/// What `slerp contents extract FILE [ITEM...] --out DIR` does: extracts the
/// items `patterns` select (all of them when none are given) under `out`.
pub fn extract_file(path: &Path, patterns: &[String], out: &Path, options: &ExtractOptions) -> Result<ExtractReport> {
    let contents = Contents::open(path)?;
    let ids = if patterns.is_empty() {
        contents.items().iter().map(|i| i.id).collect()
    } else {
        select(&contents, patterns)?
    };
    extract(&contents, &ids, out, options)
}

//endregion
