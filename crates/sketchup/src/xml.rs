//! The XML documents inside 2021 and later files: one `material.xml` per
//! material, and the styles' `style.xml`.  They come from the file, so
//! documents with a DTD (where entity expansion could blow up) are refused
//! and node counts are capped.

use roxmltree::{Document, ParsingOptions};

/// A material as `material.xml` describes it.
#[derive(Debug, Default)]
pub struct MaterialXml {
    pub name: String,
    pub color: [u8; 3],
    pub opacity: f32,
    /// A colourised copy of another material's texture (`type="2"`).
    pub colorized: bool,
    pub texture: Option<TextureXml>,
}

#[derive(Debug, Default)]
pub struct TextureXml {
    pub file_name: String,
    /// One tile's size in inches.
    pub x_scale: f64,
    pub y_scale: f64,
    /// Where a colourised copy finds its picture: in the original
    /// material's folder.
    pub image_path: String,
}

fn parse(text: &str) -> Option<Document<'_>> {
    let options = ParsingOptions { allow_dtd: false, nodes_limit: 100_000, ..ParsingOptions::default() };
    Document::parse_with_options(text, options).ok()
}

pub fn material(bytes: &[u8]) -> Option<MaterialXml> {
    let text = std::str::from_utf8(bytes).ok()?;
    let doc = parse(text)?;
    let m = doc.descendants().find(|n| n.has_tag_name("material"))?;
    let byte =
        |name: &str| m.attribute(name).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(128).clamp(0, 255) as u8;
    // `trans` counts only with `useTrans="1"`; otherwise it is a leftover
    // and the material is opaque.  It is a transparency: SketchUp's
    // "Translucent Glass Blue", 70 % opaque, stores 0.3.
    let opacity = if m.attribute("useTrans") == Some("1") {
        let trans = m.attribute("trans").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
        (1.0 - trans).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let texture = m.children().find(|n| n.has_tag_name("texture")).map(|t| {
        let number = |name: &str| t.attribute(name).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
        let image_path = t
            .descendants()
            .find(|n| n.has_tag_name("image"))
            .and_then(|i| i.attribute("path"))
            .unwrap_or_default()
            .to_string();
        TextureXml {
            file_name: t.attribute("textureFilename").unwrap_or_default().to_string(),
            x_scale: number("xScale"),
            y_scale: number("yScale"),
            image_path,
        }
    });
    Some(MaterialXml {
        name: m.attribute("name").unwrap_or("unknown").to_string(),
        color: [byte("colorRed"), byte("colorGreen"), byte("colorBlue")],
        opacity,
        colorized: m.attribute("type") == Some("2"),
        texture,
    })
}

/// A style's front and back face colours (items 2002 and 2003, stored as
/// signed 32-bit ABGR numbers).
pub fn style_colors(bytes: &[u8]) -> (Option<[u8; 3]>, Option<[u8; 3]>) {
    let Some(doc) = std::str::from_utf8(bytes).ok().and_then(parse) else { return (None, None) };
    let mut front = None;
    let mut back = None;
    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let slot = match item.attribute("id") {
            Some("2002") => &mut front,
            Some("2003") => &mut back,
            _ => continue,
        };
        let value = item
            .children()
            .find(|n| n.has_tag_name("variant"))
            .and_then(|v| v.text())
            .and_then(|t| t.trim().parse::<i64>().ok());
        if let Some(v) = value {
            let v = v as u32;
            *slot = Some([(v & 255) as u8, (v >> 8 & 255) as u8, (v >> 16 & 255) as u8]);
        }
    }
    (front, back)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_materials_and_styles() {
        let m = br#"<?xml version="1.0"?><materialDocument xmlns:mat="m"><mat:material name="Glass" type="2"
            colorRed="10" colorGreen="300" colorBlue="x" trans="0.3" useTrans="1"><mat:texture
            textureFilename="g.png" xScale="12" yScale="24"><mat:images><mat:image path="./materials/A/g.png"/>
            </mat:images></mat:texture></mat:material></materialDocument>"#;
        let m = material(m).unwrap();
        assert_eq!(m.color, [10, 255, 128]);
        assert!((m.opacity - 0.7).abs() < 1e-6 && m.colorized);
        let t = m.texture.unwrap();
        assert_eq!((t.file_name.as_str(), t.x_scale, t.y_scale), ("g.png", 12.0, 24.0));
        assert_eq!(t.image_path, "./materials/A/g.png");
        let s = br#"<styleDocument xmlns:sty="s"><sty:style xmlns:t="t"><sty:item id="2002"><t:variant type="4">-1</t:variant></sty:item><sty:item id="2003"><t:variant>-4476252</t:variant></sty:item></sty:style></styleDocument>"#;
        assert_eq!(style_colors(s), (Some([255, 255, 255]), Some([164, 178, 187])));
        let bomb = br#"<!DOCTYPE x [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]><material name="&b;"/>"#;
        assert!(material(bomb).is_none());
    }
}
