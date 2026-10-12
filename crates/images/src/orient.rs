//! EXIF orientation: photographs are stored as the sensor saw them, with a
//! number from 1 to 8 saying how to turn them for display.

/// The names of the eight orientations, as people describe the turn needed.
pub fn describe(orientation: u8) -> &'static str {
    match orientation {
        2 => "mirrored left to right",
        3 => "turned 180 degrees",
        4 => "mirrored top to bottom",
        5 => "transposed (mirrored along the main diagonal)",
        6 => "turned 90 degrees clockwise",
        7 => "transversed (mirrored along the other diagonal)",
        8 => "turned 90 degrees counter-clockwise",
        _ => "upright",
    }
}

/// Whether displaying with this orientation swaps width and height.
pub fn swaps_axes(orientation: u8) -> bool {
    matches!(orientation, 5..=8)
}

/// Turns `src`, `w` by `h` pixels of `comps` values each, as EXIF
/// orientation `orientation` asks (values other than 2 to 8 leave it
/// alone).  Returns the new pixels and the new width and height.
pub fn orient<T: Copy>(src: &[T], w: usize, h: usize, comps: usize, orientation: u8) -> (Vec<T>, usize, usize) {
    if !(2..=8).contains(&orientation) || src.len() != w * h * comps || w == 0 || h == 0 {
        return (src.to_vec(), w, h);
    }
    let (dw, dh) = if swaps_axes(orientation) { (h, w) } else { (w, h) };
    let mut out = Vec::with_capacity(src.len());
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match orientation {
                2 => (w - 1 - dx, dy),
                3 => (w - 1 - dx, h - 1 - dy),
                4 => (dx, h - 1 - dy),
                5 => (dy, dx),
                6 => (dy, h - 1 - dx),
                7 => (w - 1 - dy, h - 1 - dx),
                _ => (w - 1 - dy, dx),
            };
            let at = (sy * w + sx) * comps;
            out.extend_from_slice(&src[at..at + comps]);
        }
    }
    (out, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 by 2 picture whose pixels are numbered row by row:
    /// ```text
    /// 1 2 3
    /// 4 5 6
    /// ```
    const P: [u8; 6] = [1, 2, 3, 4, 5, 6];

    #[test]
    fn the_eight_orientations() {
        let cases: [(u8, [u8; 6], (usize, usize)); 8] = [
            (1, [1, 2, 3, 4, 5, 6], (3, 2)),
            (2, [3, 2, 1, 6, 5, 4], (3, 2)),
            (3, [6, 5, 4, 3, 2, 1], (3, 2)),
            (4, [4, 5, 6, 1, 2, 3], (3, 2)),
            // Transposed: rows become columns.
            (5, [1, 4, 2, 5, 3, 6], (2, 3)),
            // Turned clockwise: the left column becomes the top row, bottom first.
            (6, [4, 1, 5, 2, 6, 3], (2, 3)),
            (7, [6, 3, 5, 2, 4, 1], (2, 3)),
            // Turned counter-clockwise: the right column becomes the top row.
            (8, [3, 6, 2, 5, 1, 4], (2, 3)),
        ];
        for (o, want, dims) in cases {
            let (got, w, h) = orient(&P, 3, 2, 1, o);
            assert_eq!(((w, h), got.as_slice()), (dims, want.as_slice()), "orientation {o}");
        }
    }

    #[test]
    fn pixels_of_several_values_stay_together() {
        let rgb = [1u8, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4];
        let (got, w, h) = orient(&rgb, 2, 2, 3, 6);
        assert_eq!((w, h), (2, 2));
        assert_eq!(got, [3, 3, 3, 1, 1, 1, 4, 4, 4, 2, 2, 2]);
    }

    #[test]
    fn nonsense_changes_nothing() {
        assert_eq!(orient(&P, 3, 2, 1, 0).0, P);
        assert_eq!(orient(&P, 3, 2, 1, 9).0, P);
        // A length that does not match the size is left alone, not read past.
        assert_eq!(orient(&P, 4, 2, 1, 6).0, P);
    }

    #[test]
    fn two_turns_are_a_half_turn() {
        let (a, w, h) = orient(&P, 3, 2, 1, 6);
        let (b, w2, h2) = orient(&a, w, h, 1, 6);
        assert_eq!((b, w2, h2), (orient(&P, 3, 2, 1, 3).0, 3, 2));
    }
}
