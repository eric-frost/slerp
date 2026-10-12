//! The per-user temporary folder that items are written to when they are
//! pasted or dragged out of the panel, and the previews the viewer loads.
//! It lives in `$XDG_RUNTIME_DIR/slerp/contents/<pid>` (a folder only the
//! user can enter, on a memory-backed file system that the system empties
//! at log-out), else under the state directory.  The viewer removes its
//! folder when it exits, and the next start removes the folders of
//! processes that no longer exist.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A folder for one process's temporary files.
pub struct TempFolder {
    root: PathBuf,
    counter: AtomicU64,
}

fn contents_dir() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    match runtime {
        Some(r) if cfg!(unix) && !cfg!(target_os = "macos") => Some(r.join("slerp").join("contents")),
        _ => crate::config::state_dir().map(|d| d.join("contents")),
    }
}

impl TempFolder {
    /// Makes (or reuses) this process's folder.
    pub fn new() -> std::io::Result<TempFolder> {
        let base = contents_dir().ok_or_else(|| std::io::Error::other("no folder for temporary files"))?;
        let root = base.join(std::process::id().to_string());
        create_private(&root)?;
        Ok(TempFolder { root, counter: AtomicU64::new(0) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A new empty folder inside, for one batch of files, so names from
    /// different batches never clash and a pasted file keeps its own name.
    pub fn batch(&self) -> std::io::Result<PathBuf> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let dir = self.root.join(format!("b{n}"));
        create_private(&dir)?;
        Ok(dir)
    }

    /// Removes everything in the folder and the folder.
    pub fn clean(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn create_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Removes the folders earlier runs left (a crash, a kill): those of
/// processes that are gone (on Linux), or older than a day (elsewhere).
pub fn clean_stale() {
    let Some(base) = contents_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&base) else { return };
    let me = std::process::id().to_string();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == me {
            continue;
        }
        let gone = if cfg!(target_os = "linux") {
            name.parse::<u32>().is_ok() && !Path::new("/proc").join(&name).exists()
        } else {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600))
        };
        if gone {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
