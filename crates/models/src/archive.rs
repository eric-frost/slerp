//! Compressed containers the newer readers meet: zip packages (3MF, through
//! `slerp-archive`), gzip streams (`.wrz`, `.x3dz` and gzipped `.wrl`) and MSZIP blocks (compressed
//! DirectX files).  Every output is capped, so a small file that inflates to
//! gigabytes fails with `TooLarge` instead of exhausting memory.

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};
use slerp_scene::LoadError;

fn u16_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?) as u64)
}

fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?) as u64)
}

/*┌─────────────────╮
| Zip             |
└─────────────────╯*/
//region Zip

pub use slerp_archive::{Archive as Zip, Entry as ZipEntry};

fn zip_error(e: slerp_archive::ArchiveError) -> LoadError {
    match e {
        slerp_archive::ArchiveError::TooLarge(m) => LoadError::TooLarge(m),
        slerp_archive::ArchiveError::Unsupported(m) => LoadError::Unsupported(m),
        e => LoadError::invalid(e.to_string()),
    }
}

/// Opens a zip package.  The directory is read now and the entries when
/// asked for; the bytes are copied once.
pub fn zip_open(bytes: &[u8]) -> Result<Zip, LoadError> {
    let zip = Zip::from_bytes(bytes).map_err(zip_error)?;
    if zip.kind() != slerp_archive::ArchiveKind::Zip {
        return Err(LoadError::invalid("not a zip archive"));
    }
    Ok(zip)
}

/// An entry's uncompressed bytes, refused when they would exceed `limit`.
pub fn zip_read(zip: &Zip, entry: &ZipEntry, limit: u64) -> Result<Vec<u8>, LoadError> {
    if entry.size > limit {
        return Err(LoadError::TooLarge(format!("{} inflates to {} bytes", entry.path, entry.size)));
    }
    zip.read(entry).map_err(zip_error)
}

/// Finds an entry by its name in a package, ignoring a leading slash and
/// letter case as OPC part names do.
pub fn zip_find<'z>(zip: &'z Zip, name: &str) -> Option<&'z ZipEntry> {
    zip.find(name).first().and_then(|&i| zip.entry(i))
}

//endregion

/*┌─────────────────╮
| Deflate         |
└─────────────────╯*/
//region Deflate

/// Inflates a raw deflate stream.  A truncated stream gives what it held,
/// since text formats read sensibly up to the cut.
fn inflate(raw: &[u8], limit: u64, what: &str) -> Result<Vec<u8>, LoadError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit) {
        Ok(out) => Ok(out),
        Err(e) if e.status == TINFLStatus::HasMoreOutput => {
            Err(LoadError::TooLarge(format!("{what} inflates to more than {limit} bytes")))
        }
        Err(e) if e.status == TINFLStatus::FailedCannotMakeProgress && !e.output.is_empty() => Ok(e.output),
        Err(e) => Err(LoadError::invalid(format!("{what}: damaged deflate data ({:?})", e.status))),
    }
}

/// True for the gzip signature.
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// Inflates the first member of a gzip stream.
pub fn gunzip(bytes: &[u8], limit: u64) -> Result<Vec<u8>, LoadError> {
    let body = gzip_body(bytes).ok_or_else(|| LoadError::invalid("damaged gzip header"))?;
    inflate(body, limit, "gzip stream")
}

/// The first bytes a gzip stream inflates to, at most `max`, for
/// recognising what it holds from the start of a file.
pub fn gunzip_head(bytes: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut state = DecompressorOxide::new();
    let body = gzip_body(bytes)?;
    let mut out = vec![0u8; max];
    let (_, _, written) =
        decompress(&mut state, body, &mut out, 0, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
    out.truncate(written);
    (!out.is_empty()).then_some(out)
}

/// The deflate data after a gzip header.
fn gzip_body(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 10 || !is_gzip(bytes) || bytes[2] != 8 {
        return None;
    }
    let flags = bytes[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        at = at.checked_add(2 + u16_at(bytes, at)? as usize)?;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += bytes.get(at..)?.iter().position(|&c| c == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    bytes.get(at..)
}

/// The block a DirectX MSZIP stream inflates each chunk into at most.
const MSZIP_BLOCK: usize = 32 * 1024 + 18;

/// Inflates the MSZIP blocks of a compressed DirectX file (the bytes after
/// its 16-byte header).  Each block is a raw deflate stream prefixed with
/// its sizes and "CK", which may refer back into the previous block's
/// output, so all output is kept in one buffer that serves as the
/// dictionary.
pub fn mszip(bytes: &[u8], limit: u64) -> Result<Vec<u8>, LoadError> {
    let bad = |what: &str| LoadError::invalid(format!("compressed DirectX file: {what}"));
    // 1. The whole uncompressed size, which includes the 16-byte header.
    let total = u32_at(bytes, 0).ok_or_else(|| bad("truncated"))?;
    if total > limit {
        return Err(LoadError::TooLarge(format!("inflates to {total} bytes")));
    }
    let mut out: Vec<u8> = Vec::with_capacity((total as usize).min(1 << 26));
    let mut at = 4usize;
    // 2. Blocks: uncompressed size, compressed size including "CK", "CK",
    //    deflate data.
    while at + 6 <= bytes.len() {
        let size = u16_at(bytes, at).unwrap_or(0) as usize;
        let packed = u16_at(bytes, at + 2).unwrap_or(0) as usize;
        if &bytes[at + 4..at + 6] != b"CK" || packed < 2 {
            return Err(bad("missing MSZIP block header"));
        }
        let end = (at + 4 + packed).min(bytes.len());
        let data = &bytes[at + 6..end];
        at = end;
        let pos = out.len();
        if (pos + size.max(1)) as u64 > limit {
            return Err(LoadError::TooLarge(format!("inflates to more than {limit} bytes")));
        }
        out.resize(pos + MSZIP_BLOCK.max(size), 0);
        let mut state = DecompressorOxide::new();
        let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
        let (status, _, written) = decompress(&mut state, data, &mut out, pos, flags);
        out.truncate(pos + written);
        match status {
            TINFLStatus::Done => {}
            _ if written > 0 => break,
            _ => return Err(bad("damaged deflate data")),
        }
    }
    Ok(out)
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stored-only zip with the given files.
    pub(crate) fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        slerp_archive::testutil::zip(files)
    }

    #[test]
    fn reads_stored_zip_and_gzip() {
        let bytes = stored_zip(&[("3D/3dmodel.model", b"<model/>"), ("a/", b"")]);
        let zip = zip_open(&bytes).unwrap();
        assert_eq!(zip.entries().len(), 1);
        let e = zip_find(&zip, "/3d/3DMODEL.model").unwrap();
        assert_eq!(zip_read(&zip, e, 100).unwrap(), b"<model/>");
        assert!(matches!(zip_read(&zip, e, 4), Err(LoadError::TooLarge(_))));
        for cut in 0..bytes.len() {
            let _ = zip_open(&bytes[..cut]);
        }

        let deflated = miniz_oxide::deflate::compress_to_vec(b"#VRML V2.0 utf8\nShape {}\n", 6);
        let mut gz = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3];
        gz.extend_from_slice(b"name.wrl\0");
        gz.extend_from_slice(&deflated);
        gz.extend_from_slice(&[0; 8]);
        assert!(gunzip(&gz, 1000).unwrap().starts_with(b"#VRML"));
        assert!(matches!(gunzip(&gz, 4), Err(LoadError::TooLarge(_))));
        for cut in 0..gz.len() {
            let _ = gunzip(&gz[..cut], 1000);
        }
    }

    #[test]
    fn reads_mszip_blocks() {
        // Two full blocks.  Real files' later blocks refer back into the
        // previous block, which the DirectX corpus files exercise.
        let first: Vec<u8> = (0..MSZIP_BLOCK - 18).map(|i| b"abcdefgh"[i % 8]).collect();
        let second = b"abcdefghabcdefgh".to_vec();
        let packed1 = miniz_oxide::deflate::compress_to_vec(&first, 6);
        let packed2 = miniz_oxide::deflate::compress_to_vec(&second, 6);
        let mut file = Vec::new();
        file.extend_from_slice(&((first.len() + second.len() + 16) as u32).to_le_bytes());
        for (data, packed) in [(&first, &packed1), (&second, &packed2)] {
            file.extend_from_slice(&(data.len() as u16).to_le_bytes());
            file.extend_from_slice(&((packed.len() + 2) as u16).to_le_bytes());
            file.extend_from_slice(b"CK");
            file.extend_from_slice(packed);
        }
        let out = mszip(&file, 1 << 20).unwrap();
        assert_eq!(out.len(), first.len() + second.len());
        assert!(out.ends_with(&second));
        for cut in 0..file.len().min(400) {
            let _ = mszip(&file[..cut], 1 << 20);
        }
    }
}
