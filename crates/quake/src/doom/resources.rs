//! A stack of resource files, as a source port loads them: an IWAD, then
//! PWADs and PK3/PK7 archives, each able to replace what came before.
//! Every lump gets a namespace and a short name, whether it sits between a
//! WAD's markers (F_START ... F_END) or in an archive's folder (`flats/`),
//! so the rest of the loader finds pictures, scripts and maps without
//! caring which kind of file holds them.

use super::archive::{Archive, Namespace, classify};
use super::{Lump, Wad};
use std::borrow::Cow;
use std::collections::HashMap;

/// The largest lump read: map WADs inside archives run to tens of
/// megabytes, pictures to a few.
pub const MAX_LUMP_BYTES: u64 = 512 << 20;

#[derive(Clone, Copy)]
pub(crate) enum ResFile<'a> {
    Wad(&'a Wad<'a>),
    Archive(&'a Archive),
}

/// One lump of the stack.
#[derive(Clone, Debug)]
pub(crate) struct LumpInfo {
    pub file: u32,
    /// Index into the WAD's lumps or the archive's entries.
    pub index: u32,
    pub ns: Namespace,
    /// Upper case, at most 8 characters.
    pub name: String,
    /// The lower-case path inside an archive; empty for WAD lumps.
    pub path: String,
}

/// Where a map's lumps are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapLoc {
    /// A WAD's marker lump, followed by the map's lumps.
    Marker { file: u32, index: u32 },
    /// A WAD inside an archive's `maps/` folder, named after the map.
    Nested { lump: u32 },
}

pub(crate) struct Resources<'a> {
    pub files: Vec<ResFile<'a>>,
    pub lumps: Vec<LumpInfo>,
    /// Lumps by namespace and short name, in load order.
    names: HashMap<(Namespace, String), Vec<u32>>,
    /// The last lump at each archive path.
    paths: HashMap<String, u32>,
}

/// WAD markers that open (`true`) or close a namespace.
fn wad_marker(name: &str) -> Option<(Namespace, bool)> {
    let (stem, open) = if let Some(s) = name.strip_suffix("_START") {
        (s, true)
    } else if let Some(s) = name.strip_suffix("_END") {
        (s, false)
    } else {
        return None;
    };
    let ns = match stem {
        "F" | "FF" | "F1" | "F2" | "F3" => Namespace::Flats,
        "P" | "PP" | "P1" | "P2" | "P3" => Namespace::Patches,
        "S" | "SS" => Namespace::Sprites,
        "TX" => Namespace::Textures,
        "HI" => Namespace::Hires,
        "C" | "A" | "V" => Namespace::Other,
        _ => return None,
    };
    Some((ns, open))
}

impl<'a> Resources<'a> {
    pub fn new(files: Vec<ResFile<'a>>) -> Resources<'a> {
        let mut r = Resources { files, lumps: Vec::new(), names: HashMap::new(), paths: HashMap::new() };
        for fi in 0..r.files.len() {
            match r.files[fi] {
                ResFile::Wad(w) => {
                    // Inner markers (F1_START) close into the outer
                    // namespace's end, which is all that matters here.
                    let mut ns = Namespace::Global;
                    for (i, Lump { name, data }) in w.lumps.iter().enumerate() {
                        if let Some((n, open)) = wad_marker(name) {
                            ns = if open { n } else { Namespace::Global };
                            continue;
                        }
                        if data.is_empty() {
                            continue;
                        }
                        r.push(LumpInfo {
                            file: fi as u32,
                            index: i as u32,
                            ns,
                            name: name.clone(),
                            path: String::new(),
                        });
                    }
                }
                ResFile::Archive(a) => {
                    for (i, e) in a.entries.iter().enumerate() {
                        let (ns, name) = classify(&e.path);
                        let path = e.path.to_ascii_lowercase();
                        r.push(LumpInfo { file: fi as u32, index: i as u32, ns, name, path });
                    }
                }
            }
        }
        r
    }

    fn push(&mut self, l: LumpInfo) {
        let id = self.lumps.len() as u32;
        self.names.entry((l.ns, l.name.clone())).or_default().push(id);
        if !l.path.is_empty() {
            self.paths.insert(l.path.clone(), id);
        }
        self.lumps.push(l);
    }

    /// A lump's bytes: borrowed from a WAD, unpacked from an archive.
    pub fn data(&self, id: u32) -> Option<Cow<'a, [u8]>> {
        let l = self.lumps.get(id as usize)?;
        match self.files[l.file as usize] {
            ResFile::Wad(w) => w.lumps.get(l.index as usize).map(|x| Cow::Borrowed(x.data)),
            ResFile::Archive(a) => a.read(l.index as usize, MAX_LUMP_BYTES).ok().map(Cow::Owned),
        }
    }

    /// Several lumps' bytes, reading each 7z block once.  Zip entries are
    /// read in parallel.
    pub fn data_many(&self, ids: &[u32]) -> Vec<Option<Cow<'a, [u8]>>> {
        use rayon::prelude::*;
        let mut out: Vec<Option<Cow<'a, [u8]>>> = vec![None; ids.len()];
        // 1. Solid archives: one pass per archive.
        let mut solid: HashMap<u32, Vec<usize>> = HashMap::new();
        for (k, &id) in ids.iter().enumerate() {
            if let Some(l) = self.lumps.get(id as usize)
                && let ResFile::Archive(a) = self.files[l.file as usize]
                && a.is_solid()
            {
                solid.entry(l.file).or_default().push(k);
            }
        }
        for (file, ks) in &solid {
            let ResFile::Archive(a) = self.files[*file as usize] else { continue };
            let wanted: Vec<usize> = ks.iter().map(|&k| self.lumps[ids[k] as usize].index as usize).collect();
            for (k, d) in ks.iter().zip(a.read_many(&wanted, MAX_LUMP_BYTES)) {
                out[*k] = d.map(Cow::Owned);
            }
        }
        // 2. Everything else.
        let mut done = vec![false; ids.len()];
        for &k in solid.values().flatten() {
            done[k] = true;
        }
        let rest: Vec<(usize, Option<Cow<'a, [u8]>>)> =
            ids.par_iter().enumerate().filter(|(k, _)| !done[*k]).map(|(k, &id)| (k, self.data(id))).collect();
        for (k, d) in rest {
            out[k] = d;
        }
        out
    }

    /// The last lump called `name` in namespace `ns`.
    pub fn last(&self, ns: Namespace, name: &str) -> Option<u32> {
        self.names.get(&(ns, name.to_ascii_uppercase())).and_then(|v| v.last().copied())
    }

    /// Every lump called `name` in `ns`, in load order.
    pub fn all(&self, ns: Namespace, name: &str) -> &[u32] {
        self.names.get(&(ns, name.to_ascii_uppercase())).map_or(&[], |v| v.as_slice())
    }

    /// Every lump of a namespace, in load order.
    pub fn in_namespace(&self, ns: Namespace) -> impl Iterator<Item = (u32, &LumpInfo)> {
        self.lumps.iter().enumerate().filter(move |(_, l)| l.ns == ns).map(|(i, l)| (i as u32, l))
    }

    /// The last lump at an archive path, compared without case.
    pub fn by_path(&self, path: &str) -> Option<u32> {
        let p = path.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase();
        self.paths.get(&p).copied()
    }

    /// The text of a lump that a script's `#include` names: a full path,
    /// else a lump name.
    pub fn include(&self, name: &str) -> Option<Vec<u8>> {
        let id = self.by_path(name).or_else(|| self.last(Namespace::Global, name))?;
        self.data(id).map(Cow::into_owned)
    }

    /// The texts of every global lump called `name` (TEXTURES, MAPINFO),
    /// in load order, so later definitions replace earlier ones.
    pub fn texts(&self, name: &str) -> Vec<Vec<u8>> {
        self.all(Namespace::Global, name).iter().filter_map(|&id| self.data(id).map(Cow::into_owned)).collect()
    }

    /// The file a lump came from.
    pub fn file_of(&self, id: u32) -> Option<u32> {
        self.lumps.get(id as usize).map(|l| l.file)
    }

    /// Every map in load order, by upper-case name.  A name can repeat;
    /// the last one wins.
    pub fn maps(&self) -> Vec<(String, MapLoc)> {
        let mut out = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            match f {
                ResFile::Wad(w) => {
                    for (i, name) in w.maps() {
                        out.push((name.to_string(), MapLoc::Marker { file: fi as u32, index: i as u32 }));
                    }
                }
                ResFile::Archive(_) => {
                    for (id, l) in self.lumps.iter().enumerate() {
                        if l.file == fi as u32
                            && l.ns == Namespace::Maps
                            && l.path.ends_with(".wad")
                            && !l.name.is_empty()
                        {
                            out.push((l.name.clone(), MapLoc::Nested { lump: id as u32 }));
                        }
                    }
                }
            }
        }
        out
    }

    /// The last PLAYPAL's first palette.
    pub fn palette(&self) -> Option<Vec<u8>> {
        self.all(Namespace::Global, "PLAYPAL")
            .iter()
            .rev()
            .filter_map(|&id| self.data(id))
            .find(|d| d.len() >= 768)
            .map(|d| d[..768].to_vec())
    }
}

/// The entries at an archive's root that are WADs: GZDoom loads them as
/// resource files of their own, right after the archive.
pub(crate) fn embedded_wads(a: &Archive) -> Vec<usize> {
    a.entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.path.contains('/') && e.path.to_ascii_lowercase().ends_with(".wad"))
        .map(|(i, _)| i)
        .collect()
}
