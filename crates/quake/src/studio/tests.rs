use super::*;
use crate::QuakeFormat;
use crate::testutil::{self, asset};
use slerp_scene::Property;
use slerp_scene::glam::{Quat, Vec2};
use slerp_scene::textures::Pixels;
use std::path::PathBuf;

/*┌─────────────────╮
| Writing models  |
└─────────────────╯*/
//region Writing models

/// A model for the tests to write as `.mdl`, `.vvd` and `.vtx` files, with
/// the layouts the loader reads.
struct Synth {
    version: i32,
    bones: Vec<(&'static str, i32, Vec3, Quat)>,
    textures: Vec<&'static str>,
    folders: Vec<&'static str>,
    /// Skin families: texture per skin reference.
    skins: Vec<Vec<i16>>,
    parts: Vec<(&'static str, Vec<SynthModel>)>,
    flexdescs: Vec<&'static str>,
    /// Sequences, each with one animation: a name and its frames' records.
    anims: Vec<(&'static str, usize, Vec<u8>)>,
    /// Write strip groups and strips with the later games' extra fields.
    extended_vtx: bool,
    /// Store the vertices in reverse behind a fixup table.
    fixups: bool,
}

struct SynthModel {
    name: &'static str,
    meshes: Vec<SynthMesh>,
}

/// A vertex: position, normal, uv, and (bone, weight) pairs.
type SynthVertex = (Vec3, Vec3, Vec2, Vec<(u8, f32)>);

/// A flex: descriptor, pair (0 for none), and (vertex, delta, side).
type SynthFlex = (i32, i32, Vec<(u16, Vec3, u8)>);

struct SynthMesh {
    material: i32,
    verts: Vec<SynthVertex>,
    tris: Vec<[u16; 3]>,
    flexes: Vec<SynthFlex>,
}

/// A little-endian byte buffer with fields patched in later.
#[derive(Default)]
struct Buf {
    b: Vec<u8>,
    /// String fields: where the offset goes, what it counts from, the text.
    strings: Vec<(usize, usize, String)>,
}

impl Buf {
    fn at(&self) -> usize {
        self.b.len()
    }
    fn i32(&mut self, v: i32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn vec3(&mut self, v: Vec3) {
        v.to_array().iter().for_each(|&f| self.f32(f));
    }
    fn zeros(&mut self, n: usize) {
        self.b.extend(std::iter::repeat_n(0, n));
    }
    fn set_i32(&mut self, at: usize, v: i32) {
        self.b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// A string field at `at` whose offset counts from `base`.
    fn string(&mut self, at: usize, base: usize, text: &str) {
        self.strings.push((at, base, text.to_string()));
    }
    fn finish(mut self) -> Vec<u8> {
        for (at, base, text) in std::mem::take(&mut self.strings) {
            let here = self.at();
            self.b.extend_from_slice(text.as_bytes());
            self.b.push(0);
            self.set_i32(at, (here - base) as i32);
        }
        self.b
    }
}

fn half(v: f32) -> u16 {
    // Exact for the small values the tests use.
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    if v == 0.0 {
        return sign;
    }
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = ((bits >> 13) & 0x3ff) as u16;
    sign | ((exp as u16) << 10) | mant
}

impl Synth {
    /// One bone, one body part with one model: a quad of two triangles.
    fn quad() -> Synth {
        let v = |x: f32, y: f32| (Vec3::new(x, y, 0.0), Vec3::Z, Vec2::new(x, y), vec![(0u8, 1.0f32)]);
        Synth {
            version: 48,
            bones: vec![("root", -1, Vec3::ZERO, Quat::IDENTITY)],
            textures: vec!["skin_a"],
            folders: vec!["models\\test\\"],
            skins: vec![vec![0]],
            parts: vec![(
                "body",
                vec![SynthModel {
                    name: "quad",
                    meshes: vec![SynthMesh {
                        material: 0,
                        verts: vec![v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)],
                        // Clockwise seen from +Z, as Source winds them.
                        tris: vec![[0, 2, 1], [0, 3, 2]],
                        flexes: Vec::new(),
                    }],
                }],
            )],
            flexdescs: Vec::new(),
            anims: Vec::new(),
            extended_vtx: false,
            fixups: false,
        }
    }

    fn all_verts(&self) -> Vec<SynthVertex> {
        let mut out = Vec::new();
        for (_, models) in &self.parts {
            for m in models {
                for k in &m.meshes {
                    out.extend(k.verts.iter().cloned());
                }
            }
        }
        out
    }

    fn mdl(&self) -> Vec<u8> {
        let mut w = Buf::default();
        w.b.extend_from_slice(b"IDST");
        w.i32(self.version);
        w.i32(1234);
        w.zeros(mdl::HEADER_BYTES - 12);
        w.b[12..16].copy_from_slice(b"test");
        w.set_i32(152, 0);
        // Bones.
        let bones_at = w.at();
        w.set_i32(156, self.bones.len() as i32);
        w.set_i32(160, bones_at as i32);
        for (name, parent, pos, q) in &self.bones {
            let at = w.at();
            w.i32(0);
            w.string(at, at, name);
            w.i32(*parent);
            (0..6).for_each(|_| w.i32(-1));
            w.vec3(*pos);
            q.to_array().iter().for_each(|&f| w.f32(f));
            let (z, y, x) = q.to_euler(slerp_scene::glam::EulerRot::ZYX);
            w.vec3(Vec3::new(x, y, z));
            w.vec3(Vec3::splat(1.0 / 32.0));
            w.vec3(Vec3::splat(std::f32::consts::PI / 32768.0));
            w.zeros(216 - 96);
        }
        // Textures and their folders.
        let tex_at = w.at();
        w.set_i32(204, self.textures.len() as i32);
        w.set_i32(208, tex_at as i32);
        for t in &self.textures {
            let at = w.at();
            w.i32(0);
            w.string(at, at, t);
            w.zeros(60);
        }
        let cd_at = w.at();
        w.set_i32(212, self.folders.len() as i32);
        w.set_i32(216, cd_at as i32);
        for f in &self.folders {
            let at = w.at();
            w.i32(0);
            w.string(at, 0, f);
        }
        let skin_at = w.at();
        w.set_i32(220, self.skins.first().map_or(0, |s| s.len()) as i32);
        w.set_i32(224, self.skins.len() as i32);
        w.set_i32(228, skin_at as i32);
        for row in &self.skins {
            for &t in row {
                w.b.extend_from_slice(&t.to_le_bytes());
            }
        }
        // Flex descriptors.
        let fd_at = w.at();
        w.set_i32(260, self.flexdescs.len() as i32);
        w.set_i32(264, fd_at as i32);
        for name in &self.flexdescs {
            let at = w.at();
            w.i32(0);
            w.string(at, at, name);
        }
        // Body parts, then their models, then the models' meshes.
        let bp_at = w.at();
        w.set_i32(232, self.parts.len() as i32);
        w.set_i32(236, bp_at as i32);
        w.zeros(16 * self.parts.len());
        let mut vertex_index = 0usize;
        for (p, (name, models)) in self.parts.iter().enumerate() {
            let at = bp_at + 16 * p;
            w.string(at, at, name);
            w.set_i32(at + 4, models.len() as i32);
            let models_at = w.at();
            w.set_i32(at + 12, (models_at - at) as i32);
            w.zeros(148 * models.len());
            for (m, model) in models.iter().enumerate() {
                let ma = models_at + 148 * m;
                w.b[ma..ma + model.name.len()].copy_from_slice(model.name.as_bytes());
                let count: usize = model.meshes.iter().map(|k| k.verts.len()).sum();
                w.set_i32(ma + 72, model.meshes.len() as i32);
                w.set_i32(ma + 80, count as i32);
                w.set_i32(ma + 84, (vertex_index * 48) as i32);
                let meshes_at = w.at();
                w.set_i32(ma + 76, (meshes_at - ma) as i32);
                w.zeros(116 * model.meshes.len());
                let mut offset = 0;
                for (k, mesh) in model.meshes.iter().enumerate() {
                    let me = meshes_at + 116 * k;
                    w.set_i32(me, mesh.material);
                    w.set_i32(me + 8, mesh.verts.len() as i32);
                    w.set_i32(me + 12, offset as i32);
                    offset += mesh.verts.len();
                    if !mesh.flexes.is_empty() {
                        let flex_at = w.at();
                        w.set_i32(me + 16, mesh.flexes.len() as i32);
                        w.set_i32(me + 20, (flex_at - me) as i32);
                        w.zeros(60 * mesh.flexes.len());
                        for (f, (desc, pair, verts)) in mesh.flexes.iter().enumerate() {
                            let fa = flex_at + 60 * f;
                            w.set_i32(fa, *desc);
                            w.set_i32(fa + 20, verts.len() as i32);
                            w.set_i32(fa + 24, (w.at() - fa) as i32);
                            w.set_i32(fa + 28, *pair);
                            for (i, d, side) in verts {
                                w.b.extend_from_slice(&i.to_le_bytes());
                                w.b.extend_from_slice(&[0, *side]);
                                for c in d.to_array() {
                                    w.b.extend_from_slice(&half(c).to_le_bytes());
                                }
                                w.zeros(6);
                            }
                        }
                    }
                }
                vertex_index += count;
            }
        }
        // Animations and their sequences.
        let ad_at = w.at();
        w.set_i32(180, self.anims.len() as i32);
        w.set_i32(184, ad_at as i32);
        w.zeros(100 * self.anims.len());
        for (i, (name, frames, data)) in self.anims.iter().enumerate() {
            let at = ad_at + 100 * i;
            w.set_i32(at, -(at as i32));
            w.string(at + 4, at, name);
            w.set_i32(at + 8, 10.0f32.to_bits() as i32);
            w.set_i32(at + 16, *frames as i32);
            let data_at = w.at();
            w.set_i32(at + 56, (data_at - at) as i32);
            w.b.extend_from_slice(data);
        }
        let sd_at = w.at();
        w.set_i32(188, self.anims.len() as i32);
        w.set_i32(192, sd_at as i32);
        w.zeros(212 * self.anims.len());
        for (i, (name, _, _)) in self.anims.iter().enumerate() {
            let at = sd_at + 212 * i;
            w.string(at + 4, at, name);
            let grid = w.at();
            w.set_i32(at + 60, (grid - at) as i32);
            w.b.extend_from_slice(&(i as i16).to_le_bytes());
            w.b.extend_from_slice(&[0, 0]);
        }
        let mut out = w.finish();
        let len = out.len() as i32;
        out[76..80].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn vvd(&self) -> Vec<u8> {
        let verts = self.all_verts();
        let mut w = Buf::default();
        w.b.extend_from_slice(b"IDSV");
        w.i32(4);
        w.i32(1234);
        w.i32(1);
        w.i32(verts.len() as i32);
        w.zeros(28);
        let fixups = if self.fixups { verts.len() } else { 0 };
        w.i32(fixups as i32);
        w.i32(64);
        w.i32((64 + 12 * fixups) as i32);
        w.i32(0);
        assert_eq!(w.at(), 64);
        // Reversed storage, one fixup per vertex putting them back.
        for i in 0..fixups {
            w.i32(0);
            w.i32((verts.len() - 1 - i) as i32);
            w.i32(1);
        }
        let order: Vec<usize> = if self.fixups { (0..verts.len()).rev().collect() } else { (0..verts.len()).collect() };
        for i in order {
            let (p, n, uv, bw) = &verts[i];
            let mut weights = [0.0f32; 3];
            let mut bones = [0u8; 3];
            for (j, (b, wt)) in bw.iter().enumerate().take(3) {
                bones[j] = *b;
                weights[j] = *wt;
            }
            weights.iter().for_each(|&x| w.f32(x));
            w.b.extend_from_slice(&bones);
            w.b.push(bw.len() as u8);
            w.vec3(*p);
            w.vec3(*n);
            w.f32(uv.x);
            w.f32(uv.y);
        }
        w.finish()
    }

    fn vtx(&self) -> Vec<u8> {
        let (sg_bytes, strip_bytes) = if self.extended_vtx { (33, 35) } else { (25, 27) };
        let mut w = Buf::default();
        w.i32(7);
        w.i32(24);
        w.b.extend_from_slice(&[3, 0, 3, 0]);
        w.i32(3);
        w.i32(1234);
        w.i32(1);
        w.i32(0);
        w.i32(self.parts.len() as i32);
        w.i32(36);
        w.zeros(8 * self.parts.len());
        for (p, (_, models)) in self.parts.iter().enumerate() {
            let pa = 36 + 8 * p;
            let models_at = w.at();
            w.set_i32(pa, models.len() as i32);
            w.set_i32(pa + 4, (models_at - pa) as i32);
            w.zeros(8 * models.len());
            for (m, model) in models.iter().enumerate() {
                let ma = models_at + 8 * m;
                let lod = w.at();
                w.set_i32(ma, 1);
                w.set_i32(ma + 4, (lod - ma) as i32);
                w.i32(model.meshes.len() as i32);
                w.i32(12);
                w.f32(0.0);
                let meshes_at = w.at();
                w.zeros(9 * model.meshes.len());
                for (k, mesh) in model.meshes.iter().enumerate() {
                    let me = meshes_at + 9 * k;
                    let sg = w.at();
                    w.set_i32(me, 1);
                    w.set_i32(me + 4, (sg - me) as i32);
                    w.zeros(sg_bytes);
                    let verts_at = w.at();
                    for v in 0..mesh.verts.len() {
                        w.b.extend_from_slice(&[0, 1, 2, 1]);
                        w.b.extend_from_slice(&(v as u16).to_le_bytes());
                        w.b.extend_from_slice(&[0, 0, 0]);
                    }
                    let index_at = w.at();
                    for t in &mesh.tris {
                        for &i in t {
                            w.b.extend_from_slice(&i.to_le_bytes());
                        }
                    }
                    let strip_at = w.at();
                    w.i32((mesh.tris.len() * 3) as i32);
                    w.i32(0);
                    w.i32(mesh.verts.len() as i32);
                    w.i32(0);
                    w.b.extend_from_slice(&[1, 0, 1]);
                    w.zeros(strip_bytes - 19);
                    w.set_i32(sg, mesh.verts.len() as i32);
                    w.set_i32(sg + 4, (verts_at - sg) as i32);
                    w.set_i32(sg + 8, (mesh.tris.len() * 3) as i32);
                    w.set_i32(sg + 12, (index_at - sg) as i32);
                    w.set_i32(sg + 16, 1);
                    w.set_i32(sg + 20, (strip_at - sg) as i32);
                }
            }
        }
        w.finish()
    }

    /// Writes the three files into a fresh folder and returns the `.mdl`.
    fn write(&self, dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let mdl = dir.join(format!("{name}.mdl"));
        std::fs::write(&mdl, self.mdl()).unwrap();
        std::fs::write(dir.join(format!("{name}.vvd")), self.vvd()).unwrap();
        std::fs::write(dir.join(format!("{name}.dx90.vtx")), self.vtx()).unwrap();
        mdl
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-studio-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn load_file(path: &Path, limits: &Limits) -> Result<Scene> {
    load(path, &std::fs::read(path).unwrap(), limits, 0)
}

//endregion

/*┌─────────────────╮
| Synthetic       |
└─────────────────╯*/
//region Synthetic

#[test]
fn quad_loads_converted_and_wound_outward() {
    let dir = temp("quad");
    let path = Synth::quad().write(&dir, "quad");
    let s = load_file(&path, &Limits::viewer()).unwrap();
    assert_eq!(s.triangle_count(), 2);
    let prim = &s.meshes[0].primitives[0];
    // Source +Z is up: the quad lies flat at y = 0, 10 inches = 0.254 m.
    for p in &prim.positions {
        assert!(p.y.abs() < 1e-6, "{p}");
    }
    let max = prim.positions.iter().fold(Vec3::ZERO, |a, &p| a.max(p));
    assert!((max.x - 0.254).abs() < 1e-5, "{max}");
    // Counter-clockwise seen from above, where the normals point.
    let t = &prim.indices;
    let (a, b, c) = (prim.positions[t[0] as usize], prim.positions[t[1] as usize], prim.positions[t[2] as usize]);
    assert!((b - a).cross(c - a).y > 0.0);
    assert!(prim.normals.iter().all(|n| (n.y - 1.0).abs() < 1e-5));
    assert_eq!(prim.joints[0], [0, 0, 0, 0]);
    assert!(s.info.warnings.iter().any(|w| w.contains("no materials folder")), "{:?}", s.info.warnings);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fixups_and_extended_vtx_read_the_same() {
    let dir = temp("variants");
    let plain = load_file(&Synth::quad().write(&dir, "plain"), &Limits::viewer()).unwrap();
    let mut s = Synth::quad();
    s.fixups = true;
    s.extended_vtx = true;
    s.version = 49;
    let other = load_file(&s.write(&dir, "other"), &Limits::viewer()).unwrap();
    let (a, b) = (&plain.meshes[0].primitives[0], &other.meshes[0].primitives[0]);
    assert_eq!(a.positions, b.positions);
    assert_eq!(a.indices, b.indices);
    // The extended layout also reads when tried second.
    let vtx = s.vtx();
    let first = vtx::read(&vtx, false).unwrap();
    assert_eq!(first.parts[0][0][0], vtx::read(&vtx, true).unwrap().parts[0][0][0]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn skins_body_groups_and_materials() {
    let dir = temp("skins");
    let mut s = Synth::quad();
    s.textures = vec!["skin_a", "skin_b"];
    s.skins = vec![vec![0], vec![1]];
    let second = SynthModel {
        name: "alternative",
        meshes: vec![SynthMesh { material: 0, ..Synth::quad().parts.remove(0).1.remove(0).meshes.remove(0) }],
    };
    s.parts[0].1.push(second);
    let path = s.write(&dir.join("models/test"), "box");
    // Materials and textures in the game's folders, found from the model.
    let vtf = crate::vtf::tests::build(3, 0, 4, 4, 0, &[vec![200u8; 64]]);
    let bump = crate::vtf::tests::build(3, 0, 4, 4, 0, &[[128u8, 32, 255, 255].repeat(16)]);
    let mats = dir.join("materials/models/test");
    std::fs::create_dir_all(&mats).unwrap();
    std::fs::write(mats.join("skin_a.vmt"), "VertexLitGeneric { $basetexture models/test/a $bumpmap models/test/a_n }")
        .unwrap();
    std::fs::write(
        mats.join("skin_b.vmt"),
        "\"VertexLitGeneric\" { \"$basetexture\" \"models\\test\\a\" \"$nocull\" 1 }",
    )
    .unwrap();
    std::fs::write(mats.join("a.vtf"), &vtf).unwrap();
    std::fs::write(mats.join("a_n.vtf"), &bump).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(parts(&bytes), ["Skin 1", "Skin 2"]);
    assert_eq!(family_of("Skin 2"), Some(1));
    let a = load(&path, &bytes, &Limits::viewer(), 0).unwrap();
    assert!(a.info.warnings.is_empty(), "{:?}", a.info.warnings);
    let m = &a.materials[a.meshes[0].primitives[0].material.unwrap()];
    assert_eq!(m.name, "models/test/skin_a");
    assert!(m.base_color_texture.is_some() && m.normal_texture.is_some() && !m.double_sided);
    let Pixels::Rgba8(n) = a.textures[m.normal_texture.unwrap().texture].base() else { panic!() };
    assert_eq!(n[1], 255 - 32, "Direct3D green is flipped");
    let b = load(&path, &bytes, &Limits::viewer(), 1).unwrap();
    let m = &b.materials[b.meshes[0].primitives[0].material.unwrap()];
    assert_eq!(m.name, "models/test/skin_b");
    assert!(m.double_sided);
    // The second model of the body part is there, hidden.
    let meshes: Vec<(&str, bool)> =
        a.nodes.iter().filter(|n| n.mesh.is_some()).map(|n| (n.name.as_str(), n.visible)).collect();
    assert_eq!(meshes, [("quad", true), ("alternative", false)]);
    // Thumbnails skip normal maps.
    let t = load(&path, &bytes, &Limits::thumbnail(), 0).unwrap();
    assert!(t.materials.iter().all(|m| m.normal_texture.is_none()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn flexes_become_morph_targets() {
    let dir = temp("flex");
    let mut s = Synth::quad();
    s.flexdescs = vec!["smile", "blink_l", "blink_r"];
    let mesh = &mut s.parts[0].1[0].meshes[0];
    mesh.flexes =
        vec![(0, 0, vec![(2, Vec3::new(0.0, 0.0, 4.0), 0)]), (1, 2, vec![(1, Vec3::new(0.0, 0.0, 2.0), 255)])];
    let path = s.write(&dir, "face");
    let sc = load_file(&path, &Limits::viewer()).unwrap();
    assert_eq!(sc.meshes[0].target_names, ["smile", "blink_l", "blink_r"]);
    let prim = &sc.meshes[0].primitives[0];
    assert_eq!(prim.targets.len(), 3);
    // Vertex 2 (10, 10) rises 4 inches with "smile"; vertex 1 belongs wholly
    // to the right side of the blink pair.
    let i2 = prim.positions.iter().position(|p| (p.x - 0.254).abs() < 1e-5 && (p.z + 0.254).abs() < 1e-5).unwrap();
    assert!((prim.targets[0].positions[i2] - Vec3::new(0.0, 4.0 * 0.0254, 0.0)).length() < 1e-5);
    let i1 = prim.positions.iter().position(|p| (p.x - 0.254).abs() < 1e-5 && p.z.abs() < 1e-5).unwrap();
    assert!(prim.targets[1].positions[i1].length() < 1e-6);
    assert!((prim.targets[2].positions[i1].y - 2.0 * 0.0254).abs() < 1e-5);
    assert!(load_file(&path, &Limits::thumbnail()).unwrap().meshes[0].primitives[0].targets.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// One frame record per frame of bone 1: rotation as run-length Euler
/// angles about Z (0, then 16384 x rotscale), position constant.
fn turning_anim() -> Vec<u8> {
    let mut d = Vec::new();
    d.push(1u8);
    d.push(0x08 | 0x01);
    d.extend_from_slice(&0i16.to_le_bytes());
    // The pointer record: x and y still, z at offset 6.
    let ptr = d.len();
    d.extend_from_slice(&[0, 0, 0, 0]);
    d.extend_from_slice(&0i16.to_le_bytes());
    d.extend_from_slice(&half(1.0).to_le_bytes());
    d.extend_from_slice(&half(2.0).to_le_bytes());
    d.extend_from_slice(&half(3.0).to_le_bytes());
    let run = d.len();
    d.extend_from_slice(&[2, 2]);
    d.extend_from_slice(&0i16.to_le_bytes());
    d.extend_from_slice(&16384i16.to_le_bytes());
    let off = (run - ptr) as i16;
    d[ptr + 4..ptr + 6].copy_from_slice(&off.to_le_bytes());
    d
}

#[test]
fn sequences_become_clips() {
    let dir = temp("anim");
    let mut s = Synth::quad();
    s.bones.push(("arm", 0, Vec3::new(5.0, 0.0, 0.0), Quat::IDENTITY));
    s.anims = vec![("turn", 2, turning_anim()), ("turn", 2, turning_anim())];
    let path = s.write(&dir, "anim");
    let sc = load_file(&path, &Limits::viewer()).unwrap();
    assert_eq!(sc.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["turn", "turn 2"]);
    let clip = &sc.animations[0];
    assert!((clip.duration - 0.1).abs() < 1e-6);
    let arm = sc.nodes.iter().position(|n| n.name == "arm").unwrap();
    let rot = clip.channels.iter().find(|c| c.node == arm && c.property == Property::Rotation).unwrap();
    let q1 = Quat::from_slice(&rot.values[4..8]);
    // 16384 x pi/32768 = a quarter turn about Z.
    assert!(q1.dot(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)).abs() > 0.9999, "{q1:?}");
    let pos = clip.channels.iter().find(|c| c.node == arm && c.property == Property::Translation).unwrap();
    assert_eq!(pos.values[..3], [1.0, 2.0, 3.0]);
    // The root bone has no record: it keeps its rest pose.
    let root = sc.nodes.iter().position(|n| n.name == "root" && n.mesh.is_none() && n.transform.scale.x == 1.0);
    assert!(root.is_some());
    assert!(load_file(&path, &Limits::thumbnail()).unwrap().animations.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn later_versions_are_refused_with_a_message() {
    let dir = temp("titanfall");
    let mut s = Synth::quad();
    s.version = 53;
    let path = s.write(&dir, "tf");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(crate::sniff(&path, &bytes), Some(QuakeFormat::SourceMdl));
    match crate::load(QuakeFormat::SourceMdl, &path, &bytes, &Limits::viewer()) {
        Err(LoadError::Unsupported(m)) => assert!(m.contains("Titanfall"), "{m}"),
        other => panic!("{:?}", other.err()),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_companions_are_explained() {
    let dir = temp("alone");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("alone.mdl");
    std::fs::write(&path, Synth::quad().mdl()).unwrap();
    let e = load_file(&path, &Limits::viewer()).err().unwrap().to_string();
    assert!(e.contains(".vvd"), "{e}");
    let e = load_file(&path, &Limits { neighbours: false, ..Limits::viewer() }).err().unwrap().to_string();
    assert!(e.contains("weren't read"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Truncated and corrupted copies of each of the three files never panic,
/// hang or allocate without bound.
#[test]
fn damaged_files_fail_cleanly() {
    let dir = temp("damaged");
    let mut s = Synth::quad();
    s.flexdescs = vec!["a"];
    s.parts[0].1[0].meshes[0].flexes = vec![(0, 0, vec![(1, Vec3::ONE, 0)])];
    s.bones.push(("arm", 0, Vec3::X, Quat::IDENTITY));
    s.anims = vec![("turn", 2, turning_anim())];
    let path = s.write(&dir, "m");
    let (mdl, vvd, vtx) = (s.mdl(), s.vvd(), s.vtx());
    let started = std::time::Instant::now();
    for (which, good) in [("mdl", &mdl), ("vvd", &vvd), ("dx90.vtx", &vtx)] {
        for bad in testutil::mutations(good) {
            std::fs::write(dir.join(format!("m.{which}")), &bad).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let _ = load(&path, &bytes, &Limits::viewer(), 0);
            let _ = parts(&bytes);
        }
        std::fs::write(dir.join(format!("m.{which}")), good).unwrap();
    }
    // Absurd counts in the header.
    for field in [156usize, 180, 188, 204, 212, 220, 224, 232, 260, 352] {
        let mut bad = mdl.clone();
        bad[field..field + 4].copy_from_slice(&i32::MAX.to_le_bytes());
        let _ = load(&path, &bad, &Limits::viewer(), 0);
    }
    assert!(started.elapsed().as_secs() < 60, "damaged files took {:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(dir);
}

//endregion

/*┌─────────────────╮
| Real models     |
└─────────────────╯*/
//region Real models

/// The share of triangle area whose winding agrees with the stored vertex
/// normals: near 1 when triangles wind counter-clockwise seen from the
/// side their normals point to.
fn facing_normals(s: &Scene) -> f32 {
    let (mut agree, mut total) = (0.0f32, 0.0f32);
    for prim in s.meshes.iter().flat_map(|m| &m.primitives) {
        for t in prim.indices.chunks_exact(3) {
            let p = [prim.positions[t[0] as usize], prim.positions[t[1] as usize], prim.positions[t[2] as usize]];
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            let stored = t.iter().map(|&i| prim.normals[i as usize]).sum::<Vec3>();
            let a = n.length();
            total += a;
            if n.dot(stored) > 0.0 {
                agree += a;
            }
        }
    }
    if total > 0.0 { agree / total } else { 1.0 }
}

fn real(rel: &str, limits: &Limits) -> Option<Scene> {
    let p = asset(rel)?;
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(crate::sniff(&p, &bytes[..bytes.len().min(256)]), Some(QuakeFormat::SourceMdl), "{rel}");
    Some(load(&p, &bytes, limits, 0).unwrap_or_else(|e| panic!("{rel}: {e}")))
}

/// Wiremod's models (Apache-2.0), versions 44 and 48, with their materials
/// and textures in the repository's `materials/` folder.
#[test]
fn wiremod_models() {
    let Some(root) = asset("source-extra/wiremod/models") else { return };
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "mdl") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut textured = 0;
    let mut facing_well = 0;
    for p in &files {
        let bytes = std::fs::read(p).unwrap();
        let s = load(p, &bytes, &Limits::viewer(), 0).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(s.triangle_count() > 0, "{}", p.display());
        let facing = facing_normals(&s);
        if facing < 0.9 {
            eprintln!("{}: only {facing:.3} of the triangle area faces its normals", p.display());
        } else {
            facing_well += 1;
        }
        if s.materials.iter().all(|m| m.base_color_texture.is_some()) {
            textured += 1;
        } else {
            eprintln!("{}: {:?}", p.display(), s.info.warnings);
        }
    }
    eprintln!("{} models, {textured} fully textured", files.len());
    assert!(files.len() > 300 && textured * 10 > files.len() * 9, "{textured} of {}", files.len());
    // A couple of community models have normals pointing inward.
    assert!(facing_well + 3 >= files.len(), "{facing_well} of {} wind as their normals say", files.len());

    let s = real("source-extra/wiremod/models/bull/gates/logic.mdl", &Limits::viewer()).unwrap();
    let bytes = std::fs::read(asset("source-extra/wiremod/models/bull/gates/logic.mdl").unwrap()).unwrap();
    assert_eq!(parts(&bytes).len(), 14);
    assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
    let g = real("source-extra/wiremod/models/bull/various/gyroscope.mdl", &Limits::viewer()).unwrap();
    assert_eq!(g.animations.len(), 5, "{:?}", g.animations.iter().map(|a| &a.name).collect::<Vec<_>>());
}

/// Valve's models from SourcePP's test files (local only), one each of
/// versions 44, 47 and 49.
#[test]
fn sourcepp_models() {
    for v in [44, 47, 49] {
        let Some(s) = real(&format!("source-extra/sourcepp-mdl/v{v}.mdl"), &Limits::viewer()) else { continue };
        eprintln!(
            "v{v}: {} tris, {} bones, {} clips, {} meshes, warnings {:?}",
            s.triangle_count(),
            s.skins[0].joints.len(),
            s.animations.len(),
            s.meshes.len(),
            s.info.warnings
        );
        assert!(s.triangle_count() > 0);
        assert!(facing_normals(&s) > 0.9, "v{v} winds against its normals");
    }
}

//endregion

/// Draws a few models with the test rasteriser into `/tmp/studio-*.png`,
/// for looking at by eye.  Run with `-- --ignored render_samples`.
#[test]
#[ignore]
fn render_samples() {
    for rel in [
        "source-extra/wiremod/models/bull/gates/logic.mdl",
        "source-extra/wiremod/models/beer/wiremod/gate_e2.mdl",
        "source-extra/wiremod/models/bull/various/gyroscope.mdl",
        "source-extra/wiremod/models/cheeze/wires/router.mdl",
        "source-extra/wiremod/models/blacknecro/tv_plasma_4_3.mdl",
        "source-extra/sourcepp-mdl/v47.mdl",
        "source-extra/sourcepp-mdl/v49.mdl",
    ] {
        let Some(s) = real(rel, &Limits::viewer()) else { continue };
        let name = Path::new(rel).file_stem().unwrap().to_string_lossy().into_owned();
        let px = testutil::render(&s, Vec3::new(-1.0, -0.8, -1.3), 512, true);
        testutil::write_png(Path::new(&format!("/tmp/studio-{name}.png")), 512, 512, &px);
    }
}
