//! GLDEFS, the hardware renderers' definitions, for the parts that change
//! what a level looks like at rest: sky boxes (six pictures around the
//! viewer, which MAPINFO's `sky1` may name), the dynamic lights attached to
//! kinds of things (listed as markers rather than lit), and flats that
//! glow.

use super::script::Scanner;
use super::textures::color;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Skybox {
    /// North, east, south, west, top, bottom, as GLDEFS lists them; three
    /// faces mean one picture wrapped around the sides, then top and
    /// bottom.
    pub faces: Vec<String>,
    /// The top picture is upside down.
    pub flip_top: bool,
}

#[derive(Default, Debug)]
pub(crate) struct Gldefs {
    pub skyboxes: HashMap<String, Skybox>,
    /// Light colours by light name.
    pub lights: HashMap<String, [f32; 3]>,
    /// The lights each class of thing carries, by upper-case class name.
    pub objects: HashMap<String, Vec<String>>,
    /// Flats and walls that glow (GLDEFS `glow`), by name.
    pub glows: HashMap<String, Option<[f32; 3]>>,
}

const MAX_DEPTH: usize = 8;

impl Gldefs {
    pub fn parse(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>) {
        self.parse_at(text, include, 0);
    }

    fn parse_at(&mut self, text: &[u8], include: &mut dyn FnMut(&str) -> Option<Vec<u8>>, depth: usize) {
        let mut sc = Scanner::new(text);
        while let Some(t) = sc.next() {
            if t.is_sym(b'{') {
                sc.skip_block();
                continue;
            }
            let Some(word) = t.text().map(str::to_ascii_lowercase) else { continue };
            match word.as_str() {
                "#include" => {
                    if let Some(name) = sc.text()
                        && depth < MAX_DEPTH
                        && let Some(sub) = include(&name)
                    {
                        self.parse_at(&sub, include, depth + 1);
                    }
                }
                "skybox" => {
                    let Some(name) = sc.text() else { continue };
                    let mut flip_top = false;
                    while !sc.eat(b'{') {
                        match sc.text() {
                            Some(w) if w.eq_ignore_ascii_case("fliptop") => flip_top = true,
                            Some(_) => {}
                            None => break,
                        }
                    }
                    let mut faces = Vec::new();
                    while let Some(t) = sc.next() {
                        if t.is_sym(b'}') {
                            break;
                        }
                        if let Some(f) = t.text()
                            && faces.len() < 6
                        {
                            faces.push(f.to_ascii_uppercase());
                        }
                    }
                    if faces.len() == 3 || faces.len() == 6 {
                        self.skyboxes.insert(name.to_ascii_uppercase(), Skybox { faces, flip_top });
                    }
                }
                "pointlight" | "pulselight" | "flickerlight" | "flickerlight2" | "sectorlight" | "spotlight" => {
                    let Some(name) = sc.text() else { continue };
                    if !sc.eat(b'{') {
                        continue;
                    }
                    let mut rgb = [1.0f32; 3];
                    let mut depth = 1;
                    while let Some(t) = sc.next() {
                        if t.is_sym(b'{') {
                            depth += 1;
                        } else if t.is_sym(b'}') {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        } else if t.is("color") {
                            for c in &mut rgb {
                                *c = sc.number().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
                            }
                        }
                    }
                    if self.lights.len() < 1 << 16 {
                        self.lights.insert(name.to_ascii_uppercase(), rgb);
                    }
                }
                "object" => {
                    let Some(class) = sc.text() else { continue };
                    if !sc.eat(b'{') {
                        continue;
                    }
                    let mut names: Vec<String> = Vec::new();
                    let mut depth = 1;
                    while let Some(t) = sc.next() {
                        if t.is_sym(b'{') {
                            depth += 1;
                        } else if t.is_sym(b'}') {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        } else if t.is("light")
                            && let Some(n) = sc.text()
                        {
                            let n = n.to_ascii_uppercase();
                            if !names.contains(&n) && names.len() < 64 {
                                names.push(n);
                            }
                        }
                    }
                    if !names.is_empty() && self.objects.len() < 1 << 16 {
                        self.objects.insert(class.to_ascii_uppercase(), names);
                    }
                }
                "glow" => {
                    if sc.eat(b'{') {
                        self.glow(&mut sc);
                    }
                }
                _ => {}
            }
        }
    }

    /// Reads a `glow` block: `flats { A B }`, `walls { C }` and
    /// `texture "NAME", colour[, height]` lines.
    fn glow(&mut self, sc: &mut Scanner) {
        while let Some(t) = sc.next() {
            if t.is_sym(b'}') {
                return;
            }
            if t.is("flats") || t.is("walls") {
                if !sc.eat(b'{') {
                    continue;
                }
                while let Some(n) = sc.next() {
                    if n.is_sym(b'}') {
                        break;
                    }
                    if let Some(name) = n.text() {
                        self.glows.insert(name.to_ascii_uppercase(), None);
                    }
                }
            } else if t.is("texture") {
                let Some(name) = sc.text() else { continue };
                let mut rgb = None;
                if sc.eat(b',') {
                    rgb = sc.text().and_then(|c| color(&c));
                }
                while sc.eat(b',') {
                    let _ = sc.next();
                }
                self.glows.insert(name.to_ascii_uppercase(), rgb);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skyboxes_lights_and_glows() {
        let mut g = Gldefs::default();
        g.parse(
            br##"
            #include "lights.txt"
            Skybox EARTH1 fliptop { e1sky-rt e1sky-ft e1sky-lf e1sky-bk e1sky-up e1sky-dn }
            skybox THREE { side top bottom }
            skybox BAD { a b }
            FlickerLight2 CANDLE { Color 1.0 0.5 0.25 Size 24 Offset 0 0 32 }
            Object Candlestick { Frame CAND { light CANDLE } Frame CANB { light CANDLE } }
            glow { flats { LAVA1 NUKAGE1 } walls { FIREWALL } texture "BLOOD1", "ff 00 00", 32 }
            material texture "X" { normal "y" }
            brightmap texture "Z" { map "w" }
            "##,
            &mut |n| (n == "lights.txt").then(|| b"pointlight P { color 0 1 0 size 8 }".to_vec()),
        );
        let s = &g.skyboxes["EARTH1"];
        assert!(s.flip_top);
        assert_eq!(s.faces, ["E1SKY-RT", "E1SKY-FT", "E1SKY-LF", "E1SKY-BK", "E1SKY-UP", "E1SKY-DN"]);
        assert_eq!(g.skyboxes["THREE"].faces.len(), 3);
        assert!(!g.skyboxes.contains_key("BAD"));
        assert_eq!(g.lights["CANDLE"], [1.0, 0.5, 0.25]);
        assert_eq!(g.lights["P"], [0.0, 1.0, 0.0]);
        assert_eq!(g.objects["CANDLESTICK"], ["CANDLE"]);
        assert_eq!(g.glows.get("LAVA1"), Some(&None));
        assert_eq!(g.glows.get("BLOOD1"), Some(&Some([1.0, 0.0, 0.0])));
        assert!(g.glows.contains_key("FIREWALL"));
        let text = b"skybox A fliptop { a b c pointlight X { color 1";
        for cut in 0..text.len() {
            Gldefs::default().parse(&text[..cut], &mut |_| None);
        }
    }
}
