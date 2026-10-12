//! Overlay lines built on the CPU each frame: the bounding box, markers and
//! the skeleton.  Normals, tangents, the wireframe and the grid are drawn
//! by shaders instead, since they scale with the mesh or the screen.

use crate::renderer::RenderSettings;
use crate::resources::LineVertex;
use crate::scene_gpu::GpuScene;
use glam::{Vec3, Vec4};

/// The lines to draw: depth-tested ones first, then `on_top` lines that
/// show through the mesh (the skeleton).
pub(crate) fn lines(scene: &GpuScene, settings: &RenderSettings, line_length: f32) -> (Vec<LineVertex>, usize) {
    let o = &settings.overlays;
    let mut out = Vec::new();
    let line = |out: &mut Vec<LineVertex>, a: Vec3, b: Vec3, c: Vec4| {
        out.push(LineVertex { pos: a.to_array(), color: c.to_array() });
        out.push(LineVertex { pos: b.to_array(), color: c.to_array() });
    };
    if o.bounds && !scene.bounds.is_empty() {
        let (lo, hi) = (scene.bounds.min, scene.bounds.max);
        let corner = |i: u32| {
            Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        };
        let color = Vec4::new(1.0, 0.85, 0.2, 1.0);
        for i in 0..8u32 {
            for bit in [1, 2, 4] {
                if i & bit == 0 {
                    line(&mut out, corner(i), corner(i | bit), color);
                }
            }
        }
    }
    if o.markers {
        let s = line_length * 1.5;
        for m in &scene.markers {
            if !m.position.is_finite() {
                continue;
            }
            let c = Vec4::new(m.color.x, m.color.y, m.color.z, 1.0);
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                line(&mut out, m.position - axis * s, m.position + axis * s, c);
            }
        }
    }
    let before = out.len();
    if o.skeleton {
        let bone = Vec4::new(0.25, 0.9, 1.0, 0.9);
        let hot = Vec4::new(1.0, 0.3, 0.85, 1.0);
        for &(a, b, child) in &scene.bones {
            line(&mut out, a, b, if o.highlight_joint == Some(child) { hot } else { bone });
        }
        if let Some(j) = o.highlight_joint
            && let Some(&(_, b, _)) = scene.bones.iter().find(|bone| bone.2 == j)
        {
            let s = line_length;
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                line(&mut out, b - axis * s, b + axis * s, hot);
            }
        }
    }
    let on_top = out.len() - before;
    (out, on_top)
}
