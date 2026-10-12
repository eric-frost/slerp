//! A flat CNC job (a laser cutter's or a plotter's, all at one height)
//! drawn from above for its thumbnail: in 3D seen from the side it would
//! be an edge.

use super::{Feature, MoveKind, Toolpath};
use crate::draw::{Canvas, Rgba, thin};
use glam::Vec2;
use slerp_scene::Image8;

const HALO: Rgba = [0.06, 0.06, 0.08, 0.55];

/// Whether all the cuts that move sideways lie in one plane: within half a
/// millimetre, or a fiftieth of the job's width.  Plunges and lifts
/// (cuts straight up and down) don't count.
pub fn is_flat(tp: &Toolpath) -> bool {
    if tp.is_empty() {
        return false;
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 1..tp.points.len() {
        let (a, b) = (tp.points[i - 1], tp.points[i]);
        if tp.moves[i].feature == Feature::Cut && tp.moves[i].kind == MoveKind::Feed && (a[0] != b[0] || a[1] != b[1]) {
            lo = lo.min(a[2].min(b[2]));
            hi = hi.max(a[2].max(b[2]));
        }
    }
    let extent = (tp.max[0] - tp.min[0]).max(tp.max[1] - tp.min[1]);
    hi >= lo && hi - lo <= (extent * 0.02).max(0.5)
}

/// Draws the cuts from above, no larger than `size`.
pub fn cut_drawing(tp: &Toolpath, size: u32) -> Option<Image8> {
    if tp.is_empty() {
        return None;
    }
    let size = size.clamp(16, 4096) as f32;
    let (w, h) = ((tp.max[0] - tp.min[0]).max(1e-3), (tp.max[1] - tp.min[1]).max(1e-3));
    let (pw, ph) = if w >= h { (size, (size * h / w).max(size / 3.0)) } else { ((size * w / h).max(size / 3.0), size) };
    let r = (size / 256.0).max(0.6);
    let margin = (size * 0.04).max(r + 3.0);
    let scale = ((pw - 2.0 * margin) / w).min((ph - 2.0 * margin) / h);
    let (cx, cy) = ((tp.min[0] + tp.max[0]) * 0.5, (tp.min[1] + tp.max[1]) * 0.5);
    let to_px = |p: [f32; 3]| Vec2::new(pw * 0.5 + (p[0] - cx) * scale, ph * 0.5 - (p[1] - cy) * scale);

    // 1. Runs of consecutive cuts.
    let mut runs: Vec<Vec<Vec2>> = Vec::new();
    let mut run: Vec<Vec2> = Vec::new();
    for i in 1..tp.points.len() {
        let cut = tp.moves[i].kind == MoveKind::Feed && tp.moves[i].feature == Feature::Cut;
        if !cut {
            if run.len() >= 2 {
                runs.push(std::mem::take(&mut run));
            }
            run.clear();
            continue;
        }
        if run.is_empty() {
            run.push(to_px(tp.points[i - 1]));
        }
        run.push(to_px(tp.points[i]));
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    let runs: Vec<Vec<Vec2>> = runs.into_iter().map(|r| thin(&r, 0.5).into_iter().map(|i| r[i]).collect()).collect();

    // 2. A halo, then the cuts.
    let mut canvas = Canvas::new(pw.round() as u32, ph.round() as u32);
    canvas.halo(&runs, r + 1.0, HALO);
    let [cr, cg, cb] = Feature::Cut.srgb().map(|c| c as f32 / 255.0);
    for run in &runs {
        canvas.stroke(run, &[[cr, cg, cb, 1.0]], r);
    }
    Some(canvas.into_image())
}
