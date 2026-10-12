//! Where glTF's bytes come from: the GLB container, buffers (the GLB's
//! binary chunk, data URIs, files next to the model) and buffer views,
//! including views compressed with `EXT_meshopt_compression` or
//! `KHR_meshopt_compression`.

use crate::Ctx;
use crate::json::{BufferViewJ, MeshoptJ, Root};
use base64::Engine;
use rayon::prelude::*;
use slerp_scene::LoadError;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/*┌─────────────────╮
| Container       |
└─────────────────╯*/
//region Container

const GLB_MAGIC: &[u8; 4] = b"glTF";
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

/// The JSON and binary chunk of a file, which is either a GLB container or
/// plain JSON.
pub struct Container<'a> {
    pub json: &'a [u8],
    pub bin: Option<&'a [u8]>,
    /// Where the binary chunk's data starts in the file.
    pub bin_offset: u64,
    pub binary: bool,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Splits a file into its JSON and binary chunk.  A GLB cut short keeps
/// whatever part of its binary chunk survived, with a warning, so the
/// meshes stored early in the file still load.
pub fn container<'a>(bytes: &'a [u8], ctx: &Ctx) -> Result<Container<'a>, LoadError> {
    if !bytes.starts_with(GLB_MAGIC) {
        let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        return Ok(Container { json, bin: None, bin_offset: 0, binary: false });
    }
    let version = u32_at(bytes, 4).ok_or_else(|| LoadError::invalid("GLB header is truncated"))?;
    if version != 2 {
        return Err(LoadError::Unsupported(format!("GLB version {version} (only glTF 2.0 is supported)")));
    }
    let declared = u32_at(bytes, 8).ok_or_else(|| LoadError::invalid("GLB header is truncated"))? as usize;
    if declared > bytes.len() {
        ctx.warn(format!("the file is truncated: {} of {declared} bytes", bytes.len()));
    }
    let end = declared.min(bytes.len());
    let (Some(json_len), Some(json_type)) = (u32_at(bytes, 12), u32_at(bytes, 16)) else {
        return Err(LoadError::invalid("GLB has no JSON chunk"));
    };
    if json_type != CHUNK_JSON {
        return Err(LoadError::invalid("GLB's first chunk is not JSON"));
    }
    let json_end = 20usize.checked_add(json_len as usize).filter(|&e| e <= end);
    let json_end = json_end.ok_or_else(|| LoadError::invalid("GLB's JSON chunk is truncated"))?;
    let json = &bytes[20..json_end];

    // Chunks should start 4-byte aligned, but tolerate writers that forgot
    // to pad the JSON.
    let mut bin = None;
    let mut bin_offset = 0;
    for start in [json_end.next_multiple_of(4), json_end] {
        if let (Some(len), Some(CHUNK_BIN)) = (u32_at(bytes, start), u32_at(bytes, start + 4)) {
            let data_start = start + 8;
            let data_end = data_start.saturating_add(len as usize);
            if data_end > end {
                ctx.warn("the GLB's binary chunk is truncated");
            }
            bin = bytes.get(data_start.min(end)..data_end.min(end));
            bin_offset = data_start as u64;
            break;
        }
    }
    Ok(Container { json, bin, bin_offset, binary: true })
}

//endregion

/*┌─────────────────╮
| Buffers         |
└─────────────────╯*/
//region Buffers

/// Where files next to the model are looked for, and whether they may be
/// read at all.  How many bytes of them may be read is tracked by `Ctx`.
pub struct Neighbours {
    pub dir: Option<PathBuf>,
    pub enabled: bool,
}

/// Reads a URI: a data URI always, a relative file only when neighbours are
/// enabled.  `what` names the thing for warnings.
pub fn read_uri(uri: &str, neighbours: &Neighbours, ctx: &Ctx, what: &str) -> Option<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let Some((header, payload)) = rest.split_once(',') else {
            ctx.warn(format!("{what} has a malformed data URI"));
            return None;
        };
        let data = if header.ends_with(";base64") {
            BASE64.decode(payload.trim().as_bytes()).ok()
        } else {
            Some(percent_decode(payload))
        };
        if data.is_none() {
            ctx.warn(format!("{what} has a malformed data URI"));
        }
        return data;
    }
    if uri.contains("://") {
        ctx.warn(format!("{what} refers to {uri}, which is not a local file"));
        return None;
    }
    if !neighbours.enabled {
        ctx.warn(format!("{what} is in a separate file that wasn't read: {uri}"));
        return None;
    }
    let path = uri_path(uri, neighbours)?;
    read_file(&path, ctx, what, &String::from_utf8_lossy(&percent_decode(uri)))
}

/// The file a relative URI names, beside the model; `None` for data URIs
/// and other schemes.
pub fn uri_path(uri: &str, neighbours: &Neighbours) -> Option<PathBuf> {
    if uri.starts_with("data:") || uri.contains("://") {
        return None;
    }
    let decoded = String::from_utf8_lossy(&percent_decode(uri)).replace('\\', "/");
    Some(match &neighbours.dir {
        Some(dir) => dir.join(&decoded),
        None => PathBuf::from(&decoded),
    })
}

fn read_file(path: &Path, ctx: &Ctx, what: &str, name: &str) -> Option<Vec<u8>> {
    // Missing files too: the viewer reloads when one appears.
    ctx.note_file(path);
    let len = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => {
            ctx.warn(format!("{what} is missing: {name}"));
            return None;
        }
    };
    if !ctx.take_file_bytes(len) {
        ctx.warn(format!("{what} is too large: {name}"));
        return None;
    }
    match std::fs::read(path) {
        Ok(data) => Some(data),
        Err(e) => {
            ctx.warn(format!("{what} couldn't be read: {name}: {e}"));
            None
        }
    }
}

const BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Decodes `%20`-style escapes, which glTF URIs use for spaces and
/// non-ASCII names.  Malformed escapes are kept as they are.
pub fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Loads the buffers marked in `needed`.  Buffer 0 without a URI is the
/// GLB's binary chunk.  Meshopt fallback buffers have no data of their own
/// and are never needed unless decompression fails.
pub fn load_buffers<'a>(
    root: &Root,
    bin: Option<&'a [u8]>,
    needed: &[bool],
    neighbours: &Neighbours,
    ctx: &Ctx,
) -> Vec<Option<Cow<'a, [u8]>>> {
    let load = |(i, b): (usize, &crate::json::BufferJ)| -> Option<Cow<'a, [u8]>> {
        if !needed[i] {
            return None;
        }
        let data: Cow<'a, [u8]> = match &b.uri {
            None if i == 0 && bin.is_some() => Cow::Borrowed(bin?),
            None => {
                if !is_fallback(b) {
                    ctx.warn(format!("buffer {i} has no data"));
                }
                return None;
            }
            Some(uri) => Cow::Owned(read_uri(uri, neighbours, ctx, &format!("buffer {i}"))?),
        };
        if (data.len() as u64) < b.byte_length {
            ctx.warn(format!("buffer {i} is shorter than it claims ({} of {} bytes)", data.len(), b.byte_length));
        }
        Some(data)
    };
    // External files load in parallel; GLB buffers cost nothing either way.
    root.buffers.par_iter().enumerate().map(load).collect()
}

/// Where each buffer's bytes are on disk, so pictures inside them can be
/// kept as byte ranges of files (`Original::File`) rather than copies.
pub struct Origins {
    /// Per buffer: the file holding it whole and the offset of its first
    /// byte there.
    buffers: Vec<Option<(PathBuf, u64)>>,
    /// Keep copies of pictures that aren't file ranges (`Limits::originals`).
    pub memory: bool,
}

impl Origins {
    /// `on_disk` says the bytes being loaded are the file at `path`, as
    /// read from disk, so ranges of the GLB's binary chunk point into it.
    pub fn new(
        root: &Root,
        container: &Container,
        path: &Path,
        on_disk: bool,
        neighbours: &Neighbours,
        memory: bool,
    ) -> Self {
        let buffers = root
            .buffers
            .iter()
            .enumerate()
            .map(|(i, b)| match &b.uri {
                None if i == 0 && container.bin.is_some() && on_disk => {
                    Some((path.to_path_buf(), container.bin_offset))
                }
                None => None,
                Some(uri) if neighbours.enabled => uri_path(uri, neighbours).map(|p| (p, 0)),
                Some(_) => None,
            })
            .collect();
        Origins { buffers, memory }
    }

    /// The original of a picture held in buffer view `view`, whose bytes
    /// are `data`.
    pub fn of_view(&self, root: &Root, view: usize, data: &ViewData) -> Option<slerp_scene::textures::Original> {
        use slerp_scene::textures::Original;
        let json = root.buffer_views.get(view)?;
        let file = match data {
            ViewData::Borrowed(_) => self.buffers.get(json.buffer).cloned().flatten(),
            ViewData::Decoded(..) => None,
        };
        match file {
            Some((path, start)) => Some(Original::File {
                path,
                offset: start.checked_add(json.byte_offset)?,
                len: data.bytes().len() as u64,
            }),
            None => self.memory.then(|| Original::Memory(data.bytes().into())),
        }
    }
}

//endregion

/*┌─────────────────╮
| Buffer views    |
└─────────────────╯*/
//region Buffer views

/// A buffer view's bytes: a slice of a buffer, or meshopt's decoded output.
/// Decoded output is stored as `u32`s because meshopt's filters write
/// through `int` and `short` pointers, which must be aligned.
pub enum ViewData<'a> {
    Borrowed(&'a [u8]),
    Decoded(Vec<u32>, usize),
}

impl ViewData<'_> {
    pub fn bytes(&self) -> &[u8] {
        match self {
            ViewData::Borrowed(b) => b,
            ViewData::Decoded(words, len) => &bytemuck::cast_slice::<u32, u8>(words)[..*len],
        }
    }
}

fn slice<'a>(buffers: &'a [Option<Cow<'_, [u8]>>], buffer: usize, offset: u64, len: u64) -> Option<&'a [u8]> {
    let data = buffers.get(buffer)?.as_deref()?;
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    data.get(start..end)
}

pub fn meshopt_of(view: &BufferViewJ) -> Option<&MeshoptJ> {
    view.extensions.meshopt_khr.as_ref().or(view.extensions.meshopt_ext.as_ref())
}

/// Which buffers the views marked in `needed` read: the compressed source
/// for meshopt views, plus the plain buffer as a fallback.
pub fn buffers_for_views(root: &Root, needed: &[bool]) -> Vec<bool> {
    let mut buffers = vec![false; root.buffers.len()];
    let mut mark = |b: usize| {
        if let Some(slot) = buffers.get_mut(b) {
            *slot = true;
        }
    };
    for (view, _) in root.buffer_views.iter().zip(needed).filter(|(_, n)| **n) {
        if let Some(m) = meshopt_of(view) {
            mark(m.buffer);
        }
        // A compressed view's plain buffer helps only when decompression
        // fails, and only if it isn't an empty fallback buffer.
        if !root.buffers.get(view.buffer).is_some_and(is_fallback) {
            mark(view.buffer);
        }
    }
    buffers
}

fn is_fallback(b: &crate::json::BufferJ) -> bool {
    b.extensions.meshopt_ext.as_ref().or(b.extensions.meshopt_khr.as_ref()).is_some_and(|f| f.fallback)
}

/// Resolves the views marked in `needed`, decoding meshopt-compressed ones
/// in parallel.  A view that can't be resolved is `None`, with a warning.
pub fn resolve_views<'a>(
    root: &Root,
    buffers: &'a [Option<Cow<'_, [u8]>>],
    needed: &[bool],
    ctx: &Ctx,
) -> Result<Vec<Option<ViewData<'a>>>, LoadError> {
    let resolve = |(i, view): (usize, &BufferViewJ)| -> Result<Option<ViewData<'a>>, LoadError> {
        if !needed[i] {
            return Ok(None);
        }
        let plain = || slice(buffers, view.buffer, view.byte_offset, view.byte_length).map(ViewData::Borrowed);
        let Some(m) = meshopt_of(view) else {
            let data = plain();
            if data.is_none() {
                ctx.warn(format!("buffer view {i} lies outside its buffer"));
            }
            return Ok(data);
        };
        let decoded = match slice(buffers, m.buffer, m.byte_offset, m.byte_length) {
            Some(src) => meshopt_decode(m, src, ctx),
            None => Err(LoadError::invalid("compressed data lies outside its buffer")),
        };
        match decoded {
            Ok((words, len)) => Ok(Some(ViewData::Decoded(words, len))),
            Err(e @ LoadError::TooLarge(_)) => Err(e),
            Err(e) => match plain() {
                Some(data) => Ok(Some(data)),
                None => {
                    ctx.warn(format!("buffer view {i} couldn't be decompressed: {e}"));
                    Ok(None)
                }
            },
        }
    };
    root.buffer_views.par_iter().enumerate().map(resolve).collect()
}

//endregion

/*┌─────────────────╮
| Meshopt         |
└─────────────────╯*/
//region Meshopt

/// Decodes one meshopt-compressed buffer view into `count * byteStride`
/// bytes.  Every precondition meshoptimizer asserts is checked first, since
/// a failed C assert would abort the process; the decoders themselves are
/// written to be safe on untrusted input.
fn meshopt_decode(m: &MeshoptJ, src: &[u8], ctx: &Ctx) -> Result<(Vec<u32>, usize), LoadError> {
    let bad = |msg: &str| Err(LoadError::invalid(format!("meshopt: {msg}")));
    let (Ok(count), Ok(stride)) = (usize::try_from(m.count), usize::try_from(m.byte_stride)) else {
        return bad("count out of range");
    };
    let Some(len) = count.checked_mul(stride) else { return bad("count out of range") };
    let filter = m.filter.as_deref().unwrap_or("NONE");
    match m.mode.as_str() {
        "ATTRIBUTES" if stride == 0 || stride > 256 || !stride.is_multiple_of(4) => return bad("bad byteStride"),
        "TRIANGLES" if !count.is_multiple_of(3) => return bad("triangle count isn't a multiple of 3"),
        "TRIANGLES" | "INDICES" if stride != 2 && stride != 4 => return bad("bad index size"),
        "TRIANGLES" | "INDICES" if filter != "NONE" => return bad("filters apply only to attributes"),
        "ATTRIBUTES" | "TRIANGLES" | "INDICES" => {}
        other => return bad(&format!("unknown mode {other}")),
    }
    match filter {
        "NONE" => {}
        "OCTAHEDRAL" | "COLOR" if stride != 4 && stride != 8 => return bad("bad stride for filter"),
        "QUATERNION" if stride != 8 => return bad("bad stride for filter"),
        "OCTAHEDRAL" | "COLOR" | "QUATERNION" | "EXPONENTIAL" => {}
        other => return bad(&format!("unknown filter {other}")),
    }
    ctx.reserve(len as u64)?;
    let mut words = vec![0u32; len.div_ceil(4)];
    if count == 0 {
        return Ok((words, 0));
    }
    let dst = words.as_mut_ptr().cast::<std::ffi::c_void>();
    // SAFETY: `dst` holds at least `count * stride` bytes, aligned to 4, and
    // the arguments meet every assertion of the called functions (checked
    // above).  The decoders bounds-check `src` themselves.
    let result = unsafe {
        use meshopt::ffi::*;
        let (s, n) = (src.as_ptr(), src.len());
        match m.mode.as_str() {
            "ATTRIBUTES" => meshopt_decodeVertexBuffer(dst, count, stride, s, n),
            "TRIANGLES" => meshopt_decodeIndexBuffer(dst, count, stride, s, n),
            _ => meshopt_decodeIndexSequence(dst, count, stride, s, n),
        }
    };
    if result != 0 {
        return bad(&format!("corrupt data (error {result})"));
    }
    // SAFETY: as above; filters rewrite the decoded elements in place.
    unsafe {
        use meshopt::ffi::*;
        match filter {
            "OCTAHEDRAL" => meshopt_decodeFilterOct(dst, count, stride),
            "QUATERNION" => meshopt_decodeFilterQuat(dst, count, stride),
            "EXPONENTIAL" => meshopt_decodeFilterExp(dst, count, stride),
            "COLOR" => meshopt_decodeFilterColor(dst, count, stride),
            _ => {}
        }
    }
    Ok((words, len))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("Box%20With%20Spaces.bin"), b"Box With Spaces.bin");
        assert_eq!(percent_decode("bad%zzescape%2"), b"bad%zzescape%2");
        assert_eq!(String::from_utf8(percent_decode("Unicode%E2%9D%A4.png")).unwrap(), "Unicode\u{2764}.png");
    }

    #[test]
    fn data_uris() {
        let ctx = Ctx::new(1 << 20, 1 << 20);
        let n = Neighbours { dir: None, enabled: false };
        assert_eq!(read_uri("data:application/octet-stream;base64,AAEC", &n, &ctx, "x"), Some(vec![0, 1, 2]));
        assert_eq!(read_uri("data:application/octet-stream;base64,AAE", &n, &ctx, "x"), Some(vec![0, 1]));
        assert_eq!(read_uri("data:,a%20b", &n, &ctx, "x"), Some(b"a b".to_vec()));
        assert_eq!(read_uri("data:;base64,!!!", &n, &ctx, "x"), None);
        assert_eq!(read_uri("other.bin", &n, &ctx, "x"), None);
    }

    #[test]
    fn meshopt_rejects_bad_parameters_without_aborting() {
        let ctx = Ctx::new(1 << 20, 1 << 20);
        let m = |mode: &str, filter: Option<&str>, stride, count| MeshoptJ {
            mode: mode.into(),
            filter: filter.map(Into::into),
            byte_stride: stride,
            count,
            ..Default::default()
        };
        let junk = [0xA0u8; 64];
        for bad in [
            m("ATTRIBUTES", None, 6, 10),
            m("ATTRIBUTES", None, 0, 10),
            m("ATTRIBUTES", None, 260, 10),
            m("TRIANGLES", None, 2, 10),
            m("TRIANGLES", None, 3, 9),
            m("INDICES", Some("OCTAHEDRAL"), 4, 9),
            m("ATTRIBUTES", Some("QUATERNION"), 4, 9),
            m("ATTRIBUTES", Some("OCTAHEDRAL"), 12, 9),
            m("SOMETHING", None, 4, 9),
            m("ATTRIBUTES", None, 4, u64::MAX),
        ] {
            assert!(meshopt_decode(&bad, &junk, &ctx).is_err());
        }
        // Valid parameters with junk data fail cleanly too.
        assert!(meshopt_decode(&m("ATTRIBUTES", Some("EXPONENTIAL"), 12, 100), &junk, &ctx).is_err());
        assert!(meshopt_decode(&m("TRIANGLES", None, 4, 300), &junk, &ctx).is_err());
        assert!(meshopt_decode(&m("INDICES", None, 2, 30), &junk, &ctx).is_err());
    }

    #[test]
    fn meshopt_round_trip() {
        let vertices: Vec<[u16; 4]> = (0..1000u16).map(|i| [i, i.wrapping_mul(7), 3, 0]).collect();
        let encoded = meshopt::encoding::encode_vertex_buffer(&vertices).unwrap();
        let ctx = Ctx::new(1 << 20, 1 << 20);
        let m = MeshoptJ { mode: "ATTRIBUTES".into(), byte_stride: 8, count: 1000, ..Default::default() };
        let (words, len) = meshopt_decode(&m, &encoded, &ctx).unwrap();
        let bytes = &bytemuck::cast_slice::<u32, u8>(&words)[..len];
        assert_eq!(bytes, bytemuck::cast_slice::<[u16; 4], u8>(&vertices));

        let indices: Vec<u32> = (0..300).map(|i| (i * 7) % 100).collect();
        let encoded = meshopt::encoding::encode_index_buffer(&indices, 100).unwrap();
        let m = MeshoptJ { mode: "TRIANGLES".into(), byte_stride: 4, count: 300, ..Default::default() };
        let (words, _) = meshopt_decode(&m, &encoded, &ctx).unwrap();
        // The codec may rotate a triangle's corners, keeping its winding.
        let canonical = |t: &[u32]| {
            let r = (0..3).min_by_key(|&k| t[k]).unwrap_or(0);
            [t[r], t[(r + 1) % 3], t[(r + 2) % 3]]
        };
        let decoded: Vec<_> = words.chunks(3).map(canonical).collect();
        let original: Vec<_> = indices.chunks(3).map(canonical).collect();
        assert_eq!(decoded, original);
    }
}
