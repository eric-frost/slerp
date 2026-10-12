//! ANIMDEFS: animated textures and flats, warping liquids and camera
//! textures.  A level here shows its start state, so an animation shows its
//! first frame: `texture X pic Y tics 8 ...` draws Y where the map says X,
//! while a `range` starts on its first texture, which is the named one.
//! Camera and canvas textures show what a camera sees and have no picture
//! of their own; they are drawn dark rather than reported missing.

use super::script::{Scanner, Tok};
use std::collections::{HashMap, HashSet};

#[derive(Default, Debug)]
pub(crate) struct Animdefs {
    /// The first frame's name, for walls (`false`) and flats (`true`).
    pub first_frame: HashMap<(bool, String), String>,
    /// Textures and flats that ripple (`warp`, `warp2`).
    pub warp: HashSet<(bool, String)>,
    /// Camera and canvas textures, by name.
    pub cameras: HashSet<String>,
}

const MAX_DEPTH: usize = 8;

impl Animdefs {
    /// Reads one ANIMDEFS lump; `include` returns a lump `#include` names.
    pub fn parse(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>) {
        self.parse_at(text, include, 0);
    }

    fn parse_at(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>, depth: usize) {
        let mut sc = Scanner::new(text);
        while let Some(t) = sc.next() {
            let Some(word) = t.text().map(str::to_ascii_lowercase) else { continue };
            match word.as_str() {
                "texture" | "flat" => self.animation(&mut sc, word == "flat"),
                "warp" | "warp2" => {
                    let flat = sc.text().is_some_and(|k| k.eq_ignore_ascii_case("flat"));
                    if let Some(name) = sc.text() {
                        self.warp.insert((flat, name.to_ascii_uppercase()));
                    }
                }
                "cameratexture" | "canvastexture" => {
                    if let Some(name) = sc.text() {
                        self.cameras.insert(name.to_ascii_uppercase());
                    }
                }
                "#include" | "include" => {
                    if let Some(name) = sc.text()
                        && depth < MAX_DEPTH
                        && let Some(sub) = include(&name)
                    {
                        self.parse_at(&sub, include, depth + 1);
                    }
                }
                // Switches, doors and sky offsets: their words are skipped
                // by the loop until the next keyword.
                _ => {}
            }
        }
    }

    /// Reads `NAME [optional] (range LAST ... | pic X tics N ...)`.
    fn animation(&mut self, sc: &mut Scanner, flat: bool) {
        let Some(mut name) = sc.text() else { return };
        if name.eq_ignore_ascii_case("optional") {
            let Some(n) = sc.text() else { return };
            name = n;
        }
        let braced = sc.eat(b'{');
        let mut first: Option<String> = None;
        while let Some(t) = sc.next() {
            if t.is_sym(b'}') && braced {
                break;
            }
            match t.text().map(str::to_ascii_lowercase).as_deref() {
                Some("pic") => {
                    let pic = sc.next();
                    if first.is_none() {
                        first = pic.as_ref().and_then(Tok::text).map(str::to_string);
                    }
                }
                Some("range" | "tics" | "rand" | "allowdecals" | "oscillate" | "random") => {}
                Some(_) if t.number().is_some() => {}
                _ => {
                    if !braced {
                        sc.unget(t);
                        break;
                    }
                }
            }
        }
        // A number counts textures from the named one, 1 being itself;
        // others aren't followed, since the order is the WADs'.
        if let Some(f) = first
            && f.parse::<f64>().is_err()
        {
            self.first_frame.insert((flat, name.to_ascii_uppercase()), f.to_ascii_uppercase());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frames_warps_and_cameras() {
        let mut a = Animdefs::default();
        a.parse(
            br##"
            flat NUKAGE1 range NUKAGE3 tics 8
            texture optional WFALL1
                pic WFALL3 tics 8
                pic WFALL1 tics 8
            texture BLOOD pic 1 tics 4 pic 2 rand 2 6
            switch doom 1 SW1BRCOM on pic SW2BRCOM tics 0
            warp flat WATER1 4
            warp2 texture LAVAWALL
            cameratexture CAMTEX 128 128 fit 64 64
            flat X { pic Y tics 1 }
            #include "more"
            "##,
            &mut |n| (n == "more").then(|| b"texture Z pic Q tics 3".to_vec()),
        );
        assert_eq!(a.first_frame.get(&(false, "WFALL1".into())).map(String::as_str), Some("WFALL3"));
        assert!(!a.first_frame.contains_key(&(true, "NUKAGE1".into())));
        assert!(!a.first_frame.contains_key(&(false, "BLOOD".into())));
        assert_eq!(a.first_frame.get(&(true, "X".into())).map(String::as_str), Some("Y"));
        assert_eq!(a.first_frame.get(&(false, "Z".into())).map(String::as_str), Some("Q"));
        assert!(a.warp.contains(&(true, "WATER1".into())));
        assert!(a.warp.contains(&(false, "LAVAWALL".into())));
        assert!(a.cameras.contains("CAMTEX"));
        let text = b"texture A pic B tics 8 warp flat cameratexture";
        for cut in 0..text.len() {
            Animdefs::default().parse(&text[..cut], &mut |_| None);
        }
    }
}
