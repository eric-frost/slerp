//! SVG and SVGZ drawings, drawn by resvg: as a picture at a requested size
//! (thumbnails, `slerp info`, the viewer's first look) and, for the
//! viewer's zoom, any region at any scale through a [`Drawing`].
//!
//! SVG can be hostile: entity expansion, deep nesting, references in loops,
//! filters over huge regions and elaborate drawings that take minutes.
//! usvg refuses the worst (nesting deeper than 1024, element-count limits,
//! reference loops), sizes are capped here, and all parsing and drawing
//! happens in a forked child with a time and memory limit
//! (`slerp-isolate`), so a file that defeats them costs one picture.
//!
//! Text is shaped with the system's fonts, loaded only for files that have
//! text, with the usual families (DejaVu, Liberation, Noto) standing in for
//! the generic `serif`, `sans-serif` and `monospace`.  Images the drawing
//! links to load only from beside the file (relative paths, no URLs or
//! absolute paths) and only where the caller allows neighbouring files;
//! embedded images always load.

use slerp_scene::{ColorSpace, Image8, Limits, LoadError, Texture};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The largest picture drawn, in pixels, and its longest side.
const MAX_PIXELS: u64 = 64 << 20;
const MAX_SIDE: u32 = 16384;
/// The largest SVG text read (after unpacking an SVGZ).
const MAX_BYTES: usize = 256 << 20;
/// Linked images larger than this are skipped.
const MAX_IMAGE_BYTES: u64 = 64 << 20;
/// Time for a thumbnail or a first picture, and for each redraw.
const TIME: Duration = Duration::from_secs(20);
/// Memory the drawing child may add.
const MEMORY: u64 = 2 << 30;

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

/// Whether a file is an SVG drawing: XML whose root is `svg`, or a gzipped
/// file named `.svgz` or `.svg.gz`.
pub fn is_svg(path: &Path, head: &[u8]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    if head.starts_with(&[0x1f, 0x8b]) {
        return name.ends_with(".svgz") || name.ends_with(".svg.gz");
    }
    let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    let mut t = text.trim_start_matches('\u{feff}').trim_start();
    // Skip the declaration, comments, processing instructions and a
    // DOCTYPE to the first element.
    loop {
        if let Some(rest) = t.strip_prefix("<?") {
            let Some(e) = rest.find("?>") else { return false };
            t = rest[e + 2..].trim_start();
        } else if let Some(rest) = t.strip_prefix("<!--") {
            let Some(e) = rest.find("-->") else { return false };
            t = rest[e + 3..].trim_start();
        } else if t.starts_with("<!DOCTYPE") || t.starts_with("<!doctype") {
            // An internal subset may hold entities; skip to its end.
            let end = match (t.find('['), t.find('>')) {
                (Some(b), Some(g)) if b < g => t[b..].find("]>").map(|e| b + e + 2),
                (_, Some(g)) => Some(g + 1),
                _ => None,
            };
            let Some(e) = end else { return false };
            t = t[e..].trim_start();
        } else {
            break;
        }
    }
    let Some(rest) = t.strip_prefix('<') else { return false };
    let element: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
    element == "svg" || element.ends_with(":svg")
}

/// The SVG text of a file, unpacked when it is gzipped.
pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    if raw.len() > MAX_BYTES {
        return Err("the file is too large".into());
    }
    if raw.starts_with(&[0x1f, 0x8b]) { gunzip(&raw) } else { Ok(raw) }
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "a damaged gzip file".to_string();
    if data.len() < 18 || data[2] != 8 {
        return Err(bad());
    }
    let flags = data[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([*data.get(at).ok_or_else(bad)?, *data.get(at + 1).ok_or_else(bad)?]) as usize;
        at += 2 + n;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            at += data.get(at..).ok_or_else(bad)?.iter().position(|&b| b == 0).ok_or_else(bad)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(data.get(at..).ok_or_else(bad)?, MAX_BYTES).map_err(|e| {
        if matches!(e.status, miniz_oxide::inflate::TINFLStatus::HasMoreOutput) {
            "the unpacked drawing is too large".to_string()
        } else {
            bad()
        }
    })
}

//endregion

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

/// What [`parse`] needs besides the text.
#[derive(Clone)]
struct Source {
    text: Arc<Vec<u8>>,
    dir: Option<PathBuf>,
    neighbours: bool,
}

/// Parses the drawing, with fonts only when it has text.
fn parse(src: &Source) -> Result<usvg::Tree, String> {
    let mut opt =
        usvg::Options { resources_dir: src.dir.clone(), font_family: "sans-serif".into(), ..Default::default() };
    let has_text = memchr(&src.text, b"<text") || memchr(&src.text, b":text");
    if has_text {
        opt.fontdb = Arc::new(system_fonts());
    }
    let allowed = src.neighbours && src.dir.is_some();
    opt.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(move |href: &str, opts: &usvg::Options| {
            if !allowed || !local_href(href) {
                return None;
            }
            let path = opts.get_abs_path(Path::new(href));
            if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_IMAGE_BYTES) {
                return None;
            }
            (usvg::ImageHrefResolver::default_string_resolver())(href, opts)
        }),
    };
    let tree = usvg::Tree::from_data(&src.text, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    if !(size.width().is_finite() && size.height().is_finite()) || size.width() <= 0.0 || size.height() <= 0.0 {
        return Err("the drawing has no size".into());
    }
    Ok(tree)
}

/// A relative path inside the drawing's folder or below it: no scheme, no
/// absolute path, no drive letter.
fn local_href(href: &str) -> bool {
    let h = href.trim();
    !(h.is_empty()
        || h.contains("://")
        || h.starts_with('/')
        || h.starts_with('\\')
        || h.to_ascii_lowercase().starts_with("file:")
        || h.as_bytes().get(1) == Some(&b':'))
}

fn memchr(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The system's fonts, with the generic families mapped to fonts that
/// exist (fontdb's defaults name Windows fonts).
fn system_fonts() -> usvg::fontdb::Database {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    let has = |db: &usvg::fontdb::Database, name: &str| {
        db.faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
    };
    let pick = |db: &usvg::fontdb::Database, names: &[&str]| names.iter().find(|n| has(db, n)).map(|n| n.to_string());
    if let Some(n) =
        pick(&db, &["Times New Roman", "Liberation Serif", "DejaVu Serif", "Noto Serif", "FreeSerif", "Times"])
    {
        db.set_serif_family(n);
    }
    if let Some(n) = pick(&db, &["Arial", "Liberation Sans", "DejaVu Sans", "Noto Sans", "FreeSans", "Helvetica"]) {
        db.set_sans_serif_family(n);
    }
    if let Some(n) =
        pick(&db, &["Courier New", "Liberation Mono", "DejaVu Sans Mono", "Noto Sans Mono", "FreeMono", "Courier"])
    {
        db.set_monospace_family(n);
    }
    db
}

//endregion

/*┌─────────────────╮
| Drawing         |
└─────────────────╯*/
//region Drawing

/// Draws `region` (`[x0, y0, x1, y1]` in the drawing's units) into a `w` × `h`
/// picture, straight alpha.
fn draw(tree: &usvg::Tree, region: [f64; 4], w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("a picture of {w} × {h} pixels is too large"));
    }
    let [x0, y0, x1, y1] = region;
    if !(x1 > x0 && y1 > y0 && region.iter().all(|v| v.is_finite())) {
        return Err("an empty region".into());
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("no memory for the picture")?;
    let (sx, sy) = (w as f64 / (x1 - x0), h as f64 / (y1 - y0));
    let t = resvg::tiny_skia::Transform::from_row(sx as f32, 0.0, 0.0, sy as f32, (-x0 * sx) as f32, (-y0 * sy) as f32);
    resvg::render(tree, t, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied alpha.
    let mut px = pixmap.take();
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(px)
}

fn limits() -> slerp_isolate::Limits {
    slerp_isolate::Limits { time: TIME, memory: MEMORY, output: (MAX_PIXELS * 4) as usize + 4096 }
}

/// What [`facts`] reports, sent back from the child.
fn describe(tree: &usvg::Tree, text: &[u8]) -> Vec<u8> {
    let s = tree.size();
    let mut out = format!("{}\n{}\n", s.width(), s.height());
    let mut add = |k: &str, v: String| out.push_str(&format!("{k}\t{}\n", v.replace(['\n', '\t'], " ")));
    add("Size", format!("{} × {} px", trim(s.width()), trim(s.height())));
    let txt = String::from_utf8_lossy(&text[..text.len().min(1 << 20)]);
    if let Some(title) =
        between(&txt, "<title", "</title>").and_then(|t| t.split_once('>').map(|x| x.1.trim().to_string()))
        && !title.is_empty()
    {
        add("Title", title.chars().take(200).collect());
    }
    if let Some(vb) = attr(&txt, "viewBox") {
        add("View box", vb);
    }
    let (mut nodes, mut images, mut texts) = (0usize, 0usize, 0usize);
    count(tree.root(), &mut nodes, &mut images, &mut texts);
    add("Elements", nodes.to_string());
    if images > 0 {
        add("Images", images.to_string());
    }
    if texts > 0 {
        let faces = tree.fontdb().len();
        add("Text", format!("{texts} runs, drawn with {faces} system font faces available"));
    }
    if let Some(version) = between(&txt, "inkscape:version=\"", "\"") {
        add("Made with", format!("Inkscape {version}"));
    } else if memchr(text, b"Adobe Illustrator") {
        add("Made with", "Adobe Illustrator".into());
    }
    out.into_bytes()
}

fn count(g: &usvg::Group, nodes: &mut usize, images: &mut usize, texts: &mut usize) {
    for n in g.children() {
        *nodes += 1;
        match n {
            usvg::Node::Group(g) => count(g, nodes, images, texts),
            usvg::Node::Image(_) => *images += 1,
            usvg::Node::Text(_) => *texts += 1,
            usvg::Node::Path(_) => {}
        }
    }
}

fn trim(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)?;
    Some(&s[i..i + j])
}

/// An attribute of the root `svg` element.
fn attr(s: &str, name: &str) -> Option<String> {
    let start = s.find("<svg").or_else(|| s.find(":svg"))?;
    let tag = &s[start..start + s[start..].find('>')?];
    let i = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let j = tag[i..].find('"')?;
    Some(tag[i..i + j].to_string())
}

/// Linked files beside the drawing that it names (`href="..."`), for
/// watching them.
fn linked_files(text: &[u8], dir: &Path) -> Vec<PathBuf> {
    let txt = String::from_utf8_lossy(text);
    let mut out = Vec::new();
    for part in txt.split("href=").skip(1).take(4096) {
        let Some(q) = part.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        let Some(end) = part[1..].find(q) else { continue };
        let href = &part[1..1 + end];
        if local_href(href) && !href.starts_with('#') && !href.starts_with("data:") {
            let p = dir.join(href);
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Interface       |
└─────────────────╯*/
//region Interface

/// A drawing opened for the viewer: its size and facts, and a child process
/// keeping it parsed that draws any region on request.
pub struct Drawing {
    pub width: f64,
    pub height: f64,
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
    /// Files beside the drawing that it links to.
    pub neighbours: Vec<PathBuf>,
    worker: Mutex<Option<slerp_isolate::Worker>>,
    source: Source,
}

impl Drawing {
    pub fn open(path: &Path, limits: &Limits) -> Result<Drawing, String> {
        let text = Arc::new(read(path)?);
        let dir = path.parent().map(Path::to_path_buf);
        let source = Source { text: text.clone(), dir: dir.clone(), neighbours: limits.neighbours };
        let (worker, opening) = start(&source)?;
        let (width, height, facts) = parse_facts(&opening)?;
        let neighbours = match (&dir, limits.neighbours) {
            (Some(d), true) => linked_files(&text, d),
            _ => Vec::new(),
        };
        Ok(Drawing { width, height, facts, warnings: Vec::new(), neighbours, worker: Mutex::new(Some(worker)), source })
    }

    /// Draws `region` of the drawing into `width` × `height` pixels.  A
    /// child that died (a drawing that was too slow) is started again once.
    pub fn draw(&self, region: [f64; 4], width: u32, height: u32) -> Result<Image8, String> {
        let mut req = Vec::with_capacity(40);
        for v in region {
            req.extend_from_slice(&v.to_le_bytes());
        }
        req.extend_from_slice(&width.to_le_bytes());
        req.extend_from_slice(&height.to_le_bytes());
        let mut guard = self.worker.lock().map_err(|_| "the drawing's worker is unusable")?;
        if !guard.as_ref().is_some_and(|w| w.alive()) {
            *guard = Some(start(&self.source)?.0);
        }
        let worker = guard.as_mut().ok_or("no worker")?;
        let px = worker.ask(&req)?;
        if px.len() != width as usize * height as usize * 4 {
            return Err("the drawing came back the wrong size".into());
        }
        Ok(Image8 { width, height, pixels: px })
    }
}

fn start(source: &Source) -> Result<(slerp_isolate::Worker, Vec<u8>), String> {
    let src = source.clone();
    slerp_isolate::Worker::start(
        limits(),
        move || {
            let tree = parse(&src)?;
            let facts = describe(&tree, &src.text);
            Ok((tree, facts))
        },
        |tree: &mut usvg::Tree, req: &[u8]| {
            if req.len() != 40 {
                return Err("a bad request".into());
            }
            let f = |i: usize| f64::from_le_bytes(req[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
            let u = |i: usize| u32::from_le_bytes(req[i..i + 4].try_into().unwrap_or([0; 4]));
            draw(tree, [f(0), f(1), f(2), f(3)], u(32), u(36))
        },
    )
}

/// The drawing's width and height, and the facts for the inspector.
type Facts = (f64, f64, Vec<(String, String)>);

fn parse_facts(b: &[u8]) -> Result<Facts, String> {
    let text = String::from_utf8_lossy(b);
    let mut lines = text.lines();
    let w: f64 = lines.next().and_then(|l| l.parse().ok()).ok_or("no size")?;
    let h: f64 = lines.next().and_then(|l| l.parse().ok()).ok_or("no size")?;
    let facts = lines.filter_map(|l| l.split_once('\t')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Ok((w, h, facts))
}

/// The whole drawing as a picture: fitted into `limits.max_texture_size`
/// for thumbnails, else at its own size made at least 1024 pixels on its
/// longer side (drawings are often tiny icons) and at most 4096.
pub fn picture(path: &Path, limits: &Limits) -> Result<Texture, LoadError> {
    let text = Arc::new(read(path).map_err(LoadError::Invalid)?);
    let src = Source { text: text.clone(), dir: path.parent().map(Path::to_path_buf), neighbours: limits.neighbours };
    let max = limits.max_texture_size;
    let answer = slerp_isolate::run(limits_for_picture(), move || {
        let tree = parse(&src)?;
        let s = tree.size();
        let (w, h) = (s.width() as f64, s.height() as f64);
        let long = w.max(h);
        let target = match max {
            Some(m) => m as f64,
            None => long.clamp(1024.0, 4096.0),
        };
        let k = target / long;
        let (pw, ph) = (((w * k).round() as u32).max(1), ((h * k).round() as u32).max(1));
        let px = draw(&tree, [0.0, 0.0, w, h], pw, ph)?;
        let mut out = describe(&tree, &src.text);
        let header = out.len() as u32;
        out.extend_from_slice(&pw.to_le_bytes());
        out.extend_from_slice(&ph.to_le_bytes());
        out.extend_from_slice(&px);
        out.extend_from_slice(&header.to_le_bytes());
        Ok(out)
    })
    .map_err(LoadError::Invalid)?;
    // Facts, then width, height, pixels, then where the facts end.
    let n = answer.len();
    if n < 12 {
        return Err(LoadError::Invalid("no picture came back".into()));
    }
    let header = u32::from_le_bytes(answer[n - 4..].try_into().unwrap_or([0; 4])) as usize;
    let dims = answer.get(header..header + 8).ok_or_else(|| LoadError::Invalid("a bad answer".into()))?;
    let (pw, ph) =
        (u32::from_le_bytes(dims[..4].try_into().unwrap()), u32::from_le_bytes(dims[4..].try_into().unwrap()));
    let px = answer.get(header + 8..n - 4).ok_or_else(|| LoadError::Invalid("a bad answer".into()))?;
    if px.len() != pw as usize * ph as usize * 4 {
        return Err(LoadError::Invalid("the picture came back the wrong size".into()));
    }
    let (_, _, facts) = parse_facts(&answer[..header]).map_err(LoadError::Invalid)?;
    let mut tex = Texture::from_rgba8(pw, ph, px.to_vec(), ColorSpace::Srgb);
    tex.source.container =
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svgz")) { "SVGZ" } else { "SVG" }.into();
    tex.source.format = "vector drawing".into();
    tex.source.header = facts;
    tex.source.file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(tex)
}

fn limits_for_picture() -> slerp_isolate::Limits {
    limits()
}

//endregion

#[cfg(test)]
mod tests;
