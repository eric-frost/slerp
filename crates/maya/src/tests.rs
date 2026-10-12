use super::*;
use slerp_scene::{Limits, Scene};
use std::path::{Path, PathBuf};

/// A cube saved without history, as Maya writes one: eight points, twelve
/// hard edges, six faces with UVs, a blinn on faces 0-2 and lambert1 on the
/// rest, under a transform moved, turned and scaled.
const CUBE: &str = r#"//Maya ASCII 2022 scene
//Name: cube.ma
requires maya "2022";
currentUnit -l centimeter -a degree -t film;
fileInfo "product" "Maya 2022";
createNode transform -n "pCube1";
	setAttr ".t" -type "double3" 100 0 0 ;
	setAttr ".r" -type "double3" 0 90 0 ;
	setAttr ".s" -type "double3" 2 2 2 ;
createNode mesh -n "pCubeShape1" -p "pCube1";
	setAttr -k off ".v";
	setAttr -s 2 ".iog[0].og";
	setAttr ".iog[0].og[0].gcl" -type "componentList" 1 "f[0:2]";
	setAttr ".iog[0].og[1].gcl" -type "componentList" 1 "f[3:5]";
	setAttr ".uvst[0].uvsn" -type "string" "map1";
	setAttr -s 14 ".uvst[0].uvsp[0:13]" -type "float2" 0.375 0 0.625 0 0.375 0.25
		 0.625 0.25 0.375 0.5 0.625 0.5 0.375 0.75 0.625 0.75 0.375 1 0.625 1 0.875 0 0.875
		 0.25 0.125 0 0.125 0.25;
	setAttr ".cuvs" -type "string" "map1";
	setAttr -s 8 ".vt[0:7]"  -0.5 -0.5 0.5 0.5 -0.5 0.5 -0.5 0.5 0.5 0.5 0.5 0.5
		 -0.5 0.5 -0.5 0.5 0.5 -0.5 -0.5 -0.5 -0.5 0.5 -0.5 -0.5;
	setAttr -s 12 ".ed[0:11]"  0 1 0 2 3 0 4 5 0 6 7 0 0 2 0 1 3 0 2 4 0 3 5 0 4 6 0
		 5 7 0 6 0 0 7 1 0;
	setAttr -s 6 -ch 24 ".fc[0:5]" -type "polyFaces"
		f 4 0 5 -2 -5
		mu 0 4 0 1 3 2
		f 4 1 7 -3 -7
		mu 0 4 2 3 5 4
		f 4 2 9 -4 -9
		mu 0 4 4 5 7 6
		f 4 3 11 -1 -11
		mu 0 4 6 7 9 8
		f 4 -12 -10 -8 -6
		mu 0 4 1 10 11 3
		f 4 10 4 6 8
		mu 0 4 12 0 2 13 ;
createNode blinn -n "blinn1";
	setAttr ".c" -type "float3" 1 0 0 ;
createNode shadingEngine -n "blinn1SG";
connectAttr "blinn1.oc" "blinn1SG.ss";
connectAttr "pCubeShape1.iog.og[0]" "blinn1SG.dsm" -na;
connectAttr "pCubeShape1.iog.og[1]" ":initialShadingGroup.dsm" -na;
"#;

fn load_text(text: &str) -> Result<Scene, slerp_scene::LoadError> {
    load(text.as_bytes(), None, &Limits::viewer())
}

#[test]
fn reads_a_cube_with_two_materials() {
    let scene = load_text(CUBE).unwrap();
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(scene.triangle_count(), 12);
    let mesh = &scene.meshes[0];
    assert_eq!(mesh.primitives.len(), 2);
    let red = mesh.primitives.iter().find(|p| scene.materials[p.material.unwrap()].name == "blinn1").unwrap();
    assert_eq!(red.triangle_count(), 6);
    assert_eq!(scene.materials[red.material.unwrap()].base_color.x, 0.8);
    // Hard edges: every corner keeps its face's normal, 24 vertices.
    assert_eq!(mesh.primitives.iter().map(|p| p.positions.len()).sum::<usize>(), 24);
    // Centimetres to metres on the root; the transform below it.
    let root = &scene.nodes[scene.roots[0]];
    assert!((root.transform.scale.x - 0.01).abs() < 1e-6);
    let cube = &scene.nodes[root.children[0]];
    assert_eq!(cube.transform.translation.x, 100.0);
    assert!((cube.transform.scale - glam::Vec3::splat(2.0)).length() < 1e-5);
    let (axis, angle) = cube.transform.rotation.to_axis_angle();
    assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 1e-4 && axis.y > 0.99);
    assert!(scene.info.format.contains("Maya ASCII"));
}

#[test]
fn rebuilds_primitives_from_history() {
    let text = r#"//Maya ASCII 2020 scene
requires maya "2020";
currentUnit -l meter -a degree -t film;
createNode transform -n "pSphere1";
createNode mesh -n "pSphereShape1" -p "pSphere1";
	setAttr ".uvst[0].uvsn" -type "string" "map1";
createNode polySphere -n "polySphere1";
	setAttr ".r" 2;
	setAttr ".sa" 8;
	setAttr ".sh" 6;
createNode transform -n "pBlob1";
createNode mesh -n "pBlobShape1" -p "pBlob1";
createNode polyExtrudeFace -n "polyExtrudeFace1";
createNode polyCube -n "polyCube1";
connectAttr "polySphere1.out" "pSphereShape1.i";
connectAttr "polyCube1.out" "polyExtrudeFace1.ip";
connectAttr "polyExtrudeFace1.out" "pBlobShape1.i";
"#;
    let scene = load_text(text).unwrap();
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(scene.triangle_count(), 8 * 2 + 8 * 4 * 2);
    let details = &scene.info.details;
    assert!(details.iter().any(|(k, v)| k == "Rebuilt primitives" && v.contains("polySphere")));
    assert!(details.iter().any(|(k, v)| k.starts_with("Left out") && v.contains("polyExtrudeFace")));
    assert_eq!(scene.nodes[scene.roots[0]].transform.scale.x, 1.0);
}

#[test]
fn up_axis_and_hidden_layers() {
    let text = r#"//Maya ASCII 2020 scene
createNode transform -n "a";
createNode mesh -n "aShape" -p "a";
createNode polyCube -n "c";
connectAttr "c.out" "aShape.i";
createNode displayLayer -n "hidden";
	setAttr ".v" no;
connectAttr "hidden.di" "a.do";
createNode script -n "upAxisScriptNode";
	setAttr ".b" -type "string" "string $currentAxis = `upAxis -q -ax`; if ($currentAxis != \"z\") { upAxis -ax \"z\"; viewSet -home persp; }";
"#;
    let scene = load_text(text).unwrap();
    assert!(scene.info.details.iter().any(|(k, v)| k == "Units" && v.contains("Z up")));
    let root = &scene.nodes[scene.roots[0]];
    assert!(root.transform.rotation.to_axis_angle().1 > 1.5);
    assert!(!scene.nodes[root.children[0]].visible);
}

#[test]
fn history_only_files_say_why() {
    let text = "//Maya ASCII 2020 scene\ncreateNode transform -n \"a\";\ncreateNode mesh -n \"aShape\" -p \"a\";\ncreateNode polyBevel -n \"b\";\nconnectAttr \"b.out\" \"aShape.i\";\n";
    let err = load_text(text).err().expect("no scene").to_string();
    assert!(err.contains("polyBevel"), "{err}");
}

/// A triangle bound to one joint, which an animation curve turns 90
/// degrees about Z: the saved pose shows the turn.
#[test]
fn skinned_meshes_show_their_saved_pose() {
    let text = r#"//Maya ASCII 2020 scene
currentUnit -l centimeter -a degree -t film;
createNode joint -n "j";
createNode transform -n "t";
createNode mesh -n "s" -p "t";
	setAttr ".io" yes;
createNode mesh -n "sOrig" -p "t";
	setAttr ".io" yes;
	setAttr -s 3 ".vt[0:2]" 1 0 0 2 0 0 1 1 0;
	setAttr -s 3 ".ed[0:2]" 0 1 0 1 2 0 2 0 0;
	setAttr ".fc[0]" -type "polyFaces" f 3 0 1 2;
createNode mesh -n "posed" -p "t";
createNode skinCluster -n "skin";
	setAttr ".wl[0].w[0]" 1;
	setAttr ".wl[1].w[0]" 1;
	setAttr ".wl[2].w[0]" 1;
	setAttr ".pm[0]" -type "matrix" 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1;
createNode animCurveTA -n "j_rotateZ";
	setAttr -s 2 ".ktv[0:1]" 1 90 10 0;
select -ne :time1;
	setAttr ".o" 1;
connectAttr "j_rotateZ.o" "j.rz";
connectAttr "j.wm" "skin.ma[0]";
connectAttr "sOrig.w" "skin.ip[0].ig";
connectAttr "skin.og[0]" "posed.i";
"#;
    let scene = load_text(text).unwrap();
    assert!(scene.info.details.iter().any(|(k, v)| k == "Posed by their skin" && v == "posed"));
    let p = &scene.meshes[0].primitives[0].positions;
    // (1, 0, 0) turned a quarter about Z is (0, 1, 0).
    assert!(p.iter().any(|q| (*q - glam::Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4), "{p:?}");
}

#[test]
fn faces_with_holes_are_triangulated_around_them() {
    // A 3x3 square with a 1x1 hole: 8 edges outside and inside.
    let text = r#"//Maya ASCII 2020 scene
createNode transform -n "t";
createNode mesh -n "s" -p "t";
	setAttr -s 8 ".vt[0:7]" 0 0 0 3 0 0 3 3 0 0 3 0 1 1 0 1 2 0 2 2 0 2 1 0;
	setAttr -s 8 ".ed[0:7]" 0 1 0 1 2 0 2 3 0 3 0 0 4 5 0 5 6 0 6 7 0 7 4 0;
	setAttr ".fc[0]" -type "polyFaces" f 4 0 1 2 3 h 4 4 5 6 7;
"#;
    let scene = load_text(text).unwrap();
    assert_eq!(scene.triangle_count(), 8);
}

#[test]
fn damaged_files_never_panic() {
    let full = CUBE.as_bytes();
    for cut in (0..full.len()).step_by(37) {
        let _ = load(&full[..cut], None, &Limits::viewer());
    }
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..200 {
        let mut bytes = full.to_vec();
        for _ in 0..8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = 20 + (state as usize) % (bytes.len() - 20);
            bytes[i] = b"0123456789-; \"[]:.fhmu"[(state >> 32) as usize % 22];
        }
        let _ = load(&bytes, None, &Limits::viewer());
    }
}

#[test]
fn absurd_ranges_and_counts_stay_bounded() {
    let text = r#"//Maya ASCII 2020 scene
createNode transform -n "t";
createNode mesh -n "s" -p "t";
	setAttr ".vt[4000000000:4000000002]" 0 0 0 1 0 0 0 1 0;
	setAttr ".ed[0:2]" 0 1 0 1 2 0 2 0 0;
	setAttr ".fc[0]" -type "polyFaces" f 3 0 1 2 mu 0 999999999 1 2 3;
	setAttr ".fc[1]" -type "polyFaces" f 3 0 1 99999;
	setAttr ".vt[49999990]" 1 2 3;
	setAttr ".ed[49999990]" 1 2 0;
	setAttr ".fc[49999999]" -type "polyFaces" f 3 0 1 2;
	setAttr ".uvst[0].uvsp[49999999]" 0 0;
"#;
    let start = std::time::Instant::now();
    let _ = load_text(text);
    // A lone element far out doesn't make the reader fill the gap.
    assert!(start.elapsed().as_secs_f64() < 1.0);
    // A deep hierarchy and a parenting loop.
    let mut deep = String::from("//Maya ASCII 2020 scene\ncreateNode transform -n \"n0\";\n");
    for i in 1..3000 {
        deep.push_str(&format!("createNode transform -n \"n{i}\" -p \"n{}\";\n", i - 1));
    }
    deep.push_str("createNode mesh -n \"m\" -p \"n2999\";\nparent -add \"n0\" \"n5\";\n");
    let _ = load_text(&deep);
}

/// Binary files: a hand-made FOR4 file with one transform, one mesh blob
/// (a triangle) and nothing else.
#[test]
fn reads_a_minimal_binary_file() {
    fn chunk(tag: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = tag.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }
    fn group(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut data = kind.to_vec();
        data.extend_from_slice(body);
        let mut out = b"FOR4".to_vec();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&data);
        out
    }
    let mut blob = b"o\0 ".to_vec();
    let be = |v: u32| v.to_be_bytes();
    blob.extend_from_slice(&be(9));
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        blob.extend_from_slice(&f.to_be_bytes());
    }
    blob.extend_from_slice(&be(6));
    for v in [0x8000_0000u32, 1, 0x8000_0001, 2, 0x8000_0002, 0] {
        blob.extend_from_slice(&be(v));
    }
    blob.extend_from_slice(&be(3));
    for v in [0u32, 1, 0x6000_0002] {
        blob.extend_from_slice(&be(v));
    }
    blob.extend_from_slice(&be(0));
    blob.extend_from_slice(&be(0));
    let mut body = Vec::new();
    body.extend(group(b"HEAD", &chunk(b"VERS", b"2019\0")));
    body.extend(group(b"XFRM", &chunk(b"CREA", b"\x0ctri\0")));
    let mut mesh = chunk(b"CREA", b"\x04triShape\0tri\0");
    mesh.extend(chunk(b"MESH", &blob));
    body.extend(group(b"DMSH", &mesh));
    let file = group(b"Maya", &body);
    let scene = load(&file, None, &Limits::viewer()).unwrap();
    assert_eq!(scene.triangle_count(), 1);
    assert!(scene.info.format.contains("binary"));
    for cut in 0..file.len() {
        let _ = load(&file[..cut], None, &Limits::viewer());
    }
}

/// Every Maya sample in the local assets loads or fails cleanly, and those
/// known to store geometry build some.
#[test]
fn sample_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["editors-extra/maya", "autodesk-extra/maya"] {
        collect(&root.join(dir), &mut files);
    }
    if files.is_empty() {
        return;
    }
    let mut built = 0;
    for f in &files {
        let start = std::time::Instant::now();
        let result = load_path(f, &Limits::viewer());
        let ms = start.elapsed().as_millis();
        match &result {
            Ok(s) => {
                built += 1;
                eprintln!(
                    "{}: {} meshes, {} triangles, {} materials, {ms} ms",
                    f.display(),
                    s.meshes.len(),
                    s.triangle_count(),
                    s.materials.len()
                );
            }
            Err(e) => eprintln!("{}: {e} ({ms} ms)", f.display()),
        }
    }
    eprintln!("{built} of {} Maya samples built geometry", files.len());
    assert!(built > 0);
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "ma" || x == "mb") {
            out.push(p);
        }
    }
    out.sort();
}
