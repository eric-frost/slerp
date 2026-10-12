//! A drawing with depth as a 3D scene: 3D faces and meshes as shaded
//! surfaces, lines and curves as thin crossed ribbons (so that they show
//! from any side, at a size that survives a thumbnail), one node per layer.

use crate::drawing::{Drawing, FOREGROUND};
use glam::{DVec3, Vec3, Vec4};
use slerp_scene::{Info, Limits, Material, Mesh, Primitive, Scene, Topology};
use std::collections::BTreeMap;

/// The half width of a line ribbon, as a fraction of the drawing's size.
const WIRE: f64 = 0.0022;
/// Beyond this many segments lines are drawn as the renderer's own thin
/// lines instead of ribbons.
const MAX_RIBBONS: usize = 150_000;

/// sRGB (0 to 1) to linear light.
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A drawing colour as linear RGB.  The pen colour is a grey that shows on
/// both light and dark backgrounds.
fn color(c: u32, lit: bool) -> Vec4 {
    if c == FOREGROUND {
        let g = if lit { 0.8 } else { 0.55 };
        return Vec4::new(linear(g), linear(g), linear(g), 1.0);
    }
    let ch = |shift: u32| linear((c >> shift & 255) as f32 / 255.0);
    Vec4::new(ch(16), ch(8), ch(0), 1.0)
}

/// What a layer's mesh is being built from: primitives by colour and kind.
#[derive(Default)]
struct LayerGeo {
    /// Colour, then triangles (3 vertices each).
    surfaces: BTreeMap<(u32, bool), Vec<Vec3>>,
    /// Colour, then ribbon vertices or line segment ends.
    wires: BTreeMap<u32, Vec<Vec3>>,
}

fn v3(p: DVec3, origin: DVec3) -> Vec3 {
    (p - origin).as_vec3()
}

/// Builds the scene for `d`.  The drawing's z axis is up, as in CAD.
pub fn to_scene(d: &Drawing, limits: &Limits) -> Scene {
    let mut scene = Scene { name: "DXF".into(), ..Default::default() };
    let Some(bounds) = d.bounds else {
        scene.info = d.info.clone();
        return scene;
    };
    // Items are placed relative to the centre of the framing box, in f32.
    let origin = bounds.center();
    let size = bounds.size();
    let extent = size.x.max(size.y).max(size.z).max(1e-9);
    let t = extent * WIRE;
    // Items far from the rest are left out, as they are from the framing.
    let slack = extent;
    let near = |p: DVec3| {
        p.x >= bounds.min.x - slack
            && p.x <= bounds.max.x + slack
            && p.y >= bounds.min.y - slack
            && p.y <= bounds.max.y + slack
            && p.z >= bounds.min.z - slack
            && p.z <= bounds.max.z + slack
    };
    let mut layers: Vec<LayerGeo> = (0..d.layers.len().max(1)).map(|_| LayerGeo::default()).collect();
    let last = layers.len() - 1;
    let mut tris = 0u64;
    let mut segments = 0usize;
    let mut warnings = d.info.warnings.clone();
    // 1. Surfaces.
    for f in &d.fills {
        let verts = d.fill_verts(f);
        if !verts.first().is_some_and(|p| near(*p)) {
            continue;
        }
        tris += (verts.len() / 3) as u64;
        if tris > limits.max_triangles {
            warnings.push("The drawing has more surfaces than a thumbnail draws; the rest are left out.".into());
            break;
        }
        let geo = &mut layers[(f.layer as usize).min(last)];
        geo.surfaces.entry((f.color, f.lit)).or_default().extend(verts.iter().map(|p| v3(*p, origin)));
    }
    // 2. Lines, arcs and points as lists of segments.
    let mut lines: Vec<(u32, u32, Vec<DVec3>)> = Vec::new();
    let mut count = 0usize;
    for p in &d.polys {
        let pts = d.poly_pts(p);
        if pts.first().is_some_and(|q| near(*q)) {
            count += pts.len() - 1;
            lines.push((p.layer, p.color, pts.to_vec()));
        }
    }
    for a in &d.arcs {
        if !near(a.c) {
            continue;
        }
        let radius = a.u.length().max(a.v.length());
        let n = ((a.t1 - a.t0).abs() / (0.12f64.min(2.0 * (1.0 - (t / radius.max(1e-12)).min(0.5)).acos().max(0.01))))
            .ceil() as usize;
        let n = n.clamp(8, 720);
        let pts: Vec<DVec3> = (0..=n)
            .map(|i| {
                let ang = a.t0 + (a.t1 - a.t0) * i as f64 / n as f64;
                a.c + a.u * ang.cos() + a.v * ang.sin()
            })
            .collect();
        count += n;
        lines.push((a.layer, a.color, pts));
    }
    segments += count;
    let ribbons = segments <= MAX_RIBBONS;
    for (layer, c, pts) in lines {
        let geo = &mut layers[(layer as usize).min(last)];
        let wire = geo.wires.entry(c).or_default();
        for w in pts.windows(2) {
            let (p, q) = (v3(w[0], origin), v3(w[1], origin));
            if ribbons {
                ribbon(wire, p, q, t as f32);
            } else {
                wire.extend([p, q]);
            }
        }
    }
    for dot in &d.dots {
        if near(dot.p) {
            let geo = &mut layers[(dot.layer as usize).min(last)];
            let wire = geo.wires.entry(dot.color).or_default();
            let p = v3(dot.p, origin);
            let h = Vec3::splat(t as f32 * 2.0);
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                ribbon(wire, p - axis * h, p + axis * h, t as f32);
            }
        }
    }
    // 3. One node per layer.
    for (i, geo) in layers.into_iter().enumerate() {
        if geo.surfaces.is_empty() && geo.wires.is_empty() {
            continue;
        }
        let layer = d.layers.get(i);
        let mut mesh = Mesh { name: layer.map_or("0".to_string(), |l| l.name.clone()), ..Default::default() };
        for ((c, lit), positions) in geo.surfaces {
            let mut m = Material {
                base_color: color(c, true),
                double_sided: true,
                roughness: 0.65,
                unlit: !lit,
                ..Default::default()
            };
            m.name = format!("{:06X}", c & 0xFF_FFFF);
            scene.materials.push(m);
            mesh.primitives.push(flat_primitive(positions, Topology::Triangles, scene.materials.len() - 1));
        }
        for (c, positions) in geo.wires {
            let mut m = Material { base_color: color(c, false), double_sided: true, unlit: true, ..Default::default() };
            m.name = format!("lines {:06X}", c & 0xFF_FFFF);
            scene.materials.push(m);
            let topology = if ribbons { Topology::Triangles } else { Topology::Lines };
            mesh.primitives.push(flat_primitive(positions, topology, scene.materials.len() - 1));
        }
        let index = scene.add_mesh_node(mesh);
        let layer_visible = layer.is_none_or(|l| l.visible);
        scene.nodes[index].visible = layer_visible;
    }
    scene.ensure_normals();
    scene.convert_z_up();
    scene.info = Info { warnings, ..d.info.clone() };
    scene
}

/// A primitive of unshared vertices: every three make a triangle (or every
/// two a line).
fn flat_primitive(positions: Vec<Vec3>, topology: Topology, material: usize) -> Primitive {
    Primitive {
        indices: (0..positions.len() as u32).collect(),
        positions,
        topology,
        material: Some(material),
        ..Default::default()
    }
}

/// Two crossed quads along the segment `p`-`q`, each `2 * t` wide, so that
/// the line shows from every direction.
fn ribbon(out: &mut Vec<Vec3>, p: Vec3, q: Vec3, t: f32) {
    let Some(d) = (q - p).try_normalize() else { return };
    let up = if d.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let n1 = d.cross(up).normalize_or_zero() * t;
    let n2 = d.cross(n1).normalize_or_zero() * t;
    for n in [n1, n2] {
        let quad = [p - n, p + n, q + n, q - n];
        for i in [0usize, 1, 2, 0, 2, 3] {
            out.push(quad[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::{DrawLayer, Fill, Poly};

    fn layer(name: &str, visible: bool) -> DrawLayer {
        DrawLayer { name: name.into(), color: FOREGROUND, visible, items: 1 }
    }

    #[test]
    fn faces_and_lines_become_nodes_per_layer() {
        let mut d = Drawing { layers: vec![layer("0", true), layer("Wires", false)], ..Default::default() };
        d.fill_pts.extend([DVec3::ZERO, DVec3::X * 10.0, DVec3::Y * 10.0 + DVec3::Z * 5.0]);
        d.fills.push(Fill {
            layer: 0,
            owner: 0,
            color: 0xFF8000,
            first: 0,
            len: 3,
            lit: true,
            min: [0.0, 0.0],
            max: [10.0, 10.0],
        });
        d.pts.extend([DVec3::ZERO, DVec3::new(10.0, 10.0, 10.0)]);
        d.polys.push(Poly {
            layer: 1,
            owner: 1,
            color: FOREGROUND,
            first: 0,
            len: 2,
            min: [0.0, 0.0],
            max: [10.0, 10.0],
        });
        d.finish();
        assert!(!d.flat);
        let s = to_scene(&d, &Limits::thumbnail());
        let faces: usize = s.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.triangle_count()).sum();
        // One face, and a ribbon of four triangles for the line.
        assert_eq!(faces, 5);
        let hidden: Vec<bool> = s.nodes.iter().filter(|n| n.mesh.is_some()).map(|n| n.visible).collect();
        assert_eq!(hidden, vec![true, false]);
    }
}
