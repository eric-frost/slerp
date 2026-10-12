//! Small helpers shared by every decoder: bounds-checked little-endian
//! reading, error shorthands and the allocation caps.

use crate::TextureError;

/// The most bytes of decoded pixels one call may allocate.  Every decoder
/// checks this before allocating, so a header that claims a huge image can't
/// exhaust memory.  2 GiB holds a 16K by 16K 8-bit texture or an 8K by 8K
/// float one.
pub const MAX_DECODED_BYTES: u64 = 2 << 30;

/// The longest side any decoder accepts.
pub const MAX_DIMENSION: u32 = 1 << 16;

pub(crate) fn invalid(msg: impl Into<String>) -> TextureError {
    TextureError::Invalid(msg.into())
}

pub(crate) fn unsupported(msg: impl Into<String>) -> TextureError {
    TextureError::Unsupported(msg.into())
}

pub(crate) fn too_large(msg: impl Into<String>) -> TextureError {
    TextureError::TooLarge(msg.into())
}

/// Checks that an image of this size may be decoded into
/// `bytes_per_texel`-byte texels, and returns its texel count.
pub(crate) fn check_size(width: u32, height: u32, depth: u32, bytes_per_texel: u64) -> Result<usize, TextureError> {
    if width == 0 || height == 0 || depth == 0 {
        return Err(invalid(format!("empty image ({width}x{height}x{depth})")));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION || depth > MAX_DIMENSION {
        return Err(too_large(format!("{width}x{height}x{depth}")));
    }
    let texels = width as u64 * height as u64 * depth as u64;
    if texels.saturating_mul(bytes_per_texel) > MAX_DECODED_BYTES {
        return Err(too_large(format!("{width}x{height}x{depth} needs more than {} MiB", MAX_DECODED_BYTES >> 20)));
    }
    Ok(texels as usize)
}

/// Checks a container's claimed shape before any size is computed from it,
/// so the arithmetic that follows (in `u64`) cannot overflow: sides up to
/// `MAX_DIMENSION`, at most 2^32 texels per surface and 65536 layers.
pub(crate) fn check_layout(width: u32, height: u32, depth: u32, layers: u32) -> Result<(), TextureError> {
    if width == 0 || height == 0 || depth == 0 {
        return Err(invalid(format!("empty image ({width}x{height}x{depth})")));
    }
    let big = width > MAX_DIMENSION || height > MAX_DIMENSION || depth > MAX_DIMENSION || layers > 1 << 16;
    if big || width as u64 * height as u64 * depth as u64 > 1 << 32 {
        return Err(too_large(format!("{width}x{height}x{depth}, {layers} layers")));
    }
    Ok(())
}

/// Checks that a buffer of `bytes` bytes may be allocated.
pub(crate) fn check_bytes(bytes: u64) -> Result<(), TextureError> {
    if bytes > MAX_DECODED_BYTES {
        return Err(too_large(format!("{} MiB of data", bytes >> 20)));
    }
    Ok(())
}

/// What the streaming decoders read from: a file, or a slice of memory in a
/// `Cursor`.  Reading a file in bands lets a 100 MB frame become a thumbnail
/// without being held whole.
pub(crate) trait Stream: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> Stream for T {}

/// Reads up to `buf.len()` bytes, fewer only at the end of the file.
pub(crate) fn read_fully(r: &mut (impl std::io::Read + ?Sized), buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Inflates a gzip stream (RFC 1952), refusing output beyond `max` bytes so
/// a compression bomb can't exhaust memory.
pub(crate) fn gunzip(data: &[u8], max: usize) -> Result<Vec<u8>, TextureError> {
    let bad = |what: &str| invalid(format!("gzip: {what}"));
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err(bad("not a gzip stream"));
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([
            *data.get(at).ok_or_else(|| bad("short header"))?,
            *data.get(at + 1).ok_or_else(|| bad("short header"))?,
        ]);
        at += 2 + n as usize;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            let end = data.get(at..).and_then(|d| d.iter().position(|&b| b == 0)).ok_or_else(|| bad("short header"))?;
            at += end + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let body = data.get(at..data.len() - 8).ok_or_else(|| bad("short stream"))?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, max).map_err(|e| invalid(format!("gzip: {e:?}")))
}

/// A cursor over a byte slice whose reads fail instead of panicking when the
/// data runs out.
#[derive(Clone)]
pub(crate) struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], TextureError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| invalid("truncated file"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), TextureError> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, TextureError> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, TextureError> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, TextureError> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, TextureError> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, TextureError> {
        let b = self.bytes(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }
}

/// Returns `data[start..start + len]`, or an error naming `what` when the
/// range falls outside the data.
pub(crate) fn slice<'a>(data: &'a [u8], start: u64, len: u64, what: &str) -> Result<&'a [u8], TextureError> {
    let end = start.checked_add(len).ok_or_else(|| invalid(format!("{what}: bad offset")))?;
    if end > data.len() as u64 {
        return Err(invalid(format!("{what}: truncated file")));
    }
    Ok(&data[start as usize..end as usize])
}

pub(crate) fn u16_le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// The size of a mip level: `size >> level`, never below 1.
pub(crate) fn mip_dim(size: u32, level: u32) -> u32 {
    size.checked_shr(level).unwrap_or(0).max(1)
}
