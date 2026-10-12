//! Tests for every format: listing, structure, reading, and damage.

use crate::testutil::{self, ZipOptions};
use crate::{Archive, ArchiveError, ArchiveKind, DataKind, EntryKind, GroupKind, Limits, Namespace};
use std::sync::Arc;

fn open(bytes: &[u8]) -> Archive {
    Archive::from_bytes(Arc::<[u8]>::from(bytes)).unwrap()
}

fn paths(a: &Archive) -> Vec<&str> {
    a.entries().iter().map(|e| e.path.as_str()).collect()
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-archive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every mutation of `bytes` that matters: cuts at many lengths, then single
/// bytes set to 0xff, 0x7f, 0 and pseudo-random values near the start (where
/// headers are) and anywhere.
fn mutations(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let n = bytes.len();
    let step = (n / 96).max(1);
    for cut in (0..n).step_by(step).chain([n.saturating_sub(1)]) {
        out.push(bytes[..cut].to_vec());
    }
    let mut seed = 0x1234_5678u32;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..600 {
        if bytes.is_empty() {
            break;
        }
        let mut b = bytes.to_vec();
        let at = if i % 2 == 0 { rand() as usize % b.len().min(256) } else { rand() as usize % b.len() };
        b[at] = match i % 4 {
            0 => 0xff,
            1 => 0x7f,
            2 => 0,
            _ => rand() as u8,
        };
        out.push(b);
    }
    out
}

/// Opens every mutation of an archive and reads everything in it, under
/// small limits: nothing may panic.
fn survives_damage(bytes: &[u8]) {
    let limits = Limits { max_read: 1 << 20, ..Limits::default() };
    for (i, m) in mutations(bytes).into_iter().enumerate() {
        let run = std::panic::catch_unwind(|| {
            if let Ok(a) = Archive::from_bytes_with(Arc::<[u8]>::from(m.as_slice()), limits) {
                for e in a.entries() {
                    let _ = a.read(e);
                    let _ = a.read_head(e, 64);
                }
                let _ = a.read_many(&(0..a.entries().len().min(8)).collect::<Vec<_>>());
            }
        });
        assert!(run.is_ok(), "mutation {i} panicked");
    }
}

/*┌─────────────────╮
| Sniffing        |
└─────────────────╯*/
//region Sniffing

#[test]
fn magic_decides_the_format() {
    assert_eq!(crate::sniff(b"IWADxxxx"), Some(ArchiveKind::DoomWad));
    assert_eq!(crate::sniff(b"PWAD"), Some(ArchiveKind::DoomWad));
    assert_eq!(crate::sniff(b"PK\x03\x04"), Some(ArchiveKind::Zip));
    assert_eq!(crate::sniff(b"PK\x05\x06"), Some(ArchiveKind::Zip));
    assert_eq!(crate::sniff(b"7z\xBC\xAF\x27\x1C\0\x04"), Some(ArchiveKind::SevenZip));
    assert_eq!(crate::sniff(b"PACK\0\0\0\0"), Some(ArchiveKind::Pak));
    assert_eq!(crate::sniff(b"WAD2"), Some(ArchiveKind::Wad2));
    assert_eq!(crate::sniff(b"WAD3"), Some(ArchiveKind::Wad3));
    assert_eq!(crate::sniff(b"GDPC"), Some(ArchiveKind::Pck));
    assert_eq!(crate::sniff(&[0x34, 0x12, 0xAA, 0x55, 2, 0, 0, 0]), Some(ArchiveKind::Vpk));
    assert_eq!(crate::sniff(b"KenSilverman\0\0\0\0"), Some(ArchiveKind::Grp));
    assert_eq!(crate::sniff(b"\x89PNG"), None);
    assert_eq!(crate::sniff(b""), None);
}

#[test]
fn other_files_called_pak_are_not_claimed() {
    // A git pack file starts with PACK, a version and an object count.
    let mut git = b"PACK\0\0\0\x02\0\0\0\x05".to_vec();
    git.extend_from_slice(&[0x55; 200]);
    assert!(matches!(Archive::from_bytes(Arc::<[u8]>::from(git.as_slice())), Err(ArchiveError::NotMine)));
    // An Unreal-style pak ends with its magic and has no PACK in front.
    let mut unreal = vec![0u8; 300];
    unreal.extend_from_slice(&[0xE1, 0x12, 0x6F, 0x5A]);
    assert!(matches!(Archive::from_bytes(Arc::<[u8]>::from(unreal.as_slice())), Err(ArchiveError::NotMine)));
    assert!(matches!(
        Archive::from_bytes(Arc::<[u8]>::from(&b"MZ\0\0 not an archive"[..])),
        Err(ArchiveError::NotMine)
    ));
    assert!(matches!(Archive::from_bytes(Arc::<[u8]>::from(&b""[..])), Err(ArchiveError::NotMine)));
}

//endregion

/*┌─────────────────╮
| Zip             |
└─────────────────╯*/
//region Zip

#[test]
fn zip_stored_and_deflated() {
    let text = b"hello hello hello hello hello".repeat(40);
    let files: &[(&str, &[u8])] = &[("maps/MAP01.wad", b"IWAD"), ("a/b.txt", &text), ("flats/", b""), ("empty", b"")];
    for options in [
        ZipOptions::default(),
        ZipOptions { deflate: true, ..Default::default() },
        ZipOptions { deflate: true, zip64: true, ..Default::default() },
        ZipOptions { deflate: true, junk_prefix: 37, ..Default::default() },
    ] {
        let z = testutil::zip_with(files, options);
        let a = open(&z);
        assert_eq!(a.kind(), ArchiveKind::Zip);
        // Folders are left out.
        assert_eq!(paths(&a), ["maps/MAP01.wad", "a/b.txt", "empty"]);
        let e = &a.entries()[1];
        assert_eq!(e.size, text.len() as u64);
        assert_eq!(a.read(e).unwrap(), text);
        assert_eq!(a.read_head(e, 10).unwrap(), &text[..10]);
        let mut sink = Vec::new();
        assert_eq!(a.read_to(e, &mut sink).unwrap(), text.len() as u64);
        assert_eq!(sink, text);
        assert!(a.read(&a.entries()[2]).unwrap().is_empty());
        if options.deflate {
            assert!(e.stored_size < e.size);
        }
    }
}

#[test]
fn zip_names_are_cleaned_and_guessed() {
    let z = testutil::zip(&[
        ("\\sprites\\TROOA1", b"x"),
        ("/flats/FLOOR", b"x"),
        ("textures/Brick.png", b"\x89PNG\r\n\x1a\n"),
        ("sounds/x.ogg", b"OggS"),
        ("MAPINFO", b"map MAP01 \"x\""),
        ("PLAYPAL", b"x"),
    ]);
    let a = open(&z);
    assert_eq!(
        paths(&a),
        ["sprites/TROOA1", "flats/FLOOR", "textures/Brick.png", "sounds/x.ogg", "MAPINFO", "PLAYPAL"]
    );
    let e = a.entries();
    assert_eq!((e[0].namespace, e[0].kind), (Some(Namespace::Sprites), EntryKind::Sprite));
    assert_eq!((e[1].namespace, e[1].data), (Some(Namespace::Flats), DataKind::DoomFlat));
    assert_eq!(e[2].data, DataKind::Png);
    assert_eq!(e[3].kind, EntryKind::Sound);
    assert_eq!(e[4].data, DataKind::Text);
    assert_eq!(e[5].kind, EntryKind::Palette);
    assert_eq!(a.find("TEXTURES\\BRICK.PNG"), &[2]);
    assert_eq!(e[2].name(), "Brick.png");
    assert_eq!(e[2].folder(), "textures");
}

#[test]
fn zip_names_in_the_dos_code_page() {
    // A name in code page 437 (not UTF-8): "caf\x82" is "café".
    let mut z = testutil::zip(&[("cafX", b"x")]);
    for w in 0..z.len() - 4 {
        if &z[w..w + 4] == b"cafX" {
            z[w + 3] = 0x82;
        }
    }
    assert_eq!(paths(&open(&z)), ["café"]);
}

#[test]
fn zip_with_unsupported_compression_lists_but_will_not_read() {
    let mut z = testutil::zip(&[("a.txt", b"hello")]);
    // Local and central headers: method 12 (bzip2).
    for w in 0..z.len() - 4 {
        if &z[w..w + 4] == b"PK\x03\x04" {
            z[w + 8] = 12;
        }
        if &z[w..w + 4] == b"PK\x01\x02" {
            z[w + 10] = 12;
        }
    }
    let a = open(&z);
    assert!(a.entries()[0].locked);
    assert!(matches!(a.read(&a.entries()[0]), Err(ArchiveError::Encrypted(_))));
}

#[test]
fn zip_bombs_are_refused_without_allocating() {
    // A directory claiming four gigabytes from forty bytes of deflate.
    let mut z = testutil::zip_with(&[("bomb.bin", &[0u8; 64])], ZipOptions { deflate: true, ..Default::default() });
    let claimed = 4_000_000_000u32.to_le_bytes();
    for w in 0..z.len() - 4 {
        if &z[w..w + 4] == b"PK\x03\x04" {
            z[w + 22..w + 26].copy_from_slice(&claimed);
        }
        if &z[w..w + 4] == b"PK\x01\x02" {
            z[w + 24..w + 28].copy_from_slice(&claimed);
        }
    }
    let a = open(&z);
    assert_eq!(a.entries()[0].size, 4_000_000_000);
    assert!(matches!(a.read(&a.entries()[0]), Err(ArchiveError::TooLarge(_))));
    // Streaming refuses it too, on the ratio, before inflating.
    let mut sink = Vec::new();
    assert!(matches!(a.read_to(&a.entries()[0], &mut sink), Err(ArchiveError::TooLarge(_))));
    assert!(sink.is_empty());
}

#[test]
fn zip_entry_that_unpacks_to_more_than_declared_stops() {
    // 1 MB of zeros compresses to about 1 KB; the directory says 1,000.
    let big = vec![0u8; 1 << 20];
    let mut z = testutil::zip_with(&[("a.bin", &big)], ZipOptions { deflate: true, ..Default::default() });
    let claimed = 1000u32.to_le_bytes();
    for w in 0..z.len() - 4 {
        if &z[w..w + 4] == b"PK\x03\x04" {
            z[w + 22..w + 26].copy_from_slice(&claimed);
        }
        if &z[w..w + 4] == b"PK\x01\x02" {
            z[w + 24..w + 28].copy_from_slice(&claimed);
        }
    }
    let a = open(&z);
    let mut written = 0usize;
    struct Count<'a>(&'a mut usize);
    impl std::io::Write for Count<'_> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            *self.0 += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let r = a.read_to(&a.entries()[0], &mut Count(&mut written));
    assert!(matches!(r, Err(ArchiveError::Invalid(_))), "{r:?}");
    // It stopped after one output chunk, not after a megabyte.
    assert!(written <= 1000 + 65536, "{written}");
    // The first bytes can still be asked for.
    assert_eq!(a.read_head(&a.entries()[0], 100).unwrap(), vec![0u8; 100]);
}

#[test]
fn zip_that_compresses_a_lot_but_honestly_reads() {
    let zeros = vec![0u8; 50 << 20];
    let z = testutil::zip_with(&[("zeros.bin", &zeros)], ZipOptions { deflate: true, ..Default::default() });
    let a = open(&z);
    let e = &a.entries()[0];
    assert!(e.size / e.stored_size > 500);
    let mut total = 0u64;
    struct Sum<'a>(&'a mut u64);
    impl std::io::Write for Sum<'_> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            *self.0 += b.iter().map(|&x| x as u64).sum::<u64>();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(a.read_to(e, &mut Sum(&mut total)).unwrap(), zeros.len() as u64);
    assert_eq!(total, 0);
}

#[test]
fn zip_with_absurd_counts_and_sizes() {
    let z = testutil::zip(&[("a", b"x"), ("b", b"y")]);
    // An entry count far beyond the directory's bytes.
    let mut big = z.clone();
    let eocd = big.len() - 22;
    big[eocd + 8..eocd + 10].copy_from_slice(&60000u16.to_le_bytes());
    big[eocd + 10..eocd + 12].copy_from_slice(&60000u16.to_le_bytes());
    let a = open(&big);
    assert_eq!(a.entries().len(), 2);
    // A directory size past the end of the file.
    let mut far = z.clone();
    far[eocd + 12..eocd + 16].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
    assert!(Archive::from_bytes(Arc::<[u8]>::from(far.as_slice())).is_err());
    // Too many entries for the limit.
    let few = Limits { max_entries: 1, ..Limits::default() };
    assert!(matches!(Archive::from_bytes_with(Arc::<[u8]>::from(z.as_slice()), few), Err(ArchiveError::TooLarge(_))));
    // Long names are skipped, with a warning.
    let long = "x".repeat(5000);
    let z = testutil::zip(&[(long.as_str(), b"x"), ("ok", b"y")]);
    let a = open(&z);
    assert_eq!(paths(&a), ["ok"]);
    assert_eq!(a.warnings().len(), 1);
}

#[test]
fn zip_entries_that_point_outside_or_overlap() {
    let z = testutil::zip(&[("a.txt", b"0123456789"), ("b.txt", b"abcdefghij")]);
    // The second entry's local header offset points past the end.
    let mut bad = z.clone();
    let mut seen = 0;
    for w in 0..bad.len() - 4 {
        if &bad[w..w + 4] == b"PK\x01\x02" {
            seen += 1;
            if seen == 2 {
                bad[w + 42..w + 46].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
            }
        }
    }
    let a = open(&bad);
    assert_eq!(a.read(&a.entries()[0]).unwrap(), b"0123456789");
    assert!(a.read(&a.entries()[1]).is_err());
    // Both entries pointing at the same bytes is harmless.
    let mut same = z.clone();
    let mut first = [0u8; 4];
    let mut seen = 0;
    for w in 0..same.len() - 4 {
        if &same[w..w + 4] == b"PK\x01\x02" {
            seen += 1;
            if seen == 1 {
                first.copy_from_slice(&same[w + 42..w + 46]);
            } else {
                same[w + 42..w + 46].copy_from_slice(&first);
            }
        }
    }
    let a = open(&same);
    assert_eq!(a.read(&a.entries()[1]).unwrap(), a.read(&a.entries()[0]).unwrap());
}

#[test]
fn zip_survives_damage() {
    let text = b"The quick brown fox jumps over the lazy dog. ".repeat(20);
    survives_damage(&testutil::zip_with(
        &[("a.txt", &text), ("b/c.bin", &[7u8; 300]), ("d", b"")],
        ZipOptions { deflate: true, ..Default::default() },
    ));
    survives_damage(&testutil::zip_with(
        &[("a.txt", &text)],
        ZipOptions { deflate: true, zip64: true, junk_prefix: 11 },
    ));
}

//endregion

/*┌─────────────────╮
| Doom WAD        |
└─────────────────╯*/
//region Doom WAD

/// A WAD with every kind of structure.
fn doom_wad() -> Vec<u8> {
    let picture = testutil::doom_picture(2, 2, -1, 3, &[1, 2, 3, 4]);
    let flat = vec![7u8; 4096];
    let playpal = vec![0u8; 768 * 2];
    let dmx: Vec<u8> = {
        let mut d = vec![3, 0];
        d.extend_from_slice(&11025u16.to_le_bytes());
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&[128u8; 40]);
        d
    };
    testutil::wad(
        false,
        &[
            ("PLAYPAL", &playpal),
            ("DSPISTOL", &dmx),
            ("D_E1M1", b"MUS\x1a\x00\x00\x00\x00"),
            ("TITLEPIC", &picture),
            ("S_START", b""),
            ("TROOA1", &picture),
            ("TROOA1", &picture),
            ("SS_START", b""),
            ("POSSA1", &picture),
            ("SS_END", b""),
            ("S_END", b""),
            ("F_START", b""),
            ("F1_START", b""),
            ("FLOOR0_1", &flat),
            ("F1_END", b""),
            ("F_END", b""),
            ("MAP01", b""),
            ("THINGS", &[0u8; 10]),
            ("LINEDEFS", &[0u8; 14]),
            ("SIDEDEFS", b""),
            ("VERTEXES", &[0u8; 4]),
            ("MAP02", b""),
            ("THINGS", &[0u8; 10]),
            ("ENDOOM", &[0u8; 4000]),
            ("UDMF", b""),
            ("TEXTMAP", b"namespace = \"zdoom\";"),
            ("ZNODES", &[1u8; 4]),
            ("ENDMAP", b""),
            ("SNDINFO", b"$rolloff"),
        ],
    )
}

#[test]
fn wad_structure_is_shown_as_stored() {
    let a = open(&doom_wad());
    assert_eq!(a.kind(), ArchiveKind::DoomWad);
    assert_eq!(a.label(), "Doom PWAD");
    assert_eq!(
        paths(&a),
        [
            "PLAYPAL",
            "DSPISTOL",
            "D_E1M1",
            "TITLEPIC",
            "Sprites/S_START",
            "Sprites/TROOA1",
            "Sprites/TROOA1",
            "Sprites/SS_START",
            "Sprites/POSSA1",
            "Sprites/SS_END",
            "Sprites/S_END",
            "Flats/F_START",
            "Flats/F1_START",
            "Flats/FLOOR0_1",
            "Flats/F1_END",
            "Flats/F_END",
            "MAP01/MAP01",
            "MAP01/THINGS",
            "MAP01/LINEDEFS",
            "MAP01/SIDEDEFS",
            "MAP01/VERTEXES",
            "MAP02/MAP02",
            "MAP02/THINGS",
            "ENDOOM",
            "UDMF/UDMF",
            "UDMF/TEXTMAP",
            "UDMF/ZNODES",
            "UDMF/ENDMAP",
            "SNDINFO",
        ]
    );
    let e = a.entries();
    // Duplicates stay, in order; indices are the position.
    assert_eq!(e[5].index, 5);
    assert_eq!(e[6].index, 6);
    // Kinds from namespace, name and bytes.
    assert_eq!(e[0].data, DataKind::Playpal);
    assert_eq!(e[1].data, DataKind::DmxSound);
    assert_eq!(e[2].data, DataKind::Mus);
    assert_eq!(e[3].data, DataKind::DoomPicture);
    assert_eq!(e[3].kind, EntryKind::Picture);
    assert_eq!(e[4].kind, EntryKind::Marker);
    assert_eq!((e[5].kind, e[5].namespace), (EntryKind::Sprite, Some(Namespace::Sprites)));
    assert_eq!(e[13].kind, EntryKind::Flat);
    assert_eq!(e[16].kind, EntryKind::Map);
    assert_eq!(e[23].data, DataKind::Unknown);
    assert_eq!(e[28].data, DataKind::Text);
    // Groups: namespaces and maps.
    let groups: Vec<(&str, GroupKind, usize, usize)> =
        a.groups().iter().map(|g| (g.path.as_str(), g.kind, g.first, g.last)).collect();
    assert_eq!(groups[0], ("Sprites", GroupKind::Namespace(Namespace::Sprites), 4, 10));
    assert!(groups.contains(&("Flats", GroupKind::Namespace(Namespace::Flats), 11, 15)));
    assert!(groups.contains(&("MAP01", GroupKind::Map, 16, 20)));
    assert!(groups.contains(&("MAP02", GroupKind::Map, 21, 22)));
    assert!(groups.contains(&("UDMF", GroupKind::Map, 24, 27)));
    // Reading.
    assert_eq!(a.read(&e[1]).unwrap().len(), 48);
    assert_eq!(a.read(&e[17]).unwrap(), vec![0u8; 10]);
    assert!(a.read(&e[16]).unwrap().is_empty());
    assert_eq!(a.find("sprites/trooa1"), &[5, 6]);
}

#[test]
fn wad_all_the_marker_namespaces() {
    let pic = testutil::doom_picture(1, 1, 0, 0, &[5]);
    let wad = testutil::wad(
        true,
        &[
            ("P_START", b""),
            ("PP_START", b""),
            ("WALL", &pic),
            ("PP_END", b""),
            ("P_END", b""),
            ("C_START", b""),
            ("COLORMAP", &vec![0u8; 8704]),
            ("C_END", b""),
            ("TX_START", b""),
            ("TXPIC", &pic),
            ("TX_END", b""),
            ("A_START", b""),
            ("ACSLIB", b"ACS\0"),
            ("A_END", b""),
            ("V_START", b""),
            ("VOX", b"x"),
            ("V_END", b""),
            ("HI_START", b""),
            ("HIPIC", &pic),
            ("HI_END", b""),
            ("FF_START", b""),
            ("FF_END", b""),
            ("AFTER", b"abc"),
        ],
    );
    let a = open(&wad);
    assert_eq!(a.label(), "Doom IWAD");
    let folders: Vec<&str> = a.entries().iter().map(|e| e.folder()).collect();
    assert_eq!(
        folders,
        [
            "Patches",
            "Patches",
            "Patches",
            "Patches",
            "Patches",
            "Colormaps",
            "Colormaps",
            "Colormaps",
            "Textures",
            "Textures",
            "Textures",
            "ACS",
            "ACS",
            "ACS",
            "Voxels",
            "Voxels",
            "Voxels",
            "Hires",
            "Hires",
            "Hires",
            "Flats",
            "Flats",
            "",
        ]
    );
    assert_eq!(a.entries()[2].kind, EntryKind::Picture);
    assert_eq!(a.entries()[6].data, DataKind::Colormap);
    assert_eq!(a.entries()[9].data, DataKind::DoomPicture);
}

#[test]
fn wad_names_keep_their_case_and_oddities() {
    let wad = testutil::wad(false, &[("MixedCas", b"abc"), ("A\x01B", b"x"), ("12345678", b"y"), ("ÿ", b"z")]);
    let a = open(&wad);
    assert_eq!(paths(&a)[0], "MixedCas");
    // Control characters can't be in a path.
    assert_eq!(paths(&a)[1], "A?B");
    assert_eq!(paths(&a)[2], "12345678");
}

#[test]
fn wad_lumps_outside_the_file_are_locked() {
    let mut wad = testutil::wad(false, &[("A", b"abc"), ("B", b"defg")]);
    let dir = u32::from_le_bytes(wad[8..12].try_into().unwrap()) as usize;
    // Lump B starts far away.
    wad[dir + 16..dir + 20].copy_from_slice(&0x7000_0000u32.to_le_bytes());
    let a = open(&wad);
    assert_eq!(a.read(&a.entries()[0]).unwrap(), b"abc");
    assert!(a.entries()[1].locked);
    assert!(a.read(&a.entries()[1]).is_err());
    assert_eq!(a.warnings().len(), 1);
    // A negative lump count, a directory past the end, an absurd count.
    for (at, v) in [(4usize, 0xffff_ffffu32), (8, 0xffff_fff0), (4, 0x7fff_ffff), (8, 4)] {
        let mut b = testutil::wad(false, &[("A", b"abc")]);
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        assert!(Archive::from_bytes(Arc::<[u8]>::from(b.as_slice())).is_err(), "{at} {v:x}");
    }
}

#[test]
fn wad_survives_damage() {
    survives_damage(&doom_wad());
}

//endregion

/*┌─────────────────╮
| Quake and others|
└─────────────────╯*/
//region Quake and others

#[test]
fn pak_files_and_their_kinds() {
    let palette = vec![0u8; 768];
    let mut qpic = Vec::new();
    qpic.extend_from_slice(&2u32.to_le_bytes());
    qpic.extend_from_slice(&2u32.to_le_bytes());
    qpic.extend_from_slice(&[1, 2, 3, 4]);
    let pak = testutil::pak(&[
        ("gfx/palette.lmp", &palette),
        ("gfx/pause.lmp", &qpic),
        ("maps/start.bsp", &[29, 0, 0, 0, 1, 2, 3, 4]),
        ("progs/player.mdl", b"IDPO\x06\0\0\0"),
        ("sound/misc/hit.wav", b"RIFFxxxxWAVEfmt "),
        ("default.cfg", b"bind a +moveleft\n"),
        ("progs.dat", &[0u8, 1, 2, 3, 200, 201]),
    ]);
    let a = open(&pak);
    assert_eq!(a.kind(), ArchiveKind::Pak);
    let d: Vec<DataKind> = a.entries().iter().map(|e| e.data).collect();
    assert_eq!(
        d,
        [
            DataKind::Palette768,
            DataKind::QuakePicture,
            DataKind::Bsp,
            DataKind::Mdl,
            DataKind::Wav,
            DataKind::Text,
            DataKind::Unknown
        ]
    );
    assert_eq!(a.read(&a.entries()[1]).unwrap(), qpic);
    assert_eq!(a.find("GFX/Pause.LMP"), &[1]);
    survives_damage(&pak);
}

#[test]
fn pak_directory_checks() {
    let mut pak = testutil::pak(&[("a.txt", b"hello")]);
    // A directory length that is no multiple of a record.
    pak[8..12].copy_from_slice(&65u32.to_le_bytes());
    assert!(matches!(Archive::from_bytes(Arc::<[u8]>::from(pak.as_slice())), Err(ArchiveError::NotMine)));
    // A file that points outside.
    let mut pak = testutil::pak(&[("a.txt", b"hello"), ("b.txt", b"x")]);
    let dir = u32::from_le_bytes(pak[4..8].try_into().unwrap()) as usize;
    pak[dir + 64 + 56..dir + 64 + 60].copy_from_slice(&0x4000_0000u32.to_le_bytes());
    let a = open(&pak);
    assert!(a.entries()[1].locked);
    assert_eq!(a.read(&a.entries()[0]).unwrap(), b"hello");
}

#[test]
fn wad2_and_wad3_lumps_by_type() {
    let wad2 =
        testutil::wad2(false, &[("pal", b'@', &[0u8; 768]), ("wall", b'D', &[0u8; 100]), ("pic", b'B', &[0u8; 20])]);
    let a = open(&wad2);
    assert_eq!(a.kind(), ArchiveKind::Wad2);
    let d: Vec<DataKind> = a.entries().iter().map(|e| e.data).collect();
    assert_eq!(d, [DataKind::Palette768, DataKind::Miptex, DataKind::QuakePicture]);
    let wad3 = testutil::wad2(true, &[("wall", b'C', &[0u8; 100]), ("font", b'F', &[0u8; 100])]);
    let a = open(&wad3);
    assert_eq!(a.kind(), ArchiveKind::Wad3);
    let d: Vec<DataKind> = a.entries().iter().map(|e| e.data).collect();
    assert_eq!(d, [DataKind::Wad3Miptex, DataKind::Wad3Font]);
    survives_damage(&wad2);
    survives_damage(&wad3);
}

#[test]
fn grp_files() {
    let grp = testutil::grp(&[
        ("TILES000.ART", b"\x01\0\0\0abcd"),
        ("DUKE3D.CON", b"definecheat 1\n"),
        ("TITLE.MID", b"MThd"),
    ]);
    let a = open(&grp);
    assert_eq!(a.kind(), ArchiveKind::Grp);
    assert_eq!(paths(&a), ["TILES000.ART", "DUKE3D.CON", "TITLE.MID"]);
    assert_eq!(a.read(&a.entries()[1]).unwrap(), b"definecheat 1\n");
    assert_eq!(a.entries()[2].data, DataKind::Midi);
    // Sizes that overrun the file lock the file, not the archive.
    let mut grp = grp.clone();
    grp[16 + 12..16 + 16].copy_from_slice(&0x1000_0000u32.to_le_bytes());
    let a = open(&grp);
    assert!(a.entries()[0].locked);
    assert!(a.entries()[1].locked);
    survives_damage(&testutil::grp(&[("A.TXT", b"abc"), ("B.TXT", b"defg")]));
}

#[test]
fn pck_versions_1_to_4() {
    for version in 1..=4 {
        let pck = testutil::pck(version, &[("scenes/main.tscn", b"[gd_scene]"), ("icon.png", b"\x89PNG\r\n\x1a\n")]);
        let a = open(&pck);
        assert_eq!(a.kind(), ArchiveKind::Pck);
        assert_eq!(paths(&a), ["scenes/main.tscn", "icon.png"], "v{version}");
        assert_eq!(a.read(&a.entries()[0]).unwrap(), b"[gd_scene]");
        assert_eq!(a.entries()[1].data, DataKind::Png);
        assert!(a.label().contains(&format!("format {version}")));
        survives_damage(&pck);
    }
}

#[test]
fn pck_with_encrypted_directory_is_refused_with_a_message() {
    let mut pck = testutil::pck(2, &[("a.txt", b"hello")]);
    // Header flags (after the Godot version) with the directory bit set.
    pck[20..24].copy_from_slice(&1u32.to_le_bytes());
    let err = Archive::from_bytes(Arc::<[u8]>::from(pck.as_slice())).unwrap_err();
    assert!(matches!(err, ArchiveError::Encrypted(_)));
    assert!(err.to_string().contains("encrypted"));
    // An unknown format version.
    let mut pck = testutil::pck(2, &[("a.txt", b"hello")]);
    pck[4..8].copy_from_slice(&9u32.to_le_bytes());
    assert!(matches!(Archive::from_bytes(Arc::<[u8]>::from(pck.as_slice())), Err(ArchiveError::Unsupported(_))));
}

#[test]
fn vpk_versions_and_parts() {
    for version in [1, 2] {
        let dir = temp(&format!("vpk{version}"));
        let path = testutil::vpk(
            &dir,
            version,
            &[("materials/dev/dev_a.vmt", b"\"LightmappedGeneric\" { }"), ("readme.txt", b"hi")],
            &[("materials/dev/dev_a.vtf", &[7u8; 300])],
        );
        let a = Archive::open(&path).unwrap();
        assert_eq!(a.kind(), ArchiveKind::Vpk);
        let mut names = paths(&a);
        names.sort();
        assert_eq!(names, ["materials/dev/dev_a.vmt", "materials/dev/dev_a.vtf", "readme.txt"]);
        let get = |name: &str| a.read(&a.entries()[a.find(name)[0]]).unwrap();
        assert_eq!(get("Materials\\DEV\\dev_a.vmt"), b"\"LightmappedGeneric\" { }");
        assert_eq!(get("readme.txt"), b"hi");
        assert_eq!(get("materials/dev/dev_a.vtf"), vec![7u8; 300]);
        let e = &a.entries()[a.find("readme.txt")[0]];
        assert_eq!(a.read_head(e, 1).unwrap(), b"h");
        assert_eq!(a.files().len(), 2);
        // The numbered archive going missing breaks only the files in it.
        std::fs::remove_file(dir.join("pak01_000.vpk")).unwrap();
        assert!(a.read(&a.entries()[a.find("materials/dev/dev_a.vtf")[0]]).is_err());
        assert_eq!(get("readme.txt"), b"hi");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn vpk_survives_damage() {
    let dir = temp("vpkbad");
    let path = testutil::vpk(&dir, 2, &[("a/b.vmt", b"0123456789")], &[("a/c.vtf", &[1u8; 64])]);
    let good = std::fs::read(&path).unwrap();
    survives_damage(&good);
    // From disk, with the numbered archive beside it.
    let bad = dir.join("x_dir.vpk");
    for m in mutations(&good).into_iter().step_by(7) {
        std::fs::write(&bad, &m).unwrap();
        if let Ok(a) = Archive::open(&bad) {
            for e in a.entries() {
                let _ = a.read(e);
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

//endregion

/*┌─────────────────╮
| 7z              |
└─────────────────╯*/
//region 7z

fn sevenz(files: &[(&str, &[u8])]) -> Vec<u8> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let mut w = ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    let entries: Vec<ArchiveEntry> = files.iter().map(|(n, _)| ArchiveEntry::new_file(n)).collect();
    let readers: Vec<sevenz_rust2::SourceReader<std::io::Cursor<Vec<u8>>>> =
        files.iter().map(|(_, d)| std::io::Cursor::new(d.to_vec()).into()).collect();
    w.push_archive_entries(entries, readers).unwrap();
    w.finish().unwrap().into_inner()
}

#[test]
fn sevenz_reads_one_and_many() {
    let big = b"abcdefgh".repeat(10_000);
    let bytes = sevenz(&[
        ("TEXTURES", b"Texture \"W\", 64, 64 { }"),
        ("patches/wallp.png", &big),
        ("sounds/a.ogg", b"OggS..."),
    ]);
    let a = open(&bytes);
    assert_eq!(a.kind(), ArchiveKind::SevenZip);
    assert!(a.is_solid());
    assert_eq!(paths(&a), ["TEXTURES", "patches/wallp.png", "sounds/a.ogg"]);
    assert_eq!(a.read(&a.entries()[1]).unwrap(), big);
    assert_eq!(a.read(&a.entries()[2]).unwrap(), b"OggS...");
    assert_eq!(a.read_head(&a.entries()[1], 5).unwrap(), b"abcde");
    let many = a.read_many(&[2, 0, 1, 9]);
    assert_eq!(many[0].as_ref().unwrap(), b"OggS...");
    assert!(many[1].as_ref().unwrap().starts_with(b"Texture"));
    assert_eq!(many[2].as_ref().unwrap(), &big);
    assert!(many[3].is_err());
    let mut sink = Vec::new();
    assert_eq!(a.read_to(&a.entries()[1], &mut sink).unwrap(), big.len() as u64);
}

#[test]
fn sevenz_survives_damage() {
    let bytes = sevenz(&[("a.txt", b"hello hello hello"), ("b/c.bin", &[3u8; 500])]);
    for cut in (0..bytes.len()).step_by(5) {
        let r = std::panic::catch_unwind(|| {
            if let Ok(a) = Archive::from_bytes(Arc::<[u8]>::from(&bytes[..cut])) {
                let _ = a.read_many(&[0, 1]);
            }
        });
        assert!(r.is_ok(), "cut {cut} panicked");
    }
    survives_damage(&bytes);
}

//endregion

/*┌─────────────────╮
| Lump guesses    |
└─────────────────╯*/
//region Lump guesses

#[test]
fn doom_pictures_are_told_from_noise() {
    let pic = testutil::doom_picture(8, 8, 0, 0, &[3u8; 64]);
    assert_eq!(crate::doom_picture_header(&pic, pic.len()), Some((8, 8, 0, 0)));
    assert_eq!(crate::doom_picture_header(&pic[..40], pic.len()), Some((8, 8, 0, 0)));
    assert_eq!(crate::doom_picture_header(&[0u8; 64], 64), None);
    assert_eq!(crate::doom_picture_header(&[0xFFu8; 200], 200), None);
    assert_eq!(crate::doom_picture_header(b"short", 5), None);
    let text = b"This is just some text that happens to be here.\n".repeat(10);
    assert_eq!(crate::doom_picture_header(&text, text.len()), None);
    assert!(crate::looks_like_text(&text));
    assert!(!crate::looks_like_text(&[0, 1, 2, 3]));
    assert!(!crate::looks_like_text(&pic));
}

//endregion

/*┌─────────────────╮
| Unity packages  |
└─────────────────╯*/
//region Unity packages

const GUID_A: &str = "0123456789abcdef0123456789abcdef";
const GUID_B: &str = "fedcba9876543210fedcba9876543210";
const GUID_C: &str = "00000000111111112222222233333333";

#[test]
fn unity_packages_list_assets_by_path() {
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let bytes = testutil::unitypackage(&[
        (GUID_A, "Assets/Textures/brick.png", b"\x89PNG not really", Some(b"\x89PNG preview")),
        (GUID_B, "Assets/Models/ship.fbx", &big, None),
        (GUID_C, "Assets/readme.txt", b"hello", None),
    ]);
    assert!(crate::sniff(&bytes[..bytes.len().min(4096)]) == Some(ArchiveKind::UnityPackage));
    let a = open(&bytes);
    assert_eq!(a.kind(), ArchiveKind::UnityPackage);
    assert_eq!(paths(&a), ["Assets/Models/ship.fbx", "Assets/Textures/brick.png", "Assets/readme.txt"]);
    // The package is larger than tests keep in memory, so the big asset is
    // read by inflating again; the small ones too.
    assert_eq!(a.read(&a.entries()[0]).unwrap(), big);
    assert_eq!(a.read(&a.entries()[2]).unwrap(), b"hello");
    assert_eq!(a.read_head(&a.entries()[0], 10).unwrap(), &big[..10]);
    // A small package stays in memory.
    let small =
        testutil::unitypackage(&[(GUID_A, "Assets/a.txt", b"one", None), (GUID_B, "Assets/b.txt", b"two", None)]);
    let a = open(&small);
    assert_eq!(a.read(&a.entries()[1]).unwrap(), b"two");
}

#[test]
fn unity_packages_other_layouts() {
    // `./` in front of every name, a folder-only GUID, a macOS resource
    // fork, and a plain gzip'd tar that isn't a package.
    let tar = testutil::tar(&[
        ("./", b""),
        (&format!("./{GUID_A}/"), b""),
        (&format!("./{GUID_A}/pathname"), b"Assets/Folder"),
        (&format!("./{GUID_B}/pathname"), b"Assets/Folder/x.mat"),
        (&format!("./{GUID_B}/._asset"), b"junk"),
        (&format!("./{GUID_B}/asset"), b"material"),
    ]);
    let bytes = testutil::gzip(&tar);
    let a = open(&bytes);
    assert_eq!(paths(&a), ["Assets/Folder/x.mat"]);
    assert_eq!(a.read(&a.entries()[0]).unwrap(), b"material");
    let other = testutil::gzip(&testutil::tar(&[("docs/readme.txt", b"hi")]));
    assert_eq!(crate::sniff(&other), None);
    assert!(Archive::from_bytes(other).is_err());
}

#[test]
fn unity_pictures_prefer_previews() {
    let dir = temp("unity-pictures");
    let png = |n: u8| [b"\x89PNG\r\n\x1a\n".as_slice(), &[n; 8]].concat();
    let with = testutil::unitypackage(&[
        (GUID_A, "Assets/t.png", &png(1), None),
        (GUID_B, "Assets/m.mat", b"material", Some(&png(2))),
        (GUID_C, "Assets/p.prefab", b"prefab", Some(&png(3))),
    ]);
    let path = dir.join("with.unitypackage");
    std::fs::write(&path, &with).unwrap();
    let found = crate::unity::pictures(&path, 9, std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(found.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["Assets/m.mat", "Assets/p.prefab"]);
    assert!(found.iter().all(|p| p.stored_preview));
    let found = crate::unity::pictures(&path, 1, std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(found.len(), 1);
    // Without previews, the first PNG asset stands in.
    let without =
        testutil::unitypackage(&[(GUID_B, "Assets/m.mat", b"material", None), (GUID_A, "Assets/t.png", &png(1), None)]);
    std::fs::write(&path, &without).unwrap();
    let found = crate::unity::pictures(&path, 9, std::time::Duration::from_secs(5)).unwrap();
    assert_eq!((found.len(), found[0].path.as_str(), found[0].stored_preview), (1, "Assets/t.png", false));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unity_packages_survive_damage_and_bombs() {
    let bytes = testutil::unitypackage(&[
        (GUID_A, "Assets/Textures/brick.png", b"\x89PNG not really", Some(b"\x89PNG preview")),
        (GUID_B, "Assets/b.txt", &[7u8; 3000], None),
    ]);
    let dir = temp("unity-damage");
    let path = dir.join("d.unitypackage");
    for m in mutations(&bytes) {
        if let Ok(a) = Archive::from_bytes(m.clone()) {
            for e in a.entries() {
                let _ = a.read(e);
            }
        }
        std::fs::write(&path, &m).unwrap();
        let _ = crate::unity::pictures(&path, 9, std::time::Duration::from_secs(1));
    }
    // A member claiming 7 GB of zeros, which deflate packs into a few
    // kilobytes: stopped at the cap, quickly, without allocating it.
    let mut tar = testutil::tar(&[(&format!("{GUID_A}/pathname"), b"Assets/x")]);
    tar.truncate(tar.len() - 1024);
    let mut h = testutil::tar(&[(&format!("{GUID_A}/asset"), b"")]);
    h.truncate(512);
    h[124..135].copy_from_slice(format!("{:011o}", 7u64 << 30).as_bytes());
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    tar.extend_from_slice(&h);
    tar.resize(tar.len() + (32 << 20), 0);
    let bomb = testutil::gzip(&tar);
    assert!(bomb.len() < 100_000, "{}", bomb.len());
    let start = std::time::Instant::now();
    assert!(Archive::from_bytes(bomb.clone()).is_err());
    std::fs::write(&path, &bomb).unwrap();
    let _ = crate::unity::pictures(&path, 9, std::time::Duration::from_secs(5));
    assert!(start.elapsed().as_secs_f32() < 2.0, "{:?}", start.elapsed());
    let _ = std::fs::remove_dir_all(dir);
}

//endregion
