//! How bright and what colour each surface is, and the fog it fades into.
//!
//! Two models:
//!
//! - Software light, for vanilla maps and ZDoom maps that leave the light
//!   mode to the player (GZDoom's default emulates the software renderer):
//!   Doom picks one of 32 darkening colour maps from the light level and
//!   the distance; a level here uses the one Doom picks about 4 m (128
//!   units) away, so dark rooms stay dark without going black.
//! - GZDoom's hardware light modes (MAPINFO `lightmode` 0 to 5): the light
//!   level, through the mode's curve, scales the sector's light colour,
//!   and distance darkens through fog: black fog whose density follows the
//!   light level, or the sector's coloured fog (`fadecolor`,
//!   `fogdensity`, MAPINFO's `fade` and `outsidefog`).  This follows
//!   GZDoom's `hw_lighting.cpp`.
//!
//! Colours here are display (sRGB-encoded) values, as Doom's are, and
//! become linear vertex colours at the end; fog colours become linear too.

use super::map::{MapFormat, Sector, SideDef};
use super::mapinfo::MapInfo;
use crate::UNIT;
use slerp_scene::glam::Vec3;

/// A surface's distance fog: its colour (linear RGB) and density per
/// metre, for `exp(-density * distance)`, and how much brighter it is
/// near the viewer (GZDoom's Doom light mode): `near_light` times at the
/// camera, fading to none at `near_distance` metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fog {
    pub color: Vec3,
    pub density: f32,
    pub near_light: f32,
    pub near_distance: f32,
}

impl Fog {
    /// A key for grouping surfaces by fog: colour, density and
    /// brightening, rounded.
    pub fn key(&self) -> (u32, u32, u32) {
        let c = (self.color * 255.0).round().clamp(Vec3::ZERO, Vec3::splat(255.0));
        (
            (c.x as u32) << 16 | (c.y as u32) << 8 | c.z as u32,
            (self.density * 1e4).round() as u32,
            ((self.near_light * 100.0).round() as u32) << 16 | (self.near_distance * 10.0).round() as u32,
        )
    }
}

/// A sector's colours: light colour and fog colour (display RGB 0-1) and
/// its own fog density (0 when it names none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Colormap {
    pub light: [f32; 3],
    pub fade: [f32; 3],
    pub density: f64,
}

pub(crate) struct Lighting {
    /// GZDoom's light mode, or `None` for software light.
    mode: Option<i32>,
    fade: [f32; 3],
    outside_fog: Option<[f32; 3]>,
    fog_density: f64,
    outside_fog_density: f64,
    no_light_fade: bool,
    /// Light added to walls running north-south and east-west.
    wall_vert: i32,
    wall_horiz: i32,
    smooth: bool,
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f32 / 255.0)
}

/// sRGB-encoded to linear, as Slerp's vertex colours multiply linear
/// texture values.
pub(crate) fn linear(c: f32) -> f32 {
    c.max(0.0).powf(2.2)
}

/// GZDoom's distance fog density for black fog by light level
/// (`distfogtable`, with the default `gl_distfog` of 70): the first row
/// for light mode 0, the second for the others.
fn dist_fog(standard: bool, light: i32) -> f64 {
    let i = light.clamp(0, 255) as f64;
    let d = 70.0;
    if standard {
        if i < 164.0 {
            d / 2.0 + d * (164.0 - i) / 164.0
        } else if i < 230.0 {
            d / 2.0 - (d / 2.0) * (i - 164.0) / (230.0 - 164.0)
        } else {
            0.0
        }
    } else if i < 128.0 {
        6.0 + d / 2.0 + d * (128.0 - i) / 48.0
    } else if i < 216.0 {
        (216.0 - i) / (216.0 - 128.0) * d / 10.0
    } else {
        0.0
    }
}

impl Lighting {
    pub fn new(format: &MapFormat, info: Option<&MapInfo>) -> Lighting {
        let mode = info.and_then(|i| i.light_mode).filter(|m| (0..=5).contains(m));
        let even = info.is_some_and(|i| i.even_lighting);
        Lighting {
            mode,
            fade: info.and_then(|i| i.fade).unwrap_or([0.0; 3]),
            outside_fog: info.and_then(|i| i.outside_fog),
            fog_density: info.and_then(|i| i.fog_density).unwrap_or(0.0),
            outside_fog_density: info.and_then(|i| i.outside_fog_density).unwrap_or(0.0),
            no_light_fade: info.is_some_and(|i| i.no_light_fade),
            wall_vert: if even { 0 } else { info.and_then(|i| i.wall_vert_light).unwrap_or(16) },
            wall_horiz: if even { 0 } else { info.and_then(|i| i.wall_horiz_light).unwrap_or(-16) },
            smooth: info.is_some_and(|i| i.smooth_lighting) && format.zdoom(),
        }
    }

    /// The light mode's name, for the inspector.
    pub fn describe(&self) -> String {
        match self.mode {
            None => "software (colour maps by distance, approximated)".into(),
            Some(0) => "0, standard".into(),
            Some(1) => "1, bright".into(),
            Some(2) => "2, Doom".into(),
            Some(3) => "3, dark".into(),
            Some(m) => format!("{m}"),
        }
    }

    /// A sector's colours, as GZDoom's UDMF loader sets them: white light
    /// and the level's fog colour (its outside fog under the sky) unless
    /// the sector names its own.
    pub fn colormap(&self, s: &Sector, sky: bool) -> Colormap {
        let level_fade = match self.outside_fog {
            Some(c) if sky => c,
            _ => self.fade,
        };
        Colormap {
            light: s.light_color.map_or([1.0; 3], rgb),
            fade: s.fade_color.map_or(level_fade, rgb),
            density: s.fog_density.map_or(0.0, |d| (d / 2.0).floor()),
        }
    }

    /// Fake contrast for a wall from `a` to `z`: walls running east-west
    /// are darker, north-south brighter, or by angle with smooth
    /// lighting.  None in coloured fog, as GZDoom does.
    pub fn wall_contrast(&self, side: &SideDef, a: [f64; 2], z: [f64; 2], foggy: bool) -> i32 {
        if side.no_fake_contrast || (foggy && self.mode.is_some()) {
            return 0;
        }
        let (dx, dy) = (z[0] - a[0], z[1] - a[1]);
        if (self.smooth || side.smooth_lighting) && dx != 0.0 {
            let t = (dy / dx).atan().abs() / std::f64::consts::FRAC_PI_2;
            (self.wall_horiz as f64 + t * (self.wall_vert - self.wall_horiz) as f64).round() as i32
        } else if dx == 0.0 {
            self.wall_vert
        } else if dy == 0.0 {
            self.wall_horiz
        } else {
            0
        }
    }

    /// Whether a sector's fog is coloured (GZDoom's "foggy").
    pub fn foggy(cm: &Colormap) -> bool {
        cm.fade.iter().any(|&c| c > 0.0)
    }

    /// The display brightness (0-1) of light level `level` plus relative
    /// light `rel`, before the light colour.
    pub fn brightness(&self, level: f64, rel: i32) -> f32 {
        let level = level.round() as i32;
        match self.mode {
            None => {
                // Doom's colour map about 128 units away.
                let lightnum = ((level + rel).clamp(0, 255)) >> 4;
                let start = (15 - lightnum.clamp(0, 15)) * 4;
                let map = (start - 10).clamp(0, 28);
                1.0 - map as f32 / 32.0
            }
            Some(mode) => {
                // `CalcLightLevel`: the Doom and dark modes darken dim
                // light along a steeper curve.
                if level <= 0 {
                    return 0.0;
                }
                let dark = matches!(mode, 2 | 3);
                let light = if dark && level < 192 {
                    if level > 100 {
                        let l = (192.0 - (192 - level) as f32 * 1.87).round() as i32;
                        if l + rel < 20 { 20 + (l + rel - 20) / 5 } else { l + rel }
                    } else {
                        (level + rel) / 5
                    }
                } else {
                    level + rel
                };
                light.clamp(1, 255) as f32 / 255.0
            }
        }
    }

    /// A vertex colour (linear RGB): brightness times light colour, plus
    /// any glow, times a surface colour.
    pub fn color(&self, brightness: f32, cm: &Colormap, glow: [f32; 3], tint: [f32; 3]) -> Vec3 {
        let c = [0, 1, 2].map(|k| linear(((cm.light[k] * brightness + glow[k]).min(1.0)) * tint[k]));
        Vec3::from_array(c)
    }

    /// The fog a surface of light level `level` in colours `cm` fades
    /// into: GZDoom's `GetFogDensity`, converted to metres.
    pub fn fog(&self, level: f64, cm: &Colormap) -> Option<Fog> {
        let level = level.round() as i32;
        let black = !Self::foggy(cm);
        let density = match self.mode {
            Some(4) => {
                if self.fog_density != 0.0 {
                    self.fog_density
                } else {
                    18.0
                }
            }
            _ if cm.density != 0.0 => cm.density,
            None if black => 0.0,
            Some(m) if black => {
                if self.no_light_fade {
                    0.0
                } else {
                    dist_fog(m == 0, level)
                }
            }
            _ if self.outside_fog_density != 0.0 && self.outside_fog == Some(cm.fade) => self.outside_fog_density,
            _ if self.fog_density != 0.0 => self.fog_density,
            _ if level < 248 => (255 - level).clamp(30, 255) as f64,
            _ => 0.0,
        };
        if density <= 0.0 {
            return None;
        }
        // GZDoom: factor = 2^(density * -log2(e) / 64000 * distance), with
        // distance in map units.
        let per_metre = density / 64000.0 / UNIT as f64;
        let (near_light, near_distance) = match self.mode {
            Some(2) if black => near_light(self.brightness(level as f64, 0) * 255.0, level as f32),
            _ => (1.0, 0.0),
        };
        Some(Fog {
            color: Vec3::from_array(cm.fade.map(linear)),
            density: per_metre as f32,
            near_light,
            near_distance: near_distance * UNIT,
        })
    }
}

/// GZDoom's `SetShaderLight`: in the Doom light mode, surfaces near the
/// viewer are brighter, by this factor at the viewer, fading out over this
/// many map units.  `level` is the darkened light (`CalcLightLevel`),
/// `original` the sector's.
fn near_light(level: f32, original: f32) -> (f32, f32) {
    const MAXDIST: f32 = 256.0;
    const THRESHOLD: f32 = 96.0;
    const FACTOR: f32 = 0.75;
    if level <= 0.0 {
        return (1.0, 0.0);
    }
    let (dist, original) = if original < THRESHOLD {
        (MAXDIST / 2.0 + original * MAXDIST / THRESHOLD / 2.0, THRESHOLD)
    } else {
        (MAXDIST, original)
    };
    let factor = 1.0 + (original / level - 1.0) * FACTOR;
    if factor == 1.0 { (1.0, 0.0) } else { (factor, dist) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(mode: Option<i32>) -> MapInfo {
        MapInfo { light_mode: mode, ..Default::default() }
    }

    #[test]
    fn software_light_matches_the_old_curve() {
        let l = Lighting::new(&MapFormat::Doom, None);
        assert!(l.brightness(255.0, 0) > 0.99);
        assert!(l.brightness(0.0, 0) < 0.15);
        assert!(l.brightness(128.0, -16) < l.brightness(128.0, 0));
        assert!(l.brightness(128.0, 16) > l.brightness(128.0, 0));
        // No black fog in software light.
        let cm = l.colormap(&Sector::default(), false);
        assert_eq!(l.fog(100.0, &cm), None);
    }

    #[test]
    fn hardware_modes() {
        let standard = Lighting::new(&MapFormat::Udmf("zdoom".into()), Some(&info(Some(0))));
        let doom = Lighting::new(&MapFormat::Udmf("zdoom".into()), Some(&info(Some(2))));
        assert!((standard.brightness(128.0, 0) - 128.0 / 255.0).abs() < 1e-6);
        // Doom's curve: 192 - (192 - 160) * 1.87 = 132.
        assert!((doom.brightness(160.0, 0) - 132.0 / 255.0).abs() < 1e-6);
        assert!((doom.brightness(200.0, 0) - 200.0 / 255.0).abs() < 1e-6);
        // Black fog by light level: light 192 in mode 0 is density 20.2.
        let cm = standard.colormap(&Sector::default(), false);
        let fog = standard.fog(192.0, &cm).unwrap();
        let want = (35.0 - 35.0 * 28.0 / 66.0) / 64000.0 / 0.0254;
        assert!((fog.density as f64 - want).abs() < 1e-6, "{} vs {want}", fog.density);
        assert_eq!(fog.color, Vec3::ZERO);
        assert_eq!(standard.fog(240.0, &cm), None, "bright rooms have none");
        assert_eq!((fog.near_light, fog.near_distance), (1.0, 0.0), "only the Doom mode brightens");
        // The Doom mode brightens near the viewer: light 128 is drawn as
        // 72, and 1 + (128 / 72 - 1) * 0.75 times brighter up close.
        let near = doom.fog(128.0, &doom.colormap(&Sector::default(), false)).unwrap();
        assert!((near.near_light - (1.0 + (128.0 / 72.0 - 1.0) * 0.75)).abs() < 1e-4, "{}", near.near_light);
        assert!((near.near_distance - 256.0 * 0.0254).abs() < 1e-5);
        // A sector's own coloured fog and density.
        let s = Sector {
            fade_color: Some(0x408020),
            fog_density: Some(64.0),
            light_color: Some(0xff8000),
            ..Default::default()
        };
        let cm = standard.colormap(&s, false);
        assert_eq!(cm.light, [1.0, 128.0 / 255.0, 0.0]);
        let fog = standard.fog(160.0, &cm).unwrap();
        assert!((fog.density as f64 - 32.0 / 64000.0 / 0.0254).abs() < 1e-7);
        assert!(fog.color.y > fog.color.z);
        // Coloured fog without a density follows the light level.
        let s = Sector { fade_color: Some(0x808080), ..Default::default() };
        let fog = standard.fog(200.0, &standard.colormap(&s, false)).unwrap();
        assert!((fog.density as f64 - 55.0 / 64000.0 / 0.0254).abs() < 1e-7);
        assert_eq!(fog.key(), standard.fog(200.0, &standard.colormap(&s, false)).unwrap().key());
    }

    #[test]
    fn outside_fog_and_contrast() {
        let mut i = info(Some(0));
        i.outside_fog = Some([0.5, 0.5, 1.0]);
        i.outside_fog_density = Some(40.0);
        let l = Lighting::new(&MapFormat::Udmf("zdoom".into()), Some(&i));
        let cm = l.colormap(&Sector::default(), true);
        assert_eq!(cm.fade, [0.5, 0.5, 1.0]);
        assert!((l.fog(255.0, &cm).unwrap().density as f64 - 40.0 / 64000.0 / 0.0254).abs() < 1e-7);
        let side = SideDef::default();
        assert_eq!(l.wall_contrast(&side, [0.0, 0.0], [10.0, 0.0], false), -16);
        assert_eq!(l.wall_contrast(&side, [0.0, 0.0], [0.0, 10.0], false), 16);
        assert_eq!(l.wall_contrast(&side, [0.0, 0.0], [10.0, 10.0], false), 0);
        assert_eq!(l.wall_contrast(&side, [0.0, 0.0], [0.0, 10.0], true), 0, "none in coloured fog");
        let even = Lighting::new(&MapFormat::Doom, Some(&MapInfo { even_lighting: true, ..Default::default() }));
        assert_eq!(even.wall_contrast(&side, [0.0, 0.0], [10.0, 0.0], false), 0);
    }
}
