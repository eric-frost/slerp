//! A text dump of a file's structure, for developing and debugging the
//! reader (`cargo run -p slerp-blend --example blend-dump`).

use crate::file::BlendFile;
use crate::sdna::Prim;
use crate::view::Obj;
use std::fmt::Write;

/// Describes `f` according to `what`:
///
/// - `summary`: the header, block counts by code, and the scenes.
/// - `struct NAME`: the fields of structure NAME with offsets and types.
/// - `ids XX`: the data-blocks with ID code XX ("OB", "ME", "MA", ...).
/// - `id NAME [DEPTH]`: the data-block called NAME (with its type prefix,
///   such as "OBCube"), its fields printed and pointers followed to DEPTH.
pub fn dump(f: &BlendFile, what: &str) -> String {
    let mut s = String::new();
    let mut words = what.split_whitespace();
    match words.next() {
        Some("struct") => {
            let name = words.next().unwrap_or("");
            let Some(si) = f.sdna.struct_named(name) else { return format!("no struct {name}\n") };
            let st = f.sdna.get(si).unwrap();
            let _ = writeln!(s, "{name}: {} bytes", st.size);
            for fl in &st.fields {
                let t = &f.sdna.types[fl.ty as usize];
                let _ = writeln!(
                    s,
                    "  {:5} {}{} {}{}",
                    fl.offset,
                    t.name,
                    if fl.pointer { "*" } else { "" },
                    fl.name,
                    if fl.elems > 1 { format!("[{}]", fl.elems) } else { String::new() }
                );
            }
        }
        Some("ids") => {
            let code = words.next().unwrap_or("OB").as_bytes();
            for (i, b) in f.blocks.iter().enumerate() {
                if b.is_id() && b.code[..2] == code[..2.min(code.len())] {
                    let o = Obj::of_block(f, i as u32).unwrap();
                    let _ = writeln!(s, "{} {} ({}, {} bytes)", i, o.id_name(), o.type_name(), b.len);
                }
            }
        }
        Some("modifiers") => {
            for (i, b) in f.blocks.iter().enumerate() {
                if !(b.is_id() && b.id_code() == *b"OB") {
                    continue;
                }
                let Some(o) = Obj::of_block(f, i as u32) else { continue };
                let mods: Vec<String> = o
                    .list("modifiers")
                    .iter()
                    .map(|m| {
                        let c = m.sub("modifier").unwrap_or(*m);
                        format!("{}({} mode {})", m.type_name(), c.string("name"), c.int(&["mode"]).unwrap_or(0))
                    })
                    .collect();
                if !mods.is_empty() {
                    let _ = writeln!(s, "{}: {}", o.id_name(), mods.join(", "));
                }
            }
        }
        Some("nodes") => {
            // The node tree of a material ("MAName") or node group ("NTName").
            let name = words.collect::<Vec<_>>().join(" ");
            let name = name.as_str();
            for (i, b) in f.blocks.iter().enumerate() {
                let Some(o) = Obj::of_block(f, i as u32).filter(|_| b.is_id()) else { continue };
                if o.sub("id").map(|id| id.string("name")).as_deref() != Some(name) {
                    continue;
                }
                let tree = if o.type_name() == "bNodeTree" { Some(o) } else { o.deref("nodetree") };
                let Some(tree) = tree else { continue };
                let g = crate::material::Graph::read(&tree);
                for (k, n) in g.nodes.iter().enumerate() {
                    let _ = writeln!(s, "[{k}] {} \"{}\"{}", n.idname, n.name, if n.muted { " (muted)" } else { "" });
                    for (j, sock) in n.inputs.iter().enumerate() {
                        let src = g
                            .source(k, j)
                            .map(|(fnode, fs)| format!(" <- [{fnode}].{}", g.nodes[fnode].outputs[fs].name));
                        let _ = writeln!(
                            s,
                            "    in {:?} ({}) = {:?}{}{}",
                            sock.name,
                            sock.identifier,
                            sock.value,
                            if sock.hidden { " hidden" } else { "" },
                            src.unwrap_or_default()
                        );
                    }
                    if let Some(img) = n.obj.deref("id") {
                        let _ = writeln!(s, "    id: {} {}", img.type_name(), img.id_name());
                    }
                }
            }
        }
        Some("attrs") => {
            // A mesh's Blender 5 attribute storage.
            let name = words.collect::<Vec<_>>().join(" ");
            for (i, b) in f.blocks.iter().enumerate() {
                let Some(o) = Obj::of_block(f, i as u32).filter(|_| b.is_id()) else { continue };
                if o.sub("id").map(|id| id.string("name")) != Some(name.clone()) {
                    continue;
                }
                let Some(st) = o.sub("attribute_storage") else { continue };
                let n = st.int(&["dna_attributes_num"]).unwrap_or(0) as usize;
                let Some(first) = st.deref_as("dna_attributes", "Attribute") else { continue };
                for k in 0..n {
                    let Some(a) = first.nth(k) else { break };
                    let nm = crate::mesh::c_string(a.raw("name").unwrap_or(&[]));
                    let holder = a.follow(a.ptr("data"), None);
                    let _ = writeln!(
                        s,
                        "{nm:?} type {:?} domain {:?} storage {:?} holder {:?} size {:?}",
                        a.int(&["data_type"]),
                        a.int(&["domain"]),
                        a.int(&["storage_type"]),
                        holder.map(|h| h.type_name()),
                        holder.and_then(|h| h.int(&["size"]))
                    );
                }
            }
        }
        Some("id") => {
            let mut rest: Vec<&str> = words.collect();
            let depth: usize = match rest.last().and_then(|d| d.parse().ok()) {
                Some(d) => {
                    rest.pop();
                    d
                }
                None => 1,
            };
            let name = rest.join(" ");
            let name = name.as_str();
            for (i, b) in f.blocks.iter().enumerate() {
                if !b.is_id() {
                    continue;
                }
                let Some(o) = Obj::of_block(f, i as u32) else { continue };
                let full = o.sub("id").map(|id| id.string("name")).unwrap_or_default();
                if full == name {
                    print_obj(&mut s, o, 0, depth);
                }
            }
        }
        _ => {
            let h = f.header;
            let _ = writeln!(
                s,
                "version {} pointers {} {} format {} packing {:?}",
                h.version,
                h.pointer_size,
                if h.big_endian { "big-endian" } else { "little-endian" },
                h.format,
                f.packing
            );
            let mut counts = std::collections::BTreeMap::new();
            for b in &f.blocks {
                *counts.entry(String::from_utf8_lossy(&b.code).replace('\0', "")).or_insert(0usize) += 1;
            }
            let _ = writeln!(s, "{} blocks, {} structs: {:?}", f.blocks.len(), f.sdna.structs.len(), counts);
            for w in &f.warnings {
                let _ = writeln!(s, "warning: {w}");
            }
        }
    }
    s
}

fn print_obj(s: &mut String, o: Obj, indent: usize, depth: usize) {
    let pad = "  ".repeat(indent);
    let _ = writeln!(s, "{pad}{} @{} (block {})", o.type_name(), o.at, o.block);
    let Some(st) = o.f.sdna.get(o.s) else { return };
    for fl in &st.fields {
        let t = &o.f.sdna.types[fl.ty as usize];
        if fl.pointer {
            let p = o.ptr(&fl.name);
            if p == 0 {
                continue;
            }
            let target = o.f.find(p).map(|(i, off)| {
                let b = o.f.block(i);
                format!(
                    "-> block {i} {} {}+{off}",
                    String::from_utf8_lossy(&b.code).replace('\0', ""),
                    o.f.sdna.struct_name(b.sdna)
                )
            });
            let _ = writeln!(s, "{pad}  *{} {}: {:#x} {}", t.name, fl.name, p, target.unwrap_or_default());
            if depth > 0
                && t.prim == Prim::Other
                && !fl.name.starts_with("next")
                && !fl.name.starts_with("prev")
                && let Some(sub) = o.deref(&fl.name)
            {
                print_obj(s, sub, indent + 2, depth - 1);
            }
        } else if t.prim == Prim::Char && fl.elems > 1 {
            let v = o.string(&fl.name);
            if !v.is_empty() {
                let _ = writeln!(s, "{pad}  {} {}: {:?}", t.name, fl.name, v);
            }
        } else if t.prim != Prim::Other {
            let n = fl.elems.min(16) as usize;
            let vals: Vec<String> = (0..n).filter_map(|i| o.float_at(&fl.name, i)).map(|v| format!("{v}")).collect();
            if vals.iter().any(|v| v != "0") {
                let _ = writeln!(s, "{pad}  {} {}: {}", t.name, fl.name, vals.join(" "));
            }
        } else if let Some(sub) = o.sub(&fl.name) {
            if depth > 0 {
                let _ = writeln!(s, "{pad}  {}:", fl.name);
                print_obj(s, sub, indent + 2, depth - 1);
            } else {
                let _ = writeln!(s, "{pad}  {} {} (struct)", t.name, fl.name);
            }
        }
    }
}
