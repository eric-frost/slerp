//! The preview picture Blender stores in every file it saves with one (the
//! default since 2.5): a `TEST` block right after the `REND` blocks at the
//! start of the file, holding the width, the height and 8-bit RGBA rows
//! from the bottom up.  KDE's `blenderthumbnail` shows the same picture, so
//! file managers agree.  Only the start of the file is read, through the
//! decompressor when there is one.

use crate::file::{self, Header, Packing, REND, TEST};
use slerp_scene::{ColorSpace, LoadError, Texture};
use std::io::Read;

/// The largest preview accepted: Blender writes 128 by 128 (256 by 256 on
/// some versions), so anything far larger is damage.
const MAX_SIDE: u32 = 4096;

/// Reads the preview from the start of a file, compressed or not.
/// `Ok(None)` means the file has none.
pub fn read(r: impl Read) -> Result<Option<Texture>, LoadError> {
    let mut r = Buffered { r, buf: Vec::new() };
    // "BLENDER" is seven bytes; the compressors' magic numbers fewer.
    r.need(7).or_else(|_| r.need(4))?;
    let packing = Packing::of(&r.buf).ok_or(LoadError::NotMine)?;
    let inner = file::unpacking(packing, std::io::Cursor::new(std::mem::take(&mut r.buf)).chain(r.r))?;
    let mut r = Buffered { r: inner, buf: Vec::new() };
    r.need(17).or_else(|_| r.need(12))?;
    let header = file::parse_header(&r.buf)?;
    let mut pos = header.len;
    // The preview follows the REND blocks; anything else first means there
    // is none.  A file has a REND block per scene, so this is bounded by
    // the scenes' count, and each block's size by its header.
    for _ in 0..10_000 {
        r.need(pos + header.block_header_len())?;
        let Some((block, start)) = file::read_block_header(&header, &r.buf, pos) else {
            return Ok(None);
        };
        match block.code {
            REND => {
                pos = start.checked_add(block.len).ok_or_else(|| LoadError::invalid("bad block size"))?;
                if block.len > 1 << 20 {
                    return Ok(None);
                }
            }
            TEST => return decode(&header, &mut r, start, block.len),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn decode(
    header: &Header,
    r: &mut Buffered<impl Read>,
    start: usize,
    len: usize,
) -> Result<Option<Texture>, LoadError> {
    r.need(start + 8)?;
    let rd = file::Endian(header.big_endian);
    let (w, h) = (rd.i32(&r.buf[start..]), rd.i32(&r.buf[start + 4..]));
    if w <= 0 || h <= 0 || w as u32 > MAX_SIDE || h as u32 > MAX_SIDE {
        return Ok(None);
    }
    let (w, h) = (w as u32, h as u32);
    let bytes = (w * h * 4) as usize;
    if len < 8 + bytes {
        return Ok(None);
    }
    r.need(start + 8 + bytes)?;
    let src = &r.buf[start + 8..start + 8 + bytes];
    // Rows are stored bottom up, as Blender's image buffers are.
    let row = (w * 4) as usize;
    let mut pixels = Vec::with_capacity(bytes);
    for y in (0..h as usize).rev() {
        pixels.extend_from_slice(&src[y * row..(y + 1) * row]);
    }
    Ok(Some(Texture::from_rgba8(w, h, pixels, ColorSpace::Srgb)))
}

/// A reader that keeps everything read so far, so block headers can be
/// parsed from one growing buffer.
struct Buffered<R> {
    r: R,
    buf: Vec<u8>,
}

impl<R: Read> Buffered<R> {
    /// Reads until at least `n` bytes are buffered.
    fn need(&mut self, n: usize) -> Result<(), LoadError> {
        if self.buf.len() >= n {
            return Ok(());
        }
        let want = n - self.buf.len();
        let got = (&mut self.r).take(want as u64).read_to_end(&mut self.buf)?;
        if got < want {
            return Err(LoadError::invalid("the file ends before its preview"));
        }
        Ok(())
    }
}
