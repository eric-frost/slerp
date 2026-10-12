//! A log of the neighbouring files a loader reads, for loaders that read
//! them deep inside helpers on the calling thread (Quake's skins and WADs,
//! the model formats' textures and `.mtl` files).  Passing a list through
//! every helper would touch dozens of signatures; instead the helper that
//! reads a neighbour calls [`record_neighbour`], and the loader's entry
//! point wraps its work in [`collect_neighbours`] and stores the result in
//! `Info::neighbours`.  Loaders that read neighbours on other threads
//! (glTF's parallel image decoding) collect them themselves.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    /// The files recorded on this thread while a `collect_neighbours` call
    /// runs; `None` when nobody is collecting, so recording costs nothing
    /// then.
    static LOG: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
}

/// Notes that the loader running on this thread read (or looked for)
/// `path`.  Does nothing outside [`collect_neighbours`].
pub fn record_neighbour(path: &Path) {
    LOG.with(|log| {
        if let Some(list) = log.borrow_mut().as_mut()
            && !list.iter().any(|p| p == path)
        {
            list.push(path.to_path_buf());
        }
    });
}

/// Runs `load`, returning what it returned and the files it recorded with
/// [`record_neighbour`], each once, in the order first read.  Calls may
/// nest: an inner call's files are also reported to the outer one.
pub fn collect_neighbours<T>(load: impl FnOnce() -> T) -> (T, Vec<PathBuf>) {
    let outer = LOG.with(|log| log.borrow_mut().replace(Vec::new()));
    // Restores the outer log even if `load` panics, so a caught panic
    // can't leave this thread collecting for ever.
    struct Restore(Option<Option<Vec<PathBuf>>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(outer) = self.0.take() {
                LOG.with(|log| *log.borrow_mut() = outer);
            }
        }
    }
    let mut restore = Restore(Some(outer));
    let result = load();
    let mine = LOG.with(|log| log.borrow_mut().take()).unwrap_or_default();
    let outer = restore.0.take().flatten();
    LOG.with(|log| {
        *log.borrow_mut() = outer.map(|mut o| {
            for p in &mine {
                if !o.contains(p) {
                    o.push(p.clone());
                }
            }
            o
        })
    });
    (result, mine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_what_was_recorded_once_each_and_nests() {
        record_neighbour(Path::new("/ignored"));
        let ((inner, ()), outer) = collect_neighbours(|| {
            record_neighbour(Path::new("/a"));
            let inner = collect_neighbours(|| {
                record_neighbour(Path::new("/b"));
                record_neighbour(Path::new("/a"));
            });
            record_neighbour(Path::new("/a"));
            (inner.1, ())
        });
        assert_eq!(inner, vec![PathBuf::from("/b"), PathBuf::from("/a")]);
        assert_eq!(outer, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        // Nothing is collecting any more.
        let ((), after) = collect_neighbours(|| {});
        assert!(after.is_empty());
    }

    #[test]
    fn a_panicking_loader_stops_collecting() {
        let r = std::panic::catch_unwind(|| collect_neighbours(|| panic!("broken file")));
        assert!(r.is_err());
        LOG.with(|log| assert!(log.borrow().is_none()));
    }
}
