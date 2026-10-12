//! Writing OpenEXR files, for pictures whose values go beyond 0 to 1 (HDR
//! skies, floating-point texture levels, EXR layers): one part, scan lines
//! stored uncompressed, four 32-bit float channels.  That is the simplest
//! file every OpenEXR reader opens, and it keeps each value exactly; files
//! that need to be small can be recompressed by any image tool.  The format
//! is OpenEXR's "Technical Introduction" and "OpenEXR File Layout".

use super::{ContentsError, Result};

/// The magic number, then version 2 with no flags (single part, scan lines,
/// short names).
const MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];
const VERSION: [u8; 4] = [2, 0, 0, 0];
/// The pixel type of 32-bit floats in a channel list.
const FLOAT: i32 = 2;

/// An RGBA picture of `width` x `height` floats, rows from the top, as an
/// EXR file.  Alpha is written only when `alpha` is true.
pub fn exr_bytes(width: u32, height: u32, rgba: &[f32], alpha: bool) -> Result<Vec<u8>> {
    let values = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    let fits = width <= i32::MAX as u32 / 16 && height <= i32::MAX as u32;
    if !fits || values.is_none_or(|n| n == 0 || n != rgba.len()) {
        return Err(ContentsError::Convert(format!("a {width} x {height} picture with {} values", rgba.len())));
    }
    // Channels are stored in alphabetical order.
    let channels: &[(&str, usize)] =
        if alpha { &[("A", 3), ("B", 2), ("G", 1), ("R", 0)] } else { &[("B", 2), ("G", 1), ("R", 0)] };

    // 1. The header: attributes as name, type, size and value, ending with
    //    an empty name.
    let mut out = Vec::with_capacity(rgba.len() * 4 + 512 + height as usize * 8);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION);
    let attribute = |out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]| {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(kind.as_bytes());
        out.push(0);
        out.extend_from_slice(&(value.len() as i32).to_le_bytes());
        out.extend_from_slice(value);
    };
    let mut list = Vec::new();
    for (name, _) in channels {
        list.extend_from_slice(name.as_bytes());
        list.push(0);
        list.extend_from_slice(&FLOAT.to_le_bytes());
        // pLinear and three reserved bytes, then x and y sampling.
        list.extend_from_slice(&[0, 0, 0, 0]);
        list.extend_from_slice(&1i32.to_le_bytes());
        list.extend_from_slice(&1i32.to_le_bytes());
    }
    list.push(0);
    attribute(&mut out, "channels", "chlist", &list);
    attribute(&mut out, "compression", "compression", &[0]);
    let mut window = Vec::with_capacity(16);
    for v in [0i32, 0, width as i32 - 1, height as i32 - 1] {
        window.extend_from_slice(&v.to_le_bytes());
    }
    attribute(&mut out, "dataWindow", "box2i", &window);
    attribute(&mut out, "displayWindow", "box2i", &window);
    attribute(&mut out, "lineOrder", "lineOrder", &[0]);
    attribute(&mut out, "pixelAspectRatio", "float", &1f32.to_le_bytes());
    attribute(&mut out, "screenWindowCenter", "v2f", &[0; 8]);
    attribute(&mut out, "screenWindowWidth", "float", &1f32.to_le_bytes());
    out.push(0);

    // 2. The offset table: one block per scan line when uncompressed.
    let line_bytes = width as usize * channels.len() * 4;
    let table_at = out.len();
    let first_block = (table_at + height as usize * 8) as u64;
    for y in 0..height as u64 {
        out.extend_from_slice(&(first_block + y * (8 + line_bytes as u64)).to_le_bytes());
    }

    // 3. The blocks: the line's y and size, then each channel's samples.
    for y in 0..height as usize {
        out.extend_from_slice(&(y as i32).to_le_bytes());
        out.extend_from_slice(&(line_bytes as i32).to_le_bytes());
        let row = &rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
        for &(_, c) in channels {
            for px in row.chunks_exact(4) {
                out.extend_from_slice(&px[c].to_le_bytes());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_files_read_back() {
        // Values beyond 0..1 and a negative one survive exactly.
        let rgba = [0.5, 2.0, 100.0, 1.0, -0.25, 0.0, 1.0, 0.5, 3.0, 4.0, 5.0, 0.0];
        let bytes = exr_bytes(3, 1, &rgba, true).unwrap();
        let dir = std::env::temp_dir().join(format!("slerp-exr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.exr");
        std::fs::write(&path, &bytes).unwrap();
        let limits = slerp_scene::Limits::viewer();
        let asset = slerp_images::load(slerp_images::ImageFormat::Exr, &path, &limits).unwrap();
        let slerp_scene::Asset::Image(tex) = asset else { panic!("not a picture") };
        assert_eq!((tex.width(), tex.height()), (3, 1));
        let slerp_textures::Pixels::Rgba32F(px) = tex.base() else { panic!("not floats") };
        assert_eq!(&px[..], &rgba[..]);
        let _ = std::fs::remove_dir_all(dir);
        // Bad sizes are refused, not written.
        assert!(exr_bytes(0, 1, &[], true).is_err());
        assert!(exr_bytes(2, 2, &rgba, true).is_err());
    }
}
