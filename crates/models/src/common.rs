//! Pieces every loader shares: number parsing for text formats, a bounded
//! binary reader, polygon triangulation, colour conversion, and texture
//! loading from neighbouring files or embedded bytes.

use glam::{Mat3, Vec2, Vec3, Vec4};
use slerp_scene::textures::{self, DecodeOptions, Original, Pixels};
use slerp_scene::{ColorSpace, Limits, LoadError, Texture, UvTransform};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/*┌─────────────────╮
| Bytes and text  |
└─────────────────╯*/
//region Bytes and text

/// Finds `needle` in `haystack`.
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        match haystack[i..=last_start].iter().position(|&b| b == first) {
            Some(off) => {
                let at = i + off;
                if &haystack[at..at + needle.len()] == needle {
                    return Some(at);
                }
                i = at + 1;
            }
            None => return None,
        }
    }
    None
}

/// True when `head` reads as text: no NUL bytes and few control characters.
/// A UTF-8 sequence cut at the end of `head` is fine.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return false;
    }
    let control = head.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 0x0c)).count();
    !head.contains(&0) && control * 50 <= head.len()
}

/// Text as UTF-8 bytes: a UTF-8 byte order mark is dropped, and UTF-16
/// with a byte order mark (which some Windows exporters write) is
/// converted.  Anything else is passed through as it is.
pub fn utf8_text(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let units = |big: bool| {
        bytes[2..]
            .chunks_exact(2)
            .map(move |p| if big { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
    };
    let decode = |big: bool| {
        let text: String = char::decode_utf16(units(big)).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
        std::borrow::Cow::Owned(text.into_bytes())
    };
    match bytes {
        [0xef, 0xbb, 0xbf, rest @ ..] => std::borrow::Cow::Borrowed(rest),
        [0xff, 0xfe, ..] => decode(false),
        [0xfe, 0xff, ..] => decode(true),
        _ => std::borrow::Cow::Borrowed(bytes),
    }
}

/// True when `head` starts with a UTF-16 byte order mark.
pub fn is_utf16(head: &[u8]) -> bool {
    head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff])
}

/// Splits on ASCII whitespace without allocating.
pub fn tokens(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|b| b.is_ascii_whitespace()).filter(|t| !t.is_empty())
}

/// Removes leading and trailing ASCII whitespace.
pub fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |e| e + 1);
    &s[start..end.max(start)]
}

const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20,
    1e21, 1e22,
];

/// Parses a decimal number as C's `strtod` would for the forms model files
/// use, several times faster than `str::parse`, which large OBJ, PLY and
/// COLLADA files spend most of their load time in.  Anything unusual
/// (`nan`, `inf`, very long mantissas) falls back to the standard parser.
pub fn parse_f32(s: &[u8]) -> Option<f32> {
    match parse_fast(s) {
        Some(v) => Some(v as f32),
        None => parse_slow(s),
    }
}

/// `parse_f32` in double precision, for georeferenced coordinates that
/// need more than `f32`'s seven digits.
pub fn parse_f64(s: &[u8]) -> Option<f64> {
    match parse_fast(s) {
        Some(v) => Some(v),
        None => std::str::from_utf8(s).ok()?.parse::<f64>().ok(),
    }
}

/// The fast path of the number parsers: `None` sends the caller to the
/// standard library's parser.
fn parse_fast(s: &[u8]) -> Option<f64> {
    let mut i = 0;
    let neg = match s.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let mut mantissa: u64 = 0;
    let mut digits = 0u32;
    let mut exp10: i32 = 0;
    let mut any = false;
    while let Some(&c) = s.get(i) {
        if !c.is_ascii_digit() {
            break;
        }
        any = true;
        if digits < 19 {
            mantissa = mantissa * 10 + (c - b'0') as u64;
            if mantissa != 0 {
                digits += 1;
            }
        } else {
            exp10 += 1;
        }
        i += 1;
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        while let Some(&c) = s.get(i) {
            if !c.is_ascii_digit() {
                break;
            }
            any = true;
            if digits < 19 {
                mantissa = mantissa * 10 + (c - b'0') as u64;
                if mantissa != 0 {
                    digits += 1;
                }
                exp10 -= 1;
            }
            i += 1;
        }
    }
    if !any {
        return None;
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        i += 1;
        let eneg = match s.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let mut e: i32 = 0;
        let start = i;
        while let Some(&c) = s.get(i) {
            if !c.is_ascii_digit() {
                break;
            }
            e = (e * 10 + (c - b'0') as i32).min(100_000);
            i += 1;
        }
        if i == start {
            return None;
        }
        exp10 += if eneg { -e } else { e };
    }
    if i != s.len() {
        return None;
    }
    let value = if mantissa == 0 {
        0.0
    } else if digits <= 15 && exp10.unsigned_abs() <= 22 {
        let m = mantissa as f64;
        if exp10 < 0 { m / POW10[(-exp10) as usize] } else { m * POW10[exp10 as usize] }
    } else {
        return None;
    };
    Some(if neg { -value } else { value })
}

fn parse_slow(s: &[u8]) -> Option<f32> {
    std::str::from_utf8(s).ok()?.parse::<f32>().ok()
}

/// Parses a decimal integer with an optional sign.
pub fn parse_i64(s: &[u8]) -> Option<i64> {
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits.len() > 18 {
        return None;
    }
    let mut v: i64 = 0;
    for &c in digits {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as i64;
    }
    Some(if neg { -v } else { v })
}

/// A little- or big-endian reader over a byte slice that never reads past
/// the end: every read returns `None` instead.
#[derive(Clone)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0, big_endian: false }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let b = self.bytes(N)?;
        let mut out = [0; N];
        out.copy_from_slice(b);
        if self.big_endian {
            out.reverse();
        }
        Some(out)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_le_bytes)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub fn i32(&mut self) -> Option<i32> {
        self.array().map(i32::from_le_bytes)
    }

    pub fn f32(&mut self) -> Option<f32> {
        self.array().map(f32::from_le_bytes)
    }

    pub fn f64(&mut self) -> Option<f64> {
        self.array().map(f64::from_le_bytes)
    }
}

/// A quick hasher for integer keys, since vertex deduplication hashes tens
/// of millions of them and SipHash's resistance to crafted keys buys
/// nothing here.
#[derive(Default, Clone, Copy)]
pub struct FastHasher(u64);

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(b));
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.write_u64(v as u64);
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }

    fn finish(&self) -> u64 {
        // Fold the high bits down: hashbrown picks buckets from the low
        // bits and the control byte from the top seven.
        self.0 ^ (self.0 >> 29)
    }
}

pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

//endregion

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

/// Converts an sRGB-encoded channel to linear, for vertex and material
/// colours that files store as they appear on screen.
pub fn srgb_to_linear(c: f32) -> f32 {
    if !c.is_finite() || c <= 0.0 {
        0.0
    } else if c <= 0.04045 {
        c / 12.92
    } else if c < 1.0 {
        ((c + 0.055) / 1.055).powf(2.4)
    } else {
        c
    }
}

/// Encodes a linear channel as sRGB, the inverse of `srgb_to_linear`.
pub fn linear_to_srgb(c: f32) -> f32 {
    if !c.is_finite() || c <= 0.0 {
        0.0
    } else if c <= 0.003_130_8 {
        c * 12.92
    } else if c < 1.0 {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    } else {
        1.0
    }
}

pub fn srgb_color(r: f32, g: f32, b: f32, a: f32) -> Vec4 {
    Vec4::new(
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
        if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 },
    )
}

/// A material colour constant, clamped to sensible values.  Material
/// colours are taken as linear: Blender, Maya and 3ds Max work in linear
/// colour and write it to OBJ, FBX and COLLADA, and Blender's importers read
/// it back unchanged.  Vertex colours and palettes, which store what was seen
/// on screen, go through `srgb_color` instead.
pub fn linear_color(r: f32, g: f32, b: f32, a: f32) -> Vec4 {
    let c = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
    Vec4::new(c(r), c(g), c(b), if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 })
}

/// 8-bit sRGB to linear, through a table built once.
pub fn srgb8_to_linear(c: u8) -> f32 {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))[c as usize]
}

/// The polygon's normal by Newell's method, which is robust for concave and
/// slightly non-planar polygons.  Its length is twice the polygon's area.
pub fn newell_normal(points: &[Vec3]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for (i, &a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// Splits a polygon into triangles, appending corner index triples (indices
/// into `points`) to `out`.  Convex polygons become fans; concave ones are
/// ear-clipped in the polygon's best-fit plane.  Very large concave polygons
/// fall back to a fan to keep the cost bounded.
pub fn triangulate(points: &[Vec3], out: &mut Vec<[u32; 3]>) {
    let n = points.len();
    if n < 3 {
        return;
    }
    if n == 3 {
        out.push([0, 1, 2]);
        return;
    }
    let normal = newell_normal(points);
    if n == 4 {
        // Quads are most polygons in most files: no allocation, split along
        // the diagonal from a reflex corner when there is one.
        let corner = |i: usize| (points[i] - points[(i + 3) % 4]).cross(points[(i + 1) % 4] - points[i]).dot(normal);
        let reflex = (0..4).find(|&i| corner(i) < 0.0).unwrap_or(0) as u32;
        out.push([reflex, (reflex + 1) % 4, (reflex + 2) % 4]);
        out.push([reflex, (reflex + 2) % 4, (reflex + 3) % 4]);
        return;
    }
    // 1. Project onto the plane most facing the polygon's normal, keeping
    //    its winding counter-clockwise.
    let a = normal.abs();
    let flat: Vec<Vec2> = if a.x >= a.y && a.x >= a.z {
        points.iter().map(|p| if normal.x > 0.0 { Vec2::new(p.y, p.z) } else { Vec2::new(p.z, p.y) }).collect()
    } else if a.y >= a.z {
        points.iter().map(|p| if normal.y > 0.0 { Vec2::new(p.z, p.x) } else { Vec2::new(p.x, p.z) }).collect()
    } else {
        points.iter().map(|p| if normal.z > 0.0 { Vec2::new(p.x, p.y) } else { Vec2::new(p.y, p.x) }).collect()
    };
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    // 2. Convex polygons (most quads) are fans.
    let reflex: Vec<bool> = (0..n).map(|i| cross(flat[(i + n - 1) % n], flat[i], flat[(i + 1) % n]) < 0.0).collect();
    let reflex_count = reflex.iter().filter(|&&r| r).count();
    if reflex_count == 0 || normal.length_squared() == 0.0 || n > 512 {
        for i in 1..n - 1 {
            out.push([0, i as u32, (i + 1) as u32]);
        }
        return;
    }
    // 3. Ear clipping, O(n^2), with a guaranteed cut when no clean ear
    //    exists (self-intersecting input) so it always terminates.
    let mut ring: Vec<usize> = (0..n).collect();
    while ring.len() > 3 {
        let m = ring.len();
        let mut cut = None;
        for k in 0..m {
            let (p, c, nx) = (ring[(k + m - 1) % m], ring[k], ring[(k + 1) % m]);
            if cross(flat[p], flat[c], flat[nx]) <= 0.0 {
                continue;
            }
            // Only reflex corners can lie in an ear.  Corners at the same
            // spot as the ear's own, as on the bridge a keyhole polygon
            // uses to describe a hole, don't block it.
            let (a, b, d) = (flat[p], flat[c], flat[nx]);
            let inside = (0..m).any(|j| {
                let q = ring[j];
                let t = flat[q];
                if t == a || t == b || t == d {
                    return false;
                }
                let reflex = cross(flat[ring[(j + m - 1) % m]], t, flat[ring[(j + 1) % m]]) <= 0.0;
                reflex && cross(a, b, t) >= 0.0 && cross(b, d, t) >= 0.0 && cross(d, a, t) >= 0.0
            });
            if !inside {
                cut = Some(k);
                break;
            }
        }
        let k = cut.unwrap_or(0);
        let m = ring.len();
        out.push([ring[(k + m - 1) % m] as u32, ring[k] as u32, ring[(k + 1) % m] as u32]);
        ring.remove(k);
    }
    out.push([ring[0] as u32, ring[1] as u32, ring[2] as u32]);
}

/// Converts a texture-coordinate transform written for UVs with v pointing
/// up (OBJ, FBX, COLLADA) into the scene's convention.  The loaders store
/// `(u, 1 - v)`, so the transform is conjugated by that flip.  Returns `None`
/// for the identity.
pub fn flipped_uv_transform(m: Mat3) -> Option<UvTransform> {
    let flip = Mat3::from_cols(Vec3::X, Vec3::NEG_Y, Vec3::new(0.0, 1.0, 1.0));
    let t = flip * m * flip;
    let (c0, c1) = (t.x_axis.truncate(), t.y_axis.truncate());
    let rotation = c0.y.atan2(c0.x);
    let mut scale = Vec2::new(c0.length(), c1.length());
    if c0.perp_dot(c1) < 0.0 {
        scale.y = -scale.y;
    }
    let out = UvTransform { offset: t.z_axis.truncate(), rotation: -rotation, scale };
    let identity = out.offset.abs().max_element() < 1e-6
        && out.rotation.abs() < 1e-6
        && (out.scale - Vec2::ONE).abs().max_element() < 1e-6;
    (!identity && out.offset.is_finite() && out.scale.is_finite() && out.rotation.is_finite()).then_some(out)
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

/// Textures a model refers to, loaded once each and shared by materials.
/// Files are looked up next to the model (only when `Limits::neighbours`
/// allows), and every failure becomes a warning rather than an error.
pub struct TextureSet<'a> {
    dir: Option<PathBuf>,
    limits: &'a Limits,
    pub textures: Vec<Arc<Texture>>,
    by_key: HashMap<String, Option<usize>>,
    listings: HashMap<PathBuf, Vec<(String, PathBuf)>>,
    pub warnings: Vec<String>,
}

/// Folders next to a model where exporters commonly put its textures.
const TEXTURE_DIRS: [&str; 8] =
    ["textures", "texture", "tex", "images", "maps", "materials", "../textures", "../images"];

impl<'a> TextureSet<'a> {
    pub fn new(model: &Path, limits: &'a Limits) -> Self {
        let dir = limits.neighbours.then(|| model.parent().map(Path::to_path_buf).unwrap_or_default());
        TextureSet {
            dir,
            limits,
            textures: Vec::new(),
            by_key: HashMap::new(),
            listings: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    fn options(&self) -> DecodeOptions {
        self.limits.max_texture_size.map(DecodeOptions::preview).unwrap_or_else(DecodeOptions::full)
    }

    /// Loads an image file the model names, relative to the model's folder,
    /// and records a warning when it can't be found.
    pub fn file(&mut self, name: &str) -> Option<usize> {
        let result = self.file_quiet(name);
        if result.is_none() {
            self.missing(name);
        }
        result
    }

    /// Like `file`, without the warning, for callers that try several names.
    pub fn file_quiet(&mut self, name: &str) -> Option<usize> {
        let name = clean_name(name);
        if name.is_empty() {
            return None;
        }
        if let Some(&cached) = self.by_key.get(&name) {
            return cached;
        }
        let path = self.dir.clone().and_then(|dir| self.resolve(&dir, &name));
        let result = path.and_then(|path| {
            // Different spellings of one file share its texture.
            let key = format!("path:{}", path.display());
            if let Some(&cached) = self.by_key.get(&key) {
                return cached;
            }
            let decoded = match read_limited(&path, self.limits.max_file_bytes) {
                Some(bytes) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let original = Original::File { path: path.clone(), offset: 0, len: bytes.len() as u64 };
                    self.decode(&path.display().to_string(), &bytes, name, Some(original))
                }
                None => {
                    self.warnings.push(format!("texture unreadable or too large: {}", path.display()));
                    None
                }
            };
            self.by_key.insert(key, decoded);
            decoded
        });
        self.by_key.insert(name, result);
        result
    }

    /// Records that a texture the model names wasn't found, once per name.
    /// Nothing is recorded when neighbouring files are off.
    pub fn missing(&mut self, name: &str) {
        if self.dir.is_some() {
            let message = format!("texture not found: {}", clean_name(name));
            if !self.warnings.contains(&message) {
                self.warnings.push(message);
            }
        }
    }

    /// Decodes image bytes embedded in the model, under a key that identifies
    /// them so repeated references share one texture.  The key's last path
    /// segment becomes the picture's name.
    pub fn embedded(&mut self, key: &str, bytes: &[u8]) -> Option<usize> {
        let name = key.rsplit(['/', '\\']).next().unwrap_or("");
        let name = name.split_once(':').filter(|(n, _)| n.bytes().all(|b| b.is_ascii_digit())).map_or(name, |(_, r)| r);
        let name = name.to_string();
        let key = format!("embedded:{key}");
        if let Some(&cached) = self.by_key.get(&key) {
            return cached;
        }
        let original = self.limits.originals.then(|| Original::Memory(bytes.into()));
        let result = self.decode(&key, bytes, name, original);
        self.by_key.insert(key, result);
        result
    }

    /// Adds an already decoded texture.
    pub fn push(&mut self, tex: Texture) -> usize {
        self.textures.push(Arc::new(tex));
        self.textures.len() - 1
    }

    /// Reads a text or data file that sits next to the model, such as an
    /// OBJ's `.mtl`.  `None` when neighbours are off or it doesn't exist.
    pub fn read_neighbour(&mut self, name: &str) -> Option<(PathBuf, Vec<u8>)> {
        let dir = self.dir.clone()?;
        let name = clean_name(name);
        let path = self.resolve(&dir, &name)?;
        read_limited(&path, self.limits.max_file_bytes).map(|b| (path, b))
    }

    /// Decodes a picture, recording its name and where its encoded bytes
    /// are (`original`), so it can be saved unchanged.
    fn decode(&mut self, what: &str, bytes: &[u8], name: String, original: Option<Original>) -> Option<usize> {
        let opts = self.options();
        // The decoders promise not to panic, but a texture is never worth
        // losing the whole model over.
        match std::panic::catch_unwind(|| textures::decode(bytes, &opts)) {
            Ok(Ok(mut tex)) => {
                tex.source.name = name;
                tex.source.original = original;
                self.textures.push(Arc::new(tex));
                Some(self.textures.len() - 1)
            }
            Ok(Err(e)) => {
                self.warnings.push(format!("texture {what}: {e}"));
                None
            }
            Err(_) => {
                self.warnings.push(format!("texture {what}: decoder failed"));
                None
            }
        }
    }

    /// Finds the file a model names: as given, then by its bare name in the
    /// model's folder and the usual texture folders, ignoring case, since
    /// models often carry absolute paths from the author's machine.
    fn resolve(&mut self, dir: &Path, name: &str) -> Option<PathBuf> {
        let given = Path::new(name);
        let base = name.rsplit('/').next().unwrap_or(name);
        let mut candidates = Vec::new();
        if given.is_absolute() {
            candidates.push(given.to_path_buf());
        } else if !name.contains(':') {
            candidates.push(dir.join(given));
        }
        candidates.push(dir.join(base));
        for c in &candidates {
            if is_regular_file(c) {
                return Some(c.clone());
            }
        }
        let wanted = base.to_lowercase();
        let mut dirs = vec![dir.to_path_buf()];
        // A relative folder named in the path, such as "tex/wood.png".
        if let Some(parent) = given.parent().filter(|p| !given.is_absolute() && !p.as_os_str().is_empty()) {
            dirs.push(dir.join(parent));
        }
        dirs.extend(TEXTURE_DIRS.iter().map(|d| dir.join(d)));
        for d in dirs {
            let listing = self.listings.entry(d.clone()).or_insert_with(|| list_dir(&d));
            if let Some((_, path)) = listing.iter().find(|(n, _)| *n == wanted) {
                return Some(path.clone());
            }
        }
        None
    }
}

/// Lower-cased names of the regular files in a folder, plus its subfolders
/// under their own lower-cased names so "Textures" matches "textures".
fn list_dir(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten().take(10_000) {
        out.push((e.file_name().to_string_lossy().to_lowercase(), e.path()));
    }
    // Folder names that differ only in case ("Textures") resolve here too.
    if out.is_empty()
        && let (Some(parent), Some(leaf)) = (dir.parent(), dir.file_name())
    {
        let leaf = leaf.to_string_lossy().to_lowercase();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for e in entries.flatten().take(10_000) {
                if e.file_name().to_string_lossy().to_lowercase() == leaf && e.path() != dir {
                    return list_dir(&e.path());
                }
            }
        }
    }
    out
}

fn is_regular_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Reads a regular file of at most `max` bytes.  Devices and pipes are
/// refused before opening, since opening a FIFO a model names would hang.
/// Every model format reads its neighbours (`.mtl` files, textures, inlined
/// files) through here, so this records them for `Info::neighbours`.
pub fn read_limited(path: &Path, max: u64) -> Option<Vec<u8>> {
    slerp_scene::record_neighbour(path);
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut out = Vec::with_capacity(meta.len().min(1 << 30) as usize);
    std::fs::File::open(path).ok()?.take(max).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Normalises a file name from a model: Windows separators, `file://`
/// prefixes, URL escapes and quotes.
fn clean_name(name: &str) -> String {
    let mut s = name.trim().trim_matches('"').replace('\\', "/");
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_string();
        // "file:///C:/x" leaves "/C:/x".
        if s.as_bytes().get(2) == Some(&b':') {
            s.remove(0);
        }
    }
    if s.contains('%') {
        s = percent_decode(&s);
    }
    s
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first surface of a texture as 8-bit RGBA, converting HDR by clamping.
pub fn rgba8(tex: &Texture) -> Option<(u32, u32, std::borrow::Cow<'_, [u8]>)> {
    let level = tex.levels.first()?;
    let (w, h) = (level.width, level.height);
    match level.surfaces.first()? {
        Pixels::Rgba8(v) if v.len() >= (w as usize * h as usize * 4) => Some((w, h, std::borrow::Cow::Borrowed(v))),
        Pixels::Rgba32F(v) if v.len() >= (w as usize * h as usize * 4) => {
            Some((w, h, std::borrow::Cow::Owned(v.iter().map(|&f| (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect())))
        }
        _ => None,
    }
}

/// Guesses whether a texture plugged into a bump slot is really a tangent
/// space normal map, as many exporters write them there: mostly blue, with
/// red and green centred on one half.
pub fn looks_like_normal_map(tex: &Texture) -> bool {
    let Some((w, h, px)) = rgba8(tex) else { return false };
    let count = (w as usize * h as usize).max(1);
    let step = (count / 4096).max(1);
    let (mut r, mut g, mut b, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in (0..count).step_by(step) {
        let p = &px[i * 4..i * 4 + 4];
        r += p[0] as f64;
        g += p[1] as f64;
        b += p[2] as f64;
        n += 1.0;
    }
    let (r, g, b) = (r / n / 255.0, g / n / 255.0, b / n / 255.0);
    b > 0.65 && (r - 0.5).abs() < 0.2 && (g - 0.5).abs() < 0.2
}

/// Where a packed metallic-roughness channel comes from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChannelSource {
    pub texture: usize,
    /// 0 red .. 3 alpha.
    pub channel: u8,
    /// Use `1 - value`, for glossiness maps.
    pub invert: bool,
}

impl TextureSet<'_> {
    /// Packs separate roughness and metalness maps into one texture with
    /// roughness in green and metalness in blue, as glTF's material model
    /// wants.  A missing map's channel is white, so the material's constant
    /// factor applies there.  The smaller map is sampled up to the larger.
    pub fn pack_metal_rough(&mut self, rough: Option<ChannelSource>, metal: Option<ChannelSource>) -> Option<usize> {
        if rough.is_none() && metal.is_none() {
            return None;
        }
        let key = format!("pack:{rough:?}:{metal:?}");
        if let Some(&cached) = self.by_key.get(&key) {
            return cached;
        }
        let result = (|| {
            let get = |s: Option<ChannelSource>| -> Option<(ChannelSource, u32, u32, Vec<u8>)> {
                let s = s?;
                let (w, h, px) = rgba8(self.textures.get(s.texture)?)?;
                Some((s, w, h, px.into_owned()))
            };
            let (r, m) = (get(rough), get(metal));
            let (w, h) = match (&r, &m) {
                (Some(a), Some(b)) => (a.1.max(b.1), a.2.max(b.2)),
                (Some(a), None) | (None, Some(a)) => (a.1, a.2),
                (None, None) => return None,
            };
            let mut out = vec![255u8; w as usize * h as usize * 4];
            for (slot, src) in [(1usize, &r), (2usize, &m)] {
                let Some((s, sw, sh, px)) = src else { continue };
                for y in 0..h {
                    let sy = (y as u64 * *sh as u64 / h as u64) as usize;
                    for x in 0..w {
                        let sx = (x as u64 * *sw as u64 / w as u64) as usize;
                        let v = px[(sy * *sw as usize + sx) * 4 + s.channel.min(3) as usize];
                        out[(y as usize * w as usize + x as usize) * 4 + slot] = if s.invert { 255 - v } else { v };
                    }
                }
            }
            Some(self.push(Texture::from_rgba8(w, h, out, ColorSpace::Linear)))
        })();
        self.by_key.insert(key, result);
        result
    }

    /// Copies a separate opacity mask into the base colour texture's alpha,
    /// since OBJ's `map_d` and FBX's transparency maps keep it apart while
    /// glTF's material model reads alpha from the base colour.  The mask's
    /// alpha is used when it has one, otherwise its red channel.
    pub fn merge_alpha(&mut self, base: usize, mask: usize) -> Option<usize> {
        if base == mask {
            return Some(base);
        }
        let key = format!("alpha:{base}:{mask}");
        if let Some(&cached) = self.by_key.get(&key) {
            return cached;
        }
        let result = (|| {
            let base_tex = self.textures.get(base)?;
            let mask_tex = self.textures.get(mask)?;
            let channel = if mask_tex.has_alpha { 3 } else { 0 };
            let (w, h, px) = rgba8(base_tex)?;
            let (mw, mh, mpx) = rgba8(mask_tex)?;
            let mut out = px.into_owned();
            for y in 0..h as usize {
                let my = y * mh as usize / h as usize;
                for x in 0..w as usize {
                    let mx = x * mw as usize / w as usize;
                    out[(y * w as usize + x) * 4 + 3] = mpx[(my * mw as usize + mx) * 4 + channel];
                }
            }
            let color_space = base_tex.color_space;
            Some(self.push(Texture::from_rgba8(w, h, out, color_space)))
        })();
        self.by_key.insert(key, result);
        result
    }

    pub fn get(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index).map(|t| &**t)
    }
}

//endregion

/// Fails with `TooLarge` once a running triangle count passes the limit, so
/// loaders stop before building geometry nobody will draw.
pub fn check_triangles(count: u64, limits: &Limits) -> Result<(), LoadError> {
    if count > limits.max_triangles {
        Err(LoadError::TooLarge(format!("more than {} triangles", limits.max_triangles)))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numbers_like_strtod() {
        for s in [
            "0",
            "-0",
            "1",
            "-1.5",
            "3.14159",
            "1e3",
            "1.5E-3",
            "-2.5e+2",
            ".5",
            "5.",
            "123456789012345678901234",
            "1e-40",
            "0.000001",
        ] {
            let fast = parse_f32(s.as_bytes()).unwrap();
            let slow: f32 = s.parse().unwrap();
            assert!((fast - slow).abs() <= slow.abs() * 1e-6, "{s}: {fast} vs {slow}");
        }
        assert_eq!(parse_f32(b"nan").map(|v| v.is_nan()), Some(true));
        assert_eq!(parse_f32(b""), None);
        assert_eq!(parse_f32(b"-"), None);
        assert_eq!(parse_f32(b"1.2.3"), None);
        assert_eq!(parse_f32(b"1e"), None);
        assert_eq!(parse_f32(b"1e999999999999"), Some(f32::INFINITY));
        assert_eq!(parse_i64(b"-12"), Some(-12));
        assert_eq!(parse_i64(b"99999999999999999999"), None);
    }

    #[test]
    fn triangulates_convex_and_concave() {
        let mut out = Vec::new();
        let square = [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
        triangulate(&square, &mut out);
        assert_eq!(out.len(), 2);
        // An L shape: six corners, one reflex.
        let l = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0, 2.0, 0.0),
            Vec3::new(0.0, 2.0, 0.0),
        ];
        out.clear();
        triangulate(&l, &mut out);
        assert_eq!(out.len(), 4);
        let area: f32 = out
            .iter()
            .map(|t| (l[t[1] as usize] - l[t[0] as usize]).cross(l[t[2] as usize] - l[t[0] as usize]).z * 0.5)
            .sum();
        assert!((area - 3.0).abs() < 1e-5, "area {area}");
        // A square with a square hole, described as one keyhole polygon
        // whose bridge repeats two corners.
        let keyhole = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(4.0, 4.0, 0.0),
            Vec3::new(0.0, 4.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0, 3.0, 0.0),
            Vec3::new(3.0, 3.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
        ];
        out.clear();
        triangulate(&keyhole, &mut out);
        let area: f32 = out
            .iter()
            .map(|t| {
                (keyhole[t[1] as usize] - keyhole[t[0] as usize])
                    .cross(keyhole[t[2] as usize] - keyhole[t[0] as usize])
                    .z
                    * 0.5
            })
            .map(f32::abs)
            .sum();
        assert!((area - 12.0).abs() < 1e-4, "keyhole area {area}");
        // Degenerate input still terminates.
        out.clear();
        triangulate(&[Vec3::ZERO; 7], &mut out);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn flips_uv_transforms() {
        assert!(flipped_uv_transform(Mat3::IDENTITY).is_none());
        // Scaling by 2 around the origin in v-up space keeps (0, 0) fixed,
        // which is (0, 1) in v-down space.
        let t = flipped_uv_transform(Mat3::from_scale(Vec2::splat(2.0))).unwrap();
        let p = t.matrix().transform_point2(Vec2::new(0.0, 1.0));
        assert!((p - Vec2::new(0.0, 1.0)).length() < 1e-5, "{p}");
        let p = t.matrix().transform_point2(Vec2::new(0.5, 0.5));
        assert!((p - Vec2::new(1.0, 0.0)).length() < 1e-5, "{p}");
    }

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("C:\\tex\\a%20b.png"), "C:/tex/a b.png");
        assert_eq!(clean_name("file:///C:/x.png"), "C:/x.png");
        assert_eq!(clean_name("\"a.png\" "), "a.png");
        assert_eq!(percent_decode("bad%2"), "bad%2");
    }
}
