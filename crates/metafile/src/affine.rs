//! A 2D affine transform in double precision.  Metafile coordinates are
//! 32-bit integers and mapping modes multiply several of them together, so
//! the arithmetic stays in `f64` until a point is placed on the canvas.

/// `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.  The field names follow
/// Windows' `XFORM` (`eM11`, `eM12`, `eM21`, `eM22`, `eDx`, `eDy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Affine {
        Affine { a, b, c, d, e, f }
    }

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine { e: x, f: y, ..Affine::IDENTITY }
    }

    pub fn scale(x: f64, y: f64) -> Affine {
        Affine { a: x, d: y, ..Affine::IDENTITY }
    }

    /// A rotation by `radians` in the direction of increasing angle in a
    /// y-down coordinate system (clockwise on screen).
    pub fn rotate(radians: f64) -> Affine {
        let (s, c) = radians.sin_cos();
        Affine { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// The transform that applies `self` first and `next` second.
    pub fn then(self, next: Affine) -> Affine {
        Affine {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            e: next.a * self.e + next.c * self.f + next.e,
            f: next.b * self.e + next.d * self.f + next.f,
        }
    }

    pub fn apply(self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// Transforms a vector: the translation does not apply.
    pub fn apply_vector(self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.a * x + self.c * y, self.b * x + self.d * y)
    }

    pub fn det(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// The average scale: how much areas grow, as a length.  Pen widths
    /// and dash lengths are multiplied by it.
    pub fn scale_factor(self) -> f64 {
        let s = self.det().abs().sqrt();
        if s.is_finite() { s } else { 0.0 }
    }

    pub fn invert(self) -> Option<Affine> {
        let det = self.det();
        if !det.is_finite() || det.abs() < 1e-300 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine { a, b, c, d, e: -(a * self.e + c * self.f), f: -(b * self.e + d * self.f) })
    }

    pub fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f].iter().all(|v| v.is_finite())
    }

    /// For tiny-skia, which wants single precision.
    pub fn to_skia(self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_row(
            self.a as f32,
            self.b as f32,
            self.c as f32,
            self.d as f32,
            self.e as f32,
            self.f as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn then_applies_in_order() {
        let t = Affine::scale(2.0, 3.0).then(Affine::translate(10.0, 20.0));
        assert_eq!(t.apply((1.0, 1.0)), (12.0, 23.0));
        let u = Affine::translate(10.0, 20.0).then(Affine::scale(2.0, 3.0));
        assert_eq!(u.apply((1.0, 1.0)), (22.0, 63.0));
    }

    #[test]
    fn invert_round_trips() {
        let t = Affine::new(2.0, 0.5, -0.25, 3.0, 7.0, -4.0);
        let p = t.apply((3.0, 5.0));
        let q = t.invert().unwrap().apply(p);
        assert!((q.0 - 3.0).abs() < 1e-9 && (q.1 - 5.0).abs() < 1e-9);
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn scale_factor_ignores_direction() {
        assert!((Affine::scale(2.0, -2.0).scale_factor() - 2.0).abs() < 1e-12);
        assert!((Affine::rotate(1.0).scale_factor() - 1.0).abs() < 1e-12);
    }
}
