//! Builders for small archives of every format, for this crate's tests and
//! the tests of crates that read archives.  They write what the formats'
//! documentation says and nothing more, so a reader that accepts their
//! output and a real file's is reading the format rather than one tool's
//! habits.

use std::path::{Path, PathBuf};

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// How [`zip_with`] writes.
#[derive(Clone, Copy, Default)]
pub struct ZipOptions {
    /// Deflate every entry (stored otherwise).
    pub deflate: bool,
    /// Write zip64 end records and extra fields.
    pub zip64: bool,
    /// Bytes in front of the archive, as a self-extractor has.
    pub junk_prefix: usize,
}

/// A stored-only zip with the given files.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    zip_with(files, ZipOptions::default())
}

pub fn zip_with(files: &[(&str, &[u8])], options: ZipOptions) -> Vec<u8> {
    let mut out = vec![0xAAu8; options.junk_prefix];
    // A concatenated or self-extracting zip still starts like a zip.
    if options.junk_prefix >= 4 {
        out[..4].copy_from_slice(b"PK\x03\x04");
    }
    let mut dir = Vec::new();
    for (name, data) in files {
        let offset = (out.len() - options.junk_prefix) as u64;
        let (method, packed): (u16, Vec<u8>) = if options.deflate && !data.is_empty() {
            (8, miniz_oxide::deflate::compress_to_vec(data, 6))
        } else {
            (0, data.to_vec())
        };
        out.extend_from_slice(b"PK\x03\x04");
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, method);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le32(&mut out, packed.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);

        dir.extend_from_slice(b"PK\x01\x02");
        le16(&mut dir, 20);
        le16(&mut dir, 20);
        le16(&mut dir, 0);
        le16(&mut dir, method);
        le32(&mut dir, 0);
        le32(&mut dir, 0);
        let extra: Vec<u8> = if options.zip64 {
            let mut e = Vec::new();
            le16(&mut e, 1);
            le16(&mut e, 24);
            le64(&mut e, data.len() as u64);
            le64(&mut e, packed.len() as u64);
            le64(&mut e, offset);
            e
        } else {
            Vec::new()
        };
        if options.zip64 {
            le32(&mut dir, 0xFFFF_FFFF);
            le32(&mut dir, 0xFFFF_FFFF);
        } else {
            le32(&mut dir, packed.len() as u32);
            le32(&mut dir, data.len() as u32);
        }
        le16(&mut dir, name.len() as u16);
        le16(&mut dir, extra.len() as u16);
        le16(&mut dir, 0);
        le16(&mut dir, 0);
        le16(&mut dir, 0);
        le32(&mut dir, 0);
        le32(&mut dir, if options.zip64 { 0xFFFF_FFFF } else { offset as u32 });
        dir.extend_from_slice(name.as_bytes());
        dir.extend_from_slice(&extra);
    }
    let dir_at = (out.len() - options.junk_prefix) as u64;
    out.extend_from_slice(&dir);
    if options.zip64 {
        let record_at = (out.len() - options.junk_prefix) as u64;
        out.extend_from_slice(b"PK\x06\x06");
        le64(&mut out, 44);
        le16(&mut out, 45);
        le16(&mut out, 45);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le64(&mut out, files.len() as u64);
        le64(&mut out, files.len() as u64);
        le64(&mut out, dir.len() as u64);
        le64(&mut out, dir_at);
        out.extend_from_slice(b"PK\x06\x07");
        le32(&mut out, 0);
        le64(&mut out, record_at + options.junk_prefix as u64);
        le32(&mut out, 1);
    }
    out.extend_from_slice(b"PK\x05\x06");
    le16(&mut out, 0);
    le16(&mut out, 0);
    if options.zip64 {
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le32(&mut out, 0xFFFF_FFFF);
        le32(&mut out, 0xFFFF_FFFF);
    } else {
        le16(&mut out, files.len() as u16);
        le16(&mut out, files.len() as u16);
        le32(&mut out, dir.len() as u32);
        le32(&mut out, dir_at as u32);
    }
    le16(&mut out, 0);
    out
}

/// A Doom WAD with the lumps in order.  A lump named like a marker is
/// written with its data as given (normally empty).
pub fn wad(iwad: bool, lumps: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(if iwad { b"IWAD" } else { b"PWAD" });
    le32(&mut out, lumps.len() as u32);
    le32(&mut out, 0);
    let mut table = Vec::new();
    for (name, data) in lumps {
        le32(&mut table, if data.is_empty() { 0 } else { out.len() as u32 });
        le32(&mut table, data.len() as u32);
        let mut n = [0u8; 8];
        n[..name.len().min(8)].copy_from_slice(&name.as_bytes()[..name.len().min(8)]);
        table.extend_from_slice(&n);
        out.extend_from_slice(data);
    }
    let dir = out.len() as u32;
    out[8..12].copy_from_slice(&dir.to_le_bytes());
    out.extend_from_slice(&table);
    out
}

/// A Quake PAK.
pub fn pak(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    le32(&mut out, 0);
    le32(&mut out, (files.len() * 64) as u32);
    let mut table = Vec::new();
    for (name, data) in files {
        let mut n = [0u8; 56];
        n[..name.len().min(55)].copy_from_slice(&name.as_bytes()[..name.len().min(55)]);
        table.extend_from_slice(&n);
        le32(&mut table, out.len() as u32);
        le32(&mut table, data.len() as u32);
        out.extend_from_slice(data);
    }
    let dir = out.len() as u32;
    out[4..8].copy_from_slice(&dir.to_le_bytes());
    out.extend_from_slice(&table);
    out
}

/// A WAD2 (or, with `half_life`, a WAD3) of (name, type byte, bytes).
pub fn wad2(half_life: bool, lumps: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(if half_life { b"WAD3" } else { b"WAD2" });
    le32(&mut out, lumps.len() as u32);
    le32(&mut out, 0);
    let mut table = Vec::new();
    for (name, kind, data) in lumps {
        le32(&mut table, out.len() as u32);
        le32(&mut table, data.len() as u32);
        le32(&mut table, data.len() as u32);
        table.extend_from_slice(&[*kind, 0, 0, 0]);
        let mut n = [0u8; 16];
        n[..name.len().min(15)].copy_from_slice(&name.as_bytes()[..name.len().min(15)]);
        table.extend_from_slice(&n);
        out.extend_from_slice(data);
    }
    let dir = out.len() as u32;
    out[8..12].copy_from_slice(&dir.to_le_bytes());
    out.extend_from_slice(&table);
    out
}

/// A Build GRP.
pub fn grp(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"KenSilverman");
    le32(&mut out, files.len() as u32);
    for (name, data) in files {
        let mut n = [0u8; 12];
        n[..name.len().min(12)].copy_from_slice(&name.as_bytes()[..name.len().min(12)]);
        out.extend_from_slice(&n);
        le32(&mut out, data.len() as u32);
    }
    for (_, data) in files {
        out.extend_from_slice(data);
    }
    out
}

/// A Godot PCK of the given format version (1 to 4; 3 and 4 share a layout).  Paths get the
/// `res://` prefix, as Godot writes them.
pub fn pck(version: u32, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut head = Vec::new();
    head.extend_from_slice(b"GDPC");
    le32(&mut head, version);
    le32(&mut head, if version == 1 { 3 } else { 4 });
    le32(&mut head, if version == 3 { 4 } else { 2 });
    le32(&mut head, 0);
    let dir_at = match version {
        1 => 20 + 64,
        2 => 20 + 12 + 64,
        _ => 20 + 20 + 64,
    } as u64;
    // The directory first, then the files' data.
    let mut records = Vec::new();
    le32(&mut records, files.len() as u32);
    let mut dir_len = 4u64;
    for (name, _) in files {
        let path = format!("res://{name}");
        let padded = path.len() + (4 - path.len() % 4) % 4;
        dir_len += 4 + padded as u64 + 16 + 16 + if version >= 2 { 4 } else { 0 };
    }
    let mut data_at = dir_at + dir_len;
    let mut data = Vec::new();
    for (name, bytes) in files {
        let path = format!("res://{name}");
        let padded = path.len() + (4 - path.len() % 4) % 4;
        le32(&mut records, padded as u32);
        records.extend_from_slice(path.as_bytes());
        records.extend(std::iter::repeat_n(0u8, padded - path.len()));
        le64(&mut records, data_at);
        le64(&mut records, bytes.len() as u64);
        records.extend_from_slice(&[0u8; 16]);
        if version >= 2 {
            le32(&mut records, 0);
        }
        data_at += bytes.len() as u64;
        data.extend_from_slice(bytes);
    }
    let mut out = head;
    match version {
        1 => {}
        2 => {
            le32(&mut out, 0);
            le64(&mut out, 0);
        }
        _ => {
            le32(&mut out, 0);
            le64(&mut out, 0);
            le64(&mut out, dir_at);
        }
    }
    out.extend_from_slice(&[0u8; 64]);
    assert_eq!(out.len() as u64, dir_at, "PCK header size");
    out.extend_from_slice(&records);
    out.extend_from_slice(&data);
    out
}

/// Writes a VPK of the given version (1 or 2) as `pak01_dir.vpk` (and
/// `pak01_000.vpk`) in `dir`: `small` files are stored in the directory file,
/// half of each as preload bytes, `big` ones in archive 0.
pub fn vpk(dir: &Path, version: u32, small: &[(&str, &[u8])], big: &[(&str, &[u8])]) -> PathBuf {
    let mut tree = Vec::new();
    let mut dir_data = Vec::new();
    let mut archive = Vec::new();
    // Files by extension: folder, name, bytes, and whether they live in the
    // directory file.
    type Files = Vec<(String, String, Vec<u8>, bool)>;
    let mut by_ext: std::collections::BTreeMap<String, Files> = Default::default();
    for (path, data, in_dir) in small.iter().map(|(p, d)| (p, d, true)).chain(big.iter().map(|(p, d)| (p, d, false))) {
        let (folder, file) = path.rsplit_once('/').unwrap_or((" ", path));
        let (name, ext) = file.rsplit_once('.').unwrap_or((file, " "));
        by_ext.entry(ext.to_string()).or_default().push((folder.to_string(), name.to_string(), data.to_vec(), in_dir));
    }
    for (ext, files) in by_ext {
        tree.extend_from_slice(ext.as_bytes());
        tree.push(0);
        // Folders in groups, as Valve's tool writes them.
        let mut folders: std::collections::BTreeMap<String, Files> = Default::default();
        for f in files {
            folders.entry(f.0.clone()).or_default().push(f);
        }
        for (folder, files) in folders {
            tree.extend_from_slice(folder.as_bytes());
            tree.push(0);
            for (_, name, data, in_dir) in files {
                tree.extend_from_slice(name.as_bytes());
                tree.push(0);
                let (preload, rest) = if in_dir { data.split_at(data.len() / 2) } else { (&data[..0], &data[..]) };
                le32(&mut tree, 0);
                le16(&mut tree, preload.len() as u16);
                let (part, offset) =
                    if in_dir { (0x7fffu16, dir_data.len() as u32) } else { (0u16, archive.len() as u32) };
                le16(&mut tree, part);
                le32(&mut tree, offset);
                le32(&mut tree, rest.len() as u32);
                le16(&mut tree, 0xffff);
                tree.extend_from_slice(preload);
                if in_dir {
                    dir_data.extend_from_slice(rest);
                } else {
                    archive.extend_from_slice(rest);
                }
            }
            // The end of this folder's files.
            tree.push(0);
        }
        // The end of this extension's folders.
        tree.push(0);
    }
    // The end of the extensions.
    tree.push(0);
    let mut out = Vec::new();
    le32(&mut out, 0x55aa_1234);
    le32(&mut out, version);
    le32(&mut out, tree.len() as u32);
    if version == 2 {
        out.extend_from_slice(&[0; 16]);
    }
    out.extend_from_slice(&tree);
    out.extend_from_slice(&dir_data);
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("pak01_dir.vpk");
    std::fs::write(&path, out).unwrap();
    std::fs::write(dir.join("pak01_000.vpk"), archive).unwrap();
    path
}

/// A Doom picture of `width` x `height` pixels in one column per pixel
/// column, each a single post holding the `pixels` (indices, row-major).
pub fn doom_picture(width: u16, height: u16, left: i16, top: i16, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), width as usize * height as usize);
    let mut out = Vec::new();
    le16(&mut out, width);
    le16(&mut out, height);
    out.extend_from_slice(&left.to_le_bytes());
    out.extend_from_slice(&top.to_le_bytes());
    let table = out.len();
    out.extend(std::iter::repeat_n(0u8, 4 * width as usize));
    for x in 0..width as usize {
        let at = out.len() as u32;
        out[table + 4 * x..table + 4 * x + 4].copy_from_slice(&at.to_le_bytes());
        out.push(0);
        out.push(height as u8);
        out.push(0);
        for y in 0..height as usize {
            out.push(pixels[y * width as usize + x]);
        }
        out.push(0);
        out.push(0xFF);
    }
    out
}

/// A tar of the given files (ustar headers, data padded to 512 bytes, two
/// empty blocks at the end).
pub fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in files {
        let mut h = [0u8; 512];
        let n = name.len().min(100);
        h[..n].copy_from_slice(&name.as_bytes()[..n]);
        h[100..107].copy_from_slice(b"0000644");
        let size = format!("{:011o}", data.len());
        h[124..135].copy_from_slice(size.as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = if name.ends_with('/') { b'5' } else { b'0' };
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// `data` in a gzip member (the CRC is left zero, which readers here don't
/// check).
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0xFF];
    out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, 6));
    le32(&mut out, 0);
    le32(&mut out, data.len() as u32);
    out
}

/// A Unity package: for each asset its GUID folder with `asset`,
/// `asset.meta`, `pathname` and, when given, `preview.png`.
/// One asset of [`unitypackage`]: GUID, path, data and preview picture.
pub type UnityAsset<'a> = (&'a str, &'a str, &'a [u8], Option<&'a [u8]>);

pub fn unitypackage(assets: &[UnityAsset]) -> Vec<u8> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for (guid, path, data, preview) in assets {
        files.push((format!("{guid}/"), Vec::new()));
        files.push((format!("{guid}/asset"), data.to_vec()));
        files.push((format!("{guid}/asset.meta"), b"fileFormatVersion: 2\n".to_vec()));
        files.push((format!("{guid}/pathname"), format!("{path}\n00").into_bytes()));
        if let Some(p) = preview {
            files.push((format!("{guid}/preview.png"), p.to_vec()));
        }
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    gzip(&tar(&refs))
}
