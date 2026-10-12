//! Where an array's bytes come from: memory, a file read on demand, or a
//! compressed stream that is inflated only as far as it is asked to.
//!
//! A [`ByteSource`] is a flat run of bytes read by offset.  Volumes hold one
//! and never learn which kind it is, so a 4 GB raw file, a gzip-compressed
//! NIfTI and a decoded TIFF stack all go through the same plane extraction.

use slerp_scene::LoadError;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Mutex;

/// Opens the data file a detached header names.  A missing file reads as
/// what it is, since a header copied alone is common.
pub(crate) fn open_data_file(path: &Path) -> Result<File, LoadError> {
    File::open(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => LoadError::Invalid(format!(
            "the data file {} beside this header is missing",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        )),
        _ => e.into(),
    })
}

/// A flat run of bytes, read at any offset.
pub trait ByteSource: Send + Sync {
    /// The number of bytes.
    fn len(&self) -> u64;

    /// Fills `buf` from `offset`; an error when the run ends first.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// All the bytes at once, for sources that sit in memory.
    fn as_slice(&self) -> Option<&[u8]> {
        None
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Bytes held in memory.
pub struct MemSource(pub Vec<u8>);

impl ByteSource for MemSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        buf.copy_from_slice(self.0.get(start..end).ok_or_else(eof)?);
        Ok(())
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(&self.0)
    }
}

/// A stretch of a file read with positioned reads, so many planes can be
/// read from several threads without seeking.
pub struct FileSource {
    file: std::fs::File,
    base: u64,
    len: u64,
}

impl FileSource {
    /// The `len` bytes of `file` starting at `base`.
    pub fn new(file: std::fs::File, base: u64, len: u64) -> FileSource {
        FileSource { file, base, len }
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(eof)?;
        if end > self.len {
            return Err(eof());
        }
        read_exact_at(&self.file, buf, self.base + offset)
    }
}

/// Positioned read, on every platform.
pub fn read_exact_at(file: &std::fs::File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, buf, offset)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, offset)?;
        if n == 0 {
            return Err(eof());
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

pub fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "the data ends early")
}

/// A stretch of a file as a `Read`, using positioned reads so several
/// readers can share one file handle.
pub struct FileRange {
    file: std::sync::Arc<std::fs::File>,
    pos: u64,
    end: u64,
}

impl FileRange {
    /// The bytes of `file` from `start` up to (not including) `end`.
    pub fn new(file: std::sync::Arc<std::fs::File>, start: u64, end: u64) -> FileRange {
        FileRange { file, pos: start, end }
    }
}

impl Read for FileRange {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = (buf.len() as u64).min(self.end.saturating_sub(self.pos)) as usize;
        if want == 0 {
            return Ok(0);
        }
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&*self.file, &mut buf[..want], self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(&*self.file, &mut buf[..want], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// The data of a compressed file, inflated as it is read.  Reading forward
/// costs only the inflating; reading backwards starts over, so callers that
/// want random access first copy the stream into memory (and a thumbnail,
/// which needs one slice, never does).
pub struct StreamSource {
    open: Box<dyn Fn() -> io::Result<Box<dyn Read + Send>> + Send + Sync>,
    state: Mutex<(Option<Box<dyn Read + Send>>, u64)>,
    len: u64,
    /// Offsets past this are refused instead of inflated.
    reach: u64,
}

impl StreamSource {
    /// A stream of `len` bytes made by `open`, readable up to byte `reach`.
    pub fn new(
        len: u64,
        reach: u64,
        open: impl Fn() -> io::Result<Box<dyn Read + Send>> + Send + Sync + 'static,
    ) -> StreamSource {
        StreamSource { open: Box::new(open), state: Mutex::new((None, 0)), len, reach }
    }
}

impl ByteSource for StreamSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(eof)?;
        if end > self.len {
            return Err(eof());
        }
        if end > self.reach {
            return Err(io::Error::other("this part of the data is too far into the compressed stream"));
        }
        let mut guard = self.state.lock().map_err(|_| io::Error::other("the stream is poisoned"))?;
        let (reader, pos) = &mut *guard;
        if reader.is_none() || offset < *pos {
            *reader = Some((self.open)()?);
            *pos = 0;
        }
        let r = reader.as_mut().expect("opened above");
        let mut scratch = [0u8; 8192];
        while *pos < offset {
            let want = ((offset - *pos) as usize).min(scratch.len());
            r.read_exact(&mut scratch[..want])?;
            *pos += want as u64;
        }
        r.read_exact(buf)?;
        *pos = end;
        Ok(())
    }
}

/// Copies up to `limit` bytes of `r` into memory.  An error when the stream
/// holds fewer than `expect` bytes or more than `limit`.
pub fn read_all(r: &mut dyn Read, expect: u64, limit: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    out.try_reserve(expect.min(limit).min(1 << 31) as usize).map_err(|_| io::Error::other("out of memory"))?;
    loop {
        let n = r.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if out.len() as u64 + n as u64 > limit {
            return Err(io::Error::other("the data is larger than expected"));
        }
        out.extend_from_slice(&chunk[..n]);
    }
    if (out.len() as u64) < expect {
        return Err(eof());
    }
    Ok(out)
}

/*┌─────────────────╮
| Inflating       |
└─────────────────╯*/
//region Inflating

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

/// What wraps the deflate data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrapper {
    /// Bare deflate, as zip members store it.
    Raw,
    /// zlib: a two-byte header and an Adler-32 checksum.
    Zlib,
    /// gzip: a header with optional name and comment, a CRC-32 trailer, and
    /// possibly several members one after another.
    Gzip,
}

#[derive(PartialEq)]
enum Stage {
    Header,
    Body,
    Trailer,
    Done,
}

/// Inflates a deflate, zlib or gzip stream as it is read.  Memory use is
/// the 32 KB window plus a 64 KB input buffer, whatever the data expands to,
/// so a gzip bomb costs time only for what the caller reads.
pub struct Inflater<R: Read> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    inner_done: bool,
    state: Box<InflateState>,
    wrapper: Wrapper,
    stage: Stage,
    produced: u64,
    limit: u64,
}

impl<R: Read> Inflater<R> {
    /// `limit` caps the bytes produced; reading past it is an error.
    pub fn new(inner: R, wrapper: Wrapper, limit: u64) -> Inflater<R> {
        let format = match wrapper {
            Wrapper::Zlib => DataFormat::ZLibIgnoreChecksum,
            _ => DataFormat::Raw,
        };
        let stage = if wrapper == Wrapper::Gzip { Stage::Header } else { Stage::Body };
        Inflater {
            inner,
            buf: vec![0; 1 << 16],
            pos: 0,
            end: 0,
            inner_done: false,
            state: InflateState::new_boxed(format),
            wrapper,
            stage,
            produced: 0,
            limit,
        }
    }

    /// Makes sure at least `n` input bytes are buffered, if the input has
    /// them; returns how many are.
    fn fill(&mut self, n: usize) -> io::Result<usize> {
        while self.end - self.pos < n && !self.inner_done {
            if self.pos > 0 {
                self.buf.copy_within(self.pos..self.end, 0);
                self.end -= self.pos;
                self.pos = 0;
            }
            if self.end == self.buf.len() {
                break;
            }
            let got = self.inner.read(&mut self.buf[self.end..])?;
            if got == 0 {
                self.inner_done = true;
            }
            self.end += got;
        }
        Ok(self.end - self.pos)
    }

    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.fill(n)? < n {
            return Err(eof());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn gzip_header(&mut self) -> io::Result<()> {
        let h = self.take(10)?.to_vec();
        if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a gzip stream"));
        }
        let flags = h[3];
        if flags & 4 != 0 {
            let l = self.take(2)?;
            let n = u16::from_le_bytes([l[0], l[1]]) as usize;
            self.take(n)?;
        }
        for bit in [8, 16] {
            if flags & bit != 0 {
                // A zero-terminated string, at most a few hundred bytes.
                for _ in 0..65536 {
                    if self.take(1)?[0] == 0 {
                        break;
                    }
                }
            }
        }
        if flags & 2 != 0 {
            self.take(2)?;
        }
        Ok(())
    }
}

impl<R: Read> Read for Inflater<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.stage {
                Stage::Done => return Ok(0),
                Stage::Header => {
                    self.gzip_header()?;
                    self.state.reset(DataFormat::Raw);
                    self.stage = Stage::Body;
                }
                Stage::Trailer => {
                    self.take(8)?;
                    // Another member follows when the next two bytes are the magic.
                    self.stage = if self.fill(2)? >= 2 && self.buf[self.pos] == 0x1f && self.buf[self.pos + 1] == 0x8b {
                        Stage::Header
                    } else {
                        Stage::Done
                    };
                }
                Stage::Body => {
                    if self.produced >= self.limit {
                        return Err(io::Error::other("the compressed data expands beyond its declared size"));
                    }
                    self.fill(1)?;
                    let room = out.len().min((self.limit - self.produced).min(usize::MAX as u64) as usize);
                    let flush = if self.inner_done { MZFlush::Finish } else { MZFlush::None };
                    let r = inflate(&mut self.state, &self.buf[self.pos..self.end], &mut out[..room], flush);
                    self.pos += r.bytes_consumed;
                    self.produced += r.bytes_written as u64;
                    match r.status {
                        Ok(MZStatus::StreamEnd) => {
                            self.stage = if self.wrapper == Wrapper::Gzip { Stage::Trailer } else { Stage::Done };
                            if r.bytes_written > 0 {
                                return Ok(r.bytes_written);
                            }
                        }
                        Ok(_) | Err(MZError::Buf) => {
                            if r.bytes_written > 0 {
                                return Ok(r.bytes_written);
                            }
                            if r.bytes_consumed == 0 && self.inner_done && self.pos == self.end {
                                return Err(eof());
                            }
                        }
                        Err(e) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("damaged compressed data ({e:?})"),
                            ));
                        }
                    }
                }
            }
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn deflate(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec(data, 6)
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        v.extend(deflate(data));
        v.extend([0u8; 8]);
        v
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
    }

    fn sample() -> Vec<u8> {
        (0..300_000u32).map(|i| (i * 7 / 13) as u8).collect()
    }

    #[test]
    fn inflates_all_three_wrappers() {
        let data = sample();
        for (w, packed) in [(Wrapper::Raw, deflate(&data)), (Wrapper::Zlib, zlib(&data)), (Wrapper::Gzip, gzip(&data))]
        {
            let mut r = Inflater::new(&packed[..], w, u64::MAX);
            let out = read_all(&mut r, data.len() as u64, u64::MAX).unwrap();
            assert_eq!(out, data, "{w:?}");
        }
    }

    #[test]
    fn gzip_members_concatenate() {
        let a = sample();
        let b = vec![9u8; 1000];
        let mut packed = gzip(&a);
        packed.extend(gzip(&b));
        let mut r = Inflater::new(&packed[..], Wrapper::Gzip, u64::MAX);
        let out = read_all(&mut r, 0, u64::MAX).unwrap();
        assert_eq!(out.len(), a.len() + b.len());
        assert_eq!(&out[a.len()..], &b[..]);
    }

    #[test]
    fn a_bomb_stops_at_the_limit() {
        let bomb = gzip(&vec![0u8; 50_000_000]);
        assert!(bomb.len() < 100_000);
        let mut r = Inflater::new(&bomb[..], Wrapper::Gzip, 1 << 20);
        assert!(read_all(&mut r, 0, u64::MAX).is_err());
    }

    #[test]
    fn truncated_and_damaged_streams_fail_cleanly() {
        let packed = gzip(&sample());
        let mut r = Inflater::new(&packed[..packed.len() / 2], Wrapper::Gzip, u64::MAX);
        assert!(read_all(&mut r, 0, u64::MAX).is_err());
        let mut bad = packed.clone();
        for b in &mut bad[20..40] {
            *b ^= 0xa5;
        }
        let mut r = Inflater::new(&bad[..], Wrapper::Gzip, u64::MAX);
        let _ = read_all(&mut r, 0, 1 << 30);
        let mut r = Inflater::new(&b"not gzip at all"[..], Wrapper::Gzip, u64::MAX);
        assert!(read_all(&mut r, 0, 1 << 30).is_err());
    }

    #[test]
    fn stream_source_reads_forwards_and_restarts() {
        let data = sample();
        let packed = gzip(&data);
        let len = data.len() as u64;
        let src = StreamSource::new(len, len, move || {
            Ok(Box::new(Inflater::new(std::io::Cursor::new(packed.clone()), Wrapper::Gzip, u64::MAX)))
        });
        let mut b = [0u8; 100];
        src.read_at(200_000, &mut b).unwrap();
        assert_eq!(&b[..], &data[200_000..200_100]);
        src.read_at(1000, &mut b).unwrap();
        assert_eq!(&b[..], &data[1000..1100]);
        assert!(src.read_at(len - 50, &mut b).is_err());
    }

    #[test]
    fn stream_source_refuses_far_offsets() {
        let data = sample();
        let packed = gzip(&data);
        let len = data.len() as u64;
        let src = StreamSource::new(len, 1000, move || {
            Ok(Box::new(Inflater::new(std::io::Cursor::new(packed.clone()), Wrapper::Gzip, u64::MAX)))
        });
        let mut b = [0u8; 10];
        assert!(src.read_at(500, &mut b).is_ok());
        assert!(src.read_at(5000, &mut b).is_err());
    }

    #[test]
    fn memory_source_checks_bounds() {
        let m = MemSource(vec![1, 2, 3, 4]);
        let mut b = [0u8; 2];
        m.read_at(2, &mut b).unwrap();
        assert_eq!(b, [3, 4]);
        assert!(m.read_at(3, &mut b).is_err());
        assert!(m.read_at(u64::MAX, &mut b).is_err());
    }
}
