//! Cameras shared by thumbnails and the viewer.

use crate::Aabb;
use glam::camera::rh::{proj::directx as proj, view};
use glam::{Mat4, Vec3};

/// What the renderer needs from any camera.  Projections are wgpu's: depth
/// 0 to 1, Y up.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub view: Mat4,
    pub proj: Mat4,
    pub position: Vec3,
    pub near: f32,
    pub far: f32,
}

impl View {
    pub fn view_proj(&self) -> Mat4 {
        self.proj * self.view
    }
}

/// A camera circling a target point: the default for objects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Radians around +Y; 0 looks from +Z towards -Z.
    pub yaw: f32,
    /// Radians above the horizon.
    pub pitch: f32,
    pub distance: f32,
    /// Vertical field of view in radians.
    pub fov_y: f32,
    /// Orthographic projection with the same framing, when true.
    pub ortho: bool,
}

impl OrbitCamera {
    /// A three-quarter view from the front right and a little above, at a
    /// distance that fits `bounds` in a viewport of `aspect` (width/height).
    pub fn framing(bounds: &Aabb, aspect: f32) -> OrbitCamera {
        let mut cam = OrbitCamera {
            target: Vec3::ZERO,
            yaw: 30f32.to_radians(),
            pitch: 20f32.to_radians(),
            distance: 1.0,
            fov_y: 35f32.to_radians(),
            ortho: false,
        };
        cam.frame(bounds, aspect);
        cam
    }

    /// Keeps the angles and moves the target and distance so `bounds` fits.
    pub fn frame(&mut self, bounds: &Aabb, aspect: f32) {
        if bounds.is_empty() {
            self.target = Vec3::ZERO;
            self.distance = 3.0;
            return;
        }
        self.target = bounds.center();
        let radius = bounds.radius().max(1e-4);
        // Fit the bounding sphere in the narrower of the two fields of view.
        let half_y = self.fov_y * 0.5;
        let half_x = (half_y.tan() * aspect.min(1.0)).atan();
        let half = half_y.min(half_x);
        self.distance = radius / half.sin() * 1.02;
    }

    pub fn eye(&self) -> Vec3 {
        let dir = Vec3::new(self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), self.yaw.cos() * self.pitch.cos());
        self.target + dir * self.distance
    }

    pub fn view(&self, aspect: f32) -> View {
        let eye = self.eye();
        let view = view::look_at_mat4(eye, self.target, Vec3::Y);
        let near = (self.distance * 0.01).max(1e-4);
        let far = self.distance * 100.0;
        let proj = if self.ortho {
            let h = self.distance * (self.fov_y * 0.5).tan();
            proj::orthographic(-h * aspect, h * aspect, -h, h, -far, far)
        } else {
            proj::perspective(self.fov_y, aspect, near, far)
        };
        View { view, proj, position: eye, near, far }
    }
}

/// A free-flying first-person camera: the default for levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlyCamera {
    pub position: Vec3,
    /// Radians around +Y; 0 looks down -Z.
    pub yaw: f32,
    /// Radians up from the horizon.
    pub pitch: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl FlyCamera {
    pub fn forward(&self) -> Vec3 {
        Vec3::new(-self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), -self.yaw.cos() * self.pitch.cos())
    }

    pub fn right(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), 0.0, -self.yaw.sin())
    }

    pub fn view(&self, aspect: f32) -> View {
        let view = view::look_to_mat4(self.position, self.forward(), Vec3::Y);
        let proj = proj::perspective(self.fov_y, aspect, self.near, self.far);
        View { view, proj, position: self.position, near: self.near, far: self.far }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_keeps_box_in_view() {
        let b = Aabb { min: Vec3::new(-1.0, 0.0, -1.0), max: Vec3::new(1.0, 2.0, 1.0) };
        let cam = OrbitCamera::framing(&b, 1.0);
        let vp = cam.view(1.0).view_proj();
        for c in 0..8 {
            let p = Vec3::new(
                if c & 1 == 0 { b.min.x } else { b.max.x },
                if c & 2 == 0 { b.min.y } else { b.max.y },
                if c & 4 == 0 { b.min.z } else { b.max.z },
            );
            let ndc = vp.project_point3(p);
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0, "{p} -> {ndc}");
        }
    }

    #[test]
    fn fly_forward_matches_view() {
        let cam = FlyCamera { position: Vec3::ZERO, yaw: 0.5, pitch: 0.2, fov_y: 1.0, near: 0.1, far: 100.0 };
        let p = cam.view(1.0).view_proj().project_point3(cam.forward() * 10.0);
        assert!(p.x.abs() < 1e-4 && p.y.abs() < 1e-4);
    }
}
