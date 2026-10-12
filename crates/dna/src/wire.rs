//! `DnaData` as bytes, for the trip from the reading process to the one
//! that builds the scene (see `sandbox`).  Decoding checks every length
//! against the bytes left, so a garbled message is an error, not a crash
//! or a huge allocation.

use crate::extract::{DnaData, MeshData, TargetData};

/// Marks a complete message: a child that died while writing leaves it
/// out.
const END: u32 = 0x444e_4121;

pub(crate) fn encode(d: &DnaData) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(1 << 20));
    w.str(&d.name);
    w.u32(d.lod_count as u32);
    w.u32s(&d.conventions);
    w.strs(&d.joint_names);
    w.u16s(&d.joint_parents);
    w.f32s(&d.neutral_joints);
    w.len(d.lod_joints.len());
    d.lod_joints.iter().for_each(|l| w.u16s(l));
    w.strs(&d.controls);
    w.strs(&d.channels);
    w.u32(d.animated_maps as u32);
    w.strs(&d.mesh_names);
    w.len(d.lod_meshes.len());
    d.lod_meshes.iter().for_each(|l| w.u16s(l));
    w.len(d.meshes.len());
    for (index, m) in &d.meshes {
        w.u32(*index as u32);
        w.f32s(&m.positions);
        w.f32s(&m.normals);
        w.f32s(&m.uvs);
        w.u32s(&m.layouts);
        w.u32s(&m.faces);
        w.u32s(&m.skin_counts);
        w.u16s(&m.skin_joints);
        w.f32s(&m.skin_weights);
        w.len(m.targets.len());
        for t in &m.targets {
            w.u32(t.channel as u32);
            w.u32s(&t.vertices);
            w.f32s(&t.deltas);
        }
    }
    w.u64(d.peak_bytes);
    w.u32(END);
    w.0
}

pub(crate) fn decode(bytes: &[u8]) -> Result<DnaData, String> {
    let mut r = Reader(bytes);
    let mut d = DnaData { name: r.str()?, lod_count: r.u16()?, conventions: r.u32s()?, ..Default::default() };
    d.joint_names = r.strs()?;
    d.joint_parents = r.u16s()?;
    d.neutral_joints = r.f32s()?;
    let n = r.count(4)?;
    d.lod_joints = (0..n).map(|_| r.u16s()).collect::<Result<_, _>>()?;
    d.controls = r.strs()?;
    d.channels = r.strs()?;
    d.animated_maps = r.u16()?;
    d.mesh_names = r.strs()?;
    let n = r.count(4)?;
    d.lod_meshes = (0..n).map(|_| r.u16s()).collect::<Result<_, _>>()?;
    let n = r.count(4)?;
    for _ in 0..n {
        let index = r.u16()?;
        let mut m = MeshData {
            positions: r.f32s()?,
            normals: r.f32s()?,
            uvs: r.f32s()?,
            layouts: r.u32s()?,
            faces: r.u32s()?,
            skin_counts: r.u32s()?,
            skin_joints: r.u16s()?,
            skin_weights: r.f32s()?,
            targets: Vec::new(),
        };
        let targets = r.count(4)?;
        for _ in 0..targets {
            m.targets.push(TargetData { channel: r.u16()?, vertices: r.u32s()?, deltas: r.f32s()? });
        }
        d.meshes.push((index, m));
    }
    d.peak_bytes = r.u64()?;
    if r.u32()? != END || !r.0.is_empty() {
        return Err("the reader's answer was cut short".into());
    }
    Ok(d)
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn len(&mut self, n: usize) {
        self.u64(n as u64);
    }

    fn f32s(&mut self, v: &[f32]) {
        self.len(v.len());
        v.iter().for_each(|x| self.0.extend_from_slice(&x.to_le_bytes()));
    }

    fn u32s(&mut self, v: &[u32]) {
        self.len(v.len());
        v.iter().for_each(|x| self.0.extend_from_slice(&x.to_le_bytes()));
    }

    fn u16s(&mut self, v: &[u16]) {
        self.len(v.len());
        v.iter().for_each(|x| self.0.extend_from_slice(&x.to_le_bytes()));
    }

    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }

    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        v.iter().for_each(|s| self.str(s));
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if n > self.0.len() {
            return Err("the reader's answer was cut short".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u16(&mut self) -> Result<u16, String> {
        u16::try_from(self.u32()?).map_err(|_| "a count in the reader's answer is out of range".to_string())
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    /// A length prefix, checked against the bytes left at `size` bytes per
    /// element (at least one, so empty elements can't claim billions).
    fn count(&mut self, size: usize) -> Result<usize, String> {
        let n = self.u64()?;
        if n > (self.0.len() / size.max(1)) as u64 {
            return Err("a length in the reader's answer is larger than the answer".into());
        }
        Ok(n as usize)
    }

    fn f32s(&mut self) -> Result<Vec<f32>, String> {
        let n = self.count(4)?;
        Ok(self.take(n * 4)?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().expect("four"))).collect())
    }

    fn u32s(&mut self) -> Result<Vec<u32>, String> {
        let n = self.count(4)?;
        Ok(self.take(n * 4)?.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().expect("four"))).collect())
    }

    fn u16s(&mut self) -> Result<Vec<u16>, String> {
        let n = self.count(2)?;
        Ok(self.take(n * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes(c.try_into().expect("two"))).collect())
    }

    fn str(&mut self) -> Result<String, String> {
        let n = self.count(1)?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn strs(&mut self) -> Result<Vec<String>, String> {
        let n = self.count(8)?;
        (0..n).map(|_| self.str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DnaData {
        DnaData {
            name: "Tate".into(),
            lod_count: 2,
            conventions: vec![0, 0, 0, 2, 4, 0, 0],
            joint_names: vec!["root".into(), "head".into()],
            joint_parents: vec![0, 0],
            neutral_joints: vec![0.0; 12],
            lod_joints: vec![vec![0, 1], vec![0]],
            controls: vec!["CTRL_expressions.jawOpen".into()],
            channels: vec!["jaw_open".into()],
            animated_maps: 3,
            mesh_names: vec!["head_lod0_mesh".into()],
            lod_meshes: vec![vec![0], vec![]],
            meshes: vec![(
                0,
                MeshData {
                    positions: vec![1.0, 2.0, 3.0],
                    layouts: vec![0, 0, 0],
                    targets: vec![TargetData { channel: 0, vertices: vec![0], deltas: vec![0.5, 0.0, 0.0] }],
                    ..Default::default()
                },
            )],
            peak_bytes: 1234,
        }
    }

    #[test]
    fn round_trips() {
        let d = sample();
        assert_eq!(decode(&encode(&d)).unwrap(), d);
    }

    #[test]
    fn rejects_every_truncation_and_garbled_lengths() {
        let bytes = encode(&sample());
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err(), "accepted {n} of {} bytes", bytes.len());
        }
        // A huge length anywhere must fail without allocating it.
        for at in 0..bytes.len().saturating_sub(8) {
            let mut b = bytes.clone();
            b[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            let _ = decode(&b);
        }
    }
}
