//! The decoded texture model shared by every decoder, the scene loaders and
//! the renderer.

use std::borrow::Cow;
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

/// Pixels of one surface, always four channels in R, G, B, A order and
/// rows top to bottom.  Low dynamic range formats decode to 8 bits per
/// channel; HDR formats (Radiance, BC6H, ASTC HDR, half and float formats)
/// decode to 32-bit floats.
///
/// Only [`crate::decode_for_gpu`] gives [`Pixels::Blocks`]: blocks of a
/// GPU block-compressed format, which a GPU samples directly and which
/// [`crate::decompress`] turns into `Rgba8` for whatever needs texels.
#[derive(Clone)]
pub enum Pixels {
    Rgba8(Vec<u8>),
    Rgba32F(Vec<f32>),
    /// Blocks in rows from the top, each covering 4x4 texels (the edge
    /// blocks of levels smaller than that cover texels past the edge).
    Blocks(BlockFormat, Vec<u8>),
}

impl Pixels {
    /// The number of texels, which is a quarter of the number of values.
    /// For blocks, the texels the blocks cover, which can be a few more than
    /// the level holds when its size isn't a multiple of 4.
    pub fn texel_count(&self) -> usize {
        match self {
            Pixels::Rgba8(v) => v.len() / 4,
            Pixels::Rgba32F(v) => v.len() / 4,
            Pixels::Blocks(f, v) => v.len() / f.block_bytes() * 16,
        }
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self, Pixels::Rgba32F(_))
    }
}

impl fmt::Debug for Pixels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pixels::Rgba8(v) => write!(f, "Rgba8({} texels)", v.len() / 4),
            Pixels::Rgba32F(v) => write!(f, "Rgba32F({} texels)", v.len() / 4),
            Pixels::Blocks(b, v) => write!(f, "{b:?}({} blocks)", v.len() / b.block_bytes()),
        }
    }
}

/// GPU block-compressed formats that textures can stay in, all with 4x4
/// texel blocks: a quarter (16-byte blocks) or an eighth (8-byte blocks)
/// of the memory of 8-bit RGBA.  The BC formats are on every desktop GPU;
/// colour ones are sampled as sRGB or linear as the texture's colour space
/// says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFormat {
    /// Opaque RGB (DXT1), what Basis ETC1S transcodes to fastest.
    Bc1,
    /// RGB with smooth alpha (DXT5), for ETC1S with alpha.
    Bc3,
    /// Two channels, red and green (RGTC2), for normal maps: sampled as
    /// red, green, 0, 1, which is how two-channel textures decode to RGBA
    /// too.
    Bc5,
    /// RGBA (BPTC), what Basis UASTC is designed to transcode to.
    Bc7,
    /// ASTC LDR with 4x4 blocks, on mobile and Apple GPUs.
    Astc4x4,
}

impl BlockFormat {
    pub const fn block_bytes(self) -> usize {
        match self {
            BlockFormat::Bc1 => 8,
            _ => 16,
        }
    }
}

/// The block-compressed formats the GPU a texture is decoded for samples
/// directly (see [`crate::decode_for_gpu`]).  The default, none, decodes to
/// RGBA as [`crate::decode`] does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuBlocks {
    /// BC1 to BC7 (`TEXTURE_COMPRESSION_BC`): desktop GPUs, Apple silicon.
    pub bc: bool,
    /// ASTC LDR (`TEXTURE_COMPRESSION_ASTC`): mobile GPUs, Apple silicon.
    pub astc: bool,
}

impl GpuBlocks {
    pub fn any(self) -> bool {
        self.bc || self.astc
    }
}

/// The shape of a texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureKind {
    D2,
    /// Six faces per layer, in the order +X, -X, +Y, -Y, +Z, -Z.
    Cube,
    D3,
}

/// How the stored values are encoded.  The renderer still decides per
/// material slot, because glTF's PNG and JPEG images don't say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
    Linear,
    /// The container doesn't say (PNG, JPEG): sRGB for colour slots, linear
    /// for data slots such as normal maps.
    Unspecified,
}

/// One mip level.  `surfaces` holds one entry per array layer and face, in
/// the order layer 0 face 0..faces, layer 1 face 0..faces, and so on.  A 3D
/// texture's surface holds all `depth` slices one after another.
#[derive(Clone, Debug)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub surfaces: Vec<Pixels>,
}

/// Facts about the file the texture came from, for the viewer's inspector
/// and for picking which mip to decode.
#[derive(Clone, Debug, Default)]
pub struct SourceInfo {
    /// The container, such as "KTX2", "DDS" or "Radiance HDR".
    pub container: String,
    /// The pixel format as the file names it, such as "BC7_SRGB_BLOCK" or
    /// "UASTC LDR 4x4".
    pub format: String,
    /// Supercompression, such as "zstd" or "BasisLZ", when there is any.
    pub supercompression: Option<String>,
    /// Size of the largest mip level in the file, which may be larger than
    /// the decoded levels when the caller asked for a smaller one.
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layers: u32,
    pub faces: u32,
    pub mip_count: u32,
    /// Bytes of the file.
    pub file_bytes: u64,
    /// What the file says about itself, in file order, for the inspector:
    /// FITS header cards, DPX film and television fields, XISF properties.
    /// Names are the file's own keywords; values are text.
    pub header: Vec<(String, String)>,
    /// The picture's own name inside the file that holds it: a glTF
    /// image's name or URI, an FBX texture's file name, a packed Blender
    /// image's name.  Empty when it has none.
    pub name: String,
    /// The encoded picture these pixels were decoded from, so it can be
    /// saved or embedded again unchanged (a PNG inside a GLB, an FBX's
    /// embedded texture).  Only set when the pixels are exactly that
    /// picture, possibly decoded smaller; a loader that changes them
    /// (packing channels, applying a palette) leaves it `None`.
    pub original: Option<Original>,
}

/// Where the encoded bytes of a picture are.  File ranges cost nothing to
/// keep; copies in memory are made only when a load asks for them
/// (`Limits::originals` in the scene crate), since the decoded pixels are
/// what is drawn.
#[derive(Clone)]
pub enum Original {
    /// Shared bytes, for pictures that aren't a byte range of a file on
    /// disk: a glTF data URI, a member of a compressed archive.
    Memory(Arc<[u8]>),
    /// `len` bytes at `offset` in the file at `path`: a GLB's embedded
    /// picture, a glTF's neighbouring PNG (the whole file), an FBX's
    /// embedded texture, a USDZ member.
    File { path: PathBuf, offset: u64, len: u64 },
}

/// Pictures larger than this aren't read back from a file range, which
/// guards against a range that a changed or damaged file made absurd.
const MAX_ORIGINAL_BYTES: u64 = 1 << 30;

impl Original {
    /// The number of bytes.
    pub fn len(&self) -> u64 {
        match self {
            Original::Memory(b) => b.len() as u64,
            Original::File { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes, read from the file when they live there.  Fails when the
    /// file has become shorter than the range since it was loaded.
    pub fn read(&self) -> std::io::Result<Cow<'_, [u8]>> {
        match self {
            Original::Memory(b) => Ok(Cow::Borrowed(b)),
            Original::File { path, offset, len } => {
                if *len > MAX_ORIGINAL_BYTES {
                    return Err(std::io::Error::other(format!("{len} bytes is too large a picture")));
                }
                let mut file = std::fs::File::open(path)?;
                file.seek(SeekFrom::Start(*offset))?;
                let mut out = Vec::with_capacity(*len as usize);
                file.take(*len).read_to_end(&mut out)?;
                if out.len() as u64 != *len {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the file is shorter than it was",
                    ));
                }
                Ok(Cow::Owned(out))
            }
        }
    }
}

impl Original {
    /// The picture's container from its first bytes, so it can be saved
    /// under the right extension; `None` when unknown or unreadable.
    pub fn format(&self) -> Option<Format> {
        let head = match self {
            Original::Memory(b) => b[..b.len().min(64)].to_vec(),
            Original::File { path, offset, len } => {
                let mut file = std::fs::File::open(path).ok()?;
                file.seek(SeekFrom::Start(*offset)).ok()?;
                let mut head = Vec::with_capacity(64);
                file.take((*len).min(64)).read_to_end(&mut head).ok()?;
                head
            }
        };
        crate::sniff(&head)
    }
}

impl fmt::Debug for Original {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Original::Memory(b) => write!(f, "Memory({} bytes)", b.len()),
            Original::File { path, offset, len } => write!(f, "File({}, {offset}, {len} bytes)", path.display()),
        }
    }
}

/// A decoded texture: one or more mip levels, each holding every layer and
/// face that was decoded.
#[derive(Clone, Debug)]
pub struct Texture {
    pub kind: TextureKind,
    /// Number of array layers decoded, 1 for a plain texture.
    pub layers: u32,
    /// 6 for cubemaps, otherwise 1.
    pub faces: u32,
    /// The decoded mip levels, largest first.  Never empty.
    pub levels: Vec<Level>,
    pub color_space: ColorSpace,
    /// True when any texel's alpha can be below 1.
    pub has_alpha: bool,
    pub source: SourceInfo,
}

impl Texture {
    /// A plain 2D texture with a single level.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>, color_space: ColorSpace) -> Self {
        let has_alpha = data.chunks_exact(4).any(|p| p[3] != 255);
        Self::single(width, height, Pixels::Rgba8(data), color_space, has_alpha)
    }

    /// A plain 2D HDR texture with a single level.
    pub fn from_rgba32f(width: u32, height: u32, data: Vec<f32>) -> Self {
        let has_alpha = data.chunks_exact(4).any(|p| p[3] < 1.0);
        Self::single(width, height, Pixels::Rgba32F(data), ColorSpace::Linear, has_alpha)
    }

    fn single(width: u32, height: u32, pixels: Pixels, color_space: ColorSpace, has_alpha: bool) -> Self {
        debug_assert_eq!(pixels.texel_count(), (width * height) as usize);
        Texture {
            kind: TextureKind::D2,
            layers: 1,
            faces: 1,
            levels: vec![Level { width, height, depth: 1, surfaces: vec![pixels] }],
            color_space,
            has_alpha,
            source: SourceInfo { width, height, depth: 1, layers: 1, faces: 1, mip_count: 1, ..Default::default() },
        }
    }

    pub fn width(&self) -> u32 {
        self.levels[0].width
    }

    pub fn height(&self) -> u32 {
        self.levels[0].height
    }

    pub fn is_hdr(&self) -> bool {
        self.levels[0].surfaces[0].is_hdr()
    }

    /// The first surface of the largest level: layer 0, face 0, and for 3D
    /// textures the whole volume.
    pub fn base(&self) -> &Pixels {
        &self.levels[0].surfaces[0]
    }
}

/// A simple 8-bit RGBA picture with straight (not premultiplied) alpha, the
/// form thumbnails are written in.
#[derive(Clone)]
pub struct Image8 {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image8 {
    pub fn new(width: u32, height: u32) -> Self {
        Image8 { width, height, pixels: vec![0; (width * height * 4) as usize] }
    }
}

impl fmt::Debug for Image8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image8({}x{})", self.width, self.height)
    }
}

/// Containers and image formats this crate recognises by their first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Ktx2,
    Ktx1,
    Dds,
    Astc,
    Hdr,
    Png,
    Jpeg,
    Tga,
    Bmp,
    Webp,
    Pcx,
    /// Flexible Image Transport System, the astronomers' format.
    Fits,
    /// PixInsight's image format.
    Xisf,
    /// Film scans: Digital Picture Exchange and Kodak's Cineon.
    Dpx,
    Cineon,
    /// Netpbm's arbitrary map (P7) and the suckless farbfeld format.
    Pam,
    Farbfeld,
    /// JPEG XR (HD Photo, Windows HDR screenshots).
    Jxr,
}

impl Format {
    /// The MIME type, for saving a picture or naming it in a glTF file.
    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Ktx2 => "image/ktx2",
            Format::Ktx1 => "image/ktx",
            Format::Dds => "image/vnd-ms.dds",
            Format::Astc => "image/astc",
            Format::Hdr => "image/vnd.radiance",
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Tga => "image/x-tga",
            Format::Bmp => "image/bmp",
            Format::Webp => "image/webp",
            Format::Pcx => "image/vnd.zbrush.pcx",
            Format::Fits => "image/fits",
            Format::Xisf => "image/x-xisf",
            Format::Dpx => "image/x-dpx",
            Format::Cineon => "image/cineon",
            Format::Pam => "image/x-portable-arbitrarymap",
            Format::Farbfeld => "image/x-farbfeld",
            Format::Jxr => "image/jxr",
        }
    }

    /// The usual file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Ktx2 => "ktx2",
            Format::Ktx1 => "ktx",
            Format::Dds => "dds",
            Format::Astc => "astc",
            Format::Hdr => "hdr",
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Tga => "tga",
            Format::Bmp => "bmp",
            Format::Webp => "webp",
            Format::Pcx => "pcx",
            Format::Fits => "fits",
            Format::Xisf => "xisf",
            Format::Dpx => "dpx",
            Format::Cineon => "cin",
            Format::Pam => "pam",
            Format::Farbfeld => "ff",
            Format::Jxr => "jxr",
        }
    }
}

/// What to decode.  Thumbnails ask for the smallest mip that is still at
/// least `min_size` on its longer side and only the first layer; the viewer
/// asks for everything.
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// Decode only the smallest stored mip level whose longer side is at
    /// least this, then downscale if no stored level is small enough and
    /// `max_size` is set.  `None` decodes from the largest level.
    pub min_size: Option<u32>,
    /// Downscale the result so its longer side is at most this.
    pub max_size: Option<u32>,
    /// Decode every stored mip level rather than one.
    pub all_mips: bool,
    /// Decode only the first array layer (all faces of it, for cubemaps).
    pub first_layer_only: bool,
}

impl DecodeOptions {
    /// Everything, at full size, for the viewer.
    pub fn full() -> Self {
        DecodeOptions { min_size: None, max_size: None, all_mips: true, first_layer_only: false }
    }

    /// One level near `size`, first layer only, for a thumbnail or a model's
    /// textures when rendering one.
    pub fn preview(size: u32) -> Self {
        DecodeOptions { min_size: Some(size), max_size: Some(size), all_mips: false, first_layer_only: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TextureError {
    #[error("unsupported texture: {0}")]
    Unsupported(String),
    #[error("invalid texture file: {0}")]
    Invalid(String),
    #[error("texture too large: {0}")]
    TooLarge(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
