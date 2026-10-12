//! Skin weights refitted to four joints per vertex.
//!
//! A MetaHuman head blends up to twelve joints per vertex; Slerp's
//! renderer, like glTF, blends four.  Keeping the strongest four and
//! rescaling them drops up to half of a vertex's weight around the mouth,
//! which put the lips several millimetres off when the jaw opened.  So for
//! every vertex with more than four joints, the strongest four are kept
//! and their weights refitted by least squares to reproduce where all of
//! its joints put it, over poses RigLogic makes from each face control at
//! full strength.  The fit runs once per level of detail when the viewer
//! builds the rig (about a quarter of a second for a MetaHuman head), and
//! is kept in the cache (see `cache`), so later opens only apply it.

use crate::extract::DnaData;
use crate::rig::Rig;
use crate::scene::{MeshSource, Units};
use glam::{DMat4, DVec3, Vec4};
use rayon::prelude::*;
use slerp_scene::{Pose, Scene};

/// How well the four refitted joints follow all of them, over the poses
/// sampled: mean and largest distances, in metres, before and after.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FitReport {
    pub vertices: usize,
    pub poses: usize,
    pub mean_before: f64,
    pub max_before: f64,
    pub mean_after: f64,
    pub max_after: f64,
}

/// Refitted joints and weights of the head's vertices that have more than
/// four joints, by DNA mesh and vertex: what [`refit`] works out and
/// [`apply`] puts into a scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Fit {
    pub meshes: Vec<(u16, Vec<Refitted>)>,
    pub report: FitReport,
}

/// A DNA vertex's four joints and their weights.
pub(crate) type Refitted = (u32, [u16; 4], [f32; 4]);

impl Fit {
    /// The fit as bytes, for the cache.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let r = &self.report;
        out.extend_from_slice(&(r.vertices as u64).to_le_bytes());
        out.extend_from_slice(&(r.poses as u64).to_le_bytes());
        for v in [r.mean_before, r.max_before, r.mean_after, r.max_after] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.meshes.len() as u32).to_le_bytes());
        for (mesh, vertices) in &self.meshes {
            out.extend_from_slice(&mesh.to_le_bytes());
            out.extend_from_slice(&(vertices.len() as u32).to_le_bytes());
            for (p, j, w) in vertices {
                out.extend_from_slice(&p.to_le_bytes());
                j.iter().for_each(|j| out.extend_from_slice(&j.to_le_bytes()));
                w.iter().for_each(|w| out.extend_from_slice(&w.to_le_bytes()));
            }
        }
        out
    }

    /// Reads [`Fit::to_bytes`] back, `None` when the bytes don't add up.
    pub fn from_bytes(bytes: &[u8]) -> Option<Fit> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = bytes.get(at..at.checked_add(n)?)?;
            at += n;
            Some(s)
        };
        let u64 = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap_or([0; 8]));
        let mut fit = Fit::default();
        fit.report.vertices = u64(take(8)?) as usize;
        fit.report.poses = u64(take(8)?) as usize;
        let mut f64s = [0f64; 4];
        for v in &mut f64s {
            *v = f64::from_le_bytes(take(8)?.try_into().ok()?);
        }
        [fit.report.mean_before, fit.report.max_before, fit.report.mean_after, fit.report.max_after] = f64s;
        let meshes = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        for _ in 0..meshes {
            let mesh = u16::from_le_bytes(take(2)?.try_into().ok()?);
            let count = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            // Each vertex takes 28 bytes; a count the bytes can't hold is
            // damage, refused before allocating for it.
            if count.checked_mul(28)? > bytes.len() {
                return None;
            }
            let mut vertices = Vec::with_capacity(count);
            for _ in 0..count {
                let s = take(28)?;
                let p = u32::from_le_bytes(s[0..4].try_into().ok()?);
                let j = [0, 1, 2, 3].map(|i| u16::from_le_bytes([s[4 + i * 2], s[5 + i * 2]]));
                let w =
                    [0, 1, 2, 3].map(|i| f32::from_le_bytes(s[12 + i * 4..16 + i * 4].try_into().unwrap_or([0; 4])));
                vertices.push((p, j, w));
            }
            fit.meshes.push((mesh, vertices));
        }
        (at == bytes.len()).then_some(fit)
    }
}

/// Puts a fit into the head meshes of `scene`, built from the data it was
/// worked out for (at the same level of detail).
pub(crate) fn apply(scene: &mut Scene, sources: &[MeshSource], fit: &Fit) {
    for source in sources.iter().filter(|s| s.main) {
        let Some((_, vertices)) = fit.meshes.iter().find(|(m, _)| *m == source.dna_mesh) else { continue };
        let by_vertex: std::collections::HashMap<u32, ([u16; 4], [f32; 4])> =
            vertices.iter().map(|(p, j, w)| (*p, (*j, *w))).collect();
        let Some(mesh) = scene.meshes.get_mut(source.mesh) else { continue };
        for (prim, positions) in mesh.primitives.iter_mut().zip(&source.positions) {
            if !prim.is_skinned() || prim.joints.len() != positions.len() {
                continue;
            }
            for (i, p) in positions.iter().enumerate() {
                if let Some((j, w)) = by_vertex.get(p) {
                    prim.joints[i] = *j;
                    prim.weights[i] = Vec4::from_array(*w);
                }
            }
        }
    }
}

/// Works out the head meshes' refitted skin weights for `scene` (see the
/// module notes).  `rig` is the head's; its control values are left at
/// zero.
pub(crate) fn refit(scene: &Scene, sources: &[MeshSource], data: &DnaData, rig: &mut Rig) -> Fit {
    // 1. The head's skin, and its joints' skinning matrices in each pose.
    let Some(skin) = sources
        .iter()
        .filter(|s| s.main)
        .find_map(|s| scene.nodes.iter().find(|n| n.mesh == Some(s.mesh)).and_then(|n| n.skin))
    else {
        return Fit::default();
    };
    let controls: Vec<usize> = (0..rig.values.len()).filter(|&c| !rig.is_joint_input(c)).collect();
    let mut poses: Vec<Vec<DMat4>> = Vec::with_capacity(controls.len());
    let mut pose = Pose::rest(scene);
    for &c in &controls {
        rig.values.iter_mut().for_each(|v| *v = 0.0);
        rig.values[c] = 1.0;
        pose.reset(scene);
        if rig.apply_joints(&mut pose).is_err() {
            continue;
        }
        pose.update_world(scene);
        poses.push(pose.joint_matrices(scene, skin).iter().map(|m| m.as_dmat4()).collect());
    }
    rig.values.iter_mut().for_each(|v| *v = 0.0);
    if poses.is_empty() {
        return Fit::default();
    }
    let joints = poses[0].len();
    // Which joints each pose moves, so a vertex skips poses that leave all
    // of its joints alone.
    let moved: Vec<Vec<bool>> =
        poses.iter().map(|p| p.iter().map(|m| !m.abs_diff_eq(DMat4::IDENTITY, 1e-9)).collect()).collect();

    // 2. Each head mesh's positions with more than four joints, refitted.
    let units = Units::of(data);
    let mut report = FitReport { poses: poses.len(), ..Default::default() };
    let mut meshes = Vec::new();
    let (mut sum_before, mut sum_after) = (0f64, 0f64);
    for source in sources.iter().filter(|s| s.main) {
        let Some((_, m)) = data.meshes.iter().find(|(i, _)| *i == source.dna_mesh) else { continue };
        let count = m.positions.len() / 3;
        let mut offsets = Vec::with_capacity(count + 1);
        let mut at = 0usize;
        for &k in m.skin_counts.iter().take(count) {
            offsets.push(at);
            at = at.saturating_add(k as usize);
        }
        offsets.push(at);
        if offsets.len() <= count || at > m.skin_joints.len() || at > m.skin_weights.len() {
            continue;
        }
        let fitted: Vec<Option<Fitted>> = (0..count)
            .into_par_iter()
            .map(|p| {
                let influences: Vec<(usize, f64)> = (offsets[p]..offsets[p + 1])
                    .map(|i| (m.skin_joints[i] as usize, m.skin_weights[i] as f64))
                    .filter(|&(j, w)| j < joints && w.is_finite() && w > 0.0)
                    .collect();
                if influences.len() <= 4 {
                    return None;
                }
                let v =
                    DVec3::new(m.positions[p * 3] as f64, m.positions[p * 3 + 1] as f64, m.positions[p * 3 + 2] as f64)
                        * units.metres as f64;
                fit_vertex(v, &influences, &poses, &moved)
            })
            .collect();

        // 3. Kept by DNA vertex, for `apply`.
        let kept = fitted
            .iter()
            .enumerate()
            .filter_map(|(p, f)| f.as_ref().map(|f| (p as u32, f.joints, f.weights.to_array())))
            .collect();
        meshes.push((source.dna_mesh, kept));
        for f in fitted.iter().flatten() {
            report.vertices += 1;
            sum_before += f.mean_before;
            sum_after += f.mean_after;
            report.max_before = report.max_before.max(f.max_before);
            report.max_after = report.max_after.max(f.max_after);
        }
    }
    if report.vertices > 0 {
        report.mean_before = sum_before / report.vertices as f64;
        report.mean_after = sum_after / report.vertices as f64;
    }
    Fit { meshes, report }
}

struct Fitted {
    joints: [u16; 4],
    weights: Vec4,
    mean_before: f64,
    max_before: f64,
    mean_after: f64,
    max_after: f64,
}

/// Influences considered per vertex, the strongest first.
const CANDIDATES: usize = 12;

/// Chooses four of `influences` (joint, weight) and their weights so they
/// put rest position `v` where all of them do, over `poses`.  The
/// strongest joint and every choice of up to three more of the strongest
/// twelve are tried, since a weak joint that moves far (a lip's) can
/// matter more than a strong one that barely moves.
/// The weights sum to one and none is negative.
fn fit_vertex(v: DVec3, influences: &[(usize, f64)], poses: &[Vec<DMat4>], moved: &[Vec<bool>]) -> Option<Fitted> {
    let mut sorted = influences.to_vec();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let cand: Vec<usize> = sorted.iter().take(CANDIDATES).map(|x| x.0).collect();
    let k = cand.len();

    // 1. Over the poses that move any of its joints: how far each candidate
    //    joint moves the vertex, and how far all of them do.  Gram sums of
    //    those make every choice's least squares a tiny solve.
    let mut gram = [[0f64; CANDIDATES]; CANDIDATES];
    let mut cross = [0f64; CANDIDATES];
    let mut target_sq = 0f64;
    let mut poses_used = 0usize;
    let mut samples: Vec<([DVec3; CANDIDATES], DVec3)> = Vec::new();
    for (pose, moves) in poses.iter().zip(moved) {
        if !influences.iter().any(|&(j, _)| moves[j]) {
            continue;
        }
        let target = influences.iter().fold(DVec3::ZERO, |acc, &(j, w)| acc + (pose[j].transform_point3(v) - v) * w);
        let mut each = [DVec3::ZERO; CANDIDATES];
        for (i, &j) in cand.iter().enumerate() {
            each[i] = pose[j].transform_point3(v) - v;
        }
        for i in 0..k {
            cross[i] += each[i].dot(target);
            for j in 0..k {
                gram[i][j] += each[i].dot(each[j]);
            }
        }
        target_sq += target.length_squared();
        poses_used += 1;
        samples.push((each, target));
    }

    // 2. The rescaled strongest four, as a baseline.
    let total: f64 = sorted.iter().take(4).map(|x| x.1).sum();
    let start: Vec<(usize, f64)> = (0..k.min(4)).map(|i| (i, sorted[i].1 / total)).collect();
    let error = |w: &[(usize, f64)]| -> (f64, f64) {
        let (mut sum, mut max) = (0f64, 0f64);
        for (each, target) in &samples {
            let p = w.iter().fold(DVec3::ZERO, |acc, &(i, x)| acc + each[i] * x);
            let d = (p - *target).length();
            sum += d;
            max = max.max(d);
        }
        (sum / samples.len().max(1) as f64, max)
    };
    let (mean_before, max_before) = error(&start);

    // 3. Every subset of up to four candidates that keeps the strongest
    //    (which holds the vertex where nothing moves it), weights by the
    //    constrained least squares; the smallest residual with no negative
    //    weight wins.
    let residual = |w: &[(usize, f64)]| -> f64 {
        let mut r = target_sq;
        for &(i, a) in w {
            r -= 2.0 * a * cross[i];
            for &(j, b) in w {
                r += a * b * gram[i][j];
            }
        }
        r
    };
    let mut best = (residual(&start), start.clone());
    if poses_used > 0 {
        let mut subset = vec![0];
        let mut consider = |s: &[usize]| {
            if let Some(w) = solve(&gram, &cross, s)
                && w.iter().all(|&(_, x)| x >= -1e-9)
            {
                let r = residual(&w);
                if r < best.0 - 1e-18 {
                    best = (r, w);
                }
            }
        };
        consider(&subset);
        subsets(k, 4, 1, &mut subset, &mut consider);
    }
    let weights = best.1;
    let (mean_after, max_after) = error(&weights);
    let mut joints = [0u16; 4];
    let mut w4 = [0f32; 4];
    for (slot, &(i, x)) in weights.iter().enumerate() {
        joints[slot] = cand[i] as u16;
        w4[slot] = x.max(0.0) as f32;
    }
    Some(Fitted { joints, weights: Vec4::from(w4), mean_before, max_before, mean_after, max_after })
}

/// Calls `f` with `current` extended by every non-empty subset of
/// `from..n`, up to `most` members in all, in increasing order.
fn subsets(n: usize, most: usize, from: usize, current: &mut Vec<usize>, f: &mut impl FnMut(&[usize])) {
    for i in from..n {
        current.push(i);
        f(current);
        if current.len() < most {
            subsets(n, most, i + 1, current, f);
        }
        current.pop();
    }
}

/// Weights for candidates `s` minimising the squared distance to where all
/// joints put the vertex, with the weights summing to one: the equations
/// `2 G w + μ 1 = 2 c` and `1·w = 1`, solved by elimination.
fn solve(gram: &[[f64; CANDIDATES]; CANDIDATES], cross: &[f64; CANDIDATES], s: &[usize]) -> Option<Vec<(usize, f64)>> {
    let n = s.len();
    if n == 1 {
        return Some(vec![(s[0], 1.0)]);
    }
    let size = n + 1;
    let mut a = [[0f64; 5]; 5];
    let mut b = [0f64; 5];
    let trace: f64 = s.iter().map(|&i| gram[i][i]).sum();
    for (r, &i) in s.iter().enumerate() {
        for (c, &j) in s.iter().enumerate() {
            a[r][c] = 2.0 * gram[i][j];
        }
        // A whisper of ridge keeps joints that never move from making the
        // system singular.
        a[r][r] += 2.0 * (1e-9 * trace + 1e-24);
        a[r][n] = 1.0;
        a[n][r] = 1.0;
        b[r] = 2.0 * cross[i];
    }
    b[n] = 1.0;
    let x = gauss(a, b, size)?;
    let w: Vec<(usize, f64)> = s.iter().enumerate().map(|(r, &i)| (i, x[r])).collect();
    w.iter().all(|(_, x)| x.is_finite()).then_some(w)
}

/// Solves the first `n` rows of `a x = b` by Gaussian elimination with
/// partial pivoting.
fn gauss(mut a: [[f64; 5]; 5], mut b: [f64; 5], n: usize) -> Option<[f64; 5]> {
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-300 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
            let pivot = a[c];
            for (x, p) in a[r][c..n].iter_mut().zip(&pivot[c..n]) {
                *x -= f * p;
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = [0f64; 5];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|k| a[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vertex moved by five joints, the weakest of which always moves
    /// with the fourth (as a lip joint does with the jaw): refitting gives
    /// its weight to the fourth and follows all five exactly, where
    /// rescaling the four doesn't.
    #[test]
    fn refitting_beats_rescaling() {
        let offsets = [DVec3::X, DVec3::Y, DVec3::Z, DVec3::new(1.0, 1.0, 0.0), DVec3::new(1.0, 1.0, 0.0)];
        let poses: Vec<Vec<DMat4>> = (0..4)
            .map(|s| {
                (0..5)
                    .map(|j| {
                        let moves = j == s || (s == 3 && j == 4);
                        DMat4::from_translation(offsets[j] * if moves { 0.01 } else { 0.0 })
                    })
                    .collect()
            })
            .collect();
        let moved: Vec<Vec<bool>> = poses.iter().map(|p| p.iter().map(|m| *m != DMat4::IDENTITY).collect()).collect();
        let influences = [(0, 0.3), (1, 0.25), (2, 0.2), (3, 0.15), (4, 0.1)];
        let f = fit_vertex(DVec3::ZERO, &influences, &poses, &moved).unwrap();
        assert!(f.mean_before > 1e-4);
        assert!(f.mean_after < f.mean_before * 0.1, "{} vs {}", f.mean_after, f.mean_before);
        assert!((f.weights.x + f.weights.y + f.weights.z + f.weights.w - 1.0).abs() < 1e-5);
        let w3: f32 = (0..4).filter(|&k| f.joints[k] == 3).map(|k| f.weights[k]).sum();
        assert!((w3 - 0.25).abs() < 0.02, "{:?} {}", f.joints, f.weights);
        assert!(f.weights.min_element() >= 0.0);
    }

    #[test]
    fn fits_round_trip_as_bytes() {
        let fit = Fit {
            meshes: vec![(3, vec![(7, [1, 2, 3, 4], [0.5, 0.25, 0.125, 0.125])]), (5, vec![])],
            report: FitReport {
                vertices: 1,
                poses: 251,
                mean_before: 1e-3,
                max_before: 8e-3,
                mean_after: 2e-4,
                max_after: 3e-3,
            },
        };
        let bytes = fit.to_bytes();
        assert_eq!(Fit::from_bytes(&bytes), Some(fit));
        assert_eq!(Fit::from_bytes(&bytes[..bytes.len() - 1]), None);
        let mut lying = bytes.clone();
        lying[8 * 6 + 4 + 2] = 0xff;
        assert_eq!(Fit::from_bytes(&lying), None, "a count larger than the bytes is refused");
    }

    #[test]
    fn gauss_solves() {
        let mut a = [[0f64; 5]; 5];
        a[0][..3].copy_from_slice(&[2.0, 1.0, 0.0]);
        a[1][..3].copy_from_slice(&[1.0, 3.0, 1.0]);
        a[2][..3].copy_from_slice(&[0.0, 1.0, 4.0]);
        let x = gauss(a, [3.0, 5.0, 5.0, 0.0, 0.0], 3).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-12 && (x[1] - 1.0).abs() < 1e-12 && (x[2] - 1.0).abs() < 1e-12);
    }
}
