//! Bounds-checked little-endian reads.  Every format here is a web of
//! offsets and counts taken from the file, so every read goes through these
//! helpers and fails with `LoadError::Invalid` instead of panicking or
//! allocating whatever a corrupt count asks for.

use slerp_scene::LoadError;
use slerp_scene::glam::Vec3;

pub type Result<T> = std::result::Result<T, LoadError>;

/// A view of a whole file or of one part of it.  Offsets are relative to
/// the start of the view.
#[derive(Clone, Copy)]
pub struct Bytes<'a> {
    pub data: &'a [u8],
}

impl<'a> Bytes<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bytes { data }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `len` bytes at `at`, or an error naming `what` when they run past
    /// the end.
    pub fn slice(&self, at: usize, len: usize) -> Result<&'a [u8]> {
        at.checked_add(len).and_then(|end| self.data.get(at..end)).ok_or_else(|| {
            LoadError::invalid(format!("read of {len} bytes at {at} runs past the end ({})", self.len()))
        })
    }

    /// A sub-view of `len` bytes at `at`.
    pub fn sub(&self, at: usize, len: usize) -> Result<Bytes<'a>> {
        Ok(Bytes::new(self.slice(at, len)?))
    }

    /// `count` records of `stride` bytes at `at`, checked as one block so a
    /// huge count fails before anything is allocated for it.
    pub fn array(&self, at: usize, count: usize, stride: usize) -> Result<&'a [u8]> {
        let len = count.checked_mul(stride).ok_or_else(|| LoadError::invalid("record count overflows"))?;
        self.slice(at, len)
    }

    fn fixed<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        let s = self.slice(at, N)?;
        let mut out = [0; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    pub fn u8(&self, at: usize) -> Result<u8> {
        self.data.get(at).copied().ok_or_else(|| LoadError::invalid(format!("read at {at} runs past the end")))
    }

    pub fn i16(&self, at: usize) -> Result<i16> {
        Ok(i16::from_le_bytes(self.fixed(at)?))
    }

    pub fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.fixed(at)?))
    }

    pub fn i32(&self, at: usize) -> Result<i32> {
        Ok(i32::from_le_bytes(self.fixed(at)?))
    }

    pub fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed(at)?))
    }

    /// A float, with NaN and infinities replaced by 0 so they never reach
    /// bounds, cameras or the GPU.
    pub fn f32(&self, at: usize) -> Result<f32> {
        let v = f32::from_le_bytes(self.fixed(at)?);
        Ok(if v.is_finite() { v } else { 0.0 })
    }

    pub fn vec3(&self, at: usize) -> Result<Vec3> {
        Ok(Vec3::new(self.f32(at)?, self.f32(at + 4)?, self.f32(at + 8)?))
    }

    /// A count or offset stored as a signed 32-bit integer, which must not
    /// be negative.
    pub fn index(&self, at: usize) -> Result<usize> {
        let v = self.i32(at)?;
        usize::try_from(v).map_err(|_| LoadError::invalid(format!("negative count or offset {v} at {at}")))
    }

    /// A fixed-size name field: text up to the first NUL, decoded lossily.
    pub fn name(&self, at: usize, len: usize) -> Result<String> {
        Ok(cstr(self.slice(at, len)?))
    }

    pub fn starts_with(&self, magic: &[u8]) -> bool {
        self.data.starts_with(magic)
    }
}

/// Text up to the first NUL, with invalid UTF-8 replaced.
pub fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Fails when `count` exceeds `max`, for counts that size allocations.
pub fn cap(count: usize, max: usize, what: &str) -> Result<usize> {
    if count > max {
        return Err(LoadError::invalid(format!("{count} {what} is more than this format allows ({max})")));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_rejects() {
        let b = Bytes::new(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, b'a', b'b', 0, b'c']);
        assert_eq!(b.i32(0).unwrap(), 1);
        assert_eq!(b.i32(4).unwrap(), -1);
        assert!(b.index(4).is_err());
        assert!(b.i32(9).is_err());
        assert!(b.slice(usize::MAX, 2).is_err());
        assert!(b.array(0, usize::MAX, 8).is_err());
        assert_eq!(b.name(8, 4).unwrap(), "ab");
        let nan = Bytes::new(&[0, 0, 0xc0, 0x7f]);
        assert_eq!(nan.f32(0).unwrap(), 0.0);
    }
}
