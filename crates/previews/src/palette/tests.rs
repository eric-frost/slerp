//! Palette tests: small files built here for every format and feature,
//! damaged copies of them, and the real samples in
//! `local/assets/editors-extra/palettes/` when they are there (sources and
//! licences in each folder's SOURCES.md).

use super::*;
use crate::zip::tests::stored_zip;
use std::path::PathBuf;

fn read(format: PaletteFormat, bytes: &[u8]) -> Result<Palette, LoadError> {
    read_source(format, &mut Source::memory(bytes.to_vec()), &Limits::viewer())
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(|u| u.to_be_bytes()).collect()
}

/*┌─────────────────╮
| Sample files    |
└─────────────────╯*/
//region Sample files

fn ase_block(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_be_bytes().to_vec();
    b.extend_from_slice(&(body.len() as u32).to_be_bytes());
    b.extend_from_slice(body);
    b
}

fn ase_name(name: &str) -> Vec<u8> {
    let mut b = ((name.encode_utf16().count() + 1) as u16).to_be_bytes().to_vec();
    b.extend(utf16(name));
    b
}

fn ase_color(name: &str, model: &[u8; 4], values: &[f32], kind: u16) -> Vec<u8> {
    let mut body = ase_name(name);
    body.extend_from_slice(model);
    for v in values {
        body.extend_from_slice(&v.to_be_bytes());
    }
    body.extend_from_slice(&kind.to_be_bytes());
    ase_block(1, &body)
}

/// An ASE file with a loose colour, then a group of RGB, CMYK, Lab and
/// grey colours, then another loose one.
fn sample_ase() -> Vec<u8> {
    let blocks = [
        ase_color("Loose red", b"RGB ", &[1.0, 0.0, 0.0], 2),
        ase_block(0xC001, &ase_name("Brand")),
        ase_color("Spot cyan", b"CMYK", &[1.0, 0.0, 0.0, 0.0], 1),
        ase_color("Lab grey", b"LAB ", &[0.5, 0.0, 0.0], 0),
        ase_color("Half grey", b"Gray", &[0.5], 2),
        ase_block(0xC002, &[]),
        ase_color("After", b"RGB ", &[0.0, 0.0, 1.0], 2),
    ];
    let mut b = b"ASEF\x00\x01\x00\x00".to_vec();
    b.extend_from_slice(&(blocks.len() as u32).to_be_bytes());
    b.extend(blocks.concat());
    b
}

/// An ACO file with a version 1 section and the same colours named in
/// version 2: RGB, HSB, CMYK, Lab and grey.
fn sample_aco() -> Vec<u8> {
    let colors: [(u16, [u16; 4], &str); 5] = [
        (0, [65535, 0, 0, 0], "Red"),
        (1, [0, 65535, 65535, 0], "Hue zero"),
        (2, [65535, 0, 65535, 65535], "Magenta ink"),
        (7, [5000, 0, 0, 0], "Mid Lab"),
        (8, [2500, 0, 0, 0], "Light grey"),
    ];
    let mut b = Vec::new();
    for version in [1u16, 2] {
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&(colors.len() as u16).to_be_bytes());
        for (space, v, name) in &colors {
            b.extend_from_slice(&space.to_be_bytes());
            for x in v {
                b.extend_from_slice(&x.to_be_bytes());
            }
            if version == 2 {
                b.extend_from_slice(&((name.len() + 1) as u32).to_be_bytes());
                b.extend(utf16(name));
            }
        }
    }
    b
}

fn sample_act(extra: Option<(u16, u16)>) -> Vec<u8> {
    let mut b: Vec<u8> = (0..256).flat_map(|i| [i as u8, 255 - i as u8, 7]).collect();
    if let Some((count, transparent)) = extra {
        b.extend_from_slice(&count.to_be_bytes());
        b.extend_from_slice(&transparent.to_be_bytes());
    }
    b
}

fn sample_riff() -> Vec<u8> {
    let mut data = 0x300u16.to_le_bytes().to_vec();
    data.extend_from_slice(&3u16.to_le_bytes());
    data.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0]);
    let mut b = b"RIFF".to_vec();
    b.extend_from_slice(&((4 + 8 + data.len()) as u32).to_le_bytes());
    b.extend_from_slice(b"PAL data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}

const GPL: &str = "GIMP Palette\nName: Tests\nColumns: 4\n#\n  0   0   0\tBlack\n255 255 255 White with spaces\n# a comment\n 12  34  56\nnot a colour\n";

fn sample_kpl() -> Vec<u8> {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ColorSet version="2.0" name="Krita test" columns="3" comment="" readonly="false" rows="2">
 <ColorSetEntry name="Second" id="2" spot="false" bitdepth="U8">
  <RGB r="0" g="1" b="0" space="sRGB-elle-V2-srgbtrc.icc"/>
  <Position row="0" column="1"/>
 </ColorSetEntry>
 <ColorSetEntry name="First" id="1" spot="true" bitdepth="U8">
  <sRGB r="1" g="0" b="0"/>
  <Position row="0" column="0"/>
 </ColorSetEntry>
 <ColorSetEntry name="Mystery" id="9" bitdepth="U8"><YCbCr Y="1" Cb="0" Cr="0"/></ColorSetEntry>
 <Group name="Print" rows="1">
  <ColorSetEntry name="Ink" id="3" bitdepth="U8"><CMYK c="0" m="1" y="1" k="0" space="x"/><Position row="0" column="0"/></ColorSetEntry>
  <ColorSetEntry name="Lab" id="4" bitdepth="U16"><Lab L="50" a="0" b="0" space="x"/><Position row="0" column="1"/></ColorSetEntry>
  <ColorSetEntry name="Grey" id="5" bitdepth="U8"><Gray g="0.25" space="x"/></ColorSetEntry>
 </Group>
</ColorSet>"#;
    stored_zip(&[("mimetype", b"krita/x-colorset"), ("colorset.xml", xml.as_bytes()), ("profiles.xml", b"<Profiles/>")])
}

fn sample_swatches() -> Vec<u8> {
    let json = r#"{"name":"Proc","swatches":[{"hue":0,"saturation":1,"brightness":1,"alpha":1,"colorSpace":0},null,{"hue":0.5,"saturation":1,"brightness":0.5,"alpha":0.5}]}"#;
    stored_zip(&[("Swatches.json", json.as_bytes())])
}

/// Every format's sample, with the extension its files use.
fn samples() -> Vec<(PaletteFormat, &'static str, Vec<u8>)> {
    vec![
        (PaletteFormat::Ase, "ase", sample_ase()),
        (PaletteFormat::Aco, "aco", sample_aco()),
        (PaletteFormat::Act, "act", sample_act(None)),
        (PaletteFormat::Act, "act", sample_act(Some((16, 3)))),
        (PaletteFormat::RiffPal, "pal", sample_riff()),
        (PaletteFormat::Gpl, "gpl", GPL.as_bytes().to_vec()),
        (PaletteFormat::JascPal, "pal", b"JASC-PAL\r\n0100\r\n3\r\n255 0 0\r\n0 255 0\r\n0 0 255\r\n".to_vec()),
        (PaletteFormat::Hex, "hex", b"ff0044\n#00ff00\r\n\n0000FF\n".to_vec()),
        (PaletteFormat::Kpl, "kpl", sample_kpl()),
        (PaletteFormat::ProcreateSwatches, "swatches", sample_swatches()),
    ]
}

//endregion

/*┌─────────────────╮
| Formats         |
└─────────────────╯*/
//region Formats

#[test]
fn every_sample_is_recognised_and_read() {
    for (format, ext, bytes) in samples() {
        let head = &bytes[..bytes.len().min(4096)];
        assert_eq!(sniff(Path::new(&format!("x.{ext}")), head), Some(format), "{format:?}");
        let p = read(format, &bytes).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert!(!p.is_empty(), "{format:?}");
        let img = thumbnail(&p, 128);
        assert_eq!((img.width, img.height), (128, 128));
        assert!(img.pixels.chunks_exact(4).any(|px| px[3] == 255), "{format:?}: empty thumbnail");
    }
}

#[test]
fn ase_groups_models_and_kinds() {
    let p = read(PaletteFormat::Ase, &sample_ase()).unwrap();
    let names: Vec<&str> = p.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["", "Brand", ""]);
    let brand = &p.groups[1].swatches;
    assert_eq!(brand[0].name, "Spot cyan");
    assert_eq!(brand[0].kind, SwatchKind::Spot);
    assert_eq!(brand[0].hex(), "#00FFFF");
    assert_eq!(brand[0].values().unwrap(), "C 100% M 0% Y 0% K 0%");
    assert_eq!(brand[1].kind, SwatchKind::Global);
    assert_eq!(brand[1].color, Color::Lab([50.0, 0.0, 0.0]));
    assert_eq!(brand[1].hex(), "#777777");
    assert_eq!(brand[2].hex(), "#808080");
    assert_eq!(p.groups[2].swatches[0].hex(), "#0000FF");
}

#[test]
fn aco_prefers_the_named_section() {
    let p = read(PaletteFormat::Aco, &sample_aco()).unwrap();
    let s: Vec<&Swatch> = p.swatches().collect();
    assert_eq!(s.len(), 5);
    assert_eq!(s[0].name, "Red");
    assert_eq!(s[0].hex(), "#FF0000");
    assert_eq!(s[1].hex(), "#FF0000", "HSB 0, 100%, 100%");
    assert_eq!(s[2].values().unwrap(), "C 0% M 100% Y 0% K 0%");
    assert_eq!(s[2].hex(), "#FF00FF");
    assert_eq!(s[3].color, Color::Lab([50.0, 0.0, 0.0]));
    assert_eq!(s[4].values().unwrap(), "K 25%");
    // Version 1 alone: no names.
    let v1: Vec<u8> = sample_aco()[..4 + 5 * 10].to_vec();
    let p = read(PaletteFormat::Aco, &v1).unwrap();
    assert_eq!(p.len(), 5);
    assert!(p.swatches().all(|s| s.name.is_empty()));
}

#[test]
fn act_counts_and_transparency() {
    assert_eq!(read(PaletteFormat::Act, &sample_act(None)).unwrap().len(), 256);
    let p = read(PaletteFormat::Act, &sample_act(Some((16, 3)))).unwrap();
    assert_eq!(p.len(), 16);
    let s: Vec<&Swatch> = p.swatches().collect();
    assert_eq!(s[3].alpha, 0.0);
    assert_eq!(s[1].hex(), "#01FE07");
    // Other sizes, or impossible trailers, aren't colour tables.
    assert!(!binary::looks_like_act(&[0; 769]));
    assert!(!binary::looks_like_act(&sample_act(Some((300, 0)))));
    assert!(!binary::looks_like_act(&sample_act(Some((16, 400)))));
}

#[test]
fn gpl_names_columns_and_bad_lines() {
    let p = read(PaletteFormat::Gpl, GPL.as_bytes()).unwrap();
    assert_eq!(p.name, "Tests");
    assert_eq!(p.columns, Some(4));
    let s: Vec<&Swatch> = p.swatches().collect();
    assert_eq!(s.len(), 3);
    assert_eq!(s[1].name, "White with spaces");
    assert_eq!(s[2].hex(), "#0C2238");
    assert_eq!(p.warnings.len(), 1);
    // Aseprite's RGBA variant.
    let rgba = "GIMP Palette\nChannels: RGBA\n255 0 0 128 Half red\n";
    let p = read(PaletteFormat::Gpl, rgba.as_bytes()).unwrap();
    let s = p.swatches().next().unwrap();
    assert_eq!((s.name.as_str(), s.rgba8()), ("Half red", [255, 0, 0, 128]));
}

#[test]
fn kpl_positions_groups_and_models() {
    let p = read(PaletteFormat::Kpl, &sample_kpl()).unwrap();
    assert_eq!(p.name, "Krita test");
    assert_eq!(p.columns, Some(3));
    let top: Vec<&str> = p.groups[0].swatches.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(top, ["First", "Second"], "ordered by position; YCbCr skipped");
    assert_eq!(p.groups[0].swatches[0].kind, SwatchKind::Spot);
    assert_eq!(p.groups[1].name, "Print");
    assert_eq!(p.groups[1].swatches[0].hex(), "#FF0000");
    assert_eq!(p.groups[1].swatches[1].hex(), "#777777");
    assert_eq!(p.groups[1].swatches[2].hex(), "#404040");
    assert_eq!(p.warnings.len(), 1);
}

#[test]
fn procreate_swatches_skip_empty_slots() {
    let p = read(PaletteFormat::ProcreateSwatches, &sample_swatches()).unwrap();
    assert_eq!(p.name, "Proc");
    let s: Vec<&Swatch> = p.swatches().collect();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].hex(), "#FF0000");
    assert_eq!(s[1].rgba8(), [0, 128, 128, 128]);
    assert_eq!(s[1].values().unwrap(), "H 180° S 100% B 50%");
}

#[test]
fn describe_lists_groups_and_values() {
    let text = read(PaletteFormat::Ase, &sample_ase()).unwrap().describe(100);
    assert!(text.contains("Colours: 5"), "{text}");
    assert!(text.contains("group Brand:"), "{text}");
    assert!(text.contains("  #00FFFF  Spot cyan  (C 100% M 0% Y 0% K 0%)  spot"), "{text}");
    assert!(text.contains("  #777777  Lab grey  (L 50.0 a 0.0 b 0.0)  global"), "{text}");
    // The listing is capped.
    let short = read(PaletteFormat::Ase, &sample_ase()).unwrap().describe(2);
    assert!(short.contains("... 3 more"), "{short}");
}

#[test]
fn riff_jasc_and_hex() {
    let s: Vec<String> = read(PaletteFormat::RiffPal, &sample_riff()).unwrap().swatches().map(Swatch::hex).collect();
    assert_eq!(s, ["#FF0000", "#00FF00", "#0000FF"]);
    let jasc = b"JASC-PAL\n0100\n4\n1 2 3\n";
    let p = read(PaletteFormat::JascPal, jasc).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p.warnings.len(), 1, "fewer colours than announced");
    let s: Vec<String> = read(PaletteFormat::Hex, b"ff0044\n#00ff00\n").unwrap().swatches().map(Swatch::hex).collect();
    assert_eq!(s, ["#FF0044", "#00FF00"]);
}

#[test]
fn lab_and_cmyk_convert_like_the_references() {
    // sRGB red is Lab (D50) 54.29, 80.80, 69.89.
    let [r, g, b] = to_srgb(Color::Lab([54.29, 80.80, 69.89]));
    assert!((r - 1.0).abs() < 0.01 && g < 0.02 && b < 0.02, "{r} {g} {b}");
    assert_eq!(to_srgb(Color::Cmyk([0.0, 0.0, 0.0, 1.0])), [0.0; 3]);
    let w = to_srgb(Color::Lab([100.0, 0.0, 0.0]));
    assert!(w.iter().all(|v| (v - 1.0).abs() < 0.002), "{w:?}");
}

//endregion

/*┌─────────────────╮
| Ambiguous names |
└─────────────────╯*/
//region Ambiguous names

/// `.pal`, `.hex`, `.act` and `.aco` files that aren't palettes are left
/// alone.
#[test]
fn files_that_are_not_palettes_are_not_claimed() {
    let intel_hex = b":10010000214601360121470136007EFE09D2190140\n:00000001FF\n";
    assert_eq!(sniff(Path::new("firmware.hex"), intel_hex), None);
    assert_eq!(sniff(Path::new("notes.hex"), b"hello world\n"), None);
    assert_eq!(sniff(Path::new("dump.hex"), b"00 11 22 33\n"), None);
    assert_eq!(sniff(Path::new("empty.hex"), b"\n\n"), None);
    // Verilog memory images: 32-bit words.
    assert_eq!(sniff(Path::new("rom.hex"), b"40000537\n00050513\n"), None);
    // A raw 768-byte game palette named .pal has no magic, so it isn't
    // claimed (Doom's and Quake's palettes are read as lumps elsewhere).
    assert_eq!(sniff(Path::new("game.pal"), &[0u8; 768]), None);
    assert_eq!(sniff(Path::new("video.pal"), b"PAL video settings\n"), None);
    // Audio recordings use .act too.
    assert_eq!(sniff(Path::new("voice.act"), &[0u8; 5000]), None);
    assert_eq!(sniff(Path::new("x.aco"), b"\x00\x05\x00\x01"), None);
    // The extension decides nothing for files with magic.
    assert_eq!(sniff(Path::new("swatches.bin"), &sample_ase()), Some(PaletteFormat::Ase));
    assert_eq!(sniff(Path::new("x.kpl"), b"not a zip"), None);
}

/// A long hex list is sniffed from its first 4096 bytes, whose last line
/// is usually cut short.
#[test]
fn long_hex_lists_are_recognised_from_their_start() {
    let list: String = (0..1000).map(|i| format!("{:06x}\n", i * 997)).collect();
    let head = &list.as_bytes()[..4096];
    assert_eq!(sniff(Path::new("big.hex"), head), Some(PaletteFormat::Hex));
    assert_eq!(read(PaletteFormat::Hex, list.as_bytes()).unwrap().len(), 1000);
}

//endregion

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

/// Truncated and mutated copies of every sample: errors or partial
/// palettes, never panics or runaway allocations.
#[test]
fn damaged_palettes_fail_cleanly() {
    for (format, _, bytes) in samples() {
        for cut in 0..bytes.len() {
            if let Ok(p) = read(format, &bytes[..cut]) {
                let _ = thumbnail(&p, 64);
            }
        }
        let mut seed = 0x9E37_79B9u32;
        for round in 0..1500 {
            let mut copy = bytes.clone();
            for _ in 0..1 + round % 6 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % copy.len();
                copy[i] = (seed >> 8) as u8;
            }
            if let Ok(p) = read(format, &copy) {
                assert!(p.len() <= MAX_SWATCHES);
                let _ = thumbnail(&p, 64);
            }
        }
    }
}

/// Counts that promise billions of colours read only what is there.
#[test]
fn absurd_counts_are_bounded() {
    let mut ase = b"ASEF\x00\x01\x00\x00\xFF\xFF\xFF\xFF".to_vec();
    ase.extend(ase_color("x", b"RGB ", &[1.0, 1.0, 1.0], 2));
    assert_eq!(read(PaletteFormat::Ase, &ase).unwrap().len(), 1);
    // A block claiming 4 GB.
    let mut ase2 = b"ASEF\x00\x01\x00\x00\x00\x00\x00\x02".to_vec();
    ase2.extend(ase_color("x", b"RGB ", &[1.0, 1.0, 1.0], 2));
    ase2.extend_from_slice(&[0, 1, 0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(read(PaletteFormat::Ase, &ase2).unwrap().len(), 1);
    // A name claiming 65535 units.
    let mut ase3 = b"ASEF\x00\x01\x00\x00\x00\x00\x00\x01".to_vec();
    ase3.extend(ase_block(1, &[0xFF, 0xFF, 0, 0x41]));
    assert!(read(PaletteFormat::Ase, &ase3).is_err());
    let mut aco = vec![0, 1, 0xFF, 0xFF];
    aco.extend_from_slice(&[0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0]);
    assert_eq!(read(PaletteFormat::Aco, &aco).unwrap().len(), 1);
    // A version 2 name length of 4 billion.
    let mut aco2 = vec![0, 2, 0, 1, 0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0];
    aco2.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0x41]);
    assert!(read(PaletteFormat::Aco, &aco2).is_err());
    let riff = [&sample_riff()[..22], &[0xFF, 0xFF][..], &sample_riff()[24..]].concat();
    assert_eq!(read(PaletteFormat::RiffPal, &riff).unwrap().len(), 3);
    let jasc = b"JASC-PAL\n0100\n4000000000\n1 2 3\n";
    assert_eq!(read(PaletteFormat::JascPal, jasc).unwrap().len(), 1);
}

/// Palettes with more colours than the cap keep the first ones.
#[test]
fn huge_palettes_are_capped() {
    let list: String = (0..MAX_SWATCHES + 10).map(|i| format!("{:06x}\n", i)).collect();
    let p = read(PaletteFormat::Hex, list.as_bytes()).unwrap();
    assert_eq!(p.len(), MAX_SWATCHES);
    assert_eq!(p.warnings.len(), 1);
    let start = std::time::Instant::now();
    let img = thumbnail(&p, 256);
    assert_eq!(img.width, 256);
    assert!(start.elapsed().as_millis() < 2000, "{:?}", start.elapsed());
}

/// A Krita palette whose colorset.xml declares itself 4 GB inflated, and a
/// deflate bomb, are refused.
#[test]
fn zipped_palettes_refuse_bombs() {
    let big = vec![b' '; 32 << 20];
    let packed = miniz_oxide::deflate::compress_to_vec(&big, 6);
    let mut zip = stored_zip(&[("colorset.xml", &packed)]);
    let dir = zip.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    zip[dir + 10] = 8;
    zip[dir + 24..dir + 28].copy_from_slice(&(32u32 << 20).to_le_bytes());
    assert!(matches!(read(PaletteFormat::Kpl, &zip), Err(LoadError::TooLarge(_))));
    zip[dir + 24..dir + 28].copy_from_slice(&1000u32.to_le_bytes());
    assert!(read(PaletteFormat::Kpl, &zip).is_err());
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

#[test]
fn layouts_fit_and_separate_groups() {
    let mut p = Palette::new(PaletteFormat::Gpl);
    for _ in 0..16 {
        p.push(Swatch::new("", Color::Gray(0.5)));
    }
    let lay = layout(&p, 100.0, 100.0);
    assert_eq!(lay.columns, 4);
    assert!(lay.width <= 100.0 && lay.height <= 100.0);
    p.begin_group("B".into());
    p.push(Swatch::new("", Color::Gray(0.0)));
    let lay = layout(&p, 100.0, 100.0);
    let last = lay.cells.last().unwrap();
    let above = lay.cells[15];
    assert!(last.3 > above.3 + lay.cell + lay.gap, "the group starts lower, after a wider gap");
    assert!(lay.height <= 100.0);
    // One colour fills the square.
    let mut one = Palette::new(PaletteFormat::Hex);
    one.push(Swatch::new("", Color::Gray(1.0)));
    assert!(layout(&one, 100.0, 100.0).cell >= 99.0);
}

fn sample_folder(name: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra/palettes").join(name);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// Files in the palette folders that aren't claimed, by part of their
/// name: an Aseprite sprite, a Git LFS pointer, a fuzzer's crash file
/// from a Krita bug report, a program in the KPL teaching language, a
/// 1008-byte colour table (Adobe's are 768 or 772 bytes, and the size is
/// all that tells them from voice recordings sharing `.act`), and a hex
/// list with alpha (`RRGGBBAA`, which looks like a `$readmemh` memory
/// image).
const NOT_PALETTES: [&str; 6] =
    ["splash-aseprite.ase", "lfs-pointer", "kpl_crash", "kplcompiler-", "1j01-VisiBone.act", "pear36-transparency"];

/// The real samples: every palette in the format folders reads, and the
/// non-palettes beside them aren't claimed.  `SLERP_TEST_OUT=<dir>` saves
/// their thumbnails.
#[test]
fn real_samples() {
    let folders: [(&str, Option<PaletteFormat>); 12] = [
        ("ase", Some(PaletteFormat::Ase)),
        ("aco", Some(PaletteFormat::Aco)),
        ("act", Some(PaletteFormat::Act)),
        ("gpl", Some(PaletteFormat::Gpl)),
        ("kpl", Some(PaletteFormat::Kpl)),
        ("pal-jasc", Some(PaletteFormat::JascPal)),
        ("pal-riff", Some(PaletteFormat::RiffPal)),
        ("hex", Some(PaletteFormat::Hex)),
        ("swatches", Some(PaletteFormat::ProcreateSwatches)),
        ("pal-other", None),
        ("hex-other", None),
        ("xfig", None),
    ];
    for (name, format) in folders {
        let Some(dir) = sample_folder(name) else { continue };
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e != "md"))
            .collect();
        files.sort();
        for path in files {
            let bytes = std::fs::read(&path).unwrap();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            let sniffed = sniff(&path, &bytes[..bytes.len().min(4096)]);
            let Some(format) = format else {
                assert_eq!(sniffed, None, "{name}/{file} isn't a palette");
                continue;
            };
            // Negative samples kept beside the palettes: an Aseprite
            // sprite, a Git LFS pointer.
            if NOT_PALETTES.iter().any(|n| file.contains(n)) {
                assert_eq!(sniffed, None, "{name}/{file} isn't a palette");
                continue;
            }
            assert_eq!(sniffed, Some(format), "{name}/{file}");
            let start = std::time::Instant::now();
            let p = match read_path(format, &path, &Limits::thumbnail()) {
                Ok(p) => p,
                // A palette saved without colours (Krita's own test
                // files have some).
                Err(LoadError::Unsupported(e)) if e.contains("without colours") => {
                    eprintln!("{name}/{file}: {e}");
                    continue;
                }
                Err(e) => panic!("{name}/{file}: {e}"),
            };
            let img = thumbnail(&p, 256);
            let took = start.elapsed().as_secs_f64() * 1000.0;
            let groups = p.groups.iter().filter(|g| !g.name.is_empty()).count();
            eprintln!("{name}/{file}: {} colours, {groups} groups, {:?} in {took:.2} ms", p.len(), p.warnings);
            if let Some(out) = std::env::var_os("SLERP_TEST_OUT") {
                let f = std::fs::File::create(Path::new(&out).join(format!("palette-{file}.png"))).unwrap();
                let mut enc = png::Encoder::new(f, img.width, img.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
            }
        }
    }
}

//endregion
