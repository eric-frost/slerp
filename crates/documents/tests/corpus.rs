//! The documents in `local/assets/documents` (see `tools/assets.jsonc`):
//! every PDF and Illustrator file opens or is refused with a reason within
//! its time, page counts agree with poppler's `pdfinfo`, files that need a
//! password are refused and those with only an owner password open, page 1
//! looks like poppler's `pdftoppm` drawing of it by a loose measure and its
//! text has the letters poppler's `pdftotext` finds, and every EPS and
//! PostScript file gives a picture or a reason.  Skipped when
//! the assets haven't been fetched; the poppler checks when poppler isn't
//! installed.

use slerp_documents::{DocumentFormat, DrawView, PdfPages, Picture, load_path, sniff};
use slerp_scene::Limits;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/documents")
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
    out.sort();
}

fn format_of(path: &Path) -> Option<DocumentFormat> {
    let mut head = std::fs::read(path).ok()?;
    head.truncate(1024);
    sniff(path, &head)
}

fn has(program: &str) -> bool {
    Command::new(program).arg("-v").output().is_ok()
}

/// What poppler says: the page count, or `None` when it can't open the file.
fn pdfinfo_pages(path: &Path) -> Option<usize> {
    let out = Command::new("timeout").args(["10", "pdfinfo"]).arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.strip_prefix("Pages:")).and_then(|v| v.trim().parse().ok())
}

/// Poppler's drawing of page 1 with its long side `side` pixels, as RGB.
fn pdftoppm(path: &Path, side: u32, dir: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let stem = dir.join("ref");
    let ok = Command::new("timeout")
        .args(["20", "pdftoppm", "-f", "1", "-l", "1", "-singlefile", "-png", "-scale-to"])
        .arg(side.to_string())
        .arg(path)
        .arg(&stem)
        .output()
        .ok()?
        .status
        .success();
    if !ok {
        return None;
    }
    let png_path = stem.with_extension("png");
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&png_path).ok()?));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let _ = std::fs::remove_file(&png_path);
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgba => buf[..info.buffer_size()].chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
        png::ColorType::Grayscale => buf[..info.buffer_size()].iter().flat_map(|&g| [g, g, g]).collect(),
        _ => return None,
    };
    Some((info.width, info.height, rgb))
}

/// Poppler's text of page 1, or `None` when it can't read the file.
fn pdftotext(path: &Path) -> Option<String> {
    let out = Command::new("timeout")
        .args(["10", "pdftotext", "-q", "-f", "1", "-l", "1"])
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// How much of two texts' letters and digits agree, from 0 to 1, counting
/// each character as often as it appears, in any order: the texts' line
/// breaks, spacing and reading order differ between programs.
fn letters_agree(a: &str, b: &str) -> f64 {
    let count = |s: &str| {
        let mut m = std::collections::HashMap::<char, usize>::new();
        s.chars().filter(|c| c.is_alphanumeric()).for_each(|c| *m.entry(c).or_default() += 1);
        m
    };
    let (ca, cb) = (count(a), count(b));
    let (na, nb) = (ca.values().sum::<usize>(), cb.values().sum::<usize>());
    if na.max(nb) == 0 {
        return 1.0;
    }
    let shared: usize = ca.iter().map(|(c, n)| (*n).min(cb.get(c).copied().unwrap_or(0))).sum();
    shared as f64 / na.max(nb) as f64
}

/// The mean absolute difference of two pictures over the part they share,
/// from 0 to 255, after blurring both a little so anti-aliasing and
/// one-pixel shifts between renderers don't count.
fn difference(a: (u32, u32, &[u8]), b: (u32, u32, &[u8])) -> f64 {
    let (w, h) = (a.0.min(b.0) as usize, a.1.min(b.1) as usize);
    let blur = |(pw, _, px): (u32, u32, &[u8]), x: usize, y: usize, c: usize| -> f64 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for dy in 0..3 {
            for dx in 0..3 {
                let (xx, yy) = ((x + dx).saturating_sub(1).min(w - 1), (y + dy).saturating_sub(1).min(h - 1));
                sum += px[(yy * pw as usize + xx) * 3 + c] as f64;
                n += 1.0;
            }
        }
        sum / n
    };
    let mut total = 0.0;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                total += (blur(a, x, y, c) - blur(b, x, y, c)).abs();
            }
        }
    }
    total / (w * h * 3).max(1) as f64
}

#[test]
fn pdf_and_illustrator_corpus() {
    let mut all = Vec::new();
    for sub in ["pdf", "ai"] {
        files(&assets().join(sub), &mut all);
    }
    let docs: Vec<(PathBuf, DocumentFormat)> = all
        .into_iter()
        .filter_map(|p| format_of(&p).map(|f| (p, f)))
        .filter(|(p, _)| slerp_documents::is_pdf_file(p))
        .collect();
    if docs.is_empty() {
        eprintln!("no document assets; skipped");
        return;
    }
    let poppler = has("pdfinfo") && has("pdftoppm");
    let tmp = std::env::temp_dir().join(format!("slerp-doc-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (mut opened, mut refused, mut thumbs, mut stored) = (0, Vec::new(), 0, 0);
    let (mut counted, mut count_wrong) = (0, Vec::new());
    let mut diffs: Vec<(f64, String)> = Vec::new();
    let (mut texts, mut text_failed): (Vec<(f64, String)>, Vec<String>) = (Vec::new(), Vec::new());
    let mut slowest = (Duration::ZERO, String::new());
    let mut times = Vec::new();
    for (path, format) in &docs {
        let name = path.strip_prefix(assets()).unwrap().display().to_string();
        let lower = name.to_ascii_lowercase();
        // 1. A thumbnail, within the file manager's time.
        let started = Instant::now();
        let thumb = load_path(*format, path, &Limits { max_texture_size: Some(256), ..Limits::thumbnail() });
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "{name}: the thumbnail took {took:?}");
        times.push(took);
        if took > slowest.0 {
            slowest = (took, name.clone());
        }
        match &thumb {
            Ok(Picture::Image(t)) => {
                thumbs += 1;
                if t.source.header.first().is_some_and(|(k, v)| k == "Shown" && v.contains("stored")) {
                    stored += 1;
                }
            }
            Ok(Picture::Metafile { .. }) => panic!("{name}: a PDF gave a metafile"),
            Err(_) => {}
        }
        // 2. Passwords: files that need one are refused with that reason;
        //    those with only an owner password open.
        let needs_password = lower.contains("user-password") || lower.contains("password_encrypted");
        let owner_only = lower.contains("owner-only") || (lower.contains("hayro/encrypted_"));
        // 3. The document in the viewer's worker, and its page count.
        match PdfPages::open(*format, path) {
            Ok(mut pages) => {
                assert!(!needs_password, "{name} needs a password but opened");
                opened += 1;
                if poppler && let Some(n) = pdfinfo_pages(path) {
                    counted += 1;
                    if n != pages.total {
                        count_wrong.push(format!("{name}: {} pages, poppler {n}", pages.total));
                    }
                }
                // 4. Page 1 against poppler's.
                if poppler {
                    let size = pages.sizes[0];
                    let view = DrawView::fit(0, size, 128);
                    if let (Ok(ours), Some(theirs)) = (pages.draw(&view), pdftoppm(path, 128, &tmp)) {
                        let rgb: Vec<u8> = ours
                            .pixels
                            .chunks_exact(4)
                            .flat_map(|p| {
                                let a = p[3] as u32;
                                [p[0], p[1], p[2]].map(|c| ((c as u32 * a + 255 * (255 - a)) / 255) as u8)
                            })
                            .collect();
                        let d = difference((ours.width, ours.height, &rgb), (theirs.0, theirs.1, &theirs.2));
                        diffs.push((d, name.clone()));
                    }
                }
                // 5. Page 1's text, within the drawing's time, against
                //    poppler's where poppler finds some.
                let started = Instant::now();
                let text = pages.text(0);
                assert!(started.elapsed() < Duration::from_secs(35), "{name}: the text took {:?}", started.elapsed());
                match text {
                    Ok(t) if poppler => {
                        if let Some(theirs) = pdftotext(path).filter(|t| t.chars().any(char::is_alphanumeric)) {
                            texts.push((letters_agree(&t.all_text(), &theirs), name.clone()));
                        }
                    }
                    Ok(_) => {}
                    Err(e) => text_failed.push(format!("{name}: {e}")),
                }
            }
            Err(e) => {
                let e = e.to_string();
                assert!(!owner_only, "{name} has only an owner password but was refused: {e}");
                if needs_password {
                    assert!(e.contains("password"), "{name}: {e}");
                }
                refused.push(format!("{name}: {e}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    diffs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let close = diffs.iter().filter(|(d, _)| *d < 12.0).count();
    eprintln!(
        "{} PDF and Illustrator files: {opened} opened, {} refused, {thumbs} thumbnails ({stored} stored pictures), slowest thumbnail {:?} ({})",
        docs.len(),
        refused.len(),
        slowest.0,
        slowest.1
    );
    times.sort();
    eprintln!(
        "thumbnail times: median {:?}, 90th percentile {:?}",
        times[times.len() / 2],
        times[times.len() * 9 / 10]
    );
    for r in &refused {
        eprintln!("  refused: {r}");
    }
    eprintln!("page counts: {} of {counted} agree with pdfinfo", counted - count_wrong.len());
    for c in &count_wrong {
        eprintln!("  {c}");
    }
    if !diffs.is_empty() {
        let median = diffs[diffs.len() / 2].0;
        eprintln!("against pdftoppm: {close} of {} within 12/255, median {median:.2}", diffs.len());
        for (d, n) in diffs.iter().rev().take(12) {
            eprintln!("  {d:6.2} {n}");
        }
        assert!(median < 4.0, "page 1 differs from poppler's by a median {median:.2}");
        assert!(close * 100 >= diffs.len() * 85, "only {close} of {} pages are close to poppler's", diffs.len());
    }
    assert!(count_wrong.len() * 20 <= counted.max(1), "page counts disagree: {count_wrong:?}");
    if !texts.is_empty() {
        texts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let agree = texts.iter().filter(|(a, _)| *a >= 0.9).count();
        eprintln!("text against pdftotext: {agree} of {} pages share 90 % of their letters", texts.len());
        for (a, n) in texts.iter().take(12) {
            eprintln!("  {:5.1} % {n}", a * 100.0);
        }
        // The rest are mostly fonts without a map to Unicode the
        // interpreter reads (Type 3 fonts, symbolic TrueType without a
        // ToUnicode map), which poppler guesses from glyph names.
        assert!(agree * 100 >= texts.len() * 80, "only {agree} of {} pages' text is like poppler's", texts.len());
    }
    for f in &text_failed {
        eprintln!("  no text: {f}");
    }
}

#[test]
fn eps_and_postscript_corpus() {
    let mut all = Vec::new();
    for sub in ["eps", "ps", "ai"] {
        files(&assets().join(sub), &mut all);
    }
    let docs: Vec<(PathBuf, DocumentFormat)> = all
        .into_iter()
        .filter_map(|p| format_of(&p).map(|f| (p, f)))
        .filter(|(p, _)| !slerp_documents::is_pdf_file(p))
        .collect();
    if docs.is_empty() {
        eprintln!("no EPS assets; skipped");
        return;
    }
    let (mut pictures, mut previews, mut failed) = (0, 0, Vec::new());
    for (path, format) in &docs {
        let name = path.strip_prefix(assets()).unwrap().display().to_string();
        let started = Instant::now();
        let r = load_path(*format, path, &Limits { max_texture_size: Some(256), ..Limits::thumbnail() });
        let took = started.elapsed();
        assert!(took < Duration::from_secs(4), "{name}: took {took:?}");
        match r {
            Ok(Picture::Image(t)) => {
                pictures += 1;
                if t.source.header.first().is_some_and(|(_, v)| v.contains("stored")) {
                    previews += 1;
                }
            }
            Ok(Picture::Metafile { .. }) => {
                pictures += 1;
                previews += 1;
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    eprintln!(
        "{} EPS, PostScript and old Illustrator files: {pictures} pictures ({previews} stored previews)",
        docs.len()
    );
    for f in &failed {
        eprintln!("  {f}");
    }
    if slerp_documents::ghostscript().is_some() {
        assert!(failed.len() * 10 <= docs.len(), "too many failures: {failed:?}");
    }
}
