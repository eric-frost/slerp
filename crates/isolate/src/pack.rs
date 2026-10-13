//! Packing a job's input or a request into bytes, and reading it back.
//!
//! What a job needs crosses a process boundary as bytes, so each caller
//! writes its fields with [`Pack`] and the job reads them with [`Unpack`]
//! in the same order.  Both sides are the same build of the same program
//! (or a test program built from the same tree), so the format carries no
//! version or field names.

use std::path::{Path, PathBuf};

/// Writes fields one after another.
#[derive(Default)]
pub struct Pack(Vec<u8>);

impl Pack {
    pub fn new() -> Pack {
        Pack(Vec::new())
    }

    pub fn u8(mut self, v: u8) -> Pack {
        self.0.push(v);
        self
    }

    pub fn bool(self, v: bool) -> Pack {
        self.u8(v as u8)
    }

    pub fn u32(mut self, v: u32) -> Pack {
        self.0.extend(v.to_le_bytes());
        self
    }

    pub fn u64(mut self, v: u64) -> Pack {
        self.0.extend(v.to_le_bytes());
        self
    }

    pub fn f64(mut self, v: f64) -> Pack {
        self.0.extend(v.to_le_bytes());
        self
    }

    /// Bytes of any length (a file's contents, say), with their length.
    pub fn bytes(mut self, v: &[u8]) -> Pack {
        self.0.reserve(8 + v.len());
        self.0.extend((v.len() as u64).to_le_bytes());
        self.0.extend_from_slice(v);
        self
    }

    /// A path as the system spells it: bytes on Unix, UTF-16 on Windows,
    /// so names that aren't valid Unicode survive.
    pub fn path(self, p: &Path) -> Pack {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            self.bytes(p.as_os_str().as_bytes())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u8> = p.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect();
            self.bytes(&wide)
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.bytes(p.to_string_lossy().as_bytes())
        }
    }

    pub fn done(self) -> Vec<u8> {
        self.0
    }
}

/// Reads fields in the order they were packed.  Every read fails with "a
/// damaged request" when the bytes run out.
pub struct Unpack<'a> {
    rest: &'a [u8],
}

fn damaged() -> String {
    "a damaged request".into()
}

impl<'a> Unpack<'a> {
    pub fn new(bytes: &'a [u8]) -> Unpack<'a> {
        Unpack { rest: bytes }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let (head, rest) = self.rest.split_first_chunk::<N>().ok_or_else(damaged)?;
        self.rest = rest;
        Ok(*head)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take::<1>()?[0])
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    pub fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.take()?))
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = usize::try_from(self.u64()?).map_err(|_| damaged())?;
        if len > self.rest.len() {
            return Err(damaged());
        }
        let (bytes, rest) = self.rest.split_at(len);
        self.rest = rest;
        Ok(bytes)
    }

    pub fn path(&mut self) -> Result<PathBuf, String> {
        let bytes = self.bytes()?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            if bytes.len() % 2 != 0 {
                return Err(damaged());
            }
            let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_come_back_in_order() {
        let p = Path::new("dir/a file ü.pdf");
        let packed = Pack::new().u8(7).bool(true).u32(70000).u64(1 << 40).f64(-0.5).path(p).bytes(b"xyz").done();
        let mut u = Unpack::new(&packed);
        assert_eq!(u.u8().unwrap(), 7);
        assert!(u.bool().unwrap());
        assert_eq!(u.u32().unwrap(), 70000);
        assert_eq!(u.u64().unwrap(), 1 << 40);
        assert_eq!(u.f64().unwrap(), -0.5);
        assert_eq!(u.path().unwrap(), p);
        assert_eq!(u.bytes().unwrap(), b"xyz");
        assert!(u.u8().is_err());
    }

    #[test]
    fn damaged_lengths_are_refused() {
        let mut packed = Pack::new().bytes(b"abc").done();
        packed.truncate(9);
        assert!(Unpack::new(&packed).bytes().is_err());
        let huge = Pack::new().u64(u64::MAX).done();
        assert!(Unpack::new(&huge).bytes().is_err());
    }
}
