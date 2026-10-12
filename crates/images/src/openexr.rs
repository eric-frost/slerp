//! OpenEXR files through `slerp-openexr`: one layer of one part at a time,
//! as a linear float texture for the viewer's exposure path.  A file with
//! several layers or parts lists them as file parts, so the viewer offers a
//! picker; the first colour layer opens first.

use slerp_openexr::{Exr, Meaning, Options, Picture};
use slerp_scene::textures::{Level, Pixels, SourceInfo, TextureKind};
use slerp_scene::{ColorSpace, Limits, LoadError, Texture};
use std::io::BufReader;
use std::path::Path;

fn reader(path: &Path) -> Result<BufReader<std::fs::File>, LoadError> {
    Ok(BufReader::with_capacity(1 << 16, std::fs::File::open(path)?))
}

fn structure(path: &Path) -> Result<Exr, LoadError> {
    slerp_openexr::open(reader(path)?).map_err(LoadError::Invalid)
}

/// The names of a file's layers and parts, when it has more than one.
pub fn parts(path: &Path) -> Vec<String> {
    match structure(path) {
        Ok(exr) => {
            let choices = exr.choices();
            if choices.len() > 1 { choices.into_iter().map(|c| c.2).collect() } else { Vec::new() }
        }
        Err(_) => Vec::new(),
    }
}

/// The preview picture the writer stored in the default part's header
/// (the `preview` attribute), read without the pixels; `None` without one.
pub fn saved_picture(path: &Path) -> Result<Option<Texture>, LoadError> {
    let exr = structure(path)?;
    let Some((p, _)) = exr.default_choice() else { return Ok(None) };
    Ok(exr.parts[p].preview.clone().map(|(w, h, px)| {
        let mut tex = Texture::from_rgba8(w, h, px, ColorSpace::Srgb);
        tex.source.container = "OpenEXR".into();
        tex.source.format = "the preview picture stored in the header".into();
        tex
    }))
}

/// Loads the layer called `name` (one of [`parts`]), or the default one.
pub fn load(path: &Path, limits: &Limits, name: Option<&str>) -> Result<Texture, LoadError> {
    let exr = structure(path)?;
    let choices = exr.choices();
    let (p, l) = match name {
        Some(n) => choices
            .iter()
            .find(|c| c.2 == n)
            .map(|c| (c.0, c.1))
            .ok_or_else(|| LoadError::Invalid(format!("the file has no layer called {n}")))?,
        None => exr.default_choice().ok_or_else(|| LoadError::Invalid("the file has no channels".into()))?,
    };
    let part = &exr.parts[p];
    if part.deep {
        return Err(LoadError::Unsupported(
            "deep OpenEXR data (several samples per pixel) can't be shown as a picture".into(),
        ));
    }
    let opts =
        Options { max_side: limits.max_texture_size, mips: limits.max_texture_size.is_none(), ..Options::default() };
    let picture = slerp_openexr::decode(reader(path)?, p, l, &opts).map_err(|e| {
        if e.contains("aren't supported") || e.contains("unimplemented") || e.contains("deep") {
            LoadError::Unsupported(e)
        } else {
            LoadError::Invalid(e)
        }
    })?;
    let layer = &part.layers[l];
    let mut header = Vec::new();
    if choices.len() > 1 {
        let label = choices.iter().find(|c| (c.0, c.1) == (p, l)).map(|c| c.2.clone()).unwrap_or_default();
        header.push(("Showing".to_string(), label));
        header.push(("Layers and parts".into(), choices.len().to_string()));
    }
    if exr.parts.len() > 1 {
        header.push(("Parts".into(), exr.parts.len().to_string()));
    }
    header.extend(part.describe());
    header.extend(picture.notes.iter().map(|n| ("Note".to_string(), n.clone())));
    let samples = layer.channels.first().map_or("", |c| c.sample.name());
    let format = match picture.meaning {
        // One channel of data (depth, a mask) is stretched over its range.
        Meaning::Data if layer.channels.len() == 1 => format!("{samples} {} (data)", layer.channel_text()),
        Meaning::LuminanceChroma => format!("{samples} luminance and chroma"),
        _ => format!("{samples} {}", layer.channel_text()),
    };
    let mut tex = texture(picture);
    tex.source.container = "OpenEXR".into();
    tex.source.format = format;
    tex.source.header = header;
    tex.source.file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(tex)
}

fn texture(p: Picture) -> Texture {
    let (w, h) = (p.levels[0].width, p.levels[0].height);
    let mip_count = p.levels.len() as u32;
    let levels = p
        .levels
        .into_iter()
        .map(|l| Level { width: l.width, height: l.height, depth: 1, surfaces: vec![Pixels::Rgba32F(l.rgba)] })
        .collect();
    Texture {
        kind: TextureKind::D2,
        layers: 1,
        faces: 1,
        levels,
        color_space: ColorSpace::Linear,
        has_alpha: p.has_alpha,
        source: SourceInfo { width: w, height: h, depth: 1, layers: 1, faces: 1, mip_count, ..Default::default() },
    }
}
