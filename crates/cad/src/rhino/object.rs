//! Objects of the object table: a class chunk holds the class's id, its
//! data and optional user data.  From the classes that carry cached render
//! meshes (a mesh itself, a Brep with a mesh per face, an extrusion) this
//! pulls the meshes out.

use super::chunks::{
    ANONYMOUS_CHUNK, Archive, Chunk, Cursor, OPENNURBS_CLASS, OPENNURBS_CLASS_DATA, OPENNURBS_CLASS_UUID,
};
use super::mesh::{self, MeshData};

/// Class ids, as they sit in the file (the mixed-endian GUID layout).
fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&a.to_le_bytes());
    g[4..6].copy_from_slice(&b.to_le_bytes());
    g[6..8].copy_from_slice(&c.to_le_bytes());
    g[8..16].copy_from_slice(&d);
    g
}

pub fn mesh_class() -> [u8; 16] {
    guid(0x4ED7D4E4, 0xE947, 0x11d3, [0xBF, 0xE5, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0])
}

/// The ids of the classes openNURBS reads as a Brep: its own, Rhino's
/// `TL_Brep` (which Rhino 4 and later write), and the two old trimmed
/// surface and Brep classes.
pub fn is_brep_class(id: &[u8; 16]) -> bool {
    [
        guid(0x60B5DBC5, 0xE660, 0x11d3, [0xBF, 0xE4, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0]),
        guid(0xF06FC243, 0xA32A, 0x4608, [0x9D, 0xD8, 0xA7, 0xD2, 0xC4, 0xCE, 0x2A, 0x36]),
        guid(0x0705FDEF, 0x3E2A, 0x11d4, [0x80, 0x0E, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0]),
        guid(0x2D4CFEDB, 0x3E2A, 0x11d4, [0x80, 0x0E, 0x00, 0x10, 0x83, 0x01, 0x22, 0xF0]),
    ]
    .contains(id)
}

pub fn extrusion_class() -> [u8; 16] {
    guid(0x36F53175, 0x72B8, 0x4d47, [0xBF, 0x1F, 0xB4, 0xE6, 0xFC, 0x24, 0xF4, 0xB9])
}

pub fn subd_class() -> [u8; 16] {
    guid(0xF09BA4D9, 0x455B, 0x42C3, [0xBA, 0x3B, 0xE6, 0xCC, 0xAC, 0xEF, 0x85, 0x3B])
}

pub fn instance_ref_class() -> [u8; 16] {
    guid(0xF9CFB638, 0xB9D4, 0x4340, [0x87, 0xE3, 0xC5, 0x6E, 0x78, 0x65, 0xD9, 0x6A])
}

/// The id openNURBS gives the display mesh cache that Rhino 5 attached to
/// extrusions as user data.
fn v5_mesh_cache_class() -> [u8; 16] {
    guid(0xA8130A3E, 0xE4F3, 0x4CB0, [0xBB, 0x8A, 0xF1, 0x0A, 0x47, 0x39, 0x12, 0xD0])
}

/// The id of the render mesh in a Rhino 6 mesh cache.
fn render_mesh_id() -> [u8; 16] {
    guid(0xF5E3BAA9, 0xA7A2, 0x49FD, [0xB8, 0xA1, 0x66, 0xEB, 0x27, 0x4A, 0x5F, 0x91])
}

/// A class chunk taken apart.
pub struct Class<'a> {
    pub id: [u8; 16],
    /// The class's own data, which only the class's reader understands.
    pub data: &'a [u8],
    /// User data chunks (type code 0x00027FFD), whose bodies follow.
    pub user_data: Vec<&'a [u8]>,
}

/// Parses the class chunk `chunk`.
pub fn class<'a>(a: &Archive<'a>, chunk: &Chunk) -> Option<Class<'a>> {
    if chunk.tcode != OPENNURBS_CLASS {
        return None;
    }
    let mut id = None;
    let mut data: &[u8] = &[];
    let mut user_data = Vec::new();
    for c in a.chunks(chunk.body.clone()) {
        match c.tcode {
            OPENNURBS_CLASS_UUID => id = a.body(&c).get(..16).and_then(|b| b.try_into().ok()),
            OPENNURBS_CLASS_DATA => data = a.body(&c),
            0x0002_7FFD => user_data.push(a.body(&c)),
            _ => {}
        }
    }
    Some(Class { id: id?, data, user_data })
}

/// The class chunk at the cursor (an object written by `WriteObject`),
/// consumed; `None` when there isn't one.  A null object, which has a nil
/// id, is `Some(None)`.
fn next_object<'a>(a: &Archive<'a>, cur: &mut Cursor<'a>) -> Option<Option<Class<'a>>> {
    let sub = Archive { data: cur.data, version: a.version };
    let chunk = sub.chunk_at(cur.pos, cur.data.len())?;
    cur.pos = chunk.next;
    let c = class(&sub, &chunk)?;
    Some((c.id != [0u8; 16]).then_some(c))
}

/// The cached render meshes of a Brep (one per face that has one).  The
/// Brep's body is chunk-version 3.x: eight arrays in anonymous chunks (2D
/// and 3D curves, surfaces, vertices, edges, trims, loops and faces), the
/// bounding box, then from version 3.1 the render meshes and the analysis
/// meshes, each in an anonymous chunk holding a flag byte and an object per
/// face.
pub fn brep_meshes(a: &Archive, data: &[u8]) -> Vec<MeshData> {
    brep_meshes_inner(a, data).unwrap_or_default()
}

fn brep_meshes_inner(a: &Archive, data: &[u8]) -> Option<Vec<MeshData>> {
    let mut cur = Cursor::new(data);
    let (major, minor) = cur.version()?;
    if major != 3 || minor < 1 {
        return None;
    }
    let sub = Archive { data, version: a.version };
    let mut face_count = 0usize;
    for i in 0..8 {
        let c = sub.chunk_at(cur.pos, data.len())?;
        if c.tcode != ANONYMOUS_CHUNK {
            return None;
        }
        if i == 7 {
            // Faces: a version byte, then how many.
            let mut f = Cursor::new(sub.body(&c));
            f.version()?;
            face_count = usize::try_from(f.i32()?).ok()?;
        }
        cur.pos = c.next;
    }
    // The bounding box.
    cur.skip(48)?;
    let chunk = sub.chunk_at(cur.pos, data.len())?;
    if chunk.tcode != ANONYMOUS_CHUNK {
        return None;
    }
    let body = sub.body(&chunk);
    // Each face needs at least its flag byte.
    if face_count > body.len() {
        return None;
    }
    let mut meshes = Vec::new();
    let mut m = Cursor::new(body);
    for _ in 0..face_count {
        if m.u8()? == 0 {
            continue;
        }
        let body_archive = Archive { data: body, version: a.version };
        let object = next_object(&body_archive, &mut m)?;
        if let Some(c) = object
            && c.id == mesh_class()
            && let Some(mesh) = mesh::read(a, c.data)
        {
            meshes.push(mesh);
        }
    }
    Some(meshes)
}

/// The cached render mesh of an extrusion: in the object's own data from
/// archive version 60, in user data before.
pub fn extrusion_mesh(a: &Archive, class: &Class) -> Option<MeshData> {
    if a.version >= 60 {
        let sub = Archive { data: class.data, version: a.version };
        let chunk = sub.chunk_at(0, class.data.len())?;
        if chunk.tcode != ANONYMOUS_CHUNK {
            return None;
        }
        let mut cur = Cursor::new(sub.body(&chunk));
        let (major, minor) = cur.chunk_version()?;
        if major != 1 || minor < 3 {
            return None;
        }
        // The profile curve, an object.
        let body_archive = Archive { data: cur.data, version: a.version };
        next_object(&body_archive, &mut cur)?;
        // The path line (48), interval (16), up vector (24), two flags (2),
        // two vectors (48), the path domain (16), a flag (1), the profile
        // count (4) and two cap flags (2).
        cur.skip(48 + 16 + 24 + 2 + 48 + 16 + 1 + 4 + 2)?;
        render_mesh_of_cache(a, &mut cur)
    } else {
        for ud in &class.user_data {
            let sub = Archive { data: ud, version: a.version };
            let mut header = false;
            let mut mine = false;
            for c in sub.chunks(0..ud.len()) {
                let body = sub.body(&c);
                if c.tcode == 0x0002_FFF9 {
                    header = true;
                    mine = body.get(..16) == Some(&v5_mesh_cache_class()[..]);
                } else if header && mine && c.tcode == ANONYMOUS_CHUNK {
                    // The render mesh is the first of the objects.
                    let mut cur = Cursor::new(body);
                    let object = next_object(&Archive { data: body, version: a.version }, &mut cur)?;
                    return object.filter(|o| o.id == mesh_class()).and_then(|o| mesh::read(a, o.data));
                }
            }
        }
        None
    }
}

/// Reads an `ON_MeshCache` (version 1) at the cursor and returns its render
/// mesh.
fn render_mesh_of_cache(a: &Archive, cur: &mut Cursor) -> Option<MeshData> {
    let sub = Archive { data: cur.data, version: a.version };
    let chunk = sub.chunk_at(cur.pos, cur.data.len())?;
    if chunk.tcode != ANONYMOUS_CHUNK {
        return None;
    }
    let body = sub.body(&chunk);
    let mut m = Cursor::new(body);
    let (major, _) = m.chunk_version()?;
    if major != 1 {
        return None;
    }
    loop {
        if m.u8()? != 1 {
            return None;
        }
        // One cached mesh: an anonymous chunk with a version, the mesh's id
        // and the mesh object.
        let inner = Archive { data: body, version: a.version };
        let item = inner.chunk_at(m.pos, body.len())?;
        m.pos = item.next;
        let mut c = Cursor::new(inner.body(&item));
        let (major, _) = c.chunk_version()?;
        if major != 1 {
            return None;
        }
        let id = c.uuid()?;
        if id == render_mesh_id() {
            let object = next_object(&Archive { data: c.data, version: a.version }, &mut c)??;
            return (object.id == mesh_class()).then(|| mesh::read(a, object.data)).flatten();
        }
    }
}
