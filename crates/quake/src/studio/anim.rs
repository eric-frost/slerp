//! A Source model's sequences as skeletal clips.
//!
//! Each sequence plays an animation (the first of its blend grid).  An
//! animation's frames hold, per bone that moves, a record whose flags say
//! how its rotation and position are stored: a constant compressed
//! quaternion (48 or 64 bits) and position (three half floats), or
//! run-length-encoded Euler angles and position offsets per frame, scaled
//! by the bone's own scales, as Half-Life stored them.  Bones without a
//! record keep their rest pose.  Long animations are split into sections,
//! and the frames of some live in the model's `.ani` file instead of the
//! `.mdl`.

use super::mdl::{self, AnimDesc, Bone, Header};
use crate::read::{Bytes, Result};
use slerp_scene::glam::{Quat, Vec3};
use slerp_scene::{Animation, Channel, Interpolation, LoadError, Property};

/// Frames decoded per animation; the rest are dropped.
const MAX_FRAMES: usize = 4096;
/// Bone keys (one bone in one frame) decoded for the whole model.
const MAX_KEYS: usize = 4 << 20;

const RAWPOS: u8 = 0x01;
const RAWROT: u8 = 0x02;
const ANIMPOS: u8 = 0x04;
const ANIMROT: u8 = 0x08;
const RAWROT2: u8 = 0x20;

/// Where an animation's frames come from: this file, or the `.ani` file's
/// blocks, whose table is in the `.mdl` header.
pub struct Sources<'a> {
    pub mdl: Bytes<'a>,
    pub header: &'a Header,
    pub ani: Option<Bytes<'a>>,
}

impl<'a> Sources<'a> {
    /// The bytes and offset where block `block`'s data plus `index` starts.
    fn locate(&self, desc: &AnimDesc, block: usize, index: usize) -> Result<(Bytes<'a>, usize)> {
        if block == 0 {
            let at = desc.at.checked_add(index).ok_or_else(|| LoadError::invalid("animation offset"))?;
            return Ok((self.mdl, at));
        }
        let Some(ani) = self.ani else {
            return Err(LoadError::invalid("the animation is in the model's .ani file, which wasn't found"));
        };
        if block >= self.header.anim_blocks.0 {
            return Err(LoadError::invalid("animation block out of range"));
        }
        let start = self.mdl.index(self.header.anim_blocks.1 + block * 8)?;
        let at = start.checked_add(index).ok_or_else(|| LoadError::invalid("animation offset"))?;
        Ok((ani, at))
    }

    /// The first bone record of frame `frame` and the frame's number within
    /// its section.
    fn frame(&self, desc: &AnimDesc, frame: usize) -> Result<(Bytes<'a>, usize, usize)> {
        if desc.section_frames == 0 {
            let (b, at) = self.locate(desc, desc.block, desc.index)?;
            return Ok((b, at, frame));
        }
        // The last frame of a long animation has a section of its own.
        let (section, local) = if desc.frames > desc.section_frames && frame + 1 == desc.frames {
            (desc.frames / desc.section_frames + 1, 0)
        } else {
            (frame / desc.section_frames, frame % desc.section_frames)
        };
        let entry = desc
            .at
            .checked_add(desc.section_index)
            .and_then(|a| a.checked_add(section * 8))
            .ok_or_else(|| LoadError::invalid("animation section offset"))?;
        let block = self.mdl.index(entry)?;
        let index = self.mdl.index(entry + 4)?;
        let (b, at) = self.locate(desc, block, index)?;
        Ok((b, at, local))
    }
}

/// Clips for the model's sequences.  Problems with one sequence become a
/// warning and leave it out.
pub fn clips(src: &Sources, bones: &[Bone], bone_base: usize, warnings: &mut Vec<String>) -> Vec<Animation> {
    let sequences = match mdl::sequences(src.mdl, src.header) {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("sequences: {e}"));
            return Vec::new();
        }
    };
    let mut budget = MAX_KEYS;
    let mut decoded: std::collections::HashMap<usize, Option<Animation>> = Default::default();
    let mut out: Vec<Animation> = Vec::new();
    let mut failed = Vec::new();
    for seq in sequences {
        let clip = decoded.entry(seq.anim).or_insert_with(|| {
            match mdl::animdesc(src.mdl, src.header, seq.anim)
                .and_then(|d| decode(src, &d, bones, bone_base, &mut budget))
            {
                Ok(c) => Some(c),
                Err(e) => {
                    failed.push(format!("{}: {e}", seq.name));
                    None
                }
            }
        });
        let Some(clip) = clip else { continue };
        let mut clip = clip.clone();
        clip.name = if seq.name.is_empty() { format!("sequence {}", out.len() + 1) } else { seq.name.clone() };
        let base = clip.name.clone();
        let mut n = 2;
        while out.iter().any(|a| a.name == clip.name) {
            clip.name = format!("{base} {n}");
            n += 1;
        }
        out.push(clip);
    }
    if !failed.is_empty() {
        let shown: Vec<&str> = failed.iter().take(4).map(String::as_str).collect();
        let more = if failed.len() > 4 { format!(" and {} more", failed.len() - 4) } else { String::new() };
        warnings.push(format!("sequences not shown: {}{more}", shown.join("; ")));
    }
    out
}

/// Decodes one animation into a translation and a rotation channel per
/// bone, one key per frame.
fn decode(src: &Sources, desc: &AnimDesc, bones: &[Bone], bone_base: usize, budget: &mut usize) -> Result<Animation> {
    if desc.flags & mdl::ANIM_FRAMEANIM != 0 {
        return Err(LoadError::Unsupported("CS:GO's frame-encoded animations".into()));
    }
    let frames = desc.frames.clamp(1, MAX_FRAMES);
    *budget = budget
        .checked_sub(frames.saturating_mul(bones.len()))
        .ok_or_else(|| LoadError::TooLarge("more animation than this viewer keeps".into()))?;
    let delta = desc.flags & mdl::ANIM_DELTA != 0;
    let mut pos: Vec<Vec<Vec3>> = bones.iter().map(|b| vec![b.pos; frames]).collect();
    let mut rot: Vec<Vec<Quat>> = bones.iter().map(|b| vec![b.quat; frames]).collect();
    if desc.flags & mdl::ANIM_ALLZEROS == 0 {
        for f in 0..frames {
            let (data, first, local) = src.frame(desc, f)?;
            let mut at = first;
            for _ in 0..=bones.len().max(1) {
                let bone = data.u8(at)? as usize;
                let flags = data.u8(at + 1)?;
                let next = data.i16(at + 2)?;
                if let Some(b) = bones.get(bone) {
                    let (p, q) = pose(data, at, flags, local, b, delta)?;
                    pos[bone][f] = p;
                    rot[bone][f] = q;
                }
                if next <= 0 {
                    break;
                }
                at = at.checked_add(next as usize).ok_or_else(|| LoadError::invalid("animation record offset"))?;
            }
        }
    }
    let times: Vec<f32> = (0..frames).map(|f| f as f32 / desc.fps).collect();
    let mut channels = Vec::with_capacity(bones.len() * 2);
    for (i, (p, r)) in pos.into_iter().zip(rot).enumerate() {
        channels.push(Channel {
            node: bone_base + i,
            property: Property::Translation,
            interpolation: Interpolation::Linear,
            times: times.clone(),
            values: p.iter().flat_map(|v| v.to_array()).collect(),
        });
        // Keep neighbouring keys on the same side of the sphere, so linear
        // interpolation takes the short way.
        let mut prev = Quat::IDENTITY;
        let values = r
            .iter()
            .flat_map(|q| {
                let q = if q.dot(prev) < 0.0 { -*q } else { *q };
                prev = q;
                q.to_array()
            })
            .collect();
        channels.push(Channel {
            node: bone_base + i,
            property: Property::Rotation,
            interpolation: Interpolation::Linear,
            times: times.clone(),
            values,
        });
    }
    Ok(Animation { name: desc.name.clone(), duration: *times.last().unwrap_or(&0.0), channels })
}

/// One bone's position and rotation at `frame` from its record at `at`.
/// Delta animations (gestures layered on another) are shown over the rest
/// pose.
fn pose(data: Bytes, at: usize, flags: u8, frame: usize, bone: &Bone, delta: bool) -> Result<(Vec3, Quat)> {
    let mut o = at + 4;
    let mut q = bone.quat;
    if flags & RAWROT2 != 0 {
        q = quat64(data.slice(o, 8)?);
        o += 8;
    } else if flags & RAWROT != 0 {
        q = quat48(data.slice(o, 6)?);
        o += 6;
    } else if flags & ANIMROT != 0 {
        let mut a = Vec3::ZERO;
        for k in 0..3 {
            a[k] = value(data, o, k, frame)? * bone.rot_scale[k];
        }
        q = mdl::euler(if delta { a } else { a + bone.rot });
        o += 6;
    } else if delta {
        q = Quat::IDENTITY;
    }
    let mut p = if delta { Vec3::ZERO } else { bone.pos };
    if flags & RAWPOS != 0 {
        let v = data.slice(o, 6)?;
        let h = |i: usize| mdl::f16(u16::from_le_bytes([v[2 * i], v[2 * i + 1]]));
        p = Vec3::new(h(0), h(1), h(2));
    } else if flags & ANIMPOS != 0 {
        let mut d = Vec3::ZERO;
        for k in 0..3 {
            d[k] = value(data, o, k, frame)? * bone.pos_scale[k];
        }
        p = if delta { d } else { d + bone.pos };
    }
    if delta {
        q = bone.quat * q;
        p += bone.pos;
    }
    let q = if q.is_finite() && q.length_squared() > 1e-8 { q.normalize() } else { bone.quat };
    let p = if p.is_finite() { p } else { bone.pos };
    Ok((p, q))
}

/// Component `k` of the run-length-encoded values that the pointer record
/// at `ptr` points to, at `frame`; 0 when the component doesn't move.
/// Runs are a (valid, total) byte pair and `valid` 16-bit values: the
/// next `total` frames take those values, the last repeating.
fn value(data: Bytes, ptr: usize, k: usize, frame: usize) -> Result<f32> {
    let offset = data.i16(ptr + 2 * k)?;
    if offset <= 0 {
        return Ok(0.0);
    }
    let mut at = ptr + offset as usize;
    let mut k = frame;
    for _ in 0..=MAX_FRAMES * 2 {
        let (valid, total) = (data.u8(at)? as usize, data.u8(at + 1)? as usize);
        if total == 0 {
            return Ok(0.0);
        }
        if total > k {
            let i = if valid > k { k + 1 } else { valid };
            return Ok(data.i16(at + 2 * i)? as f32);
        }
        k -= total;
        at += 2 * (valid + 1);
    }
    Err(LoadError::invalid("animation runs never reach the frame"))
}

/// A quaternion packed in 48 bits: x and y in 16 bits each, z in 15, and the
/// sign of w; w follows from the others.
pub fn quat48(v: &[u8]) -> Quat {
    let x = u16::from_le_bytes([v[0], v[1]]) as f32;
    let y = u16::from_le_bytes([v[2], v[3]]) as f32;
    let zw = u16::from_le_bytes([v[4], v[5]]);
    let (x, y, z) = ((x - 32768.0) / 32768.0, (y - 32768.0) / 32768.0, ((zw & 0x7fff) as f32 - 16384.0) / 16384.0);
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    Quat::from_xyzw(x, y, z, if zw & 0x8000 != 0 { -w } else { w })
}

/// A quaternion packed in 64 bits: x, y and z in 21 bits each, and the
/// sign of w.
pub fn quat64(v: &[u8]) -> Quat {
    let bits = u64::from_le_bytes(v[..8].try_into().unwrap_or([0; 8]));
    let part = |shift: u32| (((bits >> shift) & 0x1f_ffff) as f32 - 1_048_576.0) / 1_048_576.5;
    let (x, y, z) = (part(0), part(21), part(42));
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    Quat::from_xyzw(x, y, z, if bits >> 63 != 0 { -w } else { w })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_quaternions() {
        // The identity: x, y and z at their midpoints, w positive.
        let q = quat48(&[0, 0x80, 0, 0x80, 0, 0x40]);
        assert!(q.abs_diff_eq(Quat::IDENTITY, 1e-4), "{q:?}");
        let q = quat64(&(1u64 << 20 | 1 << 41 | 1 << 62).to_le_bytes());
        assert!(q.abs_diff_eq(Quat::IDENTITY, 1e-4), "{q:?}");
        // A quarter turn about Z with w negative: z = -sqrt(0.5) and w =
        // -sqrt(0.5), the same rotation.
        let z = ((-std::f32::consts::FRAC_1_SQRT_2 * 16384.0) + 16384.0).round() as u16;
        let mut b = [0, 0x80, 0, 0x80, 0, 0];
        b[4..6].copy_from_slice(&(z | 0x8000).to_le_bytes());
        let q = quat48(&b);
        let want = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        assert!(q.dot(want).abs() > 0.9999, "{q:?}");
    }

    #[test]
    fn run_length_values() {
        // A pointer record with one component at offset 6: a run of 3
        // frames with 2 values (10, 20; 20 repeats), then a run of 2 with
        // one (-5).
        let mut d = vec![0u8; 6];
        d[0..2].copy_from_slice(&6i16.to_le_bytes());
        d.extend_from_slice(&[2, 3]);
        d.extend_from_slice(&10i16.to_le_bytes());
        d.extend_from_slice(&20i16.to_le_bytes());
        d.extend_from_slice(&[1, 2]);
        d.extend_from_slice(&(-5i16).to_le_bytes());
        let b = Bytes::new(&d);
        let got: Vec<f32> = (0..5).map(|f| value(b, 0, 0, f).unwrap()).collect();
        assert_eq!(got, [10.0, 20.0, 20.0, -5.0, -5.0]);
        assert_eq!(value(b, 0, 1, 3).unwrap(), 0.0, "an empty pointer is a still component");
        assert!(value(b, 0, 0, 9).is_err(), "past the last run");
    }
}
