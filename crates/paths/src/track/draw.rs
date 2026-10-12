//! A track's thumbnail: the route seen from above, north up, as a thick
//! line coloured by elevation (or speed), with a dark halo so it reads on
//! light and dark backgrounds alike, start and end marked, on a
//! transparent background shaped like the track.

use super::scene::{ColorBy, OUTLINE_SRGB, point_colors};
use super::stats::Profile;
use super::{LineKind, TrackSet};
use crate::draw::{Canvas, Rgba, thin};
use glam::Vec2;
use slerp_scene::Image8;

const HALO: Rgba = [0.06, 0.06, 0.08, 0.7];
const START: Rgba = [0.15, 0.75, 0.25, 1.0];
const END: Rgba = [0.85, 0.12, 0.12, 1.0];
const WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];

/// Draws `set` no larger than `size` pixels on its longer side, or `None`
/// when it holds nothing to draw.
pub fn track_thumbnail(set: &TrackSet, profile: &Profile, size: u32) -> Option<Image8> {
    if set.is_empty() {
        return None;
    }
    let size = size.clamp(16, 4096) as f32;
    let proj = set.projection();

    // 1. Everything in local metres (x east, y north), and the extent.
    let outlines: Vec<Vec<[f64; 2]>> = set
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Outline)
        .map(|l| {
            l.points
                .iter()
                .map(|p| {
                    let (e, n) = proj.project(p.lat, p.lon);
                    [e, n]
                })
                .collect()
        })
        .collect();
    let waypoints: Vec<[f64; 2]> = set
        .waypoints
        .iter()
        .map(|w| {
            let (e, n) = proj.project(w.lat, w.lon);
            [e, n]
        })
        .collect();
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    let all = profile
        .pos
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64])
        .chain(outlines.iter().flatten().copied())
        .chain(waypoints.iter().copied());
    for p in all {
        for k in 0..2 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    if !(min[0].is_finite() && max[0].is_finite()) {
        return None;
    }
    // A single point or a tiny track gets 100 m around it.
    for k in 0..2 {
        if max[k] - min[k] < 100.0 {
            let mid = (min[k] + max[k]) * 0.5;
            min[k] = mid - 50.0;
            max[k] = mid + 50.0;
        }
    }

    // 2. The picture's shape follows the track's, no thinner than 1:3.
    let (w, h) = (max[0] - min[0], max[1] - min[1]);
    let (pw, ph) = if w >= h {
        (size, (size * (h / w) as f32).max(size / 3.0))
    } else {
        ((size * (w / h) as f32).max(size / 3.0), size)
    };
    let r = (size / 90.0).max(1.2);
    let halo = r + (size / 200.0).max(1.0);
    let margin = (size * 0.05).max(halo + r * 1.5 + 2.0);
    let scale = ((pw - 2.0 * margin) as f64 / w).min((ph - 2.0 * margin) as f64 / h);
    let (cx, cy) = ((min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5);
    let to_px =
        |p: [f64; 2]| Vec2::new(pw * 0.5 + ((p[0] - cx) * scale) as f32, ph * 0.5 - ((p[1] - cy) * scale) as f32);
    let mut canvas = Canvas::new(pw.round() as u32, ph.round() as u32);

    // 3. Polygon outlines under the route.
    let outline_px: Vec<Vec<Vec2>> = outlines
        .iter()
        .map(|o| {
            let px: Vec<Vec2> = o.iter().map(|&p| to_px(p)).collect();
            thin(&px, 0.75).into_iter().map(|i| px[i]).collect()
        })
        .collect();
    let thin_r = (r * 0.6).max(1.0);
    canvas.halo(&outline_px, thin_r + (halo - r), HALO);
    for o in &outline_px {
        canvas.stroke(o, &[[OUTLINE_SRGB[0], OUTLINE_SRGB[1], OUTLINE_SRGB[2], 1.0]], thin_r);
    }

    // 4. The route, coloured point by point.
    let by = ColorBy::best(profile);
    let (colors, _) = point_colors(profile, by);
    let ends: Vec<usize> = profile.starts.iter().skip(1).copied().chain([profile.len()]).collect();
    let mut lines: Vec<(Vec<Vec2>, Vec<Rgba>)> = Vec::new();
    for (&a, &b) in profile.starts.iter().zip(&ends) {
        let px: Vec<Vec2> = profile.pos[a..b].iter().map(|p| to_px([p[0] as f64, p[1] as f64])).collect();
        let kept = thin(&px, 0.75);
        let pts = kept.iter().map(|&i| px[i]).collect();
        let cols = kept
            .iter()
            .map(|&i| {
                let c = colors[a + i];
                [c[0], c[1], c[2], 1.0]
            })
            .collect();
        lines.push((pts, cols));
    }
    let halos: Vec<Vec<Vec2>> = lines.iter().map(|l| l.0.clone()).collect();
    canvas.halo(&halos, halo, HALO);
    for (pts, cols) in &lines {
        canvas.stroke(pts, cols, r);
    }

    // 5. Waypoints, then the start and the end on top.
    if waypoints.len() <= 500 {
        for &w in &waypoints {
            canvas.marker(to_px(w), r * 0.7, WHITE, HALO, (r * 0.4).max(1.0));
        }
    }
    let first = lines.iter().find(|l| !l.0.is_empty()).and_then(|l| l.0.first().copied());
    let last = lines.iter().rev().find(|l| !l.0.is_empty()).and_then(|l| l.0.last().copied());
    let ring = (r * 0.5).max(1.0);
    if let Some(p) = last {
        // A loop's end would hide under its start: a larger red ring shows
        // around the start instead.
        let loop_end = first.is_some_and(|f| f.distance(p) < r * 3.0);
        let radius = if loop_end { r * 2.4 } else { r * 1.4 };
        canvas.marker(p, radius, END, WHITE, ring);
    }
    if let Some(p) = first {
        canvas.marker(p, r * 1.4, START, WHITE, ring);
    }
    Some(canvas.into_image())
}
