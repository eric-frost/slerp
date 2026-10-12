//! Palettes kept in zips: Krita's `.kpl` (`colorset.xml`) and Procreate's
//! `.swatches` (`Swatches.json`).

use super::{Color, Palette, PaletteFormat, Swatch, SwatchKind};
use crate::source::Source;
use crate::zip::Zip;
use slerp_scene::LoadError;

/// The largest palette document inflated.
const MAX_DOCUMENT: u64 = 16 << 20;

/*┌─────────────────╮
| Krita           |
└─────────────────╯*/
//region Krita

/// Krita palettes: `colorset.xml` holds a `ColorSet` element (name, column
/// count, version) whose `ColorSetEntry` children are the ungrouped
/// swatches and whose `Group` children hold more.  Each entry's first
/// child element is its colour in Krita's colour XML (`RGB`, `sRGB`,
/// `CMYK`, `Lab`, `Gray`, `XYZ`), and an optional `Position` gives its row
/// and column.
pub fn kpl(src: &mut Source) -> Result<Palette, LoadError> {
    let zip = Zip::open(src)?;
    let xml = zip
        .read_named(src, "colorset.xml", MAX_DOCUMENT)?
        .ok_or_else(|| LoadError::invalid("Krita palette without colorset.xml"))?;
    let text = String::from_utf8_lossy(&xml);
    let doc = roxmltree::Document::parse(&text).map_err(|e| LoadError::invalid(format!("colorset.xml: {e}")))?;
    let root = doc.root_element();
    // Early Krita 3 files spell it `Colorset`.
    if !root.tag_name().name().eq_ignore_ascii_case("ColorSet") {
        return Err(LoadError::invalid("colorset.xml has no ColorSet"));
    }
    let mut p = Palette::new(PaletteFormat::Kpl);
    p.name = root.attribute("name").unwrap_or_default().to_string();
    p.columns = root.attribute("columns").and_then(|c| c.parse().ok()).filter(|&c| c > 0);
    // Version 1.0 files stored Lab scaled to 0..1.
    let old_lab = root.attribute("version") == Some("1.0");
    kpl_group(&mut p, root, old_lab);
    for g in root.children().filter(|n| n.has_tag_name("Group")) {
        p.begin_group(g.attribute("name").unwrap_or_default().to_string());
        kpl_group(&mut p, g, old_lab);
    }
    Ok(p)
}

fn kpl_group(p: &mut Palette, parent: roxmltree::Node, old_lab: bool) {
    // 1. The entries with their positions; entries without one follow in
    //    document order.
    let mut entries: Vec<((u64, u64), Swatch)> = Vec::new();
    for (i, e) in parent.children().filter(|n| n.has_tag_name("ColorSetEntry")).enumerate() {
        let Some(c) = e.children().find(|n| n.is_element() && !n.has_tag_name("Position")) else { continue };
        let Some(color) = kpl_color(c, old_lab) else {
            if p.warnings.len() < 5 {
                p.warnings.push(format!("a {} colour was skipped", c.tag_name().name()));
            }
            continue;
        };
        let mut s = Swatch::new(e.attribute("name").unwrap_or_default(), color);
        if e.attribute("spot") == Some("true") {
            s.kind = SwatchKind::Spot;
        }
        let pos = e.children().find(|n| n.has_tag_name("Position")).and_then(|n| {
            Some((n.attribute("row")?.parse::<u64>().ok()?, n.attribute("column")?.parse::<u64>().ok()?))
        });
        entries.push((pos.unwrap_or((u64::MAX, i as u64)), s));
        if entries.len() >= super::MAX_SWATCHES {
            break;
        }
    }
    entries.sort_by_key(|(pos, _)| *pos);
    // 2. Into the group, in order.
    for (_, s) in entries {
        if !p.push(s) {
            break;
        }
    }
}

fn kpl_color(c: roxmltree::Node, old_lab: bool) -> Option<Color> {
    let f = |name: &str| c.attribute(name).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
    Some(match c.tag_name().name() {
        // Linear profiles (Krita's `...-g10.icc`, for scene-linear
        // painting) are encoded to sRGB's curve for display.
        "RGB"
            if c.attribute("space")
                .is_some_and(|s| s.contains("-g10") || s.to_ascii_lowercase().contains("linear")) =>
        {
            Color::Rgb([f("r")?, f("g")?, f("b")?].map(linear_to_srgb))
        }
        "RGB" | "sRGB" => Color::Rgb([f("r")?, f("g")?, f("b")?]),
        "CMYK" => Color::Cmyk([f("c")?, f("m")?, f("y")?, f("k")?]),
        "Gray" => Color::Gray(f("g")?),
        "Lab" if old_lab => {
            // Krita's own conversion of the old scaling.
            let ab = |v: f32| if v <= 0.5 { (0.5 - v) * 2.0 * -128.0 } else { (v - 0.5) * 2.0 * 127.0 };
            Color::Lab([f("L")? * 100.0, ab(f("a")?), ab(f("b")?)])
        }
        "Lab" => Color::Lab([f("L")?, f("a")?, f("b")?]),
        "XYZ" => {
            // Rare; shown through Lab, which keeps the original numbers
            // out of reach but the colour right.
            let (x, y, z) = (f("x")?, f("y")?, f("z")?);
            let g = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
            let (fx, fy, fz) = (g(x / 0.96422), g(y), g(z / 0.82521));
            Color::Lab([116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)])
        }
        _ => return None,
    })
}

fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

//endregion

/*┌─────────────────╮
| Procreate       |
└─────────────────╯*/
//region Procreate

/// Procreate swatches: `Swatches.json` is an object (in older files, an
/// array holding one) with a `name` and a `swatches` array of hue,
/// saturation, brightness and alpha, each from 0 to 1, or `null` for an
/// empty slot.
pub fn procreate(src: &mut Source) -> Result<Palette, LoadError> {
    let zip = Zip::open(src)?;
    let entry = zip
        .find("Swatches.json")
        .or_else(|| zip.entries.iter().find(|e| e.name.to_ascii_lowercase().ends_with(".json")))
        .ok_or_else(|| LoadError::invalid("Procreate swatches without Swatches.json"))?
        .clone();
    let json = zip.read(src, &entry, MAX_DOCUMENT)?;
    let root: serde_json::Value =
        serde_json::from_slice(&json).map_err(|e| LoadError::invalid(format!("Swatches.json: {e}")))?;
    let root = match &root {
        serde_json::Value::Array(a) => a.first().unwrap_or(&serde_json::Value::Null),
        v => v,
    };
    let mut p = Palette::new(PaletteFormat::ProcreateSwatches);
    p.name = root.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
    // Procreate shows ten swatches to a row.
    p.columns = Some(10);
    let swatches = root.get("swatches").and_then(|s| s.as_array()).map(Vec::as_slice).unwrap_or_default();
    for s in swatches {
        let num = |k: &str| s.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).filter(|v| v.is_finite());
        let (Some(h), Some(sat), Some(b)) = (num("hue"), num("saturation"), num("brightness")) else { continue };
        let mut swatch =
            Swatch::new(s.get("name").and_then(|n| n.as_str()).unwrap_or_default(), Color::Hsb([h * 360.0, sat, b]));
        swatch.alpha = num("alpha").unwrap_or(1.0).clamp(0.0, 1.0);
        if !p.push(swatch) {
            break;
        }
    }
    Ok(p)
}

//endregion
