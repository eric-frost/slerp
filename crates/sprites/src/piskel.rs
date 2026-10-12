//! Piskel sprites (`.piskel`), the browser pixel-art editor's documents:
//! JSON whose `piskel` object gives the size, the frame rate and a list of
//! layers, each layer itself a JSON string.  The file carries no preview
//! picture of its own, so thumbnails compose its first frame.
//!
//! - Model version 2 (Piskel since 2015): a layer has a name, an opacity
//!   and its frames as PNG sprite sheets in data URLs.  Older files hold
//!   one sheet with the frames side by side (`base64PNG`); newer ones a
//!   list of `chunks`, each a sheet with a `layout` giving the frame index
//!   at each column and row.  `hiddenFrames` lists frames left out of the
//!   animation.
//! - Model version 1: a layer's frames are JSON grids of colour strings,
//!   `grid[x][y]`.  Version 0 files are a bare list of such grids.
//!
//! Layers run bottom first, as Piskel draws them, and have no hidden state.
//! The layout follows Piskel's own deserializers (Apache-2.0).

use crate::blend::Blend;
use crate::pixels::{self, MAX_PIXELS};
use crate::stack::{Mode, Stack, StackLayer};
use base64::Engine;
use serde_json::Value;
use slerp_scene::{CanvasLayer, Image8, Info, Limits, LoadError};
use std::sync::Arc;

/// Most layers and frames read.
const MAX_LAYERS: usize = 1024;
const MAX_FRAMES: usize = 4096;

/// Is this a Piskel file: JSON naming its model version and a `piskel`
/// object near the start, or a version 0 list named `.piskel`?
pub fn sniff(ext: &str, text: &str) -> bool {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    (text.starts_with('{') && text.contains("\"modelVersion\"") && text.contains("\"piskel\""))
        || (ext == "piskel" && text.starts_with('['))
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("Piskel file: {what}"))
}

pub fn load(bytes: &[u8], limits: &Limits) -> Result<Stack, LoadError> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
    // 1. Version 0: frames of one layer, at the default 12 frames a second.
    if let Value::Array(grids) = &doc {
        let frames = grids.iter().take(MAX_FRAMES).map(grid).collect::<Result<Vec<_>, _>>()?;
        let (w, h) = frames.first().map_or((0, 0), |f| (f.width, f.height));
        return finish("0", w, h, 12.0, vec![("Layer 1".into(), 1.0, frames)], &[], limits);
    }
    let version = doc.get("modelVersion").and_then(Value::as_u64).unwrap_or(0);
    let piskel = doc.get("piskel").ok_or_else(|| bad("no piskel object"))?;
    let width = piskel.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = piskel.get("height").and_then(Value::as_u64).unwrap_or(0);
    if width == 0 || height == 0 || width > 1 << 15 || height > 1 << 15 || width * height > MAX_PIXELS {
        return Err(bad(&format!("a size of {width} × {height}")));
    }
    let (width, height) = (width as u32, height as u32);
    let fps = piskel.get("fps").and_then(Value::as_f64).filter(|f| f.is_finite() && *f > 0.0).unwrap_or(12.0);
    let hidden: Vec<usize> = piskel
        .get("hiddenFrames")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect())
        .unwrap_or_default();
    // 2. The layers, each a JSON string (or, leniently, an object).
    let list = piskel.get("layers").and_then(Value::as_array).ok_or_else(|| bad("no layers"))?;
    let mut layers = Vec::new();
    // Thumbnails need only the first frame; the budget counts every
    // picture kept.
    let mut budget = MAX_PIXELS;
    let first_shown = (0..MAX_FRAMES).find(|f| !hidden.contains(f)).unwrap_or(0);
    for (i, entry) in list.iter().take(MAX_LAYERS).enumerate() {
        let parsed;
        let layer = match entry {
            Value::String(s) => {
                parsed = serde_json::from_str::<Value>(s).map_err(|e| bad(&e.to_string()))?;
                &parsed
            }
            other => other,
        };
        let name = layer.get("name").and_then(Value::as_str).map_or_else(|| format!("Layer {}", i + 1), str::to_string);
        let opacity = layer.get("opacity").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        let frames = if version >= 2 {
            let wanted = if limits.animations { MAX_FRAMES } else { first_shown + 1 };
            sheet_frames(layer, width, height, wanted, &mut budget)?
        } else {
            let grids = layer.get("frames").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            let mut out = Vec::new();
            for g in grids.iter().take(MAX_FRAMES) {
                let parsed;
                let g = match g {
                    Value::String(s) => {
                        parsed = serde_json::from_str::<Value>(s).map_err(|e| bad(&e.to_string()))?;
                        &parsed
                    }
                    other => other,
                };
                out.push(grid(g)?);
            }
            out
        };
        layers.push((name, opacity, frames));
    }
    finish(&version.to_string(), width, height, fps as f32, layers, &hidden, limits)
}

/// A version 2 layer's frames, cut from its sprite sheets.
/// Only frames below `wanted` are cut out.
fn sheet_frames(
    layer: &Value,
    width: u32,
    height: u32,
    wanted: usize,
    budget: &mut u64,
) -> Result<Vec<Image8>, LoadError> {
    let count = (layer.get("frameCount").and_then(Value::as_u64).unwrap_or(1) as usize).clamp(1, MAX_FRAMES);
    // A single sheet with the frames side by side is a chunk whose layout
    // is one column per frame.
    let side_by_side;
    let chunks: &[Value] = match (layer.get("chunks").and_then(Value::as_array), layer.get("base64PNG")) {
        (Some(chunks), _) => chunks,
        (None, Some(png)) => {
            let layout: Vec<Value> = (0..count).map(|i| Value::from(vec![i])).collect();
            side_by_side = [serde_json::json!({ "base64PNG": png, "layout": layout })];
            &side_by_side
        }
        (None, None) => return Err(bad("a layer without pictures")),
    };
    let wanted = wanted.min(count);
    let mut frames: Vec<Option<Image8>> = vec![None; count];
    for chunk in chunks {
        let layout = chunk.get("layout").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        let columns = layout.len();
        let rows = layout.first().and_then(Value::as_array).map_or(0, Vec::len);
        if columns == 0 || rows == 0 {
            continue;
        }
        // Skip sheets holding none of the frames wanted.
        let indices = |c: &Value| c.as_array().map_or(Vec::new(), |r| r.iter().filter_map(Value::as_u64).collect());
        if !layout.iter().flat_map(indices).any(|f| (f as usize) < wanted) {
            continue;
        }
        let url = chunk.get("base64PNG").and_then(Value::as_str).ok_or_else(|| bad("a sheet without a picture"))?;
        let data = url.split_once(',').map_or(url, |(_, d)| d);
        let png = base64::engine::general_purpose::STANDARD
            .decode(data.trim().as_bytes())
            .map_err(|_| bad("a picture that isn't base64"))?;
        let sheet = pixels::decode(&png, *budget)?;
        *budget = budget.saturating_sub(sheet.width as u64 * sheet.height as u64);
        // Piskel takes the frame size from the sheet and its layout.
        let (fw, fh) = (sheet.width / columns as u32, sheet.height / rows as u32);
        for (i, column) in layout.iter().enumerate() {
            for (j, f) in column.as_array().map_or(&[][..], Vec::as_slice).iter().enumerate() {
                let Some(f) = f.as_u64().map(|f| f as usize).filter(|&f| f < wanted) else { continue };
                frames[f] = Some(crop(&sheet, i as u32 * fw, j as u32 * fh, fw, fh, width, height));
            }
        }
    }
    Ok(frames.into_iter().take(wanted).map(|f| f.unwrap_or_else(|| pixels::blank(width, height))).collect())
}

/// The `fw` x `fh` piece of `sheet` at `x`, `y`, placed on a `width` x
/// `height` canvas (they match unless the file is damaged).
fn crop(sheet: &Image8, x: u32, y: u32, fw: u32, fh: u32, width: u32, height: u32) -> Image8 {
    let mut out = pixels::blank(width, height);
    for row in 0..fh.min(height) {
        let sy = y + row;
        if sy >= sheet.height {
            break;
        }
        let n = fw.min(width).min(sheet.width.saturating_sub(x)) as usize * 4;
        let from = (sy as usize * sheet.width as usize + x as usize) * 4;
        let to = row as usize * width as usize * 4;
        out.pixels[to..to + n].copy_from_slice(&sheet.pixels[from..from + n]);
    }
    out
}

/// A version 0 or 1 frame: columns of colour strings.
fn grid(value: &Value) -> Result<Image8, LoadError> {
    let columns = value.as_array().ok_or_else(|| bad("a frame that isn't a grid"))?;
    let w = columns.len();
    let h = columns.first().and_then(Value::as_array).map_or(0, Vec::len);
    if w == 0 || h == 0 || (w * h) as u64 > MAX_PIXELS {
        return Err(bad("an empty or huge frame"));
    }
    let mut img = pixels::blank(w as u32, h as u32);
    for (x, column) in columns.iter().enumerate() {
        for (y, c) in column.as_array().map_or(&[][..], Vec::as_slice).iter().take(h).enumerate() {
            let px = match c {
                Value::String(s) => color(s),
                // A packed integer, red in the low byte.
                Value::Number(n) => n.as_u64().map_or([0; 4], |v| (v as u32).to_le_bytes()),
                _ => [0; 4],
            };
            let i = (y * w + x) * 4;
            img.pixels[i..i + 4].copy_from_slice(&px);
        }
    }
    Ok(img)
}

/// A CSS colour as Piskel wrote them: `#rrggbb`, `#rgb`, `rgb(...)`,
/// `rgba(...)`, or `TRANSPARENT`.
fn color(s: &str) -> [u8; 4] {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let v = |i: usize, n: usize| u8::from_str_radix(hex.get(i..i + n).unwrap_or("0"), 16).unwrap_or(0);
        return match hex.len() {
            3 => [v(0, 1) * 17, v(1, 1) * 17, v(2, 1) * 17, 255],
            6 => [v(0, 2), v(2, 2), v(4, 2), 255],
            8 => [v(0, 2), v(2, 2), v(4, 2), v(6, 2)],
            _ => [0; 4],
        };
    }
    if let Some(args) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb(")) {
        let parts: Vec<f64> = args.trim_end_matches(')').split(',').filter_map(|p| p.trim().parse().ok()).collect();
        let c = |i: usize| parts.get(i).map_or(0, |v| v.clamp(0.0, 255.0) as u8);
        let a = parts.get(3).map_or(255, |a| (a.clamp(0.0, 1.0) * 255.0).round() as u8);
        return [c(0), c(1), c(2), a];
    }
    [0; 4]
}

/// Builds the canvas: frames of every layer the same size, hidden frames
/// left out of the animation.
fn finish(
    version: &str,
    width: u32,
    height: u32,
    fps: f32,
    layers: Vec<(String, f32, Vec<Image8>)>,
    hidden: &[usize],
    limits: &Limits,
) -> Result<Stack, LoadError> {
    if width == 0 || height == 0 {
        return Err(bad("no frames"));
    }
    let count = layers.iter().map(|l| l.2.len()).max().unwrap_or(0).max(1);
    // The frames that play; all of them when every one is hidden.
    let mut shown: Vec<usize> = (0..count).filter(|f| !hidden.contains(f)).collect();
    if shown.is_empty() {
        shown = (0..count).collect();
    }
    if !limits.animations {
        shown.truncate(1);
    }
    let listed = layers
        .iter()
        .map(|(name, opacity, _)| CanvasLayer {
            name: name.clone(),
            visible: true,
            note: if *opacity < 1.0 { format!("{:.0} %", opacity * 100.0) } else { String::new() },
            ..Default::default()
        })
        .collect();
    let stack_layers = layers
        .into_iter()
        .map(|(_, opacity, frames)| {
            let frames: Vec<Option<Arc<Image8>>> = frames.into_iter().map(|f| Some(Arc::new(f))).collect();
            StackLayer {
                frames: shown.iter().map(|&f| frames.get(f).cloned().flatten()).collect(),
                opacity: (opacity * 255.0).round() as u8,
                mode: Mode::Blend(Blend::Normal),
                group: false,
                parents: Vec::new(),
            }
        })
        .collect();
    let mut info = Info { format: "Piskel sprite".into(), generator: Some("Piskel".into()), ..Default::default() };
    info.details.push(("Model version".into(), version.into()));
    info.details.push(("Frame rate".into(), format!("{fps} frames a second")));
    if !hidden.is_empty() {
        info.details.push(("Hidden frames".into(), hidden.len().to_string()));
    }
    Ok(Stack {
        info,
        width,
        height,
        layers: stack_layers,
        listed,
        durations: vec![1.0 / fps; shown.len()],
        clips: Vec::new(),
    })
}
