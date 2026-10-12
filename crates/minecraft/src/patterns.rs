//! Small procedural textures, 16 by 16 like Minecraft's own, that blocks
//! wear under their colour so walls read as blocks rather than flat paint:
//! stone noise, plank boards, brick courses, cobbles, bark, log rings,
//! speckled leaves, glass frames, plant blades, ore flecks and tiles.
//! They are grey (multiplied by the block's colour) and drawn with hard
//! pixel edges.  Nothing here comes from the game.

use crate::colors::Pattern;
use slerp_scene::textures::{ColorSpace, Texture};

pub const SIZE: u32 = 16;

/// A repeatable pseudo-random value from 0 to 1 for a pixel and a seed.
fn hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x27d4_eb2d) ^ y.wrapping_mul(0x1656_67b1) ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

/// The pattern's brightness (0 to 1) and opacity at a pixel.
fn texel(pattern: Pattern, x: u32, y: u32) -> (f32, f32) {
    let n = hash(x, y, pattern as u32 + 1);
    match pattern {
        Pattern::Noise => (0.80 + 0.20 * n, 1.0),
        Pattern::Smooth => (0.94 + 0.06 * n, 1.0),
        Pattern::Planks => {
            // Boards four pixels high, each its own shade, with a dark seam
            // under it and a joint at a different place in each.
            let board = y / 4;
            let shade = 0.84 + 0.12 * hash(board, 0, 7);
            let joint = (hash(board, 1, 9) * 16.0) as u32;
            if y % 4 == 3 || x == joint { (0.62, 1.0) } else { (shade + 0.05 * (n - 0.5), 1.0) }
        }
        Pattern::Bricks => {
            // Courses four pixels high, bricks eight long, every other
            // course shifted by half a brick.
            let course = y / 4;
            let shift = if course.is_multiple_of(2) { 0 } else { 4 };
            if y % 4 == 3 || (x + shift).is_multiple_of(8) {
                (1.0, 1.0)
            } else {
                (0.72 + 0.12 * hash((x + shift) / 8, course, 3) + 0.06 * n, 1.0)
            }
        }
        Pattern::Cobble => {
            // The nearest of a few jittered points makes irregular stones,
            // darker towards their edges.
            let mut best = (f32::MAX, f32::MAX, 0u32);
            for cy in 0..4u32 {
                for cx in 0..4u32 {
                    let px = cx as f32 * 4.0 + 4.0 * hash(cx, cy, 11);
                    let py = cy as f32 * 4.0 + 4.0 * hash(cx, cy, 13);
                    let dx = (x as f32 - px).abs().min(16.0 - (x as f32 - px).abs());
                    let dy = (y as f32 - py).abs().min(16.0 - (y as f32 - py).abs());
                    let d = dx * dx + dy * dy;
                    if d < best.0 {
                        best = (d, best.0, cy * 4 + cx);
                    } else if d < best.1 {
                        best.1 = d;
                    }
                }
            }
            let edge = best.1.sqrt() - best.0.sqrt();
            if edge < 0.9 { (0.58, 1.0) } else { (0.78 + 0.18 * hash(best.2, 0, 17) + 0.04 * n, 1.0) }
        }
        Pattern::Bark => (0.74 + 0.18 * hash(x, 0, 19) + 0.08 * n, 1.0),
        Pattern::Rings => {
            let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
            let r = dx.abs().max(dy.abs());
            if r > 6.6 { (0.6, 1.0) } else { (0.84 + 0.12 * ((r * 1.4).sin() * 0.5 + 0.5) + 0.04 * n, 1.0) }
        }
        Pattern::Leaves => {
            let v = if n > 0.82 {
                1.0
            } else if n < 0.22 {
                0.55
            } else {
                0.75 + 0.15 * n
            };
            (v, 1.0)
        }
        Pattern::Glass => {
            let edge = x == 0 || y == 0 || x == 15 || y == 15;
            let glint = (x == y || x == y + 1) && (3..=7).contains(&x);
            if edge {
                (0.92, 1.0)
            } else if glint {
                (1.0, 0.65)
            } else {
                (1.0, 0.45)
            }
        }
        Pattern::Plant => {
            // Blades rising from the bottom, tapering, with gaps between.
            let blade = [0u32, 3, 6, 9, 12, 14].iter().any(|&b| {
                let lean = ((15 - y) as f32 * (hash(b, 0, 23) - 0.5) * 0.5) as i32;
                let height = 6 + (hash(b, 1, 23) * 10.0) as u32;
                (x as i32 - (b as i32 + 1 + lean)).abs() <= if 15 - y < height / 2 { 1 } else { 0 } && 15 - y < height
            });
            if blade { (0.8 + 0.2 * n, 1.0) } else { (0.0, 0.0) }
        }
        Pattern::Ore => {
            let fleck = hash(x / 2, y / 2, 29);
            if fleck > 0.84 {
                (1.0, 1.0)
            } else if fleck < 0.1 {
                (0.6, 1.0)
            } else {
                (0.78 + 0.14 * n, 1.0)
            }
        }
        Pattern::Tiles => {
            if x % 8 == 7 || y % 8 == 7 {
                (0.66, 1.0)
            } else {
                (0.86 + 0.1 * hash(x / 8, y / 8, 31) + 0.04 * n, 1.0)
            }
        }
    }
}

/// The pattern as a texture, and the average of its linear brightness over
/// the opaque pixels, which the block's colour is divided by so that the
/// block's average colour stays what the table says.
pub fn texture(pattern: Pattern) -> (Texture, f32) {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let (mut sum, mut count) = (0.0f32, 0.0f32);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (v, a) = texel(pattern, x, y);
            let v8 = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba.extend_from_slice(&[v8, v8, v8, (a * 255.0).round() as u8]);
            if a > 0.0 {
                sum += srgb_to_linear(v8) * a;
                count += a;
            }
        }
    }
    let mut t = Texture::from_rgba8(SIZE, SIZE, rgba, ColorSpace::Srgb);
    t.source.container = "Slerp pattern".into();
    (t, if count > 0.0 { sum / count } else { 1.0 })
}

pub fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_bright_enough() {
        for p in [
            Pattern::Noise,
            Pattern::Smooth,
            Pattern::Planks,
            Pattern::Bricks,
            Pattern::Cobble,
            Pattern::Bark,
            Pattern::Rings,
            Pattern::Leaves,
            Pattern::Glass,
            Pattern::Plant,
            Pattern::Ore,
            Pattern::Tiles,
        ] {
            let (t, mean) = texture(p);
            assert_eq!((t.width(), t.height()), (16, 16));
            assert!((0.3..=1.0).contains(&mean), "{p:?}: {mean}");
        }
        // Plants have clear gaps; glass is partly see-through.
        assert!(texture(Pattern::Plant).0.has_alpha && texture(Pattern::Glass).0.has_alpha);
        assert!(!texture(Pattern::Noise).0.has_alpha);
    }
}
