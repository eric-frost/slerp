//! Tests on the sample files (checked against OpenSKP's own reading of
//! them) and on damaged copies.

use super::*;
use slerp_scene::{Pose, Topology};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where the samples live: `local/assets/editors-extra/skp` (fetched by
/// `tools/fetch-test-assets.ts`) and `local/assets/sketchup-extra`.
fn sample(name: &str) -> Option<Vec<u8>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let found = ["editors-extra/skp", "sketchup-extra"].iter().map(|d| root.join(d).join(name)).find(|p| p.exists());
    match found {
        Some(p) => std::fs::read(p).ok(),
        None => {
            eprintln!("skipping: {name} is missing");
            None
        }
    }
}

type Bounds = Option<([f32; 3], [f32; 3])>;

/// OpenSKP 1.3.0's reading of each sample (`_core.full_parse` and
/// `build_scene`): definitions, faces, edges, placements and materials,
/// and the box around every triangle in metres with Y up.
#[rustfmt::skip]
const OPENSKP: &[(&str, usize, usize, usize, usize, usize, Bounds)] = &[
    ("devtools-blank-su8.skp", 0, 0, 0, 0, 0, None),
    ("devtools-materials-cube-su7.skp", 0, 12, 24, 0, 11, Some(([0.0000, -0.0000, -1.3037], [2.5119, 1.0000, -0.0000]))),
    ("devtools-sectionPlane-su7.skp", 0, 6, 12, 0, 0, Some(([0.0000, 0.0000, -1.0000], [1.3400, 1.0000, -0.0000]))),
    ("grevit-Example.skp", 4, 29, 83, 32, 24, Some(([-1.0000, -0.0000, -11.0000], [22.6694, 12.0000, 11.0000]))),
    ("lod3-4018213-su2017.skp", 1, 15, 32, 1, 3, Some(([7.8864, 0.4199, -63.9426], [50.3531, 23.1689, -8.6340]))),
    ("openskp-coedge-2026.skp", 133, 43623, 86836, 170, 43, Some(([-0.9028, -0.0885, -1.9967], [2.6361, 1.2578, -0.5543]))),
    ("openskp-style_watermark-2025.skp", 0, 32, 84, 0, 1, Some(([-0.6072, -0.0000, -0.0000], [1.0000, 1.0000, 2.9512]))),
    ("openskp-SU_File-2025.skp", 0, 32, 84, 0, 1, Some(([-0.6072, -0.0000, -0.0000], [1.0000, 1.0000, 2.9512]))),
    ("testup-2020.skp", 1, 63, 995, 1, 8, Some(([0.1490, -0.0007, -0.4819], [0.6571, 1.7017, -0.1390]))),
    ("testup-2021.skp", 1, 628, 6193, 1, 8, Some(([-0.5809, -0.0007, 0.4376], [-0.0885, 1.6490, 0.7699]))),
    ("testup-Cube.skp", 1, 6, 12, 1, 0, Some(([0.0000, 0.0000, -1.2010], [0.9460, 1.0170, -0.0000]))),
    ("testup-jinyi.skp", 4, 24, 854, 4, 13, Some(([-0.0000, 0.0000, -0.4022], [2.2936, 3.0528, -0.0000]))),
    ("testup-skp-v6.skp", 1, 6, 12, 1, 0, Some(([0.0000, 0.0000, -1.2000], [1.0010, 0.5510, -0.0000]))),
    ("testup-skp-vff-soft-block.skp", 1, 69, 1007, 1, 9, Some(([-0.1123, -0.0007, -5.0580], [2.8992, 1.7017, -0.1408]))),
    ("testup-ui-textured.skp", 5, 5, 20, 5, 1, Some(([0.0000, 0.0000, -1.0000], [7.0000, 0.0000, -0.0000]))),
    ("openskp-arc.skp", 0, 0, 8, 0, 0, None),
    ("openskp-blank_v17.skp", 0, 0, 0, 0, 0, None),
    ("openskp-capilla_quiroz_v17.skp", 2, 181, 515, 3, 16, Some(([-8.2987, -0.5948, -5.6442], [19.9249, 3.7078, -0.0000]))),
    ("openskp-chair_and_table.skp", 9, 86, 200, 15, 4, Some(([-0.6858, 0.0000, -0.2540], [0.5588, 0.8636, 0.2540]))),
    ("openskp-circle.skp", 0, 1, 12, 0, 0, Some(([0.2540, 0.0000, -2.2860], [2.2860, 0.0000, -0.2540]))),
    ("openskp-gondola_v20.skp", 20, 1887, 9186, 53, 24, Some(([46.1689, -0.0000, -8.1409], [49.9885, 3.1402, -3.3238]))),
    ("openskp-holes.skp", 0, 1, 8, 0, 0, Some(([0.0000, 0.0000, -2.5400], [5.0800, 0.0000, -0.0000]))),
    ("openskp-legacy_annotations.skp", 0, 1, 4, 0, 0, Some(([0.0000, 0.0000, -2.5400], [2.5400, 0.0000, -0.0000]))),
    ("openskp-legacy_v2014_synthetic.skp", 3, 3, 12, 11, 34, Some(([0.0000, 0.0000, -1.0160], [4.6990, 0.0000, -0.0000]))),
    ("openskp-legacy_v3_synthetic.skp", 4, 4, 16, 11, 35, Some(([0.0000, 0.0000, -0.7620], [9.6520, 0.0000, -0.0000]))),
    ("openskp-legacy_v6_synthetic.skp", 4, 4, 16, 11, 24, Some(([0.0000, 0.0000, -0.7620], [9.6520, 0.0000, -0.0000]))),
    ("openskp-legacy_v7_synthetic.skp", 3, 3, 12, 11, 34, Some(([0.0000, 0.0000, -1.0160], [4.6990, 0.0000, -0.0000]))),
    ("openskp-materials_and_layers.skp", 0, 2, 8, 0, 2, Some(([0.0000, 0.0000, -2.5400], [2.5400, 0.2540, -0.0000]))),
    ("openskp-multi_face_shared_edge.skp", 0, 2, 7, 0, 0, Some(([0.0000, 0.0000, -2.5400], [5.0800, 0.0000, -0.0000]))),
    ("openskp-single_material_v17.skp", 0, 0, 0, 0, 1, None),
    ("openskp-Untitled.skp", 46, 1588, 12762, 46, 15, Some(([0.1188, -0.0000, -5.1881], [13.5357, 2.8200, 2.0591]))),
    ("openskp-zero_material_custom_layer_v18.skp", 0, 0, 0, 0, 0, None),
];

/// The box around every triangle of `scene`, shown or hidden.
fn triangle_bounds(scene: &Scene) -> slerp_scene::Aabb {
    let pose = Pose::rest(scene);
    let mut out = slerp_scene::Aabb::EMPTY;
    for (i, n) in scene.nodes.iter().enumerate() {
        let Some(m) = n.mesh else { continue };
        for p in scene.meshes[m].primitives.iter().filter(|p| p.topology == Topology::Triangles) {
            p.positions.iter().for_each(|&v| out.add(pose.world[i].transform_point3(v)));
        }
    }
    out
}

#[test]
fn matches_openskp() {
    for &(name, defs, faces, edges, placements, materials, bounds) in OPENSKP {
        let Some(data) = sample(name) else { continue };
        let model = read(&data, &Limits::viewer()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(model.definitions.len(), defs, "{name}: definitions");
        assert_eq!(model.counts(), (faces, edges, placements), "{name}: faces, edges, placements");
        assert_eq!(model.materials.len(), materials, "{name}: materials");
        let scene = build::scene(&model, &Limits::viewer());
        let b = triangle_bounds(&scene);
        match bounds {
            None => assert!(b.is_empty(), "{name}: expected no triangles"),
            Some((lo, hi)) => {
                let close = |a: glam::Vec3, e: [f32; 3]| (a - glam::Vec3::from(e)).abs().max_element() < 0.002;
                assert!(close(b.min, lo) && close(b.max, hi), "{name}: bounds {b:?}, OpenSKP {lo:?} {hi:?}");
            }
        }
        let misread = scene.info.warnings.iter().any(|w| w.contains("not have been read correctly"));
        assert!(!misread, "{name}: {:?}", scene.info.warnings);
    }
}

#[test]
fn reads_materials_tags_and_views() {
    // A 2021 file: colours from material.xml, the saved camera.
    if let Some(data) = sample("testup-2021.skp") {
        let model = read(&data, &Limits::viewer()).unwrap();
        let hair = model.materials.iter().find(|m| m.name == "Sumele_Hair").expect("material");
        assert_eq!((hair.color, hair.opacity), ([19, 13, 7], 1.0));
        assert_eq!((model.style.front, model.style.back), ([255, 255, 255], [164, 178, 187]));
        let cam = model.camera.expect("saved view");
        assert!((cam.eye - glam::DVec3::new(300.57, -305.546, 137.081)).length() < 0.01 && cam.perspective);
        let scene = load(&data, &Limits::viewer()).unwrap();
        let view = saved_view(&scene, &triangle_bounds(&scene)).expect("orbit camera");
        let eye = view.eye();
        // The same eye, in metres with Y up.
        let want = glam::Vec3::new(300.57, 137.081, 305.546) * build::INCH as f32;
        assert!((eye - want).length() < 0.01, "{eye} {want}");
        assert!((view.fov_y.to_degrees() - 35.0).abs() < 0.01);
    }
    // A classic file with textures (pictures stored as CDib objects).
    if let Some(data) = sample("devtools-materials-cube-su7.skp") {
        let model = read(&data, &Limits::viewer()).unwrap();
        let textured: Vec<_> = model.materials.iter().filter_map(|m| m.texture.as_ref()).collect();
        assert!(!textured.is_empty());
        assert!(textured.iter().all(|t| t.width > 0.0 && t.height > 0.0 && !t.bytes.is_empty()));
        let scene = load(&data, &Limits::viewer()).unwrap();
        assert!(!scene.textures.is_empty(), "{:?}", scene.info.warnings);
    }
    // Tags in a 2025 file, one of them switched off.
    if let Some(data) = sample("openskp-Untitled.skp") {
        let model = read(&data, &Limits::viewer()).unwrap();
        assert_eq!(model.layers.len(), 14);
    }
}

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

/// A small deterministic random source.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

/// Damaged copies of `data`: cut short at many places, with bytes changed,
/// and with counts and lengths made absurd.
fn damaged(data: &[u8], seed: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rng = Lcg(seed);
    for len in (0..data.len().min(800)).step_by(13).chain((1..48).map(|k| data.len() * k / 48)) {
        out.push(data[..len].to_vec());
    }
    for _ in 0..200 {
        let mut d = data.to_vec();
        for _ in 0..1 + rng.below(8) {
            let i = rng.below(d.len());
            d[i] = rng.next() as u8;
        }
        out.push(d);
    }
    for k in 0..120 {
        let mut d = data.to_vec();
        let i = rng.below(d.len().saturating_sub(4));
        let v: u32 = [0xFFFF_FFFF, 0x7FFF_FFFF, 0x0010_0000, 0xFFFF_7FFF][k % 4];
        if i + 4 <= d.len() {
            d[i..i + 4].copy_from_slice(&v.to_le_bytes());
        }
        out.push(d);
    }
    out
}

/// Runs `f` on every damaged copy, each within a time limit.
fn survives(name: &str, copies: Vec<Vec<u8>>, f: impl Fn(&[u8])) {
    for (k, d) in copies.iter().enumerate() {
        let t = Instant::now();
        f(d);
        assert!(t.elapsed() < Duration::from_secs(3), "{name} copy {k} took {:?}", t.elapsed());
    }
}

#[test]
fn damaged_classic_files_never_panic_or_hang() {
    let limits = Limits::thumbnail();
    for name in [
        "testup-2020.skp",
        "grevit-Example.skp",
        "testup-skp-v6.skp",
        "lod3-4018213-su2017.skp",
        "openskp-chair_and_table.skp",
    ] {
        let Some(data) = sample(name) else { continue };
        survives(name, damaged(&data, name.len() as u64), |d| {
            // No catch_unwind here: a panic fails the test.
            if let Ok(model) = legacy::read(d, &Caps::from(&limits)) {
                build::scene(&model, &limits);
            }
        });
    }
}

#[test]
fn damaged_entity_records_never_panic_or_hang() {
    let limits = Limits::thumbnail();
    for name in ["testup-2021.skp", "openskp-SU_File-2025.skp", "openskp-Untitled.skp"] {
        let Some(data) = sample(name) else { continue };
        let dat = vff::model_dat(&data).expect("model.dat");
        survives(name, damaged(&dat, 7), |d| {
            let mut model = Model::default();
            vff::entities(d, &mut model, &Default::default());
            build::scene(&model, &limits);
        });
        // The whole file damaged too: the zip and the XML.
        survives(name, damaged(&data, 11).into_iter().step_by(4).collect(), |d| {
            if let Ok(model) = vff::read(d, &Caps::from(&limits)) {
                build::scene(&model, &limits);
            }
        });
    }
}

/// A definition with one square face.
fn square() -> Definition {
    let vertices = vec![
        glam::DVec3::new(0.0, 0.0, 0.0),
        glam::DVec3::new(10.0, 0.0, 0.0),
        glam::DVec3::new(10.0, 10.0, 0.0),
        glam::DVec3::new(0.0, 10.0, 0.0),
    ];
    let edges = (0..4u32)
        .map(|i| Edge { a: i, b: (i + 1) % 4, hidden: false, soft: false, smooth: false, layer: None })
        .collect();
    let face = Face {
        loops: vec![(0..4).map(|edge| CoEdge { edge, reversed: false }).collect()],
        normal: glam::DVec3::Z,
        front: None,
        back: None,
        mapping: [None, None],
        hidden: false,
        layer: None,
    };
    Definition { name: "square".into(), vertices, edges, faces: vec![face], ..Definition::default() }
}

fn place(definition: u32, x: f64) -> Instance {
    let mut matrix = vff::IDENTITY;
    matrix[9] = x;
    Instance { definition, matrix, material: None, layer: None, hidden: false, name: String::new() }
}

#[test]
fn components_that_contain_themselves_are_left_out() {
    // A contains B, B contains A.
    let mut a = square();
    a.instances.push(place(1, 20.0));
    let mut b = square();
    b.instances.push(place(0, 20.0));
    let model = Model {
        root: Definition { instances: vec![place(0, 0.0)], ..Definition::default() },
        definitions: vec![a, b],
        ..Model::default()
    };
    let scene = build::scene(&model, &Limits::viewer());
    assert!(scene.info.warnings.iter().any(|w| w.contains("contain themselves")), "{:?}", scene.info.warnings);
    // The meshes are shared: two definitions, two meshes, however placed.
    assert_eq!(scene.meshes.len(), 2);
}

#[test]
fn exponential_nesting_stops_at_the_caps() {
    // Twelve levels of ten copies each: 10^12 placements.
    let mut definitions = Vec::new();
    for level in 0..12u32 {
        let mut d = square();
        if level < 11 {
            d.instances = (0..10).map(|k| place(level + 1, k as f64 * 12.0)).collect();
        }
        definitions.push(d);
    }
    let model = Model {
        root: Definition { instances: vec![place(0, 0.0)], ..Definition::default() },
        definitions,
        ..Model::default()
    };
    let t = Instant::now();
    let scene = build::scene(&model, &Limits::thumbnail());
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert!(scene.nodes.len() <= 1_000_001);
    assert!(scene.info.warnings.iter().any(|w| w.contains("too large")), "{:?}", scene.info.warnings);
}

#[test]
fn faces_keep_two_sides_and_edges_show() {
    let model = Model { root: square(), ..Model::default() };
    let scene = build::scene(&model, &Limits::viewer());
    let mesh = &scene.meshes[0];
    let tris: Vec<_> = mesh.primitives.iter().filter(|p| p.topology == Topology::Triangles).collect();
    // Unpainted: the style's white front and blue-grey back.
    assert_eq!(tris.len(), 2);
    let names: Vec<_> = tris.iter().map(|p| scene.materials[p.material.unwrap()].name.as_str()).collect();
    assert_eq!(names, ["Default front", "Default back"]);
    // Four edges, drawn on both sides of their single face.
    let lines = mesh.primitives.iter().find(|p| p.topology == Topology::Lines).expect("edges");
    assert_eq!(lines.indices.len(), 4 * 2 * 2);
}

//endregion
