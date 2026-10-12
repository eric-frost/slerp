//! Turning a format's coordinates into Slerp's: right-handed, Y up, the
//! model facing +Z (towards the default camera), in metres.
//!
//! Each format is converted in its data rather than under a converting
//! root node, because two of them (MikuMikuDance and Unreal) are
//! left-handed: their conversion is a reflection, which a node transform
//! would leave to the renderer as a mirrored skinned mesh.  A reflection
//! reverses which way round a triangle's corners go, so meshes ask
//! [`Basis::flip_winding`] whether to swap two of every triangle's
//! corners.

use slerp_scene::Transform;
use slerp_scene::glam::{Mat3, Quat, Vec3};

/// A change of axes (a signed permutation: a rotation, or a rotation and a
/// reflection) and a unit scale.
#[derive(Clone, Copy, Debug)]
pub struct Basis {
    /// Columns are where the file's X, Y and Z axes end up.
    axes: Mat3,
    /// The rotation part of `axes`: `axes` itself, or `-axes` for a
    /// reflection.  Rotations convert as `rot * q * rot⁻¹`, since
    /// `axes * R * axesᵀ` equals `rot * R * rotᵀ` either way.
    rot: Quat,
    mirrored: bool,
    scale: f32,
}

impl Basis {
    /// `axes` columns: where the file's X, Y and Z axes point in Slerp's
    /// space.  Must be a signed permutation.
    pub fn new(axes: Mat3, scale: f32) -> Basis {
        let mirrored = axes.determinant() < 0.0;
        let proper = if mirrored { -axes } else { axes };
        Basis { axes, rot: Quat::from_mat3(&proper).normalize(), mirrored, scale }
    }

    /// Doom 3, Source and Quake: right-handed, Z up, models facing +X, one
    /// unit an inch.
    pub fn z_up_x_forward_inches() -> Basis {
        Basis::new(Mat3::from_cols(Vec3::Z, Vec3::X, Vec3::Y), 0.0254)
    }

    /// MikuMikuDance: left-handed, Y up, models facing -Z, one unit about
    /// 8 cm (a 158 cm character is about 20 units tall).
    pub fn mmd() -> Basis {
        Basis::new(Mat3::from_cols(Vec3::X, Vec3::Y, Vec3::NEG_Z), 0.08)
    }

    /// Unreal: left-handed, Z up, skeletal meshes facing +Y, centimetres.
    pub fn unreal() -> Basis {
        Basis::new(Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::Y), 0.01)
    }

    pub fn point(&self, v: Vec3) -> Vec3 {
        self.axes * v * self.scale
    }

    /// A direction or normal: turned, not scaled.
    pub fn dir(&self, v: Vec3) -> Vec3 {
        self.axes * v
    }

    pub fn rotation(&self, q: Quat) -> Quat {
        (self.rot * q * self.rot.conjugate()).normalize()
    }

    /// A per-axis scale, permuted with the axes.
    pub fn scale(&self, s: Vec3) -> Vec3 {
        self.axes.abs() * s
    }

    pub fn transform(&self, t: Transform) -> Transform {
        Transform {
            translation: self.point(t.translation),
            rotation: self.rotation(t.rotation),
            scale: self.scale(t.scale),
        }
    }

    /// Whether triangles must swap two corners to face the right way after
    /// conversion.  `outward` says whether, in the file's own coordinates,
    /// the cross product `(b - a) × (c - a)` of a triangle's corners in file
    /// order points out of the model: true for counter-clockwise front
    /// faces in a right-handed space and for clockwise ones in a
    /// left-handed space (Direct3D's convention).
    pub fn flip_winding(&self, outward: bool) -> bool {
        outward == self.mirrored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    /// A rotation converted on its own must turn converted points the way
    /// the original turned the originals.
    fn check_rotations(b: Basis) {
        let q = Quat::from_axis_angle(Vec3::new(0.3, -0.5, 0.8).normalize(), 0.9);
        let p = Vec3::new(1.0, 2.0, -3.0);
        assert!(close(b.rotation(q) * b.dir(p), b.dir(q * p)), "{b:?}");
    }

    #[test]
    fn source_style_axes() {
        let b = Basis::z_up_x_forward_inches();
        // Up is up, forward faces the camera, left is the viewer's right.
        assert!(close(b.dir(Vec3::Z), Vec3::Y));
        assert!(close(b.dir(Vec3::X), Vec3::Z));
        assert!(close(b.dir(Vec3::Y), Vec3::X));
        assert!(close(b.point(Vec3::new(0.0, 0.0, 100.0)), Vec3::new(0.0, 2.54, 0.0)));
        assert!(!b.flip_winding(true));
        check_rotations(b);
    }

    #[test]
    fn left_handed_formats_mirror() {
        for b in [Basis::mmd(), Basis::unreal()] {
            check_rotations(b);
            assert!(b.flip_winding(true));
            assert!(!b.flip_winding(false));
        }
        // MMD's -Z (the way models face) becomes +Z.
        assert!(close(Basis::mmd().dir(Vec3::NEG_Z), Vec3::Z));
        // Unreal's Z up and +Y forward become Y up and +Z forward.
        assert!(close(Basis::unreal().dir(Vec3::Z), Vec3::Y));
        assert!(close(Basis::unreal().dir(Vec3::Y), Vec3::Z));
    }

    #[test]
    fn transforms_compose_like_matrices() {
        let b = Basis::unreal();
        let t = Transform {
            translation: Vec3::new(10.0, 20.0, 30.0),
            rotation: Quat::from_rotation_z(0.7),
            scale: Vec3::new(1.0, 2.0, 3.0),
        };
        let c = b.transform(t);
        // Converting a point then applying the converted transform equals
        // applying the transform then converting.
        let p = Vec3::new(1.0, -1.0, 2.0);
        let direct = b.point(t.matrix().transform_point3(p));
        let via = c.matrix().transform_point3(b.point(p));
        assert!(close(direct, via), "{direct} {via}");
    }
}
