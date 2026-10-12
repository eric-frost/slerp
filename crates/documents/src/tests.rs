use super::*;
use crate::eps::{BinaryHeader, Dsc, EpsiHeader, decode_epsi};
use slerp_scene::textures::Pixels;
use std::path::PathBuf;
use std::time::Instant;

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

/// A folder of its own for a test's files.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("slerp-documents-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A PDF from the bodies of its objects (object 1 first), with a correct
/// cross-reference table and the catalog as object 1.  `trailer` adds
/// entries to the trailer.
fn pdf_from(objects: &[Vec<u8>], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!("trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend(data);
    s.extend(b"\nendstream");
    s
}

/// A PDF whose pages draw `contents`, each on a page of `size` points.
fn pdf_pages(contents: &[&str], size: (u32, u32), page_extra: &str) -> Vec<u8> {
    let n = contents.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
    ];
    for (i, c) in contents.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R {page_extra} >>",
                size.0,
                size.1,
                4 + 2 * i
            )
            .into_bytes(),
        );
        objects.push(stream("", c.as_bytes()));
    }
    pdf_from(&objects, "")
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn rgba(t: &Texture) -> &[u8] {
    match &t.levels[0].surfaces[0] {
        Pixels::Rgba8(px) => px,
        _ => panic!("not 8-bit"),
    }
}

fn pixel(t: &Texture, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * t.width() + x) * 4) as usize;
    rgba(t)[i..i + 4].try_into().unwrap()
}

fn thumb(size: u32) -> Limits {
    Limits { max_texture_size: Some(size), ..Limits::thumbnail() }
}

fn image(p: Result<Picture, LoadError>) -> Texture {
    match p {
        Ok(Picture::Image(t)) => t,
        Ok(Picture::Metafile { .. }) => panic!("a metafile, not a picture"),
        Err(e) => panic!("{e}"),
    }
}

const RED_PAGE: &str = "1 0 0 rg 0 0 612 792 re f";
const BLUE_PAGE: &str = "0 0 1 rg 0 0 612 792 re f";

//endregion

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

#[test]
fn documents_are_recognised_by_content_and_name() {
    let p = Path::new;
    assert_eq!(sniff(p("a.pdf"), b"%PDF-1.4\n"), Some(DocumentFormat::Pdf));
    assert_eq!(sniff(p("download"), b"\xEF\xBB\xBFjunk\n%PDF-1.7\n"), Some(DocumentFormat::Pdf));
    assert_eq!(sniff(p("logo.ai"), b"%PDF-1.6\n"), Some(DocumentFormat::Illustrator));
    assert_eq!(sniff(p("old.AI"), b"%!PS-Adobe-3.0\n"), Some(DocumentFormat::Illustrator));
    assert_eq!(sniff(p("x.bin"), &[0xC5, 0xD0, 0xD3, 0xC6, 30, 0]), Some(DocumentFormat::Eps));
    assert_eq!(sniff(p("fig"), b"%!PS-Adobe-3.0 EPSF-3.0\n"), Some(DocumentFormat::Eps));
    assert_eq!(sniff(p("fig.eps"), b"%!PS-Adobe-2.0\n"), Some(DocumentFormat::Eps));
    assert_eq!(sniff(p("book.ps"), b"%!PS-Adobe-3.0\n"), Some(DocumentFormat::PostScript));
    assert_eq!(sniff(p("book"), b"\x04%!PS-Adobe-3.0\n"), Some(DocumentFormat::PostScript));
    // `%!` alone is too weak without a name that says PostScript.
    assert_eq!(sniff(p("notes.txt"), b"%! not postscript\n"), None);
    assert_eq!(sniff(p("a.ai"), b"<svg/>"), None);
    assert_eq!(sniff(p("a.pdf"), b"PK\x03\x04"), None);
    // PDF wins over the name.
    assert_eq!(sniff(p("mislabelled.eps"), b"%PDF-1.5\n"), Some(DocumentFormat::Pdf));
}

#[test]
fn page_names_and_sizes() {
    assert_eq!(page_index(DocumentFormat::Pdf, "Page 3"), Some(2));
    assert_eq!(page_index(DocumentFormat::Illustrator, "Artboard 1"), Some(0));
    assert_eq!(page_index(DocumentFormat::Pdf, "12"), Some(11));
    assert_eq!(page_index(DocumentFormat::Pdf, "Page 0"), None);
    assert_eq!(page_index(DocumentFormat::Pdf, "Cover"), None);
    assert_eq!(describe_size(595.28, 841.89), "210 × 297 mm (A4)");
    assert_eq!(describe_size(792.0, 612.0), "11.0 × 8.5 in (Letter, landscape)");
    assert_eq!(describe_size(100.0, 50.0), "100 × 50 pt (35 × 18 mm)");
}

#[test]
fn pdf_text_strings() {
    assert_eq!(text::pdf_text(b"\xFE\xFF\x00H\x00i\x20\x14"), "Hi—");
    assert_eq!(text::pdf_text(b"Caf\xE9 \x84 \x92"), "Café — ™");
    assert_eq!(text::pdf_text(b"\xEF\xBB\xBFna\xC3\xAFve\0\0"), "naïve");
    assert_eq!(text::ps_text(b" (A \\(paren\\) \\101) "), "A (paren) A");
    let long = vec![b'x'; 1000];
    assert!(text::pdf_text(&long).chars().count() <= 301);
}

//endregion

/*┌─────────────────╮
| PDF             |
└─────────────────╯*/
//region PDF

#[test]
fn a_pdf_page_becomes_a_picture() {
    let d = dir("pdf-page");
    let path = write(&d, "red.pdf", &pdf_pages(&[RED_PAGE], (612, 792), ""));
    let t = image(load_path(DocumentFormat::Pdf, &path, &thumb(128)));
    assert_eq!((t.width(), t.height()), (99, 128));
    assert_eq!(pixel(&t, 50, 64), [255, 0, 0, 255]);
    let facts: Vec<&str> = t.source.header.iter().map(|(k, _)| k.as_str()).collect();
    assert!(facts.contains(&"Page size") && facts.contains(&"Format"), "{facts:?}");
    assert!(page_names(DocumentFormat::Pdf, &path).is_empty());
}

#[test]
fn pages_are_chosen_by_name() {
    let d = dir("pdf-pages");
    let path = write(&d, "three.pdf", &pdf_pages(&[RED_PAGE, BLUE_PAGE, RED_PAGE], (612, 792), ""));
    assert_eq!(page_names(DocumentFormat::Pdf, &path), ["Page 1", "Page 2", "Page 3"]);
    assert_eq!(page_names(DocumentFormat::Illustrator, &path)[1], "Artboard 2");
    let t = image(load_page(DocumentFormat::Pdf, &path, &thumb(64), "Page 2"));
    assert_eq!(pixel(&t, 20, 20), [0, 0, 255, 255]);
    assert!(t.source.header.iter().any(|(k, v)| k == "Shown" && v == "Page 2 of 3"));
    assert!(load_page(DocumentFormat::Pdf, &path, &thumb(64), "Page 4").is_err());
    assert!(load_page(DocumentFormat::Pdf, &path, &thumb(64), "Index").is_err());
}

#[test]
fn rotated_pages_are_drawn_upright() {
    let d = dir("pdf-rotate");
    // A red band along the bottom of a portrait page; turned 90 degrees
    // clockwise it runs down the left side of a landscape picture.
    let path = write(&d, "r.pdf", &pdf_pages(&["1 0 0 rg 0 0 612 100 re f"], (612, 792), "/Rotate 90"));
    let t = image(load_path(DocumentFormat::Pdf, &path, &thumb(200)));
    assert!(t.width() > t.height(), "{}x{}", t.width(), t.height());
    assert_eq!(pixel(&t, 5, t.height() / 2), [255, 0, 0, 255]);
    assert_eq!(pixel(&t, t.width() - 5, t.height() / 2), [255, 255, 255, 255]);
}

#[test]
fn the_viewer_draws_any_part_of_any_page_at_any_scale() {
    let d = dir("pdf-view");
    // Page 2: white with a black 10-point square at (100, 100) from the
    // top left (PDF's origin is the bottom left).
    let square = "0 0 0 rg 100 682 10 10 re f";
    let path = write(&d, "v.pdf", &pdf_pages(&[RED_PAGE, square], (612, 792), ""));
    let mut pages = PdfPages::open(DocumentFormat::Pdf, &path).unwrap();
    assert_eq!(pages.sizes.len(), 2);
    assert_eq!(pages.total, 2);
    assert_eq!(pages.names(), ["Page 1", "Page 2"]);
    assert!(pages.facts.iter().any(|(k, v)| k == "Pages" && v == "2"));
    // Zoomed 20 times on the square's corner: 200 pixels show 10 points.
    let view = DrawView { page: 1, left: 95.0, top: 95.0, scale: 20.0, width: 200, height: 200 };
    let img = pages.draw(&view).unwrap();
    let at = |x: u32, y: u32| img.pixels[((y * 200 + x) * 4) as usize..][..4].to_vec();
    assert_eq!(at(150, 150), [0, 0, 0, 255], "inside the square");
    assert_eq!(at(20, 20), [255, 255, 255, 255], "beside it");
    // Past the page's edge the picture is transparent.
    let edge = DrawView { page: 1, left: -10.0, top: -10.0, scale: 1.0, width: 40, height: 40 };
    let img = pages.draw(&edge).unwrap();
    assert_eq!(img.pixels[3], 0);
    assert_eq!(img.pixels[((20 * 40 + 20) * 4 + 3) as usize], 255);
    // Views that can't be drawn are refused, not attempted.
    for bad in [
        DrawView { width: 0, ..view },
        DrawView { width: 100_000, ..view },
        DrawView { scale: f64::NAN, ..view },
        DrawView { page: 7, ..view },
    ] {
        assert!(pages.draw(&bad).is_err(), "{bad:?}");
    }
    assert!(pages.draw(&view).is_ok(), "the worker survives refusals");
}

#[test]
fn a_pdf_with_a_user_password_is_refused() {
    let d = dir("pdf-locked");
    // The standard security handler with a /U entry no empty password
    // matches: the reader needs a password to open it.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_vec(),
        format!("<< /Filter /Standard /V 1 /R 2 /Length 40 /P -4 /O <{}> /U <{}> >>", "11".repeat(32), "5a".repeat(32))
            .into_bytes(),
    ];
    let bytes = pdf_from(
        &objects,
        "/Encrypt 4 0 R /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>]",
    );
    let path = write(&d, "locked.pdf", &bytes);
    let e = load_path(DocumentFormat::Pdf, &path, &thumb(64)).err().expect("refused").to_string();
    assert!(e.contains("password"), "{e}");
    let e = PdfPages::open(DocumentFormat::Pdf, &path).err().expect("refused").to_string();
    assert!(e.contains("password"), "{e}");
}

/// A small green JPEG, 64 x 32, as Adobe writes thumbnails into XMP (with
/// line-break entities in the base64).
const GREEN_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAAgAEADASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAb/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFgEBAQEAAAAAAAAAAAAAAAAAAAgJ/8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAwDAQACEQMRAD8AuwEPs4gAAAAAAAAAAAAAH//Z";

fn xmp_packet() -> String {
    let (a, b) = GREEN_JPEG.split_at(60);
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><xmp:Thumbnails><rdf:Alt><rdf:li><xmpGImg:width>64</xmpGImg:width><xmpGImg:image>{a}&#xA;{b}</xmpGImg:image></rdf:li></rdf:Alt></xmp:Thumbnails></x:xmpmeta>"
    )
}

#[test]
fn base64_as_xmp_writes_it() {
    assert_eq!(stored::base64(b"aGVs&#xA;bG8=").unwrap(), b"hello");
    assert_eq!(stored::base64(b" aGk= ").unwrap(), b"hi");
    assert!(stored::base64(b"a*b").is_none());
    assert!(stored::base64(b"").is_none());
    let jpeg = stored::xmp_image(xmp_packet().as_bytes()).unwrap();
    let (w, h, px) = stored::decode_jpeg(&jpeg).unwrap();
    assert_eq!((w, h), (64, 32));
    assert!(px[1] > 240 && px[0] < 20, "{:?}", &px[..4]);
    assert!(stored::xmp_image(b"<xmpGImg:image>AAAA").is_none(), "unterminated");
    assert!(stored::decode_jpeg(b"\xFF\xD8 not a jpeg").is_none());
}

#[test]
fn saved_pictures_are_read_without_drawing() {
    let d = dir("stored");
    let saved = |format, path: &Path| match saved_picture(format, path).unwrap() {
        Some(Picture::Image(t)) => Some(t),
        Some(Picture::Metafile { .. }) => panic!("a metafile"),
        None => None,
    };
    // 1. An Illustrator file with an XMP thumbnail: that is its saved
    //    picture; loading it, for the viewer's pages, `slerp info` or a
    //    thumbnail of a file without one, draws the page.
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        stream("", RED_PAGE.as_bytes()),
        stream("/Type /Metadata /Subtype /XML", xmp_packet().as_bytes()),
    ];
    let ai = write(&d, "logo.ai", &pdf_from(&objects, ""));
    let t = saved(DocumentFormat::Illustrator, &ai).expect("the XMP thumbnail");
    assert_eq!((t.width(), t.height()), (64, 32));
    assert!(t.source.header[0].1.contains("XMP"), "{:?}", t.source.header);
    for limits in [thumb(64), Limits::viewer()] {
        let drawn = image(load_path(DocumentFormat::Illustrator, &ai, &limits));
        assert_eq!(pixel(&drawn, 10, 10), [255, 0, 0, 255]);
    }
    let mut pages = PdfPages::open(DocumentFormat::Illustrator, &ai).unwrap();
    let full = pages.draw(&DrawView::fit(0, pages.sizes[0], 40)).unwrap();
    assert_eq!(&full.pixels[..4], &[255, 0, 0, 255]);
    // 2. A page's own /Thumb: a 64 x 64 blue picture.
    objects.truncate(4);
    objects[2] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Thumb 5 0 R >>".to_vec();
    objects.push(stream("/Width 64 /Height 64 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0, 0, 255].repeat(4096)));
    let pdf = write(&d, "thumb.pdf", &pdf_from(&objects, ""));
    let t = saved(DocumentFormat::Pdf, &pdf).expect("the page thumbnail");
    assert_eq!((t.width(), t.height()), (64, 64));
    assert_eq!(pixel(&t, 1, 1), [0, 0, 255, 255]);
    assert!(t.source.header[0].1.contains("page thumbnail"));
    let t = image(load_path(DocumentFormat::Pdf, &pdf, &thumb(64)));
    assert_eq!(pixel(&t, 10, 10), [255, 0, 0, 255], "loading draws");
    // 3. A /Thumb too small to stand for the page (ImageMagick writes
    //    1 x 1 ones for huge pictures), or of a kind not taken (indexed),
    //    isn't one; nor is anything in a file without either.
    objects[4] = stream("/Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[0, 0, 255]);
    let pdf = write(&d, "tiny.pdf", &pdf_from(&objects, ""));
    assert!(saved(DocumentFormat::Pdf, &pdf).is_none());
    objects[4] = stream("/Width 2 /Height 2 /ColorSpace [/Indexed /DeviceRGB 0 <0000ff>] /BitsPerComponent 8", &[0; 4]);
    let pdf = write(&d, "indexed.pdf", &pdf_from(&objects, ""));
    assert!(saved(DocumentFormat::Pdf, &pdf).is_none());
    let plain = write(&d, "plain.pdf", &pdf_pages(&[RED_PAGE], (612, 792), ""));
    assert!(saved(DocumentFormat::Pdf, &plain).is_none());
    // 4. An EPS without a binary or EPSI preview has its XMP thumbnail.
    let eps = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 40 40\n%%EndComments\n%begin_xml_packet\n%{}\n0 0 40 40 rectfill\n",
        xmp_packet()
    );
    let path = write(&d, "xmp.eps", eps.as_bytes());
    let t = saved(DocumentFormat::Eps, &path).expect("the XMP thumbnail");
    assert_eq!((t.width(), t.height()), (64, 32));
    assert!(t.source.header[0].1.contains("XMP"));
    assert!(t.source.header.iter().any(|(k, _)| k == "Bounding box"));
    assert!(saved(DocumentFormat::PostScript, &path).is_none(), "PostScript pages are drawn");
}

//endregion

/*┌─────────────────╮
| Text            |
└─────────────────╯*/
//region Text

/// A page dictionary entry giving the page Helvetica as `/F1`, one of the
/// standard fonts every reader has, so the tests' text has known widths
/// (H is 722 thousandths of the font size wide).
const HELVETICA: &str = "/Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>";

fn near(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5)
}

/// Opens a one-page PDF drawing `contents` with Helvetica, and reads its
/// text.
fn page_text(name: &str, contents: &str, page_extra: &str) -> PageText {
    let d = dir(name);
    let path = write(&d, "t.pdf", &pdf_pages(&[contents], (612, 792), &format!("{HELVETICA} {page_extra}")));
    PdfPages::open(DocumentFormat::Pdf, &path).unwrap().text(0).unwrap()
}

#[test]
fn a_pages_text_comes_with_each_glyphs_place() {
    // Two lines of 20-point text; the second leaves out its space and moves
    // the next word along instead, as many PDF writers do.
    let t =
        page_text("text-lines", "BT /F1 20 Tf 72 700 Td (Hello world) Tj 0 -30 Td [(Second) -400 (line)] TJ ET", "");
    assert_eq!(t.len(), 21);
    assert_eq!(t.all_text(), "Hello world\nSecond line");
    // The H: from x 72 to 72 + 0.722 × 20, its em box reaching 0.8 of the
    // size above the baseline (792 - 700 from the top) and 0.2 below.
    assert!(near(t.selection_rects(0..1)[0], [72.0, 76.0, 86.44, 96.0]), "{:?}", t.selection_rects(0..1));
    assert_eq!(t.glyph_at([75.0, 85.0]), Some(0));
    assert!(t.on_text([75.0, 85.0]));
    assert!(!t.on_text([75.0, 200.0]), "the paper below");
    // A double-click on "world" takes the word; a drag from before it into
    // the second line takes what lies between.
    let world = t.word_at([150.0, 85.0]).unwrap();
    assert_eq!(t.copy_text(world.clone()), "world");
    assert_eq!(t.caret_at([500.0, 85.0]), Some(11), "past the end of the first line");
    assert_eq!(t.copy_text(world.start..14), "world\nSec");
    // A selection over two lines highlights a bar on each.
    assert_eq!(t.selection_rects(world.start..14).len(), 2);
}

#[test]
fn text_drawn_twice_or_invisibly_is_read_once() {
    // Filled and stroked (2 Tr) comes from the interpreter twice; invisible
    // text (3 Tr) is how OCR puts words over a scanned page.
    let t = page_text("text-modes", "BT /F1 20 Tf 2 Tr 72 700 Td (AB) Tj 3 Tr 0 -30 Td (CD) Tj ET", "");
    assert_eq!(t.all_text(), "AB\nCD");
    assert_eq!(t.len(), 4);
}

#[test]
fn text_is_placed_on_the_page_as_shown() {
    // A page turned a quarter: 792 wide and 612 tall as shown, its level
    // text now running down the screen, every glyph inside the page, and
    // text set past the page's edge left out.
    let t = page_text("text-turned", "BT /F1 20 Tf 72 700 Td (Turned) Tj 1000 0 Td (Gone) Tj ET", "/Rotate 90");
    assert_eq!(t.all_text(), "Turned");
    let bar = t.selection_rects(0..t.len());
    assert_eq!(bar.len(), 1);
    assert!(bar[0][3] - bar[0][1] > bar[0][2] - bar[0][0], "a bar down the page: {bar:?}");
    for r in t.selection_rects(0..t.len()) {
        assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[2] <= 792.0 && r[3] <= 612.0, "{r:?}");
    }
    // A page without text has none, and asking for a page that doesn't
    // exist is refused without stopping the worker.
    let d = dir("text-none");
    let path = write(&d, "t.pdf", &pdf_pages(&[RED_PAGE], (612, 792), ""));
    let mut pages = PdfPages::open(DocumentFormat::Pdf, &path).unwrap();
    assert!(pages.text(0).unwrap().is_empty());
    assert!(pages.text(5).is_err());
    assert!(pages.text(0).is_ok());
}

#[test]
fn lines_words_and_copied_text() {
    // Glyphs ten points tall and six wide: "ab cd" with a space glyph,
    // then far to the right on the same line a second column's "ef", then
    // a line below with trailing spaces and a ligature.
    let g = |x: f32, y: f32, s: &'static str| ([x, y, x + 6.0, y + 10.0], Direction::Right, s);
    let glyphs = vec![
        g(0.0, 0.0, "a"),
        g(6.0, 0.0, "b"),
        g(12.0, 0.0, " "),
        g(18.0, 0.0, "c"),
        g(24.0, 0.0, "d"),
        g(200.0, 0.0, "e"),
        g(206.0, 0.0, "f"),
        g(0.0, 14.0, "\u{FB01}"),
        g(6.0, 14.0, "x"),
        g(12.0, 14.0, " "),
        g(18.0, 14.0, "\u{0007}"),
    ];
    let t = PageText::new(glyphs);
    // The bell character is no text, so that glyph is left out.
    assert_eq!(t.len(), 10);
    assert_eq!(t.all_text(), "ab cd ef\nfix");
    // The paper between the columns is not text; between words is.
    assert!(t.on_text([15.0, 5.0]));
    assert!(!t.on_text([100.0, 5.0]));
    assert_eq!(t.word_at([20.0, 5.0]), Some(3..5));
    assert_eq!(t.word_at([13.0, 5.0]), Some(2..3), "a space is a word of its own");
    assert_eq!(t.word_at([100.0, 5.0]), None);
    // The columns highlight apart; the caret goes to the nearer one.
    assert_eq!(t.selection_rects(0..7), vec![[0.0, 0.0, 30.0, 10.0], [200.0, 0.0, 212.0, 10.0]]);
    assert_eq!(t.caret_at([150.0, 5.0]), Some(5));
    assert_eq!(t.caret_at([2.0, 5.0]), Some(0));
    assert_eq!(t.caret_at([4.0, 5.0]), Some(1));
    assert_eq!(t.caret_at([4.0, 100.0]), Some(8), "below every line, the nearest");
    assert_eq!(PageText::default().caret_at([0.0, 0.0]), None);
    // Spaces drawn at a line's ends neither widen its bar nor are copied.
    let centred = PageText::new([g(0.0, 0.0, " "), g(6.0, 0.0, " "), g(12.0, 0.0, "a"), g(18.0, 0.0, " ")]);
    assert_eq!(centred.all_text(), "a");
    assert_eq!(centred.selection_rects(0..4), vec![[12.0, 0.0, 18.0, 10.0]]);
    assert!(!centred.on_text([3.0, 5.0]), "the spaces before it are paper");
    assert_eq!(centred.copy_text(0..2), "");
    // Words drawn without space glyphs get one where the gap is wide.
    let apart = PageText::new([g(0.0, 0.0, "a"), g(8.0, 0.0, "b"), g(14.5, 0.0, "c")]);
    assert_eq!(apart.all_text(), "a bc");
    // A small fraction, numerator raised and denominator lowered, stays
    // on its line, as sub- and superscripts do; the next line doesn't.
    let small = |x: f32, y: f32, s: &'static str| ([x, y, x + 4.0, y + 7.0], Direction::Right, s);
    let fraction = PageText::new([
        g(0.0, 0.0, "2"),
        small(8.0, -3.0, "n"),
        small(8.0, 5.0, "q"),
        g(12.5, 0.0, "x"),
        g(0.0, 12.0, "y"),
    ]);
    assert_eq!(fraction.all_text(), "2 nqx\ny");
    // Text running up the page, as on a chart's side: "up" from the
    // bottom, then the next line to its right.
    let up = |x: f32, y: f32, s: &'static str| ([x, y - 6.0, x + 10.0, y], Direction::Up, s);
    let side = PageText::new([up(0.0, 100.0, "u"), up(0.0, 94.0, "p"), up(14.0, 100.0, "o"), up(14.0, 92.0, "n")]);
    assert_eq!(side.all_text(), "up\no n");
    assert_eq!(side.selection_rects(0..2), vec![[0.0, 88.0, 10.0, 100.0]]);
    assert_eq!(side.caret_at([5.0, 0.0]), Some(2), "above the top of the first line, its end");
    assert_eq!(Direction::of(0.0, -3.0), Direction::Up);
    assert_eq!(Direction::of(-2.0, 1.0), Direction::Left);
}

#[test]
fn a_page_holds_a_bounded_number_of_glyphs() {
    let mut b = crate::page_text::Builder::default();
    let right = Direction::Right;
    for i in 0..MAX_GLYPHS {
        assert!(b.push([i as f32, 0.0, i as f32 + 1.0, 1.0], right, "x"));
    }
    assert!(!b.push([0.0, 0.0, 1.0, 1.0], right, "x"), "full");
    // Boxes that aren't boxes are left out; long mappings are cut.
    let mut small = crate::page_text::Builder::default();
    small.push([f32::NAN, 0.0, 1.0, 1.0], right, "n");
    small.push([0.0, 1.0, 1.0, 1.0], right, "flat");
    small.push([0.0, 0.0, 1.0, 1.0], right, &"é".repeat(100));
    let answer = small.encode();
    let t = PageText::decode(&answer).unwrap();
    assert_eq!(t.all_text(), "é".repeat(16));
    let full = PageText::decode(&b.encode()).unwrap();
    assert!(full.truncated());
    assert_eq!(full.len(), MAX_GLYPHS);
    // Damaged answers are refused, never trusted.
    for cut in 0..answer.len() {
        assert!(PageText::decode(&answer[..cut]).is_none(), "cut at {cut}");
    }
    let mut bad = answer.clone();
    bad[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(PageText::decode(&bad).is_none());
    let mut split = answer.clone();
    split[5 + 17] = 3;
    assert!(PageText::decode(&split).is_none(), "a length that splits a character");
    let mut turned = answer.clone();
    turned[5 + 16] = 9;
    assert!(PageText::decode(&turned).is_none(), "no such direction");
}

#[test]
fn text_of_sample_documents() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/documents");
    let (pdf, ai) = (DocumentFormat::Pdf, DocumentFormat::Illustrator);
    for (format, file, page, wanted) in [
        (pdf, "pdf/samples/pdf-rs/xelatex.pdf", 0, "Problem Set 2, Part 1"),
        (pdf, "pdf/samples/pdfbox-rotation/rot90.pdf", 0, "Page rotation: 90"),
        (pdf, "pdf/samples/pdfbox-rotation/rot270.pdf", 0, "Page rotation: 270"),
        // The fi ligature comes out as its letters.
        (ai, "ai/coala_artwork/coala_stand_popup_display.ai", 0, "linting and fixing code"),
        // Its Japanese lines use a CID font without a ToUnicode map, which
        // hayro needs to know the characters, so only the Latin title reads.
        (ai, "ai/VLDB2020_artworks/vldb2020-flier.ai", 0, "VLDB2020Tokyo"),
    ] {
        let path = root.join(file);
        if !path.exists() {
            eprintln!("skipped: {} is missing", path.display());
            continue;
        }
        let mut pages = PdfPages::open(format, &path).unwrap();
        let t = pages.text(page).unwrap();
        assert!(t.all_text().contains(wanted), "{file}: {:?}", t.all_text());
        let (w, h) = pages.sizes[page];
        for r in t.selection_rects(0..t.len()) {
            assert!(r[0] >= -1.0 && r[1] >= -1.0 && r[2] <= w + 1.0 && r[3] <= h + 1.0, "{file}: {r:?}");
        }
    }
}

//endregion

/*┌─────────────────╮
| Damaged PDFs    |
└─────────────────╯*/
//region Damaged PDFs

/// Loads a damaged file for a thumbnail: any answer is fine, as long as it
/// comes within the thumbnail's time and the test process survives.
fn survives(path: &Path) -> Result<Picture, LoadError> {
    let started = Instant::now();
    let r = load_path(DocumentFormat::Pdf, path, &thumb(96));
    assert!(started.elapsed().as_secs_f32() < 4.0, "{} took {:?}", path.display(), started.elapsed());
    r
}

#[test]
fn truncated_and_mutated_pdfs_never_take_the_caller_down() {
    let d = dir("pdf-damaged");
    let good = pdf_pages(&[RED_PAGE, "BT /F1 24 Tf 72 700 Td (Hello) Tj ET", BLUE_PAGE], (612, 792), "");
    for cut in (0..good.len()).step_by(good.len() / 24) {
        let _ = survives(&write(&d, &format!("cut{cut}.pdf"), &good[..cut]));
    }
    // A cheap reproducible shuffle of bytes.
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..40 {
        let mut b = good.clone();
        for _ in 0..1 + next() % 12 {
            let at = (next() % b.len() as u64) as usize;
            b[at] = next() as u8;
        }
        let _ = survives(&write(&d, &format!("mut{i}.pdf"), &b));
    }
}

#[test]
fn hostile_pdfs_end_within_their_limits() {
    let d = dir("pdf-hostile");
    let page = |contents: &str| pdf_pages(&[contents], (612, 792), "");
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // 1. Objects that refer to each other in a loop, and a page tree that
    //    contains itself.
    cases.push((
        "ref-loop",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R /Contents 5 0 R >>".to_vec(),
                b"5 0 R".to_vec(),
                b"4 0 R".to_vec(),
            ],
            "",
        ),
    ));
    cases.push((
        "tree-loop",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>".to_vec(),
                b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Parent 2 0 R /Count 1000000000 >>".to_vec(),
            ],
            "",
        ),
    ));
    // 2. A cross-reference table claiming two billion objects, and a
    //    /Prev chain pointing at itself.
    let mut bomb = page(RED_PAGE);
    let at = bomb.windows(5).rposition(|w| w == b"/Size").unwrap();
    bomb.splice(at..at + 5, b"/Size 2147483647 /Prev 9".iter().copied());
    cases.push(("xref-bomb", bomb));
    // 3. A picture claiming 100000 x 100000 pixels with a few bytes of data.
    cases.push((
        "huge-image",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /I 5 0 R >> >> >>".to_vec(),
                stream("", b"q 612 0 0 792 0 0 cm /I Do Q"),
                stream("/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceRGB /BitsPerComponent 8", b"\xff\x00\xff"),
            ],
            "",
        ),
    ));
    // 4. A stream that inflates to 300 MB of zeros.
    let zeros = vec![0u8; 300 << 20];
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&zeros, 6);
    drop(zeros);
    cases.push((
        "inflate-bomb",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
                stream("/Filter /FlateDecode", &packed),
            ],
            "",
        ),
    ));
    // 5. Deep nesting: saved graphics states, arrays in the content, and a
    //    form that draws itself.
    cases.push(("deep-q", page(&format!("{}0 0 1 rg 0 0 10 10 re f", "q ".repeat(200_000)))));
    cases.push(("deep-array", page(&format!("{}1{} pop", "[".repeat(100_000), "]".repeat(100_000)))));
    cases.push((
        "self-form",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /F 5 0 R >> >> >>".to_vec(),
                stream("", b"/F Do"),
                stream("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /F 5 0 R >> >>", b"/F Do /F Do"),
            ],
            "",
        ),
    ));
    // 6. Millions of drawing operations.
    cases.push(("many-ops", page(&"0 0 m 600 780 l S ".repeat(400_000))));
    // 7. A page tree of a million pages.
    let kids = vec!["3 0 R"; 1_000_000].join(" ");
    cases.push((
        "million-pages",
        pdf_from(
            &[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                format!("<< /Type /Pages /Kids [{kids}] /Count 1000000 >>").into_bytes(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec(),
            ],
            "",
        ),
    ));
    for (name, bytes) in &cases {
        let path = write(&d, &format!("{name}.pdf"), bytes);
        let started = Instant::now();
        let r = survives(&path);
        eprintln!(
            "{name}: {:?} after {:?}",
            r.as_ref().map(|_| "a picture").map_err(|e| e.to_string()),
            started.elapsed()
        );
    }
}

//endregion

/*┌─────────────────╮
| EPS             |
└─────────────────╯*/
//region EPS

/// A small EPS drawing a black square in the top half of a 40-point box.
const EPS: &str = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 40 40\n%%HiResBoundingBox: 0 0 40 40\n%%Title: (Square \\(test\\))\n%%Creator: slerp tests\n%%Pages: 1\n%%EndComments\n0 20 40 20 rectfill\nshowpage\n%%EOF\n";

/// An uncompressed 2 x 2 RGB TIFF: red, green, blue, white.
fn tiny_tiff() -> Vec<u8> {
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    let entries: [(u16, u16, u32, u32); 8] = [
        (256, 3, 1, 2),
        (257, 3, 1, 2),
        (258, 3, 1, 8),
        (259, 3, 1, 1),
        (262, 3, 1, 2),
        (273, 4, 1, 8 + 2 + 8 * 12 + 4),
        (277, 3, 1, 3),
        (279, 4, 1, 12),
    ];
    t.extend((entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        t.extend(tag.to_le_bytes());
        t.extend(kind.to_le_bytes());
        t.extend(count.to_le_bytes());
        t.extend(value.to_le_bytes());
    }
    t.extend(0u32.to_le_bytes());
    t.extend([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    t
}

/// A DOS EPS binary file around `ps` with a WMF and a TIFF preview.
fn dos_eps(ps: &[u8], wmf: &[u8], tiff: &[u8]) -> Vec<u8> {
    let ps_at = 30u32;
    let wmf_at = ps_at + ps.len() as u32;
    let tiff_at = wmf_at + wmf.len() as u32;
    let mut out = vec![0xC5, 0xD0, 0xD3, 0xC6];
    for (at, len) in [(ps_at, ps.len()), (wmf_at, wmf.len()), (tiff_at, tiff.len())] {
        let at = if len == 0 { 0 } else { at };
        out.extend(at.to_le_bytes());
        out.extend((len as u32).to_le_bytes());
    }
    out.extend([0xFF, 0xFF]);
    out.extend(ps);
    out.extend(wmf);
    out.extend(tiff);
    out
}

#[test]
fn the_dos_header_is_checked_against_the_file() {
    let b = dos_eps(EPS.as_bytes(), b"WMF!", &tiny_tiff());
    let h = BinaryHeader::parse(&b, b.len() as u64).unwrap().unwrap();
    assert_eq!(h.ps, (30, EPS.len() as u64));
    assert_eq!(h.wmf, Some((30 + EPS.len() as u64, 4)));
    assert_eq!(h.tiff.unwrap().1, tiny_tiff().len() as u64);
    assert_eq!(BinaryHeader::parse(b"%!PS", 4).unwrap(), None);
    // Sections past the end: the PostScript is fatal, a preview dropped.
    assert!(BinaryHeader::parse(&b, 100).is_err());
    let short = BinaryHeader::parse(&b, b.len() as u64 - 1).unwrap().unwrap();
    assert_eq!(short.tiff, None);
    assert!(BinaryHeader::parse(&b[..10], 10).is_err());
}

#[test]
fn structuring_comments_are_read() {
    let tail = b"%%Trailer\n%%BoundingBox: 1 2 30 40\n%%Pages: 7\n%%EOF\n";
    let head = b"%!PS-Adobe-3.0\r\n%%BoundingBox: (atend)\r\n%%Pages: (atend)\r\n%%Title: (Caf\\351)\r\n%%EndComments\r\n%%BeginProlog\r\n";
    let d = Dsc::parse(head, 0, tail);
    assert_eq!(d.bbox, Some([1.0, 2.0, 30.0, 40.0]));
    assert_eq!(d.pages, Some(7));
    assert_eq!(d.title.as_deref(), Some("Café"));
    assert_eq!(d.version, "%!PS-Adobe-3.0");
    let d = Dsc::parse(EPS.as_bytes(), 0, b"");
    assert_eq!(d.best_bbox(), Some([0.0, 0.0, 40.0, 40.0]));
    assert_eq!(d.title.as_deref(), Some("Square (test)"));
    // An EPSI preview's header, and where its data starts.
    let epsi = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 8 2\n%%EndComments\n%%BeginPreview: 8 2 1 2\n% FF\n% 00\n%%EndPreview\n";
    let d = Dsc::parse(epsi, 100, b"");
    let p = d.preview.unwrap();
    assert_eq!((p.width, p.height, p.depth, p.lines), (8, 2, 1, 2));
    assert_eq!(&epsi[(p.offset - 100) as usize..][..4], b"% FF");
    // Past the first line of code there is no preview to find.
    let late = b"%!PS-Adobe-3.0 EPSF-3.0\n%%EndComments\nnewpath\n%%BeginPreview: 8 2 1 2\n";
    assert!(Dsc::parse(late, 0, b"").preview.is_none());
}

#[test]
fn epsi_bitmaps_decode_with_black_as_the_largest_value() {
    let h = |w, h, depth| EpsiHeader { width: w, height: h, depth, lines: h, offset: 0 };
    // 1 bit: a black row over a white one.
    let (w, ht, px) = decode_epsi(b"% FF\n% 00\n%%EndPreview\n", &h(8, 2, 1), false).unwrap();
    assert_eq!((w, ht), (8, 2));
    assert_eq!(&px[..4], &[0, 0, 0, 255]);
    assert_eq!(&px[8 * 4..8 * 4 + 4], &[255, 255, 255, 255]);
    // ImageMagick's sense is the other way round.
    let (_, _, px) = decode_epsi(b"% FF\n% 00\n", &h(8, 2, 1), true).unwrap();
    assert_eq!(px[0], 255);
    // 2, 4 and 8 bits, mid grey.
    for (depth, data) in [(2u32, &b"% 55"[..]), (4, b"% 77"), (8, b"% 80")] {
        let (_, _, px) = decode_epsi(data, &h(1, 1, depth), false).unwrap();
        assert!((80..=180).contains(&px[0]), "{depth}: {}", px[0]);
    }
    // Cut short: the rest stays white.  Unusable headers are refused.
    let (_, _, px) = decode_epsi(b"% FF\n", &h(8, 4, 1), false).unwrap();
    assert_eq!(px.len(), 8 * 4 * 4);
    assert_eq!(px[px.len() - 4], 255);
    assert!(decode_epsi(b"", &h(0, 1, 1), false).is_err());
    assert!(decode_epsi(b"", &h(100_000, 1, 1), false).is_err());
    assert!(decode_epsi(b"", &h(1, 1, 3), false).is_err());
}

#[test]
fn an_eps_shows_its_stored_preview() {
    let d = dir("eps-preview");
    let path = write(&d, "tiff.eps", &dos_eps(EPS.as_bytes(), b"", &tiny_tiff()));
    let t = image(load_path(DocumentFormat::Eps, &path, &thumb(64)));
    assert_eq!((t.width(), t.height()), (2, 2));
    assert_eq!(pixel(&t, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&t, 1, 1), [255, 255, 255, 255]);
    let shown = t.source.header.iter().find(|(k, _)| k == "Shown").unwrap();
    assert!(shown.1.contains("preview picture stored"), "{shown:?}");
    assert!(t.source.header.iter().any(|(k, v)| k == "Bounding box" && v.starts_with("0 0 40 40")));
    // A WMF preview goes back to the caller to draw.
    let path = write(&d, "wmf.eps", &dos_eps(EPS.as_bytes(), b"\xD7\xCD\xC6\x9Afake", b""));
    match load_path(DocumentFormat::Eps, &path, &thumb(64)) {
        Ok(Picture::Metafile { bytes, facts }) => {
            assert!(bytes.starts_with(b"\xD7\xCD\xC6\x9A"));
            assert!(facts.iter().any(|(k, v)| k == "Shown" && v.contains("Windows Metafile")));
        }
        _ => panic!("expected the metafile"),
    }
    // A broken TIFF preview isn't the end: Ghostscript or a message.
    let path = write(&d, "bad.eps", &dos_eps(EPS.as_bytes(), b"", b"II*\0garbage"));
    let r = load_path(DocumentFormat::Eps, &path, &thumb(64));
    assert!(r.is_ok() == ghostscript().is_some(), "{:?}", r.err());
}

#[test]
fn without_a_preview_ghostscript_draws_or_a_message_says_why() {
    let d = dir("eps-plain");
    let path = write(&d, "plain.eps", EPS.as_bytes());
    match ghostscript() {
        None => {
            let e = load_path(DocumentFormat::Eps, &path, &thumb(64)).err().unwrap().to_string();
            assert!(e.contains("needs Ghostscript"), "{e}");
        }
        Some(_) => {
            let t = image(load_path(DocumentFormat::Eps, &path, &thumb(64)));
            assert_eq!((t.width(), t.height()), (64, 64));
            // The square fills the top half: black there, white below.
            assert!(pixel(&t, 32, 10)[0] < 40, "{:?}", pixel(&t, 32, 10));
            assert!(pixel(&t, 32, 54)[0] > 220, "{:?}", pixel(&t, 32, 54));
            assert!(t.source.header[0].1.starts_with("Drawn by Ghostscript"));
        }
    }
}

#[test]
fn an_eps_with_a_preview_also_offers_ghostscripts_drawing() {
    if ghostscript().is_none() {
        return;
    }
    let d = dir("eps-both");
    let path = write(&d, "both.eps", &dos_eps(EPS.as_bytes(), b"", &tiny_tiff()));
    assert_eq!(page_names(DocumentFormat::Eps, &path), [STORED, DRAWN]);
    let t = image(load_page(DocumentFormat::Eps, &path, &thumb(40), DRAWN));
    assert_eq!((t.width(), t.height()), (40, 40));
    // Without a preview there is nothing to choose between.
    let plain = write(&d, "plain.eps", EPS.as_bytes());
    assert!(page_names(DocumentFormat::Eps, &plain).is_empty());
}

#[test]
fn postscript_pages_through_ghostscript() {
    if ghostscript().is_none() {
        return;
    }
    let d = dir("ps-pages");
    let ps = "%!PS-Adobe-3.0\n%%BoundingBox: 0 0 100 100\n%%Pages: 2\n%%EndComments\n%%Page: 1 1\n<< /PageSize [100 100] >> setpagedevice\n1 0 0 setrgbcolor 0 0 100 100 rectfill showpage\n%%Page: 2 2\n0 0 1 setrgbcolor 0 0 100 100 rectfill showpage\n%%EOF\n";
    let path = write(&d, "two.ps", ps.as_bytes());
    assert_eq!(page_names(DocumentFormat::PostScript, &path), ["Page 1", "Page 2"]);
    let t = image(load_page(DocumentFormat::PostScript, &path, &thumb(32), "Page 2"));
    let p = pixel(&t, t.width() / 2, t.height() / 2);
    assert!(p[2] > 200 && p[0] < 50, "{p:?}");
}

#[test]
fn ghostscript_is_kept_on_a_leash() {
    if ghostscript().is_none() {
        return;
    }
    let d = dir("eps-leash");
    // 1. A drawing that never ends is stopped at the thumbnail's limit.
    let forever = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n%%EndComments\n{ } loop\n";
    let path = write(&d, "forever.eps", forever.as_bytes());
    let started = Instant::now();
    let e = load_path(DocumentFormat::Eps, &path, &thumb(32)).err().unwrap().to_string();
    assert!(e.contains("longer than"), "{e}");
    assert!(started.elapsed().as_secs_f32() < 4.0);
    // 2. Safe mode: the PostScript can't write files.
    let target = d.join("written.txt");
    let writer = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n%%EndComments\n({}) (w) file (owned) writestring\n0 0 10 10 rectfill showpage\n",
        target.display()
    );
    let path = write(&d, "writer.eps", writer.as_bytes());
    let _ = load_path(DocumentFormat::Eps, &path, &thumb(32));
    assert!(!target.exists(), "Ghostscript wrote a file");
    // 3. A name that looks like an option or a pipe is still a file name.
    let odd = write(&d, "-dNOSAFER %pipe%echo.eps", EPS.as_bytes());
    assert!(load_path(DocumentFormat::Eps, &odd, &thumb(32)).is_ok());
}

//endregion
