//! Finding the IWAD a PWAD or PK3 was made for, the way GZDoom finds
//! installed games: the folder of the opened file, `DOOMWADDIR` and
//! `DOOMWADPATH`, GZDoom's own search list (`[IWADSearch.Directories]` in
//! its ini), Steam libraries (every library `libraryfolders.vdf` names,
//! with the folders Doom's Steam releases install to), GOG installs, the
//! usual system folders, and the folders next to the opened file's.
//!
//! The commercial game wins over Freedoom wherever each is found: names are
//! tried in order (`doom2.wad`, then `freedoom2.wad`), and for each name the
//! folders in the order above.  Steam's original `base/` releases come
//! before its re-release (`rerelease/`), whose WADs carry extra content.

use std::path::{Path, PathBuf};

/// The IWADs to try for a map, best first: the one a mod's GAMEINFO names
/// (with its Freedoom stand-in after it), then by the map: Hexen's for
/// Hexen-format maps, Doom's for ExMy, Doom II's for MAPxx and other names
/// (most ZDoom mods are for Doom II).  Freedoom comes after every
/// commercial IWAD of the game.
pub(crate) fn names(map: &str, hexen: bool, wanted: Option<&str>) -> Vec<String> {
    const DOOM2: &[&str] = &["doom2.wad", "plutonia.wad", "tnt.wad", "doom2f.wad"];
    const DOOM: &[&str] = &["doom.wad", "doomu.wad", "doom1.wad"];
    const FREE2: &[&str] = &["freedoom2.wad", "freedm.wad"];
    const FREE1: &[&str] = &["freedoom1.wad"];
    let m = map.to_ascii_uppercase();
    let b = m.as_bytes();
    let episodic = b.len() == 4 && b[0] == b'E' && b[2] == b'M' && b[1].is_ascii_digit() && b[3].is_ascii_digit();
    let mut out: Vec<String> = Vec::new();
    if let Some(w) = wanted {
        let w = w.to_ascii_lowercase();
        let stand_in = match w.as_str() {
            "doom2.wad" | "plutonia.wad" | "tnt.wad" => Some("freedoom2.wad"),
            "doom.wad" | "doomu.wad" => Some("freedoom1.wad"),
            _ => None,
        };
        out.push(w);
        if let Some(s) = stand_in {
            out.push(s.into());
        }
    }
    let order: Vec<&[&str]> = if hexen {
        vec![&["hexen.wad"]]
    } else if episodic {
        vec![DOOM, &["heretic.wad"], DOOM2, FREE1, FREE2]
    } else {
        vec![DOOM2, DOOM, FREE2, FREE1]
    };
    for n in order.into_iter().flatten() {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// Every folder an IWAD may be in, best first.
pub(crate) fn folders(opened: &Path) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    // 1. Beside the opened file.
    let here = opened.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    dirs.push(here.to_path_buf());
    // 2. The environment, as every source port reads it.
    if let Some(v) = std::env::var_os("DOOMWADDIR") {
        dirs.push(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("DOOMWADPATH") {
        dirs.extend(std::env::split_paths(&v));
    }
    // 3. GZDoom's and UZDoom's own search lists.
    if let Some(h) = &home {
        for ini in [".config/gzdoom/gzdoom.ini", ".config/uzdoom/uzdoom.ini"] {
            if let Ok(text) = std::fs::read_to_string(h.join(ini)) {
                dirs.extend(ini_search_paths(&text, h));
            }
        }
    }
    // 4. Steam: the original releases, then the re-releases.
    let steam = steam_libraries(home.as_deref());
    let (base, rerelease): (Vec<&str>, Vec<&str>) = STEAM_APPS.iter().partition(|a| !a.contains("rerelease"));
    for apps in [base, rerelease] {
        for lib in &steam {
            for app in &apps {
                dirs.push(lib.join("steamapps/common").join(app));
            }
        }
    }
    // 5. GOG installs, by hand or through Heroic.
    if let Some(h) = &home {
        for root in ["GOG Games", "Games/Heroic", "Games"] {
            dirs.extend(game_folders(&h.join(root)));
        }
    }
    for root in ["C:\\GOG Games", "C:\\Program Files (x86)\\GOG Galaxy\\Games"] {
        dirs.extend(game_folders(Path::new(root)));
    }
    // 6. The usual install locations.
    let data_home =
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(d) = data_home {
        dirs.push(d.join("games/doom"));
    }
    for d in ["/usr/local/share/games/doom", "/usr/share/games/doom", "/usr/local/share/doom", "/usr/share/doom"] {
        dirs.push(PathBuf::from(d));
    }
    // 7. The folders next to the opened file's, as in a downloads folder
    //    with one folder per WAD.
    if let Some(parent) = here.parent()
        && let Ok(entries) = std::fs::read_dir(if parent.as_os_str().is_empty() { Path::new("..") } else { parent })
    {
        let mut siblings: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        siblings.sort();
        dirs.extend(siblings.into_iter().take(64));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Where Doom's Steam releases keep their WADs, inside a library's
/// `steamapps/common`, as GZDoom's `i_steam.cpp` lists them.
const STEAM_APPS: &[&str] = &[
    "Ultimate Doom/base",
    "Ultimate Doom/base/doom2",
    "Ultimate Doom/base/tnt",
    "Ultimate Doom/base/plutonia",
    "Doom 2/base",
    "Doom 2/finaldoombase",
    "Final Doom/base",
    "Master Levels of Doom/doom2",
    "DOOM 3 BFG Edition/base/wads",
    "Heretic Shadow of the Serpent Riders/base",
    "Hexen/base",
    "Hexen Deathkings of the Dark Citadel/base",
    "Heretic + Hexen/dos/base/heretic",
    "Heretic + Hexen/dos/base/hexen",
    "Ultimate Doom/rerelease",
    "Ultimate Doom/rerelease/DOOM_Data/StreamingAssets",
    "Doom 2/rerelease/DOOM II_Data/StreamingAssets",
];

/// Steam's library folders: the installs' own, and every library their
/// `libraryfolders.vdf` names.
fn steam_libraries(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        for r in [
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            ".var/app/com.valvesoftware.Steam/data/Steam",
            "Library/Application Support/Steam",
        ] {
            roots.push(h.join(r));
        }
    }
    roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    roots.push(PathBuf::from("C:\\Program Files\\Steam"));
    let mut libs: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, libs: &mut Vec<PathBuf>| {
        let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if p.is_dir() && !libs.iter().any(|l| std::fs::canonicalize(l).unwrap_or_else(|_| l.clone()) == key) {
            libs.push(p);
        }
    };
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        push(root.clone(), &mut libs);
        for vdf in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            if let Ok(text) = std::fs::read_to_string(root.join(vdf)) {
                for p in vdf_paths(&text) {
                    push(PathBuf::from(p), &mut libs);
                }
            }
        }
    }
    libs
}

/// The `"path"` values of a `libraryfolders.vdf`, with VDF's escaped
/// backslashes undone.
pub(crate) fn vdf_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\"path\"") {
        rest = &rest[at + 6..];
        let Some(open) = rest.find('"') else { break };
        // The value must follow on the same line.
        if rest[..open].contains('\n') {
            continue;
        }
        let value = &rest[open + 1..];
        let mut s = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => s.extend(chars.next()),
                c => s.push(c),
            }
        }
        rest = &value[s.len().min(value.len())..];
        if !s.is_empty() && out.len() < 64 {
            out.push(s);
        }
    }
    out
}

/// The paths of GZDoom's `[IWADSearch.Directories]`, with `$HOME`, `~` and
/// `$DOOMWADDIR` expanded; others (`$PROGDIR`) are left out.
pub(crate) fn ini_search_paths(text: &str, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line.eq_ignore_ascii_case("[IWADSearch.Directories]");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !inside || !key.trim().eq_ignore_ascii_case("path") {
            continue;
        }
        let value = value.trim();
        let expanded = if let Some(r) = value.strip_prefix("$HOME").or_else(|| value.strip_prefix('~')) {
            home.join(r.trim_start_matches(['/', '\\']))
        } else if let Some(r) = value.strip_prefix("$DOOMWADDIR") {
            match std::env::var_os("DOOMWADDIR") {
                Some(d) => PathBuf::from(d).join(r.trim_start_matches(['/', '\\'])),
                None => continue,
            }
        } else if value.starts_with('$') || value.is_empty() {
            continue;
        } else {
            PathBuf::from(value)
        };
        out.push(expanded);
    }
    out
}

/// The game folders below a GOG-style root whose names say Doom, Heretic,
/// Hexen or Strife, each with its immediate subfolders (`base`, `DOOM II`).
fn game_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten().take(256) {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if !["doom", "heretic", "hexen", "strife"].iter().any(|g| name.contains(g)) || !e.path().is_dir() {
            continue;
        }
        out.push(e.path());
        if let Ok(sub) = std::fs::read_dir(e.path()) {
            out.extend(sub.flatten().map(|s| s.path()).filter(|p| p.is_dir()).take(32));
        }
    }
    out.sort();
    out
}

/// The first IWAD found: names in order, and for each the folders in
/// order.  `opened` itself never counts.
pub(crate) fn find(opened: &Path, names: &[String], folders: &[PathBuf]) -> Option<(PathBuf, Vec<u8>)> {
    let me = std::fs::canonicalize(opened).ok();
    for name in names {
        for dir in folders {
            let Some(p) = crate::find_in(dir, name) else { continue };
            if me.is_some() && std::fs::canonicalize(&p).ok() == me {
                continue;
            }
            if let Some(bytes) = crate::read_neighbour(&p)
                && bytes.starts_with(b"IWAD")
            {
                return Some((p, bytes));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commercial_iwads_before_freedoom() {
        let dir = std::env::temp_dir().join(format!("slerp-iwad-{}", std::process::id()));
        let (free, steam) = (dir.join("free"), dir.join("steam/Ultimate Doom/base/doom2"));
        std::fs::create_dir_all(&free).unwrap();
        std::fs::create_dir_all(&steam).unwrap();
        let iwad = |p: &Path| std::fs::write(p, b"IWAD\0\0\0\0\x0c\0\0\0").unwrap();
        iwad(&free.join("freedoom2.wad"));
        let pwad = free.join("mod.pk3");
        let wanted = names("MAP01", false, Some("DOOM2.WAD"));
        assert_eq!(&wanted[..2], ["doom2.wad", "freedoom2.wad"]);
        // Freedoom alone: the fallback.
        let found = find(&pwad, &wanted, &[free.clone(), steam.clone()]).unwrap();
        assert!(found.0.ends_with("freedoom2.wad"));
        // Doom II anywhere beats Freedoom beside the mod; case doesn't matter
        // (a file system that ignores case, as macOS's, gives the name asked
        // for, which is the same file).
        iwad(&steam.join("DOOM2.WAD"));
        let found = find(&pwad, &wanted, &[free.clone(), steam.clone()]).unwrap();
        let real = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(real(&found.0), real(&steam.join("DOOM2.WAD")), "{:?}", found.0);
        // Not an IWAD: skipped.
        std::fs::write(steam.join("DOOM2.WAD"), b"PWAD\0\0\0\0").unwrap();
        assert!(find(&pwad, &wanted, &[free.clone(), steam.clone()]).unwrap().0.ends_with("freedoom2.wad"));
        std::fs::remove_dir_all(&dir).unwrap();
        // Episodes look for Doom's IWAD first.
        assert_eq!(names("E1M1", false, None)[0], "doom.wad");
        assert_eq!(names("e1", false, None)[0], "doom2.wad", "not an ExMy name");
        assert_eq!(names("MAP01", true, None), ["hexen.wad"]);
    }

    #[test]
    fn steam_and_gzdoom_config() {
        let vdf = r#""libraryfolders"
        {
            "0" { "path"		"/home/user/.local/share/Steam" "label" "" }
            "1" { "path"		"D:\\Games\\SteamLibrary"
                  "apps" { "2280" "0" } }
        }"#;
        assert_eq!(vdf_paths(vdf), ["/home/user/.local/share/Steam", "D:\\Games\\SteamLibrary"]);
        assert!(vdf_paths("\"path\"\n\"x\"").is_empty(), "a value on the next line is no value");
        let ini = "[GlobalSettings]\nPath=/nope\n[IWADSearch.Directories]\nPath=.\nPath=$HOME/wads\nPath=~/more\nPath=$PROGDIR\n[FileSearch.Directories]\nPath=/files\n";
        let home = Path::new("/home/x");
        assert_eq!(
            ini_search_paths(ini, home),
            [PathBuf::from("."), PathBuf::from("/home/x/wads"), PathBuf::from("/home/x/more")]
        );
        // The real folders: the opened file's first, Steam's base before
        // its re-release when Steam is installed.
        let f = folders(Path::new("/some/where/mod.pk3"));
        assert_eq!(f[0], PathBuf::from("/some/where"));
        let pos = |s: &str| f.iter().position(|d| d.ends_with(s));
        if let (Some(b), Some(r)) = (pos("Ultimate Doom/base"), pos("Ultimate Doom/rerelease")) {
            assert!(b < r);
        }
    }
}
