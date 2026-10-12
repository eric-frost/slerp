//! PlayCanvas's SOG ("spatially ordered Gaussians", as written by
//! `splat-transform` and SuperSplat): a zip of WebP images, one texel per
//! splat in the same order in each, and `meta.json` saying how to read
//! them.  Positions are 16 bits per axis split over two images (low and
//! high bytes) between the file's minimum and maximum, in a log-like
//! encoding (`sign(v) × (e^|v| − 1)`); rotations are the three smallest
//! quaternion components plus the largest's index in the alpha byte
//! (252 + index); scales and band 0 colours are bytes indexing 256-entry
//! codebooks (version 2) or spans between a minimum and maximum (version
//! 1); opacity is a byte.  Higher spherical-harmonic bands, when present,
//! are a palette: a label image picks one of up to 65,536 entries of a
//! centroid image, whose bytes index another codebook.  Coordinates are
//! the training's (x right, y down), as in PLY.

use crate::{Builder, SH_C0, SplatFormat, degree_for, sigmoid};
use glam::{Quat, Vec3};
use serde_json::Value;
use slerp_scene::{Info, Limits, LoadError, Scene};
use std::borrow::Cow;
use std::io::Read;

/// Whether `head` starts a zip and the file is named `.sog`.
pub(crate) fn sniff(path: &std::path::Path, head: &[u8]) -> bool {
    let sog = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("sog"));
    sog && head.starts_with(b"PK\x03\x04")
}

/*┌─────────────────╮
| Zip             |
└─────────────────╯*/
//region Zip

struct Entry {
    name: String,
    method: u16,
    /// Offset of the entry's data.
    data: usize,
    packed: usize,
    size: usize,
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The entries of a zip, from its central directory.  Zip64 isn't needed:
/// SOG files are a few megabytes per million splats.
fn entries(b: &[u8]) -> Option<Vec<Entry>> {
    let tail = b.len().saturating_sub(65_557);
    let eocd = (tail..b.len().saturating_sub(21)).rev().find(|&i| b[i..].starts_with(b"PK\x05\x06"))?;
    let count = u16_at(b, eocd + 10)?;
    let mut at = u32_at(b, eocd + 16)?;
    let mut out = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        if !b.get(at..)?.starts_with(b"PK\x01\x02") {
            return None;
        }
        let name_len = u16_at(b, at + 28)?;
        let name = String::from_utf8_lossy(b.get(at + 46..at + 46 + name_len)?).into_owned();
        let local = u32_at(b, at + 42)?;
        if !b.get(local..)?.starts_with(b"PK\x03\x04") {
            return None;
        }
        let data = local + 30 + u16_at(b, local + 26)? + u16_at(b, local + 28)?;
        let entry = Entry {
            name,
            method: u16_at(b, at + 10)? as u16,
            data,
            packed: u32_at(b, at + 20)?,
            size: u32_at(b, at + 24)?,
        };
        at += 46 + name_len + u16_at(b, at + 30)? + u16_at(b, at + 32)?;
        out.push(entry);
    }
    Some(out)
}

/// An entry's contents, stored or deflated, at most 1 GB.
fn contents<'a>(b: &'a [u8], e: &Entry) -> Result<Cow<'a, [u8]>, LoadError> {
    let raw = b
        .get(e.data..e.data.saturating_add(e.packed))
        .ok_or_else(|| LoadError::invalid(format!("{} runs past the end", e.name)))?;
    match e.method {
        0 => Ok(Cow::Borrowed(raw)),
        8 => {
            let mut out = Vec::with_capacity(e.size.min(64 << 20));
            flate2::read::DeflateDecoder::new(raw)
                .take((e.size as u64).min(1 << 30))
                .read_to_end(&mut out)
                .map_err(|err| LoadError::invalid(format!("{}: {err}", e.name)))?;
            Ok(Cow::Owned(out))
        }
        m => Err(LoadError::Unsupported(format!("zip compression method {m}"))),
    }
}

//endregion

/// A decoded image: width and RGBA bytes.
struct Image {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Image {
    fn texel(&self, i: usize) -> [u8; 4] {
        let p = &self.rgba[i * 4..i * 4 + 4];
        [p[0], p[1], p[2], p[3]]
    }
}

/// A 256-entry codebook, whose first entry some writers leave null, as
/// PlayCanvas patches it.
fn codebook(v: &Value) -> Option<Vec<f32>> {
    let list = v.get("codebook")?.as_array()?;
    if list.len() != 256 {
        return None;
    }
    let mut book: Vec<f32> = list.iter().map(|x| x.as_f64().map_or(f32::NAN, |f| f as f32)).collect();
    if book[0].is_nan() {
        book[0] = book[1] + (book[1] - book[255]) / 255.0;
    }
    book.iter().all(|f| f.is_finite()).then_some(book)
}

fn floats(v: &Value, key: &str) -> Vec<f32> {
    match v.get(key) {
        Some(Value::Array(a)) => a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect(),
        Some(x) => x.as_f64().map(|f| vec![f as f32]).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Reads `[min, max]` lists of `n` values.
fn span(v: &Value, n: usize) -> Option<(Vec<f32>, Vec<f32>)> {
    let (lo, hi) = (floats(v, "mins"), floats(v, "maxs"));
    (lo.len() >= n && hi.len() >= n).then_some((lo, hi))
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub(crate) fn load(bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. The archive and its description.
    let files = entries(bytes).ok_or_else(|| LoadError::invalid("SOG isn't a readable zip"))?;
    let file = |name: &str| -> Result<Cow<[u8]>, LoadError> {
        let e = files.iter().find(|e| e.name == name || e.name.ends_with(&format!("/{name}")));
        contents(bytes, e.ok_or_else(|| LoadError::invalid(format!("SOG has no {name}")))?)
    };
    let meta: Value =
        serde_json::from_slice(&file("meta.json")?).map_err(|e| LoadError::invalid(format!("SOG meta.json: {e}")))?;
    let version = meta.get("version").and_then(Value::as_u64).unwrap_or(1);
    let count = meta
        .get("count")
        .and_then(Value::as_u64)
        .or_else(|| meta.pointer("/means/shape/0").and_then(Value::as_u64))
        .ok_or_else(|| LoadError::invalid("SOG meta.json has no splat count"))?;
    let section = |name: &str| meta.get(name).ok_or_else(|| LoadError::invalid(format!("SOG meta.json has no {name}")));
    let (means, scales, sh0) = (section("means")?, section("scales")?, section("sh0")?);
    let image_names = |v: &Value| -> Vec<String> {
        v.get("files")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };

    // 2. The images, each with a texel for every splat.
    let decode = |name: &str, needed: u64| -> Result<Image, LoadError> {
        let opts = slerp_scene::textures::DecodeOptions {
            min_size: None,
            max_size: None,
            all_mips: false,
            first_layer_only: true,
        };
        let tex = slerp_scene::textures::decode(&file(name)?, &opts)?;
        let level = tex.levels.into_iter().next().ok_or_else(|| LoadError::invalid(format!("{name} is empty")))?;
        let (width, height) = (level.width as usize, level.height as usize);
        let Some(slerp_scene::textures::Pixels::Rgba8(rgba)) = level.surfaces.into_iter().next() else {
            return Err(LoadError::invalid(format!("{name} isn't an 8-bit picture")));
        };
        if ((width * height) as u64) < needed || rgba.len() < width * height * 4 {
            return Err(LoadError::invalid(format!("{name} has too few texels")));
        }
        Ok(Image { width, height, rgba })
    };
    let means_files = image_names(means);
    let [low, high] = [0, 1].map(|i| means_files.get(i).cloned().unwrap_or_default());
    let means_l = decode(&low, count)?;
    let means_u = decode(&high, count)?;
    let quats = decode(image_names(section("quats")?).first().map_or("quats.webp", String::as_str), count)?;
    let scale_img = decode(image_names(scales).first().map_or("scales.webp", String::as_str), count)?;
    let color_img = decode(image_names(sh0).first().map_or("sh0.webp", String::as_str), count)?;
    let (mean_lo, mean_hi) = span(means, 3).ok_or_else(|| LoadError::invalid("SOG means need mins and maxs"))?;
    let scale_book = codebook(scales);
    let color_book = codebook(sh0);
    let scale_span = span(scales, 3);
    let color_span = span(sh0, 4);
    if version >= 2 && (scale_book.is_none() || color_book.is_none()) {
        return Err(LoadError::invalid("SOG version 2 needs scale and colour codebooks"));
    }
    if version < 2 && (scale_span.is_none() || color_span.is_none()) {
        return Err(LoadError::invalid("SOG version 1 needs scale and colour ranges"));
    }

    // 3. Higher bands, when there are any and they read.
    let mut info = Info { format: format!("{} (version {version})", SplatFormat::Sog.name()), ..Default::default() };
    if let Some(g) = meta.pointer("/asset/generator").and_then(Value::as_str) {
        info.generator = Some(g.to_string());
    }
    let harmonics = meta.get("shN").and_then(|sh| {
        let names = image_names(sh);
        let centroids = decode(names.first()?, 0).ok()?;
        let labels = decode(names.get(1)?, count).ok()?;
        let per_channel = centroids.width / 64;
        degree_for(per_channel).filter(|&d| d > 0 && centroids.width == 64 * per_channel)?;
        let lookup: Box<dyn Fn(u8) -> f32> = match codebook(sh) {
            Some(book) => Box::new(move |b| book[b as usize]),
            None => {
                let (lo, hi) = (floats(sh, "mins"), floats(sh, "maxs"));
                let (lo, hi) = (*lo.first()?, *hi.first()?);
                Box::new(move |b| lerp(lo, hi, b as f32 / 255.0))
            }
        };
        Some((centroids, labels, per_channel, lookup))
    });
    if meta.get("shN").is_some() && harmonics.is_none() {
        info.warnings.push("spherical harmonics couldn't be read; drawn without them".into());
    }
    let degree = harmonics.as_ref().and_then(|h| degree_for(h.2)).unwrap_or(0);

    // 4. Splats.
    let mut out = Builder::new(count, limits, degree, true);
    let position = |a: usize, t: u32| {
        let v = lerp(mean_lo[a], mean_hi[a], t as f32 / 65535.0);
        v.signum() * (v.abs().exp() - 1.0)
    };
    for i in 0..count as usize {
        if !out.keeps(i as u64) {
            continue;
        }
        let (l, u) = (means_l.texel(i), means_u.texel(i));
        let p = Vec3::new(
            position(0, l[0] as u32 | (u[0] as u32) << 8),
            position(1, l[1] as u32 | (u[1] as u32) << 8),
            position(2, l[2] as u32 | (u[2] as u32) << 8),
        );
        let q = quats.texel(i);
        let c = |b: u8| (b as f32 / 255.0 - 0.5) * std::f32::consts::SQRT_2;
        let (a, b, cc) = (c(q[0]), c(q[1]), c(q[2]));
        let d = (1.0 - (a * a + b * b + cc * cc)).max(0.0).sqrt();
        let rotation = match q[3].wrapping_sub(252) {
            0 => Quat::from_xyzw(a, b, cc, d),
            1 => Quat::from_xyzw(d, b, cc, a),
            2 => Quat::from_xyzw(b, d, cc, a),
            _ => Quat::from_xyzw(b, cc, d, a),
        };
        let s = scale_img.texel(i);
        let k = color_img.texel(i);
        let (log_scale, dc, opacity) = match (&scale_book, &color_book, &scale_span, &color_span) {
            (Some(sb), Some(cb), _, _) if version >= 2 => (
                Vec3::new(sb[s[0] as usize], sb[s[1] as usize], sb[s[2] as usize]),
                Vec3::new(cb[k[0] as usize], cb[k[1] as usize], cb[k[2] as usize]),
                k[3] as f32 / 255.0,
            ),
            (_, _, Some((slo, shi)), Some((clo, chi))) => (
                Vec3::new(
                    lerp(slo[0], shi[0], s[0] as f32 / 255.0),
                    lerp(slo[1], shi[1], s[1] as f32 / 255.0),
                    lerp(slo[2], shi[2], s[2] as f32 / 255.0),
                ),
                Vec3::new(
                    lerp(clo[0], chi[0], k[0] as f32 / 255.0),
                    lerp(clo[1], chi[1], k[1] as f32 / 255.0),
                    lerp(clo[2], chi[2], k[2] as f32 / 255.0),
                ),
                sigmoid(lerp(clo[3], chi[3], k[3] as f32 / 255.0)),
            ),
            _ => continue,
        };
        let color = (dc * SH_C0 + 0.5).extend(opacity);
        let sh = |coef: usize, ch: usize| -> f32 {
            let Some((centroids, labels, per_channel, lookup)) = &harmonics else { return 0.0 };
            let label = labels.texel(i);
            let n = label[0] as usize | (label[1] as usize) << 8;
            let (u, v) = ((n % 64) * per_channel, n / 64);
            if v >= centroids.height {
                return 0.0;
            }
            lookup(centroids.rgba[((v * centroids.width) + u + coef) * 4 + ch])
        };
        out.push(p, log_scale.exp(), rotation, color, sh);
    }
    out.finish(info)
}
