//! Instanced drawing.  Every draw is one primitive on one node, and every
//! per-draw fact the shaders read (the node's transform, its skin's joint
//! base, its active morph targets) lives in its draw record, so draws of
//! the same primitive differ only by record.  A [`Group`] is the draws of
//! one primitive, and one call draws them all as instances: instance `i`
//! of a call reads record `instances[i]`, a list of draw indices bound
//! beside the records.  A scene that places one tree 100,000 times, as USD
//! point instancers and glTF's `EXT_mesh_gpu_instancing` do, then costs a
//! few calls instead of 100,000 per pass.
//!
//! The list has two halves.  `Renderer::update_pose` fills the first with
//! each group's visible draws, those whose node doesn't mirror first,
//! since mirrored ones need the pipeline with the other winding.  The
//! shadow passes and every group the camera sees whole draw from it.  A
//! group the frustum's edge crosses gets its instances tested one by one
//! each frame ([`Frustum`]), and the survivors are copied into the second
//! half, so off-screen instances cost nothing on the GPU.  Only draws whose
//! box holds every vertex are culled: not skinned or morphed draws, points
//! (their dots reach past their positions), billboards or displaced
//! material balls.
//!
//! Blended and transmissive draws stay one instance per call, since they
//! must be sorted back to front one by one.

use crate::scene_gpu::{DrawItem, GpuScene};
use glam::{Mat4, Vec3, Vec4};
use slerp_scene::{Aabb, Topology};

/*┌─────────────────╮
| Groups          |
└─────────────────╯*/
//region Groups

/// The draws of one primitive, which one instanced call draws.
pub(crate) struct Group {
    pub prim: usize,
    /// Its draws, in scene order.
    pub draws: Vec<u32>,
    /// Where its visible draws start in the instance list, and how many of
    /// them don't mirror and do (listed in that order).
    pub first: u32,
    pub plain: u32,
    pub mirrored: u32,
    /// The visible draws' box in world space.
    pub bounds: Aabb,
    /// Every visible draw is rigid (`DrawItem::rigid`), so `bounds` holds
    /// all their vertices.
    pub rigid: bool,
}

impl Group {
    /// Visible draws, mirrored or not.
    pub fn visible(&self) -> u32 {
        self.plain + self.mirrored
    }
}

/// Groups draws by primitive, in the order of each primitive's first draw,
/// so a scene without repeats keeps its draw order.
pub(crate) fn group_draws(draws: &[DrawItem], prims: usize) -> Vec<Group> {
    let mut group_of = vec![usize::MAX; prims];
    let mut groups: Vec<Group> = Vec::new();
    for (i, d) in draws.iter().enumerate() {
        let Some(slot) = group_of.get_mut(d.prim) else { continue };
        if *slot == usize::MAX {
            *slot = groups.len();
            groups.push(Group {
                prim: d.prim,
                draws: Vec::new(),
                first: 0,
                plain: 0,
                mirrored: 0,
                bounds: Aabb::EMPTY,
                rigid: true,
            });
        }
        groups[*slot].draws.push(i as u32);
    }
    groups
}

/// Lists each group's visible draws in `listed`, unmirrored first, and
/// notes where they are, their box and whether they are all rigid.
pub(crate) fn list_instances(groups: &mut [Group], draws: &[DrawItem], listed: &mut Vec<u32>) {
    listed.clear();
    for g in groups {
        g.first = listed.len() as u32;
        g.bounds = Aabb::EMPTY;
        g.rigid = true;
        for mirrored in [false, true] {
            let start = listed.len();
            for &i in &g.draws {
                let d = &draws[i as usize];
                if d.visible && d.mirrored == mirrored {
                    listed.push(i);
                    g.bounds.union(&d.bounds);
                    g.rigid &= d.rigid;
                }
            }
            let count = (listed.len() - start) as u32;
            if mirrored {
                g.mirrored = count;
            } else {
                g.plain = count;
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Culling         |
└─────────────────╯*/
//region Culling

/// Where a box lies against the view frustum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Outside,
    Inside,
    /// Partly in: its instances are tested one by one.
    Crossing,
}

/// The camera's view volume as six planes, each kept by `dot(n, p) + w >=
/// 0`, from a view-projection matrix whose clip depth runs from 0 to w (as
/// wgpu's does).
pub(crate) struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    /// `None` when the matrix isn't finite, which draws everything.
    pub fn new(view_proj: &Mat4) -> Option<Frustum> {
        if !view_proj.is_finite() {
            return None;
        }
        let r = [0, 1, 2, 3].map(|i| view_proj.row(i));
        let planes = [r[3] + r[0], r[3] - r[0], r[3] + r[1], r[3] - r[1], r[2], r[3] - r[2]];
        // Planes are scaled to unit normals so a box's slack is in world
        // units; a plane without a direction keeps everything.
        let planes = planes.map(|p| {
            let n = p.truncate().length();
            if n > 1e-20 { p / n } else { Vec4::new(0.0, 0.0, 0.0, 1.0) }
        });
        planes.iter().all(|p| p.is_finite()).then_some(Frustum { planes })
    }

    /// Where `b` lies.  An empty or unbounded box counts as inside, and a
    /// box is grown by a hair first, so rounding never drops anything the
    /// GPU would have drawn.
    pub fn side(&self, b: &Aabb) -> Side {
        if b.is_empty() || !b.min.is_finite() || !b.max.is_finite() {
            return Side::Inside;
        }
        let slack = Vec3::splat(b.size().length() * 1e-4 + b.center().abs().max_element() * 1e-5);
        let (lo, hi) = (b.min - slack, b.max + slack);
        let mut inside = true;
        for p in &self.planes {
            let n = p.truncate();
            let ahead = n.cmpge(Vec3::ZERO);
            // The corner farthest along the plane's normal, and the one
            // farthest against it.
            if n.dot(Vec3::select(ahead, hi, lo)) + p.w < 0.0 {
                return Side::Outside;
            }
            if n.dot(Vec3::select(ahead, lo, hi)) + p.w < 0.0 {
                inside = false;
            }
        }
        if inside { Side::Inside } else { Side::Crossing }
    }
}

//endregion

/*┌─────────────────╮
| Frame           |
└─────────────────╯*/
//region Frame

/// One draw call: `count` instances of primitive `prim`, whose draw
/// indices are listed from `first` in the instance list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub prim: usize,
    pub first: u32,
    pub count: u32,
    pub mirrored: bool,
    /// The group's place in the scene, for drawing in scene order.
    pub order: u32,
}

/// What a frame draws, sorted for drawing: opaque surfaces and points by
/// material, then transmissive and blended surfaces farthest first.
#[derive(Default)]
pub(crate) struct FrameRuns {
    pub opaque: Vec<Run>,
    pub transmissive: Vec<Run>,
    pub blended: Vec<Run>,
    /// Draw indices for the second half of the instance list: the
    /// instances of crossing groups that the camera sees.
    pub culled: Vec<u32>,
    /// A visible opaque surface is lit by the environment (or a point), so
    /// ambient occlusion has something to darken; and some visible surface
    /// is transmissive.  Counted before culling, so a frame's settings
    /// don't depend on what is off screen.
    pub occludable: bool,
    pub any_transmissive: bool,
}

impl FrameRuns {
    /// Sorts the scene's visible draws into runs, culling against `frustum`
    /// (`view` gives depths for sorting).
    pub fn build(&mut self, scene: &GpuScene, view: &Mat4, frustum: Option<&Frustum>) {
        self.opaque.clear();
        self.culled.clear();
        self.occludable = false;
        self.any_transmissive = false;
        let mut transmissive = Vec::new();
        let mut blended = Vec::new();
        // The second half of the list starts after room for every draw.
        let culled_base = scene.draws.len() as u32;
        for (order, g) in scene.groups.iter().enumerate() {
            if g.visible() == 0 {
                continue;
            }
            let order = order as u32;
            let prim = &scene.prims[g.prim];
            let mat = &scene.materials[prim.material];
            let points = prim.topology == Topology::Points;
            let cullable = g.rigid && !points && !mat.moves_vertices;
            let side = match frustum {
                Some(f) if cullable => f.side(&g.bounds),
                _ => Side::Inside,
            };
            let listed = &scene.listed[g.first as usize..(g.first + g.visible()) as usize];
            let seen = |i: u32| {
                side != Side::Crossing
                    || frustum.is_none_or(|f| f.side(&scene.draws[i as usize].bounds) != Side::Outside)
            };
            if (mat.transmissive || mat.blend) && !points {
                // 1. One instance per call, each placed by its centre.
                self.any_transmissive |= mat.transmissive;
                if side == Side::Outside {
                    continue;
                }
                let list = if mat.transmissive { &mut transmissive } else { &mut blended };
                for (k, &i) in listed.iter().enumerate() {
                    if !seen(i) {
                        continue;
                    }
                    let d = &scene.draws[i as usize];
                    // View-space z is negative in front of the camera.
                    let depth = (*view * d.center.extend(1.0)).z;
                    let run = Run { prim: g.prim, first: g.first + k as u32, count: 1, mirrored: d.mirrored, order };
                    list.push((depth, run));
                }
                continue;
            }
            // 2. Opaque: the whole group, its unmirrored and mirrored
            //    draws in a call each, from the first half of the list when
            //    the camera sees all of it, else the survivors copied into
            //    the second.
            self.occludable |= points || (mat.lit && prim.topology == Topology::Triangles);
            match side {
                Side::Outside => {}
                Side::Inside => {
                    for (first, count, mirrored) in [(g.first, g.plain, false), (g.first + g.plain, g.mirrored, true)] {
                        if count > 0 {
                            self.opaque.push(Run { prim: g.prim, first, count, mirrored, order });
                        }
                    }
                }
                Side::Crossing => {
                    let (plain, mirrored) = listed.split_at(g.plain as usize);
                    for (part, mirrored) in [(plain, false), (mirrored, true)] {
                        let first = culled_base + self.culled.len() as u32;
                        self.culled.extend(part.iter().copied().filter(|&i| seen(i)));
                        let count = culled_base + self.culled.len() as u32 - first;
                        if count > 0 {
                            self.opaque.push(Run { prim: g.prim, first, count, mirrored, order });
                        }
                    }
                }
            }
        }
        // Few materials, then scene order, as before instancing: draws
        // sharing a material run together and the rest keeps its order.
        self.opaque.sort_by_key(|r| (scene.prims[r.prim].material, r.order, r.mirrored));
        // Farthest first; ties keep scene order.
        for (list, out) in [(&mut transmissive, &mut self.transmissive), (&mut blended, &mut self.blended)] {
            list.sort_by(|a, b| a.0.total_cmp(&b.0));
            out.clear();
            out.extend(list.iter().map(|(_, r)| *r));
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(prim: usize, visible: bool, mirrored: bool) -> DrawItem {
        DrawItem {
            node: 0,
            prim,
            skin: None,
            visible,
            sky: false,
            mirrored,
            center: Vec3::ZERO,
            bounds: Aabb { min: Vec3::ZERO, max: Vec3::ONE },
            rigid: true,
        }
    }

    /// Draws of one primitive share a group whatever their nodes; other
    /// primitives (another material, or a skinned mesh's own primitive) get
    /// their own, in the order they first appear.
    #[test]
    fn draws_of_one_primitive_group_together() {
        let draws = [draw(2, true, false), draw(0, true, false), draw(2, true, false), draw(1, true, false)];
        let groups = group_draws(&draws, 3);
        let shape: Vec<(usize, Vec<u32>)> = groups.iter().map(|g| (g.prim, g.draws.clone())).collect();
        assert_eq!(shape, [(2, vec![0, 2]), (0, vec![1]), (1, vec![3])]);
    }

    /// Hidden draws are left out of the list, mirrored ones come after the
    /// rest of their group, and a skinned draw makes its group unrigid.
    #[test]
    fn listing_skips_hidden_and_orders_mirrored_last() {
        let mut draws = vec![
            draw(0, true, true),
            draw(0, false, false),
            draw(0, true, false),
            draw(1, false, false),
            draw(0, true, false),
        ];
        draws[4].rigid = false;
        draws[4].bounds = Aabb { min: Vec3::splat(-2.0), max: Vec3::ZERO };
        let mut groups = group_draws(&draws, 2);
        let mut listed = Vec::new();
        list_instances(&mut groups, &draws, &mut listed);
        assert_eq!(listed, [2, 4, 0]);
        assert_eq!((groups[0].first, groups[0].plain, groups[0].mirrored), (0, 2, 1));
        assert_eq!(groups[0].bounds, Aabb { min: Vec3::splat(-2.0), max: Vec3::ONE });
        assert!(!groups[0].rigid);
        assert_eq!((groups[1].first, groups[1].visible()), (3, 0));
    }

    /// A camera at the origin looking down -Z: boxes in front are inside,
    /// behind or far to the side outside, and across the edge crossing.
    #[test]
    fn frustum_sides() {
        let proj = glam::camera::rh::proj::directx::perspective(60f32.to_radians(), 1.0, 0.1, 100.0);
        let f = Frustum::new(&proj).unwrap();
        let cube =
            |x: f32, z: f32| Aabb { min: Vec3::new(x - 0.5, -0.5, z - 0.5), max: Vec3::new(x + 0.5, 0.5, z + 0.5) };
        assert_eq!(f.side(&cube(0.0, -10.0)), Side::Inside);
        assert_eq!(f.side(&cube(0.0, 10.0)), Side::Outside);
        assert_eq!(f.side(&cube(50.0, -10.0)), Side::Outside);
        assert_eq!(f.side(&cube(0.0, -200.0)), Side::Outside);
        assert_eq!(f.side(&cube(5.77, -10.0)), Side::Crossing);
        assert_eq!(f.side(&Aabb::EMPTY), Side::Inside);
        assert!(Frustum::new(&Mat4::from_cols_array(&[f32::NAN; 16])).is_none());
    }
}
