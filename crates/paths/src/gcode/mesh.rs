//! A toolpath as something to look at: each extrusion a thin tube with the
//! bead's real width and height (a four-sided cross-section with rounded
//! shading, as slicers' previews draw it), coloured by feature type, so a
//! thumbnail of a print looks like the print.  Travel moves are lines and
//! hidden by default.
//!
//! The viewer gets one node per layer, so a layer slider only switches
//! nodes on and off.  Thumbnails get one node, without the features hidden
//! behind outer walls and top surfaces, and with every other layer (or
//! fewer) when even that is more than a thumbnail can afford: each layer
//! kept is drawn as thick as the layers it stands for, so walls stay
//! solid.
//!
//! Straight runs of short moves (arcs and curves are written as hundreds of
//! them) are merged within a tolerance, a fraction of a pixel for
//! thumbnails.

use super::{Feature, Move, MoveKind, Toolpath};
use glam::{Vec3, Vec4};
use rayon::prelude::*;
use slerp_scene::{Info, Material, Mesh, Node, Primitive, Scene, Topology};

/// Millimetres to the scene's metres.
const SCALE: f32 = 0.001;
/// Segments the viewer draws at most before merging harder, then dropping
/// layers: about half a gigabyte of vertices.
const VIEWER_SEGMENTS: usize = 2_500_000;
/// Segments a thumbnail draws at most.
const THUMBNAIL_SEGMENTS: usize = 250_000;
/// Vertices per primitive, well inside a storage binding.
const PRIMITIVE_VERTICES: usize = 1 << 20;
/// Runs bend at most this much at a shared ring; sharper corners start a
/// new tube, so corners stay crisp rather than pinched.
const MAX_BEND_COS: f32 = 0.70;
const TRAVEL_COLOR: Vec4 = Vec4::new(0.05, 0.25, 0.95, 1.0);

/// What to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolpathLook {
    /// Shown features, indexed by `Feature::index`.
    pub shown: [bool; Feature::COUNT],
    pub travel: bool,
}

impl ToolpathLook {
    /// Everything but travel moves, as the viewer opens a file.
    pub fn all() -> ToolpathLook {
        ToolpathLook { shown: [true; Feature::COUNT], travel: false }
    }

    /// What a thumbnail needs: outer walls cover inner walls and infill,
    /// top surfaces cover solid infill, when the slicer says which is which.
    pub fn outside(tp: &Toolpath) -> ToolpathLook {
        let mut look = ToolpathLook::all();
        if tp.has_feature(Feature::OuterWall) {
            look.shown[Feature::InnerWall.index()] = false;
            look.shown[Feature::SparseInfill.index()] = false;
        }
        if tp.has_feature(Feature::TopSurface) {
            look.shown[Feature::SolidInfill.index()] = false;
        }
        // Start G-code's purge lines sit far from the print and would
        // shrink it in the picture.
        if Feature::ALL.iter().any(|&f| f != Feature::Custom && tp.has_feature(f)) {
            look.shown[Feature::Custom.index()] = false;
        }
        look
    }
}

/// Who the scene is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// One node per layer, everything kept.
    Viewer,
    /// One node, merged to a fraction of a pixel at this size.
    Thumbnail(u32),
}

/// One point of a tube's centre line, in millimetres, with the bead's
/// width there.
#[derive(Clone, Copy, Debug)]
struct RunPoint {
    p: Vec3,
    w: f32,
}

/// The vertices and indices being filled for one layer.
#[derive(Default)]
struct Geometry {
    prims: Vec<Primitive>,
    current: Primitive,
    travel: Vec<Vec3>,
}

struct Ctx<'a> {
    tp: &'a Toolpath,
    look: &'a ToolpathLook,
    /// The XY centre of what is drawn, in millimetres.
    centre: [f32; 2],
    /// Merging tolerance in millimetres.
    tolerance: f32,
    /// A CNC cut's width and height, from the size of the job.
    cut_width: f32,
}

/// Builds the scene of `tp` as `look` says.  For the viewer, node `i` is
/// layer `i` (a CNC program has one).
pub fn toolpath_scene(tp: &Toolpath, look: &ToolpathLook, target: Target) -> Scene {
    let extent = (0..2).map(|i| tp.max[i] - tp.min[i]).fold(0.0f32, f32::max);
    let extent = if extent.is_finite() && extent > 0.0 { extent } else { 100.0 };
    let centre =
        if tp.is_empty() { [0.0, 0.0] } else { [(tp.min[0] + tp.max[0]) * 0.5, (tp.min[1] + tp.max[1]) * 0.5] };
    let mut ctx = Ctx { tp, look, centre, tolerance: 0.002, cut_width: (extent * 0.004).clamp(0.05, 5.0) };
    let mut warnings = tp.warnings.clone();

    // 1. Decide how much detail fits: merge harder, then keep every
    //    `stride`th layer.
    let (budget, tolerances): (usize, Vec<f32>) = match target {
        Target::Viewer => (VIEWER_SEGMENTS, vec![0.002, 0.01, 0.03, 0.08]),
        Target::Thumbnail(size) => (THUMBNAIL_SEGMENTS, vec![extent / size.max(16) as f32 * 0.5]),
    };
    let heights: Vec<f32> = (0..tp.layers.len()).map(|li| ctx.height(li)).collect();
    let mut total = 0;
    for &t in &tolerances {
        ctx.tolerance = t;
        total = (0..tp.layers.len()).into_par_iter().map(|li| ctx.count(li, heights[li])).sum::<usize>();
        if total <= budget {
            break;
        }
    }
    // A toolpath thinned while it was read is already as sparse as it gets.
    let stride = if tp.thinned > 1 || tp.sampled { 1 } else { total.div_ceil(budget.max(1)).max(1) };
    if stride > 1 && target == Target::Viewer {
        warnings.push(format!("only every {stride}th layer is drawn: the toolpath is too large to show whole"));
    }
    let last = tp.layers.len().saturating_sub(1);
    let kept = |li: usize| (last - li).is_multiple_of(stride);
    let thickness = |li: usize| {
        if tp.sampled {
            // Each sampled layer stands for the gap below it.
            let below = if li > 0 { tp.layers[li - 1].z } else { 0.0 };
            return (tp.layers[li].z - below).max(heights[li]);
        }
        if tp.thinned > 1 {
            return heights[li] * tp.thinned as f32;
        }
        if stride == 1 {
            return heights[li];
        }
        let below = li.checked_sub(stride).map(|b| tp.layers[b].z);
        match below {
            Some(z) if tp.layers[li].z - z > 0.0 => tp.layers[li].z - z,
            _ => heights[li] * stride as f32,
        }
    };

    // 2. The layers' geometry, side by side.
    let layers: Vec<Geometry> = (0..tp.layers.len())
        .into_par_iter()
        .map(|li| if kept(li) { ctx.build(li, thickness(li)) } else { Geometry::default() })
        .collect();

    // 3. The scene.
    let mut scene = Scene {
        name: "Toolpath".into(),
        materials: vec![
            Material { name: "Extrusion".into(), base_color: Vec4::ONE, roughness: 0.55, ..Default::default() },
            Material { name: "Travel".into(), base_color: Vec4::ONE, unlit: true, ..Default::default() },
        ],
        info: Info {
            format: tp.format.clone(),
            generator: tp.meta.generator.clone(),
            details: tp.details(),
            warnings,
            ..Default::default()
        },
        ..Default::default()
    };
    let add_node = |scene: &mut Scene, name: String, prims: Vec<Primitive>| {
        let mut node = Node::new(name.clone());
        if !prims.is_empty() {
            node.mesh = Some(scene.meshes.len());
            scene.meshes.push(Mesh { name, primitives: prims, target_names: Vec::new() });
        }
        scene.roots.push(scene.nodes.len());
        scene.nodes.push(node);
    };
    match target {
        Target::Viewer => {
            for (li, g) in layers.into_iter().enumerate() {
                let name = if tp.cnc {
                    "Toolpath".to_string()
                } else {
                    format!("Layer {} · {:.2} mm", li + 1, tp.layers[li].z)
                };
                add_node(&mut scene, name, g.into_primitives());
            }
        }
        Target::Thumbnail(_) => {
            let prims: Vec<Primitive> = layers.into_iter().flat_map(Geometry::into_primitives).collect();
            add_node(&mut scene, "Toolpath".into(), merge(prims));
        }
    }
    scene
}

/// Joins small primitives up to the vertex limit, so a thumbnail draws a
/// few large batches rather than one per layer.
fn merge(prims: Vec<Primitive>) -> Vec<Primitive> {
    let mut out: Vec<Primitive> = Vec::new();
    for p in prims {
        match out.last_mut() {
            Some(last)
                if last.topology == p.topology
                    && last.material == p.material
                    && last.positions.len() + p.positions.len() <= PRIMITIVE_VERTICES =>
            {
                let base = last.positions.len() as u32;
                last.positions.extend_from_slice(&p.positions);
                last.normals.extend_from_slice(&p.normals);
                last.colors.extend_from_slice(&p.colors);
                last.indices.extend(p.indices.iter().map(|i| i + base));
            }
            _ => out.push(p),
        }
    }
    out
}

impl Geometry {
    fn into_primitives(mut self) -> Vec<Primitive> {
        if !self.current.positions.is_empty() {
            self.prims.push(std::mem::take(&mut self.current));
        }
        if !self.travel.is_empty() {
            let n = self.travel.len() as u32;
            self.prims.push(Primitive {
                positions: std::mem::take(&mut self.travel),
                colors: vec![TRAVEL_COLOR; n as usize],
                indices: (0..n).collect(),
                topology: Topology::Lines,
                material: Some(1),
                ..Default::default()
            });
        }
        self.prims
    }
}

impl Ctx<'_> {
    /// Whether a move is drawn as a tube.
    fn drawn(&self, m: &Move) -> bool {
        match m.kind {
            MoveKind::Extrude => self.look.shown[m.feature.index()],
            MoveKind::Feed => self.tp.cnc && self.look.shown[Feature::Cut.index()],
            _ => false,
        }
    }

    fn height(&self, li: usize) -> f32 {
        if self.tp.cnc { self.cut_width } else { self.tp.layers[li].height.clamp(0.02, 2.0) }
    }

    fn width(&self, m: &Move, h: f32) -> f32 {
        if self.tp.cnc { self.cut_width } else { self.tp.width_of(m, h) }
    }

    /// Walks layer `li`'s drawn moves as runs of tube: consecutive moves of
    /// one feature and width, without sharp bends, merged where straight.
    fn walk(&self, li: usize, h: f32, emit: &mut impl FnMut(&[RunPoint], Feature)) {
        let tp = self.tp;
        let mut run: Vec<RunPoint> = Vec::new();
        let mut feature = Feature::Other;
        let tol2 = self.tolerance * self.tolerance;
        for i in tp.layer_range(li) {
            let m = tp.moves[i];
            if i == 0 || !self.drawn(&m) {
                if run.len() >= 2 {
                    emit(&run, feature);
                }
                run.clear();
                continue;
            }
            let a = Vec3::from(tp.points[i - 1]);
            let b = Vec3::from(tp.points[i]);
            if a == b {
                continue;
            }
            let w = self.width(&m, h);
            let f = if tp.cnc { Feature::Cut } else { m.feature };
            let continues = run.last().is_some_and(|l| l.p == a && f == feature && (l.w - w).abs() <= 0.2 * w);
            let bends = continues && run.len() >= 2 && {
                let n = run.len();
                let d0 = (run[n - 1].p - run[n - 2].p).normalize_or_zero();
                d0.dot((b - a).normalize_or_zero()) < MAX_BEND_COS
            };
            if !continues || bends {
                if run.len() >= 2 {
                    emit(&run, feature);
                }
                run.clear();
                run.push(RunPoint { p: a, w });
                feature = f;
            } else if run.len() >= 2 {
                // Drop the middle point when it lies on the line from the
                // one before to the new one.
                let n = run.len();
                let (p0, p1) = (run[n - 2].p, run[n - 1].p);
                if (p1 - p0).dot(b - p1) > 0.0 && distance2_to_segment(p1, p0, b) <= tol2 {
                    run.pop();
                }
            }
            run.push(RunPoint { p: b, w });
        }
        if run.len() >= 2 {
            emit(&run, feature);
        }
    }

    /// The number of tube segments layer `li` draws.
    fn count(&self, li: usize, h: f32) -> usize {
        let mut n = 0;
        self.walk(li, h, &mut |run, _| n += run.len() - 1);
        n
    }

    /// Layer `li`'s tubes, `h` thick, and its travel lines if shown.
    fn build(&self, li: usize, h: f32) -> Geometry {
        let mut g = Geometry::default();
        let width_h = self.height(li);
        self.walk(li, width_h, &mut |run, feature| self.tube(run, feature, h, &mut g));
        if self.look.travel && !self.tp.cnc {
            let tp = self.tp;
            for i in tp.layer_range(li) {
                let m = tp.moves[i];
                let travel = matches!(m.kind, MoveKind::Travel | MoveKind::Feed);
                if i > 0 && travel {
                    g.travel.push(self.to_scene(Vec3::from(tp.points[i - 1])));
                    g.travel.push(self.to_scene(Vec3::from(tp.points[i])));
                }
            }
        } else if self.look.travel {
            // A CNC program's rapid moves.
            let tp = self.tp;
            for i in tp.layer_range(li) {
                if i > 0 && tp.moves[i].kind == MoveKind::Travel {
                    g.travel.push(self.to_scene(Vec3::from(tp.points[i - 1])));
                    g.travel.push(self.to_scene(Vec3::from(tp.points[i])));
                }
            }
        }
        g
    }

    /// Machine millimetres (Z up) to scene metres (Y up), centred.
    fn to_scene(&self, p: Vec3) -> Vec3 {
        Vec3::new((p.x - self.centre[0]) * SCALE, p.z * SCALE, -(p.y - self.centre[1]) * SCALE)
    }

    /// Adds a tube along `run`: a ring of four vertices (top, right,
    /// bottom, left of the bead) at each point, joined by quads, capped at
    /// both ends.  The nozzle rides on top of the bead, so the bead's
    /// centre is half its height below the path.  Normals point out from
    /// the bead's centre, which shades the diamond as a rounded bead.
    fn tube(&self, run: &[RunPoint], feature: Feature, h: f32, g: &mut Geometry) {
        let n = run.len();
        if n < 2 {
            return;
        }
        if g.current.positions.len() + n * 4 > PRIMITIVE_VERTICES && !g.current.positions.is_empty() {
            g.prims.push(std::mem::take(&mut g.current));
        }
        let prim = &mut g.current;
        if prim.positions.is_empty() {
            prim.material = Some(0);
        }
        let color = feature.linear();
        let pts: Vec<Vec3> = run.iter().map(|r| self.to_scene(r.p)).collect();
        let base = prim.positions.len() as u32;
        let hh = h * SCALE * 0.5;
        for k in 0..n {
            let d_in = if k > 0 { (pts[k] - pts[k - 1]).normalize_or_zero() } else { Vec3::ZERO };
            let d_out = if k + 1 < n { (pts[k + 1] - pts[k]).normalize_or_zero() } else { Vec3::ZERO };
            let dir = (d_in + d_out).normalize_or(Vec3::X);
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::Z);
            let up = side.cross(dir).normalize_or(Vec3::Y);
            let hw = run[k].w * SCALE * 0.5;
            let centre = pts[k] - Vec3::Y * hh;
            for (offset, normal) in [(up * hh, up), (side * hw, side), (-up * hh, -up), (-side * hw, -side)] {
                prim.positions.push(centre + offset);
                prim.normals.push(normal);
                prim.colors.push(color);
            }
        }
        for k in 0..(n as u32 - 1) {
            let a = base + k * 4;
            let b = a + 4;
            for j in 0..4 {
                let (a0, a1, b0, b1) = (a + j, a + (j + 1) % 4, b + j, b + (j + 1) % 4);
                prim.indices.extend_from_slice(&[a0, a1, b1, a0, b1, b0]);
            }
        }
        let e = base + (n as u32 - 1) * 4;
        prim.indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        prim.indices.extend_from_slice(&[e, e + 1, e + 2, e, e + 2, e + 3]);
    }
}

/// The squared distance from `p` to the segment `a`-`b`.
fn distance2_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= 0.0 {
        return (p - a).length_squared();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (a + ab * t - p).length_squared()
}
