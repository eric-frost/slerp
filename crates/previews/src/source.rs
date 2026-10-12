//! Reading parts of a file by offset, so a multi-gigabyte scene costs only
//! the few kilobytes its preview takes.

use slerp_scene::LoadError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The most bytes read in one piece: far more than any preview.
pub const MAX_READ: u64 = 64 << 20;

/// A file read in pieces, or bytes already in memory (for tests).
pub struct Source {
    inner: Inner,
    len: u64,
}

enum Inner {
    File(File),
    Memory(Vec<u8>),
}

impl Source {
    /// Opens a regular file; devices and pipes are refused, since reading
    /// them could hang.
    pub fn open(path: &Path) -> Result<Source, LoadError> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(LoadError::invalid(format!("{} is not a file", path.display())));
        }
        Ok(Source { inner: Inner::File(File::open(path)?), len: meta.len() })
    }

    pub fn memory(bytes: Vec<u8>) -> Source {
        Source { len: bytes.len() as u64, inner: Inner::Memory(bytes) }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads `n` bytes at `offset`.  Bytes past the end of the file come
    /// back as zeros, as long as the read starts inside it: OLE files often
    /// end in a short last sector.
    pub fn read_at(&mut self, offset: u64, n: u64) -> Result<Vec<u8>, LoadError> {
        if n > MAX_READ {
            return Err(LoadError::TooLarge(format!("a {n}-byte part")));
        }
        if offset >= self.len && n > 0 {
            return Err(LoadError::invalid("a part lies beyond the end of the file"));
        }
        let have = n.min(self.len - offset.min(self.len)) as usize;
        let mut out = vec![0u8; n as usize];
        match &mut self.inner {
            Inner::File(f) => {
                f.seek(SeekFrom::Start(offset))?;
                f.read_exact(&mut out[..have])?;
            }
            Inner::Memory(b) => out[..have].copy_from_slice(&b[offset as usize..offset as usize + have]),
        }
        Ok(out)
    }

    /// Reads exactly `n` bytes at `offset`, all inside the file.
    pub fn exact(&mut self, offset: u64, n: u64) -> Result<Vec<u8>, LoadError> {
        if offset.checked_add(n).is_none_or(|end| end > self.len) {
            return Err(LoadError::invalid("the file is truncated"));
        }
        self.read_at(offset, n)
    }

    /// The first `n` bytes, or the whole file when it is shorter.
    pub fn head(&mut self, n: u64) -> Result<Vec<u8>, LoadError> {
        let n = n.min(self.len);
        if n == 0 {
            return Ok(Vec::new());
        }
        self.read_at(0, n)
    }
}
