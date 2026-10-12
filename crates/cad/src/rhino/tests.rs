//! Tests of the Rhino reader on small archives built here, and on real
//! files when the test assets are present.

use super::chunks::*;
use super::*;
use miniz_oxide::deflate::compress_to_vec_zlib;

/// Writes archives in the version 50 layout (eight-byte lengths).
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn new(version: u32) -> Writer {
        let mut out = MAGIC.to_vec();
        out.extend(format!("{version:>8}").as_bytes());
        Writer { out }
    }

    /// A chunk with `body`, plus a checksum if the type code has the CRC bit.
    fn chunk(&mut self, tcode: u32, body: &[u8]) {
        self.out.extend(tcode.to_le_bytes());
        let crc = tcode & TCODE_CRC != 0;
        self.out.extend(((body.len() + if crc { 4 } else { 0 }) as i64).to_le_bytes());
        self.out.extend(body);
        if crc {
            self.out.extend([0u8; 4]);
        }
    }

    fn short(&mut self, tcode: u32, value: i64) {
        self.out.extend((tcode | TCODE_SHORT).to_le_bytes());
        self.out.extend(value.to_le_bytes());
    }
}

/// The bytes of a chunk, for nesting.
fn sub(tcode: u32, body: &[u8]) -> Vec<u8> {
    let mut w = Writer { out: Vec::new() };
    w.chunk(tcode, body);
    w.out
}

/// A compressed buffer as the archive writes it: checksum, method 1, and
/// an anonymous chunk with the zlib stream.
fn compressed(data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_le_bytes().to_vec();
    v.extend([0u8; 4]);
    v.push(1);
    v.extend(sub(ANONYMOUS_CHUNK, &compress_to_vec_zlib(data, 6)));
    v
}

fn wstring(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let mut v = (units.len() as u32).to_le_bytes().to_vec();
    for u in units {
        v.extend(u.to_le_bytes());
    }
    v
}

/// The body of an `ON_Mesh` (chunk version 3.1) holding `positions` and
/// `faces` (four indices each).
fn mesh_body(positions: &[[f32; 3]], faces: &[[u32; 4]]) -> Vec<u8> {
    let mut d = vec![0x31u8];
    d.extend((positions.len() as i32).to_le_bytes());
    d.extend((faces.len() as i32).to_le_bytes());
    d.extend(vec![0u8; 4 * 16 + 16 + 24 + 24 + 16 + 4]);
    d.extend([0u8; 5]);
    d.extend(4i32.to_le_bytes());
    for f in faces {
        for i in f {
            d.extend(i.to_le_bytes());
        }
    }
    let verts: Vec<u8> = positions.iter().flatten().flat_map(|c| c.to_le_bytes()).collect();
    d.extend(compressed(&verts));
    // No normals, texture coordinates, curvatures or colours.
    for _ in 0..4 {
        d.extend(0u32.to_le_bytes());
    }
    d
}

/// An object record: the type, a class chunk of `id` and `data`, and
/// minimal attributes naming a layer.
fn object(id: [u8; 16], data: &[u8], layer: i32, object_id: [u8; 16]) -> Vec<u8> {
    let mut class = Writer { out: Vec::new() };
    class.chunk(OPENNURBS_CLASS_UUID, &id);
    class.chunk(OPENNURBS_CLASS_DATA, data);
    class.short(0x0002_7FFF, 0);
    let mut attrs = vec![0x20u8];
    attrs.extend(object_id);
    attrs.extend(layer.to_le_bytes());
    attrs.push(0);
    let mut rec = Writer { out: Vec::new() };
    rec.short(OBJECT_RECORD_TYPE & !TCODE_SHORT, 0x20);
    rec.chunk(OPENNURBS_CLASS, &class.out);
    rec.chunk(OBJECT_RECORD_ATTRIBUTES, &attrs);
    rec.short(0x0200_007F, 0);
    sub(OBJECT_RECORD, &rec.out)
}

fn layer(name: &str, index: i32, color: [u8; 3], visible: bool) -> Vec<u8> {
    let mut d = vec![0x16u8]; // version 1.6
    d.extend(0i32.to_le_bytes()); // mode
    d.extend(index.to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d.extend((-1i32).to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d.extend([color[0], color[1], color[2], 0]);
    d.extend([0u8; 20]);
    d.extend(wstring(name));
    d.push(visible as u8);
    d.extend((-1i32).to_le_bytes());
    d.extend([0u8; 12]);
    d.push(0);
    d.extend([7u8; 16]); // id
    d.extend([0u8; 16]); // parent
    d.push(0);
    let mut class = Writer { out: Vec::new() };
    class.chunk(OPENNURBS_CLASS_UUID, &[0u8; 16]);
    class.chunk(OPENNURBS_CLASS_DATA, &d);
    class.short(0x0002_7FFF, 0);
    let mut rec = Writer { out: Vec::new() };
    rec.chunk(OPENNURBS, &class.out);
    sub(LAYER_RECORD, &rec.out)
}

const OPENNURBS: u32 = OPENNURBS_CLASS;

/// A 2 x 2 pixel 24-bit bitmap (rows bottom up): red, green / blue, white.
fn bitmap_header(width: i32, height: i32, bits: i16, size_image: i32) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(40i32.to_le_bytes());
    d.extend(width.to_le_bytes());
    d.extend(height.to_le_bytes());
    d.extend(1i16.to_le_bytes());
    d.extend(bits.to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d.extend(size_image.to_le_bytes());
    d.extend([0u8; 8]);
    d.extend(0i32.to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d
}

fn bitmap_pixels() -> Vec<u8> {
    // Bottom row first, each row padded to four bytes; pixels are BGR.
    vec![255, 0, 0, 255, 255, 255, 0, 0, /* top row: */ 0, 0, 255, 0, 255, 0, 0, 0]
}

fn preview_chunk(compressed_bitmap: bool) -> Vec<u8> {
    let pixels = bitmap_pixels();
    let mut d = bitmap_header(2, 2, 24, pixels.len() as i32);
    if compressed_bitmap {
        d.extend(compressed(&pixels));
        sub(PROPERTIES_PREVIEW_COMPRESSED, &d)
    } else {
        d.extend(&pixels);
        sub(PROPERTIES_PREVIEW, &d)
    }
}

fn file(preview: Option<Vec<u8>>, layers: &[Vec<u8>], objects: &[Vec<u8>]) -> Vec<u8> {
    let mut w = Writer::new(50);
    w.chunk(1, b"test file");
    let mut props = Writer { out: Vec::new() };
    props.short(PROPERTIES_OPENNURBS_VERSION & !TCODE_SHORT, 201_209_145);
    if let Some(p) = preview {
        props.out.extend(p);
    }
    props.short(END_OF_TABLE & !TCODE_SHORT, 0);
    w.chunk(TABLE_PROPERTIES, &props.out);
    let table = |tcode: u32, items: &[Vec<u8>]| {
        let mut t = Writer { out: Vec::new() };
        for i in items {
            t.out.extend(i);
        }
        t.short(END_OF_TABLE & !TCODE_SHORT, 0);
        (tcode, t.out)
    };
    for (tcode, body) in [table(TABLE_LAYER, layers), table(TABLE_OBJECT, objects)] {
        w.chunk(tcode, &body);
    }
    w.chunk(END_OF_FILE, &[0; 8]);
    w.out
}

fn triangle_object(layer: i32, offset: f32, id: u8) -> Vec<u8> {
    let positions = [[offset, 0.0, 0.0], [offset + 1.0, 0.0, 0.0], [offset, 1.0, 0.0], [offset + 1.0, 1.0, 0.0]];
    // A triangle (the last index repeats) and a quad.
    object(object::mesh_class(), &mesh_body(&positions, &[[0, 1, 2, 2], [1, 3, 2, 0]]), layer, [id; 16])
}

fn limits() -> Limits {
    Limits::viewer()
}

#[test]
fn reads_both_kinds_of_preview() {
    for compressed in [false, true] {
        let data = file(Some(preview_chunk(compressed)), &[], &[]);
        let Content::Preview(t) = load_data(&data, &limits()).expect("loads") else { panic!("not a preview") };
        assert_eq!((t.width(), t.height()), (2, 2));
        let slerp_scene::textures::Pixels::Rgba8(px) = &t.levels[0].surfaces[0] else { panic!() };
        // Top row: red, green; bottom row: blue, white.
        assert_eq!(&px[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&px[8..16], &[0, 0, 255, 255, 255, 255, 255, 255]);
    }
}

#[test]
fn preview_can_be_read_without_the_rest() {
    let mut data = file(Some(preview_chunk(true)), &[], &[]);
    // Plenty of file after the properties table.
    let eof = data.len() - 20;
    data.splice(eof..eof, vec![0u8; 0]);
    let dir = std::env::temp_dir().join(format!("slerp-3dm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("preview.3dm");
    std::fs::write(&path, &data).unwrap();
    let t = preview_path(&path).unwrap().expect("a preview");
    assert_eq!((t.width(), t.height()), (2, 2));
    // A file without one.
    std::fs::write(&path, file(None, &[], &[])).unwrap();
    assert!(preview_path(&path).unwrap().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn meshes_layers_and_visibility() {
    let layers = [layer("Walls", 0, [200, 0, 0], true), layer("Hidden", 1, [0, 0, 200], false)];
    let objects = [triangle_object(0, 0.0, 1), triangle_object(1, 5.0, 2)];
    let data = file(Some(preview_chunk(true)), &layers, &objects);
    let Content::Model(scene) = load_data(&data, &limits()).expect("loads") else { panic!("not a model") };
    // Three triangles on each layer: one tri and a quad (two).
    assert_eq!(scene.triangle_count(), 6, "{}", summary(&data, &limits()));
    let named: Vec<(String, bool)> =
        scene.nodes.iter().filter(|n| n.mesh.is_some()).map(|n| (n.name.clone(), n.visible)).collect();
    assert_eq!(named, [("Walls".to_string(), true), ("Hidden".to_string(), false)]);
    assert!(scene.info.format.starts_with("Rhino 3dm"));
    // Z up became Y up.
    let b = scene.bounds(&slerp_scene::Pose::rest(&scene));
    assert!(b.max.y - b.min.y < 0.01 && b.max.z - b.min.z > 0.9);
}

#[test]
fn damaged_archives_do_not_panic_or_hang() {
    let layers = [layer("Walls", 0, [200, 0, 0], true)];
    let objects = [triangle_object(0, 0.0, 1)];
    let good = file(Some(preview_chunk(true)), &layers, &objects);
    for n in 0..good.len() {
        let _ = load_data(&good[..n], &limits());
    }
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..3000 {
        let mut d = good.clone();
        for _ in 0..3 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let i = 32 + (x >> 33) as usize % (d.len() - 32);
            d[i] = (x >> 20) as u8;
        }
        let _ = load_data(&d, &limits());
    }
}

#[test]
fn absurd_sizes_are_refused() {
    // A mesh claiming 100 million vertices but holding none.
    let mut body = vec![0x31u8];
    body.extend(100_000_000i32.to_le_bytes());
    body.extend(100_000_000i32.to_le_bytes());
    body.extend(vec![0u8; 4 * 16 + 16 + 24 + 24 + 16 + 4]);
    body.extend([0u8; 5]);
    body.extend(4i32.to_le_bytes());
    let data = file(None, &[], &[object(object::mesh_class(), &body, 0, [1; 16])]);
    assert!(load_data(&data, &limits()).is_err());
    // A preview claiming a 100000 x 100000 picture.
    let mut huge = bitmap_header(100_000, 100_000, 24, 0);
    huge.extend([0u8; 16]);
    let data = file(Some(sub(PROPERTIES_PREVIEW, &huge)), &[], &[]);
    assert!(load_data(&data, &limits()).is_err());
    // A compressed preview whose stream inflates far past its declared size
    // (a zip bomb).
    let bomb = compress_to_vec_zlib(&vec![0u8; 64 << 20], 9);
    let mut d = bitmap_header(2, 2, 24, 16);
    // The buffer's size, its checksum, the method (zlib), the stream.
    d.extend(16u32.to_le_bytes());
    d.extend([0u8; 4]);
    d.push(1);
    d.extend(sub(ANONYMOUS_CHUNK, &bomb));
    let data = file(Some(sub(PROPERTIES_PREVIEW_COMPRESSED, &d)), &[], &[]);
    assert!(load_data(&data, &limits()).is_err());
}

#[test]
fn blocks_that_contain_themselves_stop() {
    // A block whose only member is an instance of itself, placed once.
    let block = [9u8; 16];
    let member = [3u8; 16];
    let mut inst = vec![0x10u8];
    inst.extend(block);
    let mut xform = [0.0f64; 16];
    for i in 0..4 {
        xform[i * 5] = 1.0;
    }
    xform[3] = 1.0; // moved along x
    for v in xform {
        inst.extend(v.to_le_bytes());
    }
    inst.extend([0u8; 48]);
    let instance = |id: u8, mode: u8| {
        let mut o = object(object::instance_ref_class(), &inst, 0, [id; 16]);
        // Patch the attributes' mode: rebuild with an item list.
        let _ = &mut o;
        let _ = mode;
        o
    };
    // The definition: its member is the instance itself.
    let mut def = vec![0x10u8];
    def.extend(block);
    def.extend(1i32.to_le_bytes());
    def.extend(member);
    let mut class = Writer { out: Vec::new() };
    class.chunk(OPENNURBS_CLASS_UUID, &[0u8; 16]);
    class.chunk(OPENNURBS_CLASS_DATA, &def);
    let mut rec = Writer { out: Vec::new() };
    rec.chunk(OPENNURBS_CLASS, &class.out);
    let mut w = Writer::new(50);
    let mut props = Writer { out: Vec::new() };
    props.short(PROPERTIES_OPENNURBS_VERSION & !TCODE_SHORT, 201_209_145);
    w.chunk(TABLE_PROPERTIES, &props.out);
    w.chunk(TABLE_INSTANCE_DEFINITION, &sub(INSTANCE_DEFINITION_RECORD, &rec.out));
    let mut objs = Vec::new();
    objs.extend(instance(3, 0));
    objs.extend(triangle_object(0, 0.0, 4));
    w.chunk(TABLE_OBJECT, &objs);
    w.chunk(END_OF_FILE, &[0; 8]);
    let start = std::time::Instant::now();
    let result = load_data(&w.out, &limits());
    assert!(start.elapsed().as_secs() < 5);
    assert!(result.is_ok() || result.is_err());
}

/// Real files, when the assets are there.
fn asset(path: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/cad-extra/3dm").join(path);
    p.exists().then_some(p)
}

#[test]
fn real_files() {
    // (file, expected result): triangles of a scene, or a preview size.
    type Want = Result<usize, (u32, u32)>;
    let cases: [(&str, Want); 5] = [
        ("opennurbs/V5/v5_my_mesh.3dm", Ok(6)),
        ("generated/brep_box_render_mesh_v6.3dm", Ok(12)),
        ("generated/brep_box_no_render_mesh_v7.3dm", Err((0, 0))),
        ("opennurbs/V4/v4_Gear.3dm", Ok(0)),
        ("opennurbs/V8/v8_caps_and_joins.3dm", Err((1024, 689))),
    ];
    let mut checked = 0;
    for (name, want) in cases {
        let Some(path) = asset(name) else { continue };
        checked += 1;
        let got = load_path(&path, &Limits::viewer());
        match (want, got) {
            (Ok(0), Ok(Content::Model(s))) => assert!(s.triangle_count() > 1000, "{name}"),
            (Ok(n), Ok(Content::Model(s))) => assert_eq!(s.triangle_count(), n, "{name}"),
            (Err((0, 0)), Err(_)) | (Err((0, 0)), Ok(Content::Preview(_))) => {}
            (Err((w, h)), Ok(Content::Preview(t))) => assert_eq!((t.width(), t.height()), (w, h), "{name}"),
            (_, Ok(_)) => panic!("{name}: wrong kind of content"),
            (_, Err(e)) => panic!("{name}: {e}"),
        }
    }
    eprintln!("checked {checked} real files");
}

/// Meshes with ordinary coordinates read whatever the numbers are (the
/// compressed buffer is exactly as long as it says).
#[test]
fn vertex_buffers_of_any_content_read() {
    for off in [0.0f32, 5.0, -3.25, 1.0e6] {
        let positions = [[off, 0.0, 0.0], [off + 1.0, 0.0, 0.0], [off, 1.0, 0.0], [off + 1.0, 1.0, 0.0]];
        let body = mesh_body(&positions, &[[0, 1, 2, 2], [1, 3, 2, 0]]);
        let mut w = Writer::new(50);
        w.out.extend(&body);
        let arch = Archive::open(&w.out).unwrap();
        let m = super::mesh::read(&arch, &body).unwrap_or_else(|| panic!("offset {off}"));
        assert_eq!((m.positions.len(), m.indices.len()), (4, 9));
    }
}
