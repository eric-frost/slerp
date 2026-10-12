//! Pictures from retro computers, drawn the way the original screens
//! showed them.
//!
//! Most of these formats are raw dumps of screen memory with no magic
//! number, so they are recognised by extension plus exact size or a header
//! check (see [`sniff`]).  Each decoder turns a dump into an RGBA picture at
//! the pixel shape of the original monitor (see `pic`), and the result is an
//! ordinary [`Asset::Image`]: thumbnails and the viewer scale it with hard
//! pixel edges like any other texture.
//!
//! - Atari ST: DEGAS and DEGAS Elite (`.pi1`-`.pi3`, `.pc1`-`.pc3`),
//!   NEOchrome (`.neo`), Tiny Stuff (`.tn1`-`.tn3`, `.tny`).
//! - Commodore 64: Koala (`.koa`, `.kla`), Art Studio (`.art`, `.aas`),
//!   Doodle (`.dd`, `.jj`), Advanced Art Studio (`.ocp`), `.hir`, `.hpi`.
//! - ZX Spectrum `.scr`, Amstrad CPC `.scr`, MSX screens (`.sc2`, `.sc5`,
//!   `.sr5`, ...), Apple II `.hgr` and `.dhr`, Atari 8-bit `.mic`, `.gr9`,
//!   `.gr8`, BBC Micro `.bbc`.
//! - BBS ANSI art (`.ans` and any file ending in a SAUCE record).
//! - PICO-8 cartridges (`.p8`) and TIC-80 cartridges (`.tic`).
//! - Amiga IFF ILBM and PBM pictures.

mod ansi;
mod apple2;
mod atari8;
mod atari_st;
mod bbc;
mod c64;
mod cpc;
mod gif;
mod ilbm;
mod msx;
mod pic;
mod pico8;
mod spectrum;
mod tic80;

use slerp_scene::{Asset, Limits, LoadError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use ansi::{Sauce, VGA as VGA_PALETTE, cp437_to_string, sauce};
pub use c64::PALETTE as C64_PALETTE;
pub use pico8::PALETTE as PICO8_PALETTE;

/// Which retro format a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetroFormat {
    /// DEGAS and DEGAS Elite, plain or compressed.
    Degas,
    Neochrome,
    TinyStuff,
    C64(c64::Kind),
    Spectrum,
    Cpc,
    Msx(msx::Screen),
    AppleHires,
    AppleDoubleHires,
    Atari8(atari8::Mode),
    Bbc,
    Ansi,
    Pico8,
    Tic80,
    Ilbm,
}

impl RetroFormat {
    /// A name for messages.
    pub fn name(self) -> &'static str {
        match self {
            RetroFormat::Degas => "Atari ST DEGAS",
            RetroFormat::Neochrome => "Atari ST NEOchrome",
            RetroFormat::TinyStuff => "Atari ST Tiny Stuff",
            RetroFormat::C64(k) => k.name(),
            RetroFormat::Spectrum => "ZX Spectrum screen",
            RetroFormat::Cpc => "Amstrad CPC screen",
            RetroFormat::Msx(_) => "MSX screen",
            RetroFormat::AppleHires => "Apple II hi-res",
            RetroFormat::AppleDoubleHires => "Apple II double hi-res",
            RetroFormat::Atari8(_) => "Atari 8-bit screen",
            RetroFormat::Bbc => "BBC Micro screen",
            RetroFormat::Ansi => "ANSI art",
            RetroFormat::Pico8 => "PICO-8 cartridge",
            RetroFormat::Tic80 => "TIC-80 cartridge",
            RetroFormat::Ilbm => "Amiga IFF",
        }
    }
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default()
}

/// Recognises a retro picture from its name, size and first bytes.  The file
/// size comes from the file system, so `path` must exist; use
/// [`sniff_sized`] when it does not.
pub fn sniff(path: &Path, head: &[u8]) -> Option<RetroFormat> {
    if ilbm::sniff(head) {
        return Some(RetroFormat::Ilbm);
    }
    let ext = extension(path);
    if ext.is_empty() || ext.len() > 5 {
        return None;
    }
    // The size is only worth a system call for names we know.
    if !KNOWN.contains(&ext.as_str()) {
        return None;
    }
    let len = std::fs::metadata(path).ok()?.len();
    sniff_sized(&ext, head, len)
}

/// Extensions that [`sniff`] looks at.
const KNOWN: &[&str] = &[
    "pi1", "pi2", "pi3", "pc1", "pc2", "pc3", "neo", "tn1", "tn2", "tn3", "tny", "koa", "kla", "art", "aas", "dd",
    "jj", "ocp", "hir", "hpi", "scr", "sc2", "sr2", "sc5", "sr5", "sc6", "sr6", "sc7", "sr7", "sc8", "sr8", "sc12",
    "sr12", "s12", "hgr", "dhr", "mic", "gr15", "g15", "gr9", "gr8", "bbc", "ans", "p8", "tic",
];

/// [`sniff`] with the extension (lower case) and file size given.
pub fn sniff_sized(ext: &str, head: &[u8], len: u64) -> Option<RetroFormat> {
    let word = |at: usize| {
        u16::from_be_bytes([head.get(at).copied().unwrap_or(0xff), head.get(at + 1).copied().unwrap_or(0xff)])
    };
    let c64 = |k: c64::Kind| c64::plausible(k, len).then_some(RetroFormat::C64(k));
    match ext {
        "pi1" | "pi2" | "pi3" => {
            (32034..=32100).contains(&len).then_some(()).filter(|_| word(0) <= 2).map(|_| RetroFormat::Degas)
        }
        // DEGAS Elite writes compressed files with the top bit of the
        // resolution word set; some programs save plain pictures under the
        // compressed names.
        "pc1" | "pc2" | "pc3" => {
            (len >= 40 && (word(0) <= 2 || (0x8000..=0x8002).contains(&word(0)))).then_some(RetroFormat::Degas)
        }
        "neo" => (len == 32128 && word(0) == 0 && word(2) <= 2).then_some(RetroFormat::Neochrome),
        "tn1" | "tn2" | "tn3" | "tny" => {
            (len >= 40 && head.first().is_some_and(|&b| b <= 5)).then_some(RetroFormat::TinyStuff)
        }
        "koa" | "kla" => c64(c64::Kind::Koala),
        "art" | "aas" => {
            // Build engine tile sets share `.art`; they start with the
            // little-endian version number 1.
            (head.get(..4) != Some(&[1, 0, 0, 0])).then(|| c64(c64::Kind::ArtStudio)).flatten()
        }
        "dd" | "jj" => c64(c64::Kind::Doodle),
        "ocp" => c64(c64::Kind::AdvancedArtStudio),
        "hir" | "hpi" => c64(c64::Kind::Hires),
        "scr" => {
            if head.starts_with(b"MZ") {
                None
            } else if spectrum::plausible(len) {
                Some(RetroFormat::Spectrum)
            } else if cpc::plausible(len) {
                Some(RetroFormat::Cpc)
            } else {
                None
            }
        }
        "hgr" => apple2::plausible_hgr(len).then_some(RetroFormat::AppleHires),
        "dhr" => apple2::plausible_dhr(len).then_some(RetroFormat::AppleDoubleHires),
        "mic" | "gr15" | "g15" | "gr9" | "gr8" => {
            atari8::plausible(len).then(|| atari8::Mode::from_extension(ext)).flatten().map(RetroFormat::Atari8)
        }
        "bbc" => bbc::plausible(len, head).then_some(RetroFormat::Bbc),
        "ans" => (len <= ansi::MAX_BYTES && len > 0).then_some(RetroFormat::Ansi),
        "p8" => pico8::sniff(head).then_some(RetroFormat::Pico8),
        "tic" => tic80::sniff(head, len).then_some(RetroFormat::Tic80),
        _ => msx::Screen::from_extension(ext).filter(|&s| msx::plausible(s, len)).map(RetroFormat::Msx),
    }
}

/// Files that no format claimed by name but that end in a SAUCE record
/// describing text, which is ANSI art whatever it is called.  Reads the last
/// 128 bytes (and the comment block's marker) of the file.
pub fn sniff_by_content(path: &Path, head: &[u8]) -> Option<RetroFormat> {
    use std::io::{Read, Seek, SeekFrom};
    // A plain text file has no business here unless it carries escapes or
    // a SAUCE record, and a binary one never does.
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if !(129..=ansi::MAX_BYTES).contains(&len) {
        return None;
    }
    f.seek(SeekFrom::Start(len - 128)).ok()?;
    let mut tail = [0u8; 128];
    f.read_exact(&mut tail).ok()?;
    let s = ansi::sauce(&tail)?;
    // ASCII and ANSi text files are drawn as they are; only files with an
    // escape sequence are claimed, so a SAUCE-signed .txt keeps its text
    // preview where the SAUCE says plain ASCII.
    (ansi::is_text_sauce(&s) && (s.file_type != 0 || head.contains(&0x1b))).then_some(RetroFormat::Ansi)
}

/// A small file saved beside the picture with another extension (a CPC
/// `.pal`, an MSX `.pl5`), in either case; only regular files of at most a
/// few kilobytes are used.
fn companion(path: &Path, extensions: &[&str]) -> Option<PathBuf> {
    extensions
        .iter()
        .flat_map(|e| [e.to_string(), e.to_ascii_uppercase()])
        .map(|e| path.with_extension(e))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= 4096))
}

/// Decodes a file already read into memory.  `path` locates companion files
/// (a CPC picture's `.pal`).  With a texture size limit in `limits` (a
/// thumbnail) very long ANSI files give their top part only.
pub fn load(format: RetroFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let (container, picture): (&str, (pic::Pic, String)) = match format {
        RetroFormat::Degas => ("Atari ST DEGAS", atari_st::degas(bytes)?),
        RetroFormat::Neochrome => ("Atari ST NEOchrome", atari_st::neochrome(bytes)?),
        RetroFormat::TinyStuff => ("Atari ST Tiny Stuff", atari_st::tiny(bytes)?),
        RetroFormat::C64(k) => (k.name(), c64::decode(k, bytes)?),
        RetroFormat::Spectrum => ("ZX Spectrum SCR", spectrum::decode(bytes)?),
        RetroFormat::Cpc => {
            let pal = if limits.neighbours {
                companion(path, &["pal", "ink"]).and_then(|p| std::fs::read(p).ok())
            } else {
                None
            };
            ("Amstrad CPC SCR", cpc::decode(bytes, pal.as_deref())?)
        }
        RetroFormat::Msx(s) => {
            let pal = match msx::palette_extension(s) {
                Some(ext) if limits.neighbours => companion(path, &[ext]).and_then(|p| std::fs::read(p).ok()),
                _ => None,
            };
            ("MSX screen dump", msx::decode(s, bytes, pal.as_deref())?)
        }
        RetroFormat::AppleHires => ("Apple II hi-res", apple2::hgr(bytes)?),
        RetroFormat::AppleDoubleHires => ("Apple II double hi-res", apple2::dhgr(bytes)?),
        RetroFormat::Atari8(m) => ("Atari 8-bit screen", atari8::decode(m, bytes)?),
        RetroFormat::Bbc => ("BBC Micro screen", bbc::decode(bytes)?),
        RetroFormat::Ansi => ("ANSI art", ansi::decode(bytes, limits.max_texture_size.is_some())?),
        RetroFormat::Pico8 => ("PICO-8 cartridge", pico8::decode(bytes)?),
        RetroFormat::Tic80 => ("TIC-80 cartridge", tic80::decode(bytes)?),
        RetroFormat::Ilbm => return Ok(Asset::Image(Arc::new(ilbm::decode(bytes)?.0))),
    };
    let (pic, detail) = picture;
    pic::check_size(pic.width, pic.height)?;
    Ok(Asset::Image(Arc::new(pic.into_texture(container, &detail))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_and_size_pick_the_format() {
        let koala = vec![0u8; 10003];
        assert_eq!(sniff_sized("koa", &koala, 10003), Some(RetroFormat::C64(c64::Kind::Koala)));
        assert_eq!(sniff_sized("koa", &koala, 10005), Some(RetroFormat::C64(c64::Kind::Koala)));
        assert_eq!(sniff_sized("koa", &koala, 10006), None);
        assert_eq!(sniff_sized("scr", &[0; 8], 6912), Some(RetroFormat::Spectrum));
        assert_eq!(sniff_sized("scr", &[0; 8], 16384), Some(RetroFormat::Cpc));
        assert_eq!(sniff_sized("scr", &[0; 8], 12345), None);
        // A Windows screensaver is an executable.
        assert_eq!(sniff_sized("scr", b"MZ\x90\0", 6912), None);
        assert_eq!(sniff_sized("hgr", &[], 8192), Some(RetroFormat::AppleHires));
        assert_eq!(sniff_sized("dhr", &[], 16384), Some(RetroFormat::AppleDoubleHires));
        assert_eq!(sniff_sized("sr5", &[], 30000), Some(RetroFormat::Msx(msx::Screen::S5)));
        assert_eq!(sniff_sized("sr5", &[], 100), None);
        assert_eq!(sniff_sized("txt", &[], 6912), None);
        assert_eq!(sniff_sized("pi1", &[0, 0], 32034), Some(RetroFormat::Degas));
        assert_eq!(sniff_sized("pi1", &[0, 9], 32034), None);
        assert_eq!(sniff_sized("pc1", &[0x80, 0], 9000), Some(RetroFormat::Degas));
        assert_eq!(sniff_sized("neo", &[0, 0, 0, 1], 32128), Some(RetroFormat::Neochrome));
    }

    #[test]
    fn build_engine_tile_sets_are_not_art_studio_pictures() {
        // `.art` with Build's version-1 header is left to the tile reader.
        assert_eq!(sniff_sized("art", &[1, 0, 0, 0, 0, 0, 0, 0], 9009), None);
        assert_eq!(sniff_sized("art", &[0, 0x20, 0, 0], 9009), Some(RetroFormat::C64(c64::Kind::ArtStudio)));
    }

    #[test]
    fn text_carts_need_their_header() {
        assert_eq!(sniff_sized("p8", b"pico-8 cartridge // http://www.pico-8.com\n", 100), Some(RetroFormat::Pico8));
        assert_eq!(sniff_sized("p8", b"-----BEGIN PRIVATE KEY-----", 100), None);
        assert_eq!(sniff_sized("tic", b"\x01\x20\x00\x00", 40), Some(RetroFormat::Tic80));
        assert_eq!(sniff_sized("tic", b"hello this is not a cartridge", 40), None);
    }

    #[test]
    fn iff_is_recognised_by_magic() {
        let mut f = b"FORM\0\0\0\x04ILBM".to_vec();
        f.extend([0; 10]);
        assert_eq!(sniff(Path::new("x.bin"), &f), Some(RetroFormat::Ilbm));
        assert_eq!(sniff(Path::new("x.bin"), b"FORM\0\0\0\x04AIFF"), None);
    }

    #[test]
    fn loading_gives_a_texture_with_notes() {
        let mut f = vec![0u8; 6912];
        f[6144] = 0x47;
        let Asset::Image(t) = load(RetroFormat::Spectrum, Path::new("x.scr"), &f, &Limits::viewer()).unwrap() else {
            panic!("not an image")
        };
        assert_eq!((t.width(), t.height()), (256, 192));
        assert_eq!(t.source.container, "ZX Spectrum SCR");
        assert!(t.source.format.contains("standard screen"));
    }
}
