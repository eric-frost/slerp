use super::*;
use crate::pixels::QUAKE_PALETTE;
use crate::testutil::{self, asset, external};
use crate::wad::tests::build_wad;

fn load_path(p: &Path, limits: &Limits) -> Scene {
    let bytes = std::fs::read(p).unwrap();
    load(p, &bytes, limits).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn describe(s: &Scene) -> String {
    format!(
        "{} '{}': {} nodes, {} tris, {} materials, {} textures, {} markers, warnings {:?}, details {:?}",
        s.info.format,
        s.name,
        s.nodes.len(),
        s.triangle_count(),
        s.materials.len(),
        s.textures.len(),
        s.markers.len(),
        s.info.warnings,
        s.info.details
    )
}

/*┌─────────────────╮
| Synthetic maps  |
└─────────────────╯*/
//region Synthetic maps

/// Splits a BSP into its version bytes and lump contents.
fn split(bytes: &[u8]) -> ([u8; 4], Vec<Vec<u8>>) {
    let b = Bytes::new(bytes);
    let lumps = (0..LUMP_COUNT)
        .map(|i| {
            let (o, l) = (b.index(4 + 8 * i).unwrap(), b.index(8 + 8 * i).unwrap());
            bytes[o..o + l].to_vec()
        })
        .collect();
    (bytes[..4].try_into().unwrap(), lumps)
}

/// Writes lumps back into a BSP, 4-byte aligned.
fn join(version: [u8; 4], lumps: &[Vec<u8>]) -> Vec<u8> {
    let mut out = version.to_vec();
    out.resize(HEADER_BYTES, 0);
    for (i, l) in lumps.iter().enumerate() {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let at = out.len() as u32;
        out[4 + 8 * i..8 + 8 * i].copy_from_slice(&at.to_le_bytes());
        out[8 + 8 * i..12 + 8 * i].copy_from_slice(&(l.len() as u32).to_le_bytes());
        out.extend_from_slice(l);
    }
    out
}

/// Turns a Quake version 29 map into a Half-Life version 30 one: textures
/// gain palettes (or move to a WAD3 when `external`), lightmaps become RGB
/// with byte offsets, and the worldspawn names the WAD by a Windows path.
fn to_half_life(quake: &[u8], external: bool) -> (Vec<u8>, Vec<u8>) {
    let (_, mut lumps) = split(quake);
    let tex = Bytes::new(&lumps[TEXTURES]);
    let count = tex.index(0).unwrap();
    let mut out_tex = Vec::new();
    let mut wad_lumps = Vec::new();
    let mut blobs = Vec::new();
    for i in 0..count {
        let ofs = tex.i32(4 + 4 * i).unwrap();
        if ofs < 0 {
            blobs.push(None);
            continue;
        }
        let ofs = ofs as usize;
        let h = miptex::Header::read(tex, ofs).unwrap();
        let n = (h.width * h.height) as usize;
        let mut mt = lumps[TEXTURES][ofs..ofs + 40].to_vec();
        if h.is_external() {
            blobs.push(Some(mt));
            continue;
        }
        let pixels = &lumps[TEXTURES][ofs + h.offsets[0] as usize..ofs + h.offsets[0] as usize + n * 85 / 64];
        mt.extend_from_slice(pixels);
        mt.extend_from_slice(&256u16.to_le_bytes());
        mt.extend_from_slice(QUAKE_PALETTE);
        mt.extend_from_slice(&[0, 0]);
        if external {
            wad_lumps.push((h.name.clone(), 0x43u8, mt.clone()));
            let mut header = mt[..40].to_vec();
            header[24..40].fill(0);
            blobs.push(Some(header));
        } else {
            blobs.push(Some(mt));
        }
    }
    out_tex.extend_from_slice(&(count as u32).to_le_bytes());
    let mut at = 4 + 4 * count;
    let mut body = Vec::new();
    for b in &blobs {
        match b {
            Some(b) => {
                out_tex.extend_from_slice(&(at as i32).to_le_bytes());
                at += b.len();
                body.extend_from_slice(b);
            }
            None => out_tex.extend_from_slice(&(-1i32).to_le_bytes()),
        }
    }
    out_tex.extend_from_slice(&body);
    lumps[TEXTURES] = out_tex;
    lumps[LIGHTING] = lumps[LIGHTING].iter().flat_map(|&v| [v, v, v]).collect();
    for f in lumps[FACES].chunks_exact_mut(20) {
        let ofs = i32::from_le_bytes(f[16..20].try_into().unwrap());
        if ofs >= 0 {
            f[16..20].copy_from_slice(&(ofs * 3).to_le_bytes());
        }
    }
    let ents: String = String::from_utf8_lossy(&lumps[ENTITIES])
        .lines()
        .filter(|l| !l.starts_with("\"wad\""))
        .collect::<Vec<_>>()
        .join("\n");
    let ents = ents.replacen('{', "{\n\"wad\" \"\\sierra\\half-life\\valve\\test.wad\"", 1);
    lumps[ENTITIES] = ents.into_bytes();
    let refs: Vec<(&str, u8, Vec<u8>)> = wad_lumps.iter().map(|(n, k, d)| (n.as_str(), *k, d.clone())).collect();
    (join([30, 0, 0, 0], &lumps), build_wad(b"WAD3", &refs))
}

//endregion

/*┌─────────────────╮
| Tests           |
└─────────────────╯*/
//region Tests

#[test]
fn spawn_at_eye_height_and_facing() {
    let Some(p) = asset("quake-extra/libquake/maps/start.bsp") else { return };
    let s = load_path(&p, &Limits::viewer());
    // info_player_start: origin "0 -160 424", angle 90 (facing Quake +Y).
    let spawn = s.spawn.unwrap();
    let want = Vec3::new(0.0, 424.0 + 22.0, 160.0) * 0.0254;
    assert!((spawn.position - want).length() < 1e-4, "{:?}", spawn.position);
    assert!(spawn.yaw.abs() < 1e-6);
    assert_eq!(s.name, "Tainted Tech Threshold");
    assert_eq!(s.info.format, "Quake BSP");
    // Quake +Y is -Z here, which is where yaw 0 looks.
    let facing = slerp_scene::glam::Quat::from_rotation_y(spawn.yaw) * Vec3::NEG_Z;
    assert!((facing - to_y_up(Vec3::Y) / crate::UNIT).length() < 1e-5);
}

#[test]
fn faces_wind_counter_clockwise_along_their_normals() {
    let Some(p) = asset("quake-extra/libquake/maps/start.bsp") else { return };
    let s = load_path(&p, &Limits::viewer());
    let (mut agree, mut total) = (0, 0);
    for prim in s.meshes.iter().flat_map(|m| &m.primitives) {
        for t in prim.indices.chunks_exact(3) {
            let p = [0, 1, 2].map(|k| prim.positions[t[k] as usize]);
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            if n.length() > 1e-8 {
                total += 1;
                agree += (n.dot(prim.normals[t[0] as usize]) > 0.0) as usize;
            }
        }
    }
    assert_eq!(agree, total);
}

#[test]
fn materials_and_lightmaps() {
    let Some(p) = asset("quake-extra/libquake/maps/start.bsp") else { return };
    let s = load_path(&p, &Limits::viewer());
    let outdoor = asset("quake-extra/libquake/maps/lq_e0m3.bsp").map(|p| load_path(&p, &Limits::viewer()));
    if let Some(outdoor) = outdoor {
        let sky = outdoor.materials.iter().find(|m| m.effect == Effect::Sky).expect("lq_e0m3 has sky");
        assert!(sky.unlit && sky.lightmap_texture.is_none() && sky.base_color_texture.is_some());
        let tex = &outdoor.textures[sky.base_color_texture.unwrap().texture];
        assert_eq!(tex.width(), 2 * tex.height(), "both sky layers side by side");
    }
    let lit: Vec<&Material> = s.materials.iter().filter(|m| m.lightmap_texture.is_some()).collect();
    assert!(!lit.is_empty());
    assert!(lit.iter().all(|m| m.lightmap_scale == 2.0 && m.lightmap_texture.unwrap().uv_set == 1));
    assert!(s.materials.iter().any(|m| m.emissive_texture.is_some()), "start.bsp has fullbright lamps");
    assert!(
        s.materials.iter().all(|m| !m.name.eq_ignore_ascii_case("trigger") && !m.name.eq_ignore_ascii_case("clip"))
    );
    for prim in s.meshes.iter().flat_map(|m| &m.primitives) {
        let mat = &s.materials[prim.material.unwrap()];
        assert_eq!(prim.uv0.len(), prim.positions.len());
        assert_eq!(prim.uv1.len(), if mat.lightmap_texture.is_some() { prim.positions.len() } else { 0 });
        assert!(prim.uv1.iter().all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));
    }
    // The .lit file next to start.bsp makes the light coloured.
    assert!(s.info.details.iter().any(|(k, v)| k == "Lightmap" && v.starts_with("coloured")));
    let no_neighbours = Limits { neighbours: false, ..Limits::viewer() };
    let s = load_path(&p, &no_neighbours);
    assert!(s.info.details.iter().any(|(k, v)| k == "Lightmap" && v.starts_with("grey")));
}

#[test]
fn thumbnail_limits_shrink_textures_but_not_lightmaps() {
    let Some(p) = asset("quake-extra/libquake/maps/lq_e0m1.bsp") else { return };
    let limits = Limits { max_texture_size: Some(32), ..Limits::thumbnail() };
    let s = load_path(&p, &limits);
    for m in &s.materials {
        if let Some(t) = m.base_color_texture {
            assert!(s.textures[t.texture].width().max(s.textures[t.texture].height()) <= 32);
        }
        if let Some(t) = m.lightmap_texture {
            assert!(s.textures[t.texture].width() > 32);
        }
    }
    let too_many = Limits { max_triangles: 100, ..Limits::viewer() };
    let bytes = std::fs::read(&p).unwrap();
    assert!(matches!(load(&p, &bytes, &too_many), Err(LoadError::TooLarge(_))));
}

#[test]
fn half_life_maps_with_embedded_and_wad_textures() {
    let Some(p) = asset("quake-extra/libquake/maps/start.bsp") else { return };
    let quake = std::fs::read(&p).unwrap();
    let dir = std::env::temp_dir().join(format!("slerp-quake-hl-{}", std::process::id()));
    let maps = dir.join("Valve/maps");
    std::fs::create_dir_all(&maps).unwrap();

    // 1. Textures inside the map.
    let (hl, _) = to_half_life(&quake, false);
    let path = maps.join("embedded.bsp");
    std::fs::write(&path, &hl).unwrap();
    let s = load_path(&path, &Limits::viewer());
    assert_eq!(s.info.format, "Half-Life BSP");
    assert!(s.materials.iter().all(|m| m.lightmap_texture.is_none() || m.lightmap_scale == 1.0));
    let spawn = s.spawn.unwrap();
    assert!((spawn.position.y - (424.0 + 28.0) * 0.0254).abs() < 1e-4, "Half-Life's eye height is 28 units");
    let with_pixels = s.materials.iter().filter(|m| m.base_color_texture.is_some()).count();

    // 2. Textures in a WAD3 named by a Windows path, found one folder up
    //    with a different letter case.
    let (hl, wad) = to_half_life(&quake, true);
    let path = maps.join("external.bsp");
    std::fs::write(&path, &hl).unwrap();
    std::fs::write(dir.join("Valve/TEST.WAD"), &wad).unwrap();
    let s = load_path(&path, &Limits::viewer());
    assert_eq!(
        s.materials.iter().filter(|m| m.base_color_texture.is_some()).count(),
        with_pixels,
        "{:?}",
        s.info.warnings
    );
    assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);

    // 3. Without neighbours the map still loads, untextured, with a note.
    let s = load_path(&path, &Limits { neighbours: false, ..Limits::viewer() });
    assert!(s.materials.iter().all(|m| m.base_color_texture.is_none()));
    assert!(s.info.warnings.iter().any(|w| w.contains("WAD")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_maps_never_panic() {
    let Some(p) = asset("quake-extra/libquake/maps/b_batt0.bsp") else { return };
    let bytes = std::fs::read(&p).unwrap();
    let limits = Limits { neighbours: false, ..Limits::viewer() };
    for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
        let r = std::panic::catch_unwind(|| load(&p, m, &limits).map(|s| s.triangle_count()));
        assert!(r.is_ok(), "mutation {i} panicked");
    }
    for cut in [0, 3, 4, 100, 123, 124] {
        assert!(load(&p, &bytes[..cut], &limits).is_err());
    }
    // A lump pointing past the end.
    let mut bad = bytes.clone();
    bad[4 + 8 * FACES..8 + 8 * FACES].copy_from_slice(&0x7fff_fff0u32.to_le_bytes());
    assert!(load(&p, &bad, &limits).is_err());
}

#[test]
fn real_maps_load() {
    let mut paths = Vec::new();
    for p in [
        testutil::chalk!("local/test-old.bsp"),
        testutil::chalk!("local/yard-old.bsp"),
        testutil::chalk!("local/calib/hall.bsp"),
    ] {
        paths.extend(external(p));
    }
    for m in ["start", "lq_e0m1", "lq_e0m3", "lq_e0m8", "dev", "b_batt0"] {
        paths.extend(asset(&format!("quake-extra/libquake/maps/{m}.bsp")));
    }
    for p in paths {
        let bytes = std::fs::read(&p).unwrap();
        let t = std::time::Instant::now();
        let thumb = load(&p, &bytes, &Limits::thumbnail()).unwrap();
        let thumb_time = t.elapsed();
        let t = std::time::Instant::now();
        let s = load(&p, &bytes, &Limits::viewer()).unwrap();
        eprintln!("{}: thumbnail {thumb_time:?}, viewer {:?}: {}", p.display(), t.elapsed(), describe(&s));
        assert!(s.triangle_count() > 0 && thumb.triangle_count() == s.triangle_count());
        assert_eq!(s.kind, SceneKind::Level);
    }
}

#[test]
fn bsp2_rmq_variant() {
    // 2PSB differs from BSP2 only in the node and leaf lumps, which
    // drawing doesn't read, so a relabelled BSP2 exercises its path.
    let Some(p) = external(testutil::chalk!("local/yard-old.bsp")) else { return };
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[..4].copy_from_slice(b"2PSB");
    assert_eq!(crate::sniff(&p, &bytes[..128]), Some(crate::QuakeFormat::Bsp));
    let s = load(&p, &bytes, &Limits::viewer()).unwrap();
    assert_eq!(s.info.format, "Quake BSP2 (2PSB)");
    assert!(s.triangle_count() > 0);
}

#[test]
fn atlas_pages_hold_any_block() {
    let block = |width, height| FaceLight {
        width,
        height,
        sample: 0,
        slots: vec![],
        s: Vec4::ZERO,
        t: Vec4::ZERO,
        page: 0,
        x: 0,
        y: 0,
    };
    let mut faces: Vec<FaceLight> = (0..2000).map(|_| block(17, 9)).collect();
    faces.push(block(300, 1000));
    let mut blocks: Vec<&mut FaceLight> = faces.iter_mut().collect();
    let pages = pack(&mut blocks);
    for b in &blocks {
        let (w, h) = pages[b.page];
        assert!(b.x + b.width < w && b.y + b.height < h, "block inside its page");
    }
}

#[test]
fn chalk_bsp2_with_decoupled_hdr_light() {
    let Some(p) = external(testutil::chalk!("local/calib/hall.bsp")) else { return };
    let s = load_path(&p, &Limits::viewer());
    assert_eq!(s.info.format, "Quake BSP2");
    let lm = s.materials.iter().find_map(|m| m.lightmap_texture).unwrap();
    let tex = &s.textures[lm.texture];
    assert!(tex.is_hdr());
    // The hall's roof is lit: some luxels are bright on the 8-bit scale.
    let slerp_scene::textures::Pixels::Rgba32F(px) = tex.base() else { panic!() };
    assert!(px.chunks_exact(4).any(|p| p[0] > 0.3));
}

/// Writes top-down and 3/4 overviews of a LibreQuake map, with markers and
/// the spawn point, for checking by eye.
#[test]
fn debug_overview() {
    let Some(p) = asset("quake-extra/libquake/maps/start.bsp") else { return };
    let s = load_path(&p, &Limits::viewer());
    let size = 1024;
    let mut top = testutil::render(&s, Vec3::NEG_Y, size, true);
    let mut points: Vec<(Vec3, [u8; 3])> = s
        .markers
        .iter()
        .map(|m| (m.position, [(m.color.x * 255.0) as u8, (m.color.y * 255.0) as u8, (m.color.z * 255.0) as u8]))
        .collect();
    points.extend(s.spawn.map(|sp| (sp.position, [255, 0, 255])));
    testutil::overlay_points(&s, &mut top, size, &points);
    let three_quarter = testutil::render(&s, Vec3::new(-1.0, -1.2, -0.8), size, true);
    // Side by side.
    let mut both = vec![0u8; (size * 2 * size * 4) as usize];
    for y in 0..size as usize {
        let row = size as usize * 4;
        both[y * row * 2..y * row * 2 + row].copy_from_slice(&top[y * row..(y + 1) * row]);
        both[y * row * 2 + row..(y + 1) * row * 2].copy_from_slice(&three_quarter[y * row..(y + 1) * row]);
    }
    testutil::write_png(Path::new("/tmp/slerp-quake/start-overview.png"), size * 2, size, &both);
}

//endregion
