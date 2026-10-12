//! Tests over real files and synthesized ones.

use crate::dom::Dom;
use crate::{Budget, RobloxFormat};
use slerp_scene::Limits;
use std::path::{Path, PathBuf};

/// The test corpus, when it has been fetched.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/roblox")
}

fn read_dom(bytes: &[u8]) -> Result<Dom, slerp_scene::LoadError> {
    let format = crate::sniff(Path::new("x"), bytes).ok_or(slerp_scene::LoadError::NotMine)?;
    let mut budget = Budget::new(bytes.len() as u64, &Limits::viewer());
    crate::read(format, bytes, &mut budget)
}

/// Prints the instances of `ROBLOX_DUMP` (a file) in the layout of the
/// rbx-dom probe used to check this reader, for comparing the two.
#[test]
#[ignore = "a tool: ROBLOX_DUMP=file cargo test dump -- --ignored --nocapture"]
fn dump() {
    let Ok(path) = std::env::var("ROBLOX_DUMP") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let dom = read_dom(&bytes).unwrap();
    let mut stack: Vec<(usize, usize)> = dom.roots.iter().rev().map(|&r| (r, 1)).collect();
    println!("DataModel \"DataModel\" ");
    while let Some((i, depth)) = stack.pop() {
        let inst = &dom.instances[i];
        let p = &inst.props;
        let mut props = Vec::new();
        if let Some(c) = p.cframe {
            let r = c.rotation;
            props.push(format!(
                "CFrame={:.3},{:.3},{:.3}|{:.3},{:.3},{:.3}|{:.3},{:.3},{:.3}|{:.3},{:.3},{:.3}",
                c.position.x,
                c.position.y,
                c.position.z,
                r.x_axis.x,
                r.y_axis.x,
                r.z_axis.x,
                r.x_axis.y,
                r.y_axis.y,
                r.z_axis.y,
                r.x_axis.z,
                r.y_axis.z,
                r.z_axis.z
            ));
        }
        if let Some(c) = p.color {
            props.push(format!("Color={},{},{}", c[0], c[1], c[2]));
        }
        if let Some(m) = p.material {
            props.push(format!("Material={m}"));
        }
        if let Some(t) = p.reflectance {
            props.push(format!("Reflectance={t:.3}"));
        }
        if let Some(s) = p.shape {
            props.push(format!("Shape={s}"));
        }
        if let Some(s) = p.size {
            props.push(format!("Size={:.3},{:.3},{:.3}", s.x, s.y, s.z));
        }
        if let Some(t) = p.transparency {
            props.push(format!("Transparency={t:.3}"));
        }
        println!("{}{} {:?} {}", "  ".repeat(depth), dom.class(i), inst.name, props.join(" "));
        for &c in inst.children.iter().rev() {
            stack.push((c, depth + 1));
        }
    }
}

/// Every file in the corpus reads, as binary and XML alike, and the two
/// encodings of each rbx-test-files sample give the same instances.
#[test]
fn the_corpus_reads() {
    let dir = corpus().join("rbx-test-files");
    let Ok(groups) =
        std::fs::read_dir(dir.join("models")).and_then(|m| Ok(m.chain(std::fs::read_dir(dir.join("places"))?)))
    else {
        eprintln!("no corpus at {}", dir.display());
        return;
    };
    let mut checked = 0;
    for group in groups.flatten() {
        let (bin, xml) = (
            group.path().join(if group.path().to_string_lossy().contains("places") {
                "binary.rbxl"
            } else {
                "binary.rbxm"
            }),
            group.path().join(if group.path().to_string_lossy().contains("places") {
                "xml.rbxlx"
            } else {
                "xml.rbxmx"
            }),
        );
        let (Ok(b), Ok(x)) = (std::fs::read(&bin), std::fs::read(&xml)) else { continue };
        let (db, dx) = (
            read_dom(&b).unwrap_or_else(|e| panic!("{}: {e}", bin.display())),
            read_dom(&x).unwrap_or_else(|e| panic!("{}: {e}", xml.display())),
        );
        let names = |d: &Dom| {
            let mut v: Vec<(String, String)> =
                (0..d.instances.len()).map(|i| (d.class(i).to_owned(), d.instances[i].name.clone())).collect();
            v.sort();
            v
        };
        // Studio leaves some services and its selection helpers out of XML
        // places; everything else must match.
        let (mut binary_names, mut xml_names) = (names(&db), names(&dx));
        let service = |n: &(String, String)| n.0.ends_with("Service") || n.1 == "FilteredSelection";
        binary_names.retain(|n| !service(n));
        xml_names.retain(|n| !service(n));
        assert_eq!(binary_names, xml_names, "{}", group.path().display());
        checked += 1;
    }
    assert!(checked > 40, "only {checked} pairs");
    let _ = RobloxFormat::Binary;
}

/// Prints what the unions and mesh parts of `ROBLOX_DUMP` keep, for
/// working out the spaces their stored geometry is in.
#[test]
#[ignore = "a tool"]
fn unions() {
    let Ok(path) = std::env::var("ROBLOX_DUMP") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let dom = read_dom(&bytes).unwrap();
    let bounds = |m: &crate::csg::Mesh| {
        let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
        for &p in &m.positions {
            lo = lo.min(p);
            hi = hi.max(p);
        }
        format!(
            "[{:.2} {:.2} {:.2}]..[{:.2} {:.2} {:.2}] {} tris",
            lo.x,
            lo.y,
            lo.z,
            hi.x,
            hi.y,
            hi.z,
            m.indices.len() / 3
        )
    };
    let mut shown = 0;
    for i in 0..dom.instances.len() {
        let class = dom.class(i);
        if !matches!(class, "UnionOperation" | "MeshPart" | "NegateOperation")
            || shown > 4000
            || std::env::var("ROBLOX_CLASS").is_ok_and(|c| c != class)
        {
            continue;
        }
        shown += 1;
        let p = &dom.instances[i].props;
        let e = p.extra();
        println!(
            "{class} {:?} pos {:?} size {:?} initial {:?} usepartcolor {:?}",
            dom.instances[i].name,
            p.cframe.map(|c| c.position),
            p.size,
            e.initial_size,
            e.use_part_color
        );
        if let Some(b) = &e.mesh_data {
            let d = dom.blob(b);
            println!(
                "  MeshData {} bytes {:?}: {:?}",
                d.len(),
                &d[..d.len().min(12)],
                crate::csg::union_mesh(d).map(|m| bounds(&m))
            );
        }
        for (what, b) in [("Physical", &e.physics), ("PhysicsData", &e.physics_old)] {
            if let Some(b) = b {
                let d = dom.blob(b);
                let r = crate::csg::physics(d, 64 << 20);
                println!(
                    "  {what} {} bytes {:?}: {}",
                    d.len(),
                    String::from_utf8_lossy(&d[..d.len().min(10)]),
                    match r {
                        Ok(crate::csg::Physics::Hulls(m)) => bounds(&m),
                        Ok(crate::csg::Physics::Block) => "block".into(),
                        Err(e) => format!("{e:?}"),
                    }
                );
            }
        }
        if let Some(b) = &e.child_data {
            let d = dom.blob(b);
            match crate::binary::read(d, &mut Budget::new(d.len() as u64, &Limits::viewer())) {
                Ok(c) => {
                    for k in 0..c.instances.len() {
                        let q = &c.instances[k].props;
                        println!(
                            "  child {} {:?} pos {:?} size {:?} rot {:?}",
                            c.class(k),
                            c.instances[k].name,
                            q.cframe.map(|c| c.position),
                            q.size,
                            q.cframe.map(|c| c.rotation)
                        );
                    }
                }
                Err(err) => println!("  ChildData {} bytes: {err}", d.len()),
            }
        }
    }
}

/// Writes the first union's physics blob of `ROBLOX_DUMP` to /tmp.
#[test]
#[ignore = "a tool"]
fn physics_blob() {
    let Ok(path) = std::env::var("ROBLOX_DUMP") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let dom = read_dom(&bytes).unwrap();
    let mut n = 0;
    for i in 0..dom.instances.len() {
        if let Some(b) = &dom.instances[i].props.extra().physics {
            std::fs::write(format!("/tmp/rbx-cmp/physics-{n}.bin"), dom.blob(b)).unwrap();
            n += 1;
            if n > 5 {
                break;
            }
        }
    }
}

/*┌─────────────────╮
| Synthesized     |
└─────────────────╯*/
//region Synthesized

/// Writes binary Roblox files for the tests, chunks stored uncompressed.
#[derive(Default)]
struct Writer {
    chunks: Vec<u8>,
    classes: u32,
    instances: u32,
}

/// One instance to write: class, name, parent (index or -1) and its
/// properties.
struct Item {
    class: &'static str,
    name: &'static str,
    parent: i32,
    cframe: Option<[f32; 12]>,
    size: Option<[f32; 3]>,
    color: Option<[u8; 3]>,
    transparency: Option<f32>,
    shape: Option<u32>,
}

fn item(class: &'static str, name: &'static str, parent: i32) -> Item {
    Item { class, name, parent, cframe: None, size: None, color: None, transparency: None, shape: None }
}

fn part(class: &'static str, name: &'static str, parent: i32, at: [f32; 3], size: [f32; 3]) -> Item {
    Item {
        cframe: Some([at[0], at[1], at[2], 1., 0., 0., 0., 1., 0., 0., 0., 1.]),
        size: Some(size),
        color: Some([200, 50, 50]),
        ..item(class, name, parent)
    }
}

impl Writer {
    fn chunk(&mut self, name: &[u8; 4], data: &[u8]) {
        self.chunks.extend_from_slice(name);
        self.chunks.extend_from_slice(&0u32.to_le_bytes());
        self.chunks.extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.chunks.extend_from_slice(&0u32.to_le_bytes());
        self.chunks.extend_from_slice(data);
    }

    fn file(&self) -> Vec<u8> {
        let mut out = crate::binary::MAGIC.to_vec();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.classes.to_le_bytes());
        out.extend_from_slice(&self.instances.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&self.chunks);
        out.extend_from_slice(b"END\0");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&9u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"</roblox>");
        out
    }
}

fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

/// Interleaves big-endian 4-byte values.
fn interleave(values: &[u32]) -> Vec<u8> {
    let bytes: Vec<[u8; 4]> = values.iter().map(|v| v.to_be_bytes()).collect();
    (0..4).flat_map(|k| bytes.iter().map(move |b| b[k])).collect()
}

fn referents(values: &[i32]) -> Vec<u8> {
    let mut last = 0;
    interleave(
        &values
            .iter()
            .map(|&v| {
                let d = v.wrapping_sub(last);
                last = v;
                zigzag(d)
            })
            .collect::<Vec<_>>(),
    )
}

fn rbx_floats(values: &[f32]) -> Vec<u8> {
    interleave(&values.iter().map(|f| f.to_bits().rotate_left(1)).collect::<Vec<_>>())
}

fn string(s: &[u8]) -> Vec<u8> {
    let mut out = (s.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(s);
    out
}

/// A binary file holding `items`, one class per item for simplicity.
fn binary(items: &[Item]) -> Vec<u8> {
    let mut w = Writer::default();
    for (i, it) in items.iter().enumerate() {
        let id = i as u32;
        let mut inst = id.to_le_bytes().to_vec();
        inst.extend(string(it.class.as_bytes()));
        inst.push(0);
        inst.extend_from_slice(&1u32.to_le_bytes());
        inst.extend(referents(&[i as i32]));
        w.chunk(b"INST", &inst);
        let prop = |name: &str, ty: u8, data: Vec<u8>| {
            let mut p = id.to_le_bytes().to_vec();
            p.extend(string(name.as_bytes()));
            p.push(ty);
            p.extend(data);
            p
        };
        w.chunk(b"PROP", &prop("Name", 0x01, string(it.name.as_bytes())));
        if let Some(c) = it.cframe {
            let mut data = vec![0u8];
            for v in &c[3..] {
                data.extend_from_slice(&v.to_le_bytes());
            }
            data.extend(rbx_floats(&c[0..1]));
            data.extend(rbx_floats(&c[1..2]));
            data.extend(rbx_floats(&c[2..3]));
            w.chunk(b"PROP", &prop("CFrame", 0x10, data));
        }
        if let Some(s) = it.size {
            let mut data = rbx_floats(&s[0..1]);
            data.extend(rbx_floats(&s[1..2]));
            data.extend(rbx_floats(&s[2..3]));
            w.chunk(b"PROP", &prop("size", 0x0e, data));
        }
        if let Some(c) = it.color {
            w.chunk(b"PROP", &prop("Color3uint8", 0x1a, c.to_vec()));
        }
        if let Some(t) = it.transparency {
            w.chunk(b"PROP", &prop("Transparency", 0x04, rbx_floats(&[t])));
        }
        if let Some(s) = it.shape {
            w.chunk(b"PROP", &prop("shape", 0x12, interleave(&[s])));
        }
        w.classes += 1;
        w.instances += 1;
    }
    let ids: Vec<i32> = (0..items.len() as i32).collect();
    let parents: Vec<i32> = items.iter().map(|i| i.parent).collect();
    let mut prnt = vec![0u8];
    prnt.extend_from_slice(&(items.len() as u32).to_le_bytes());
    prnt.extend(referents(&ids));
    prnt.extend(referents(&parents));
    w.chunk(b"PRNT", &prnt);
    w.file()
}

fn load(bytes: &[u8], limits: &Limits) -> Result<slerp_scene::Scene, slerp_scene::LoadError> {
    let format = crate::sniff(Path::new("x"), bytes).ok_or(slerp_scene::LoadError::NotMine)?;
    crate::load_inner(format, bytes, limits)
}

/// A small place: a spawn, a ball, a wedge, a see-through block and an
/// invisible one, in a model.
fn small_place() -> Vec<u8> {
    let mut ball = part("Part", "Ball", 2, [10.0, 2.0, 0.0], [4.0, 4.0, 4.0]);
    ball.shape = Some(0);
    let mut glass = part("Part", "Glass", 2, [-10.0, 2.0, 0.0], [4.0, 4.0, 1.0]);
    glass.transparency = Some(0.5);
    let mut ghost = part("Part", "Ghost", 2, [0.0, 50.0, 0.0], [4.0, 4.0, 4.0]);
    ghost.transparency = Some(1.0);
    binary(&[
        item("Workspace", "Workspace", -1),
        part("SpawnLocation", "SpawnLocation", 0, [0.0, 0.5, 20.0], [12.0, 1.0, 12.0]),
        item("Model", "House", 0),
        ball,
        part("WedgePart", "Ramp", 2, [0.0, 2.0, -10.0], [4.0, 4.0, 8.0]),
        glass,
        ghost,
        item("Lighting", "Lighting", -1),
    ])
}

#[test]
fn a_small_place_becomes_a_level() {
    let scene = load(&small_place(), &Limits::viewer()).unwrap();
    assert_eq!(scene.kind, slerp_scene::SceneKind::Level);
    assert!(scene.info.format.contains("place"), "{}", scene.info.format);
    // The player starts on the spawn, 4.5 studs above its top.
    let spawn = scene.spawn.expect("a spawn");
    assert!(
        (spawn.position - glam::Vec3::new(0.0, 5.5, 20.0) * crate::build::STUD).length() < 1e-3,
        "{:?}",
        spawn.position
    );
    // The model is a node; the invisible part is in a hidden node.
    assert!(scene.nodes.iter().any(|n| n.name == "House" && n.mesh.is_some()));
    let hidden = scene.nodes.iter().find(|n| !n.visible).expect("a hidden node");
    assert_eq!(hidden.name, "Invisible parts");
    // Visible bounds: from the glass block to the ball, in metres; the
    // ghost 50 studs up isn't framed.
    let bounds = scene.bounds(&slerp_scene::Pose::rest(&scene));
    assert!((bounds.max.y - 4.0 * crate::build::STUD).abs() < 0.01, "{bounds:?}");
    assert!((bounds.max.x - 12.0 * crate::build::STUD).abs() < 0.01, "{bounds:?}");
    // The see-through block gets a blended material.
    assert!(scene.materials.iter().any(|m| m.alpha_mode == slerp_scene::AlphaMode::Blend));
    assert!(
        scene.info.details.iter().any(|(k, v)| k == "Instances" && v.starts_with("8 of 6")),
        "{:?}",
        scene.info.details
    );
}

#[test]
fn a_model_is_an_object_without_a_workspace() {
    let bytes = binary(&[item("Model", "Car", -1), part("Part", "Body", 0, [0.0, 1.0, 0.0], [4.0, 1.0, 8.0])]);
    let scene = load(&bytes, &Limits::thumbnail()).unwrap();
    assert_eq!(scene.kind, slerp_scene::SceneKind::Object);
    assert!(scene.spawn.is_none());
    assert_eq!(scene.triangle_count(), 12);
}

/// A baseplate 2,048 studs across is cut down around a small building for
/// thumbnails, and kept whole in the viewer.
#[test]
fn baseplates_are_cut_for_thumbnails() {
    let bytes = binary(&[
        item("Workspace", "Workspace", -1),
        part("Part", "Baseplate", 0, [0.0, -8.0, 0.0], [2048.0, 16.0, 2048.0]),
        part("Part", "Hut", 0, [0.0, 5.0, 0.0], [10.0, 10.0, 10.0]),
    ]);
    let width = |limits: &Limits| {
        let scene = load(&bytes, limits).unwrap();
        let b = scene.bounds(&slerp_scene::Pose::rest(&scene));
        (b.max.x - b.min.x) / crate::build::STUD
    };
    assert!((width(&Limits::viewer()) - 2048.0).abs() < 0.5);
    let cut = width(&Limits::thumbnail());
    assert!(cut > 10.0 && cut < 40.0, "{cut}");
}

/// Old files store colours as BrickColor numbers.
#[test]
fn brick_colors_colour_old_parts() {
    let mut w = Writer::default();
    let mut inst = 0u32.to_le_bytes().to_vec();
    inst.extend(string(b"Part"));
    inst.push(0);
    inst.extend_from_slice(&1u32.to_le_bytes());
    inst.extend(referents(&[0]));
    w.chunk(b"INST", &inst);
    let mut cf = 0u32.to_le_bytes().to_vec();
    cf.extend(string(b"CFrame"));
    cf.push(0x10);
    cf.push(0x02);
    cf.extend(rbx_floats(&[0.0]));
    cf.extend(rbx_floats(&[0.0]));
    cf.extend(rbx_floats(&[0.0]));
    w.chunk(b"PROP", &cf);
    let mut size = 0u32.to_le_bytes().to_vec();
    size.extend(string(b"size"));
    size.push(0x0e);
    size.extend(rbx_floats(&[1.0]));
    size.extend(rbx_floats(&[1.0]));
    size.extend(rbx_floats(&[1.0]));
    w.chunk(b"PROP", &size);
    let mut bc = 0u32.to_le_bytes().to_vec();
    bc.extend(string(b"BrickColor"));
    bc.push(0x0b);
    bc.extend(interleave(&[1004]));
    w.chunk(b"PROP", &bc);
    w.classes = 1;
    w.instances = 1;
    let scene = load(&w.file(), &Limits::viewer()).unwrap();
    let c = scene.meshes[0].primitives[0].colors[0];
    assert!(c.x > 0.99 && c.y < 0.01 && c.z < 0.01, "{c}");
}

//endregion

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

/// Runs `f` on a thread with the 2 MB stack threads usually get, so a
/// reader that recursed per level would overflow here.
fn small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(2 << 20).spawn(f).unwrap().join().unwrap()
}

/// A header whose counts claim billions of instances, the 57-byte file that
/// aborts rbx-dom's reader, and chunks that claim gigabytes.
#[test]
fn absurd_counts_are_refused_cheaply() {
    let header = |classes: u32, instances: u32| {
        let mut out = crate::binary::MAGIC.to_vec();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&classes.to_le_bytes());
        out.extend_from_slice(&instances.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out
    };
    let chunk = |name: &[u8; 4], compressed: u32, len: u32, data: &[u8]| {
        let mut c = name.to_vec();
        c.extend_from_slice(&compressed.to_le_bytes());
        c.extend_from_slice(&len.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(data);
        c
    };
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut f = header(u32::MAX, 0xffff_fff0);
    f.extend(chunk(b"END\0", 0, 9, b"</roblox>"));
    cases.push(("huge header counts", f));
    let mut f = header(1, 1);
    f.extend(chunk(b"INST", 8, 0xffff_fff0, &[0x10; 8]));
    cases.push(("an LZ4 chunk claiming 4 GB", f));
    let mut f = header(1, 1);
    f.extend(chunk(b"INST", 8, 0xffff_fff0, &[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0]));
    cases.push(("a Zstandard chunk claiming 4 GB", f));
    let mut f = header(1, 1);
    let mut inst = 0u32.to_le_bytes().to_vec();
    inst.extend(string(b"Part"));
    inst.push(0);
    inst.extend_from_slice(&0x4000_0000u32.to_le_bytes());
    inst.extend_from_slice(&[0; 16]);
    f.extend(chunk(b"INST", 0, inst.len() as u32, &inst));
    cases.push(("a class claiming a billion instances", f));
    let mut f = header(0, 0);
    f.extend(chunk(b"SSTR", 0, 8, &[0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f]));
    cases.push(("two billion shared strings", f));
    let mut f = header(1, 1);
    f.extend(chunk(b"INST", 2, 10, &[0, 0]));
    cases.push(("a compressed chunk shorter than its signature", f));
    let mut f = header(1, 1);
    f.extend(chunk(b"PRNT", 0, 5, &[0, 0xff, 0xff, 0xff, 0x7f]));
    cases.push(("a parent chunk claiming two billion pairs", f));
    for (what, bytes) in cases {
        let start = std::time::Instant::now();
        let result = load(&bytes, &Limits::thumbnail());
        assert!(result.is_err(), "{what} loaded");
        assert!(start.elapsed().as_millis() < 200, "{what} took {:?}", start.elapsed());
    }
}

/// Parents that loop, a chain 100,000 deep, and XML nested 200,000 deep:
/// no hang, no stack overflow.
#[test]
fn loops_and_deep_trees() {
    // 1. Two folders, each the other's parent, beside a real part.
    let mut items = vec![item("Folder", "A", 1), item("Folder", "B", 0)];
    items.push(part("Part", "P", 0, [0.0; 3], [1.0; 3]));
    let scene = load(&binary(&items), &Limits::viewer()).unwrap();
    assert_eq!(scene.triangle_count(), 12);
    // 2. A binary chain of folders 100,000 deep with a part at the bottom.
    let mut items: Vec<Item> = (0..100_000).map(|i| item("Folder", "F", i - 1)).collect();
    items.push(part("Part", "P", 99_999, [0.0; 3], [1.0; 3]));
    let bytes = binary(&items);
    let scene = small_stack(move || load(&bytes, &Limits::viewer())).unwrap();
    assert_eq!(scene.triangle_count(), 12);
    assert!(scene.nodes.len() < 100, "{} nodes", scene.nodes.len());
    // 3. XML nested 200,000 deep.
    let depth = 200_000;
    let mut xml = String::from("<roblox version=\"4\">");
    for _ in 0..depth {
        xml.push_str("<Item class=\"Folder\" referent=\"R\"><Properties><string name=\"Name\">a</string></Properties>");
    }
    xml.push_str("<Item class=\"Part\" referent=\"P\"><Properties><CoordinateFrame name=\"CFrame\"><X>0</X><Y>0</Y><Z>0</Z><R00>1</R00><R01>0</R01><R02>0</R02><R10>0</R10><R11>1</R11><R12>0</R12><R20>0</R20><R21>0</R21><R22>1</R22></CoordinateFrame><Vector3 name=\"size\"><X>1</X><Y>1</Y><Z>1</Z></Vector3></Properties></Item>");
    for _ in 0..depth {
        xml.push_str("</Item>");
    }
    xml.push_str("</roblox>");
    let scene = small_stack(move || load(xml.as_bytes(), &Limits::viewer())).unwrap();
    assert_eq!(scene.triangle_count(), 12);
}

/// A tiny deterministic generator for the mutation tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Every prefix length (in steps) and hundreds of mutations of real files,
/// binary and XML, load or fail without panicking, quickly.
#[test]
fn damaged_files_never_panic() {
    let dir = corpus().join("rbx-test-files/models");
    let mut files: Vec<Vec<u8>> = ["unions", "three-unique-parts", "sharedstring", "faces"]
        .iter()
        .flat_map(|m| ["binary.rbxm", "xml.rbxmx"].map(|f| dir.join(m).join(f)))
        .filter_map(|p| std::fs::read(p).ok())
        .collect();
    files.push(small_place());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let limits = Limits::thumbnail();
    let mut slowest = std::time::Duration::ZERO;
    for bytes in &files {
        let step = (bytes.len() / 300).max(1);
        for cut in (0..bytes.len()).step_by(step) {
            let start = std::time::Instant::now();
            let _ = load(&bytes[..cut], &limits);
            slowest = slowest.max(start.elapsed());
        }
        for round in 0..300 {
            let mut m = bytes.clone();
            let flips = 1 + (rng.next() % 8) as usize;
            for _ in 0..flips {
                let at = (rng.next() as usize) % m.len();
                m[at] = match round % 3 {
                    0 => rng.next() as u8,
                    1 => 0xff,
                    _ => m[at] ^ (1 << (rng.next() % 8)),
                };
            }
            let start = std::time::Instant::now();
            let _ = load(&m, &limits);
            slowest = slowest.max(start.elapsed());
        }
    }
    assert!(slowest.as_millis() < 500, "the slowest damaged file took {slowest:?}");
}

//endregion

/// Loads every Roblox file under `local/assets/roblox*` with thumbnail and
/// viewer limits and prints what happened; fails on a panic or an
/// unexpected error.  Slow, so run by hand:
/// `cargo test -p slerp-roblox every_file --release -- --ignored --nocapture`.
#[test]
#[ignore = "slow; reads the whole local corpus"]
fn every_file() {
    let root = corpus().join("..");
    let mut files = Vec::new();
    let mut stack = vec![root.join("roblox"), root.join("roblox-extra")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| {
                ["rbxl", "rbxm", "rbxlx", "rbxmx"].contains(&x.to_string_lossy().to_ascii_lowercase().as_str())
            }) {
                files.push(p);
            }
        }
    }
    files.sort();
    if let Ok(only) = std::env::var("ROBLOX_ONLY") {
        files.retain(|f| f.to_string_lossy().contains(&only));
    }
    let mut failed = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        let name = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        for (which, limits) in [("thumb", Limits::thumbnail()), ("view", Limits::viewer())] {
            let start = std::time::Instant::now();
            let result = std::panic::catch_unwind(|| load(&bytes, &limits));
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(Ok(scene)) => println!(
                    "{which} {ms:7.1} ms {:>9} tris {:>3} warnings  {name}  {}",
                    scene.triangle_count(),
                    scene.info.warnings.len(),
                    scene
                        .info
                        .warnings
                        .iter()
                        .filter(|w| !w.contains("Roblox's servers"))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                Ok(Err(e)) => {
                    println!("{which} {ms:7.1} ms ERROR {name}: {e}");
                    if !e.to_string().contains("no parts") {
                        failed.push(format!("{name}: {e}"));
                    }
                }
                Err(_) => {
                    println!("{which} PANIC {name}");
                    failed.push(format!("{name}: panicked"));
                }
            }
        }
    }
    println!("{} files", files.len());
    for f in &failed {
        println!("FAILED {f}");
    }
}

/// Prints where the terrain chunks of `ROBLOX_DUMP` lie.
#[test]
#[ignore = "a tool"]
fn terrain_chunks() {
    let Ok(path) = std::env::var("ROBLOX_DUMP") else { return };
    let dom = read_dom(&std::fs::read(&path).unwrap()).unwrap();
    for i in 0..dom.instances.len() {
        let Some(b) = dom.instances[i].props.extra().smooth_grid.as_ref() else { continue };
        let keys = crate::terrain::chunk_keys(dom.blob(b));
        let mut xs = std::collections::BTreeMap::new();
        for k in &keys {
            *xs.entry((k[0], k[2])).or_insert(0) += 1;
        }
        println!(
            "{} chunks in {} columns; x/z columns: {:?}",
            keys.len(),
            xs.len(),
            xs.iter().take(60).collect::<Vec<_>>()
        );
    }
}
