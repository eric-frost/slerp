//! What the shell hands the thumbnail handler, and turning it into a path
//! the loaders can read.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{E_FAIL, ERROR_FILE_TOO_LARGE};
use windows::Win32::System::Com::{CoTaskMemFree, IStream, STATFLAG_DEFAULT, STATSTG, STREAM_SEEK_SET};
use windows::core::{HRESULT, Result};

/// What the shell handed over.
pub enum Source {
    File(PathBuf),
    Stream(IStream),
}

/// True inside Explorer's isolated thumbnail process, the COM surrogate
/// for thumbcache's out-of-process server.
pub fn in_isolated_host() -> bool {
    static ISOLATED: OnceLock<bool> = OnceLock::new();
    *ISOLATED.get_or_init(|| {
        std::env::args().any(|a| a.to_ascii_uppercase().contains("AB8902B4-09CA-4BB6-B78D-A8F59079A8D5"))
    })
}

/*┌─────────────────╮
| Input files     |
└─────────────────╯*/
//region Input files

/// The file the loaders read: the real one, or a temporary copy of a
/// stream, deleted when this is dropped.
pub enum Input {
    Real(PathBuf),
    Copy(PathBuf),
}

impl Input {
    pub fn path(&self) -> &Path {
        match self {
            Input::Real(p) | Input::Copy(p) => p,
        }
    }

    /// Copies a stream to a temporary file, keeping the name's extension:
    /// the loaders read paths, and recognise some formats (OBJ, STL,
    /// MaterialX) partly by extension.  Only the stream's own content is
    /// there, so files it names next to it are missing.
    pub fn resolve(source: &Source) -> Result<Input> {
        let stream = match source {
            Source::File(path) => return Ok(Input::Real(path.clone())),
            Source::Stream(stream) => stream,
        };
        let mut stat = STATSTG::default();
        // SAFETY: stat receives a name, freed right after.
        unsafe { stream.Stat(&mut stat, STATFLAG_DEFAULT)? };
        let name = if stat.pwcsName.is_null() {
            String::new()
        } else {
            // SAFETY: Stat returned a CoTaskMemAlloc'd NUL-terminated name.
            let name = unsafe { stat.pwcsName.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(stat.pwcsName.0.cast())) };
            name
        };
        let limit = slerp_core::thumbnail_limits(256).max_file_bytes;
        if stat.cbSize > limit {
            return Err(HRESULT::from_win32(ERROR_FILE_TOO_LARGE.0).into());
        }
        let file_name = Path::new(&name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let input = Input::Copy(temp_path(&file_name).map_err(io_error)?);
        let mut file = std::fs::File::create(input.path()).map_err(io_error)?;
        // SAFETY: rewinding a stream we were given.
        unsafe { stream.Seek(0, STREAM_SEEK_SET, None)? };
        let copied = std::io::copy(&mut StreamReader(stream).take(limit + 1), &mut file).map_err(io_error)?;
        if copied > limit {
            return Err(HRESULT::from_win32(ERROR_FILE_TOO_LARGE.0).into());
        }
        file.flush().map_err(io_error)?;
        Ok(input)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Input::Copy(path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A unique file in `%TEMP%\slerp-shell` ending in `name`.
fn temp_path(name: &str) -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join("slerp-shell");
    std::fs::create_dir_all(&dir)?;
    let safe: String = name.chars().filter(|c| !r#"\/:*?"<>|"#.contains(*c)).take(100).collect();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!("{}-{n}-{safe}", std::process::id())))
}

fn io_error(e: std::io::Error) -> windows::core::Error {
    windows::core::Error::new(E_FAIL, e.to_string())
}

/// Reads an `IStream` through `std::io::Read`.
struct StreamReader<'a>(&'a IStream);

impl Read for StreamReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let mut read = 0u32;
        // SAFETY: buf is valid for len bytes of writes.
        let hr = unsafe { self.0.Read(buf.as_mut_ptr().cast(), len, Some(&mut read)) };
        if hr.is_err() {
            return Err(std::io::Error::other(windows::core::Error::from(hr)));
        }
        Ok(read as usize)
    }
}

/// The first bytes of a file, enough for a PNG's header chunk.
pub fn read_head(path: &Path) -> Vec<u8> {
    let mut head = Vec::with_capacity(32);
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(32).read_to_end(&mut head);
    }
    head
}

/// A 16-bit greyscale PNG (with or without alpha): heightmaps and masks,
/// which Slerp stretches to their range.  The same test as the
/// `image/x-png-16bit-grey` MIME type on Linux.
pub fn is_data_png(head: &[u8]) -> bool {
    head.starts_with(b"\x89PNG\r\n\x1a\n") && head.get(24) == Some(&16) && matches!(head.get(25), Some(0 | 4))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_16_bit_grey_pngs() {
        let mut head = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x10\0\0\0\x10".to_vec();
        head.extend([16, 0]);
        assert!(is_data_png(&head));
        head[25] = 4;
        assert!(is_data_png(&head));
        head[25] = 2;
        assert!(!is_data_png(&head));
        head[24] = 8;
        head[25] = 0;
        assert!(!is_data_png(&head));
        assert!(!is_data_png(b"\x89PNG"));
    }
}
