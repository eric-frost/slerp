//! A track in 3D over its own elevation, as route viewers draw them: a
//! ribbon along the route, a translucent curtain from it down to the
//! lowest point, and its shadow on that ground, coloured by elevation,
//! speed, heart rate, cadence or power.  East is +X, north is -Z, up is
//! +Y, in metres; the root node's Y scale exaggerates the elevation, so the
//! viewer can change it without rebuilding anything.

use super::stats::{Profile, range};
use super::{LineKind, Projection, TrackSet};
use glam::{Vec3, Vec4};
use slerp_scene::{AlphaMode, Info, Marker, Material, Mesh, Node, Primitive, Scene, Transform};

/// Vertices per primitive.
const PRIMITIVE_VERTICES: usize = 1 << 20;
/// The colour of a track coloured by nothing: an orange that reads on maps
/// and on file managers' light and dark backgrounds.
pub const PLAIN: [f32; 3] = [0.99, 0.30, 0.01];
pub const OUTLINE_SRGB: [f32; 3] = [0.45, 0.55, 0.75];

/// What colours a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorBy {
    Elevation,
    Speed,
    HeartRate,
    Cadence,
    Power,
    Plain,
}

impl ColorBy {
    pub const ALL: [ColorBy; 6] =
        [ColorBy::Elevation, ColorBy::Speed, ColorBy::HeartRate, ColorBy::Cadence, ColorBy::Power, ColorBy::Plain];

    pub fn name(self) -> &'static str {
        match self {
            ColorBy::Elevation => "Elevation",
            ColorBy::Speed => "Speed",
            ColorBy::HeartRate => "Heart rate",
            ColorBy::Cadence => "Cadence",
            ColorBy::Power => "Power",
            ColorBy::Plain => "Nothing",
        }
    }

    /// The profile's values for this metric, in display units.
    pub fn values(self, p: &Profile) -> Option<Vec<f32>> {
        let v = match self {
            ColorBy::Elevation => p.ele.clone(),
            ColorBy::Speed => p.speed.iter().map(|s| s * 3.6).collect(),
            ColorBy::HeartRate => p.hr.clone(),
            ColorBy::Cadence => p.cad.clone(),
            ColorBy::Power => p.power.clone(),
            ColorBy::Plain => return None,
        };
        let (lo, hi) = range(&v);
        (hi > lo).then_some(v)
    }

    pub fn unit(self) -> &'static str {
        match self {
            ColorBy::Elevation => "m",
            ColorBy::Speed => "km/h",
            ColorBy::HeartRate => "bpm",
            ColorBy::Cadence => "rpm",
            ColorBy::Power => "W",
            ColorBy::Plain => "",
        }
    }

    /// The metrics this profile can be coloured by.
    pub fn available(p: &Profile) -> Vec<ColorBy> {
        ColorBy::ALL.iter().copied().filter(|c| *c == ColorBy::Plain || c.values(p).is_some()).collect()
    }

    /// Elevation when it varies, else speed, else nothing.
    pub fn best(p: &Profile) -> ColorBy {
        [ColorBy::Elevation, ColorBy::Speed].into_iter().find(|c| c.values(p).is_some()).unwrap_or(ColorBy::Plain)
    }
}

/// The colour scale, blue through green and amber to red, for `t` from 0
/// to 1, as sRGB.
pub fn metric_color(t: f32) -> [f32; 3] {
    const STOPS: [[f32; 3]; 5] =
        [[0.18, 0.35, 0.85], [0.10, 0.70, 0.75], [0.35, 0.80, 0.25], [0.98, 0.75, 0.10], [0.90, 0.15, 0.10]];
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 } * (STOPS.len() - 1) as f32;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f32;
    [0, 1, 2].map(|k| STOPS[i][k] + (STOPS[i + 1][k] - STOPS[i][k]) * f)
}

/// The range a metric's colours span: the 2nd to 98th percentile, so a
/// few wild readings don't wash the rest out.
pub fn robust_range(values: &[f32]) -> (f32, f32) {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return (0.0, 1.0);
    }
    v.sort_by(f32::total_cmp);
    let at = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize];
    let (lo, hi) = (at(0.02), at(0.98));
    if hi > lo { (lo, hi) } else { (v[0], v[v.len() - 1].max(v[0] + 1e-3)) }
}

/// Each profile point's colour (sRGB), and the range the scale spans.
pub fn point_colors(p: &Profile, by: ColorBy) -> (Vec<[f32; 3]>, Option<(f32, f32)>) {
    let Some(values) = by.values(p) else { return (vec![PLAIN; p.len()], None) };
    let (lo, hi) = robust_range(&values);
    let mut last = 0.5;
    let colors = values
        .iter()
        .map(|&v| {
            // Gaps take the colour before them.
            if v.is_finite() {
                last = (v - lo) / (hi - lo);
            }
            metric_color(last)
        })
        .collect();
    (colors, Some((lo, hi)))
}

/// How the 3D track looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLook {
    pub color_by: ColorBy,
    /// How many times higher than true the elevation is drawn.
    pub exaggeration: f32,
    pub curtain: bool,
}

impl TrackLook {
    pub fn for_profile(p: &Profile) -> TrackLook {
        TrackLook { color_by: ColorBy::best(p), exaggeration: default_exaggeration(p), curtain: true }
    }
}

/// Enough exaggeration that the climbs show: the elevation range drawn at
/// about a tenth of the track's extent, between 1 and 10 times.
pub fn default_exaggeration(p: &Profile) -> f32 {
    let (lo, hi) = range(&p.ele);
    let rise = hi - lo;
    let (mut min, mut max) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for q in &p.pos {
        for k in 0..2 {
            min[k] = min[k].min(q[k]);
            max[k] = max[k].max(q[k]);
        }
    }
    let extent = (max[0] - min[0]).max(max[1] - min[1]);
    if rise.is_nan() || extent.is_nan() || rise <= 0.0 || extent <= 0.0 {
        return 1.0;
    }
    ((0.1 * extent / rise).clamp(1.0, 10.0) * 2.0).round() / 2.0
}

/// The elevation every height is measured from: the lowest point.
pub fn base_elevation(set: &TrackSet, p: &Profile) -> f32 {
    let mut lo = range(&p.ele).0;
    for l in set.lines.iter().filter(|l| l.kind == LineKind::Outline) {
        for pt in &l.points {
            if pt.ele.is_finite() {
                lo = if lo.is_finite() { lo.min(pt.ele) } else { pt.ele };
            }
        }
    }
    if lo.is_finite() { lo } else { 0.0 }
}

/// Heights above the base with gaps filled from their neighbours.
fn heights(ele: &[f32], base: f32) -> Vec<f32> {
    let first = ele.iter().copied().find(|e| e.is_finite()).unwrap_or(base);
    let mut last = first;
    ele.iter()
        .map(|&e| {
            if e.is_finite() {
                last = e;
            }
            last - base
        })
        .collect()
}

fn linear(c: [f32; 3], a: f32) -> Vec4 {
    let l = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    Vec4::new(l(c[0]), l(c[1]), l(c[2]), a)
}

/// Builds the 3D scene.  With `markers`, waypoints become scene markers at
/// the look's exaggeration (the viewer draws its own instead, which follow
/// the exaggeration as it changes).
pub fn track_scene(set: &TrackSet, p: &Profile, proj: &Projection, look: &TrackLook, markers: bool) -> Scene {
    let base = base_elevation(set, p);
    let (colors, _) = point_colors(p, look.color_by);
    let has_height = p.has_elevation();
    let (mut min, mut max) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
    for q in &p.pos {
        for k in 0..2 {
            min[k] = min[k].min(q[k]);
            max[k] = max[k].max(q[k]);
        }
    }
    let extent = (max[0] - min[0]).max(max[1] - min[1]);
    let width = if extent.is_finite() && extent > 0.0 { (extent * 0.005).clamp(0.5, 500.0) } else { 5.0 };

    let mut ribbon = Builder::default();
    let mut curtain = Builder::default();
    let mut shadow = Builder::default();
    let h = heights(&p.ele, base);
    let ends: Vec<usize> = p.starts.iter().skip(1).copied().chain([p.len()]).collect();
    for (&a, &b) in p.starts.iter().zip(&ends) {
        let pts: Vec<Vec3> = (a..b).map(|i| Vec3::new(p.pos[i][0], h[i], -p.pos[i][1])).collect();
        let cols: Vec<[f32; 3]> = colors[a..b].to_vec();
        ribbon.ribbon(&pts, |i| linear(cols[i], 1.0), width);
        if has_height && look.curtain {
            curtain.curtain(&pts, |i| linear(cols[i], 0.55), |i| linear(cols[i], 0.06));
            let ground: Vec<Vec3> = pts.iter().map(|q| Vec3::new(q.x, 0.0, q.z)).collect();
            shadow.ribbon(&ground, |_| Vec4::new(0.0, 0.0, 0.0, 0.35), width);
        }
    }
    let mut outline = Builder::default();
    for l in set.lines.iter().filter(|l| l.kind == LineKind::Outline) {
        let ele: Vec<f32> = l.points.iter().map(|q| q.ele).collect();
        let hs = heights(&ele, base);
        let pts: Vec<Vec3> = l
            .points
            .iter()
            .zip(&hs)
            .map(|(q, &y)| {
                let (e, n) = proj.project(q.lat, q.lon);
                Vec3::new(e as f32, y, -(n as f32))
            })
            .collect();
        outline.ribbon(&pts, |_| linear(OUTLINE_SRGB, 1.0), width * 0.6);
    }

    let mut scene = Scene {
        name: set.name.clone().unwrap_or_else(|| "Track".into()),
        materials: vec![
            Material {
                name: "Route".into(),
                base_color: Vec4::ONE,
                unlit: true,
                double_sided: true,
                ..Default::default()
            },
            Material {
                name: "Curtain".into(),
                base_color: Vec4::ONE,
                unlit: true,
                double_sided: true,
                alpha_mode: AlphaMode::Blend,
                ..Default::default()
            },
        ],
        info: Info {
            format: set.format.clone(),
            generator: set.creator.clone(),
            warnings: set.warnings.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut root = Node::new("Track");
    root.transform = Transform { scale: Vec3::new(1.0, look.exaggeration, 1.0), ..Transform::IDENTITY };
    scene.nodes.push(root);
    scene.roots.push(0);
    for (name, builder, material) in
        [("Route", ribbon, 0), ("Curtain", curtain, 1), ("Shadow", shadow, 1), ("Outlines", outline, 0)]
    {
        let prims = builder.finish(material);
        if prims.is_empty() {
            continue;
        }
        let mut node = Node::new(name);
        node.mesh = Some(scene.meshes.len());
        scene.meshes.push(Mesh { name: name.into(), primitives: prims, target_names: Vec::new() });
        let index = scene.nodes.len();
        scene.nodes[0].children.push(index);
        scene.nodes.push(node);
    }
    if markers {
        for w in &set.waypoints {
            let (e, n) = proj.project(w.lat, w.lon);
            let y = if w.ele.is_finite() { (w.ele - base) * look.exaggeration } else { 0.0 };
            let label = if w.name.is_empty() { "Waypoint".to_string() } else { w.name.clone() };
            scene.markers.push(Marker { label, position: Vec3::new(e as f32, y, -(n as f32)), color: Vec4::ONE });
        }
    }
    scene
}

/// Collects triangles into primitives of bounded size.
#[derive(Default)]
struct Builder {
    prims: Vec<Primitive>,
    current: Primitive,
}

impl Builder {
    fn room(&mut self, verts: usize) {
        if self.current.positions.len() + verts > PRIMITIVE_VERTICES && !self.current.positions.is_empty() {
            self.prims.push(std::mem::take(&mut self.current));
        }
    }

    /// A flat strip `width` wide along `pts`, facing up.
    fn ribbon(&mut self, pts: &[Vec3], color: impl Fn(usize) -> Vec4, width: f32) {
        if pts.len() < 2 {
            return;
        }
        for chunk in chunks(pts.len()) {
            self.room(chunk.len() * 2);
            let p = &mut self.current;
            let base = p.positions.len() as u32;
            for i in chunk.clone() {
                let d_in = if i > 0 { pts[i] - pts[i - 1] } else { Vec3::ZERO };
                let d_out = if i + 1 < pts.len() { pts[i + 1] - pts[i] } else { Vec3::ZERO };
                let d = Vec3::new(d_in.x + d_out.x, 0.0, d_in.z + d_out.z).normalize_or(Vec3::X);
                let side = d.cross(Vec3::Y) * (width * 0.5);
                for q in [pts[i] - side, pts[i] + side] {
                    p.positions.push(q);
                    p.normals.push(Vec3::Y);
                    p.colors.push(color(i));
                }
            }
            for k in 0..(chunk.len() as u32 - 1) {
                let (a, b) = (base + k * 2, base + k * 2 + 2);
                p.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
    }

    /// A vertical wall from `pts` down to the ground (y = 0).
    fn curtain(&mut self, pts: &[Vec3], top: impl Fn(usize) -> Vec4, bottom: impl Fn(usize) -> Vec4) {
        if pts.len() < 2 {
            return;
        }
        for chunk in chunks(pts.len()) {
            self.room(chunk.len() * 2);
            let p = &mut self.current;
            let base = p.positions.len() as u32;
            for i in chunk.clone() {
                let d_in = if i > 0 { pts[i] - pts[i - 1] } else { Vec3::ZERO };
                let d_out = if i + 1 < pts.len() { pts[i + 1] - pts[i] } else { Vec3::ZERO };
                let d = Vec3::new(d_in.x + d_out.x, 0.0, d_in.z + d_out.z).normalize_or(Vec3::X);
                let normal = d.cross(Vec3::Y).normalize_or(Vec3::Z);
                p.positions.push(pts[i]);
                p.positions.push(Vec3::new(pts[i].x, 0.0, pts[i].z));
                p.normals.extend_from_slice(&[normal, normal]);
                p.colors.push(top(i));
                p.colors.push(bottom(i));
            }
            for k in 0..(chunk.len() as u32 - 1) {
                let (a, b) = (base + k * 2, base + k * 2 + 2);
                p.indices.extend_from_slice(&[a, a + 1, b, b, a + 1, b + 1]);
            }
        }
    }

    fn finish(mut self, material: usize) -> Vec<Primitive> {
        if !self.current.positions.is_empty() {
            self.prims.push(self.current);
        }
        for p in &mut self.prims {
            p.material = Some(material);
        }
        self.prims
    }
}

/// Splits `n` points into overlapping runs (each shares its first point
/// with the last of the one before) that fit a primitive.
fn chunks(n: usize) -> Vec<std::ops::Range<usize>> {
    let max = PRIMITIVE_VERTICES / 2 - 1;
    let mut out = Vec::new();
    let mut start = 0;
    while start + 1 < n {
        let end = (start + max).min(n);
        out.push(start..end);
        start = end - 1;
    }
    out
}
