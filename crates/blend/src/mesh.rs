//! Reading a mesh data-block into a [`PolyMesh`], whichever way the saving
//! Blender stored it:
//!
//! - Blender 2.4 to 2.62: `MFace` triangles and quads (with `MTFace` UVs
//!   and `MCol` colours on the same faces).
//! - 2.63 to 3.4: `MVert`, `MEdge`, `MPoly` and `MLoop` structures, UVs in
//!   `MLoopUV` layers, colours in `MLoopCol` layers.
//! - 3.5 to 4.5: generic attributes in `CustomData` layers ("position",
//!   ".corner_vert", "material_index", "sharp_face", UV maps as 2D float
//!   corner attributes) with face offsets in `poly_offset_indices`.
//! - 5.0 and later: the same attributes in the mesh's `AttributeStorage`.
//!
//! Every array is read from the bytes actually present, so a count in the
//! file can't make it allocate more than the file holds.

use crate::file::{BlendFile, Endian};
use crate::poly::{Layer, PolyMesh, edge_key};
use crate::view::Obj;
use glam::{Vec2, Vec3, Vec4};

/// What a mesh data-block gave, besides the polygons.
pub struct MeshData {
    pub poly: PolyMesh,
    /// The mesh's material slots, as old addresses of Material data-blocks.
    pub materials: Vec<u64>,
    /// Custom normals still in Blender's encoding (two angles per corner,
    /// relative to the corner's smooth fan); `normals.rs` decodes them.
    pub encoded_normals: Option<Vec<[i16; 2]>>,
    /// Blender before 4.1 smoothed by angle when the mesh's "Auto Smooth"
    /// was on: the angle in radians.
    pub auto_smooth: Option<f32>,
    /// Vertex group weights per vertex, as (group, weight), for the
    /// Armature modifier; empty when the mesh has none.
    pub weights: Vec<Vec<(u16, f32)>>,
    /// Vertex group names (since 3.0 on the mesh; before, on the object).
    pub groups: Vec<String>,
}

/*┌─────────────────╮
| Attributes      |
└─────────────────╯*/
//region Attributes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Domain {
    Point,
    Edge,
    Face,
    Corner,
    /// The triangles and quads of meshes from before 2.63.
    TessFace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    Int8,
    Int16x2,
    Int32,
    Int32x2,
    Float,
    Float2,
    Float3,
    ColorByte,
    ColorFloat,
    /// A structure layer from before generic attributes, by its CustomData
    /// type: 0 MVert, 3 MEdge, 4 MFace, 5 MTFace, 6 MCol, 16 MLoopUV,
    /// 25 MPoly, 26 MLoop.
    Legacy(i32),
}

impl Kind {
    fn size(self) -> usize {
        match self {
            Kind::Bool | Kind::Int8 => 1,
            Kind::Int16x2 | Kind::Int32 | Kind::Float | Kind::ColorByte => 4,
            Kind::Int32x2 | Kind::Float2 => 8,
            Kind::Float3 => 12,
            Kind::ColorFloat => 16,
            Kind::Legacy(_) => 0,
        }
    }

    /// The kind of a CustomData layer type.
    fn of_custom_data(t: i32) -> Kind {
        match t {
            10 | 30 => Kind::Float,
            11 => Kind::Int32,
            17 => Kind::ColorByte,
            22 | 41 => Kind::Int16x2,
            45 => Kind::Int8,
            46 => Kind::Int32x2,
            47 => Kind::ColorFloat,
            48 => Kind::Float3,
            49 => Kind::Float2,
            50 => Kind::Bool,
            t => Kind::Legacy(t),
        }
    }

    /// The kind of a Blender 5 attribute type (`bke::AttrType`).
    fn of_attr_type(t: i64) -> Option<Kind> {
        Some(match t {
            0 => Kind::Bool,
            1 => Kind::Int8,
            2 => Kind::Int16x2,
            3 => Kind::Int32,
            4 => Kind::Int32x2,
            5 => Kind::Float,
            6 => Kind::Float2,
            7 => Kind::Float3,
            9 => Kind::ColorByte,
            10 => Kind::ColorFloat,
            _ => return None,
        })
    }
}

/// One attribute or layer, its values still raw bytes in the file.
#[derive(Clone)]
struct Attr<'a> {
    name: String,
    domain: Domain,
    kind: Kind,
    bytes: &'a [u8],
    /// A single value for the whole domain (Blender 5's single storage).
    single: bool,
    /// The structure of a legacy layer's elements.
    s: Option<u32>,
    /// Which layer of its type renders use (CustomData's `active_rnd`).
    active_rnd: i64,
}

/// Every attribute and layer of a mesh.
struct Attrs<'a> {
    list: Vec<Attr<'a>>,
    rd: Endian,
}

impl<'a> Attrs<'a> {
    fn collect(f: &'a BlendFile, me: &Obj<'a>) -> Attrs<'a> {
        let mut list = Vec::new();
        // 1. Blender 5's attribute storage.
        if let Some(st) = me.sub("attribute_storage") {
            let n = st.int(&["dna_attributes_num"]).unwrap_or(0).clamp(0, 1 << 16) as usize;
            let want = Some("Attribute");
            if let Some(first) = st.follow(st.ptr("dna_attributes"), want) {
                for i in 0..n.min(first.fits()) {
                    let Some(a) = first.nth(i) else { break };
                    let name = c_string(a.raw("name").unwrap_or(&[]));
                    let domain = match a.int(&["domain"]) {
                        Some(0) => Domain::Point,
                        Some(1) => Domain::Edge,
                        Some(2) => Domain::Face,
                        Some(3) => Domain::Corner,
                        _ => continue,
                    };
                    let Some(kind) = a.int(&["data_type"]).and_then(Kind::of_attr_type) else { continue };
                    let single = a.int(&["storage_type"]) == Some(1);
                    let holder = if single { "AttributeSingle" } else { "AttributeArray" };
                    let Some(h) = a.deref_as("data", holder) else { continue };
                    let Some(bytes) = h.raw("data") else { continue };
                    list.push(Attr { name, domain, kind, bytes, single, s: None, active_rnd: 0 });
                }
            }
        }
        // 2. CustomData layers: generic attributes before Blender 5, and
        //    the legacy structures.
        for (field, domain) in [
            ("vdata", Domain::Point),
            ("edata", Domain::Edge),
            ("pdata", Domain::Face),
            ("ldata", Domain::Corner),
            ("fdata", Domain::TessFace),
        ] {
            let Some(cd) = me.sub(field) else { continue };
            let n = cd.int(&["totlayer"]).unwrap_or(0).clamp(0, 1 << 12) as usize;
            let Some(first) = cd.deref("layers") else { continue };
            for i in 0..n.min(first.fits()) {
                let Some(l) = first.nth(i) else { break };
                let t = l.int(&["type"]).unwrap_or(-1) as i32;
                let Some((bi, offset)) = l.target("data") else { continue };
                let block = f.block(bi);
                let bytes = &f.block_bytes(bi)[offset..];
                let kind = Kind::of_custom_data(t);
                let s = matches!(kind, Kind::Legacy(_)).then_some(block.sdna);
                let mut name = l.string("name");
                if t == 41 {
                    name = "custom_normal".into();
                }
                let active_rnd = l.int(&["active_rnd"]).unwrap_or(0);
                list.push(Attr { name, domain, kind, bytes, single: false, s, active_rnd });
            }
        }
        Attrs { list, rd: f.endian() }
    }

    fn find(&self, name: &str, domain: Domain, kind: Kind) -> Option<&Attr<'a>> {
        self.list.iter().find(|a| a.name == name && a.domain == domain && a.kind == kind)
    }

    fn legacy(&self, t: i32) -> Option<&Attr<'a>> {
        self.list.iter().find(|a| a.kind == Kind::Legacy(t))
    }

    /// `n` values of a numeric attribute, converted to f32 (several per
    /// element for vectors and colours), or None if the bytes run short.
    fn floats(&self, a: &Attr, n: usize) -> Option<Vec<f32>> {
        let rd = self.rd;
        let size = a.kind.size();
        let per = match a.kind {
            Kind::Float2 | Kind::Int32x2 | Kind::Int16x2 => 2,
            Kind::Float3 => 3,
            Kind::ColorByte | Kind::ColorFloat => 4,
            _ => 1,
        };
        let elems = if a.single { 1 } else { n };
        let bytes = a.bytes.get(..elems.checked_mul(size)?)?;
        let mut out = Vec::with_capacity(n * per);
        for e in bytes.chunks_exact(size) {
            match a.kind {
                Kind::Bool | Kind::Int8 => out.push(e[0] as i8 as f32),
                Kind::Int16x2 => out.extend([rd.u16(e) as i16 as f32, rd.u16(&e[2..]) as i16 as f32]),
                Kind::Int32 => out.push(rd.i32(e) as f32),
                Kind::Int32x2 => out.extend([rd.i32(e) as f32, rd.i32(&e[4..]) as f32]),
                Kind::Float | Kind::Float2 | Kind::Float3 | Kind::ColorFloat => {
                    out.extend(e.chunks_exact(4).map(|c| rd.f32(c)))
                }
                Kind::ColorByte => out.extend(e.iter().map(|&b| b as f32 / 255.0)),
                Kind::Legacy(_) => return None,
            }
        }
        if a.single {
            let one = out.clone();
            for _ in 1..n {
                out.extend_from_slice(&one);
            }
        }
        Some(out)
    }

    fn ints(&self, a: &Attr, n: usize) -> Option<Vec<i64>> {
        let rd = self.rd;
        let size = a.kind.size();
        let elems = if a.single { 1 } else { n };
        let bytes = a.bytes.get(..elems.checked_mul(size)?)?;
        let mut out: Vec<i64> = match a.kind {
            Kind::Int32 => bytes.chunks_exact(4).map(|c| rd.i32(c) as i64).collect(),
            Kind::Int32x2 => bytes.chunks_exact(4).map(|c| rd.i32(c) as i64).collect(),
            Kind::Bool | Kind::Int8 => bytes.iter().map(|&b| b as i8 as i64).collect(),
            _ => return None,
        };
        if a.single {
            out = out.iter().copied().cycle().take(out.len() * n).collect();
        }
        Some(out)
    }
}

/// A legacy structure array: `n` elements of structure `s` from `bytes`,
/// read one field at a time.
struct Structs<'a> {
    f: &'a BlendFile,
    bytes: &'a [u8],
    size: usize,
    n: usize,
    s: u32,
}

impl<'a> Structs<'a> {
    fn new(f: &'a BlendFile, bytes: &'a [u8], s: u32, n: usize) -> Option<Structs<'a>> {
        let size = f.sdna.get(s)?.size as usize;
        if size == 0 {
            return None;
        }
        Some(Structs { f, bytes, size, n: n.min(bytes.len() / size), s })
    }

    /// Element `i` of a field, for every structure, as f32: `out[k]` is
    /// structure k's value.  Missing fields give None.
    fn field(&self, name: &str, i: usize) -> Option<Vec<f32>> {
        let st = self.f.sdna.get(self.s)?;
        let fl = st.field(name)?;
        let prim = self.f.sdna.types.get(fl.ty as usize)?.prim;
        let w = prim.size();
        if fl.pointer || w == 0 || i >= fl.elems as usize {
            return None;
        }
        let rd = self.f.endian();
        let off = fl.offset as usize + i * w;
        if off + w > self.size {
            return None;
        }
        Some(
            (0..self.n)
                .map(|k| {
                    let b = &self.bytes[k * self.size + off..k * self.size + off + w];
                    use crate::sdna::Prim::*;
                    match prim {
                        Char => b[0] as i8 as f32,
                        UChar => b[0] as f32,
                        Short => rd.u16(b) as i16 as f32,
                        UShort => rd.u16(b) as f32,
                        Int => rd.i32(b) as f32,
                        UInt => rd.u32(b) as f32,
                        Int64 => rd.i64(b) as f32,
                        UInt64 => rd.u64(b) as f32,
                        Float => rd.f32(b),
                        Double => rd.f64(b) as f32,
                        Other => 0.0,
                    }
                })
                .collect(),
        )
    }

    /// Like `field`, as exact integers (vertex indices exceed f32's 24
    /// bits of precision).
    fn int_field(&self, name: &str, i: usize) -> Option<Vec<i64>> {
        let st = self.f.sdna.get(self.s)?;
        let fl = st.field(name)?;
        let prim = self.f.sdna.types.get(fl.ty as usize)?.prim;
        let w = prim.size();
        if fl.pointer || w == 0 || i >= fl.elems as usize || !matches!(w, 1 | 2 | 4 | 8) {
            return None;
        }
        let rd = self.f.endian();
        let off = fl.offset as usize + i * w;
        if off + w > self.size {
            return None;
        }
        use crate::sdna::Prim::*;
        Some(
            (0..self.n)
                .map(|k| {
                    let b = &self.bytes[k * self.size + off..k * self.size + off + w];
                    match prim {
                        Char => b[0] as i8 as i64,
                        UChar => b[0] as i64,
                        Short => rd.u16(b) as i16 as i64,
                        UShort => rd.u16(b) as i64,
                        Int => rd.i32(b) as i64,
                        UInt => rd.u32(b) as i64,
                        Int64 | UInt64 => rd.i64(b),
                        Float => rd.f32(b) as i64,
                        Double => rd.f64(b) as i64,
                        Other => 0,
                    }
                })
                .collect(),
        )
    }
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// Reads mesh data-block `me`.  Problems that leave a usable mesh go into
/// `warnings`.
pub fn read(f: &BlendFile, me: &Obj, warnings: &mut Vec<String>) -> Option<MeshData> {
    let name = me.id_name();
    let count = |names: &[&str]| me.int(names).unwrap_or(0).clamp(0, i32::MAX as i64) as usize;
    let verts = count(&["totvert", "verts_num"]);
    let edges = count(&["totedge", "edges_num"]);
    let faces = count(&["totpoly", "faces_num"]);
    let corners = count(&["totloop", "corners_num"]);
    let tessfaces = count(&["totface", "totface_legacy"]);
    let attrs = Attrs::collect(f, me);
    let mut poly = PolyMesh { face_starts: vec![0], ..Default::default() };

    // 1. Positions.
    if let Some(a) = attrs.find("position", Domain::Point, Kind::Float3)
        && let Some(v) = attrs.floats(a, verts)
    {
        poly.positions = v.chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
    } else if let Some(s) = legacy_structs(f, me, &attrs, 0, "mvert", "MVert", verts)
        && let (Some(x), Some(y), Some(z)) = (s.field("co", 0), s.field("co", 1), s.field("co", 2))
    {
        poly.positions = (0..x.len()).map(|i| Vec3::new(x[i], y[i], z[i])).collect();
    }
    if poly.positions.is_empty() {
        return None;
    }
    let verts = poly.positions.len();

    // 2. Faces, from offsets and corner vertices, from MPoly and MLoop, or
    //    from MFace.
    let mut tess = false;
    let offsets = me.raw("poly_offset_indices").filter(|_| faces > 0);
    let corner_vert_attr = attrs.find(".corner_vert", Domain::Corner, Kind::Int32);
    if let (Some(off), Some(cv)) = (offsets, corner_vert_attr)
        && let Some(cv) = attrs.ints(cv, corners)
        && off.len() >= (faces + 1) * 4
    {
        let rd = f.endian();
        poly.face_starts = (0..=faces).map(|i| rd.i32(&off[i * 4..]).clamp(0, cv.len() as i32) as u32).collect();
        poly.corner_verts = cv.iter().map(|&v| v.clamp(0, u32::MAX as i64) as u32).collect();
        read_corner_layers(f, &attrs, &mut poly, corners, None);
    } else if let Some(mp) = legacy_structs(f, me, &attrs, 25, "mpoly", "MPoly", faces)
        && let Some(ml) = legacy_structs(f, me, &attrs, 26, "mloop", "MLoop", corners)
        && let (Some(start), Some(total), Some(v)) =
            (mp.int_field("loopstart", 0), mp.int_field("totloop", 0), ml.int_field("v", 0))
    {
        // MPoly allowed faces in any order of their corners; corners are
        // copied face by face so each face's run is contiguous.
        poly.face_starts = vec![0];
        for (&s, &t) in start.iter().zip(&total) {
            let (s, t) = (s.max(0) as usize, t.max(0) as usize);
            if let Some(run) = v.get(s..s.saturating_add(t)) {
                poly.corner_verts.extend(run.iter().map(|&x| x.clamp(0, u32::MAX as i64) as u32));
            }
            poly.face_starts.push(poly.corner_verts.len() as u32);
        }
        if let Some(m) = mp.int_field("mat_nr", 0) {
            poly.face_material = m.iter().map(|&m| m.clamp(0, u16::MAX as i64) as u16).collect();
        }
        if let Some(fl) = mp.int_field("flag", 0) {
            poly.face_smooth = fl.iter().map(|&x| x & 1 != 0).collect();
        }
        // Corner attributes were stored per loop, in loop order; reorder
        // them to match the copied corners.
        let order: Vec<usize> = start
            .iter()
            .zip(&total)
            .flat_map(|(&s, &t)| {
                let s = s.max(0) as usize;
                (s..s.saturating_add(t.max(0) as usize)).filter(|&i| i < v.len())
            })
            .collect();
        read_corner_layers(f, &attrs, &mut poly, corners, Some(&order));
    } else if let Some(mf) = legacy_structs(f, me, &attrs, 4, "mface", "MFace", tessfaces) {
        tess = true;
        read_tessfaces(f, &attrs, &mf, &mut poly);
    }

    // 3. Face attributes stored as generic attributes.
    let nfaces = poly.face_count();
    if let Some(a) = attrs.find("material_index", Domain::Face, Kind::Int32)
        && let Some(m) = attrs.ints(a, nfaces)
    {
        poly.face_material = m.iter().map(|&m| m.clamp(0, u16::MAX as i64) as u16).collect();
    }
    if let Some(a) = attrs.find("sharp_face", Domain::Face, Kind::Bool)
        && let Some(s) = attrs.ints(a, nfaces)
    {
        poly.face_smooth = s.iter().map(|&s| s == 0).collect();
    } else if poly.face_smooth.is_empty() && !tess {
        // Since generic attributes, a missing "sharp_face" means smooth.
        poly.face_smooth = vec![true; nfaces];
    }

    // 4. Sharp edges and creases, by vertex pair.
    let auto_smooth = auto_smooth(f, me);
    let edge_verts: Option<Vec<i64>> =
        attrs.find(".edge_verts", Domain::Edge, Kind::Int32x2).and_then(|a| attrs.ints(a, edges)).or_else(|| {
            let me_s = legacy_structs(f, me, &attrs, 3, "medge", "MEdge", edges)?;
            let (a, b) = (me_s.int_field("v1", 0)?, me_s.int_field("v2", 0)?);
            Some(a.iter().zip(&b).flat_map(|(&a, &b)| [a, b]).collect())
        });
    if let Some(ev) = &edge_verts {
        let key = |i: usize| {
            let (a, b) = (ev[2 * i], ev[2 * i + 1]);
            ((0..verts as i64).contains(&a) && (0..verts as i64).contains(&b)).then(|| edge_key(a as u32, b as u32))
        };
        let n = ev.len() / 2;
        // Before 4.1, marked sharp edges only counted with Auto Smooth on.
        let sharp_counts = f.header.version >= 401 || auto_smooth.is_some();
        let sharp: Option<Vec<bool>> = attrs
            .find("sharp_edge", Domain::Edge, Kind::Bool)
            .and_then(|a| attrs.ints(a, n))
            .map(|v| v.iter().map(|&x| x != 0).collect())
            .or_else(|| {
                let s = legacy_structs(f, me, &attrs, 3, "medge", "MEdge", n)?;
                Some(s.int_field("flag", 0)?.iter().map(|&fl| fl & (1 << 9) != 0).collect())
            });
        if sharp_counts && let Some(sharp) = sharp {
            poly.sharp_edges = (0..n.min(sharp.len())).filter(|&i| sharp[i]).filter_map(key).collect();
        }
        let crease: Option<Vec<f32>> = attrs
            .find("crease_edge", Domain::Edge, Kind::Float)
            .or_else(|| {
                attrs.list.iter().find(|a| a.domain == Domain::Edge && a.kind == Kind::Float && a.name.is_empty())
            })
            .and_then(|a| attrs.floats(a, n))
            .or_else(|| {
                let s = legacy_structs(f, me, &attrs, 3, "medge", "MEdge", n)?;
                Some(s.field("crease", 0)?.iter().map(|&c| if c < 0.0 { c + 256.0 } else { c } / 255.0).collect())
            });
        if let Some(c) = crease {
            poly.creases =
                (0..n.min(c.len())).filter(|&i| c[i] > 0.0).filter_map(|i| Some((key(i)?, c[i].min(1.0)))).collect();
        }
    }

    // 5. Custom normals.
    let mut encoded_normals = None;
    if let Some(a) = attrs.find("custom_normal", Domain::Corner, Kind::Int16x2)
        && let Some(v) = attrs.floats(a, corners)
        && !tess
    {
        let v: Vec<[i16; 2]> = v.chunks_exact(2).map(|c| [c[0] as i16, c[1] as i16]).collect();
        encoded_normals = Some(v);
    } else if let Some(a) = attrs.list.iter().find(|a| a.name == "custom_normal" && a.kind == Kind::Float3) {
        // Blender 4.5's "free" normals: plain vectors on any domain.
        let n = match a.domain {
            Domain::Point => verts,
            Domain::Face => nfaces,
            _ => corners,
        };
        if let Some(v) = attrs.floats(a, n) {
            let v: Vec<Vec3> = v.chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
            poly.custom_normals = Some(spread(&poly, a.domain, &v));
        }
    }

    // 6. Material slots.
    let totcol = me.int(&["totcol"]).unwrap_or(0).clamp(0, i16::MAX as i64) as usize;
    let materials = me.ptr_array("mat", totcol);

    // 7. Shape keys and vertex groups, which objects use.
    apply_shape_keys(me, &mut poly);
    let weights = deform_weights(f, me, &attrs, verts);
    let groups = me.list("vertex_group_names").iter().map(|g| g.string("name")).collect();

    poly.validate();
    if let Some(n) = &encoded_normals
        && n.len() != poly.corner_verts.len()
    {
        warnings.push(format!("mesh \"{name}\": custom normals don't match its corners, ignored"));
        encoded_normals = None;
    }
    Some(MeshData { poly, materials, encoded_normals, auto_smooth, weights, groups })
}

/// Moves the vertices by the mesh's relative shape keys at the values they
/// were saved with, as the viewport shows them.
fn apply_shape_keys(me: &Obj, poly: &mut PolyMesh) {
    let Some(key) = me.deref("key").filter(|k| k.type_name() == "Key") else { return };
    if key.int(&["type"]).unwrap_or(1) != 1 {
        return;
    }
    let blocks = key.list("block");
    let n = poly.positions.len();
    let rd = me.f.endian();
    let read = |kb: &Obj| -> Option<Vec<Vec3>> {
        let raw = kb.raw("data")?;
        let count = (kb.int(&["totelem"]).unwrap_or(0).max(0) as usize).min(n);
        let bytes = raw.get(..count * 12)?;
        Some(bytes.chunks_exact(12).map(|c| Vec3::new(rd.f32(c), rd.f32(&c[4..]), rd.f32(&c[8..]))).collect())
    };
    let mut delta = vec![Vec3::ZERO; n];
    let mut any = false;
    for kb in blocks.iter().skip(1) {
        let value = kb.float(&["curval"]).unwrap_or(0.0);
        if value == 0.0 || !value.is_finite() || kb.int(&["flag"]).unwrap_or(0) & 1 != 0 {
            continue;
        }
        let rel = kb.int(&["relative"]).unwrap_or(0).clamp(0, blocks.len() as i64 - 1) as usize;
        let (Some(shape), Some(base)) = (read(kb), read(&blocks[rel])) else { continue };
        for (i, d) in delta.iter_mut().enumerate().take(shape.len().min(base.len())) {
            *d += (shape[i] - base[i]) * value;
        }
        any = true;
    }
    if any {
        for (p, d) in poly.positions.iter_mut().zip(delta) {
            if d.is_finite() {
                *p += d;
            }
        }
    }
}

/// Vertex group weights: an `MDeformVert` per vertex, each pointing to its
/// own (group, weight) pairs.
fn deform_weights(f: &BlendFile, me: &Obj, attrs: &Attrs, n: usize) -> Vec<Vec<(u16, f32)>> {
    let Some(dv) = legacy_structs(f, me, attrs, 2, "dvert", "MDeformVert", n) else { return Vec::new() };
    let Some(st) = f.sdna.get(dv.s) else { return Vec::new() };
    let (Some(dw), Some(tot)) = (st.field("dw"), dv.int_field("totweight", 0)) else { return Vec::new() };
    let rd = f.endian();
    let ptr_size = f.header.pointer_size as usize;
    // The weight arrays belong to the mesh, like every block after it.
    let block = me.block;
    let mut out = Vec::with_capacity(dv.n);
    for (k, &count) in tot.iter().enumerate() {
        let at = k * dv.size + dw.offset as usize;
        let addr = match dv.bytes.get(at..at + ptr_size) {
            Some(b) if ptr_size == 4 => rd.u32(b) as u64,
            Some(b) => rd.u64(b),
            None => 0,
        };
        let count = count.clamp(0, 64) as usize;
        let mut list = Vec::with_capacity(count);
        if let Some((bi, off)) = f.find_near(addr, block) {
            let bytes = &f.block_bytes(bi)[off..];
            for w in bytes.chunks_exact(8).take(count) {
                let (group, weight) = (rd.i32(w), rd.f32(&w[4..]));
                if (0..u16::MAX as i32).contains(&group) && weight.is_finite() && weight > 0.0 {
                    list.push((group as u16, weight));
                }
            }
        }
        out.push(list);
    }
    out
}

/// The legacy structure array of CustomData type `t`, or failing that the
/// mesh's direct pointer `field` (files from before CustomData).
fn legacy_structs<'a>(
    f: &'a BlendFile,
    me: &Obj<'a>,
    attrs: &Attrs<'a>,
    t: i32,
    field: &str,
    struct_name: &str,
    n: usize,
) -> Option<Structs<'a>> {
    if let Some(a) = attrs.legacy(t) {
        let s = a.s.filter(|&s| f.sdna.struct_name(s) == struct_name).or_else(|| f.sdna.struct_named(struct_name))?;
        return Structs::new(f, a.bytes, s, n);
    }
    let (i, offset) = me.target(field)?;
    let s = f.sdna.struct_named(struct_name)?;
    Structs::new(f, &f.block_bytes(i)[offset..], s, n)
}

/// UV maps and colours stored per corner (and colours per point), in
/// either the generic or the legacy layout.  `order` maps output corners
/// to stored ones when MPoly reordered them.
fn read_corner_layers(f: &BlendFile, attrs: &Attrs, poly: &mut PolyMesh, corners: usize, order: Option<&[usize]>) {
    let reorder = |v: Vec<Vec2>| -> Vec<Vec2> {
        match order {
            Some(o) => o.iter().map(|&i| v.get(i).copied().unwrap_or(Vec2::ZERO)).collect(),
            None => v,
        }
    };
    // 1. UV maps: 2D float corner attributes (since 3.5), or MLoopUV.
    let mut uv_active = None;
    for a in &attrs.list {
        if a.domain != Domain::Corner || a.name.starts_with('.') {
            continue;
        }
        let values: Option<Vec<Vec2>> = match a.kind {
            Kind::Float2 => {
                attrs.floats(a, corners).map(|v| v.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect())
            }
            Kind::Legacy(16) => {
                let s = a.s.or_else(|| f.sdna.struct_named("MLoopUV"));
                s.and_then(|s| Structs::new(f, a.bytes, s, corners)).and_then(|st| {
                    let (u, v) = (st.field("uv", 0)?, st.field("uv", 1)?);
                    Some(u.iter().zip(&v).map(|(&u, &v)| Vec2::new(u, v)).collect())
                })
            }
            _ => None,
        };
        if let Some(values) = values {
            if uv_active.is_none() {
                uv_active = Some(a.active_rnd.max(0) as usize);
            }
            poly.uvs.push(Layer { name: a.name.clone(), values: reorder(values) });
        }
    }
    poly.default_uv = uv_active.unwrap_or(0);
    // 2. Colour attributes on corners or points, or MLoopCol layers.
    let mut col_active = None;
    for a in &attrs.list {
        if a.name.starts_with('.') || !matches!(a.kind, Kind::ColorByte | Kind::ColorFloat) {
            continue;
        }
        let n = match a.domain {
            Domain::Corner => corners,
            Domain::Point => poly.positions.len(),
            _ => continue,
        };
        let Some(v) = attrs.floats(a, n) else { continue };
        let srgb = a.kind == Kind::ColorByte;
        let v: Vec<Vec4> = v
            .chunks_exact(4)
            .map(|c| {
                let c = Vec4::new(c[0], c[1], c[2], c[3]);
                if srgb { Vec4::new(to_linear(c.x), to_linear(c.y), to_linear(c.z), c.w) } else { c }
            })
            .collect();
        let values = if a.domain == Domain::Point {
            poly.corner_verts.iter().map(|&p| v.get(p as usize).copied().unwrap_or(Vec4::ONE)).collect()
        } else {
            match order {
                Some(o) => o.iter().map(|&i| v.get(i).copied().unwrap_or(Vec4::ONE)).collect(),
                None => v,
            }
        };
        if col_active.is_none() && a.kind == Kind::ColorByte {
            col_active = Some(a.active_rnd.max(0) as usize);
        }
        poly.colors.push(Layer { name: a.name.clone(), values });
    }
    poly.default_color = col_active.unwrap_or(0);
}

/// Meshes from before 2.63: `MFace` triangles and quads, with UVs from
/// `MTFace` and colours from `MCol`, four per face.
fn read_tessfaces(f: &BlendFile, attrs: &Attrs, mf: &Structs, poly: &mut PolyMesh) {
    let (Some(v1), Some(v2), Some(v3), Some(v4)) =
        (mf.int_field("v1", 0), mf.int_field("v2", 0), mf.int_field("v3", 0), mf.int_field("v4", 0))
    else {
        return;
    };
    let mat = mf.int_field("mat_nr", 0).unwrap_or_default();
    let flag = mf.int_field("flag", 0).unwrap_or_default();
    let n = v1.len();
    let mut corner_of = Vec::with_capacity(n * 4);
    for i in 0..n {
        let quad = v4[i] != 0;
        let vs = if quad { &[v1[i], v2[i], v3[i], v4[i]][..] } else { &[v1[i], v2[i], v3[i]][..] };
        for (k, &v) in vs.iter().enumerate() {
            poly.corner_verts.push(v.clamp(0, u32::MAX as i64) as u32);
            corner_of.push((i, k));
        }
        poly.face_starts.push(poly.corner_verts.len() as u32);
        poly.face_material.push(mat.get(i).copied().unwrap_or(0).clamp(0, u16::MAX as i64) as u16);
        poly.face_smooth.push(flag.get(i).is_some_and(|&x| x & 1 != 0));
    }
    // UVs: MTFace layers in the face data, four UV pairs per face.
    for a in attrs.list.iter().filter(|a| a.domain == Domain::TessFace && a.kind == Kind::Legacy(5)) {
        let Some(st) = a.s.or_else(|| f.sdna.struct_named("MTFace")).and_then(|s| Structs::new(f, a.bytes, s, n))
        else {
            continue;
        };
        let uv: Vec<Option<Vec<f32>>> = (0..8).map(|k| st.field("uv", k)).collect();
        if uv.iter().any(|u| u.is_none()) {
            continue;
        }
        let values = corner_of
            .iter()
            .map(|&(i, k)| {
                let (u, v) = (uv[2 * k].as_ref().unwrap(), uv[2 * k + 1].as_ref().unwrap());
                Vec2::new(u.get(i).copied().unwrap_or(0.0), v.get(i).copied().unwrap_or(0.0))
            })
            .collect();
        poly.uvs.push(Layer { name: a.name.clone(), values });
    }
    // Colours: MCol layers, four per face, stored as a, r, g, b with red
    // and blue swapped.
    for a in attrs.list.iter().filter(|a| a.domain == Domain::TessFace && a.kind == Kind::Legacy(6)) {
        let Some(st) = a.s.or_else(|| f.sdna.struct_named("MCol")).and_then(|s| Structs::new(f, a.bytes, s, n * 4))
        else {
            continue;
        };
        let (Some(r), Some(g), Some(b), Some(al)) =
            (st.field("b", 0), st.field("g", 0), st.field("r", 0), st.field("a", 0))
        else {
            continue;
        };
        let byte = |x: f32| if x < 0.0 { x + 256.0 } else { x } / 255.0;
        let values = corner_of
            .iter()
            .map(|&(i, k)| {
                let j = i * 4 + k;
                let get = |c: &Vec<f32>| c.get(j).copied().map(byte).unwrap_or(1.0);
                Vec4::new(to_linear(get(&r)), to_linear(get(&g)), to_linear(get(&b)), get(&al))
            })
            .collect();
        poly.colors.push(Layer { name: a.name.clone(), values });
    }
}

/// Copies values stored per point or per face onto every corner.
fn spread(poly: &PolyMesh, domain: Domain, v: &[Vec3]) -> Vec<Vec3> {
    match domain {
        Domain::Point => poly.corner_verts.iter().map(|&p| v.get(p as usize).copied().unwrap_or(Vec3::ZERO)).collect(),
        Domain::Face => {
            let mut out = vec![Vec3::ZERO; poly.corner_verts.len()];
            for f in 0..poly.face_count() {
                for c in poly.face(f) {
                    out[c] = v.get(f).copied().unwrap_or(Vec3::ZERO);
                }
            }
            out
        }
        _ => v.to_vec(),
    }
}

/// The Auto Smooth angle of a mesh from before Blender 4.1, which stored
/// it in degrees (as a short) before 2.8 and in radians since.
fn auto_smooth(f: &BlendFile, me: &Obj) -> Option<f32> {
    if f.header.version >= 401 {
        return None;
    }
    let flag = me.int(&["flag"])?;
    if flag & (1 << 5) == 0 {
        return None;
    }
    let fl = me.field("smoothresh")?;
    let degrees = f.sdna.types.get(fl.ty as usize).is_some_and(|t| t.prim == crate::sdna::Prim::Short);
    let v = me.float(&["smoothresh"])?;
    Some(if degrees { v.to_radians() } else { v })
}

//endregion

/// A NUL-terminated string from the start of `b`.
pub fn c_string(b: &[u8]) -> String {
    let len = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..len]).into_owned()
}

/// sRGB-encoded to linear.
pub fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
