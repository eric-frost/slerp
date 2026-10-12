//! ZDoom's TEXTURES lump: composite textures written as text, the modern
//! replacement for TEXTURE1 and PNAMES.
//!
//! ```text
//! Texture "BRICK", 128, 64          // name, size in texels
//! {
//!     XScale 2.0                    // texels per map unit
//!     Patch "BRICK1", 0, 0 { FlipX Rotate 90 Alpha 0.5 Style Translucent }
//!     Graphic "textures/x.png", 64, 0
//! }
//! ```
//!
//! The kind (`Texture`, `WallTexture`, `Flat`, `Sprite`, `Graphic`) says
//! where the name may be used.  `#include` pulls in another lump, and
//! `define` makes a high-resolution picture stand for a texture of a given
//! size.

use super::script::Scanner;

/// Where a definition may be used, as GZDoom's texture types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Use {
    /// `Texture`: walls and flats alike.
    Any,
    Wall,
    Flat,
    Sprite,
    Graphic,
}

/// How a patch is drawn onto what's beneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Copy,
    Translucent,
    Add,
    Subtract,
    ReverseSubtract,
    Modulate,
    CopyAlpha,
    CopyNewAlpha,
    Overlay,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PatchDef {
    /// A lump name or a full path in an archive.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Quarter turns clockwise: 0 to 3.
    pub rotate: u8,
    pub alpha: f32,
    pub style: Style,
    /// A colour mixed over the patch, RGB 0-1 and strength (1 tints by
    /// the patch's brightness, as GZDoom's blend without an amount).
    pub blend: Option<[f32; 4]>,
    /// The patch is drawn in grey, for translations this doesn't model.
    pub desaturate: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextureDef {
    pub use_as: Use,
    /// Upper case.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Texels per map unit.
    pub x_scale: f64,
    pub y_scale: f64,
    /// Offsets apply in texture units rather than texels.
    pub world_panning: bool,
    /// An invisible texture (`NullTexture`).
    pub null: bool,
    pub patches: Vec<PatchDef>,
}

/// A `define`: picture `path` stands for a texture of `width` by `height`
/// map units, named after its file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Define {
    pub name: String,
    pub path: String,
    pub width: u32,
    pub height: u32,
}

/// Largest texture side accepted, and most patches in one texture.
const MAX_SIDE: f64 = 16384.0;
const MAX_PATCHES: usize = 4096;
/// Most nested `#include`s and definitions read.
const MAX_DEPTH: usize = 8;
const MAX_DEFS: usize = 1 << 18;

#[derive(Default)]
pub(crate) struct Parsed {
    pub textures: Vec<TextureDef>,
    pub defines: Vec<Define>,
}

/// Parses a TEXTURES lump.  `include` returns the text of a lump named by
/// `#include`.
pub(crate) fn parse(text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut Parsed) {
    parse_at(text, include, out, 0);
}

fn parse_at(text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut Parsed, depth: usize) {
    let mut sc = Scanner::new(text);
    while let Some(t) = sc.next() {
        if out.textures.len() >= MAX_DEFS {
            return;
        }
        let Some(word) = t.text().map(str::to_ascii_lowercase) else { continue };
        let use_as = match word.as_str() {
            "texture" => Use::Any,
            "walltexture" => Use::Wall,
            "flat" => Use::Flat,
            "sprite" => Use::Sprite,
            "graphic" => Use::Graphic,
            "#include" => {
                if let Some(name) = sc.text()
                    && depth < MAX_DEPTH
                    && let Some(sub) = include(&name)
                {
                    parse_at(&sub, include, out, depth + 1);
                }
                continue;
            }
            "define" => {
                let Some(path) = sc.text() else { continue };
                let mut w = sc.number();
                if w.is_none() {
                    // `force32bit` may come before the size.
                    let _ = sc.text();
                    w = sc.number();
                }
                let h = sc.number();
                if let (Some(w), Some(h)) = (w, h)
                    && w >= 1.0
                    && h >= 1.0
                {
                    let base = path.rsplit(['/', '\\']).next().unwrap_or(&path);
                    let base = base.rsplit_once('.').map_or(base, |(b, _)| b);
                    let name = base.chars().take(8).collect::<String>().to_ascii_uppercase();
                    out.defines.push(Define {
                        name,
                        path,
                        width: w.min(MAX_SIDE) as u32,
                        height: h.min(MAX_SIDE) as u32,
                    });
                }
                continue;
            }
            _ => continue,
        };
        if let Some(def) = texture(&mut sc, use_as) {
            out.textures.push(def);
        }
    }
}

/// Reads one definition after its kind: `[optional] "NAME", w, h { ... }`.
fn texture(sc: &mut Scanner, use_as: Use) -> Option<TextureDef> {
    let mut name = sc.text()?;
    if name.eq_ignore_ascii_case("optional") {
        name = sc.text()?;
    }
    sc.eat(b',');
    let w = sc.number().unwrap_or(0.0);
    sc.eat(b',');
    let h = sc.number().unwrap_or(0.0);
    let mut def = TextureDef {
        use_as,
        name: name.to_ascii_uppercase(),
        width: w.clamp(0.0, MAX_SIDE) as u32,
        height: h.clamp(0.0, MAX_SIDE) as u32,
        x_scale: 1.0,
        y_scale: 1.0,
        world_panning: false,
        null: false,
        patches: Vec::new(),
    };
    if !sc.eat(b'{') {
        return Some(def);
    }
    while let Some(t) = sc.next() {
        if t.is_sym(b'}') {
            break;
        }
        let Some(word) = t.text().map(str::to_ascii_lowercase) else { continue };
        match word.as_str() {
            "xscale" => def.x_scale = scale(sc.number()),
            "yscale" => def.y_scale = scale(sc.number()),
            "scale" => {
                let s = scale(sc.number());
                (def.x_scale, def.y_scale) = (s, s);
            }
            "worldpanning" => def.world_panning = true,
            "nulltexture" => def.null = true,
            "offset" | "offset2" => {
                let _ = sc.number();
                sc.eat(b',');
                let _ = sc.number();
            }
            "patch" | "graphic" | "sprite" => {
                if let Some(p) = patch(sc)
                    && def.patches.len() < MAX_PATCHES
                {
                    def.patches.push(p);
                }
            }
            _ => {}
        }
    }
    Some(def)
}

/// A scale: positive and finite, else 1.
fn scale(v: Option<f64>) -> f64 {
    v.filter(|v| *v > 1e-4 && *v < 1e4).unwrap_or(1.0)
}

/// Reads `"NAME", x, y [{ options }]`.
fn patch(sc: &mut Scanner) -> Option<PatchDef> {
    let name = sc.text()?;
    sc.eat(b',');
    let x = sc.number().unwrap_or(0.0);
    sc.eat(b',');
    let y = sc.number().unwrap_or(0.0);
    let mut p = PatchDef {
        name,
        x: x.clamp(-1e6, 1e6) as i32,
        y: y.clamp(-1e6, 1e6) as i32,
        flip_x: false,
        flip_y: false,
        rotate: 0,
        alpha: 1.0,
        style: Style::Copy,
        blend: None,
        desaturate: false,
    };
    if !sc.eat(b'{') {
        return Some(p);
    }
    while let Some(t) = sc.next() {
        if t.is_sym(b'}') {
            break;
        }
        let Some(word) = t.text().map(str::to_ascii_lowercase) else { continue };
        match word.as_str() {
            "flipx" => p.flip_x = !p.flip_x,
            "flipy" => p.flip_y = !p.flip_y,
            "rotate" => {
                let r = sc.number().unwrap_or(0.0) as i64;
                p.rotate = (r.rem_euclid(360) / 90) as u8 & 3;
            }
            "alpha" => p.alpha = sc.number().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
            "style" => {
                p.style = match sc.text().unwrap_or_default().to_ascii_lowercase().as_str() {
                    "translucent" => Style::Translucent,
                    "add" => Style::Add,
                    "subtract" => Style::Subtract,
                    "reversesubtract" => Style::ReverseSubtract,
                    "modulate" => Style::Modulate,
                    "copyalpha" => Style::CopyAlpha,
                    "copynewalpha" => Style::CopyNewAlpha,
                    "overlay" => Style::Overlay,
                    _ => Style::Copy,
                }
            }
            "blend" => p.blend = blend(sc),
            "translation" => {
                // Palette ranges and named translations aren't modelled;
                // "Desaturate" and "Gray" are common enough to keep.
                let first = sc.text().unwrap_or_default().to_ascii_lowercase();
                let first = first.split(',').next().unwrap_or("").trim();
                p.desaturate = matches!(first, "desaturate" | "gray" | "grey" | "ice");
                skip_list(sc);
            }
            "colormap" => skip_list(sc),
            _ => {}
        }
    }
    Some(p)
}

/// Reads `Blend "colour"[, amount]` or `Blend r, g, b[, amount]`.
fn blend(sc: &mut Scanner) -> Option<[f32; 4]> {
    let rgb = if let Some(r) = sc.number() {
        sc.eat(b',');
        let g = sc.number().unwrap_or(0.0);
        sc.eat(b',');
        let b = sc.number().unwrap_or(0.0);
        [r, g, b].map(|c| (c / 255.0).clamp(0.0, 1.0) as f32)
    } else {
        color(&sc.text()?)?
    };
    let mut amount = 1.0;
    if sc.eat(b',') {
        amount = sc.number().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    }
    (amount > 0.0).then_some([rgb[0], rgb[1], rgb[2], amount])
}

/// Skips the rest of a comma-separated list of values.
fn skip_list(sc: &mut Scanner) {
    loop {
        let Some(t) = sc.next() else { return };
        if t.is_sym(b'}') {
            sc.unget(t);
            return;
        }
        if !sc.eat(b',') {
            return;
        }
    }
}

/// A colour written as `"RR GG BB"`, `"#RRGGBB"`, `"RRGGBB"` or one of a
/// few common names, as 0-1 RGB.
pub(crate) fn color(s: &str) -> Option<[f32; 3]> {
    let t = s.trim();
    let named = match t.to_ascii_lowercase().as_str() {
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        "red" => Some([255, 0, 0]),
        "green" => Some([0, 128, 0]),
        "blue" => Some([0, 0, 255]),
        "yellow" => Some([255, 255, 0]),
        "gray" | "grey" => Some([128, 128, 128]),
        _ => None,
    };
    if let Some(c) = named {
        return Some(c.map(|v| v as f32 / 255.0));
    }
    let parts: Vec<&str> = t.split_whitespace().collect();
    let bytes: Vec<u8> = if parts.len() == 3 {
        parts.iter().filter_map(|p| u8::from_str_radix(p, 16).ok()).collect()
    } else {
        let hex = t.trim_start_matches('#');
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        (0..3).filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
    };
    (bytes.len() == 3).then(|| [bytes[0], bytes[1], bytes[2]].map(|v| v as f32 / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Parsed {
        let mut out = Parsed::default();
        let mut inc = |n: &str| (n == "more").then(|| b"Flat \"FL\", 64, 64 { Patch \"P\", 0, 0 }".to_vec());
        parse(s.as_bytes(), &mut inc, &mut out);
        out
    }

    #[test]
    fn composite_textures() {
        let p = parse_str(
            r##"// header
            Texture "Brick", 128, 64
            {
                XScale 2.0
                YScale 2
                Offset 4, 8
                Patch "BRK1", 0, 0
                Patch "textures/b/brk2.png", 64, -8 { FlipX Rotate 270 Alpha 0.5 Style Translucent }
                Graphic "G", 1, 2 { Blend "#ff0000", 0.25 Translation Desaturate, 31 }
            }
            WallTexture optional "W", 64, 64 { WorldPanning }
            Sprite "SPRA0", 3, 3 { Offset 1, 3 Patch "SPRA0", 0, 0 { Rotate 90 } }
            #include "more"
            define "hires/foo.png" 64 32
            Texture "NOBODY", 32, 32
            "##,
        );
        assert_eq!(p.textures.len(), 5);
        let t = &p.textures[0];
        assert_eq!((t.name.as_str(), t.width, t.height, t.use_as), ("BRICK", 128, 64, Use::Any));
        assert_eq!((t.x_scale, t.y_scale), (2.0, 2.0));
        assert_eq!(t.patches.len(), 3);
        let b = &t.patches[1];
        assert_eq!((b.name.as_str(), b.x, b.y, b.flip_x, b.rotate), ("textures/b/brk2.png", 64, -8, true, 3));
        assert_eq!((b.alpha, b.style), (0.5, Style::Translucent));
        let g = &t.patches[2];
        assert_eq!(g.blend, Some([1.0, 0.0, 0.0, 0.25]));
        assert!(g.desaturate);
        assert_eq!((p.textures[1].use_as, p.textures[1].world_panning), (Use::Wall, true));
        assert_eq!(p.textures[2].patches[0].rotate, 1);
        assert_eq!((p.textures[3].use_as, p.textures[3].name.as_str()), (Use::Flat, "FL"));
        assert!(p.textures[4].patches.is_empty());
        assert_eq!(p.defines, [Define { name: "FOO".into(), path: "hires/foo.png".into(), width: 64, height: 32 }]);
    }

    #[test]
    fn broken_scripts_never_panic() {
        let base = br##"Texture "A", 16, 16 { XScale 0 Patch "B", 0, 0 { Rotate 45 Blend 1, 2 } } #include"##;
        for cut in 0..base.len() {
            let mut out = Parsed::default();
            parse(&base[..cut], &mut |_| None, &mut out);
        }
        let mut out = Parsed::default();
        parse(b"Texture \"A\", 99999999, -5 { XScale nan }", &mut |_| None, &mut out);
        assert_eq!((out.textures[0].width, out.textures[0].height, out.textures[0].x_scale), (16384, 0, 1.0));
        // An include loop stops.
        let mut out = Parsed::default();
        parse(b"#include \"x\"", &mut |_| Some(b"#include \"x\" Flat \"F\", 1, 1".to_vec()), &mut out);
        assert_eq!(out.textures.len(), MAX_DEPTH);
        assert_eq!(color("12 34 56"), Some([0x12 as f32 / 255.0, 0x34 as f32 / 255.0, 0x56 as f32 / 255.0]));
        assert_eq!(color("zz"), None);
    }
}
