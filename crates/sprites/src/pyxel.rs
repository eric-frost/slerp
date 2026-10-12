//! Pyxel Edit documents (`.pyxel`): a zip holding `docData.json` and one PNG
//! per layer (`layer0.png`, `layer1.png`, ...) the size of the canvas, plus
//! the tileset's tiles (`tile0.png`, ...).  The layer pictures already
//! hold every tile placed on them, so the canvas is those pictures laid over
//! each other.  The document keeps no preview picture, so thumbnails show
//! that composite too.
//!
//! `docData.json` gives the canvas size and the layers under
//! `canvas.layers`, keyed `"0"`, `"1"`, ... from the top layer down, each
//! with a name, `hidden`, an `alpha` of 0 to 255 and a `blendMode`
//! (`normal`, `multiply`, `add`, `subtract`, `difference`, `darken`,
//! `lighten`, `screen`, `overlay`, `hardlight`, `invert`).  Files from 0.4.9
//! on mark groups with a `type` and put layers in them with `parentIndex`.
//! The tile animations are not played.

use crate::blend::Blend;
use crate::pixels::{self, MAX_PIXELS};
use crate::stack::{Mode, Stack, StackLayer};
use serde_json::Value;
use slerp_archive::Archive;
use slerp_scene::{CanvasLayer, Info, Limits, LoadError};
use std::sync::Arc;

/// Most layers read.
const MAX_LAYERS: usize = 4096;

/// The largest `docData.json` and layer picture read.
const MAX_MEMBER: u64 = 64 << 20;

/// Is this a Pyxel Edit document: a zip named `.pyxel`, or one whose first
/// member is `docData.json`?
pub fn sniff(ext: &str, head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04")
        && (ext == "pyxel" || slerp_archive::first_zip_name(head).is_some_and(|n| n == "docData.json"))
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("Pyxel Edit document: {what}"))
}

pub fn load(bytes: &[u8], _limits: &Limits) -> Result<Stack, LoadError> {
    let zip = Archive::from_bytes(bytes.to_vec()).map_err(|e| bad(&e.to_string()))?;
    let read = |name: &str| -> Result<Option<Vec<u8>>, LoadError> {
        let Some(&i) = zip.find(name).last() else { return Ok(None) };
        let entry = zip.entry(i).ok_or_else(|| bad("a damaged directory"))?;
        if entry.size > MAX_MEMBER {
            return Err(LoadError::TooLarge(format!("{name} is {} bytes", entry.size)));
        }
        zip.read(entry).map(Some).map_err(|e| bad(&e.to_string()))
    };
    let doc: Value = serde_json::from_slice(&read("docData.json")?.ok_or_else(|| bad("no docData.json"))?)
        .map_err(|e| bad(&e.to_string()))?;
    let canvas = doc.get("canvas").ok_or_else(|| bad("no canvas"))?;
    let size = |key: &str| canvas.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (width, height) = (size("width"), size("height"));
    if width == 0 || height == 0 || width > 1 << 15 || height > 1 << 15 || width * height > MAX_PIXELS {
        return Err(bad(&format!("a canvas of {width} × {height}")));
    }
    let (width, height) = (width as u32, height as u32);
    let entries = canvas.get("layers").and_then(Value::as_object).ok_or_else(|| bad("no layers"))?;
    let count = (canvas.get("numLayers").and_then(Value::as_u64).unwrap_or(entries.len() as u64) as usize)
        .min(entries.len())
        .min(MAX_LAYERS);
    // 1. The layers, top first as the file numbers them, and the pictures
    //    within a budget of memory.
    let mut budget = MAX_PIXELS;
    let mut top_first = Vec::with_capacity(count);
    let mut warnings = Vec::new();
    for i in 0..count {
        let Some(layer) = entries.get(&i.to_string()) else { continue };
        let group = layer.get("type").and_then(Value::as_str).is_some_and(|t| t.contains("group"));
        let picture = if group {
            None
        } else {
            match read(&format!("layer{i}.png"))? {
                Some(png) if budget > 0 => {
                    let img = pixels::decode(&png, budget.min(MAX_PIXELS))?;
                    budget = budget.saturating_sub(img.width as u64 * img.height as u64);
                    if (img.width, img.height) != (width, height) {
                        warnings.push(format!("layer {i}'s picture isn't the canvas's size"));
                    }
                    Some(Arc::new(img))
                }
                Some(_) => {
                    warnings.push("too many layers to compose them all".into());
                    None
                }
                None => None,
            }
        };
        top_first.push((i, layer, group, picture));
    }
    // 2. Bottom first, as canvases list layers, with each layer's groups
    //    found through the file's parent numbers.
    let order: Vec<usize> = top_first.iter().rev().map(|l| l.0).collect();
    let position = |file_index: i64| order.iter().position(|&i| i as i64 == file_index);
    let mut layers = Vec::with_capacity(order.len());
    let mut listed = Vec::with_capacity(order.len());
    for (i, layer, group, picture) in top_first.into_iter().rev() {
        let text = |key: &str| layer.get(key).and_then(Value::as_str).unwrap_or("");
        let mode = match text("blendMode") {
            "multiply" => Mode::Blend(Blend::Multiply),
            "add" => Mode::Blend(Blend::Addition),
            "subtract" => Mode::Blend(Blend::Subtract),
            "difference" => Mode::Blend(Blend::Difference),
            "darken" => Mode::Blend(Blend::Darken),
            "lighten" => Mode::Blend(Blend::Lighten),
            "screen" => Mode::Blend(Blend::Screen),
            "overlay" => Mode::Blend(Blend::Overlay),
            "hardlight" => Mode::Blend(Blend::HardLight),
            "invert" => Mode::Invert,
            _ => Mode::Blend(Blend::Normal),
        };
        let opacity = layer.get("alpha").and_then(Value::as_u64).unwrap_or(255).min(255) as u8;
        // The chain of groups above, stopped if the numbers loop.
        let mut parents = Vec::new();
        let mut parent = layer.get("parentIndex").and_then(Value::as_i64).unwrap_or(-1);
        while parent >= 0 && parents.len() < 64 {
            let Some(p) = position(parent) else { break };
            if parents.contains(&p) {
                break;
            }
            parents.push(p);
            parent = entries
                .get(&parent.to_string())
                .and_then(|l| l.get("parentIndex"))
                .and_then(Value::as_i64)
                .unwrap_or(-1);
        }
        let mut note = Vec::new();
        if let Mode::Blend(b) = mode
            && b != Blend::Normal
        {
            note.push(b.name().to_string());
        } else if mode == Mode::Invert {
            note.push("invert".into());
        }
        if opacity < 255 {
            note.push(format!("{:.0} %", opacity as f32 / 2.55));
        }
        listed.push(CanvasLayer {
            name: if text("name").is_empty() { format!("Layer {i}") } else { text("name").to_string() },
            visible: !layer.get("hidden").and_then(Value::as_bool).unwrap_or(false),
            depth: parents.len() as u32,
            group,
            note: note.join(", "),
        });
        layers.push(StackLayer { frames: vec![picture], opacity, mode, group, parents });
    }
    let mut info = Info {
        format: "Pyxel Edit document".into(),
        generator: Some("Pyxel Edit".into()),
        warnings,
        ..Default::default()
    };
    if let Some(v) = doc.get("version").and_then(Value::as_str) {
        info.details.push(("Saved by".into(), format!("Pyxel Edit {v}")));
    }
    if let Some(t) = doc.get("tileset") {
        let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
        info.details.push(("Tiles".into(), format!("{} of {} × {}", n("numTiles"), n("tileWidth"), n("tileHeight"))));
    }
    Ok(Stack { info, width, height, layers, listed, durations: vec![0.1], clips: Vec::new() })
}
