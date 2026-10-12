//! The 3D view's cameras: orbiting an object (drag, pan, dolly, a pivot
//! picked by double-click, framing and the standard views, all eased) and
//! flying through a level (WASD with mouse look), switched with Tab.

use glam::{Mat4, Vec2, Vec3};
use slerp_scene::{Aabb, FlyCamera, OrbitCamera, View};
use std::f32::consts::FRAC_PI_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Orbit,
    Fly,
}

/// Seconds a framing or view change takes.
const EASE_SECONDS: f32 = 0.25;
/// Radians of orbit per point dragged.
const ORBIT_SPEED: f32 = 0.006;
/// Radians of mouse look per pixel moved.
const LOOK_SPEED: f32 = 0.0025;
/// The orbit pitch limit, short of straight up or down where yaw is lost.
const MAX_PITCH: f32 = FRAC_PI_2 - 0.001;

pub struct Cameras {
    pub mode: Mode,
    pub orbit: OrbitCamera,
    pub fly: FlyCamera,
    /// Metres per second.
    pub fly_speed: f32,
    /// Bounds of the scene, for clip planes and dolly limits.
    bounds: Aabb,
    home_orbit: OrbitCamera,
    home_fly: FlyCamera,
    home_mode: Mode,
    ease: Option<Ease>,
}

struct Ease {
    from: OrbitCamera,
    to: OrbitCamera,
    t: f32,
}

impl Cameras {
    /// Makes `orbit` the starting and home view, for scenes framed by
    /// something other than their bounds (a MetaHuman head's portrait).
    pub fn set_home_orbit(&mut self, orbit: OrbitCamera) {
        self.orbit = orbit;
        self.home_orbit = orbit;
    }

    /// Cameras for a scene with `bounds`: an orbit fitted to them, and a
    /// fly camera at `spawn` (position and yaw) or looking in from the
    /// orbit's eye.  `fly_first` picks the starting mode, for levels.
    pub fn new(bounds: Aabb, aspect: f32, pitch_deg: f32, spawn: Option<(Vec3, f32)>, fly_first: bool) -> Cameras {
        let mut orbit = OrbitCamera::framing(&bounds, aspect);
        orbit.pitch = pitch_deg.to_radians();
        let (near, far) = slerp_core::fly_clip_planes(&bounds);
        let mut fly = FlyCamera { position: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 75f32.to_radians(), near, far };
        match spawn {
            Some((position, yaw)) => {
                fly.position = position;
                fly.yaw = yaw;
            }
            None => fly_from_orbit(&mut fly, &orbit),
        }
        let radius = if bounds.is_empty() { 1.0 } else { bounds.radius() };
        let mode = if fly_first { Mode::Fly } else { Mode::Orbit };
        Cameras {
            mode,
            orbit,
            fly,
            // Quake's run speed is 320 units, about 8 m/s; scale with the
            // scene so small objects can be flown around too.
            fly_speed: (radius * 0.15).clamp(0.2, 8.0),
            bounds,
            home_orbit: orbit,
            home_fly: fly,
            home_mode: mode,
            ease: None,
        }
    }

    /// The view for a viewport of `aspect`.
    pub fn view(&self, aspect: f32) -> View {
        match self.mode {
            Mode::Orbit => self.orbit.view(aspect),
            Mode::Fly => self.fly.view(aspect),
        }
    }

    pub fn is_easing(&self) -> bool {
        self.ease.is_some()
    }

    /// Moves eased transitions on.  Returns whether the camera moved.
    pub fn advance(&mut self, dt: f32) -> bool {
        let Some(ease) = &mut self.ease else { return false };
        ease.t = (ease.t + dt / EASE_SECONDS).min(1.0);
        let k = ease.t * ease.t * (3.0 - 2.0 * ease.t);
        let (a, b) = (ease.from, ease.to);
        // Turn the short way round.
        let mut dyaw = (b.yaw - a.yaw).rem_euclid(std::f32::consts::TAU);
        if dyaw > std::f32::consts::PI {
            dyaw -= std::f32::consts::TAU;
        }
        self.orbit = OrbitCamera {
            target: a.target.lerp(b.target, k),
            yaw: a.yaw + dyaw * k,
            pitch: a.pitch + (b.pitch - a.pitch) * k,
            distance: a.distance * (b.distance / a.distance.max(1e-6)).powf(k),
            fov_y: a.fov_y + (b.fov_y - a.fov_y) * k,
            ortho: b.ortho,
        };
        if ease.t >= 1.0 {
            self.orbit = b;
            self.ease = None;
        }
        true
    }

    fn ease_to(&mut self, to: OrbitCamera) {
        self.mode = Mode::Orbit;
        self.ease = Some(Ease { from: self.orbit, to, t: 0.0 });
    }

    /// Cancels an eased move where it is, as soon as the user takes over.
    fn stop_easing(&mut self) {
        self.ease = None;
    }

    /*┌─────────────────╮
    | Orbit controls  |
    └─────────────────╯*/
    //region Orbit controls

    /// Turns the orbit by a drag of `delta` points.
    pub fn orbit_drag(&mut self, delta: Vec2) {
        self.stop_easing();
        self.orbit.yaw -= delta.x * ORBIT_SPEED;
        self.orbit.pitch = (self.orbit.pitch + delta.y * ORBIT_SPEED).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Slides the orbit target with the pointer by a drag of `delta`
    /// points in a viewport `height` points tall, so the point under the
    /// pointer at the target's depth stays under it.
    pub fn pan(&mut self, delta: Vec2, height: f32) {
        self.stop_easing();
        let o = &mut self.orbit;
        let per_point = 2.0 * o.distance * (o.fov_y * 0.5).tan() / height.max(1.0);
        let view = o.view(1.0).view;
        let right = view.row(0).truncate();
        let up = view.row(1).truncate();
        o.target += (-right * delta.x + up * delta.y) * per_point;
    }

    /// Moves towards (positive `steps`) or away from the target.
    pub fn dolly(&mut self, steps: f32) {
        self.stop_easing();
        let radius = self.radius();
        self.orbit.distance = (self.orbit.distance * 0.85f32.powf(steps)).clamp(radius * 1e-3, radius * 200.0);
    }

    /// Fits the bounds in view, keeping the angles.
    pub fn frame(&mut self, bounds: &Aabb, aspect: f32) {
        match self.mode {
            Mode::Orbit => {
                let mut to = self.orbit;
                to.frame(bounds, aspect);
                self.ease_to(to);
            }
            Mode::Fly => {
                // Back away along the view direction until the bounds fit.
                let mut o = OrbitCamera { ortho: false, fov_y: self.fly.fov_y, ..self.orbit };
                o.yaw = self.fly.yaw;
                o.pitch = -self.fly.pitch;
                o.frame(bounds, aspect);
                fly_from_orbit(&mut self.fly, &o);
            }
        }
    }

    /// One of the standard views, in degrees.
    pub fn look_from(&mut self, yaw_deg: f32, pitch_deg: f32) {
        let mut to = self.orbit;
        to.yaw = yaw_deg.to_radians();
        to.pitch = pitch_deg.to_radians().clamp(-MAX_PITCH, MAX_PITCH);
        self.ease_to(to);
    }

    /// Makes `point` the orbit's pivot, turning to centre it but keeping
    /// the eye where it is.
    pub fn pivot(&mut self, point: Vec3) {
        let eye = self.current_eye();
        let d = eye - point;
        let distance = d.length();
        if distance < 1e-6 {
            return;
        }
        let dir = d / distance;
        let to = OrbitCamera {
            target: point,
            yaw: dir.x.atan2(dir.z),
            pitch: dir.y.clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH),
            distance,
            ..self.orbit
        };
        if self.mode == Mode::Fly {
            self.orbit = to;
            self.mode = Mode::Orbit;
        } else {
            self.ease_to(to);
        }
    }

    pub fn set_ortho(&mut self, ortho: bool) {
        self.orbit.ortho = ortho;
        if let Some(e) = &mut self.ease {
            e.to.ortho = ortho;
        }
    }

    //endregion

    /*┌─────────────────╮
    | Fly controls    |
    └─────────────────╯*/
    //region Fly controls

    /// Mouse look by a movement of `delta` pixels.
    pub fn look(&mut self, delta: Vec2) {
        self.fly.yaw -= delta.x * LOOK_SPEED;
        self.fly.pitch = (self.fly.pitch - delta.y * LOOK_SPEED).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Flies by `dir` (x right, y up, z forward, each -1 to 1) for `dt`
    /// seconds at the fly speed times `boost`.  Forward follows the view,
    /// up is always the world's.
    pub fn fly_move(&mut self, dir: Vec3, dt: f32, boost: f32) {
        let f = self.fly.forward();
        let r = self.fly.right();
        let step = (r * dir.x + Vec3::Y * dir.y + f * dir.z) * self.fly_speed * boost * dt;
        if step.is_finite() {
            self.fly.position += step;
        }
    }

    /// Wheel in fly mode: faster or slower by `steps` notches.
    pub fn change_speed(&mut self, steps: f32) {
        self.fly_speed = (self.fly_speed * 1.2f32.powf(steps)).clamp(0.01, 500.0);
    }

    //endregion

    /// Switches between orbit and fly, keeping the view where it is.
    pub fn toggle_mode(&mut self) {
        self.stop_easing();
        match self.mode {
            Mode::Orbit => {
                fly_from_orbit(&mut self.fly, &self.orbit);
                self.mode = Mode::Fly;
            }
            Mode::Fly => {
                let f = self.fly.forward();
                self.orbit.yaw = self.fly.yaw;
                self.orbit.pitch = (-self.fly.pitch).clamp(-MAX_PITCH, MAX_PITCH);
                self.orbit.target = self.fly.position + f * self.orbit.distance;
                self.mode = Mode::Orbit;
            }
        }
    }

    /// Back to how the file opened.
    pub fn home(&mut self) {
        self.mode = self.home_mode;
        self.fly = self.home_fly;
        if self.mode == Mode::Orbit {
            let to = OrbitCamera { ortho: self.orbit.ortho, ..self.home_orbit };
            self.ease_to(to);
        } else {
            self.ease = None;
            self.orbit = self.home_orbit;
        }
    }

    /// Keeps the cameras' scene size current after visibility changes.
    /// Looks the way `old` does, after the file was reloaded: the mode,
    /// orbit and flying position and speed.  The new scene's bounds, clip
    /// planes and home view stay.
    pub fn keep_view_of(&mut self, old: &Cameras) {
        self.mode = old.mode;
        self.orbit = old.ease.as_ref().map_or(old.orbit, |e| e.to);
        let (near, far) = (self.fly.near, self.fly.far);
        self.fly = FlyCamera { near, far, ..old.fly };
        self.fly_speed = old.fly_speed;
        self.ease = None;
    }

    pub fn set_bounds(&mut self, bounds: Aabb) {
        if bounds.is_empty() {
            return;
        }
        self.bounds = bounds;
        let (near, far) = slerp_core::fly_clip_planes(&bounds);
        self.fly.near = near;
        self.fly.far = far;
    }

    fn radius(&self) -> f32 {
        if self.bounds.is_empty() { 1.0 } else { self.bounds.radius().max(1e-4) }
    }

    fn current_eye(&self) -> Vec3 {
        match self.mode {
            Mode::Orbit => self.orbit.eye(),
            Mode::Fly => self.fly.position,
        }
    }
}

/// Puts the fly camera at the orbit's eye, looking at its target.
fn fly_from_orbit(fly: &mut FlyCamera, orbit: &OrbitCamera) {
    fly.position = orbit.eye();
    // The orbit's eye sits along (sin yaw cos pitch, sin pitch, cos yaw
    // cos pitch) from the target; looking back is the fly camera's forward
    // at the same yaw and the opposite pitch.
    fly.yaw = orbit.yaw;
    fly.pitch = -orbit.pitch;
}

/// A ray from the eye through a point of the viewport, `ndc` from -1 to 1
/// with y up.
pub fn ray(view: &View, ndc: Vec2) -> (Vec3, Vec3) {
    let inv: Mat4 = view.view_proj().inverse();
    let near = inv.project_point3(ndc.extend(0.0));
    let far = inv.project_point3(ndc.extend(1.0));
    (near, (far - near).normalize_or_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box() -> Aabb {
        Aabb { min: Vec3::splat(-1.0), max: Vec3::splat(1.0) }
    }

    #[test]
    fn switching_modes_keeps_the_view() {
        let mut c = Cameras::new(unit_box(), 1.5, 20.0, None, false);
        let before = c.view(1.5).view;
        c.toggle_mode();
        assert_eq!(c.mode, Mode::Fly);
        let fly = c.view(1.5).view;
        assert!(before.abs_diff_eq(fly, 1e-4), "{before} {fly}");
        c.look(Vec2::new(30.0, -12.0));
        let turned = c.view(1.5).view;
        c.toggle_mode();
        assert!(turned.abs_diff_eq(c.view(1.5).view, 1e-4));
    }

    #[test]
    fn pivot_keeps_the_eye() {
        let mut c = Cameras::new(unit_box(), 1.0, 20.0, None, false);
        let eye = c.orbit.eye();
        c.pivot(Vec3::new(0.5, 0.2, -0.3));
        while c.advance(0.05) {}
        assert!((c.orbit.eye() - eye).length() < 1e-4);
        assert_eq!(c.orbit.target, Vec3::new(0.5, 0.2, -0.3));
    }

    #[test]
    fn pan_follows_the_pointer() {
        let mut c = Cameras::new(unit_box(), 1.0, 0.0, None, false);
        c.orbit.yaw = 0.0;
        let height = 500.0;
        let target = c.orbit.target;
        let before = c.view(1.0).view_proj().project_point3(target);
        c.pan(Vec2::new(50.0, 0.0), height);
        let after = c.view(1.0).view_proj().project_point3(target);
        // 50 points of 500 is a fifth of the half-height 1.0 in NDC.
        assert!(((after.x - before.x) - 0.2).abs() < 1e-3, "{before} {after}");
    }

    #[test]
    fn rays_go_through_the_centre() {
        let c = Cameras::new(unit_box(), 1.0, 20.0, None, false);
        let (o, d) = ray(&c.view(1.0), Vec2::ZERO);
        let to_target = (c.orbit.target - o).normalize();
        assert!(d.dot(to_target) > 0.9999);
    }
}
