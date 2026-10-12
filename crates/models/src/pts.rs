//! Leica's text point formats, PTS and PTX.
//!
//! PTS is a point count line followed by one point per line: `x y z`,
//! optionally an intensity (-2048 to 2047 in Leica's own files, 0 to 255
//! in others), optionally 8-bit RGB.  Some files hold several blocks, each
//! with its own count line.
//!
//! PTX holds one or more gridded scans.  Each starts with ten header lines:
//! columns, rows, the scanner's position and axes (three lines), and a 4x4
//! matrix placing the scan, written for row vectors; then rows × columns
//! points of `x y z intensity [r g b]`, where `0 0 0` marks a direction with
//! no return.  Both are metres, Z up.

use crate::common::{parse_f64, tokens};
use crate::points::Cloud;
use glam::{DMat4, DVec3, DVec4};
use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

/// True when the first line is a single count and the next a point.
pub fn looks_like_pts(head: &[u8]) -> bool {
    let mut lines = text_lines(head);
    let first: Vec<&[u8]> = lines.next().map(|l| tokens(l).collect()).unwrap_or_default();
    let second: Vec<&[u8]> = lines.next().map(|l| tokens(l).collect()).unwrap_or_default();
    let numeric = |w: &[&[u8]]| w.iter().all(|t| parse_f64(t).is_some());
    let point = |w: &[&[u8]]| matches!(w.len(), 3 | 4 | 6 | 7) && numeric(w);
    (first.len() == 1 && numeric(&first) && point(&second)) || (point(&first) && point(&second))
}

/// True when the first two lines are the column and row counts.
pub fn looks_like_ptx(head: &[u8]) -> bool {
    let mut lines = text_lines(head);
    let one = |l: Option<&[u8]>| {
        l.is_some_and(|l| {
            let w: Vec<&[u8]> = tokens(l).collect();
            w.len() == 1 && crate::common::parse_i64(w[0]).is_some_and(|v| v >= 0)
        })
    };
    one(lines.next()) && one(lines.next())
}

fn text_lines(b: &[u8]) -> impl Iterator<Item = &[u8]> {
    b.split(|&c| c == b'\n').filter(|l| l.iter().any(|c| !c.is_ascii_whitespace()))
}

/// Numbers of one line, into `out`; false when a token isn't a number.
fn numbers(line: &[u8], out: &mut Vec<f64>) -> bool {
    out.clear();
    for t in tokens(line) {
        match parse_f64(t) {
            Some(v) => out.push(v),
            None => return false,
        }
    }
    true
}

fn rgb(v: &[f64]) -> [u16; 3] {
    [0, 1, 2].map(|i| (v[i].clamp(0.0, 255.0) * 257.0) as u16)
}

pub fn load_pts(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // The first count is a hint for even thinning; lines are what count.
    let mut lines = text_lines(bytes).peekable();
    let mut v = Vec::new();
    let mut expected = None;
    if let Some(first) = lines.peek()
        && numbers(first, &mut v)
        && v.len() == 1
    {
        expected = Some((v[0].max(0.0) as u64).min(bytes.len() as u64 / 6));
    }
    let mut cloud = Cloud::new(expected, limits);
    for line in lines {
        if !numbers(line, &mut v) || v.len() < 3 {
            continue;
        }
        if !cloud.wants() {
            continue;
        }
        let p = DVec3::new(v[0], v[1], v[2]);
        let (intensity, color) = match v.len() {
            4 => (Some(v[3]), None),
            6 => (None, Some(rgb(&v[3..6]))),
            7.. => (Some(v[3]), Some(rgb(&v[4..7]))),
            _ => (None, None),
        };
        // Leica's intensities run from -2048; shift them positive.
        let intensity = intensity.map(|i| (i + 2048.0).clamp(0.0, 65535.0) as u16);
        cloud.push(p, color, intensity);
    }
    cloud.finish("PTS", true)
}

pub fn load_ptx(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let mut lines = text_lines(bytes);
    // Every point takes a line of at least six bytes ("0 0 0\n").
    let mut cloud = Cloud::new(Some(bytes.len() as u64 / 6), limits);
    let mut v = Vec::new();
    let mut scans = 0;
    let mut warnings = Vec::new();
    'scans: loop {
        // 1. The scan's header.
        let mut header = Vec::new();
        for _ in 0..10 {
            let Some(line) = lines.next() else { break 'scans };
            if !numbers(line, &mut v) {
                warnings.push("PTX scan header is damaged".into());
                break 'scans;
            }
            header.push(v.clone());
        }
        let count = |h: &Vec<f64>| h.first().map_or(0, |&c| c.max(0.0) as u64);
        let points = count(&header[0]).saturating_mul(count(&header[1]));
        let row = |i: usize| {
            let r = &header[i];
            DVec4::new(
                *r.first().unwrap_or(&0.0),
                *r.get(1).unwrap_or(&0.0),
                *r.get(2).unwrap_or(&0.0),
                *r.get(3).unwrap_or(&0.0),
            )
        };
        let m = DMat4::from_cols(row(6), row(7), row(8), row(9));
        let m = if m.is_finite() && m.determinant().abs() > 1e-12 { m } else { DMat4::IDENTITY };
        scans += 1;
        // 2. Its points.
        for _ in 0..points {
            let Some(line) = lines.next() else { break 'scans };
            if !numbers(line, &mut v) || v.len() < 3 {
                continue;
            }
            if !cloud.wants() {
                continue;
            }
            let local = DVec3::new(v[0], v[1], v[2]);
            let p = if local == DVec3::ZERO { DVec3::NAN } else { m.transform_point3(local) };
            let intensity = v.get(3).map(|&i| (i.clamp(0.0, 1.0) * 65535.0) as u16);
            let color = (v.len() >= 7).then(|| rgb(&v[4..7]));
            cloud.push(p, color, intensity);
        }
    }
    let mut scene = cloud.finish("PTX", true)?;
    scene.info.details.push(("Scans".into(), scans.to_string()));
    scene.info.warnings = warnings;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pts_variants() {
        let pts = "3\n0 0 0 -2048 255 0 0\n1 0 0 0 0 255 0\n0 1 0 2047 0 0 255\n";
        assert!(looks_like_pts(pts.as_bytes()));
        let s = load_pts(Path::new("a.pts"), pts.as_bytes(), &Limits::viewer()).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 3);
        assert_eq!(p.colors[0], glam::Vec4::new(1.0, 0.0, 0.0, 1.0));
        let plain = "0 0 0\n1 0 0\n0 0 1\n";
        assert!(looks_like_pts(plain.as_bytes()));
        assert_eq!(
            load_pts(Path::new("a.pts"), plain.as_bytes(), &Limits::viewer()).unwrap().meshes[0].primitives[0]
                .positions
                .len(),
            3
        );
        for cut in 0..pts.len() {
            let _ = load_pts(Path::new("a.pts"), &pts.as_bytes()[..cut], &Limits::viewer());
        }
    }

    #[test]
    fn reads_ptx_scans_with_transforms() {
        // Two 2x1 scans, the second moved 10 m along X; one point has no
        // return.
        let scan = |tx: f64| {
            format!(
                "2\n1\n0 0 0\n1 0 0\n0 1 0\n0 0 1\n1 0 0 0\n0 1 0 0\n0 0 1 0\n{tx} 0 0 1\n0 0 1 0.5 10 20 30\n0 0 0 0.5 0 0 0\n"
            )
        };
        let ptx = format!("{}{}", scan(0.0), scan(10.0));
        assert!(looks_like_ptx(ptx.as_bytes()));
        let s = load_ptx(Path::new("a.ptx"), ptx.as_bytes(), &Limits::viewer()).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 2);
        assert!((p.positions[1] - p.positions[0]).length() > 9.99);
        assert!(s.info.details.iter().any(|d| d.0 == "Scans" && d.1 == "2"));
        for cut in 0..ptx.len() {
            let _ = load_ptx(Path::new("a.ptx"), &ptx.as_bytes()[..cut], &Limits::viewer());
        }
    }
}
