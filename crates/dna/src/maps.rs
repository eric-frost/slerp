//! Textures for a DNA file's meshes, from the maps folder MetaHuman
//! Creator's DCC Export writes beside it (`Maps/Head_Basecolor.png`,
//! `Head_Normal.png`, `Head_SRMF.png`, ...), or Epic's older names
//! (`head_color_map`, `head_normal_map`, `head_roughness_map`).  Without
//! maps the eyes get a painted iris, so a bare head still looks at you.

use rayon::prelude::*;
use slerp_scene::textures::{self, ColorSpace, DecodeOptions, Pixels, Texture};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// One mesh's textures, ready for glTF's material slots.
#[derive(Clone, Default)]
pub struct MapSet {
    pub color: Option<Arc<Texture>>,
    /// Green flipped from Unreal's convention to glTF's.
    pub normal: Option<Arc<Texture>>,
    /// Roughness in green and metallic in blue, as glTF reads them.
    pub roughness: Option<Arc<Texture>>,
    /// The colour map is a white mask (lashes in Epic's samples), coloured
    /// by the material.
    pub lash_tint: bool,
    /// `roughness` is an SRMF map, whose red channel is Unreal's specular
    /// mask.
    pub specular_mask: bool,
}

/// The textures found for each kind of mesh: "head", "teeth", "eyes",
/// "eyelashes", "body".
#[derive(Clone, Default)]
pub struct Maps {
    sets: HashMap<&'static str, MapSet>,
    pub warnings: Vec<String>,
    /// The files read, for `Info::neighbours`.
    pub files: Vec<PathBuf>,
}

impl Maps {
    pub fn get(&self, kind: &str) -> Option<&MapSet> {
        self.sets.get(kind)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Color,
    Normal,
    /// DCC Export's SRMF map: specular, roughness, metallic, fuzz, which
    /// already has roughness in green and metallic in blue.
    Srmf,
    /// A grey roughness map, read from red.
    RoughnessRed,
}

/// Each kind's maps as (role, names to try in order).
const WANTED: &[(&str, Role, &[&str])] = &[
    ("head", Role::Color, &["Head_Basecolor", "head_color_map"]),
    ("head", Role::Normal, &["Head_Normal", "head_normal_map"]),
    ("head", Role::Srmf, &["Head_SRMF"]),
    ("head", Role::RoughnessRed, &["head_roughness_map"]),
    ("teeth", Role::Color, &["Teeth_Color", "teeth_color_map"]),
    ("teeth", Role::Normal, &["Teeth_Normal", "teeth_normal_map"]),
    ("eyes", Role::Color, &["Eyes_Color", "eyes_color_map"]),
    ("eyes", Role::Normal, &["Eyes_Normal"]),
    ("eyelashes", Role::Color, &["Eyelashes_Color", "eyelashes_color_map"]),
    ("body", Role::Color, &["Body_Basecolor", "body_color_map"]),
    ("body", Role::Normal, &["Body_Normal", "body_normal_map"]),
    ("body", Role::Srmf, &["Body_SRMF"]),
    ("body", Role::RoughnessRed, &["body_roughness_map"]),
];

const EXTENSIONS: [&str; 5] = ["png", "tga", "jpg", "jpeg", "PNG"];

/// The maps folder beside `dna`: `Maps`, `maps` or `Textures`, else the
/// file's own folder when that holds maps.
pub fn find_folder(dna: &Path) -> Option<PathBuf> {
    let dir = dna.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    ["Maps", "maps", "Textures", "textures", ""]
        .iter()
        .map(|sub| if sub.is_empty() { dir.to_path_buf() } else { dir.join(sub) })
        .find(|d| d.is_dir() && WANTED.iter().any(|(_, _, names)| find_file(d, names).is_some()))
}

fn find_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().flat_map(|n| EXTENSIONS.iter().map(move |e| dir.join(format!("{n}.{e}")))).find(|p| p.is_file())
}

/// Decodes the maps in `folder`, in parallel, no larger than
/// `max_size` on their longer side.
pub fn load(folder: &Path, max_size: Option<u32>) -> Maps {
    let opts = match max_size {
        Some(n) => DecodeOptions::preview(n),
        None => DecodeOptions::full(),
    };
    let jobs: Vec<(&str, Role, PathBuf)> =
        WANTED.iter().filter_map(|&(kind, role, names)| Some((kind, role, find_file(folder, names)?))).collect();
    let decoded: Vec<(&str, Role, PathBuf, Result<Texture, String>)> = jobs
        .into_par_iter()
        .map(|(kind, role, path)| {
            let tex = textures::decode_file(&path, &opts).map_err(|e| e.to_string()).map(|t| prepare(t, role));
            (kind, role, path, tex)
        })
        .collect();
    let mut maps = Maps::default();
    for (kind, role, path, tex) in decoded {
        maps.files.push(path.clone());
        let tex = match tex {
            Ok(t) => t,
            Err(e) => {
                maps.warnings.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let set = maps.sets.entry(kind).or_default();
        match role {
            Role::Color => {
                set.lash_tint = kind == "eyelashes" && is_white_mask(&tex);
                set.color = Some(Arc::new(tex));
            }
            Role::Normal => set.normal = Some(Arc::new(tex)),
            // SRMF wins over a grey roughness map when both exist.
            Role::Srmf => {
                set.roughness = Some(Arc::new(tex));
                set.specular_mask = true;
            }
            Role::RoughnessRed => {
                if set.roughness.is_none() {
                    set.roughness = Some(Arc::new(tex));
                }
            }
        }
    }
    maps
}

/// Converts a decoded map to what glTF's slot for it expects.
fn prepare(mut t: Texture, role: Role) -> Texture {
    match role {
        Role::Color => t.color_space = ColorSpace::Srgb,
        Role::Normal => {
            // Unreal's normal maps point green the other way from glTF's.
            for_each_texel(&mut t, |p| p[1] = 255 - p[1], |p| p[1] = 1.0 - p[1]);
            t.color_space = ColorSpace::Linear;
        }
        Role::Srmf => {
            // Fuzz in alpha would cut the surface out if anything read it.
            for_each_texel(&mut t, |p| p[3] = 255, |p| p[3] = 1.0);
            t.color_space = ColorSpace::Linear;
            t.has_alpha = false;
        }
        Role::RoughnessRed => {
            for_each_texel(
                &mut t,
                |p| {
                    p[1] = p[0];
                    p[2] = 0;
                    p[3] = 255;
                },
                |p| {
                    p[1] = p[0];
                    p[2] = 0.0;
                    p[3] = 1.0;
                },
            );
            t.color_space = ColorSpace::Linear;
            t.has_alpha = false;
        }
    }
    t
}

fn for_each_texel(t: &mut Texture, mut f8: impl FnMut(&mut [u8]), mut f32: impl FnMut(&mut [f32])) {
    for level in &mut t.levels {
        for surface in &mut level.surfaces {
            match surface {
                Pixels::Rgba8(px) => px.chunks_exact_mut(4).for_each(&mut f8),
                Pixels::Rgba32F(px) => px.chunks_exact_mut(4).for_each(&mut f32),
                // Maps are read with `decode`, which never keeps blocks.
                Pixels::Blocks(..) => {}
            }
        }
    }
}

/// Whether a colour map is white wherever it covers, as Epic's sample
/// lash maps are (chalk's bake makes the same test).
fn is_white_mask(t: &Texture) -> bool {
    let Pixels::Rgba8(px) = t.base() else { return false };
    let (mut sum, mut weight) = (0f64, 0f64);
    for p in px.chunks_exact(4) {
        let a = p[3] as f64;
        sum += (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0 * a;
        weight += a;
    }
    weight > 0.0 && sum / weight > 180.0
}

/// An eye colour map painted from scratch, for heads without maps: a
/// white with a brown iris and a black pupil, placed where MetaHuman eye
/// maps put them (the iris a circle of radius 0.152 around the middle of
/// the texture, as chalk's eye shader also assumes).
pub fn painted_eye() -> Arc<Texture> {
    static EYE: OnceLock<Arc<Texture>> = OnceLock::new();
    EYE.get_or_init(|| {
        const N: u32 = 128;
        let mut px = Vec::with_capacity((N * N * 4) as usize);
        for y in 0..N {
            for x in 0..N {
                let (u, v) = ((x as f32 + 0.5) / N as f32 - 0.5, (y as f32 + 0.5) / N as f32 - 0.5);
                let r = (u * u + v * v).sqrt();
                let smooth = |edge: f32, width: f32| ((r - edge) / width + 0.5).clamp(0.0, 1.0);
                let sclera = [222.0, 214.0, 206.0];
                let iris = [92.0 - 40.0 * (r / 0.152), 62.0 - 28.0 * (r / 0.152), 40.0 - 18.0 * (r / 0.152)];
                let pupil = [8.0, 6.0, 6.0];
                let mut c = [0f32; 3];
                let (to_sclera, to_iris) = (smooth(0.152, 0.012), smooth(0.055, 0.008));
                for k in 0..3 {
                    let inner = pupil[k] + (iris[k] - pupil[k]) * to_iris;
                    c[k] = inner + (sclera[k] - inner) * to_sclera;
                }
                px.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
            }
        }
        Arc::new(Texture::from_rgba8(N, N, px, ColorSpace::Srgb))
    })
    .clone()
}
