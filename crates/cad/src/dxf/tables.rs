//! The layer and text-style tables, and the names entities refer to them by.

use super::entities::Color;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub color: Color,
    /// Switched off, or frozen: neither is drawn.
    pub off: bool,
    pub frozen: bool,
    /// How many items were drawn on it.
    pub items: u32,
}

impl Layer {
    pub fn new(name: String) -> Layer {
        Layer { name, color: Color::Aci(7), off: false, frozen: false, items: 0 }
    }

    pub fn visible(&self) -> bool {
        !self.off && !self.frozen
    }
}

/// The layers of a drawing by index, found by name without regard to case
/// (AutoCAD ignores it).  Entities may name a layer the table lacks, which
/// gets an ordinary default one.
#[derive(Default)]
pub struct Layers {
    pub list: Vec<Layer>,
    by_name: HashMap<Vec<u8>, u32>,
    /// The last name looked up and its index: neighbouring entities mostly
    /// share a layer.
    last: Option<(Vec<u8>, u32)>,
}

/// Layers beyond this are folded into the last one, so a file can't make
/// the layer list unbounded.
const MAX_LAYERS: usize = 50_000;

impl Layers {
    pub fn new() -> Layers {
        let mut l = Layers::default();
        // Layer 0 always exists, and is index 0.
        l.id(b"0", |_| "0".to_string());
        l
    }

    /// The index of the layer called `name`, adding it (named by `decode`)
    /// when it is new.
    pub fn id(&mut self, name: &[u8], decode: impl FnOnce(&[u8]) -> String) -> u32 {
        if let Some((n, i)) = &self.last
            && n == name
        {
            return *i;
        }
        let key: Vec<u8> = name.trim_ascii().to_ascii_uppercase();
        let id = match self.by_name.get(&key) {
            Some(&i) => i,
            None if self.list.len() >= MAX_LAYERS => (self.list.len() - 1) as u32,
            None => {
                let i = self.list.len() as u32;
                self.list.push(Layer::new(decode(name.trim_ascii())));
                self.by_name.insert(key, i);
                i
            }
        };
        self.last = Some((name.to_vec(), id));
        id
    }

    pub fn get(&self, id: u32) -> Option<&Layer> {
        self.list.get(id as usize)
    }
}

/// A text style: how its font stretches and slants the letters.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub width: f64,
    pub oblique: f64,
    /// Zero when the text says its own height.
    pub height: f64,
}

impl Default for Style {
    fn default() -> Style {
        Style { width: 1.0, oblique: 0.0, height: 0.0 }
    }
}

#[derive(Default)]
pub struct Styles {
    map: HashMap<Vec<u8>, Style>,
}

impl Styles {
    pub fn set(&mut self, name: &[u8], style: Style) {
        self.map.insert(name.trim_ascii().to_ascii_uppercase(), style);
    }

    pub fn get(&self, name: &[u8]) -> Style {
        self.map.get(&name.trim_ascii().to_ascii_uppercase()).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_found_without_regard_to_case() {
        let mut l = Layers::new();
        let dec = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let a = l.id(b"Walls", dec);
        assert_eq!(l.id(b"WALLS", dec), a);
        assert_eq!(l.id(b" walls ", dec), a);
        assert_ne!(l.id(b"Doors", dec), a);
        assert_eq!(l.id(b"0", dec), 0);
        assert_eq!(l.list[a as usize].name, "Walls");
        assert_eq!(l.list.len(), 3);
    }

    #[test]
    fn layer_count_is_bounded() {
        let mut l = Layers::new();
        for i in 0..MAX_LAYERS + 100 {
            l.id(format!("L{i}").as_bytes(), |b| String::from_utf8_lossy(b).into_owned());
        }
        assert_eq!(l.list.len(), MAX_LAYERS);
    }
}
