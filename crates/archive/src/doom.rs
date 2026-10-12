//! Doom WADs (IWAD and PWAD): a header, the lumps' bytes, and a directory of
//! 16-byte records at the end.  Lumps have no folders, so the structure is
//! in their order: pairs of marker lumps (`S_START` and `S_END`) enclose a
//! namespace, and a map's marker lump is followed by the lumps that make
//! it.  This reader shows that structure as folders and groups, and keeps
//! the names as stored and the duplicates in order.

use crate::source::{Le, Source};
use crate::{
    ArchiveError, ArchiveKind, DataKind, Entry, Extra, Group, GroupKind, Limits, Namespace, Parsed, Result, Slot,
};

/// How many lumps get their first bytes read to tell what they are.  Past
/// this the names alone decide, which keeps a huge WAD quick to list.
const MAX_SNIFFED: usize = 100_000;
/// The bytes read from each lump to recognise it: enough for the column
/// table of a picture up to about a thousand pixels wide.
const SNIFF_BYTES: usize = 4224;

/// The lumps that follow a map's marker as parts of the map.  UDMF maps
/// have TEXTMAP and end with ENDMAP; GL nodes have their own marker.
const MAP_LUMPS: &[&str] = &[
    "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS", "REJECT", "BLOCKMAP",
    "BEHAVIOR", "SCRIPTS", "TEXTMAP", "ZNODES", "DIALOGUE", "ENDMAP", "LIGHTMAP", "GL_VERT", "GL_SEGS", "GL_SSECT",
    "GL_NODES", "GL_PVS",
];

/// A marker lump's namespace and whether it opens it.
fn marker(upper: &str) -> Option<(Namespace, bool)> {
    let (stem, open) = if let Some(s) = upper.strip_suffix("_START") {
        (s, true)
    } else if let Some(s) = upper.strip_suffix("_END") {
        (s, false)
    } else {
        return None;
    };
    let ns = match stem {
        "S" | "SS" => Namespace::Sprites,
        "F" | "FF" | "F1" | "F2" | "F3" => Namespace::Flats,
        "P" | "PP" | "P1" | "P2" | "P3" => Namespace::Patches,
        "TX" => Namespace::Textures,
        "C" => Namespace::Colormaps,
        "A" => Namespace::Acs,
        "V" | "VX" => Namespace::Voxels,
        "HI" => Namespace::Hires,
        _ => return None,
    };
    Some((ns, open))
}

/// A lump name as text: ASCII as it is, other bytes as the Latin-1
/// characters of the same value, control characters as `?`.
fn lump_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    raw[..end].iter().map(|&b| if b < 0x20 || b == 0x7F { '?' } else { b as char }).collect()
}

struct Lump {
    name: String,
    offset: i64,
    size: i64,
}

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let len = source.len();
    let mut header = [0u8; 12];
    source.read_at(0, &mut header).map_err(|_| ArchiveError::invalid("the WAD header is cut short"))?;
    let h = Le(&header);
    let iwad = header.starts_with(b"IWAD");
    let (count, dir) = (h.i32(4).unwrap_or(-1), h.i32(8).unwrap_or(-1));
    if count < 0 || dir < 12 {
        return Err(ArchiveError::invalid("the WAD header has a negative lump count or directory offset"));
    }
    if count as usize > limits.max_entries {
        return Err(ArchiveError::TooLarge(format!("a WAD of {count} lumps")));
    }
    let table = source
        .bytes(dir as u64, count as u64 * 16)
        .map_err(|_| ArchiveError::invalid("the WAD's directory lies outside the file"))?;
    let t = Le(&table);
    let lumps: Vec<Lump> = (0..count as usize)
        .map(|i| Lump {
            offset: t.i32(i * 16).unwrap_or(-1) as i64,
            size: t.i32(i * 16 + 4).unwrap_or(-1) as i64,
            name: lump_name(&table[i * 16 + 8..i * 16 + 16]),
        })
        .collect();

    // Which lumps are map markers, and where each map's lumps end.
    let uppers: Vec<String> = lumps.iter().map(|l| l.name.to_ascii_uppercase()).collect();
    let sniff = lumps.len() <= MAX_SNIFFED;
    let mut entries: Vec<Entry> = Vec::with_capacity(lumps.len());
    let mut slots: Vec<Slot> = Vec::with_capacity(lumps.len());
    let mut groups: Vec<Group> = Vec::new();
    let mut outside = 0usize;
    let mut namespace: Option<Namespace> = None;
    let mut open_group: Option<usize> = None;
    let mut depth = 0u32;

    let push = |entries: &mut Vec<Entry>,
                slots: &mut Vec<Slot>,
                outside: &mut usize,
                i: usize,
                folder: &str,
                namespace: Option<Namespace>,
                forced: Option<DataKind>| {
        let l = &lumps[i];
        let end = l.offset.checked_add(l.size);
        let inside = l.offset >= 0 && l.size >= 0 && end.is_some_and(|e| e as u64 <= len);
        let size = if l.size < 0 { 0 } else { l.size as u64 };
        let data = if let Some(d) = forced {
            d
        } else if inside && sniff && size > 0 {
            let head = source.head(l.offset as u64, (size as usize).min(SNIFF_BYTES));
            crate::doom_lump(&l.name, namespace, &head, size)
        } else {
            crate::from_name(&l.name, namespace)
        };
        let path = if folder.is_empty() { l.name.clone() } else { format!("{folder}/{}", l.name) };
        let slot = if inside {
            Slot::Stored { offset: l.offset as u64, len: size }
        } else {
            *outside += 1;
            Slot::Locked("the lump lies outside the file".into())
        };
        entries.push(Entry {
            index: entries.len(),
            path,
            size,
            stored_size: size,
            kind: data.kind(namespace),
            data,
            namespace,
            locked: matches!(slot, Slot::Locked(_)),
        });
        slots.push(slot);
    };

    let mut i = 0usize;
    while i < lumps.len() {
        let upper = uppers[i].as_str();
        // A namespace marker opens or closes a region.  Inner markers (`SS_`,
        // `FF_`, `F1_` and so on) nest inside the outer pair, and a region
        // ends when every marker opened has been closed.
        if let Some((ns, open)) = marker(upper) {
            if open {
                if namespace == Some(ns) {
                    depth += 1;
                } else {
                    namespace = Some(ns);
                    depth = 1;
                    open_group = None;
                }
            } else if namespace == Some(ns) {
                depth = depth.saturating_sub(1);
            }
            let first = entries.len();
            push(&mut entries, &mut slots, &mut outside, i, ns.folder(), Some(ns), Some(DataKind::Marker));
            if open && open_group.is_none() {
                groups.push(Group {
                    path: ns.folder().to_string(),
                    kind: GroupKind::Namespace(ns),
                    first,
                    last: first,
                });
                open_group = Some(groups.len() - 1);
            } else if let Some(g) = open_group {
                groups[g].last = first;
            }
            if !open && depth == 0 && namespace == Some(ns) {
                namespace = None;
                open_group = None;
            }
            i += 1;
            continue;
        }
        // A map: the marker, then the lumps that belong to it.
        let next = uppers.get(i + 1).map(String::as_str).unwrap_or("");
        let is_map = namespace.is_none() && (next == "THINGS" || next == "TEXTMAP");
        let is_gl = namespace.is_none() && upper.starts_with("GL_") && next == "GL_VERT";
        if is_map || is_gl {
            let folder = lumps[i].name.clone();
            let first = entries.len();
            push(&mut entries, &mut slots, &mut outside, i, &folder, None, Some(DataKind::MapLump));
            let mut j = i + 1;
            while j < lumps.len() && MAP_LUMPS.contains(&uppers[j].as_str()) {
                push(&mut entries, &mut slots, &mut outside, j, &folder, None, Some(DataKind::MapLump));
                let ended = uppers[j] == "ENDMAP";
                j += 1;
                if ended {
                    break;
                }
            }
            groups.push(Group { path: folder, kind: GroupKind::Map, first, last: entries.len() - 1 });
            i = j;
            continue;
        }
        let folder = namespace.map_or("", |ns| ns.folder());
        push(&mut entries, &mut slots, &mut outside, i, folder, namespace, None);
        if let Some(g) = open_group {
            groups[g].last = entries.len() - 1;
        }
        i += 1;
    }
    let mut warnings = Vec::new();
    if outside > 0 {
        warnings.push(format!("{outside} lumps lie outside the file and can't be read"));
    }
    Ok(Parsed {
        kind: ArchiveKind::DoomWad,
        entries,
        slots,
        groups,
        warnings,
        label: if iwad { "Doom IWAD" } else { "Doom PWAD" }.into(),
        extra: Extra::None,
    })
}
