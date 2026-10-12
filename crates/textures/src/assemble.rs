//! The steps every texture container shares: choosing which mip levels and
//! layers to decode, putting rows in top-to-bottom order, and building the
//! `Texture`.

use crate::util::mip_dim;
use crate::{ColorSpace, DecodeOptions, Level, Pixels, SourceInfo, Texture, TextureError, TextureKind};

/// What a container's header says about its texture.
#[derive(Clone)]
pub(crate) struct Layout {
    pub kind: TextureKind,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    /// Array layers, at least 1.
    pub layers: u32,
    /// 6 for cubemaps, otherwise 1.
    pub faces: u32,
    /// Mip levels stored, at least 1.
    pub levels: u32,
    pub color_space: ColorSpace,
    /// Rows are stored bottom row first, or texels right to left.
    pub flip_y: bool,
    pub flip_x: bool,
    /// Whether the format can hold alpha at all; if so, decoded texels are
    /// checked.
    pub alpha: bool,
    pub source: SourceInfo,
}

impl Layout {
    pub fn level_size(&self, level: u32) -> (u32, u32, u32) {
        let d = if self.kind == TextureKind::D3 { mip_dim(self.depth, level) } else { 1 };
        (mip_dim(self.width, level), mip_dim(self.height, level), d)
    }
}

/// The stored levels to decode: the smallest whose longer side is at least
/// `min_size` (or `max_size` when only that is set), and with `all_mips`
/// every smaller one after it.
pub(crate) fn pick_levels(layout: &Layout, opts: &DecodeOptions) -> std::ops::Range<u32> {
    let long = |l: u32| {
        let (w, h, _) = layout.level_size(l);
        w.max(h)
    };
    let base = match opts.min_size.or(opts.max_size) {
        Some(t) => (0..layout.levels).rev().find(|&l| long(l) >= t).unwrap_or(0),
        None => 0,
    };
    if opts.all_mips { base..layout.levels } else { base..base + 1 }
}

/// Decodes the chosen levels with `decode(level, layers)`, which returns one
/// surface per layer and face in the order layer 0 faces, layer 1 faces, and
/// so on.
pub(crate) fn assemble(
    layout: Layout,
    opts: &DecodeOptions,
    mut decode: impl FnMut(u32, u32) -> Result<Vec<Pixels>, TextureError>,
) -> Result<Texture, TextureError> {
    let layers = if opts.first_layer_only { 1 } else { layout.layers };
    let mut levels = Vec::new();
    for l in pick_levels(&layout, opts) {
        let (w, h, d) = layout.level_size(l);
        let mut surfaces = decode(l, layers)?;
        if surfaces.len() != (layers * layout.faces) as usize {
            return Err(crate::util::invalid("wrong number of surfaces"));
        }
        if layout.flip_y || layout.flip_x {
            for s in &mut surfaces {
                flip(s, w, h, d, layout.flip_x, layout.flip_y);
            }
        }
        levels.push(Level { width: w, height: h, depth: d, surfaces });
    }
    let has_alpha = layout.alpha && levels[0].surfaces.iter().any(any_alpha);
    let mut source = layout.source;
    source.width = layout.width;
    source.height = layout.height;
    source.depth = layout.depth;
    source.layers = layout.layers;
    source.faces = layout.faces;
    source.mip_count = layout.levels;
    Ok(Texture {
        kind: layout.kind,
        layers,
        faces: layout.faces,
        levels,
        color_space: layout.color_space,
        has_alpha,
        source,
    })
}

fn any_alpha(p: &Pixels) -> bool {
    match p {
        Pixels::Rgba8(v) => v.chunks_exact(4).any(|t| t[3] != 255),
        Pixels::Rgba32F(v) => v.chunks_exact(4).any(|t| t[3] < 1.0),
        // Not worth decoding to find out: the format's say stands.
        Pixels::Blocks(..) => true,
    }
}

/// Mirrors each slice of a surface vertically and/or horizontally.
pub(crate) fn flip(p: &mut Pixels, w: u32, h: u32, d: u32, x: bool, y: bool) {
    fn go<T: Copy>(v: &mut [T], w: usize, h: usize, d: usize, fx: bool, fy: bool) {
        let row = w * 4;
        for slice in v.chunks_exact_mut(row * h).take(d) {
            if fy {
                for r in 0..h / 2 {
                    let (top, bottom) = slice.split_at_mut((h - 1 - r) * row);
                    top[r * row..(r + 1) * row].swap_with_slice(&mut bottom[..row]);
                }
            }
            if fx {
                for line in slice.chunks_exact_mut(row) {
                    for i in 0..w / 2 {
                        for c in 0..4 {
                            line.swap(i * 4 + c, (w - 1 - i) * 4 + c);
                        }
                    }
                }
            }
        }
    }
    match p {
        Pixels::Rgba8(v) => go(v, w as usize, h as usize, d as usize, x, y),
        Pixels::Rgba32F(v) => go(v, w as usize, h as usize, d as usize, x, y),
        // Callers keep blocks only for textures stored top row first.
        Pixels::Blocks(..) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(w: u32, h: u32, levels: u32) -> Layout {
        Layout {
            kind: TextureKind::D2,
            width: w,
            height: h,
            depth: 1,
            layers: 1,
            faces: 1,
            levels,
            color_space: ColorSpace::Linear,
            flip_y: false,
            flip_x: false,
            alpha: false,
            source: SourceInfo::default(),
        }
    }

    #[test]
    fn level_choice() {
        let l = layout(2048, 1024, 12);
        assert_eq!(pick_levels(&l, &DecodeOptions::preview(256)), 3..4);
        assert_eq!(pick_levels(&l, &DecodeOptions::preview(300)), 2..3);
        assert_eq!(pick_levels(&l, &DecodeOptions::full()), 0..12);
        // No level is big enough: use the largest.
        assert_eq!(pick_levels(&layout(100, 100, 3), &DecodeOptions::preview(256)), 0..1);
        let only_max = DecodeOptions { min_size: None, max_size: Some(512), all_mips: false, first_layer_only: true };
        assert_eq!(pick_levels(&l, &only_max), 2..3);
    }

    #[test]
    fn flips() {
        let mut p = Pixels::Rgba8((0..2 * 3 * 4).map(|i| (i / 4) as u8).collect());
        flip(&mut p, 2, 3, 1, false, true);
        let Pixels::Rgba8(v) = &p else { panic!() };
        let firsts: Vec<u8> = v.chunks(4).map(|t| t[0]).collect();
        assert_eq!(firsts, vec![4, 5, 2, 3, 0, 1]);
        flip(&mut p, 2, 3, 1, true, false);
        let Pixels::Rgba8(v) = &p else { panic!() };
        let firsts: Vec<u8> = v.chunks(4).map(|t| t[0]).collect();
        assert_eq!(firsts, vec![5, 4, 3, 2, 1, 0]);
    }
}
