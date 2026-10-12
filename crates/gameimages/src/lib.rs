//! Game image formats other tools skip:
//!
//! - **XNB** (`.xnb`): the textures and sprite-font textures of XNA,
//!   MonoGame and FNA games (Stardew Valley, Celeste, ...), LZX and LZ4
//!   compressed or not.
//! - **BLP** (`.blp`): Blizzard's Warcraft III (BLP1) and World of Warcraft
//!   (BLP2) textures.
//! - **PVR** (`.pvr`) and **PKM** (`.pkm`): PowerVR and Ericsson containers
//!   for mobile texture formats (PVRTC, ETC1, ETC2, DXT and plain pixels).
//! - **ART** (`.art`): Build engine tile sets (Duke Nukem 3D, Shadow Warrior,
//!   Blood), a gallery of tiles drawn with the `PALETTE.DAT` beside the file.
//!
//! Every format is recognised by its magic number (ART by its version-1
//! header plus the extension) and decodes into the shared texture or gallery
//! types; thumbnails and the viewer then treat the result like any other
//! texture.

mod art;
mod blp;
mod common;
mod pvr;
mod xnb;

use slerp_scene::{Asset, Limits, LoadError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use art::palette_from_dat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameImageFormat {
    Xnb,
    Blp,
    Pvr,
    Pkm,
    BuildArt,
}

impl GameImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            GameImageFormat::Xnb => "XNA/MonoGame XNB",
            GameImageFormat::Blp => "Blizzard BLP",
            GameImageFormat::Pvr => "PowerVR PVR",
            GameImageFormat::Pkm => "Ericsson PKM",
            GameImageFormat::BuildArt => "Build engine ART",
        }
    }
}

/// Recognises a file from its first bytes (and, for `.art`, its name).
pub fn sniff(path: &Path, head: &[u8]) -> Option<GameImageFormat> {
    if xnb::sniff(head) {
        return Some(GameImageFormat::Xnb);
    }
    if blp::sniff(head) {
        return Some(GameImageFormat::Blp);
    }
    if pvr::sniff_pkm(head) {
        return Some(GameImageFormat::Pkm);
    }
    let pvr_name = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pvr"));
    if pvr::sniff_pvr(head, pvr_name) {
        return Some(GameImageFormat::Pvr);
    }
    let art_name = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("art"));
    (art_name && art::sniff(head)).then_some(GameImageFormat::BuildArt)
}

/// `PALETTE.DAT` beside a tile file, in whatever case the files were
/// unpacked in.
fn palette_beside(path: &Path) -> Option<PathBuf> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    ["PALETTE.DAT", "palette.dat", "Palette.dat", "PALETTE.dat"].iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

pub fn load(format: GameImageFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let want = limits.max_texture_size;
    let tex = match format {
        GameImageFormat::Xnb => xnb::decode(bytes)?,
        GameImageFormat::Blp => blp::decode(bytes, want)?,
        GameImageFormat::Pvr => pvr::decode_pvr(bytes)?,
        GameImageFormat::Pkm => pvr::decode_pkm(bytes)?,
        GameImageFormat::BuildArt => {
            let dat = if limits.neighbours { palette_beside(path).and_then(|p| std::fs::read(p).ok()) } else { None };
            return Ok(Asset::Gallery(art::decode(bytes, dat.as_deref())?));
        }
    };
    Ok(Asset::Image(Arc::new(tex)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_told_apart_by_magic() {
        assert_eq!(sniff(Path::new("a.xnb"), b"XNBw\x04\x00\x10\x00\x00\x00"), Some(GameImageFormat::Xnb));
        assert_eq!(sniff(Path::new("a"), b"BLP2\x01\x00\x00\x00"), Some(GameImageFormat::Blp));
        assert_eq!(
            sniff(Path::new("a"), b"PKM 10\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"),
            Some(GameImageFormat::Pkm)
        );
        assert_eq!(sniff(Path::new("a.pvr"), b"PVR\x03\x00\x00\x00\x00"), Some(GameImageFormat::Pvr));
        let art = [1u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(sniff(Path::new("TILES000.ART"), &art), Some(GameImageFormat::BuildArt));
        // Version-1-looking bytes in some other file are not tiles.
        assert_eq!(sniff(Path::new("data.bin"), &art), None);
        assert_eq!(sniff(Path::new("a.txt"), b"hello world, not an image"), None);
    }
}
