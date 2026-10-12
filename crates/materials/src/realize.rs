//! Texture slots as material files describe them, and turning those slots
//! into decoded textures laid out the way `slerp_scene::Material`
//! expects (glTF's: roughness in green and metallic in blue of one texture,
//! occlusion and height in red, opacity in the base colour's alpha).
//!
//! Material files name one file per property and say which channel to read,
//! so a slot whose channel isn't where glTF looks gets a small repacked
//! texture.  Textures decode in parallel, and a texture that can't be read
//! is a warning: the material keeps its constant value, or the fallback the
//! file gives for that texture.

use rayon::prelude::*;
use slerp_archive::Archive;
use slerp_scene::glam::{Mat3, Vec2, Vec3};
use slerp_scene::textures::{self, DecodeOptions, Original, Pixels};
use slerp_scene::{Asset, ColorSpace, Info, Limits, Material, MaterialAsset, Sampler, TexRef, Texture, UvTransform};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/*┌─────────────────╮
| Slots           |
└─────────────────╯*/
//region Slots

/// Where a texture file's bytes are.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FileRef {
    Disk(PathBuf),
    /// An entry of the package being loaded (a USDZ), by its index in the
    /// package's directory.
    Embedded {
        name: String,
        index: usize,
    },
}

impl FileRef {
    pub fn display(&self) -> String {
        match self {
            FileRef::Disk(p) => p.display().to_string(),
            FileRef::Embedded { name, .. } => name.clone(),
        }
    }
}

/// Which part of a texture a slot reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Channel {
    R,
    G,
    B,
    A,
    /// Red, green and blue together: colours and normals.
    Rgb,
    /// Rec. 709 luminance of the colour, Godot's "grayscale" channel.
    Luminance,
}

impl Channel {
    /// Index into an RGBA factor or fallback for a scalar slot.
    pub fn index(self) -> usize {
        match self {
            Channel::G => 1,
            Channel::B => 2,
            Channel::A => 3,
            _ => 0,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Channel::R => "R",
            Channel::G => "G",
            Channel::B => "B",
            Channel::A => "A",
            Channel::Rgb => "RGB",
            Channel::Luminance => "grey",
        }
    }
}

/// One texture as a material slot uses it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TexInput {
    pub file: FileRef,
    /// The path as the material file wrote it, for messages and details.
    pub label: String,
    pub color_space: ColorSpace,
    pub channel: Channel,
    pub transform: Option<UvTransform>,
    pub sampler: Sampler,
    pub uv_set: u8,
    /// The texture's value when its file can't be read, when the material
    /// file says (USD's `fallback`, MaterialX's `default`).
    pub fallback: Option<[f32; 4]>,
}

impl TexInput {
    pub fn new(file: FileRef, label: String, channel: Channel) -> Self {
        TexInput {
            file,
            label,
            color_space: ColorSpace::Unspecified,
            channel,
            transform: None,
            sampler: Sampler::default(),
            uv_set: 0,
            fallback: None,
        }
    }
}

/// A material as read from a file: constant values in `material` (whose
/// texture slots are all empty) plus the texture each property reads.
/// Factors in `material` multiply the textures, as in glTF.
#[derive(Clone, Debug)]
pub(crate) struct MatDesc {
    pub material: Material,
    pub base_color: Option<TexInput>,
    /// Opacity read from its own texture (or the base colour's alpha).
    pub opacity: Option<TexInput>,
    pub metallic: Option<TexInput>,
    pub roughness: Option<TexInput>,
    pub normal: Option<TexInput>,
    pub occlusion: Option<TexInput>,
    pub emissive: Option<TexInput>,
    pub height: Option<TexInput>,
    /// The shading model the file used, such as "open_pbr_surface".
    pub model: String,
}

impl MatDesc {
    pub fn new(name: &str, model: &str) -> Self {
        MatDesc {
            material: Material { name: name.to_string(), ..Default::default() },
            base_color: None,
            opacity: None,
            metallic: None,
            roughness: None,
            normal: None,
            occlusion: None,
            emissive: None,
            height: None,
            model: model.to_string(),
        }
    }

    /// Every texture slot with its name, filled or not.
    pub fn slots(&self) -> [(&'static str, Option<&TexInput>); 8] {
        [
            ("Base colour", self.base_color.as_ref()),
            ("Opacity", self.opacity.as_ref()),
            ("Metallic", self.metallic.as_ref()),
            ("Roughness", self.roughness.as_ref()),
            ("Normal", self.normal.as_ref()),
            ("Occlusion", self.occlusion.as_ref()),
            ("Emissive", self.emissive.as_ref()),
            ("Height", self.height.as_ref()),
        ]
    }

    /// The shading model and texture list, for `Info::details`.
    pub fn details(&self) -> Vec<(String, String)> {
        let mut out = vec![("Shading model".to_string(), self.model.clone())];
        for (slot, tex) in self.slots() {
            if let Some(t) = tex {
                let label = match t.channel {
                    Channel::Rgb => t.label.clone(),
                    ch => format!("{} ({})", t.label, ch.name()),
                };
                out.push((slot.to_string(), label));
            }
        }
        out
    }

    /// Paths of the texture files on disk, so a preview search doesn't pick
    /// one of them.
    pub fn disk_files(&self) -> Vec<PathBuf> {
        self.slots()
            .iter()
            .filter_map(|(_, t)| match t.map(|t| &t.file) {
                Some(FileRef::Disk(p)) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Converts a texture-coordinate transform written for OpenGL's convention
/// (MaterialX, USD: v grows upwards from the bottom-left of the image) into
/// glTF's (v grows downwards from the top-left), and drops identities.
pub(crate) fn uv_transform_from_gl(m: Mat3) -> Option<UvTransform> {
    let flip = Mat3::from_cols(Vec3::X, Vec3::NEG_Y, Vec3::new(0.0, 1.0, 1.0));
    let g = flip * m * flip;
    if g.abs_diff_eq(Mat3::IDENTITY, 1e-6) {
        return None;
    }
    let col0 = g.x_axis.truncate();
    let col1 = g.y_axis.truncate();
    let scale_x = col0.length();
    // UvTransform::matrix rotates by -rotation, so the angle of the first
    // column is -rotation.
    let angle = col0.y.atan2(col0.x);
    let scale_y = col1.dot(Vec2::new(-angle.sin(), angle.cos()));
    let t = UvTransform { offset: g.z_axis.truncate(), rotation: -angle, scale: Vec2::new(scale_x, scale_y) };
    let finite = t.offset.is_finite() && t.rotation.is_finite() && t.scale.is_finite();
    finite.then_some(t)
}

//endregion

/*┌─────────────────╮
| Decoding        |
└─────────────────╯*/
//region Decoding

/// Decoded materials and the textures their slots index.
pub(crate) struct Realized {
    pub materials: Vec<Material>,
    pub textures: Vec<Arc<Texture>>,
}

/// Decodes every texture the materials use and builds their final form.
/// With `skip_textures` the materials keep only their constant values, for
/// thumbnails that show a preview picture instead.
pub(crate) fn realize(
    descs: &[&MatDesc],
    package: Option<&Archive>,
    limits: &Limits,
    skip_textures: bool,
    warnings: &mut Vec<String>,
) -> Realized {
    // 1. Collect the distinct files (and the colour space each is read as).
    let mut keys: Vec<(FileRef, ColorSpace)> = Vec::new();
    let mut key_index: HashMap<(FileRef, u8), usize> = HashMap::new();
    if !skip_textures {
        for desc in descs {
            for (_, tex) in desc.slots() {
                if let Some(t) = tex {
                    key_index.entry((t.file.clone(), cs_key(t.color_space))).or_insert_with(|| {
                        keys.push((t.file.clone(), t.color_space));
                        keys.len() - 1
                    });
                }
            }
        }
    }

    // 2. Decode them in parallel, noting the files here first, since the
    //    threads decoding them don't collect neighbours.
    for (file, _) in &keys {
        if let FileRef::Disk(path) = file {
            slerp_scene::record_neighbour(path);
        }
    }
    let decoded: Vec<Result<Texture, String>> = keys
        .par_iter()
        .map(|(file, cs)| {
            let mut tex = decode(file, package, limits)?;
            if tex.color_space == ColorSpace::Unspecified {
                tex.color_space = *cs;
            }
            Ok(tex)
        })
        .collect();
    for ((file, _), result) in keys.iter().zip(&decoded) {
        if let Err(e) = result {
            warnings.push(format!("texture {}: {e}", file.display()));
        }
    }

    // 3. Fill in each material's slots, repacking channels where needed.
    let mut asm = Assembler { decoded, direct: HashMap::new(), packed: HashMap::new(), out: Vec::new() };
    let lookup = |t: &TexInput| key_index.get(&(t.file.clone(), cs_key(t.color_space))).copied();
    let materials = descs.iter().map(|d| asm.material(d, &lookup)).collect();
    Realized { materials, textures: asm.out }
}

/// `ColorSpace` isn't hashable, so texture keys use this instead.
fn cs_key(c: ColorSpace) -> u8 {
    match c {
        ColorSpace::Srgb => 0,
        ColorSpace::Linear => 1,
        ColorSpace::Unspecified => 2,
    }
}

/// Builds a material asset from a material file's description: its preview
/// picture (`embedded_preview`, which the file itself names, or else one
/// found next to `path`), then its textures, which a thumbnail-sized load
/// skips when it has a preview.
pub(crate) fn material_asset(
    path: &Path,
    package: Option<&Archive>,
    desc: MatDesc,
    mut info: Info,
    embedded_preview: Option<(FileRef, String)>,
    limits: &Limits,
) -> Asset {
    let mut preview_file = embedded_preview;
    if preview_file.is_none()
        && limits.neighbours
        && let Some(p) = crate::preview::find_neighbour(path, &desc.disk_files())
    {
        let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        preview_file = Some((FileRef::Disk(p), label));
    }
    let mut preview = None;
    if let Some((file, label)) = preview_file {
        match decode(&file, package, limits) {
            Ok(t) => {
                info.details.push(("Preview".into(), label));
                preview = Some(Arc::new(t));
            }
            Err(e) => info.warnings.push(format!("preview {label}: {e}")),
        }
    }
    let skip = preview.is_some() && limits.max_texture_size.is_some();
    let realized = realize(&[&desc], package, limits, skip, &mut info.warnings);
    info.details.extend(desc.details());
    let material = realized.materials.into_iter().next().unwrap_or_default();
    let name = desc.material.name.clone();
    Asset::Material(MaterialAsset { name, material, textures: realized.textures, preview, info })
}

/// Decodes one file, catching panics so a decoder bug costs one texture
/// rather than the thumbnail.
pub(crate) fn decode(file: &FileRef, package: Option<&Archive>, limits: &Limits) -> Result<Texture, String> {
    match file {
        FileRef::Disk(path) => {
            if !limits.neighbours {
                return Err("not read: neighbouring files are off".into());
            }
            slerp_scene::record_neighbour(path);
            if !path.is_file() {
                return Err("missing".into());
            }
            let opts = decode_options(limits);
            let mut tex = catch(|| textures::decode_file(path, &opts).map_err(|e| e.to_string()))?;
            // Where the encoded picture is, so it can be saved unchanged.
            tex.source.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Ok(meta) = std::fs::metadata(path) {
                tex.source.original = Some(Original::File { path: path.clone(), offset: 0, len: meta.len() });
            }
            Ok(tex)
        }
        FileRef::Embedded { index, .. } => {
            let package = package.ok_or("not in a package")?;
            let entry = package.entry(*index).ok_or("entry outside the package")?;
            if entry.size > limits.max_file_bytes.min(1 << 31) {
                return Err("entry too large".into());
            }
            let data = package.read(entry).map_err(|e| e.to_string())?;
            let mut tex = decode_bytes(&data, limits)?;
            tex.source.name = entry.name().to_string();
            if limits.originals {
                tex.source.original = Some(Original::Memory(data.into()));
            }
            Ok(tex)
        }
    }
}

/// Decodes an image held in memory.
pub(crate) fn decode_bytes(data: &[u8], limits: &Limits) -> Result<Texture, String> {
    let opts = decode_options(limits);
    catch(|| textures::decode(data, &opts).map_err(|e| e.to_string()))
}

fn decode_options(limits: &Limits) -> DecodeOptions {
    match limits.max_texture_size {
        Some(n) => DecodeOptions::preview(n),
        None => DecodeOptions::full(),
    }
}

fn catch(f: impl FnOnce() -> Result<Texture, String>) -> Result<Texture, String> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err("the decoder failed".into()))
}

//endregion

/*┌─────────────────╮
| Assembly        |
└─────────────────╯*/
//region Assembly

/// A source for one channel of a repacked texture: a channel of a decoded
/// texture, or a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    Tex(usize, Channel),
    Const(u8),
}

struct Assembler {
    decoded: Vec<Result<Texture, String>>,
    /// Decoded index to output index, for textures used as they are.
    direct: HashMap<usize, usize>,
    packed: HashMap<[Part; 4], Option<usize>>,
    out: Vec<Arc<Texture>>,
}

impl Assembler {
    fn ok(&self, i: Option<usize>) -> Option<usize> {
        i.filter(|&i| self.decoded.get(i).is_some_and(|r| r.is_ok()))
    }

    fn use_direct(&mut self, i: usize) -> Option<usize> {
        if let Some(&o) = self.direct.get(&i) {
            return Some(o);
        }
        let tex = self.decoded.get(i)?.as_ref().ok()?.clone();
        self.out.push(Arc::new(tex));
        let o = self.out.len() - 1;
        self.direct.insert(i, o);
        Some(o)
    }

    fn use_packed(&mut self, parts: [Part; 4], color_space: ColorSpace) -> Option<usize> {
        if let Some(&o) = self.packed.get(&parts) {
            return o;
        }
        let tex = pack(&self.decoded, parts, color_space);
        let o = tex.map(|t| {
            self.out.push(Arc::new(t));
            self.out.len() - 1
        });
        self.packed.insert(parts, o);
        o
    }

    fn material(&mut self, d: &MatDesc, lookup: &dyn Fn(&TexInput) -> Option<usize>) -> Material {
        let mut m = d.material.clone();
        let idx = |t: &Option<TexInput>| t.as_ref().and_then(lookup);
        let tref = |t: &TexInput, texture: usize| TexRef {
            texture,
            uv_set: t.uv_set,
            transform: t.transform,
            sampler: t.sampler,
        };

        // 1. Base colour, with opacity in its alpha.
        let base = self.ok(idx(&d.base_color));
        let opacity = self.ok(idx(&d.opacity));
        if let (Some(t), None) = (&d.base_color, base) {
            apply_color_fallback(&mut m.base_color, t);
        }
        if let (Some(t), None) = (&d.opacity, opacity)
            && let Some(f) = t.fallback
        {
            m.base_color.w *= f[t.channel.index()];
        }
        let base_tex = d.base_color.as_ref().filter(|_| base.is_some());
        match (base, opacity, &d.opacity, base_tex) {
            (Some(b), None, _, Some(bt)) => m.base_color_texture = self.use_direct(b).map(|o| tref(bt, o)),
            (Some(b), Some(o), Some(ot), Some(bt)) if b == o && ot.channel == Channel::A => {
                m.base_color_texture = self.use_direct(b).map(|o| tref(bt, o));
            }
            (b, Some(o), Some(ot), _) => {
                let rgb = |c| b.map_or(Part::Const(255), |b| Part::Tex(b, c));
                let parts = [rgb(Channel::R), rgb(Channel::G), rgb(Channel::B), Part::Tex(o, ot.channel)];
                let cs = d.base_color.as_ref().map_or(ColorSpace::Srgb, |t| t.color_space);
                let carrier = base_tex.unwrap_or(ot);
                if let Some(p) = self.use_packed(parts, cs) {
                    m.base_color_texture = Some(tref(carrier, p));
                }
            }
            _ => {}
        }

        // 2. Roughness in green and metallic in blue.
        let rough = self.ok(idx(&d.roughness));
        let metal = self.ok(idx(&d.metallic));
        if let (Some(t), None) = (&d.roughness, rough)
            && let Some(f) = t.fallback
        {
            m.roughness *= f[t.channel.index()];
        }
        if let (Some(t), None) = (&d.metallic, metal)
            && let Some(f) = t.fallback
        {
            m.metallic *= f[t.channel.index()];
        }
        let rough_t = d.roughness.as_ref().filter(|_| rough.is_some());
        let metal_t = d.metallic.as_ref().filter(|_| metal.is_some());
        let rough_ch = rough_t.map(|t| t.channel);
        let metal_ch = metal_t.map(|t| t.channel);
        let direct = match (rough, metal) {
            (Some(r), Some(mt)) => r == mt && rough_ch == Some(Channel::G) && metal_ch == Some(Channel::B),
            (Some(_), None) => rough_ch == Some(Channel::G) && m.metallic == 0.0,
            _ => false,
        };
        if let Some(carrier) = rough_t.or(metal_t) {
            if let (true, Some(r)) = (direct, rough) {
                m.metallic_roughness_texture = self.use_direct(r).map(|o| tref(carrier, o));
            } else {
                let g = match (rough, rough_ch) {
                    (Some(r), Some(ch)) => Part::Tex(r, ch),
                    _ => Part::Const(255),
                };
                let b = match (metal, metal_ch) {
                    (Some(mt), Some(ch)) => Part::Tex(mt, ch),
                    _ => Part::Const(255),
                };
                if let Some(p) = self.use_packed([Part::Const(255), g, b, Part::Const(255)], ColorSpace::Linear) {
                    m.metallic_roughness_texture = Some(tref(carrier, p));
                }
            }
        }

        // 3. Normal and emissive maps are used as they are.
        if let (Some(t), Some(i)) = (&d.normal, self.ok(idx(&d.normal))) {
            m.normal_texture = self.use_direct(i).map(|o| tref(t, o));
        }
        match (&d.emissive, self.ok(idx(&d.emissive))) {
            (Some(t), Some(i)) => m.emissive_texture = self.use_direct(i).map(|o| tref(t, o)),
            (Some(t), None) => {
                if let Some(f) = t.fallback {
                    m.emissive *= Vec3::new(f[0], f[1], f[2]);
                }
            }
            _ => {}
        }

        // 4. Occlusion and height read red.
        if let Some(r) = self.red_slot(&d.occlusion, idx(&d.occlusion), &tref) {
            m.occlusion_texture = Some(r);
        }
        match self.red_slot(&d.height, idx(&d.height), &tref) {
            Some(r) => m.height_texture = Some(r),
            None => {
                if d.height.is_some() {
                    // Without its map, a displacement scale would only move
                    // the whole surface.
                    m.height_scale = 0.0;
                }
            }
        }
        m
    }

    fn red_slot(
        &mut self,
        t: &Option<TexInput>,
        i: Option<usize>,
        tref: &dyn Fn(&TexInput, usize) -> TexRef,
    ) -> Option<TexRef> {
        let t = t.as_ref()?;
        let i = self.ok(i)?;
        if t.channel == Channel::R {
            return self.use_direct(i).map(|o| tref(t, o));
        }
        let parts = [Part::Tex(i, t.channel), Part::Const(0), Part::Const(0), Part::Const(255)];
        self.use_packed(parts, ColorSpace::Linear).map(|p| tref(t, p))
    }
}

fn apply_color_fallback(color: &mut slerp_scene::glam::Vec4, t: &TexInput) {
    if let Some(f) = t.fallback {
        color.x *= f[0];
        color.y *= f[1];
        color.z *= f[2];
    }
}

/// Builds an 8-bit texture whose channels come from `parts`, at the size of
/// the first texture among them.  Others are sampled at the nearest texel.
fn pack(decoded: &[Result<Texture, String>], parts: [Part; 4], color_space: ColorSpace) -> Option<Texture> {
    let first = parts.iter().find_map(|p| match p {
        Part::Tex(i, _) => decoded.get(*i)?.as_ref().ok(),
        Part::Const(_) => None,
    })?;
    let (w, h) = (first.width(), first.height());
    let len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    let mut data = vec![0u8; len];
    for (c, part) in parts.iter().enumerate() {
        match *part {
            Part::Const(v) => data[c..].iter_mut().step_by(4).for_each(|x| *x = v),
            Part::Tex(i, ch) => {
                let tex = decoded.get(i)?.as_ref().ok()?;
                let (tw, th) = (tex.width() as usize, tex.height() as usize);
                let src = tex.base();
                if src.texel_count() < tw * th || tw == 0 || th == 0 {
                    return None;
                }
                for y in 0..h as usize {
                    let sy = y * th / h as usize;
                    for x in 0..w as usize {
                        let sx = x * tw / w as usize;
                        data[(y * w as usize + x) * 4 + c] = texel(src, sy * tw + sx, ch);
                    }
                }
            }
        }
    }
    Some(Texture::from_rgba8(w, h, data, color_space))
}

fn texel(src: &Pixels, i: usize, ch: Channel) -> u8 {
    let get = |c: usize| -> f32 {
        match src {
            Pixels::Rgba8(v) => v[i * 4 + c] as f32 / 255.0,
            Pixels::Rgba32F(v) => v[i * 4 + c],
            // Material textures are read with `decode`, which never keeps
            // blocks.
            Pixels::Blocks(..) => 0.0,
        }
    };
    let value = match ch {
        Channel::R | Channel::Rgb => get(0),
        Channel::G => get(1),
        Channel::B => get(2),
        Channel::A => get(3),
        Channel::Luminance => 0.2126 * get(0) + 0.7152 * get(1) + 0.0722 * get(2),
    };
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

//endregion
