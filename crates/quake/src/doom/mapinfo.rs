//! MAPINFO, ZMAPINFO and UMAPINFO: what source ports know about each map
//! beyond its lumps.  Only what changes how a map looks is kept: its title,
//! sky textures, fog colours and densities, light mode and wall shading,
//! plus the `DoomEdNums` that name the things a mod adds.
//!
//! Two syntaxes exist.  The old one puts each setting on its own line after
//! `map MAP01 "Title"`; the new one wraps them in braces with `=`.  Both are
//! read here, along with `defaultmap` and `adddefaultmap`, whose settings
//! every later map starts from, and `include`.

use super::script::{Scanner, Tok};
use super::textures::color;
use std::collections::HashMap;

/// One map's settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MapInfo {
    pub title: Option<String>,
    pub sky1: Option<String>,
    pub sky2: Option<String>,
    /// The fog colour of sectors that name none (MAPINFO's `fade`).
    pub fade: Option<[f32; 3]>,
    /// The fog colour of sectors under the sky.
    pub outside_fog: Option<[f32; 3]>,
    pub fog_density: Option<f64>,
    pub outside_fog_density: Option<f64>,
    pub sky_fog: Option<f64>,
    /// GZDoom's light mode: 0 standard, 1 bright, 2 Doom, 3 dark, 4
    /// Legacy, 8 software, 16 vanilla.
    pub light_mode: Option<i32>,
    /// No fake contrast on walls.
    pub even_lighting: bool,
    /// Fake contrast by the wall's angle rather than in two steps.
    pub smooth_lighting: bool,
    /// Light added to walls running north-south and east-west (MAPINFO's
    /// `vertwallshade` and `horizwallshade`).
    pub wall_vert_light: Option<i32>,
    pub wall_horiz_light: Option<i32>,
    /// No distance fading of black fog.
    pub no_light_fade: bool,
}

#[derive(Default, Debug)]
pub(crate) struct Infos {
    /// Maps in the order they were defined, by upper-case lump name.
    pub maps: Vec<(String, MapInfo)>,
    /// Class names of editor numbers (ZMAPINFO's `DoomEdNums`).
    pub ednums: HashMap<u32, String>,
    defaults: MapInfo,
}

/// Most nested includes, and most maps kept.
const MAX_DEPTH: usize = 8;
const MAX_MAPS: usize = 4096;

impl Infos {
    /// A map's settings, when MAPINFO defines it.
    pub fn map(&self, name: &str) -> Option<&MapInfo> {
        self.maps.iter().rev().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, m)| m)
    }

    /// Reads one MAPINFO-family lump.  `include` returns the text of a lump
    /// an `include` names.
    pub fn parse(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>) {
        self.parse_at(text, include, 0);
    }

    fn parse_at(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>, depth: usize) {
        let mut sc = Scanner::new(text);
        while let Some(t) = sc.next() {
            let Some(word) = t.text().map(str::to_ascii_lowercase) else {
                if t.is_sym(b'{') {
                    sc.skip_block();
                }
                continue;
            };
            match word.as_str() {
                "include" => {
                    if let Some(name) = sc.text()
                        && depth < MAX_DEPTH
                        && let Some(sub) = include(&name)
                    {
                        self.parse_at(&sub, include, depth + 1);
                    }
                }
                "map" => {
                    let Some(lump) = sc.text() else { continue };
                    let mut info = self.defaults.clone();
                    // The title: a string, or `lookup "KEY"` (a LANGUAGE
                    // key, kept as is).
                    match sc.next() {
                        Some(Tok::Quoted(s)) => info.title = Some(s),
                        Some(t) if t.is("lookup") => info.title = sc.text(),
                        Some(t) => sc.unget(t),
                        None => {}
                    }
                    read_settings(&mut sc, &mut info);
                    if self.maps.len() < MAX_MAPS {
                        self.maps.push((lump.to_ascii_uppercase(), info));
                    }
                }
                "defaultmap" => {
                    let mut info = MapInfo::default();
                    read_settings(&mut sc, &mut info);
                    self.defaults = info;
                }
                "adddefaultmap" => {
                    let mut info = std::mem::take(&mut self.defaults);
                    read_settings(&mut sc, &mut info);
                    self.defaults = info;
                }
                "doomednums" => {
                    if sc.eat(b'{') {
                        self.ednums(&mut sc);
                    }
                }
                _ => {
                    // Other blocks (episodes, clusters, skills, gameinfo):
                    // skip their braces.
                    while let Some(t) = sc.next() {
                        if t.is_sym(b'{') {
                            sc.skip_block();
                            break;
                        }
                        if t.text().is_some_and(is_top_level) {
                            sc.unget(t);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Reads `number = ClassName[, args...]` pairs up to the closing brace.
    fn ednums(&mut self, sc: &mut Scanner) {
        while let Some(t) = sc.next() {
            if t.is_sym(b'}') {
                return;
            }
            let Some(n) = t.number() else { continue };
            if !sc.eat(b'=') {
                continue;
            }
            let Some(class) = sc.text() else { continue };
            while sc.eat(b',') {
                let _ = sc.next();
            }
            if (0.0..=1e6).contains(&n) && self.ednums.len() < 1 << 16 {
                self.ednums.insert(n as u32, class);
            }
        }
    }
}

/// Words that start a top-level definition, which end an old-style map's
/// settings.
fn is_top_level(w: &str) -> bool {
    matches!(
        w.to_ascii_lowercase().as_str(),
        "map"
            | "defaultmap"
            | "adddefaultmap"
            | "episode"
            | "clearepisodes"
            | "cluster"
            | "skill"
            | "clearskills"
            | "gameinfo"
            | "intermission"
            | "include"
            | "doomednums"
            | "spawnnums"
            | "conversationids"
            | "automap"
            | "automap_overlay"
            | "damagetype"
    )
}

/// Reads a map's settings: a braced block (new style) or lines up to the
/// next top-level word (old style).
fn read_settings(sc: &mut Scanner, info: &mut MapInfo) {
    let braced = sc.eat(b'{');
    loop {
        let Some(t) = sc.next() else { return };
        if braced && t.is_sym(b'}') {
            return;
        }
        if t.is_sym(b'{') {
            sc.skip_block();
            continue;
        }
        let Some(key) = t.text().map(str::to_ascii_lowercase) else { continue };
        if !braced && is_top_level(&key) {
            sc.unget(t);
            return;
        }
        // Values: after `=` (new style) or up to the end of the line (old
        // style), separated by commas or spaces.
        let mut values: Vec<String> = Vec::new();
        if sc.eat(b'=') {
            loop {
                match sc.next() {
                    Some(Tok::Word(w)) | Some(Tok::Quoted(w)) => values.push(w),
                    Some(t) => {
                        sc.unget(t);
                        break;
                    }
                    None => break,
                }
                if !sc.eat(b',') {
                    break;
                }
            }
        } else if !braced {
            while !sc.at_line_start() {
                match sc.next() {
                    Some(Tok::Word(w)) | Some(Tok::Quoted(w)) => values.push(w),
                    Some(Tok::Sym(b',')) => {}
                    Some(t) => {
                        sc.unget(t);
                        break;
                    }
                    None => break,
                }
            }
        }
        apply(info, &key, &values);
    }
}

fn apply(info: &mut MapInfo, key: &str, values: &[String]) {
    let first = values.first().map(String::as_str);
    let num = || first.and_then(super::script::parse_number);
    let colour = || {
        // `fade = "00 00 00"`, `fade = 000000`, or three numbers in the
        // old style.
        if values.len() == 3 { color(&values.join(" ")) } else { first.and_then(color) }
    };
    match key {
        "levelname" => info.title = first.map(str::to_string),
        "sky1" | "skytexture" => info.sky1 = first.map(str::to_ascii_uppercase),
        "sky2" => info.sky2 = first.map(str::to_ascii_uppercase),
        "fade" => info.fade = colour(),
        "outsidefog" => info.outside_fog = colour(),
        "fogdensity" => info.fog_density = num(),
        "outsidefogdensity" => info.outside_fog_density = num(),
        "skyfog" => info.sky_fog = num(),
        "lightmode" => info.light_mode = num().map(|v| v as i32),
        "evenlighting" => info.even_lighting = true,
        "smoothlighting" => info.smooth_lighting = true,
        "vertwallshade" => info.wall_vert_light = num().map(|v| v as i32),
        "horizwallshade" => info.wall_horiz_light = num().map(|v| v as i32),
        "nolightfade" => info.no_light_fade = true,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_old_syntax() {
        let mut infos = Infos::default();
        infos.parse(
            br##"
            gameinfo { titlemusic = "X" }
            defaultmap { fade = "10 20 30" lightmode = 2 }
            map e1 "Heart of the Feathered Serpent" {
                levelnum = 2
                sky1 = "EARTH1"
                nointermission
                Next="e2"
                skyfog = 16
                EvenLighting
            }
            adddefaultmap { outsidefog = "#ff0000" }
            map MAP02 lookup "HUSTR_2" { sky1 = "SKY2", 0.5 fogdensity = 64 }
            DoomEdNums { 16050 = AirSkyBoxCloudCannon 9001 = MapSpot, 1 }
            clearepisodes
            episode e1 { name = "x" }
            "##,
            &mut |_| None,
        );
        infos.parse(
            b"map MAP03 \"Old\"\nsky1 SKY3 0\nfade 0a0b0c\nlightmode 8\nevenlighting\nmap MAP04 \"Next\"\ncluster 5\n",
            &mut |_| None,
        );
        let e1 = infos.map("E1").unwrap();
        assert_eq!(e1.title.as_deref(), Some("Heart of the Feathered Serpent"));
        assert_eq!(e1.sky1.as_deref(), Some("EARTH1"));
        assert_eq!((e1.sky_fog, e1.light_mode, e1.even_lighting), (Some(16.0), Some(2), true));
        assert_eq!(e1.fade, color("10 20 30"));
        assert_eq!(e1.outside_fog, None);
        let m2 = infos.map("map02").unwrap();
        assert_eq!(
            (m2.title.as_deref(), m2.sky1.as_deref(), m2.fog_density),
            (Some("HUSTR_2"), Some("SKY2"), Some(64.0))
        );
        assert_eq!(m2.outside_fog, Some([1.0, 0.0, 0.0]));
        assert_eq!(m2.light_mode, Some(2), "defaults carry over");
        let m3 = infos.map("MAP03").unwrap();
        assert_eq!((m3.sky1.as_deref(), m3.light_mode, m3.even_lighting), (Some("SKY3"), Some(8), true));
        assert_eq!(m3.fade, color("0a0b0c"));
        assert_eq!(infos.map("MAP04").unwrap().title.as_deref(), Some("Next"));
        assert_eq!(infos.ednums.get(&16050).map(String::as_str), Some("AirSkyBoxCloudCannon"));
        assert_eq!(infos.ednums.get(&9001).map(String::as_str), Some("MapSpot"));
        let order: Vec<&str> = infos.maps.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(order, ["E1", "MAP02", "MAP03", "MAP04"]);
    }

    #[test]
    fn broken_mapinfo_never_panics() {
        let text = br##"map E1 "x" { sky1 = "A", fade = include "y" } defaultmap { "##;
        for cut in 0..text.len() {
            let mut infos = Infos::default();
            infos.parse(&text[..cut], &mut |_| Some(b"include \"y\" map Q { }".to_vec()));
        }
    }
}
