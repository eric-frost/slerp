//! Linked libraries: data-blocks a `.blend` file uses from other `.blend`
//! files.  The using file keeps only a placeholder (an `ID` block with the
//! data-block's name) and a `Library` block naming the other file, by a
//! path relative to itself (`//props/chair.blend`) or absolute, or packed
//! inside it.  Production files link characters, sets and props this way,
//! so a scene without its libraries can be nearly empty.
//!
//! Libraries are read once each, including the libraries they link in
//! turn, up to a few levels deep and a total size, and only when
//! [`Limits::neighbours`] allows reading other files (packed libraries
//! always).

use crate::file::{self, BlendFile};
use crate::view::Obj;
use slerp_scene::{Limits, record_neighbour};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Libraries deeper than this aren't followed.
const MAX_DEPTH: usize = 4;
/// At most this many library files are read.
const MAX_LIBRARIES: usize = 64;

pub struct Library {
    pub file: BlendFile,
    /// The folder its own relative paths start from.
    pub dir: Option<PathBuf>,
    /// Its own data-blocks by full name ("OBChair", "GRProps"), which
    /// linking files ask for.
    ids: HashMap<String, u32>,
}

/// Every library a file links, directly or through other libraries.
#[derive(Default)]
pub struct Libraries {
    pub list: Vec<Library>,
    /// Library index by (file, Library block address in it), the file
    /// being the main file (`usize::MAX`) or a library.
    links: HashMap<(usize, u64), usize>,
    pub warnings: Vec<String>,
}

impl Libraries {
    /// Reads the libraries `main` (in folder `dir`) links.
    pub fn read(main: &BlendFile, dir: Option<&Path>, limits: &Limits) -> Libraries {
        let mut libs = Libraries::default();
        let mut by_path: HashMap<PathBuf, usize> = HashMap::new();
        let mut budget = limits.max_file_bytes.min(4 << 30);
        // (file index or usize::MAX for main, depth) still to scan.
        let mut queue = vec![(usize::MAX, 0usize)];
        while let Some((from, depth)) = queue.pop() {
            let (f, base) = match from {
                usize::MAX => (main, dir.map(Path::to_path_buf)),
                i => (&libs.list[i].file, libs.list[i].dir.clone()),
            };
            // (Library block address, packed bytes, file found, stored path)
            type Found = (u64, Option<Vec<u8>>, Option<PathBuf>, String);
            let mut found: Vec<Found> = Vec::new();
            for (bi, b) in f.blocks.iter().enumerate() {
                if !(b.is_id() && b.id_code() == *b"LI") {
                    continue;
                }
                let Some(li) = Obj::of_block(f, bi as u32) else { continue };
                let stored = li.string("name");
                let stored = if stored.is_empty() { li.string("filepath") } else { stored };
                let packed = li.deref("packedfile").and_then(|pf| {
                    let size = pf.int(&["size"])?.max(0) as usize;
                    pf.raw("data")?.get(..size).map(<[u8]>::to_vec)
                });
                let path = if packed.is_none() { locate(&stored, base.as_deref()) } else { None };
                found.push((b.old, packed, path, stored));
            }
            for (addr, packed, path, stored) in found {
                if let Some(&i) = path.as_ref().and_then(|p| by_path.get(p)) {
                    libs.links.insert((from, addr), i);
                    continue;
                }
                if depth >= MAX_DEPTH || libs.list.len() >= MAX_LIBRARIES {
                    continue;
                }
                let bytes = match (&packed, &path) {
                    (Some(b), _) => b.clone(),
                    (None, Some(p)) if limits.neighbours => match std::fs::read(p) {
                        Ok(b) if (b.len() as u64) <= budget => b,
                        Ok(_) => {
                            libs.warnings.push(format!("library {stored} is too large"));
                            continue;
                        }
                        Err(_) => {
                            libs.warnings.push(format!("library not found: {stored}"));
                            continue;
                        }
                    },
                    (None, None) if limits.neighbours => {
                        libs.warnings.push(format!("library not found: {stored}"));
                        continue;
                    }
                    _ => {
                        libs.warnings.push(format!("library {stored} is a separate file"));
                        continue;
                    }
                };
                budget = budget.saturating_sub(bytes.len() as u64);
                let cap = (bytes.len() as u64).saturating_mul(64).saturating_add(64 << 20).min(budget.max(1 << 20));
                let parsed = file::unpack(&bytes, cap).and_then(|(data, packing)| BlendFile::parse(data, packing));
                let file = match parsed {
                    Ok(f) => f,
                    Err(e) => {
                        libs.warnings.push(format!("library {stored}: {e}"));
                        continue;
                    }
                };
                let ids = id_names(&file);
                let lib_dir = path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).or_else(|| base.clone());
                libs.list.push(Library { file, dir: lib_dir, ids });
                let i = libs.list.len() - 1;
                if let Some(p) = path {
                    by_path.insert(p, i);
                }
                libs.links.insert((from, addr), i);
                queue.push((i, depth + 1));
            }
        }
        libs
    }

    /// Which file `f` is: `usize::MAX` for the main file, else a library
    /// index.
    fn index_of(&self, f: &BlendFile) -> usize {
        self.list.iter().position(|l| std::ptr::eq(&l.file, f)).unwrap_or(usize::MAX)
    }

    /// The real data-block behind `id` when it is linked from a library,
    /// found by name in that library (and onwards through libraries that
    /// link it again).  None when `id` isn't linked or the library or
    /// data-block can't be found.
    pub fn resolve<'a>(&'a self, id: &Obj<'a>) -> Option<Obj<'a>> {
        let mut cur = *id;
        for _ in 0..=MAX_DEPTH {
            let head = if cur.type_name() == "ID" { Some(cur) } else { cur.sub("id") }?;
            let lib = head.ptr("lib");
            if lib == 0 {
                return (!std::ptr::eq(cur.f, id.f) || cur.at != id.at).then_some(cur);
            }
            let full = head.string("name");
            let i = *self.links.get(&(self.index_of(cur.f), lib))?;
            let target = &self.list[i];
            let bi = *target.ids.get(&full)?;
            cur = Obj::of_block(&target.file, bi)?;
        }
        None
    }

    /// The folder of the file `f` belongs to, when it is a library.
    pub fn dir_of(&self, f: &BlendFile) -> Option<Option<&Path>> {
        let i = self.index_of(f);
        (i != usize::MAX).then(|| self.list[i].dir.as_deref())
    }
}

/// Every local data-block of `f` by its full name, prefix included.
fn id_names(f: &BlendFile) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    for (i, b) in f.blocks.iter().enumerate() {
        if !b.is_id() || b.code == *b"ID\0\0" || b.code == *b"LI\0\0" {
            continue;
        }
        if let Some(o) = Obj::of_block(f, i as u32)
            && let Some(id) = o.sub("id")
        {
            out.entry(id.string("name")).or_insert(i as u32);
        }
    }
    out
}

/// Where a library's file is: its stored path (relative to `base` when it
/// starts with `//`), else its file name in `base`.
fn locate(stored: &str, base: Option<&Path>) -> Option<PathBuf> {
    let unified = stored.replace('\\', "/");
    let mut candidates = Vec::new();
    if let Some(rel) = unified.strip_prefix("//") {
        if let Some(b) = base {
            candidates.push(b.join(rel));
        }
    } else if !unified.is_empty() {
        candidates.push(PathBuf::from(&unified));
    }
    if let (Some(b), Some(name)) = (base, unified.rsplit('/').next().filter(|n| !n.is_empty())) {
        candidates.push(b.join(name));
    }
    for c in &candidates {
        record_neighbour(c);
    }
    candidates.into_iter().find(|c| c.is_file()).map(|c| c.canonicalize().unwrap_or(c))
}
