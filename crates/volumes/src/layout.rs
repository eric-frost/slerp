//! Axis layout shared by the formats whose first dimension varies fastest
//! (NIfTI, MetaImage, NRRD).

use crate::volume::{Axis, Dir, Role};

/// Builds the axes of an array of `dims` (first dimension fastest) whose
/// samples have `samples` interleaved values (colour, innermost).  With
/// `dirs` (the body direction each of the first three axes points to), roles
/// follow the anatomy; without, the first three axes are x, y and z.  The
/// fourth is time, the rest are held at their first index.
pub fn fastest_first(
    dims: &[u64],
    samples: usize,
    dirs: Option<[Dir; 3]>,
    spacing: &[f64],
) -> Result<Vec<Axis>, String> {
    let mut stride = samples as u64;
    let mut axes = Vec::new();
    for (i, &d) in dims.iter().enumerate() {
        if d == 0 || d > 1 << 31 {
            return Err("a dimension is zero or absurdly large".into());
        }
        let st = usize::try_from(stride).map_err(|_| "the volume is absurdly large")?;
        let (role, name) = match i {
            0 => (Role::X, "x"),
            1 => (Role::Y, "y"),
            2 => (Role::Z, "z"),
            3 => (Role::T, "t"),
            _ => (Role::Other, "u"),
        };
        let mut a = Axis::new(d as usize, st, role).named(name);
        if i < 3 {
            a.spacing = spacing.get(i).copied().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(0.0);
            if let Some(dirs) = dirs.filter(|_| dims.len() >= 3) {
                a.dir = Some(dirs[i]);
                a.role = dirs[i].role();
            }
        }
        axes.push(a);
        stride = stride.checked_mul(d).filter(|&s| s < 1 << 50).ok_or("the volume is absurdly large")?;
    }
    if samples > 1 {
        axes.push(Axis::new(samples, 1, Role::Color).named("colour"));
    }
    Ok(axes)
}

/// The body direction each axis points to, from the direction vectors of the
/// three spatial axes in a world whose +x, +y and +z are `world`.  `None`
/// unless the three axes point three different ways.
pub fn dirs_from_vectors(vectors: &[[f64; 3]; 3], world: [Dir; 3]) -> Option<([Dir; 3], [f64; 3])> {
    let mut dirs = [Dir::R; 3];
    let mut size = [0f64; 3];
    let mut used = [false; 3];
    for (i, v) in vectors.iter().enumerate() {
        size[i] = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let (k, c) = v.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
        if used[k] || c.abs() < 1e-9 || !c.is_finite() {
            return None;
        }
        used[k] = true;
        dirs[i] = if *c > 0.0 { world[k] } else { world[k].opposite() };
    }
    Some((dirs, size))
}

/// Roles for the axes of an array whose axes carry no meaning in the file (a
/// NumPy or HDF5 array), from its shape: pictures, stacks of pictures, colour
/// pictures, time series; whether the choice of slice axis is a guess; and why
/// no picture can be made.
pub fn array_roles(shape: &[u64]) -> (Vec<Role>, bool, Option<String>) {
    let n = shape.len();
    let colour = |k: usize| shape[k] == 3 || shape[k] == 4;
    match n {
        0 => (vec![], false, Some("a single value".into())),
        1 => (vec![Role::X], false, Some("a one-dimensional array".into())),
        2 => (vec![Role::Y, Role::X], false, None),
        3 if colour(2) => (vec![Role::Y, Role::X, Role::Color], false, None),
        3 => (vec![Role::Z, Role::Y, Role::X], true, None),
        4 if colour(3) => (vec![Role::Z, Role::Y, Role::X, Role::Color], false, None),
        4 => (vec![Role::T, Role::Z, Role::Y, Role::X], true, None),
        _ => (vec![Role::Other; n], false, Some(format!("a {n}-dimensional array"))),
    }
}

pub const RAS: [Dir; 3] = [Dir::R, Dir::A, Dir::S];
pub const LAS: [Dir; 3] = [Dir::L, Dir::A, Dir::S];
pub const LPS: [Dir; 3] = [Dir::L, Dir::P, Dir::S];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_follow_directions_or_position() {
        let a = fastest_first(&[4, 3, 2, 5], 1, Some([Dir::A, Dir::S, Dir::R]), &[1.0, 2.0, 3.0]).unwrap();
        assert_eq!(a.iter().map(|x| x.role).collect::<Vec<_>>(), vec![Role::Y, Role::Z, Role::X, Role::T]);
        assert_eq!(a[1].stride, 4);
        assert_eq!(a[3].stride, 24);
        let b = fastest_first(&[4, 3], 3, None, &[]).unwrap();
        assert_eq!(b.iter().map(|x| x.role).collect::<Vec<_>>(), vec![Role::X, Role::Y, Role::Color]);
        assert_eq!(b[0].stride, 3);
        assert!(fastest_first(&[4, 0], 1, None, &[]).is_err());
        assert!(fastest_first(&[1 << 30, 1 << 30, 1 << 30], 1, None, &[]).is_err());
    }

    #[test]
    fn vectors_give_directions() {
        let v = [[0.0, -2.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 3.0]];
        let (d, s) = dirs_from_vectors(&v, RAS).unwrap();
        assert_eq!(d, [Dir::P, Dir::R, Dir::S]);
        assert_eq!(s, [2.0, 1.0, 3.0]);
        assert!(dirs_from_vectors(&[[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], RAS).is_none());
    }
}
