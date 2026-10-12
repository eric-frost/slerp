//! The picture a VRM avatar carries of itself: VRM 0.x's `meta.texture`
//! (a texture index) and VRM 1.0's `meta.thumbnailImage` (an image index),
//! which avatar tools and VRoid Hub show as the model's portrait.  Plain
//! glTF has no such field.
//!
//! Only the GLB's JSON chunk and the picture's bytes are read, so a 20 MB
//! avatar costs a few hundred kilobytes.

use base64::Engine;
use serde_json::Value;
use slerp_scene::LoadError;
use slerp_scene::textures::{DecodeOptions, Texture};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The largest JSON chunk read, and the largest picture.
const MAX_JSON_BYTES: u64 = 64 << 20;
const MAX_PICTURE_BYTES: u64 = 32 << 20;

/// Reads the avatar picture of the GLB or VRM file at `path`, or `None` for
/// files without one (every non-VRM glTF).
pub fn read(path: &Path, max_side: Option<u32>) -> Result<Option<Texture>, LoadError> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 20];
    if f.read_exact(&mut head).is_err() || !head.starts_with(b"glTF") || &head[16..20] != b"JSON" {
        return Ok(None);
    }
    let json_len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as u64;
    if json_len > MAX_JSON_BYTES {
        return Ok(None);
    }
    let mut json = vec![0u8; json_len as usize];
    f.read_exact(&mut json)?;
    // Most glTF files aren't avatars; don't parse their JSON.
    if !contains(&json, b"\"VRM") {
        return Ok(None);
    }
    let Ok(root) = serde_json::from_slice::<Value>(&json) else { return Ok(None) };
    let Some((image, version)) = thumbnail_image(&root) else { return Ok(None) };
    let image = &root["images"][image];
    let bytes = if let Some(uri) = image["uri"].as_str() {
        let Some(data) = uri.strip_prefix("data:").and_then(|d| d.split_once(";base64,")).map(|(_, b)| b) else {
            return Ok(None);
        };
        match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        }
    } else {
        let Some(view) = image["bufferView"].as_u64().map(|v| &root["bufferViews"][v as usize]) else {
            return Ok(None);
        };
        if view["buffer"].as_u64().unwrap_or(0) != 0 {
            return Ok(None);
        }
        let offset = view["byteOffset"].as_u64().unwrap_or(0);
        let Some(length) = view["byteLength"].as_u64().filter(|&l| l <= MAX_PICTURE_BYTES) else { return Ok(None) };
        // The BIN chunk follows the JSON chunk, padded to four bytes.
        let bin = 20 + json_len.div_ceil(4) * 4;
        let mut chunk = [0u8; 8];
        f.seek(SeekFrom::Start(bin))?;
        if f.read_exact(&mut chunk).is_err() || &chunk[4..8] != b"BIN\0" {
            return Ok(None);
        }
        let bin_len = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64;
        if offset.checked_add(length).is_none_or(|end| end > bin_len) {
            return Ok(None);
        }
        let mut b = vec![0u8; length as usize];
        f.seek(SeekFrom::Start(bin + 8 + offset))?;
        f.read_exact(&mut b)?;
        b
    };
    let opts = max_side.map_or_else(DecodeOptions::full, DecodeOptions::preview);
    let Ok(mut t) = slerp_scene::textures::decode(&bytes, &opts) else { return Ok(None) };
    t.source.container = "VRM".into();
    t.source.format = format!("the avatar's thumbnail (VRM {version})");
    Ok(Some(t))
}

/// The image index of the avatar's thumbnail, with the VRM version naming it.
fn thumbnail_image(root: &Value) -> Option<(usize, &'static str)> {
    let ext = &root["extensions"];
    if let Some(i) = ext["VRMC_vrm"]["meta"]["thumbnailImage"].as_u64() {
        return Some((i as usize, "1.0"));
    }
    let texture = ext["VRM"]["meta"]["texture"].as_i64().filter(|&t| t >= 0)?;
    let source = root["textures"][texture as usize]["source"].as_u64()?;
    Some((source as usize, "0.x"))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every avatar in the corpus that names a thumbnail gives a picture, and
    /// those that don't give none.
    #[test]
    fn corpus_avatars() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/vrm");
        let cases = [
            ("AliciaSolid_vrm-0.51.vrm", true),
            ("AvatarSample_A.vrm", true),
            ("AvatarSample_B.vrm", true),
            ("Seed-san.vrm", true),
            ("VRM1_Constraint_Twist_Sample.vrm", true),
            ("EmissionMigration_v0.106.0.vrm", false),
            ("VRMC_materials_mtoon_UV_Animation_Test.vrm", false),
        ];
        for (name, has) in cases {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let t = read(&path, Some(256)).unwrap();
            assert_eq!(t.is_some(), has, "{name}");
            if let Some(t) = t {
                assert!(t.width().max(t.height()) >= 64, "{name}: {}x{}", t.width(), t.height());
            }
        }
    }

    #[test]
    fn damaged_files_give_none() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/vrm/AliciaSolid_vrm-0.51.vrm");
        let Ok(bytes) = std::fs::read(&path) else { return };
        let dir = std::env::temp_dir().join(format!("slerp-vrm-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cut.vrm");
        for cut in [0, 12, 20, 100, 5000, bytes.len() / 3, bytes.len() - 10] {
            std::fs::write(&p, &bytes[..cut]).unwrap();
            let _ = read(&p, Some(128));
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
