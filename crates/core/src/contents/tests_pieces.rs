//! Tests for the pieces of files that aren't archives: listing from a
//! loaded scene, picture, canvas and gallery, what showing each means,
//! saving them (original bytes unchanged, PNG, EXR and GLB), and damaged
//! files.

use super::*;
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::{
    Animation, Asset, Canvas, CanvasLayer, Expression, FrameClip, Image8, Info, Material, Mesh, MorphTarget, Node,
    PlayDirection, Primitive, Scene, Skin, TexRef, Texture,
};
use slerp_textures::{ColorSpace, Level, Original, Pixels, TextureKind};
use std::sync::Arc;

/// A new empty folder, by its real path: extraction reports real paths, and
/// on macOS the temporary folder is reached through a link.
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-pieces-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn item<'a>(c: &'a Contents, path: &str) -> &'a Item {
    c.items().iter().find(|i| i.path == path).unwrap_or_else(|| {
        let all: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
        panic!("no item {path} in {all:#?}")
    })
}

/// A 2x2 RGBA PNG.
fn small_png() -> Vec<u8> {
    let img =
        slerp_quake::convert::Image { width: 2, height: 2, rgba: (0..16).map(|i| i * 16).collect(), offset: None };
    png_bytes(&img).unwrap()
}

fn triangle(material: usize, targets: usize) -> Primitive {
    Primitive {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        uv0: vec![Vec2::ZERO, Vec2::X, Vec2::Y],
        indices: vec![0, 1, 2],
        targets: (0..targets).map(|_| MorphTarget { positions: vec![Vec3::Z; 3], normals: Vec::new() }).collect(),
        material: Some(material),
        ..Default::default()
    }
}

/// A body (with a hat under it) wearing skin and felt, two blend shapes on
/// the body, a texture with its original PNG and one without, a clip, a
/// skin and an expression.
fn sample_scene() -> Scene {
    let mut body = Node::new("Body");
    body.mesh = Some(0);
    body.children = vec![1];
    body.skin = Some(0);
    let mut hat = Node::new("Hat/Cap");
    hat.mesh = Some(1);
    hat.transform.translation = Vec3::new(0.0, 2.0, 0.0);
    let bone = Node::new("Bone");
    let skin_mat = Material {
        name: "Skin".into(),
        base_color_texture: Some(TexRef::new(0)),
        normal_texture: Some(TexRef::new(1)),
        ..Default::default()
    };
    let felt = Material { name: "Felt".into(), base_color: Vec4::new(0.2, 0.1, 0.0, 1.0), ..Default::default() };
    let mut with_png = Texture::from_rgba8(2, 2, (0..16).map(|i| i * 16).collect(), ColorSpace::Srgb);
    with_png.source.original = Some(Original::Memory(small_png().into()));
    with_png.source.name = "textures/Skin%20Colour.png".into();
    with_png.source.container = "PNG".into();
    let made = Texture::from_rgba8(1, 1, vec![128, 128, 255, 255], ColorSpace::Linear);
    Scene {
        name: "sample".into(),
        nodes: vec![body, hat, bone],
        roots: vec![0, 2],
        meshes: vec![
            Mesh {
                name: "Body".into(),
                primitives: vec![triangle(0, 2)],
                target_names: vec!["smile".into(), "".into()],
            },
            Mesh { name: "Hat".into(), primitives: vec![triangle(1, 0)], target_names: Vec::new() },
        ],
        materials: vec![skin_mat, felt],
        textures: vec![Arc::new(with_png), Arc::new(made)],
        skins: vec![Skin {
            name: String::new(),
            joints: vec![2],
            inverse_bind: vec![glam::Mat4::IDENTITY],
            skeleton: None,
        }],
        animations: vec![Animation { name: "Walk".into(), duration: 1.5, channels: Vec::new() }],
        expressions: vec![Expression { name: "happy".into(), preset: Some("happy".into()), binds: Vec::new() }],
        info: Info { format: "Test scene".into(), ..Default::default() },
        ..Default::default()
    }
}

fn scene_contents() -> Contents {
    let scene = sample_scene();
    let c = pieces::of_loaded(
        crate::Kind::Model(slerp_models::ModelFormat::Obj),
        Path::new("/nonexistent/sample.obj"),
        &Asset::Scene(clone_scene(&scene, None)),
        &[],
        None,
    )
    .expect("worth listing");
    c.set_scene(Arc::new(scene));
    c
}

/*┌─────────────────╮
| Scenes          |
└─────────────────╯*/
//region Scenes

#[test]
fn a_scene_lists_its_pieces() {
    let c = scene_contents();
    assert_eq!(c.label(), "Test scene");
    let paths: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Whole scene",
            "Nodes/Body",
            // Slashes in names don't make folders.
            "Nodes/Body/Hat_Cap",
            "Nodes/Bone",
            "Meshes/Body",
            "Meshes/Body/smile",
            "Meshes/Body/target 1",
            "Meshes/Hat",
            "Materials/Skin",
            "Materials/Felt",
            // A texture is named by its own name in the file, else by the
            // first slot that uses it.
            "Textures/Skin Colour",
            "Textures/Skin normal",
            "Animations/Walk",
            "Skeletons/Bone",
            "Expressions/happy",
        ]
    );
    let formats = |p: &str| item(&c, p).formats.clone();
    assert_eq!(formats("Whole scene"), [Format::Glb]);
    assert_eq!(formats("Nodes/Body/Hat_Cap"), [Format::Glb]);
    assert_eq!(formats("Meshes/Body"), [Format::Glb]);
    // The original PNG first, then a conversion; a made texture converts.
    assert_eq!(formats("Textures/Skin Colour"), [Format::Raw, Format::Png]);
    assert_eq!(formats("Textures/Skin normal"), [Format::Png]);
    // Blend shapes and expressions can't stand alone.
    assert!(formats("Meshes/Body/smile").is_empty() && !item(&c, "Meshes/Body/smile").can_save());
    assert!(formats("Expressions/happy").is_empty());
    assert_eq!(item(&c, "Meshes/Body").detail, "1 triangle, 2 blend shapes");
    assert_eq!(item(&c, "Whole scene").detail, "2 triangles");
    assert_eq!(item(&c, "Animations/Walk").detail, "1.50 s");
    assert_eq!(item(&c, "Materials/Skin").detail, "base colour, normal");
    // Auto takes the first format: the original, a GLB.
    assert_eq!(item(&c, "Textures/Skin Colour").resolve(Format::Auto), Format::Raw);
    assert_eq!(item(&c, "Textures/Skin normal").resolve(Format::Auto), Format::Png);
    assert_eq!(item(&c, "Meshes/Hat").resolve(Format::Auto), Format::Glb);
}

#[test]
fn showing_a_piece_isolates_it() {
    let c = scene_contents();
    let show = |p: &str| c.show(item(&c, p).id);
    assert_eq!(show("Whole scene"), Show::Whole);
    assert_eq!(show("Nodes/Body"), Show::Nodes { nodes: vec![0], subtree: true });
    assert_eq!(show("Meshes/Hat"), Show::Nodes { nodes: vec![1], subtree: false });
    assert_eq!(show("Materials/Felt"), Show::Nodes { nodes: vec![1], subtree: false });
    assert_eq!(show("Skeletons/Bone"), Show::Nodes { nodes: vec![0], subtree: false });
    assert_eq!(show("Meshes/Body/smile"), Show::BlendShape { nodes: vec![0], mesh: 0, target: 0 });
    assert_eq!(show("Animations/Walk"), Show::Clip(0));
    assert_eq!(show("Expressions/happy"), Show::Expression(0));
    assert_eq!(show("Textures/Skin Colour"), Show::Preview);
    // Hovering outlines the same nodes.
    assert_eq!(c.nodes(item(&c, "Materials/Skin").id), [0]);
    assert!(c.nodes(item(&c, "Animations/Walk").id).is_empty());
}

#[test]
fn original_bytes_come_out_unchanged() {
    let c = scene_contents();
    let id = item(&c, "Textures/Skin Colour").id;
    let (name, bytes) = c.export(id, Format::Auto).unwrap();
    assert_eq!((name.as_str(), bytes), ("Skin Colour.png", small_png()));
    assert_eq!(c.read(id).unwrap(), small_png());
    // A picture stored at a range of a file comes out as that range.
    let dir = temp("range");
    let file = dir.join("model.bin");
    let mut data = b"junk before".to_vec();
    data.extend_from_slice(&small_png());
    data.extend_from_slice(b"junk after");
    std::fs::write(&file, &data).unwrap();
    let mut scene = sample_scene();
    let mut t = (*scene.textures[0]).clone();
    t.source.original = Some(Original::File { path: file.clone(), offset: 11, len: small_png().len() as u64 });
    scene.textures[0] = Arc::new(t);
    let kind = crate::Kind::Model(slerp_models::ModelFormat::Obj);
    let c = pieces::of_loaded(kind, &dir.join("x.obj"), &Asset::Scene(scene), &[], None).unwrap();
    let id = item(&c, "Textures/Skin Colour").id;
    assert_eq!(c.export(id, Format::Raw).unwrap().1, small_png());
    // A conversion asked for is a conversion.
    let (name, png) = c.export(id, Format::Png).unwrap();
    assert_eq!(name, "Skin Colour.png");
    let decoded = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2, 2));
    // A file that shrank since is an error, not a short file.
    std::fs::write(&file, b"short").unwrap();
    assert!(c.export(id, Format::Raw).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scene_pieces_save_as_glb() {
    let c = scene_contents();
    let reload = |bytes: &[u8]| -> Scene {
        let dir = temp("glb");
        let path = dir.join("piece.glb");
        std::fs::write(&path, bytes).unwrap();
        let asset = crate::load(&path, &slerp_scene::Limits::viewer()).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        match asset {
            Asset::Scene(s) => s,
            _ => panic!("not a scene"),
        }
    };
    let (name, bytes) = c.export(item(&c, "Meshes/Hat").id, Format::Auto).unwrap();
    assert_eq!(name, "Hat.glb");
    assert_eq!(&bytes[..4], b"glTF");
    let hat = reload(&bytes);
    assert_eq!(hat.meshes.len(), 1);
    assert_eq!(hat.triangle_count(), 1);
    // The whole scene, named after the file.
    let (name, bytes) = c.export(item(&c, "Whole scene").id, Format::Glb).unwrap();
    assert_eq!(name, "sample.glb");
    assert_eq!(reload(&bytes).meshes.len(), 2);
    // A material on a ball.
    let (_, bytes) = c.export(item(&c, "Materials/Felt").id, Format::Glb).unwrap();
    let ball = reload(&bytes);
    assert_eq!(ball.materials.len(), 1);
    assert!(ball.triangle_count() > 100);
    // Formats a piece doesn't have are refused rather than guessed.
    assert!(c.export(item(&c, "Meshes/Body/smile").id, Format::Glb).is_err());
}

/// Collections list as folders of the collections inside them, then their
/// nodes; a collection shows and saves all of its nodes; a cycle, a node or
/// collection past the end and a collection in two places are survived.
#[test]
fn collections_list_as_folders_and_save_their_nodes() {
    let mut scene = sample_scene();
    let group = |name: &str, nodes: Vec<usize>, children: Vec<usize>| slerp_scene::Collection {
        name: name.into(),
        nodes,
        children,
    };
    scene.collections = vec![
        group("Characters", vec![0], vec![1]),
        // Props is inside itself, and Rigs inside Props.
        group("Props", vec![1], vec![1, 2]),
        group("Rigs", vec![2, 99], vec![9]),
    ];
    let kind = crate::Kind::Blend;
    let c = pieces::of_loaded(
        kind,
        Path::new("/nonexistent/sample.blend"),
        &Asset::Scene(clone_scene(&scene, None)),
        &[],
        None,
    )
    .unwrap();
    let paths: Vec<&str> =
        c.items().iter().map(|i| i.path.as_str()).filter(|p| p.starts_with("Collections/")).collect();
    assert_eq!(
        paths,
        [
            "Collections/Characters",
            "Collections/Characters/Props",
            "Collections/Characters/Props/Rigs",
            "Collections/Characters/Props/Rigs/Bone",
            "Collections/Characters/Props/Hat_Cap",
            "Collections/Characters/Body",
        ]
    );
    let characters = item(&c, "Collections/Characters");
    assert_eq!((characters.kind, characters.detail.as_str()), (ItemKind::Collection, "3 nodes"));
    assert_eq!(characters.formats, [Format::Glb]);
    assert_eq!(c.show(characters.id), Show::Nodes { nodes: vec![0, 1, 2], subtree: true });
    assert_eq!(c.nodes(item(&c, "Collections/Characters/Props/Rigs").id), [2]);
    assert_eq!(c.show(item(&c, "Collections/Characters/Body").id), Show::Nodes { nodes: vec![0], subtree: true });
    // Saved, a collection is all of its nodes.
    c.set_scene(Arc::new(scene));
    let (name, bytes) = c.export(item(&c, "Collections/Characters/Props").id, Format::Auto).unwrap();
    assert_eq!(name, "Props.glb");
    let dir = temp("collection");
    let path = dir.join(&name);
    std::fs::write(&path, &bytes).unwrap();
    let Ok(Asset::Scene(back)) = crate::load(&path, &slerp_scene::Limits::viewer()) else { panic!("a scene") };
    let names: Vec<&str> = back.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"Hat/Cap") && names.contains(&"Bone") && !names.contains(&"Body"), "{names:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A Blender file's collections, from the loader to the listing.
#[test]
fn blender_collections_are_listed() {
    let Some(path) = asset("blend-extra/demos/race_spaceship.blend") else { return };
    let c = Contents::open(&path).unwrap();
    for want in
        ["Collections/Set", "Collections/Set/Set Lights", "Collections/Ship/Ship Geometry", "Collections/Camera/Camera"]
    {
        assert!(c.items().iter().any(|i| i.path == want), "no {want}");
    }
    let ship = item(&c, "Collections/Ship");
    assert_eq!(ship.detail, "38 nodes");
    let (name, bytes) = c.export(ship.id, Format::Glb).unwrap();
    assert_eq!((name.as_str(), &bytes[..4]), ("Ship.glb", &b"glTF"[..]));
}

/// Meshes, nodes and collections get row pictures drawn from small scenes
/// of their own: a mesh where its node puts it (a skinned one where its
/// joints do), a node with what is beneath it, each with only the
/// materials it wears, and textures shrunk for drawing.
#[test]
fn scene_pieces_draw_their_own_row_pictures() {
    let mut scene = sample_scene();
    scene.textures[0] = Arc::new(Texture::from_rgba8(512, 256, vec![200; 512 * 256 * 4], ColorSpace::Srgb));
    // The body is skinned to the bone, which stands 5 to the right.
    scene.meshes[0].primitives[0].joints = vec![[0; 4]; 3];
    scene.meshes[0].primitives[0].weights = vec![Vec4::X; 3];
    scene.nodes[2].transform.translation = Vec3::new(5.0, 0.0, 0.0);
    let c = pieces::of_loaded(
        crate::Kind::Model(slerp_models::ModelFormat::Obj),
        Path::new("/nonexistent/sample.obj"),
        &Asset::Scene(clone_scene(&scene, None)),
        &[],
        None,
    )
    .unwrap();
    let hat = c.piece_scene(item(&c, "Meshes/Hat").id, &scene, &[]).unwrap();
    assert_eq!((hat.nodes.len(), hat.meshes.len(), hat.materials.len()), (1, 1, 1));
    assert_eq!(hat.materials[0].name, "Felt");
    assert_eq!(hat.meshes[0].primitives[0].material, Some(0));
    assert_eq!(hat.nodes[0].transform.translation, Vec3::new(0.0, 2.0, 0.0));
    let body = c.piece_scene(item(&c, "Meshes/Body").id, &scene, &[]).unwrap();
    assert_eq!(body.nodes[0].transform.translation, Vec3::new(5.0, 0.0, 0.0));
    assert!(body.nodes[0].skin.is_none() && body.skins.is_empty());
    let both = c.piece_scene(item(&c, "Nodes/Body").id, &scene, &[]).unwrap();
    assert_eq!((both.nodes.len(), both.roots.len(), both.materials.len()), (2, 2, 2));
    // What the file hides stays hidden, whatever the viewer hides now.
    let alone = c.piece_scene(item(&c, "Nodes/Body").id, &scene, &[true, false, true]).unwrap();
    assert_eq!(alone.nodes.len(), 1);
    assert!(
        c.is_scene_piece(item(&c, "Nodes/Bone").id) && c.piece_scene(item(&c, "Nodes/Bone").id, &scene, &[]).is_none()
    );
    assert!(!c.is_scene_piece(item(&c, "Animations/Walk").id));
    assert!(c.piece_scene(item(&c, "Animations/Walk").id, &scene, &[]).is_none());

    let mut renderer = crate::ThumbRenderer::new("software");
    let Some(img) = c.draw_piece(body, &mut renderer, 48) else {
        return eprintln!("skipped drawing: no lavapipe");
    };
    assert_eq!((img.width, img.height), (48, 48));
    assert!(img.pixels.chunks_exact(4).any(|p| p[3] > 0), "something is drawn");
    let small = c.small_textures.lock().unwrap();
    let (_, shrunk) = &small[&0];
    assert_eq!((shrunk.width(), shrunk.height()), (128, 64));
}

#[test]
fn extraction_writes_pieces_and_skips_what_cant_stand_alone() {
    let c = scene_contents();
    let out = temp("extract");
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let report = extract(&c, &ids, &out, &ExtractOptions::default()).unwrap();
    // With `/` between folders on every system.
    let mut written: Vec<String> =
        report.written.iter().map(|p| p.strip_prefix(&out).unwrap().to_string_lossy().replace('\\', "/")).collect();
    written.sort();
    assert!(written.contains(&"Textures/Skin Colour.png".to_string()), "{written:?}");
    assert!(written.contains(&"Meshes/Hat.glb".to_string()), "{written:?}");
    assert!(written.contains(&"Nodes/Body/Hat_Cap.glb".to_string()), "{written:?}");
    assert!(!written.iter().any(|w| w.contains("smile") || w.contains("happy")), "{written:?}");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(std::fs::read(out.join("Textures/Skin Colour.png")).unwrap(), small_png());
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn a_one_mesh_file_has_nothing_to_list() {
    let scene = Scene {
        meshes: vec![Mesh { name: "m".into(), primitives: vec![triangle(0, 0)], target_names: Vec::new() }],
        nodes: vec![Node { mesh: Some(0), ..Node::new("n") }],
        roots: vec![0],
        materials: vec![Material::default()],
        ..Default::default()
    };
    let kind = crate::Kind::Model(slerp_models::ModelFormat::Stl);
    // One node, one mesh, one material: just enough to be worth a panel.
    assert!(pieces::of_loaded(kind, Path::new("/nonexistent/a.stl"), &Asset::Scene(scene), &[], None).is_some());
    let empty = Scene::default();
    assert!(pieces::of_loaded(kind, Path::new("/nonexistent/a.stl"), &Asset::Scene(empty), &[], None).is_none());
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// A cube map of 2 levels (4x4 and 2x2), each face filled with its index.
fn cube(float: bool) -> Texture {
    let level = |size: u32| Level {
        width: size,
        height: size,
        depth: 1,
        surfaces: (0..6)
            .map(|f| {
                let n = (size * size) as usize;
                if float {
                    Pixels::Rgba32F([f as f32 * 2.5, 0.5, 0.25, 1.0].repeat(n))
                } else {
                    Pixels::Rgba8([f as u8 * 40, 7, 9, 255].repeat(n))
                }
            })
            .collect(),
    };
    let mut t = Texture::from_rgba8(1, 1, vec![0; 4], ColorSpace::Linear);
    t.kind = TextureKind::Cube;
    t.faces = 6;
    t.levels = vec![level(4), level(2)];
    t
}

#[test]
fn pictures_list_their_faces_and_mips() {
    let kind = crate::Kind::Texture(slerp_textures::Format::Ktx2);
    let c =
        pieces::of_loaded(kind, Path::new("/nonexistent/sky.ktx2"), &Asset::Image(Arc::new(cube(false))), &[], None)
            .unwrap();
    let paths: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(&paths[..4], ["+X", "+X/Mip 1", "-X", "-X/Mip 1"]);
    assert_eq!(paths.len(), 13);
    assert_eq!(paths[12], "Panorama");
    assert_eq!(item(&c, "Panorama").detail, "16 × 8, equirectangular");
    assert_eq!(item(&c, "+Y").kind, ItemKind::Face);
    assert_eq!(item(&c, "+Y/Mip 1").detail, "2 × 2");
    assert_eq!(c.show(item(&c, "-Z/Mip 1").id), Show::Surface { level: 1, layer: 0, face: 5 });
    // A face's mip as a PNG of exactly its pixels.
    let (name, png) = c.export(item(&c, "+Z/Mip 1").id, Format::Auto).unwrap();
    assert_eq!(name, "Mip 1.png");
    let t = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
    assert_eq!((t.width(), t.height()), (2, 2));
    let Pixels::Rgba8(px) = t.base() else { panic!("8-bit") };
    assert_eq!(&px[..4], [160, 7, 9, 255]);
    // Thumbnails for the rows.
    assert!(c.thumbnail(item(&c, "+X").id, 32).is_some());
}

#[test]
fn floating_point_pictures_save_as_exr() {
    let kind = crate::Kind::Texture(slerp_textures::Format::Dds);
    let c = pieces::of_loaded(kind, Path::new("/nonexistent/sky.dds"), &Asset::Image(Arc::new(cube(true))), &[], None)
        .unwrap();
    let id = item(&c, "-Y").id;
    assert_eq!(c.item(id).unwrap().formats, [Format::Exr, Format::Png]);
    let (name, exr) = c.export(id, Format::Auto).unwrap();
    assert_eq!(name, "-Y.exr");
    let dir = temp("exr");
    let path = dir.join(&name);
    std::fs::write(&path, &exr).unwrap();
    let Asset::Image(t) =
        slerp_images::load(slerp_images::ImageFormat::Exr, &path, &slerp_scene::Limits::viewer()).unwrap()
    else {
        panic!("a picture")
    };
    let Pixels::Rgba32F(px) = t.base() else { panic!("floats") };
    // Face 3's red is 7.5, kept exactly.
    assert_eq!(&px[..4], [7.5, 0.5, 0.25, 1.0]);
    let _ = std::fs::remove_dir_all(dir);
}

/// The renderer's `cube_dir` (`ibl.rs`): the direction through point
/// (`u`, `v`) of a face, each -1 to 1 with `v` downwards.
fn renderer_cube_dir(face: usize, u: f32, v: f32) -> Vec3 {
    match face {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
    .normalize()
}

/// Where the renderer reads direction `d` in an HDR panorama
/// (`cs_equirect` in `ibl.wgsl`), 0 to 1 across and down.
fn renderer_equirect_uv(d: Vec3) -> (f32, f32) {
    let pi = std::f32::consts::PI;
    (d.z.atan2(d.x) / (2.0 * pi) + 0.5, d.y.clamp(-1.0, 1.0).acos() / pi)
}

/// A float cube map of `size`-texel faces whose texels hold `f` of their
/// direction, as the renderer draws its own cubes.
fn cube_of(size: u32, f: impl Fn(Vec3) -> [f32; 4]) -> Texture {
    let faces = (0..6)
        .map(|face| {
            let mut px = Vec::with_capacity((size * size * 4) as usize);
            for y in 0..size {
                for x in 0..size {
                    let (u, v) =
                        ((x as f32 + 0.5) / size as f32 * 2.0 - 1.0, (y as f32 + 0.5) / size as f32 * 2.0 - 1.0);
                    px.extend(f(renderer_cube_dir(face, u, v)));
                }
            }
            Pixels::Rgba32F(px)
        })
        .collect();
    let mut t = Texture::from_rgba32f(1, 1, vec![0.0; 4]);
    t.kind = TextureKind::Cube;
    t.faces = 6;
    t.levels = vec![Level { width: size, height: size, depth: 1, surfaces: faces }];
    t
}

/// A cube whose texels are their own direction makes a panorama whose
/// pixels are the direction the renderer reads them for, so the faces'
/// order and orientation and the panorama's mapping both match the
/// renderer's.
#[test]
fn a_cube_map_makes_the_panorama_the_renderer_would_read() {
    let colour = |d: Vec3| [d.x * 0.5 + 0.5, d.y * 0.5 + 0.5, d.z * 0.5 + 0.5, 1.0];
    let cube = cube_of(64, colour);
    let pano = super::panorama::panorama(&cube, 0, 1 << 20).unwrap();
    assert_eq!((pano.width(), pano.height()), (256, 128));
    let Pixels::Rgba32F(px) = pano.base() else { panic!("floats stay floats") };
    let mut worst = 0f32;
    for y in 0..128u32 {
        for x in 0..256u32 {
            // The direction whose renderer lookup lands on this pixel.
            let (u, v) = ((x as f32 + 0.5) / 256.0, (y as f32 + 0.5) / 128.0);
            let d = super::panorama::direction(u, v);
            let (ru, rv) = renderer_equirect_uv(d);
            let du = (ru - u + 0.5).rem_euclid(1.0) - 0.5;
            assert!(du.abs() < 1e-4 && (rv - v).abs() < 1e-4, "({x}, {y}) is read at ({ru}, {rv})");
            let want = colour(d);
            let at = ((y * 256 + x) * 4) as usize;
            for k in 0..3 {
                worst = worst.max((px[at + k] - want[k]).abs());
            }
        }
    }
    assert!(worst < 0.02, "worst difference {worst}");
    // 8-bit faces give an 8-bit panorama in their colour space.
    let mut srgb = cube_of(4, colour);
    for s in &mut srgb.levels[0].surfaces {
        let Pixels::Rgba32F(f) = s else { unreachable!() };
        *s = Pixels::Rgba8(f.iter().map(|v| (v * 255.0) as u8).collect());
    }
    srgb.color_space = ColorSpace::Srgb;
    let small = super::panorama::panorama(&srgb, 0, 1 << 20).unwrap();
    assert!(matches!(small.base(), Pixels::Rgba8(_)) && small.color_space == ColorSpace::Srgb);
    assert_eq!((small.width(), small.height()), (16, 8));
    // Not a cube, or no such layer.
    assert!(super::panorama::panorama(&Texture::from_rgba8(2, 2, vec![0; 16], ColorSpace::Srgb), 0, 64).is_err());
    assert!(super::panorama::panorama(&cube, 1, 64).is_err());
}

/// A real HDR sky drawn into a cube as the renderer draws a panorama into
/// its lighting cube (`cs_equirect`), then made a panorama again, matches
/// the sky it started as; with two faces swapped it doesn't.
#[test]
fn an_hdr_sky_survives_a_round_trip_through_a_cube() {
    let Some(path) = asset("hdr/venice_sunset_1k.hdr") else { return };
    let sky = slerp_textures::decode_file(&path, &slerp_textures::DecodeOptions::full()).unwrap();
    let (w, h) = (sky.width(), sky.height());
    let Pixels::Rgba32F(src) = sky.base().clone() else { panic!("an HDR sky is floats") };
    let read = |u: f32, v: f32| -> [f32; 4] {
        // Bilinear, wrapping around and held at the poles, as the
        // renderer's sampler does.
        let x = u * w as f32 - 0.5;
        let y = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (x.floor(), y.floor() as u32);
        let (fx, fy) = (x - x0, y - y0 as f32);
        let texel = |x: i64, y: u32| {
            let x = x.rem_euclid(w as i64) as u32;
            let at = ((y.min(h - 1) * w + x) * 4) as usize;
            [src[at], src[at + 1], src[at + 2], 1.0]
        };
        let (a, b) = (texel(x0 as i64, y0), texel(x0 as i64 + 1, y0));
        let (c, d) = (texel(x0 as i64, y0 + 1), texel(x0 as i64 + 1, y0 + 1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            top + (c[k] + (d[k] - c[k]) * fx - top) * fy
        })
    };
    let face = w / 4;
    let cube = cube_of(face, |d| {
        let (u, v) = renderer_equirect_uv(d);
        read(u, v)
    });
    // Tone-mapped, so the sun doesn't decide everything.
    let difference = |cube: &Texture| -> f32 {
        let back = super::panorama::panorama(cube, 0, w).unwrap();
        assert_eq!((back.width(), back.height()), (w, h));
        let Pixels::Rgba32F(px) = back.base() else { panic!("floats") };
        let tone = |v: f32| v / (1.0 + v);
        let total: f32 = px
            .chunks_exact(4)
            .zip(src.chunks_exact(4))
            .map(|(a, b)| (0..3).map(|k| (tone(a[k]) - tone(b[k])).abs()).sum::<f32>() / 3.0)
            .sum();
        total / (w * h) as f32
    };
    let same = difference(&cube);
    let mut swapped = cube.clone();
    swapped.levels[0].surfaces.swap(4, 5);
    let wrong = difference(&swapped);
    eprintln!("round trip: {same:.4} mean difference; with +Z and -Z swapped: {wrong:.4}");
    assert!(same < 0.01, "mean difference {same}");
    assert!(wrong > 5.0 * same, "a swapped cube should differ: {wrong} vs {same}");
}

#[test]
fn a_cube_maps_panorama_saves_and_previews() {
    let kind = crate::Kind::Texture(slerp_textures::Format::Dds);
    let c = pieces::of_loaded(kind, Path::new("/nonexistent/sky.dds"), &Asset::Image(Arc::new(cube(true))), &[], None)
        .unwrap();
    let id = item(&c, "Panorama").id;
    assert_eq!(c.item(id).unwrap().formats, [Format::Exr, Format::Png]);
    assert_eq!(c.show(id), Show::Preview);
    let (name, exr) = c.export(id, Format::Auto).unwrap();
    assert_eq!(name, "sky panorama.exr");
    assert_eq!(&exr[..4], [0x76, 0x2f, 0x31, 0x01]);
    let (name, png) = c.export(id, Format::Png).unwrap();
    assert_eq!(name, "sky panorama.png");
    let t = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
    assert_eq!((t.width(), t.height()), (16, 8));
    let PreviewKind::File { name, .. } = c.preview(id).unwrap().kind else { panic!("a picture to show") };
    assert_eq!(name, "sky panorama.exr");
    assert!(c.thumbnail(id, 32).is_some());
    // A real cube map: the sky is above, the ground below.
    let Some(path) = asset("textures/ktx-software/ktx2/cubemap_yokohama_blze.ktx2") else { return };
    let c = Contents::open(&path).unwrap();
    let (name, png) = c.export(item(&c, "Panorama").id, Format::Auto).unwrap();
    assert_eq!(name, "cubemap_yokohama_blze panorama.png");
    let t = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
    assert_eq!((t.width(), t.height(), t.width() % 2), (8192, 4096, 0));
}

//endregion

/*┌─────────────────╮
| Canvases        |
└─────────────────╯*/
//region Canvases

/// A 2x1 canvas: a group holding "Ink", then "Paper"; frame `f` colours
/// shown layer `l`'s pixel `l` with value `(f + 1) * 10 + l`.
struct TestCanvas {
    layers: Vec<CanvasLayer>,
    clips: Vec<FrameClip>,
    info: Info,
}

impl TestCanvas {
    fn new() -> TestCanvas {
        let layer = |name: &str, depth, group| CanvasLayer {
            name: name.into(),
            visible: true,
            depth,
            group,
            note: String::new(),
        };
        TestCanvas {
            layers: vec![layer("Group", 0, true), layer("Ink", 1, false), layer("Paper", 0, false)],
            clips: vec![FrameClip { name: "run".into(), from: 1, to: 2, direction: PlayDirection::Forward, repeat: 0 }],
            info: Info { format: "Test sprite".into(), ..Default::default() },
        }
    }
}

impl Canvas for TestCanvas {
    fn info(&self) -> &Info {
        &self.info
    }
    fn size(&self) -> (u32, u32) {
        (3, 1)
    }
    fn layers(&self) -> &[CanvasLayer] {
        &self.layers
    }
    fn frames(&self) -> &[f32] {
        &[0.1, 0.1, 0.2]
    }
    fn clips(&self) -> &[FrameClip] {
        &self.clips
    }
    fn compose(&self, frame: usize, shown: &[bool], _scale: f32) -> Image8 {
        let mut img = Image8::new(3, 1);
        for (l, &on) in shown.iter().enumerate() {
            if on {
                img.pixels[l * 4..l * 4 + 4].copy_from_slice(&[((frame + 1) * 10 + l) as u8, 0, 0, 255]);
            }
        }
        img
    }
}

#[test]
fn canvases_list_layers_frames_and_tags() {
    let kind = crate::Kind::Sprite(slerp_sprites::SpriteFormat::Aseprite);
    let canvas: Arc<dyn Canvas> = Arc::new(TestCanvas::new());
    let c = pieces::of_loaded(kind, Path::new("/nonexistent/s.aseprite"), &Asset::Canvas(canvas), &[], None).unwrap();
    let paths: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Layers/Group",
            "Layers/Group/Ink",
            "Layers/Paper",
            "Frames/Frame 1",
            "Frames/Frame 2",
            "Frames/Frame 3",
            "Tags/run"
        ]
    );
    let pixels = |path: &str| {
        let (_, png) = c.export(item(&c, path).id, Format::Png).unwrap();
        let t = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
        let Pixels::Rgba8(px) = t.base().clone() else { panic!("8-bit") };
        (t.width(), px.chunks_exact(4).map(|p| p[0]).collect::<Vec<u8>>())
    };
    // A layer alone keeps the group it is in; a group keeps what it holds.
    assert_eq!(pixels("Layers/Group/Ink"), (3, vec![10, 11, 0]));
    assert_eq!(pixels("Layers/Paper"), (3, vec![0, 0, 12]));
    assert_eq!(pixels("Layers/Group"), (3, vec![10, 11, 0]));
    assert_eq!(pixels("Frames/Frame 3"), (3, vec![30, 31, 32]));
    // A tag is a sprite sheet of its frames side by side.
    assert_eq!(pixels("Tags/run"), (6, vec![20, 21, 22, 30, 31, 32]));
    assert_eq!(c.show(item(&c, "Tags/run").id), Show::Tag(0));
    assert_eq!(c.show(item(&c, "Frames/Frame 2").id), Show::Frame(1));
    assert!(c.thumbnail(item(&c, "Layers/Paper").id, 16).is_some());
}

//endregion

/*┌─────────────────╮
| Files           |
└─────────────────╯*/
//region Files

/// The minimal 3MF a slicer could write: one triangle, and a plate picture.
fn three_mf() -> Vec<u8> {
    let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;
    let types = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/><Default Extension="png" ContentType="image/png"/></Types>"#;
    let model = br#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources><build><item objectid="1"/></build></model>"#;
    let png = small_png();
    slerp_archive::testutil::zip(&[
        ("[Content_Types].xml", types),
        ("_rels/.rels", rels),
        ("3D/3dmodel.model", model),
        ("Metadata/plate_1.png", &png),
    ])
}

#[test]
fn zip_based_files_list_their_member_files_unchanged() {
    let dir = temp("3mf");
    let path = dir.join("part.3mf");
    std::fs::write(&path, three_mf()).unwrap();
    let c = Contents::open(&path).unwrap();
    assert!(c.items().iter().any(|i| i.path == "Meshes/mesh 0" || i.kind == ItemKind::Mesh), "{:#?}", c.items());
    let plate = item(&c, "Files/Metadata/plate_1.png");
    assert_eq!(plate.formats, [Format::Raw]);
    assert_eq!(c.export(plate.id, Format::Auto).unwrap(), ("plate_1.png".to_string(), small_png()));
    // The listing is the 3MF's, not the archive's.
    assert_eq!(c.kind(), Some(crate::Kind::Model(slerp_models::ModelFormat::ThreeMf)));
    assert_eq!(c.files(), std::slice::from_ref(&path));
    let _ = std::fs::remove_dir_all(dir);
}

/// The test corpus, when it is there.
fn asset(path: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(path);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// The byte ranges of a GLB's images, read straight from its JSON.
fn glb_images(bytes: &[u8]) -> Vec<Vec<u8>> {
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + len]).unwrap();
    let bin = 20 + len + 8;
    let views = json["bufferViews"].as_array().unwrap();
    json["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|img| {
            let v = &views[img["bufferView"].as_u64()? as usize];
            let (at, n) = (v["byteOffset"].as_u64().unwrap_or(0) as usize, v["byteLength"].as_u64()? as usize);
            Some(bytes[bin + at..bin + at + n].to_vec())
        })
        .collect()
}

#[test]
fn embedded_glb_pictures_come_out_byte_for_byte() {
    for name in [
        "gltf/khronos/Fox/glTF-Binary/Fox.glb",
        "gltf/khronos/DamagedHelmet/glTF-Binary/DamagedHelmet.glb",
        "gltf/khronos/Avocado/glTF-Binary/Avocado.glb",
        "vrm/Seed-san.vrm",
    ] {
        let Some(path) = asset(name) else { continue };
        let images = glb_images(&std::fs::read(&path).unwrap());
        let c = Contents::open(&path).unwrap();
        let textures: Vec<&Item> = c.items().iter().filter(|i| i.kind == ItemKind::Texture).collect();
        assert!(!textures.is_empty(), "{name}");
        let mut checked = 0;
        for t in textures.iter().filter(|t| t.formats.first() == Some(&Format::Raw)) {
            let (file, bytes) = c.export(t.id, Format::Auto).unwrap();
            assert!(images.contains(&bytes), "{name}: {} isn't one of the file's images", t.path);
            assert!(file.ends_with(".png") || file.ends_with(".jpg"), "{file}");
            checked += 1;
        }
        assert!(checked > 0, "{name}: no texture kept its original");
    }
}

#[test]
fn every_format_in_the_table_lists_its_pieces() {
    let cases: &[(&str, &[&str])] = &[
        ("gltf/khronos/Fox/glTF-Binary/Fox.glb", &["Animations/Walk", "Skeletons/_rootJoint", "Meshes/fox1"]),
        (
            "gltf/khronos/AnimatedMorphCube/glTF-Binary/AnimatedMorphCube.glb",
            &["Meshes/Cube/target 1", "Animations/Square"],
        ),
        ("vrm/Seed-san.vrm", &["Expressions/happy", "Meshes/head"]),
        ("fbx/assimp/box_orphant_embedded_texture.fbx", &["Textures/GridGrey"]),
        ("halflife/webgl.bsp", &["Textures/trollface base colour", "Meshes/world"]),
        ("halflife/bankje.mdl", &["Animations/idle", "Textures/wood.bmp base colour"]),
        ("quake-extra/mdl/steg.mdl", &["Textures/skin base colour"]),
        ("quake-extra/md2/dolphin.md2", &["Meshes/dolphin/glide1"]),
        ("quake-extra/md3/watercan.md3", &["Textures/watercan base colour"]),
        ("usd/apple/toy_car.usdz", &["Files/toy_car.usdc", "Meshes/toy_car_realistic_lod0"]),
        ("blend-extra/gltf-blender-io/01_alpha_blend.blend", &["Saved picture", "Meshes/Cube"]),
        ("textures/ktx-software/ktx2/cubemap_yokohama_blze.ktx2", &["+X", "-Z/Mip 11"]),
        ("textures/ktx-software/ktx2/bc3_unorm_array_7.ktx2", &["Layer 6"]),
        ("textures/dds/cube/Pretoria Gardens DX10.dds", &["+Y"]),
        ("images23-extra/exr/generated/py-aovs-zip.exr", &["Layers/diffuse (RGB)"]),
        ("images23-extra/exr/openexr-images/MultiResolution/Kapaa.exr", &["Mip 3", "Saved picture"]),
        ("aseprite/layers_and_tags.aseprite", &["Layers/Group 1/Layer 5", "Frames/Frame 4", "Tags/T3"]),
        ("tiled/tiled-examples/orthogonal-outside.tmx", &["Layers/Objects"]),
        ("editors-extra/ldtk/AutoLayers_1_basic.ldtk", &["Layers/IntGrid_layer"]),
        ("materials/ambientcg/Bricks097/Bricks097_1K-JPG.tres", &["Textures/Bricks097_1K-JPG_Color", "Material ball"]),
    ];
    for (name, expected) in cases {
        let Some(path) = asset(name) else { continue };
        let c = Contents::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        for want in *expected {
            assert!(
                c.items().iter().any(|i| i.path == *want),
                "{name}: no {want} in {:#?}",
                c.items().iter().map(|i| &i.path).collect::<Vec<_>>()
            );
        }
        // Every item that can be saved names a file, and the listing reads
        // as text.
        for i in c.items() {
            if i.can_save() {
                assert!(c.file_name(i.id, Format::Auto).is_some_and(|n| !n.is_empty()), "{name}: {}", i.path);
            }
        }
        assert!(list_text(&c, false).lines().count() > 1);
    }
}

#[test]
fn pictures_save_from_real_files() {
    let Some(path) = asset("textures/ktx-software/ktx2/cubemap_yokohama_blze.ktx2") else { return };
    let c = Contents::open(&path).unwrap();
    let (name, png) = c.export(item(&c, "+Y/Mip 2").id, Format::Auto).unwrap();
    assert_eq!(name, "Mip 2.png");
    let t = slerp_textures::decode(&png, &slerp_textures::DecodeOptions::full()).unwrap();
    assert_eq!((t.width(), t.height()), (512, 512));
    let Some(path) = asset("images23-extra/exr/generated/py-aovs-zip.exr") else { return };
    let c = Contents::open(&path).unwrap();
    let (name, exr) = c.export(item(&c, "Layers/depth (Z)").id, Format::Auto).unwrap();
    assert_eq!(name, "depth (Z).exr");
    assert_eq!(&exr[..4], [0x76, 0x2f, 0x31, 0x01]);
    let Some(path) = asset("aseprite/layers_and_tags.aseprite") else { return };
    let c = Contents::open(&path).unwrap();
    let (name, sheet) = c.export(item(&c, "Tags/T3").id, Format::Auto).unwrap();
    assert_eq!(name, "T3.png");
    let t = slerp_textures::decode(&sheet, &slerp_textures::DecodeOptions::full()).unwrap();
    let Some(Asset::Canvas(canvas)) = crate::load(&path, &slerp_scene::Limits::viewer()).ok() else { return };
    // Three frames side by side.
    assert_eq!((t.width(), t.height()), (canvas.size().0 * 3, canvas.size().1));
}

#[test]
fn damaged_files_never_panic() {
    let samples = [
        "gltf/khronos/Fox/glTF-Binary/Fox.glb",
        "vrm/Seed-san.vrm",
        "halflife/bankje.mdl",
        "halflife/webgl.bsp",
        "quake-extra/md2/dolphin.md2",
        "textures/ktx-software/ktx2/cubemap_yokohama_blze.ktx2",
        "textures/dds/cube/Pretoria Gardens DX10.dds",
        "aseprite/layers_and_tags.aseprite",
        "images23-extra/exr/generated/py-aovs-zip.exr",
        "usd/apple/toy_car.usdz",
        "fbx/assimp/box_orphant_embedded_texture.fbx",
        "blend-extra/gltf-blender-io/01_alpha_blend.blend",
    ];
    let dir = temp("damaged");
    let mut tried = 0;
    for name in samples {
        let Some(path) = asset(name) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        let file = dir.join(path.file_name().unwrap());
        let n = bytes.len();
        let mut versions: Vec<Vec<u8>> =
            [16, 64, n / 3, n / 2, n.saturating_sub(1)].iter().map(|&k| bytes[..k.min(n)].to_vec()).collect();
        // Bytes overwritten through the header and the middle.
        for at in [8, 20, 64, n / 4, n / 2] {
            let mut b = bytes.clone();
            for x in b.iter_mut().skip(at).take(32) {
                *x ^= 0xa5;
            }
            versions.push(b);
        }
        for v in versions {
            std::fs::write(&file, &v).unwrap();
            if let Ok(c) = Contents::open(&file) {
                // Whatever was listed saves or fails cleanly.
                for i in c.items().iter().filter(|i| i.can_save()).take(40) {
                    let _ = c.export(i.id, Format::Auto);
                    let _ = c.thumbnail(i.id, 32);
                }
                let _ = list_text(&c, true);
            }
            tried += 1;
        }
    }
    eprintln!("{tried} damaged files listed without a panic");
    let _ = std::fs::remove_dir_all(dir);
}

//endregion
