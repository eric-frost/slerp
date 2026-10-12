//! Valve's VPK packs (versions 1 and 2), read-only, through `slerp-archive`:
//! the `*_dir.vpk` file holds a directory tree and small files' bytes, and
//! the rest live in numbered archives beside it.  Materials and textures
//! are looked up by path, in any case and with either slash.

use slerp_archive::{Archive, ArchiveKind};
use std::path::{Path, PathBuf};

pub struct Vpk {
    archive: Archive,
    /// Lower-case paths, `/`-separated, in directory order, once each.
    names: Vec<String>,
}

impl std::fmt::Debug for Vpk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Vpk({:?}, {} files)", self.archive.path(), self.names.len())
    }
}

fn invalid(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.into())
}

impl Vpk {
    /// Opens a pack by its directory file (`*_dir.vpk`, or a single-file
    /// pack), reading only the directory tree.
    pub fn open(dir: &Path) -> std::io::Result<Vpk> {
        let archive = Archive::open(dir).map_err(|e| match e {
            slerp_archive::ArchiveError::Io(e) => e,
            e => invalid(e.to_string()),
        })?;
        if archive.kind() != ArchiveKind::Vpk {
            return Err(invalid("not a VPK"));
        }
        let mut seen = std::collections::HashSet::new();
        let names =
            archive.entries().iter().map(|e| e.path.to_ascii_lowercase()).filter(|n| seen.insert(n.clone())).collect();
        Ok(Vpk { archive, names })
    }

    /// Every file's path, lower case with `/` separators, in directory
    /// order.
    #[allow(dead_code)]
    pub fn list(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    pub fn contains(&self, path: &str) -> bool {
        !self.archive.find(path).is_empty()
    }

    /// The bytes of the file at `path` (any case, either slash).  When the
    /// pack lists it twice, the later one wins.
    pub fn read(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let &at = self.archive.find(path).last().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let entry = self.archive.entry(at).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        self.archive.read(entry).map_err(|e| match e {
            slerp_archive::ArchiveError::Io(e) => e,
            e => invalid(e.to_string()),
        })
    }

    /// The pack's directory file, for watching.
    pub fn path(&self) -> &Path {
        self.archive.path().unwrap_or(Path::new(""))
    }

    /// The directory file and the numbered archives beside it.
    #[allow(dead_code)]
    pub fn files(&self) -> Vec<PathBuf> {
        self.archive.files()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a version `version` pack: `small` files stored in the
    /// directory (half as preload bytes), `big` ones in archive 0.
    pub fn write(dir: &Path, version: u32, small: &[(&str, &[u8])], big: &[(&str, &[u8])]) -> PathBuf {
        slerp_archive::testutil::vpk(dir, version, small, big)
    }

    #[test]
    fn reads_both_versions() {
        for version in [1, 2] {
            let dir = std::env::temp_dir().join(format!("slerp-vpk-{}-{version}", std::process::id()));
            let path = write(
                &dir,
                version,
                &[("materials/dev/dev_a.vmt", b"\"LightmappedGeneric\" { }"), ("readme.txt", b"hi")],
                &[("materials/dev/dev_a.vtf", &[7u8; 300])],
            );
            let vpk = Vpk::open(&path).unwrap();
            let mut names: Vec<&str> = vpk.list().collect();
            names.sort();
            assert_eq!(names, ["materials/dev/dev_a.vmt", "materials/dev/dev_a.vtf", "readme.txt"]);
            assert_eq!(vpk.read("Materials\\DEV\\dev_a.vmt").unwrap(), b"\"LightmappedGeneric\" { }");
            assert_eq!(vpk.read("readme.txt").unwrap(), b"hi");
            assert_eq!(vpk.read("materials/dev/dev_a.vtf").unwrap(), vec![7u8; 300]);
            assert!(vpk.read("nothing").is_err());
            assert!(vpk.contains("README.TXT"));
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn damaged_packs_fail_cleanly() {
        let dir = std::env::temp_dir().join(format!("slerp-vpk-bad-{}", std::process::id()));
        let path = write(&dir, 2, &[("a/b.vmt", b"0123456789")], &[("a/c.vtf", &[1u8; 64])]);
        let good = std::fs::read(&path).unwrap();
        let bad = dir.join("bad_dir.vpk");
        for cut in [0, 3, 11, 27, 30, good.len() - 1] {
            std::fs::write(&bad, &good[..cut]).unwrap();
            if let Ok(v) = Vpk::open(&bad) {
                for n in v.list().map(String::from).collect::<Vec<_>>() {
                    let _ = v.read(&n);
                }
            }
        }
        for m in crate::testutil::mutations(&good) {
            std::fs::write(&bad, &m).unwrap();
            if let Ok(v) = Vpk::open(&bad) {
                for n in v.list().map(String::from).collect::<Vec<_>>() {
                    let _ = v.read(&n);
                }
            }
        }
        // The archive is gone: the directory still lists it, reading fails.
        std::fs::remove_file(dir.join("pak01_000.vpk")).unwrap();
        let v = Vpk::open(&path).unwrap();
        assert!(v.read("a/c.vtf").is_err());
        assert_eq!(v.read("a/b.vmt").unwrap(), b"0123456789");
        let _ = std::fs::remove_dir_all(dir);
    }
}
