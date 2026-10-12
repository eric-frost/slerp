//! Pictures a document's own program saved in it, which file-manager
//! thumbnails use before drawing the page themselves (the viewer always
//! draws the real content): a PDF page's `/Thumb` image (read in the
//! renderer's child, see `pdf.rs`), and the thumbnail Adobe's programs
//! (Illustrator above all) put in a file's XMP metadata as a base64 JPEG
//! (`<xmpGImg:image>`), found here by scanning the file's bytes.

use std::io::Read;
use std::path::Path;

/// How much of a file is searched for its XMP thumbnail: Illustrator puts
/// the metadata near the start, and a scan of this much costs a few
/// milliseconds.
const SCAN_BYTES: u64 = 32 << 20;
/// The longest base64 text taken.
const MAX_BASE64: usize = 16 << 20;

/// The XMP thumbnail of the file at `path`, as straight RGBA with its size.
pub fn xmp_thumbnail(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(SCAN_BYTES).read_to_end(&mut bytes).ok()?;
    let jpeg = xmp_image(&bytes)?;
    decode_jpeg(&jpeg)
}

/// The decoded bytes of the first `<xmpGImg:image>` element in `bytes`.
pub fn xmp_image(bytes: &[u8]) -> Option<Vec<u8>> {
    const OPEN: &[u8] = b"<xmpGImg:image>";
    const CLOSE: &[u8] = b"</xmpGImg:image>";
    let start = memchr::memmem::find(bytes, OPEN)? + OPEN.len();
    let rest = &bytes[start..bytes.len().min(start + MAX_BASE64)];
    let end = memchr::memmem::find(rest, CLOSE)?;
    base64(&rest[..end])
}

/// Decodes base64 written into XML: whitespace and the line-break entities
/// Adobe writes (`&#xA;`) are skipped.
pub fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        i += 1;
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'&' => {
                // An entity such as `&#xA;`: skip to its semicolon.
                let semi = text[i..].iter().take(8).position(|&b| b == b';')?;
                i += semi + 1;
                continue;
            }
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Whether a stored picture can stand for the page: big enough to show
/// something (ImageMagick stores 1 x 1 page thumbnails for huge pictures)
/// and not absurdly large.
pub fn usable(width: u32, height: u32) -> bool {
    width.max(height) >= 48 && width.min(height) >= 4 && width <= 4096 && height <= 4096
}

/// A small JPEG as straight RGBA.
pub fn decode_jpeg(jpeg: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut d = jpeg_decoder::Decoder::new(jpeg);
    d.set_max_decoding_buffer_size(64 << 20);
    d.read_info().ok()?;
    let info = d.info()?;
    if !usable(info.width as u32, info.height as u32) {
        return None;
    }
    let px = d.decode().ok()?;
    let n = info.width as usize * info.height as usize;
    let rgba: Vec<u8> = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 if px.len() >= n * 3 => {
            px.chunks_exact(3).take(n).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        jpeg_decoder::PixelFormat::L8 if px.len() >= n => px.iter().take(n).flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    Some((info.width as u32, info.height as u32, rgba))
}
