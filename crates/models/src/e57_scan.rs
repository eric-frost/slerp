//! E57 laser scans (ASTM E2807), read through the `e57` crate: every scan in
//! the file, placed by its pose, with spherical coordinates converted and
//! colour or intensity per point.  E57 is metres, Z up.

use crate::points::Cloud;
use glam::DVec3;
use slerp_scene::{Limits, LoadError, Scene};
use std::io::Cursor;
use std::path::Path;

/// True for the E57 file signature.
pub fn looks_like_e57(head: &[u8]) -> bool {
    head.starts_with(b"ASTM-E57")
}

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // The `e57` crate checks what it reads, but a scan is never worth a
    // crash: a panic inside it becomes an error.
    std::panic::catch_unwind(|| read(bytes, limits)).unwrap_or_else(|_| Err(LoadError::invalid("E57 file is damaged")))
}

fn read(bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let mut reader = e57::E57Reader::new(Cursor::new(bytes)).map_err(|e| LoadError::invalid(format!("E57: {e}")))?;
    let scans = reader.pointclouds();
    // Each point takes at least a bit of the file, whatever the XML claims.
    let total: u64 = scans.iter().map(|s| s.records).fold(0u64, u64::saturating_add).min(bytes.len() as u64 * 8);
    let mut cloud = Cloud::new(Some(total), limits);
    let mut warnings = Vec::new();
    let mut sensors = Vec::new();
    for scan in &scans {
        if let Some(model) = scan.sensor_model.as_ref().filter(|m| !m.is_empty() && !sensors.contains(*m)) {
            sensors.push(model.clone());
        }
        let mut points = match reader.pointcloud_simple(scan) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("scan {}: {e}", scan.name.as_deref().unwrap_or("?")));
                continue;
            }
        };
        points.spherical_to_cartesian(true);
        points.intensity_to_color(false);
        points.apply_pose(true);
        for point in points {
            let point = match point {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(format!("scan {} is damaged: {e}", scan.name.as_deref().unwrap_or("?")));
                    break;
                }
            };
            if !cloud.wants() {
                continue;
            }
            let position = match point.cartesian {
                e57::CartesianCoordinate::Valid { x, y, z } => DVec3::new(x, y, z),
                _ => DVec3::NAN,
            };
            let unit = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            let color = point.color.map(|c| [unit(c.red), unit(c.green), unit(c.blue)]);
            cloud.push(position, color, point.intensity.map(unit));
        }
    }
    let mut scene = cloud.finish("E57", true)?;
    scene.info.details.push(("Scans".into(), scans.len().to_string()));
    if !sensors.is_empty() {
        scene.info.details.push(("Scanner".into(), sensors.join(", ")));
    }
    scene.info.generator = reader.library_version().map(str::to_string).filter(|v| !v.is_empty());
    scene.info.warnings = warnings;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_garbage() {
        let mut b = b"ASTM-E57".to_vec();
        b.extend_from_slice(&[0u8; 2040]);
        assert!(load(Path::new("a.e57"), &b, &Limits::viewer()).is_err());
        for cut in [0, 8, 47, 48, 1024] {
            let _ = load(Path::new("a.e57"), &b[..cut.min(b.len())], &Limits::viewer());
        }
    }
}
