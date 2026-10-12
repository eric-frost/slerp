//! Which MIME types KDE's own thumbnail plugins claim.  Dolphin asks KIO for
//! previews, and KIO lists its compiled plugins (`kf6/thumbcreator/*.so`)
//! before freedesktop `.thumbnailer` files, giving each MIME type to the
//! first enabled plugin that claims it exactly; so for a type a KDE plugin
//! claims, Slerp's thumbnailer is never asked, whatever Dolphin's settings
//! say about Slerp.
//!
//! The plugins carry their MIME types in Qt's plugin metadata: an ELF note
//! named `qt-project!` holding a CBOR map, whose key 4 is the plugin's JSON
//! metadata (`KPlugin.MimeTypes`).  Reading it here needs no Qt and doesn't
//! load the plugins.

use std::path::{Path, PathBuf};

/// A compiled KDE thumbnail plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KdePlugin {
    /// Its id in Dolphin's `[PreviewSettings] Plugins` list.
    pub id: String,
    /// What Dolphin's preview settings call it.
    pub name: String,
    pub mime_types: Vec<String>,
}

/// Every compiled thumbnail plugin in the Qt plugin folders.
pub fn plugins(dirs: &[PathBuf]) -> Vec<KdePlugin> {
    let mut out: Vec<KdePlugin> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("kf6/thumbcreator")) else { continue };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "so")) {
            if let Some(p) = read_plugin(path)
                && !out.iter().any(|o| o.id == p.id)
            {
                out.push(p);
            }
        }
    }
    out
}

/// Where Qt keeps plugins: `QT_PLUGIN_PATH`, then the usual install
/// folders of distributions.
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        std::env::var_os("QT_PLUGIN_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    for d in [
        "/usr/lib/x86_64-linux-gnu/qt6/plugins",
        "/usr/lib/aarch64-linux-gnu/qt6/plugins",
        "/usr/lib/qt6/plugins",
        "/usr/lib64/qt6/plugins",
        "/usr/local/lib/qt6/plugins",
    ] {
        let p = PathBuf::from(d);
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// One plugin's id, name and MIME types, from its metadata note.
pub fn read_plugin(path: &Path) -> Option<KdePlugin> {
    // Plugins are a few hundred kilobytes; the note sits near the start.
    let bytes = std::fs::read(path).ok().filter(|b| b.len() < 64 << 20)?;
    let at = find(&bytes, b"qt-project!\0")?;
    // A 4-byte header (format version, Qt major and minor, architecture
    // flags), then CBOR.
    let start = (at + 12).next_multiple_of(4) + 4;
    let value = Cbor::parse(bytes.get(start..)?)?;
    let meta = value.get_int(4)?;
    let kplugin = meta.get("KPlugin")?;
    let mime_types = kplugin
        .get("MimeTypes")
        .map(|m| match m {
            Cbor::Array(items) => items
                .iter()
                .filter_map(Cbor::text)
                .flat_map(|t| {
                    t.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect::<Vec<_>>()
                })
                .collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let id = kplugin.get("Id").and_then(Cbor::text).map(str::to_owned).unwrap_or(stem);
    let name = kplugin.get("Name").and_then(Cbor::text).unwrap_or(&id).to_owned();
    let mut unique: Vec<String> = Vec::new();
    for m in mime_types {
        if !unique.contains(&m) {
            unique.push(m);
        }
    }
    Some(KdePlugin { id, name, mime_types: unique })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/*┌─────────────────╮
| CBOR            |
└─────────────────╯*/
//region CBOR

/// The parts of CBOR (RFC 8949) Qt's plugin metadata uses.
#[derive(Debug, PartialEq)]
enum Cbor {
    Int(i128),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Simple,
}

impl Cbor {
    fn parse(bytes: &[u8]) -> Option<Cbor> {
        let mut r = Reader { bytes, at: 0, depth: 0 };
        r.value()
    }

    fn text(&self) -> Option<&str> {
        match self {
            Cbor::Text(t) => Some(t),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&Cbor> {
        match self {
            Cbor::Map(entries) => entries.iter().find(|(k, _)| k.text() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    fn get_int(&self, key: i128) -> Option<&Cbor> {
        match self {
            Cbor::Map(entries) => entries.iter().find(|(k, _)| *k == Cbor::Int(key)).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// A bounds-checked reader; a malformed or hostile note gives `None`.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: u32,
}

/// What a head's length field says: a count, or "until the break byte".
enum Len {
    Known(u64),
    Indefinite,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn take(&mut self, n: u64) -> Option<&[u8]> {
        let n = usize::try_from(n).ok()?;
        let end = self.at.checked_add(n)?;
        let s = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn uint(&mut self, info: u8) -> Option<Len> {
        Some(Len::Known(match info {
            0..=23 => u64::from(info),
            24 => u64::from(self.byte()?),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().ok()?)),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().ok()?)),
            27 => u64::from_be_bytes(self.take(8)?.try_into().ok()?),
            31 => return Some(Len::Indefinite),
            _ => return None,
        }))
    }

    fn at_break(&mut self) -> bool {
        if self.bytes.get(self.at) == Some(&0xff) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Cbor> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let head = self.byte()?;
        let (major, info) = (head >> 5, head & 0x1f);
        let len = self.uint(info)?;
        let out = match (major, len) {
            (0, Len::Known(n)) => Cbor::Int(i128::from(n)),
            (1, Len::Known(n)) => Cbor::Int(-1 - i128::from(n)),
            (2 | 3, Len::Known(n)) => {
                let b = self.take(n)?.to_vec();
                if major == 3 { Cbor::Text(String::from_utf8(b).ok()?) } else { Cbor::Bytes(b) }
            }
            (2 | 3, Len::Indefinite) => {
                let mut all = Vec::new();
                while !self.at_break() {
                    match self.value()? {
                        Cbor::Bytes(b) => all.extend(b),
                        Cbor::Text(t) => all.extend(t.into_bytes()),
                        _ => return None,
                    }
                }
                if major == 3 { Cbor::Text(String::from_utf8(all).ok()?) } else { Cbor::Bytes(all) }
            }
            (4, len) => {
                let mut items = Vec::new();
                match len {
                    Len::Known(n) => {
                        for _ in 0..n.min(1 << 16) {
                            items.push(self.value()?);
                        }
                    }
                    Len::Indefinite => {
                        while !self.at_break() {
                            items.push(self.value()?);
                        }
                    }
                }
                Cbor::Array(items)
            }
            (5, len) => {
                let mut entries = Vec::new();
                match len {
                    Len::Known(n) => {
                        for _ in 0..n.min(1 << 16) {
                            entries.push((self.value()?, self.value()?));
                        }
                    }
                    Len::Indefinite => {
                        while !self.at_break() {
                            entries.push((self.value()?, self.value()?));
                        }
                    }
                }
                Cbor::Map(entries)
            }
            // A tag: the tagged value stands for itself.
            (6, Len::Known(_)) => self.value()?,
            // Simple values and floats (the length read already skipped
            // their bytes).
            (7, Len::Known(_)) => Cbor::Simple,
            _ => return None,
        };
        self.depth -= 1;
        Some(out)
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_qt_metadata_shaped_cbor() {
        // {2: "iid", 4: {"KPlugin": {"Id": "x", "MimeTypes": ["a/b", "c/d;"]}}}
        let mut cbor = vec![0xbf, 0x02, 0x63];
        cbor.extend(b"iid");
        cbor.extend([0x04, 0xa1, 0x67]);
        cbor.extend(b"KPlugin");
        cbor.extend([0xa2, 0x62]);
        cbor.extend(b"Id");
        cbor.extend([0x61, b'x', 0x69]);
        cbor.extend(b"MimeTypes");
        cbor.extend([0x82, 0x63]);
        cbor.extend(b"a/b");
        cbor.extend([0x64]);
        cbor.extend(b"c/d;");
        cbor.push(0xff);
        let mut file = vec![0u8; 20];
        file.extend(b"qt-project!\0\x01\x06\x0a\x01");
        file.extend(&cbor);
        let dir = std::env::temp_dir().join(format!("slerp-kio-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("kf6/thumbcreator")).unwrap();
        std::fs::write(dir.join("kf6/thumbcreator/fake.so"), &file).unwrap();
        let found = plugins(std::slice::from_ref(&dir));
        assert_eq!(
            found,
            [KdePlugin { id: "x".into(), name: "x".into(), mime_types: vec!["a/b".into(), "c/d".into()] }]
        );
        // Cut short anywhere: no plugin, no panic.
        for n in 0..file.len() {
            std::fs::write(dir.join("kf6/thumbcreator/fake.so"), &file[..n]).unwrap();
            let _ = plugins(std::slice::from_ref(&dir));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_this_machines_plugins() {
        let found = plugins(&plugin_dirs());
        let Some(image) = found.iter().find(|p| p.id == "imagethumbnail") else { return };
        assert!(image.mime_types.iter().any(|m| m == "image/png"), "{image:?}");
        if let Some(blender) = found.iter().find(|p| p.id == "blenderthumbnail") {
            assert_eq!(blender.mime_types, ["application/x-blender"]);
        }
    }
}
