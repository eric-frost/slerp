//! EPS and PostScript: the DOS EPS binary header and the TIFF or Windows
//! Metafile preview behind it, EPSI preview bitmaps, the comments of the
//! Document Structuring Conventions (bounding box, pages, title, creator),
//! and pictures of the pages through Ghostscript when it is installed.

use crate::text::{ps_text, trim_bytes};
use crate::{DocumentFormat, Picture, ghostscript};
use slerp_scene::LoadError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The first four bytes of a DOS EPS binary file (0xC6D3D0C5 little-endian).
pub const DOS_MAGIC: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];
/// The largest preview section read into memory.
const MAX_PREVIEW_BYTES: u64 = 64 << 20;
/// How much of the PostScript's start is searched for comments and an
/// EPSI preview, and how much of its end for comments deferred there
/// (`(atend)`).
const HEAD_BYTES: u64 = 512 << 10;
const TAIL_BYTES: u64 = 64 << 10;
/// The largest EPSI preview, per side.
const MAX_EPSI_SIDE: u32 = 4096;

/*┌─────────────────╮
| Binary header   |
└─────────────────╯*/
//region Binary header

/// Where the sections of a DOS EPS binary file are: the PostScript, and the
/// preview as a Windows Metafile or a TIFF picture, each as (offset,
/// length) in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryHeader {
    pub ps: (u64, u64),
    pub wmf: Option<(u64, u64)>,
    pub tiff: Option<(u64, u64)>,
}

impl BinaryHeader {
    /// Reads the 30-byte header at the start of `head`, checking each
    /// section against the file's length.  `None` when the file has no such
    /// header; an error when it has one that doesn't fit the file.
    pub fn parse(head: &[u8], file_len: u64) -> Result<Option<BinaryHeader>, String> {
        if !head.starts_with(&DOS_MAGIC) {
            return Ok(None);
        }
        let u = |o: usize| head.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64);
        let field = |o: usize| u(o).zip(u(o + 4)).ok_or("the EPS header is cut short");
        let section = |(start, len): (u64, u64)| -> Result<Option<(u64, u64)>, String> {
            if start == 0 || len == 0 {
                return Ok(None);
            }
            match start.checked_add(len) {
                Some(end) if start >= 30 && end <= file_len => Ok(Some((start, len))),
                _ => Err("a section of the EPS file lies outside it".into()),
            }
        };
        let ps = section(field(4)?)?.ok_or("the EPS file has no PostScript section")?;
        // A preview that doesn't fit is left out rather than spoiling the
        // PostScript.
        let wmf = section(field(12)?).ok().flatten();
        let tiff = section(field(20)?).ok().flatten();
        Ok(Some(BinaryHeader { ps, wmf, tiff }))
    }
}

//endregion

/*┌─────────────────╮
| Comments        |
└─────────────────╯*/
//region Comments

/// An EPSI preview's header: `%%BeginPreview: width height depth lines`,
/// and where its first data line starts in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpsiHeader {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub lines: u32,
    pub offset: u64,
}

/// What a PostScript file's structuring comments say.
#[derive(Clone, Debug, Default)]
pub struct Dsc {
    /// `%%BoundingBox` (lower-left and upper-right corners in points), and
    /// the finer `%%HiResBoundingBox` when given.
    pub bbox: Option<[f64; 4]>,
    pub hires: Option<[f64; 4]>,
    pub pages: Option<usize>,
    pub title: Option<String>,
    pub creator: Option<String>,
    pub created: Option<String>,
    pub level: Option<String>,
    /// The first line: `%!PS-Adobe-3.0 EPSF-3.0` and the like.
    pub version: String,
    pub preview: Option<EpsiHeader>,
}

impl Dsc {
    /// The bounding box to draw: the fine one if it is usable.
    pub fn best_bbox(&self) -> Option<[f64; 4]> {
        let ok = |b: &[f64; 4]| b.iter().all(|v| v.is_finite()) && b[2] > b[0] && b[3] > b[1];
        self.hires.filter(ok).or(self.bbox.filter(ok))
    }

    /// Reads the comments from the start of the PostScript (`head`, which
    /// starts at file offset `head_offset`) and from its end (`tail`), where
    /// values deferred with `(atend)` are given.
    pub fn parse(head: &[u8], head_offset: u64, tail: &[u8]) -> Dsc {
        let mut d = Dsc::default();
        let mut at = 0usize;
        let mut header_done = false;
        while at < head.len() {
            let end = head[at..].iter().position(|&b| b == b'\n' || b == b'\r').map_or(head.len(), |e| at + e);
            let line = &head[at..end];
            let next = end + usize::from(end < head.len());
            // A CR LF pair is one line end.
            let next = if head.get(end) == Some(&b'\r') && head.get(next) == Some(&b'\n') { next + 1 } else { next };
            if at == 0 {
                d.version = String::from_utf8_lossy(line.strip_prefix(b"\x04").unwrap_or(line)).trim().to_string();
            }
            if let Some(rest) = line.strip_prefix(b"%%BeginPreview:") {
                let n: Vec<u32> =
                    String::from_utf8_lossy(rest).split_whitespace().filter_map(|v| v.parse().ok()).collect();
                if let [w, h, depth, lines, ..] = n[..] {
                    d.preview =
                        Some(EpsiHeader { width: w, height: h, depth, lines, offset: head_offset + next as u64 });
                }
                break;
            }
            if line.starts_with(b"%%EndComments") || line.starts_with(b"%%BeginProlog") {
                header_done = true;
            }
            if !header_done {
                d.take(line);
            } else if !line.starts_with(b"%") && !line.iter().all(u8::is_ascii_whitespace) {
                // The preview comes right after the header comments; past
                // the first code there is none.
                break;
            }
            at = next;
        }
        // Values given at the end replace "(atend)".
        for line in tail.split(|&b| b == b'\n' || b == b'\r') {
            if line.starts_with(b"%%BoundingBox:")
                || line.starts_with(b"%%HiResBoundingBox:")
                || line.starts_with(b"%%Pages:")
            {
                d.take(line);
            }
        }
        d
    }

    /// Takes one header comment.
    fn take(&mut self, line: &[u8]) {
        let Some(colon) = line.iter().position(|&b| b == b':') else { return };
        let (key, value) = (&line[..colon], trim_bytes(&line[colon + 1..]));
        // Deferred to the end, or left unset ("none", as LibreOffice writes).
        if value == b"(atend)" || value.eq_ignore_ascii_case(b"none") || value.eq_ignore_ascii_case(b"(none)") {
            return;
        }
        let numbers = || -> Option<[f64; 4]> {
            let v: Vec<f64> =
                String::from_utf8_lossy(value).split_whitespace().filter_map(|n| n.parse().ok()).collect();
            v.get(..4).and_then(|s| s.try_into().ok())
        };
        match key {
            b"%%BoundingBox" => self.bbox = numbers().or(self.bbox),
            b"%%HiResBoundingBox" => self.hires = numbers().or(self.hires),
            b"%%Pages" => {
                let first = String::from_utf8_lossy(value).split_whitespace().next().and_then(|n| n.parse().ok());
                self.pages = first.or(self.pages);
            }
            b"%%Title" => self.title = Some(ps_text(value)).filter(|t| !t.is_empty()),
            b"%%Creator" => self.creator = Some(ps_text(value)).filter(|t| !t.is_empty()),
            b"%%CreationDate" => self.created = Some(ps_text(value)).filter(|t| !t.is_empty()),
            b"%%LanguageLevel" => self.level = Some(ps_text(value)).filter(|t| !t.is_empty()),
            _ => {}
        }
    }
}

/// The PostScript section of a file: all of it, or what a DOS EPS header
/// points to; with the header, if any.
fn sections(f: &mut File) -> Result<(Option<BinaryHeader>, (u64, u64)), String> {
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut head = [0u8; 30];
    let n = read_at(f, 0, &mut head)?;
    let binary = BinaryHeader::parse(&head[..n], len)?;
    let ps = binary.map_or((0, len), |b| b.ps);
    Ok((binary, ps))
}

fn read_at(f: &mut File, offset: u64, buf: &mut [u8]) -> Result<usize, String> {
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(got)
}

fn read_section(f: &mut File, (start, len): (u64, u64), most: u64) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len.min(most) as usize];
    let n = read_at(f, start, &mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// Reads the structuring comments of the PostScript in `path`.
pub fn read_dsc(path: &Path) -> Result<Dsc, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let (_, ps) = sections(&mut f)?;
    read_dsc_of(&mut f, ps)
}

fn read_dsc_of(f: &mut File, (start, len): (u64, u64)) -> Result<Dsc, String> {
    let head = read_section(f, (start, len), HEAD_BYTES)?;
    let tail_len = len.min(TAIL_BYTES);
    let tail = read_section(f, (start + len - tail_len, tail_len), TAIL_BYTES)?;
    Ok(Dsc::parse(&head, start, &tail))
}

//endregion

/*┌─────────────────╮
| Previews        |
└─────────────────╯*/
//region Previews

/// Decodes an EPSI preview: hexadecimal comment lines, rows from the top,
/// with the largest value black (Adobe's EPSF specification inverts the
/// image operator's sense; Ghostscript's `ps2epsi` follows it).
/// ImageMagick writes 1 for white instead, so `inverted` flips it back.
/// Returns straight RGBA.
pub fn decode_epsi(data: &[u8], h: &EpsiHeader, inverted: bool) -> Result<(u32, u32, Vec<u8>), String> {
    if !(1..=MAX_EPSI_SIDE).contains(&h.width) || !(1..=MAX_EPSI_SIDE).contains(&h.height) {
        return Err("the EPSI preview has an unusable size".into());
    }
    if !matches!(h.depth, 1 | 2 | 4 | 8) {
        return Err(format!("EPSI previews of {} bits aren't supported", h.depth));
    }
    let row_bytes = (h.width as usize * h.depth as usize).div_ceil(8);
    let need = row_bytes * h.height as usize;
    // 1. The hex digits of the comment lines, up to %%EndPreview.
    let mut bytes = Vec::with_capacity(need);
    let mut high: Option<u8> = None;
    'lines: for line in data.split(|&b| b == b'\n' || b == b'\r') {
        if line.starts_with(b"%%EndPreview") {
            break;
        }
        let Some(rest) = line.strip_prefix(b"%") else { continue };
        for &c in rest {
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => continue,
            };
            match high.take() {
                None => high = Some(v),
                Some(hi) => {
                    bytes.push(hi << 4 | v);
                    if bytes.len() == need {
                        break 'lines;
                    }
                }
            }
        }
    }
    if bytes.len() < need {
        // A preview cut short still shows its top; the rest stays white.
        bytes.resize(need, 0);
    }
    // 2. Samples to grey.
    let max = (1u32 << h.depth) - 1;
    let mut rgba = Vec::with_capacity(h.width as usize * h.height as usize * 4);
    for row in bytes.chunks_exact(row_bytes) {
        for x in 0..h.width as usize {
            let bit = x * h.depth as usize;
            let byte = row[bit / 8];
            let shift = 8 - h.depth as usize - bit % 8;
            let v = (byte >> shift) as u32 & max;
            let ink = if inverted { max - v } else { v };
            let g = (255 - ink * 255 / max) as u8;
            rgba.extend([g, g, g, 255]);
        }
    }
    Ok((h.width, h.height, rgba))
}

/// Decodes a TIFF preview through `slerp-tiff`, which reads files: the
/// section goes to a temporary file first.
fn decode_tiff(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>, String), String> {
    use slerp_tiff::{Options, Sample};
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("slerp-eps-preview-{}-{n}.tif", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("couldn't write a temporary file: {e}"))?;
    let decoded = slerp_tiff::decode_page(&tmp, 0, &Options { min_long_side: 0, max_pixels: 1 << 26 });
    let _ = std::fs::remove_file(&tmp);
    let r = decoded?;
    let (w, h) = (r.width, r.height);
    let bytes_per = r.sample.size();
    let value = |i: usize| -> u8 {
        match r.sample {
            Sample::U8 => r.bytes[i],
            Sample::U16 => r.bytes[i * 2 + 1],
            _ => 0,
        }
    };
    if !matches!(r.sample, Sample::U8 | Sample::U16) || r.bytes.len() < w * h * r.samples * bytes_per {
        return Err("the TIFF preview's samples aren't a picture".into());
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let px = match r.samples {
            1 | 2 => {
                let g = if r.invert { 255 - value(i) } else { value(i) };
                let a = r.alpha.as_ref().map_or(255, |a| a.get(i * bytes_per + bytes_per - 1).copied().unwrap_or(255));
                [g, g, g, a]
            }
            s => {
                let a = if s >= 4 { value(i * s + 3) } else { 255 };
                [value(i * s), value(i * s + 1), value(i * s + 2), a]
            }
        };
        rgba.extend(px);
    }
    let kind = if r.samples >= 3 { "colour" } else { "greyscale" };
    Ok((w as u32, h as u32, rgba, format!("TIFF, {kind}, {w} × {h}")))
}

/// Whether ImageMagick made the file: its EPSI previews use 1 for white.
fn imagemagick(d: &Dsc) -> bool {
    d.creator.as_deref().is_some_and(|c| c.to_ascii_lowercase().contains("imagemagick"))
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// The names of an EPS file's two pictures when it has both: the preview
/// it stores (the default, as quick as any thumbnail), and Ghostscript's
/// drawing of its PostScript.
pub const STORED: &str = "Stored preview";
pub const DRAWN: &str = "Drawn by Ghostscript";

/// What an EPS or PostScript file offers to show: its stored preview, if
/// any, and whether Ghostscript can draw it.
pub fn choices(path: &Path) -> Vec<String> {
    let Ok(mut f) = File::open(path) else { return Vec::new() };
    let Ok((binary, ps)) = sections(&mut f) else { return Vec::new() };
    let epsi = read_dsc_of(&mut f, ps).ok().and_then(|d| d.preview).is_some();
    let stored = binary.is_some_and(|b| b.tiff.is_some() || b.wmf.is_some())
        || epsi
        || crate::stored::xmp_thumbnail(path).is_some();
    if stored && ghostscript().is_some() { vec![STORED.into(), DRAWN.into()] } else { Vec::new() }
}

/// A picture of an EPS or PostScript file: page `index` (counting from 0)
/// of a PostScript document; for an EPS (and a PostScript Illustrator
/// file), index 0 is the stored preview when there is one, else
/// Ghostscript's drawing, and index 1 Ghostscript's drawing.  `side` fits
/// a thumbnail.
pub fn picture(format: DocumentFormat, path: &Path, index: usize, side: Option<u32>) -> Result<Picture, LoadError> {
    let mut f = File::open(path)?;
    let (binary, ps) = sections(&mut f).map_err(LoadError::Invalid)?;
    let dsc = read_dsc_of(&mut f, ps).map_err(LoadError::Invalid)?;
    let mut facts = facts(format, &dsc, binary.as_ref());
    let pages = format == DocumentFormat::PostScript;

    // 1. The stored preview: a TIFF, a metafile or an EPSI bitmap.
    if !pages && index == 0 {
        if let Some(p) = stored_preview(&mut f, format, path, binary, &dsc, &mut facts)? {
            return Ok(p);
        }
    } else if !pages && index > 1 {
        return Err(LoadError::Unsupported(format!("an EPS has no picture {}", index + 1)));
    }

    // 2. Ghostscript's drawing.
    let Some(gs) = ghostscript() else {
        let what = if pages { "PostScript" } else { "EPS without a preview" };
        return Err(LoadError::Unsupported(format!("{what} needs Ghostscript, which isn't installed")));
    };
    let page = if pages { Some(index + 1) } else { None };
    let size = dsc.best_bbox().map(|b| (b[2] - b[0], b[3] - b[1]));
    let drawn = ghostscript::draw(gs, path, ps, page, !pages, size, side).map_err(LoadError::Unsupported)?;
    let version = crate::ghostscript_version().map(|v| format!(" {v}")).unwrap_or_default();
    facts.insert(0, ("Shown".into(), format!("Drawn by Ghostscript{version} at {:.0} dpi", drawn.dpi)));
    if let (true, Some(n)) = (pages, dsc.pages) {
        facts.push(("Page shown".into(), format!("{} of {n}", index + 1)));
    }
    let container = if pages { "PostScript" } else { "EPS" };
    Ok(Picture::Image(crate::texture(drawn.width, drawn.height, drawn.rgba, container, facts)))
}

/// The preview picture an EPS (or a PostScript Illustrator file) stores,
/// read without Ghostscript: a TIFF or Windows Metafile in the DOS binary
/// header, an EPSI bitmap, or the XMP metadata's thumbnail; `None` without
/// one, and for PostScript documents, whose pages are always drawn.  This
/// is the picture Slerp's thumbnails show (see `crate::saved_picture`).
pub fn stored(format: DocumentFormat, path: &Path) -> Result<Option<Picture>, LoadError> {
    if format == DocumentFormat::PostScript {
        return Ok(None);
    }
    let mut f = File::open(path)?;
    let (binary, ps) = sections(&mut f).map_err(LoadError::Invalid)?;
    let dsc = read_dsc_of(&mut f, ps).map_err(LoadError::Invalid)?;
    let mut facts = facts(format, &dsc, binary.as_ref());
    stored_preview(&mut f, format, path, binary, &dsc, &mut facts)
}

/// Step 1 of [`picture`] and all of [`stored`]: the stored preview, with
/// `facts` in front of the picture's own note.  A preview that can't be
/// read adds a fact and gives `None`.
fn stored_preview(
    f: &mut File,
    format: DocumentFormat,
    path: &Path,
    binary: Option<BinaryHeader>,
    dsc: &Dsc,
    facts: &mut Vec<(String, String)>,
) -> Result<Option<Picture>, LoadError> {
    let note = |what: &str| ("Shown".to_string(), format!("The preview picture stored in the file ({what})"));
    if let Some(t) = binary.and_then(|b| b.tiff) {
        match read_section(f, t, MAX_PREVIEW_BYTES).and_then(|b| decode_tiff(&b)) {
            Ok((w, h, rgba, what)) => {
                facts.insert(0, note(&what));
                return Ok(Some(Picture::Image(crate::texture(
                    w,
                    h,
                    rgba,
                    "EPS with a TIFF preview",
                    std::mem::take(facts),
                ))));
            }
            Err(e) => facts.push(("Preview".into(), format!("a TIFF that couldn't be read: {e}"))),
        }
    }
    if let Some(wmf) = binary.and_then(|b| b.wmf) {
        let bytes = read_section(f, wmf, MAX_PREVIEW_BYTES).map_err(LoadError::Invalid)?;
        facts.insert(0, note("Windows Metafile"));
        return Ok(Some(Picture::Metafile { bytes, facts: std::mem::take(facts) }));
    }
    if let Some(p) = &dsc.preview {
        let len = (p.height as u64 + 2) * (p.width as u64 * p.depth as u64 / 4 + 8) * 2 + 4096;
        let data =
            read_section(f, (p.offset, len.min(MAX_PREVIEW_BYTES)), MAX_PREVIEW_BYTES).map_err(LoadError::Invalid)?;
        let (w, h, rgba) = decode_epsi(&data, p, imagemagick(dsc)).map_err(LoadError::Invalid)?;
        facts.insert(0, note(&format!("EPSI, {}-bit, {w} × {h}", p.depth)));
        return Ok(Some(Picture::Image(crate::texture(w, h, rgba, "EPS with an EPSI preview", std::mem::take(facts)))));
    }
    // Adobe's programs also keep a thumbnail in the XMP metadata.
    if let Some(mut t) = crate::xmp_picture(format, path) {
        facts.insert(0, t.source.header.remove(0));
        t.source.header = std::mem::take(facts);
        return Ok(Some(Picture::Image(t)));
    }
    Ok(None)
}

/// The inspector's facts about a file.
fn facts(format: DocumentFormat, d: &Dsc, binary: Option<&BinaryHeader>) -> Vec<(String, String)> {
    let mut f = Vec::new();
    let what = match (format, binary) {
        (DocumentFormat::Illustrator, _) => "Adobe Illustrator, PostScript (saved without PDF compatibility)",
        (DocumentFormat::PostScript, _) => "PostScript",
        (_, Some(_)) => "EPS with a DOS binary header",
        _ => "EPS",
    };
    let version = d.version.trim_start_matches("%!").trim();
    f.push(("Format".into(), if version.is_empty() { what.into() } else { format!("{what} ({version})") }));
    if let Some(b) = d.best_bbox() {
        let size = crate::describe_size(b[2] - b[0], b[3] - b[1]);
        f.push(("Bounding box".into(), format!("{} {} {} {} points: {size}", b[0], b[1], b[2], b[3])));
    }
    if let Some(n) = d.pages.filter(|_| format == DocumentFormat::PostScript) {
        f.push(("Pages".into(), n.to_string()));
    }
    for (k, v) in [("Title", &d.title), ("Made with", &d.creator), ("Created", &d.created)] {
        if let Some(v) = v {
            f.push((k.into(), v.clone()));
        }
    }
    if let Some(l) = &d.level {
        f.push(("PostScript level".into(), l.clone()));
    }
    f
}

//endregion
