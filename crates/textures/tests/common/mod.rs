//! Builders for texture files, shared by the integration tests.
#![allow(dead_code)]

use slerp_textures::{Pixels, Texture};

/*┌─────────────────╮
| Builders        |
└─────────────────╯*/
//region Builders

pub struct Ktx2 {
    pub vk_format: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layers: u32,
    pub faces: u32,
    pub scheme: u32,
    /// Level data, largest first, already supercompressed if `scheme` says.
    pub levels: Vec<(Vec<u8>, u64)>,
    pub kv: Vec<(&'static str, &'static [u8])>,
    /// The descriptor's colour model: 1 is RGB, 2 YUV.
    pub model: u8,
}

impl Ktx2 {
    pub fn new(vk_format: u32, width: u32, height: u32, level0: Vec<u8>) -> Self {
        let n = level0.len() as u64;
        Ktx2 {
            vk_format,
            width,
            height,
            depth: 0,
            layers: 0,
            faces: 1,
            scheme: 0,
            levels: vec![(level0, n)],
            kv: vec![],
            model: 1,
        }
    }

    pub fn build(&self) -> Vec<u8> {
        let mut kvd = Vec::new();
        for (k, v) in &self.kv {
            let len = k.len() + 1 + v.len();
            kvd.extend((len as u32).to_le_bytes());
            kvd.extend(k.as_bytes());
            kvd.push(0);
            kvd.extend(*v);
            kvd.resize(kvd.len().div_ceil(4) * 4, 0);
        }
        // A minimal descriptor: total size, one basic block without samples.
        let mut dfd = 28u32.to_le_bytes().to_vec();
        dfd.extend([0; 4]);
        dfd.extend([2, 0, 24, 0]);
        dfd.extend([self.model, 1, 1, 0]);
        dfd.extend([0; 16]);
        let index_at = 80;
        let dfd_at = index_at + 24 * self.levels.len();
        let kvd_at = dfd_at + dfd.len();
        let mut data_at = (kvd_at + kvd.len()).div_ceil(16) * 16;
        let mut f = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
        for v in [
            self.vk_format,
            1,
            self.width,
            self.height,
            self.depth,
            self.layers,
            self.faces,
            self.levels.len() as u32,
            self.scheme,
        ] {
            f.extend(v.to_le_bytes());
        }
        for v in [dfd_at as u32, dfd.len() as u32, kvd_at as u32, kvd.len() as u32] {
            f.extend(v.to_le_bytes());
        }
        f.extend([0; 16]);
        for (data, raw) in &self.levels {
            f.extend((data_at as u64).to_le_bytes());
            f.extend((data.len() as u64).to_le_bytes());
            f.extend(raw.to_le_bytes());
            data_at += data.len().div_ceil(16) * 16;
        }
        f.extend(&dfd);
        f.extend(&kvd);
        for (data, _) in &self.levels {
            f.resize(f.len().div_ceil(16) * 16, 0);
            f.extend(data);
        }
        f
    }
}

pub fn rgba8(tex: &Texture, level: usize, surface: usize) -> &[u8] {
    match &tex.levels[level].surfaces[surface] {
        Pixels::Rgba8(v) => v,
        _ => panic!("expected 8-bit texels"),
    }
}

pub fn rgba32f(tex: &Texture) -> &[f32] {
    match tex.base() {
        Pixels::Rgba32F(v) => v,
        _ => panic!("expected float texels"),
    }
}

/// A BC1 block with two RGB565 endpoints and one 2-bit index per texel.
pub fn bc1(c0: u16, c1: u16, indices: u32) -> Vec<u8> {
    [c0.to_le_bytes().as_slice(), &c1.to_le_bytes(), &indices.to_le_bytes()].concat()
}

/// An ASTC LDR void-extent block: one 16-bit-per-channel colour.
pub fn astc_solid(c: [u16; 4]) -> Vec<u8> {
    let mut b = vec![0xfc, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    for v in c {
        b.extend(v.to_le_bytes());
    }
    b
}

/// The same with half-float components (HDR).
pub fn astc_solid_hdr(c: [u16; 4]) -> Vec<u8> {
    let mut b = astc_solid(c);
    b[1] = 0xff;
    b
}

/// A KTX1 file with one level holding `faces` images.
pub fn ktx1(gl: [u32; 4], w: u32, h: u32, faces: u32, kv: &[u8], images: &[Vec<u8>]) -> Vec<u8> {
    let mut f = b"\xabKTX 11\xbb\r\n\x1a\n".to_vec();
    let [gl_type, gl_format, internal, base] = gl;
    for v in [0x0403_0201, gl_type, 1, gl_format, internal, base, w, h, 0, 0, faces, 1, kv.len() as u32] {
        f.extend(u32::to_le_bytes(v));
    }
    f.extend(kv);
    f.extend((images[0].len() as u32).to_le_bytes());
    for img in images {
        f.extend(img);
        f.resize(f.len().div_ceil(4) * 4, 0);
    }
    f
}

/// A DDS header; `dx10` adds the extension with this DXGI format, array
/// size and misc flags.
pub fn dds(
    w: u32,
    h: u32,
    mips: u32,
    pf: (u32, &[u8; 4], u32, [u32; 4]),
    caps2: u32,
    dx10: Option<(u32, u32, u32)>,
) -> Vec<u8> {
    let mut f = b"DDS ".to_vec();
    for v in [124, 0x1007, h, w, 0, 1, mips] {
        f.extend(u32::to_le_bytes(v));
    }
    f.extend([0; 44]);
    f.extend(32u32.to_le_bytes());
    f.extend(pf.0.to_le_bytes());
    f.extend(pf.1);
    f.extend(pf.2.to_le_bytes());
    for m in pf.3 {
        f.extend(m.to_le_bytes());
    }
    for v in [0x1000, caps2, 0, 0, 0] {
        f.extend(u32::to_le_bytes(v));
    }
    if let Some((format, array, misc)) = dx10 {
        for v in [format, 3, misc, array, 0] {
            f.extend(u32::to_le_bytes(v));
        }
    }
    f
}

pub const FOURCC: u32 = 4;

pub fn astc_file(bw: u8, bh: u8, bd: u8, w: u32, h: u32, blocks: &[u8]) -> Vec<u8> {
    let mut f = vec![0x13, 0xab, 0xa1, 0x5c, bw, bh, bd];
    for d in [w, h, 1] {
        f.extend(&d.to_le_bytes()[..3]);
    }
    f.extend(blocks);
    f
}

//endregion
