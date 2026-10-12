//! Turning Slerp's RGBA pictures into the bitmaps Explorer takes: a
//! 32-bit top-down DIB section in BGRA order with straight alpha.
//!
//! Explorer multiplies a `WTSAT_ARGB` thumbnail's colour by its alpha
//! itself when it draws it.  Measured on Windows 11 with half-transparent
//! quads over a white window, premultiplied input came out darker by
//! exactly a second multiplication (7.1 levels off on average, against
//! 1.1 for that model), so the bitmap carries straight alpha.

use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, HBITMAP};
use windows::core::Result;

/// Reorders RGBA to the BGRA a DIB section stores.
pub fn to_bgra(rgba: &[u8], out: &mut [u8]) {
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
    }
}

/// Reorders RGBA to BGRA composited over an opaque `matte` (RGB).
pub fn to_bgra_over(rgba: &[u8], out: &mut [u8], matte: [u8; 3]) {
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = u32::from(src[3]);
        let mix = |c: u8, m: u8| ((u32::from(c) * a + u32::from(m) * (255 - a) + 127) / 255) as u8;
        dst.copy_from_slice(&[mix(src[2], matte[2]), mix(src[1], matte[1]), mix(src[0], matte[0]), 255]);
    }
}

/// Makes a DIB section holding a `w` x `h` straight-alpha RGBA picture,
/// or, with a `matte`, the picture composited over that colour.  The
/// caller owns the handle and passes it to Explorer, which frees it.
pub fn dib_section(w: u32, h: u32, rgba: &[u8], matte: Option<[u8; 3]>) -> Result<HBITMAP> {
    let bytes = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4));
    if w == 0 || h == 0 || bytes != Some(rgba.len()) || w > i32::MAX as u32 || h > i32::MAX as u32 {
        return Err(windows::Win32::Foundation::E_INVALIDARG.into());
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            // Negative height: rows run top to bottom, as in `rgba`.
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: info describes a 32-bit DIB; bits receives its pixel memory.
    let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
    if bits.is_null() {
        return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
    }
    // SAFETY: a w x h 32-bit DIB section has exactly w * h * 4 bytes of
    // pixels, which nothing else references yet.
    let out = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len()) };
    match matte {
        Some(matte) => to_bgra_over(rgba, out, matte),
        None => to_bgra(rgba, out),
    }
    Ok(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_to_bgra_keeping_straight_alpha() {
        let rgba = [255, 128, 0, 255, 200, 100, 50, 128, 255, 255, 255, 0];
        let mut out = [0u8; 12];
        to_bgra(&rgba, &mut out);
        assert_eq!(out, [0, 128, 255, 255, 50, 100, 200, 128, 255, 255, 255, 0]);
    }

    #[test]
    fn composites_over_a_matte() {
        let rgba = [255, 128, 0, 255, 200, 100, 50, 128, 9, 9, 9, 0];
        let mut out = [0u8; 12];
        to_bgra_over(&rgba, &mut out, [255, 255, 255]);
        assert_eq!(out, [0, 128, 255, 255, 152, 177, 227, 255, 255, 255, 255, 255]);
    }
}
