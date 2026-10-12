//! OME-XML, the description OME-TIFF files carry in their first page: the
//! sizes and order of the Z, C and T axes, physical pixel sizes, channel
//! names and colours, and which page holds which plane.

/// What the first image of an OME-XML document says.
#[derive(Clone, Debug, Default)]
pub struct Ome {
    pub name: String,
    pub images: usize,
    pub size_x: usize,
    pub size_y: usize,
    pub size_z: usize,
    pub size_c: usize,
    pub size_t: usize,
    /// "XYZCT" and the like: the order planes follow in the file.
    pub order: String,
    pub pixel_type: String,
    /// Physical sizes of a pixel along x, y and z in micrometres; 0 when absent.
    pub physical: [f64; 3],
    pub time_increment: Option<f64>,
    pub time_unit: String,
    pub channels: Vec<Channel>,
    pub tiff_data: Vec<TiffData>,
    pub acquired: String,
    /// The `urn:uuid:` that names this file, which its planes' `TiffData`
    /// entries repeat, so a renamed file still finds its own planes.
    pub uuid: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub name: String,
    pub color: Option<[u8; 3]>,
    pub samples: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TiffData {
    pub ifd: Option<usize>,
    pub first_z: usize,
    pub first_c: usize,
    pub first_t: usize,
    pub plane_count: Option<usize>,
    /// The file the planes are in, when it isn't this one.
    pub file: Option<String>,
    /// The `urn:uuid:` of the file the planes are in.
    pub uuid: Option<String>,
}

impl TiffData {
    /// True when the planes are in the file called `this_file` whose own
    /// UUID is `own`: by name, or by UUID if the file has been renamed.
    pub fn is_in(&self, this_file: &str, own: Option<&str>) -> bool {
        match (&self.file, &self.uuid) {
            (None, None) => true,
            (file, uuid) => file.as_deref() == Some(this_file) || uuid.as_deref().zip(own).is_some_and(|(a, b)| a == b),
        }
    }
}

/// True when a page description is OME-XML.
pub fn looks_like_ome(desc: &str) -> bool {
    let d = desc.trim_start();
    d.starts_with('<') && d.contains("<OME") && d.len() < 64 << 20
}

fn to_um(value: f64, unit: Option<&str>) -> f64 {
    let f = match unit.unwrap_or("µm") {
        "nm" => 1e-3,
        "mm" => 1e3,
        "cm" => 1e4,
        "m" => 1e6,
        "pm" => 1e-6,
        _ => 1.0,
    };
    value * f
}

/// Parses the first image of an OME-XML document.
pub fn parse(xml: &str) -> Result<Ome, String> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() },
    )
    .map_err(|e| format!("the OME-XML is damaged: {e}"))?;
    let named = |n: &roxmltree::Node, name: &str| n.is_element() && n.tag_name().name() == name;
    let images: Vec<_> = doc.descendants().filter(|n| named(n, "Image")).collect();
    let image = images.first().ok_or("the OME-XML has no Image")?;
    let pixels = image.children().find(|n| named(n, "Pixels")).ok_or("the OME-XML image has no Pixels")?;
    let num = |name: &str| -> usize { pixels.attribute(name).and_then(|v| v.parse().ok()).unwrap_or(1) };
    let fl = |name: &str| -> Option<f64> { pixels.attribute(name).and_then(|v| v.parse().ok()) };
    let mut o = Ome {
        name: image.attribute("Name").unwrap_or("").to_string(),
        images: images.len(),
        size_x: num("SizeX"),
        size_y: num("SizeY"),
        size_z: num("SizeZ"),
        size_c: num("SizeC"),
        size_t: num("SizeT"),
        uuid: doc.root_element().attribute("UUID").map(|u| u.trim().to_ascii_lowercase()),
        order: pixels.attribute("DimensionOrder").unwrap_or("XYZCT").to_string(),
        pixel_type: pixels.attribute("Type").unwrap_or("").to_string(),
        acquired: image
            .children()
            .find(|n| named(n, "AcquisitionDate"))
            .and_then(|n| n.text())
            .unwrap_or("")
            .to_string(),
        ..Default::default()
    };
    for (i, (k, u)) in [
        ("PhysicalSizeX", "PhysicalSizeXUnit"),
        ("PhysicalSizeY", "PhysicalSizeYUnit"),
        ("PhysicalSizeZ", "PhysicalSizeZUnit"),
    ]
    .iter()
    .enumerate()
    {
        if let Some(v) = fl(k).filter(|v| *v > 0.0) {
            o.physical[i] = to_um(v, pixels.attribute(*u));
        }
    }
    o.time_increment = fl("TimeIncrement");
    o.time_unit = pixels.attribute("TimeIncrementUnit").unwrap_or("s").to_string();
    for c in pixels.children().filter(|n| named(n, "Channel")) {
        // The colour is a signed 32-bit integer holding RGBA, red highest.
        let color = c.attribute("Color").and_then(|v| {
            if let Ok(n) = v.trim().parse::<i64>() {
                let u = n as i32 as u32;
                Some([(u >> 24) as u8, (u >> 16) as u8, (u >> 8) as u8])
            } else {
                // Some writers put "(r, g, b)" there instead.
                let parts: Vec<u8> = v
                    .trim()
                    .trim_matches(|c| c == '(' || c == ')')
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
                (parts.len() >= 3).then(|| [parts[0], parts[1], parts[2]])
            }
        });
        o.channels.push(Channel {
            name: c.attribute("Name").unwrap_or("").to_string(),
            color,
            samples: c.attribute("SamplesPerPixel").and_then(|v| v.parse().ok()).unwrap_or(1),
        });
        if o.channels.len() >= 256 {
            break;
        }
    }
    for t in pixels.children().filter(|n| named(n, "TiffData")) {
        let get = |a: &str| -> Option<usize> { t.attribute(a).and_then(|v| v.parse().ok()) };
        let uuid_node = t.children().find(|n| named(n, "UUID"));
        o.tiff_data.push(TiffData {
            ifd: get("IFD"),
            first_z: get("FirstZ").unwrap_or(0),
            first_c: get("FirstC").unwrap_or(0),
            first_t: get("FirstT").unwrap_or(0),
            plane_count: get("PlaneCount"),
            file: uuid_node.and_then(|u| u.attribute("FileName")).map(str::to_string),
            uuid: uuid_node.and_then(|u| u.text()).map(|u| u.trim().to_ascii_lowercase()),
        });
        if o.tiff_data.len() >= 1 << 20 {
            break;
        }
    }
    if o.size_x == 0 || o.size_y == 0 || o.size_z == 0 || o.size_c == 0 || o.size_t == 0 {
        return Err("the OME-XML has a zero size".into());
    }
    let ok = {
        let mut dims: Vec<char> = o.order.chars().collect();
        dims.sort_unstable();
        dims.iter().collect::<String>() == "CTXYZ"
    };
    if !ok {
        return Err(format!("the dimension order '{}' isn't one OME defines", o.order));
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06">
<Image ID="Image:0" Name="stack">
<Pixels ID="Pixels:0" DimensionOrder="XYCZT" Type="uint16" SizeX="64" SizeY="48" SizeZ="5" SizeC="2" SizeT="3" PhysicalSizeX="0.5" PhysicalSizeXUnit="nm" PhysicalSizeY="0.25" PhysicalSizeZ="2" TimeIncrement="1.5">
<Channel ID="Channel:0:0" Name="DAPI" Color="-16776961" SamplesPerPixel="1"/>
<Channel ID="Channel:0:1" Name="GFP" Color="(0, 255, 0)"/>
<TiffData IFD="0" PlaneCount="30"/>
</Pixels></Image><Image ID="Image:1"><Pixels DimensionOrder="XYZCT" Type="uint8" SizeX="1" SizeY="1" SizeZ="1" SizeC="1" SizeT="1"/></Image></OME>"#;

    #[test]
    fn reads_axes_channels_and_sizes() {
        let o = parse(XML).unwrap();
        assert_eq!((o.size_x, o.size_y, o.size_z, o.size_c, o.size_t), (64, 48, 5, 2, 3));
        assert_eq!(o.order, "XYCZT");
        assert_eq!(o.images, 2);
        assert_eq!(o.channels.len(), 2);
        assert_eq!(o.channels[0].name, "DAPI");
        assert_eq!(o.channels[0].color, Some([255, 0, 0]));
        assert_eq!(o.channels[1].color, Some([0, 255, 0]));
        assert!((o.physical[0] - 0.0005).abs() < 1e-9);
        assert_eq!(o.physical[2], 2.0);
        assert_eq!(o.time_increment, Some(1.5));
        assert_eq!(o.tiff_data[0].plane_count, Some(30));
    }

    #[test]
    fn rejects_damaged_documents() {
        assert!(parse("<OME><Image></Image></OME>").is_err());
        assert!(parse("<OME><Image><Pixels SizeX=\"0\" SizeY=\"1\" DimensionOrder=\"XYZCT\"/></Image></OME>").is_err());
        assert!(parse("<OME><Image><Pixels SizeX=\"2\" SizeY=\"1\" DimensionOrder=\"XYXCT\"/></Image></OME>").is_err());
        assert!(parse("not xml").is_err());
        assert!(parse("<!DOCTYPE x [<!ENTITY a \"b\">]><OME/>").is_err());
        assert!(looks_like_ome(XML));
        assert!(!looks_like_ome("ImageJ=1.5"));
    }
}
