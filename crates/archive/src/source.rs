//! Where an archive's bytes come from: memory (a file already read, or an
//! archive inside another) or a file on disk, read at offsets so a
//! gigabyte archive costs only the parts that are used.  Every read is
//! checked against the real length before anything is allocated, so a
//! count or size taken from a damaged directory can't ask for more memory
//! than the file has bytes.

use crate::{ArchiveError, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum Source {
    Memory(Arc<[u8]>),
    File(Arc<File>, u64),
}

impl Source {
    pub fn open(path: &Path) -> std::io::Result<Source> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Source::File(Arc::new(file), len))
    }

    pub fn len(&self) -> u64 {
        match self {
            Source::Memory(d) => d.len() as u64,
            Source::File(_, len) => *len,
        }
    }

    /// Fills `buf` from `at`, failing when that runs past the end.
    pub fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<()> {
        self.check(at, buf.len() as u64)?;
        match self {
            Source::Memory(d) => buf.copy_from_slice(&d[at as usize..at as usize + buf.len()]),
            Source::File(f, _) => read_exact_at(f, buf, at)?,
        }
        Ok(())
    }

    /// `len` bytes at `at`, failing when they run past the end.  The length
    /// is checked before the buffer is made.
    pub fn bytes(&self, at: u64, len: u64) -> Result<Vec<u8>> {
        self.check(at, len)?;
        let mut buf = vec![0u8; len as usize];
        self.read_at(at, &mut buf)?;
        Ok(buf)
    }

    /// Up to `max` bytes from `at`: fewer near the end, none past it.
    pub fn head(&self, at: u64, max: usize) -> Vec<u8> {
        let n = self.len().saturating_sub(at).min(max as u64);
        self.bytes(at, n).unwrap_or_default()
    }

    fn check(&self, at: u64, len: u64) -> Result<()> {
        match at.checked_add(len) {
            Some(end) if end <= self.len() => Ok(()),
            _ => Err(ArchiveError::invalid(format!("a read of {len} bytes at {at} runs past the end of the file"))),
        }
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, at)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut at: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, at)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        at += n as u64;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(f: &File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    let mut f = f;
    f.seek(SeekFrom::Start(at))?;
    f.read_exact(buf)
}

/// A `Source` as a seekable stream, for the 7z reader.
pub(crate) struct SourceReader {
    pub source: Source,
    pub pos: u64,
}

impl Read for SourceReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.source.len().saturating_sub(self.pos).min(buf.len() as u64) as usize;
        if n == 0 {
            return Ok(0);
        }
        match &self.source {
            Source::Memory(m) => buf[..n].copy_from_slice(&m[self.pos as usize..self.pos as usize + n]),
            Source::File(f, _) => read_exact_at(f, &mut buf[..n], self.pos)?,
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SourceReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let len = self.source.len() as i128;
        let at = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(n) => len + n as i128,
            SeekFrom::Current(n) => self.pos as i128 + n as i128,
        };
        if at < 0 {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.pos = at as u64;
        Ok(self.pos)
    }
}

/// Little-endian reads from a slice, `None` past the end.
pub(crate) struct Le<'a>(pub &'a [u8]);

impl Le<'_> {
    pub fn u8(&self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }
    pub fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(self.0.get(at..at.checked_add(2)?)?.try_into().ok()?))
    }
    pub fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(self.0.get(at..at.checked_add(4)?)?.try_into().ok()?))
    }
    pub fn u64(&self, at: usize) -> Option<u64> {
        Some(u64::from_le_bytes(self.0.get(at..at.checked_add(8)?)?.try_into().ok()?))
    }
    pub fn i32(&self, at: usize) -> Option<i32> {
        self.u32(at).map(|v| v as i32)
    }
    pub fn slice(&self, at: usize, len: usize) -> Option<&[u8]> {
        self.0.get(at..at.checked_add(len)?)
    }
}

/// Sequential reads from a point in a source, in blocks, for directories
/// whose length isn't known until they are read (a PCK's).  `take` fails
/// when the data runs out or `budget` bytes have been handed out.
pub(crate) struct Stream<'a> {
    source: &'a Source,
    next: u64,
    buf: Vec<u8>,
    pos: usize,
    budget: u64,
}

impl<'a> Stream<'a> {
    pub fn new(source: &'a Source, at: u64, budget: u64) -> Stream<'a> {
        Stream { source, next: at, buf: Vec::new(), pos: 0, budget }
    }

    /// The next `n` bytes.
    pub fn take(&mut self, n: usize) -> Option<&[u8]> {
        if n as u64 > self.budget {
            return None;
        }
        if self.buf.len() - self.pos < n {
            self.buf.drain(..self.pos);
            self.pos = 0;
            let want = n.max(64 * 1024).saturating_sub(self.buf.len()) as u64;
            let chunk = self.source.head(self.next, want.min(usize::MAX as u64) as usize);
            self.next += chunk.len() as u64;
            self.buf.extend_from_slice(&chunk);
            if self.buf.len() < n {
                return None;
            }
        }
        self.budget -= n as u64;
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Some(out)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    }
}
