//! Tests for the contents model: items and their formats, conversions with
//! known pixels, composite textures, safe names and extraction.

use super::*;
use slerp_archive::testutil;
use std::path::Path;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-contents-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Colour `i` is (i, 255 - i, i / 2).
fn playpal() -> Vec<u8> {
    (0..256u32).flat_map(|i| [i as u8, 255 - i as u8, (i / 2) as u8]).collect()
}

fn dmx(samples: &[u8]) -> Vec<u8> {
    let mut d = vec![3, 0];
    d.extend_from_slice(&22050u16.to_le_bytes());
    d.extend_from_slice(&(samples.len() as u32 + 32).to_le_bytes());
    d.extend_from_slice(&[128; 16]);
    d.extend_from_slice(samples);
    d.extend_from_slice(&[128; 16]);
    d
}

/// A WAD with a palette, a picture, a flat, a sound, music, a sprite
/// namespace and a map.
fn sample_wad() -> Vec<u8> {
    let picture = testutil::doom_picture(2, 2, -3, 5, &[1, 2, 3, 4]);
    let lumps: &[(&str, &[u8])] = &[
        ("PLAYPAL", &playpal()),
        ("TITLEPIC", &picture),
        ("DSTEST", &dmx(&[10, 20, 30])),
        ("D_TEST", b"MUS\x1a\x04\x00\x10\x00\x01\x00\x00\x00\x01\x00\x00\x00\x60\x00\x00\x00"),
        ("S_START", b""),
        ("TROOA1", &picture),
        ("S_END", b""),
        ("F_START", b""),
        ("FLOOR", &[9u8; 4096]),
        ("F_END", b""),
        ("MAP01", b""),
        ("THINGS", &[0u8; 10]),
        ("LINEDEFS", &[0u8; 14]),
        ("MAP02", b""),
        ("THINGS", &[0u8; 10]),
        ("README", b"Read me, please.\n"),
    ];
    testutil::wad(true, lumps)
}

fn item_named(c: &Contents, path: &str) -> ItemId {
    c.items().iter().find(|i| i.path == path).unwrap_or_else(|| panic!("no item {path}")).id
}

/*┌─────────────────╮
| Items           |
└─────────────────╯*/
//region Items

#[test]
fn items_know_their_formats() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let paths: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(&paths[..4], ["PLAYPAL", "TITLEPIC", "DSTEST", "D_TEST"]);
    assert!(paths.contains(&"Sprites/TROOA1") && paths.contains(&"Flats/FLOOR") && paths.contains(&"MAP01/THINGS"));
    let formats = |p: &str| c.item(item_named(&c, p)).unwrap().formats.clone();
    assert_eq!(formats("PLAYPAL"), [Format::Raw, Format::Png]);
    assert_eq!(formats("TITLEPIC"), [Format::Raw, Format::Png]);
    assert_eq!(formats("DSTEST"), [Format::Raw, Format::Wav]);
    assert_eq!(formats("D_TEST"), [Format::Raw, Format::Mid]);
    assert_eq!(formats("Flats/FLOOR"), [Format::Raw, Format::Png]);
    assert_eq!(formats("README"), [Format::Raw]);
    // A map's marker can also be saved as a WAD of the map.
    assert_eq!(formats("MAP01/MAP01"), [Format::Raw, Format::Wad]);
    assert_eq!(formats("MAP01/THINGS"), [Format::Raw]);
    assert_eq!(c.maps().len(), 2);
    assert_eq!(c.label(), "Doom IWAD");
}

#[test]
fn a_request_resolves_per_item() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let get = |p: &str| c.item(item_named(&c, p)).unwrap().clone();
    assert_eq!(get("TITLEPIC").resolve(Format::Auto), Format::Png);
    assert_eq!(get("DSTEST").resolve(Format::Auto), Format::Wav);
    assert_eq!(get("D_TEST").resolve(Format::Auto), Format::Mid);
    assert_eq!(get("README").resolve(Format::Auto), Format::Raw);
    // Auto leaves palettes and map data as stored.
    assert_eq!(get("PLAYPAL").resolve(Format::Auto), Format::Raw);
    assert_eq!(get("MAP01/THINGS").resolve(Format::Auto), Format::Raw);
    // A format the item can't be saved as falls back to the bytes.
    assert_eq!(get("README").resolve(Format::Png), Format::Raw);
    assert_eq!(get("DSTEST").resolve(Format::Png), Format::Raw);
    assert_eq!(get("TITLEPIC").resolve(Format::Raw), Format::Raw);
    assert_eq!(get("TITLEPIC").resolve(Format::Png), Format::Png);
}

#[test]
fn keys_find_items_again() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    // THINGS appears under two maps; the key keeps the path.
    let id = item_named(&c, "MAP02/THINGS");
    let key = c.key(id).unwrap();
    assert_eq!(key, ItemKey { path: "MAP02/THINGS".into(), nth: 0 });
    assert_eq!(c.find_key(&key), Some(id));
    // Repeated paths are told apart by their order.
    let z = testutil::wad(false, &[("DUP", b"a"[..].as_ref()), ("DUP", b"b"), ("DUP", b"c")]);
    let c = Contents::from_bytes(z).unwrap();
    let second = c.items()[1].id;
    let key = c.key(second).unwrap();
    assert_eq!(key.nth, 1);
    assert_eq!(c.find_key(&key), Some(second));
    assert_eq!(c.find_key(&ItemKey { path: "DUP".into(), nth: 5 }), None);
    assert_eq!(c.find_key(&ItemKey { path: "NOPE".into(), nth: 0 }), None);
}

//endregion

/*┌─────────────────╮
| Conversions     |
└─────────────────╯*/
//region Conversions

fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn pictures_become_pngs_with_offsets() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let (name, png) = c.export(item_named(&c, "TITLEPIC"), Format::Png).unwrap();
    assert_eq!(name, "TITLEPIC.png");
    let (w, h, pixels) = decode_png(&png);
    assert_eq!((w, h), (2, 2));
    // The archive's own PLAYPAL: index 1 is (1, 254, 0).
    assert_eq!(&pixels[..4], [1, 254, 0, 255]);
    assert_eq!(&pixels[12..16], [4, 251, 2, 255]);
    assert_eq!(png_offsets(&png), Some((-3, 5)));
    // The same through the entry-level function.
    let entry = c.entry(item_named(&c, "TITLEPIC")).unwrap();
    let again = export(c.archive().unwrap(), entry, Format::Png).unwrap();
    assert_eq!(again.1, png);
    assert_eq!(convert_options(entry), [Format::Raw, Format::Png]);
    // Raw is the lump as stored, named with the lump extension.
    let (name, raw) = c.export(item_named(&c, "TITLEPIC"), Format::Raw).unwrap();
    assert_eq!(name, "TITLEPIC.lmp");
    assert_eq!(raw, testutil::doom_picture(2, 2, -3, 5, &[1, 2, 3, 4]));
}

#[test]
fn pictures_without_playpal_use_freedoom_s() {
    let picture = testutil::doom_picture(1, 1, 0, 0, &[7]);
    let c = Contents::from_bytes(testutil::wad(false, &[("PIC", &picture)])).unwrap();
    let (_, png) = c.export(c.items()[0].id, Format::Png).unwrap();
    let (_, _, pixels) = decode_png(&png);
    let default = slerp_quake::convert::default_doom_palette();
    assert_eq!(&pixels[..3], &default[21..24]);
}

#[test]
fn flats_palettes_sounds_and_music_convert() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let (name, png) = c.export(item_named(&c, "Flats/FLOOR"), Format::Png).unwrap();
    assert_eq!(name, "FLOOR.png");
    let (w, h, pixels) = decode_png(&png);
    assert_eq!((w, h), (64, 64));
    assert_eq!(&pixels[..4], [9, 246, 4, 255]);

    let (name, png) = c.export(item_named(&c, "PLAYPAL"), Format::Png).unwrap();
    assert_eq!(name, "PLAYPAL.png");
    assert_eq!(decode_png(&png).0, 128);

    let (name, wav) = c.export(item_named(&c, "DSTEST"), Format::Wav).unwrap();
    assert_eq!(name, "DSTEST.wav");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[44..], [10, 20, 30]);

    let (name, midi) = c.export(item_named(&c, "D_TEST"), Format::Mid).unwrap();
    assert_eq!(name, "D_TEST.mid");
    assert_eq!(&midi[..4], b"MThd");
    // Not convertible: asking falls back to the bytes.
    let (name, bytes) = c.export(item_named(&c, "README"), Format::Png).unwrap();
    assert_eq!((name.as_str(), bytes.as_slice()), ("README.txt", &b"Read me, please.\n"[..]));
}

#[test]
fn a_map_saves_as_a_wad_of_its_lumps() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let (name, bytes) = c.export(item_named(&c, "MAP01/MAP01"), Format::Wad).unwrap();
    assert_eq!(name, "MAP01.wad");
    let inner = Contents::from_bytes(bytes).unwrap();
    let paths: Vec<&str> = inner.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, ["MAP01/MAP01", "MAP01/THINGS", "MAP01/LINEDEFS"]);
    assert_eq!(inner.label(), "Doom PWAD");
    assert_eq!(inner.read(inner.items()[2].id).unwrap().len(), 14);
}

/// A WAD with a composite texture of two patches.
fn texture_wad() -> Vec<u8> {
    let mut pal = vec![0u8; 768];
    pal[3..6].copy_from_slice(&[255, 0, 0]);
    pal[6..9].copy_from_slice(&[0, 255, 0]);
    let red = testutil::doom_picture(2, 2, 0, 0, &[1; 4]);
    let green = testutil::doom_picture(2, 2, 0, 0, &[2; 4]);
    let mut pnames = 2u32.to_le_bytes().to_vec();
    pnames.extend_from_slice(b"RED\0\0\0\0\0GREEN\0\0\0");
    let mut t = 2u32.to_le_bytes().to_vec();
    t.extend_from_slice(&12u32.to_le_bytes());
    t.extend_from_slice(&(12u32 + 22 + 20).to_le_bytes());
    for (name, patches) in
        [(b"WALL\0\0\0\0", [(0i16, 0i16, 0u16), (2, 2, 1)]), (b"WALL2\0\0\0", [(0, 0, 1), (2, 0, 0)])]
    {
        t.extend_from_slice(name);
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&4i16.to_le_bytes());
        t.extend_from_slice(&4i16.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&2i16.to_le_bytes());
        for (x, y, p) in patches {
            t.extend_from_slice(&x.to_le_bytes());
            t.extend_from_slice(&y.to_le_bytes());
            t.extend_from_slice(&p.to_le_bytes());
            t.extend_from_slice(&[1, 0, 0, 0]);
        }
    }
    let lumps: &[(&str, &[u8])] = &[
        ("PLAYPAL", &pal),
        ("PNAMES", &pnames),
        ("TEXTURE1", &t),
        ("P_START", b""),
        ("RED", &red),
        ("GREEN", &green),
        ("P_END", b""),
    ];
    testutil::wad(true, lumps)
}

#[test]
fn composite_textures_are_items_of_their_own() {
    let c = Contents::from_bytes(texture_wad()).unwrap();
    let paths: Vec<&str> = c.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(&paths[..5], ["PLAYPAL", "PNAMES", "TEXTURE1", "TEXTURE1/WALL", "TEXTURE1/WALL2"]);
    let wall = c.item(item_named(&c, "TEXTURE1/WALL")).unwrap();
    assert_eq!(wall.kind, ItemKind::Texture);
    assert_eq!(wall.formats, [Format::Png]);
    assert_eq!(wall.detail, "4x4, 2 patches");
    assert_eq!(wall.resolve(Format::Raw), Format::Png);
    let (name, png) = c.export(wall.id, Format::Auto).unwrap();
    assert_eq!(name, "WALL.png");
    let (w, h, px) = decode_png(&png);
    assert_eq!((w, h), (4, 4));
    assert_eq!(&px[..4], [255, 0, 0, 255]);
    assert_eq!(&px[(2 * 4 + 2) * 4..(2 * 4 + 2) * 4 + 4], [0, 255, 0, 255]);
    // The second texture draws its patches in the other order.
    let (_, png2) = c.export(item_named(&c, "TEXTURE1/WALL2"), Format::Png).unwrap();
    let (_, _, px2) = decode_png(&png2);
    assert_eq!(&px2[..4], [0, 255, 0, 255]);
    // The definitions lump itself is an ordinary item.
    assert_eq!(c.item(item_named(&c, "TEXTURE1")).unwrap().formats, [Format::Raw]);
    assert!(c.read(wall.id).is_err());
}

#[test]
fn zdoom_textures_lumps_list_their_textures() {
    let pic = testutil::doom_picture(1, 1, 0, 0, &[1]);
    let text = b"Texture \"BRICK\", 16, 8 { Patch \"RED\", 0, 0 }\nTexture \"TWO\", 4, 4 { Patch \"RED\", 0, 0 }";
    let lumps: &[(&str, &[u8])] = &[("TEXTURES", text), ("P_START", b""), ("RED", &pic), ("P_END", b"")];
    let c = Contents::from_bytes(testutil::wad(false, lumps)).unwrap();
    let details: Vec<(&str, &str)> = c
        .items()
        .iter()
        .filter(|i| i.path.starts_with("TEXTURES/"))
        .map(|i| (i.path.as_str(), i.detail.as_str()))
        .collect();
    assert_eq!(details, [("TEXTURES/BRICK", "16x8, 1 patch"), ("TEXTURES/TWO", "4x4, 1 patch")]);
    let (_, png) = c.export(item_named(&c, "TEXTURES/BRICK"), Format::Png).unwrap();
    assert_eq!(decode_png(&png).0, 16);
}

#[test]
fn quake_pictures_use_the_packs_palette() {
    let mut qpic = Vec::new();
    qpic.extend_from_slice(&1u32.to_le_bytes());
    qpic.extend_from_slice(&1u32.to_le_bytes());
    qpic.push(3);
    let pal = playpal();
    let files: &[(&str, &[u8])] = &[("gfx/palette.lmp", &pal[..768]), ("gfx/pause.lmp", &qpic)];
    let pak = testutil::pak(files);
    let c = Contents::from_bytes(pak).unwrap();
    assert_eq!(c.label(), "Quake PAK");
    let id = item_named(&c, "gfx/pause.lmp");
    assert_eq!(c.item(id).unwrap().formats, [Format::Raw, Format::Png]);
    let (name, png) = c.export(id, Format::Png).unwrap();
    assert_eq!(name, "pause.png");
    assert_eq!(&decode_png(&png).2[..4], [3, 252, 1, 255]);
    let (name, _) = c.export(id, Format::Raw).unwrap();
    assert_eq!(name, "pause.lmp");
}

//endregion

/*┌─────────────────╮
| Previews        |
└─────────────────╯*/
//region Previews

#[test]
fn previews_say_what_to_show() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let preview = |p: &str| c.preview(item_named(&c, p)).unwrap();
    // A picture becomes a PNG file, with its size and offset as facts.
    let p = preview("TITLEPIC");
    let PreviewKind::File { name, bytes } = p.kind else { panic!("not a file") };
    assert_eq!(name, "TITLEPIC.png");
    assert_eq!(decode_png(&bytes).0, 2);
    assert!(p.facts.contains(&("Offset".into(), "-3, 5".into())));
    // A sound becomes a waveform picture with its rate.
    let p = preview("DSTEST");
    assert!(matches!(p.kind, PreviewKind::File { .. }));
    assert!(p.facts.iter().any(|(k, v)| k == "Sample rate" && v == "22050 Hz"));
    // Text is text; map data and music are only facts.
    assert!(matches!(preview("README").kind, PreviewKind::Text(t) if t.starts_with("Read me")));
    assert!(matches!(preview("MAP01/THINGS").kind, PreviewKind::Info));
    assert!(matches!(preview("D_TEST").kind, PreviewKind::Info));
    // Files the viewer opens as they are keep their bytes and name.
    let png = {
        let img = slerp_quake::convert::Image { width: 1, height: 1, rgba: vec![9, 8, 7, 255], offset: None };
        png_bytes(&img).unwrap()
    };
    let files: &[(&str, &[u8])] = &[("textures/a.png", &png), ("sounds/x.ogg", b"OggS....")];
    let z = Contents::from_bytes(testutil::zip(files)).unwrap();
    let PreviewKind::File { name, bytes } = z.preview(z.items()[0].id).unwrap().kind else { panic!() };
    assert_eq!((name.as_str(), bytes), ("a.png", png));
    assert!(matches!(z.preview(z.items()[1].id).unwrap().kind, PreviewKind::Info));
}

#[test]
fn a_wav_is_drawn_as_a_waveform() {
    let samples: Vec<u8> = (0..2000).map(|i| (128.0 + 100.0 * (i as f32 / 20.0).sin()) as u8).collect();
    let wav = slerp_quake::convert::wav_u8_mono(8000, &samples);
    let files: &[(&str, &[u8])] = &[("a.wav", &wav)];
    let c = Contents::from_bytes(testutil::zip(files)).unwrap();
    let p = c.preview(c.items()[0].id).unwrap();
    assert!(p.facts.contains(&("Sample rate".into(), "8000 Hz".into())));
    assert!(p.facts.contains(&("Length".into(), "250 ms".into())));
    let PreviewKind::File { bytes, .. } = p.kind else { panic!() };
    assert_eq!(decode_png(&bytes).1, 256);
}

//endregion

/*┌─────────────────╮
| Safe names      |
└─────────────────╯*/
//region Safe names

/// A name is safe when joining it to a folder stays inside the folder on
/// every platform: no separators, no `..`, no device names, not empty.
fn assert_safe(name: &str, what: &str) {
    assert!(!name.is_empty(), "{what}: empty");
    assert!(!name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*', '\0']), "{what}: {name:?}");
    assert!(name != "." && name != "..", "{what}: {name:?}");
    assert!(!name.ends_with(['.', ' ']), "{what}: {name:?}");
    assert!(name.len() <= 200, "{what}: {} bytes", name.len());
    let stem = name.split('.').next().unwrap().to_ascii_uppercase();
    assert!(!["CON", "PRN", "AUX", "NUL", "COM1", "LPT1", "COM9", "LPT9"].contains(&stem.as_str()), "{what}: {name:?}");
}

const NASTY: &[&str] = &[
    "..",
    ".",
    "...",
    "../evil",
    "..\\evil",
    "/etc/passwd",
    "\\\\server\\share\\x",
    "C:\\Windows\\System32\\x.dll",
    "C:evil",
    "a/../../b",
    "CON",
    "con.txt",
    "Aux.tar.gz",
    "NUL",
    "COM1",
    "LPT9.png",
    "CONIN$",
    "a:b",
    "name.",
    "name ",
    " ",
    "",
    "a<b>c",
    "what?*",
    "tab\there",
    "nul\0byte",
    "\u{202e}rtl",
    "%00",
    "~root",
    "-rf",
    "x/",
];

#[test]
fn components_are_made_safe() {
    for n in NASTY {
        assert_safe(&safe_component(n), n);
    }
    assert_eq!(safe_component("TROOA1"), "TROOA1");
    assert_eq!(safe_component("Brick wall (2).png"), "Brick wall (2).png");
    assert_eq!(safe_component("a:b"), "a_b");
    assert_eq!(safe_component("con"), "_con");
    // Long names are cut on a character boundary and keep their extension.
    let long = format!("{}.png", "é".repeat(500));
    let short = safe_component(&long);
    assert!(short.len() <= 200 && short.ends_with(".png"), "{short}");
    assert_safe(&short, "long");
    assert_safe(&safe_component(&"x".repeat(100_000)), "huge");
}

#[test]
fn paths_stay_inside_their_folder() {
    for n in NASTY {
        let p = safe_relative_path(n);
        assert!(p.is_relative(), "{n}: {p:?}");
        for c in p.components() {
            let std::path::Component::Normal(part) = c else { panic!("{n}: {p:?}") };
            assert_safe(&part.to_string_lossy(), n);
        }
    }
    assert_eq!(safe_relative_path("Sprites/TROOA1"), Path::new("Sprites/TROOA1"));
    assert_eq!(safe_relative_path("/abs/path"), Path::new("abs/path"));
    assert_eq!(safe_relative_path("a\\b//c/./d"), Path::new("a/b/c/d"));
    assert_eq!(safe_relative_path("../../x"), Path::new("_/_/x"));
    assert_eq!(safe_relative_path(""), Path::new("_"));
    // Deep paths are folded.
    let deep = (0..100).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
    assert!(safe_relative_path(&deep).components().count() <= 32);
    // Random strings never produce an unsafe path.
    let mut seed = 99u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let alphabet: Vec<char> = "ab./\\:*?<>|\"~ -\u{0}\u{e9}\u{202e}CONUL".chars().collect();
    for _ in 0..5000 {
        let s: String = (0..rand() % 24).map(|_| alphabet[rand() as usize % alphabet.len()]).collect();
        for c in safe_relative_path(&s).components() {
            let std::path::Component::Normal(part) = c else { panic!("{s:?}") };
            assert_safe(&part.to_string_lossy(), &s);
        }
    }
}

#[test]
fn globs_match_like_shells_without_case() {
    assert!(glob_match("*.png", "brick.PNG"));
    assert!(!glob_match("*.png", "dir/brick.png"));
    assert!(glob_match("**/*.png", "dir/sub/brick.png"));
    assert!(glob_match("**.png", "dir/sub/brick.png"));
    assert!(glob_match("Sprites/TROO*", "sprites/trooa1"));
    assert!(glob_match("TROO?1", "TROOA1"));
    assert!(!glob_match("TROO?1", "TROO/1"));
    assert!(glob_match("MAP0[1-3]/THINGS", "map02/things"));
    assert!(!glob_match("MAP0[1-3]/THINGS", "MAP04/THINGS"));
    assert!(glob_match("MAP0[!1]", "MAP02"));
    assert!(!glob_match("MAP0[!1]", "MAP01"));
    assert!(glob_match("*", ""));
    assert!(glob_match("a*b*c", "aXXbYYc"));
    assert!(!glob_match("a*b*c", "aXXbYY"));
    assert!(glob_match("[", "["));
    // Many stars on a long text stay fast.
    assert!(!glob_match(&"*a".repeat(30), &"a".repeat(29)));
    assert!(glob_match(&"**".repeat(50), &"x/".repeat(500)));
}

//endregion

/*┌─────────────────╮
| Extraction      |
└─────────────────╯*/
//region Extraction

fn files_under(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn extract_writes_converted_files_in_folders() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let out = temp("extract");
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let report = extract(&c, &ids, &out, &ExtractOptions::default()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(
        files_under(&out),
        [
            "DSTEST.wav",
            "D_TEST.mid",
            "Flats/FLOOR.png",
            "MAP01/LINEDEFS.lmp",
            "MAP01/THINGS.lmp",
            "MAP02/THINGS.lmp",
            "PLAYPAL.lmp",
            "README.txt",
            "Sprites/TROOA1.png",
            "TITLEPIC.png",
        ]
    );
    // Raw keeps every lump's bytes.
    let raw = temp("extract-raw");
    extract(&c, &ids, &raw, &ExtractOptions { format: Format::Raw, ..Default::default() }).unwrap();
    assert!(files_under(&raw).contains(&"TITLEPIC.lmp".to_string()));
    assert_eq!(std::fs::read(raw.join("README.txt")).unwrap(), b"Read me, please.\n");
    // Markers on request.
    let with = temp("extract-markers");
    extract(&c, &ids, &with, &ExtractOptions { markers: true, ..Default::default() }).unwrap();
    assert!(files_under(&with).contains(&"Sprites/S_START.lmp".to_string()));
    for d in [out, raw, with] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn extract_selects_by_glob_and_folder() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let pick = |p: &[&str]| -> Vec<String> {
        let patterns: Vec<String> = p.iter().map(|s| s.to_string()).collect();
        select(&c, &patterns).unwrap().iter().map(|&i| c.item(i).unwrap().path.clone()).collect()
    };
    assert_eq!(pick(&["titlepic"]), ["TITLEPIC"]);
    assert_eq!(pick(&["THINGS"]), ["MAP01/THINGS", "MAP02/THINGS"]);
    assert_eq!(pick(&["map01"]), ["MAP01/MAP01", "MAP01/THINGS", "MAP01/LINEDEFS"]);
    assert_eq!(pick(&["sprites/*", "DS*"]), ["DSTEST", "Sprites/S_START", "Sprites/TROOA1", "Sprites/S_END"]);
    assert_eq!(pick(&["**/THINGS", "map01/"]).len(), 4);
    assert!(select(&c, &["nothing*".to_string()]).is_err());
}

/// An archive whose names try every trick.
fn hostile_zip() -> Vec<u8> {
    let files: &[(&str, &[u8])] = &[
        ("../evil.txt", b"1"),
        ("..\\evil2.txt", b"2"),
        ("/etc/slerp-test-passwd", b"3"),
        ("C:\\Windows\\x.dll", b"4"),
        ("a/../../b.txt", b"5"),
        ("CON", b"6"),
        ("nul.txt", b"7"),
        ("normal/ok.txt", b"8"),
        ("Normal/OK.txt", b"9"),
        ("normal/ok.txt", b"10"),
        (&"long".repeat(200), b"11"),
        ("trailing.", b"12"),
    ];
    testutil::zip(files)
}

#[test]
fn hostile_names_cannot_leave_the_output_folder() {
    let c = Contents::from_bytes(hostile_zip()).unwrap();
    let parent = temp("hostile");
    let out = parent.join("out");
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let report = extract(&c, &ids, &out, &ExtractOptions::default()).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.written.len(), 12);
    // Everything written is under the output folder, and nothing beside it.
    for w in &report.written {
        assert!(w.starts_with(std::fs::canonicalize(&out).unwrap()), "{w:?}");
    }
    assert_eq!(files_under(&parent).len(), 12);
    assert!(files_under(&parent).iter().all(|f| f.starts_with("out/")));
    assert!(!parent.join("evil.txt").exists() && !parent.join("b.txt").exists());
    assert!(!Path::new("/etc/slerp-test-passwd").exists());
    // Case-only and exact duplicates get their own names.
    let names = files_under(&out);
    assert!(names.contains(&"normal/ok.txt".to_string()));
    assert!(names.contains(&"Normal/OK.txt".to_string()) || names.contains(&"Normal/OK~2.txt".to_string()));
    assert_eq!(names.iter().filter(|n| n.to_lowercase().starts_with("normal/ok")).count(), 3);
    assert!(names.iter().all(|n| n.split('/').all(|p| p.len() <= 200)), "{names:?}");
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn extract_never_overwrites_without_force() {
    let files: &[(&str, &[u8])] = &[("a.txt", b"new"), ("b.txt", b"new")];
    let z = testutil::zip(files);
    let c = Contents::from_bytes(z).unwrap();
    let out = temp("overwrite");
    std::fs::write(out.join("a.txt"), b"old").unwrap();
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let report = extract(&c, &ids, &out, &ExtractOptions::default()).unwrap();
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"old");
    assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"new");
    assert_eq!(report.written.len(), 1);
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].1.contains("--force"));
    let report = extract(&c, &ids, &out, &ExtractOptions { force: true, ..Default::default() }).unwrap();
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"new");
    assert_eq!(report.written.len(), 2);
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn extract_does_not_follow_links() {
    let parent = temp("links");
    let out = parent.join("out");
    let elsewhere = parent.join("elsewhere");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    // A folder in the output that is a link to somewhere else.
    std::os::unix::fs::symlink(&elsewhere, out.join("dir")).unwrap();
    // A file name that is a link to a file elsewhere.
    std::fs::write(elsewhere.join("target.txt"), b"precious").unwrap();
    std::os::unix::fs::symlink(elsewhere.join("target.txt"), out.join("file.txt")).unwrap();
    let files: &[(&str, &[u8])] = &[("dir/x.txt", b"x"), ("file.txt", b"overwritten"), ("ok.txt", b"ok")];
    let c = Contents::from_bytes(testutil::zip(files)).unwrap();
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    // The link folder is refused outright.
    let err = extract(&c, &ids, &out, &ExtractOptions::default());
    assert!(matches!(err, Err(ContentsError::Unsafe(_))), "{err:?}");
    assert!(!elsewhere.join("x.txt").exists());
    // A link in the way of a file isn't followed, with or without force.
    let only: Vec<ItemId> = ids[1..].to_vec();
    let report = extract(&c, &only, &out, &ExtractOptions::default()).unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(std::fs::read(elsewhere.join("target.txt")).unwrap(), b"precious");
    extract(&c, &only, &out, &ExtractOptions { force: true, ..Default::default() }).unwrap();
    assert_eq!(std::fs::read(elsewhere.join("target.txt")).unwrap(), b"precious");
    assert_eq!(std::fs::read(out.join("file.txt")).unwrap(), b"overwritten");
    assert!(!std::fs::symlink_metadata(out.join("file.txt")).unwrap().file_type().is_symlink());
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn extract_stops_at_the_total_cap() {
    let files: &[(&str, &[u8])] = &[("a.bin", &[1u8; 1000]), ("b.bin", &[2u8; 1000]), ("c.bin", &[3u8; 1000])];
    let z = testutil::zip(files);
    let c = Contents::from_bytes(z).unwrap();
    let out = temp("cap");
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let err = extract(&c, &ids, &out, &ExtractOptions { max_total: Some(1500), ..Default::default() });
    assert!(matches!(err, Err(ContentsError::TooLargeOutput(2000, 1500))), "{err:?}");
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn extract_reports_unreadable_items_and_goes_on() {
    let lumps: &[(&str, &[u8])] = &[("GOOD", b"abc"), ("BAD", b"defg")];
    let mut wad = testutil::wad(false, lumps);
    let dir = u32::from_le_bytes(wad[8..12].try_into().unwrap()) as usize;
    wad[dir + 16..dir + 20].copy_from_slice(&0x7000_0000u32.to_le_bytes());
    let c = Contents::from_bytes(wad).unwrap();
    let out = temp("unreadable");
    let ids: Vec<ItemId> = c.items().iter().map(|i| i.id).collect();
    let report = extract(&c, &ids, &out, &ExtractOptions::default()).unwrap();
    assert_eq!(report.written.len(), 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "BAD");
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn flat_and_stripped_extraction_for_pasting() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let flat = temp("flat");
    let ids = [item_named(&c, "MAP01/THINGS"), item_named(&c, "MAP02/THINGS"), item_named(&c, "Sprites/TROOA1")];
    extract(&c, &ids, &flat, &ExtractOptions { flat: true, ..Default::default() }).unwrap();
    assert_eq!(files_under(&flat), ["THINGS.lmp", "THINGS~2.lmp", "TROOA1.png"]);
    let strip = temp("strip");
    let beneath = c.items_in("MAP01");
    extract(&c, &beneath, &strip, &ExtractOptions { strip: 0, ..Default::default() }).unwrap();
    assert_eq!(files_under(&strip), ["MAP01/LINEDEFS.lmp", "MAP01/THINGS.lmp"]);
    for d in [flat, strip] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn materialize_gives_paths_for_a_file_manager() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let dir = temp("materialize");
    let selection = [
        Selected::Item(item_named(&c, "TITLEPIC")),
        Selected::Folder("MAP01".into()),
        Selected::Item(item_named(&c, "DSTEST")),
        Selected::Folder("Sprites".into()),
    ];
    let paths = c.materialize(&selection, &ExtractOptions::default(), &dir).unwrap();
    let shown: Vec<String> =
        paths.iter().map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(shown, ["TITLEPIC.png", "DSTEST.wav", "MAP01", "Sprites"]);
    assert!(dir.join("MAP01/THINGS.lmp").is_file());
    assert!(dir.join("Sprites/TROOA1.png").is_file());
    assert_eq!(files_under(&dir).len(), 2 + 2 + 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn listing_is_a_tree() {
    let c = Contents::from_bytes(sample_wad()).unwrap();
    let text = list_text(&c, false);
    assert!(text.starts_with("Doom IWAD: 16 items"), "{text}");
    assert!(text.contains("\nSprites/\n  S_START"), "{text}");
    assert!(text.contains("\nMAP01/\n  MAP01"), "{text}");
    assert!(text.lines().any(|l| l.starts_with("PLAYPAL") && l.contains("768") && l.contains("palette")), "{text}");
    let flat = list_text(&c, true);
    assert!(flat.lines().any(|l| l == "TITLEPIC\t16\tpicture\traw,png" || l.starts_with("TITLEPIC\t")), "{flat}");
    assert!(flat.contains("Sprites/TROOA1\t"));
}

//endregion

/*┌─────────────────╮
| Temporary files |
└─────────────────╯*/
//region Temporary files

#[test]
fn temporary_batches_are_private_and_cleaned() {
    let dir = temp("runtime");
    // SAFETY: tests that set the environment run on their own thread of a
    // process that doesn't read it elsewhere meanwhile.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
    let t = TempFolder::new().unwrap();
    let a = t.batch().unwrap();
    let b = t.batch().unwrap();
    assert_ne!(a, b);
    assert!(a.starts_with(t.root()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
    }
    // A stale folder of a dead process goes at the next start; this
    // process's stays.
    let stale = t.root().parent().unwrap().join("999999999");
    std::fs::create_dir_all(&stale).unwrap();
    clean_stale();
    assert!(!stale.exists() || !cfg!(target_os = "linux"));
    assert!(t.root().exists());
    t.clean();
    assert!(!t.root().exists());
    let _ = std::fs::remove_dir_all(dir);
}

//endregion

#[test]
fn damaged_archives_never_panic_through_the_contents_api() {
    let wad = sample_wad();
    let mut seed = 7u32;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..300 {
        let mut m = wad.clone();
        let at = rand() as usize % m.len();
        m[at] = rand() as u8;
        if i % 3 == 0 {
            m.truncate(rand() as usize % m.len());
        }
        let run = std::panic::catch_unwind(|| {
            if let Ok(c) = Contents::from_bytes(m) {
                for item in c.items() {
                    for f in Format::CHOICES {
                        let _ = c.export(item.id, f);
                    }
                    let _ = c.preview(item.id);
                }
            }
        });
        assert!(run.is_ok(), "mutation {i} panicked");
    }
}
