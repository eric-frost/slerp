use super::*;

fn tmp(name: &str, body: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-svg-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

const SQUARES: &str = r##"<?xml version="1.0"?>
<!-- two squares -->
<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20">
  <title>Squares</title>
  <rect x="0" y="0" width="20" height="20" fill="#ff0000"/>
  <rect x="20" y="0" width="20" height="20" fill="#0000ff" fill-opacity="0.5"/>
</svg>"##;

#[test]
fn recognises_svg_and_svgz() {
    assert!(is_svg(Path::new("a.svg"), SQUARES.as_bytes()));
    assert!(is_svg(Path::new("x"), b"\xef\xbb\xbf<!DOCTYPE svg [<!ENTITY a \"b\">]>\n<svg:svg xmlns:svg='x'/>"));
    assert!(is_svg(Path::new("a.svgz"), &[0x1f, 0x8b, 8, 0]));
    assert!(!is_svg(Path::new("a.tar.gz"), &[0x1f, 0x8b, 8, 0]));
    assert!(!is_svg(Path::new("a.svg"), b"<html><svg/></html>"));
    assert!(!is_svg(Path::new("a.xml"), b"<?xml version='1.0'?><rss/>"));
}

#[test]
fn draws_at_any_size_and_region() {
    let p = tmp("squares.svg", SQUARES.as_bytes());
    let thumb = picture(&p, &Limits { max_texture_size: Some(20), ..Limits::thumbnail() }).unwrap();
    assert_eq!((thumb.width(), thumb.height()), (20, 10));
    assert!(thumb.source.header.iter().any(|(k, v)| k == "Title" && v == "Squares"));
    let big = picture(&p, &Limits::viewer()).unwrap();
    assert_eq!((big.width(), big.height()), (1024, 512));
    let d = Drawing::open(&p, &Limits::viewer()).unwrap();
    assert_eq!((d.width, d.height), (40.0, 20.0));
    // The right half only, at 4 pixels per unit: blue at half alpha.
    let img = d.draw([20.0, 0.0, 40.0, 20.0], 80, 80).unwrap();
    assert_eq!(&img.pixels[0..4], &[0, 0, 255, 128]);
    let img = d.draw([0.0, 0.0, 10.0, 10.0], 8, 8).unwrap();
    assert_eq!(&img.pixels[0..4], &[255, 0, 0, 255]);
    assert!(d.draw([0.0, 0.0, 1.0, 1.0], 100_000, 100_000).is_err());
}

#[test]
fn svgz_unpacks() {
    // A stored deflate block inside a gzip wrapper.
    let body = SQUARES.as_bytes();
    let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    gz.push(1);
    gz.extend_from_slice(&(body.len() as u16).to_le_bytes());
    gz.extend_from_slice(&(!(body.len() as u16)).to_le_bytes());
    gz.extend_from_slice(body);
    gz.extend_from_slice(&[0; 8]);
    let p = tmp("squares.svgz", &gz);
    assert_eq!(read(&p).unwrap(), body);
    assert!(picture(&p, &Limits::thumbnail()).is_ok());
}

#[test]
fn hostile_drawings_fail_cleanly() {
    let laughs = format!(
        "<?xml version='1.0'?><!DOCTYPE svg [{}]><svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><text>&l9;</text></svg>",
        (1..10).map(|i| format!("<!ENTITY l{i} '{}'>", format!("&l{};", i - 1).repeat(10))).collect::<String>()
            + "<!ENTITY l0 'lol'>"
    );
    let deep = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'>{}<rect width='1' height='1'/>{}</svg>",
        "<g>".repeat(5000),
        "</g>".repeat(5000)
    );
    let huge = "<svg xmlns='http://www.w3.org/2000/svg' width='1e30' height='1e30'><rect width='1' height='1'/></svg>";
    let loops = "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' width='10' height='10'><use id='a' xlink:href='#b'/><use id='b' xlink:href='#a'/></svg>";
    let outside = "<svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' width='10' height='10'><image width='10' height='10' xlink:href='/etc/passwd'/><image xlink:href='http://example.com/a.png'/></svg>";
    for (name, body) in
        [("laughs", laughs.as_str()), ("deep", &deep), ("huge", huge), ("loops", loops), ("outside", outside)]
    {
        let p = tmp(&format!("{name}.svg"), body.as_bytes());
        let start = std::time::Instant::now();
        let _ = picture(&p, &Limits::thumbnail());
        assert!(start.elapsed() < Duration::from_secs(25), "{name}");
    }
    assert!(
        !local_href("/etc/passwd") && !local_href("C:\\x.png") && !local_href("file:///x") && local_href("img/a.png")
    );
}

/// Every file of the corpus's malformed set fails or draws within the time
/// limit.  Skipped without the corpus.
#[test]
fn malformed_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/images23-extra/svg/malformed");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let start = std::time::Instant::now();
        let _ = picture(&e.path(), &Limits::thumbnail());
        assert!(start.elapsed() < Duration::from_secs(25), "{}", e.path().display());
    }
}
