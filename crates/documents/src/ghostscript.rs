//! The system's Ghostscript, run as a separate program to draw PostScript.
//!
//! Ghostscript is AGPL, so it is never linked or bundled: Slerp only runs
//! the `gs` a user installed, the way a file manager runs a thumbnailer.
//! It runs in safe mode (`-dSAFER`: no file access beyond what it needs,
//! no pipes or devices), reading the PostScript from its standard input
//! (so no file name can pass for an option or a `%pipe%`, and the
//! AppArmor profile Ubuntu gives `gs`, which lets it read only some
//! folders and extensions, doesn't matter), its output and temporary files
//! going to a private scratch folder (safe mode still lets PostScript write
//! in the temporary folder), its environment's `GS_OPTIONS` removed, a
//! processor and memory limit, and a time limit after which it is killed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Time Ghostscript may take for a thumbnail, and for the viewer.
const THUMBNAIL_TIME: Duration = Duration::from_millis(1800);
const VIEWER_TIME: Duration = Duration::from_secs(20);
/// Memory Ghostscript may use (Linux).
#[cfg(target_os = "linux")]
const MEMORY: u64 = 1536 << 20;
/// The largest picture read back.
const MAX_SIDE: u32 = crate::MAX_SIDE;

/// The Ghostscript program, if one is installed: `gs` on the search path
/// (and in Homebrew's folders on macOS, which apps started from the Finder
/// don't have on theirs), `gswin64c.exe` on Windows.  `SLERP_GHOSTSCRIPT`
/// names another, or `none` to act as if there were none.
pub fn ghostscript() -> Option<&'static Path> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            if let Some(p) = std::env::var_os("SLERP_GHOSTSCRIPT") {
                return (p != "none").then(|| PathBuf::from(p)).filter(|p| p.is_file());
            }
            let names: &[&str] = if cfg!(windows) { &["gswin64c.exe", "gswin32c.exe", "gs.exe"] } else { &["gs"] };
            let mut dirs: Vec<PathBuf> =
                std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
            if cfg!(target_os = "macos") {
                dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from));
            }
            dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
        })
        .as_deref()
}

/// Ghostscript's version ("10.06.0"), asked once.
pub fn ghostscript_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            let gs = ghostscript()?;
            let out =
                run_limited(Command::new(gs).arg("--version"), None::<std::io::Empty>, Duration::from_secs(3)).ok()?;
            let v = String::from_utf8_lossy(&out).trim().to_string();
            (!v.is_empty() && v.len() < 40).then_some(v)
        })
        .as_deref()
}

/// A drawn picture.
pub struct Drawn {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub dpi: f64,
}

/// Draws the PostScript at `section` (offset and length) of `path` with
/// Ghostscript: an EPS cropped to its bounding box, or page `page` (from 1)
/// of a PostScript document.  `size` is the drawing's size in points when
/// known; `fit` the long side wanted for a thumbnail, else the viewer's size
/// (twice the natural size, within bounds).
pub fn draw(
    gs: &Path,
    path: &Path,
    section: (u64, u64),
    page: Option<usize>,
    eps: bool,
    size: Option<(f64, f64)>,
    fit: Option<u32>,
) -> Result<Drawn, String> {
    // 1. The resolution that gives the size wanted.
    let (w, h) = size.filter(|(w, h)| *w > 0.0 && *h > 0.0).unwrap_or((612.0, 792.0));
    let long = w.max(h);
    let target = match fit {
        Some(n) => n.clamp(16, MAX_SIDE) as f64,
        None => (long * 2.0).clamp(1024.0, 4096.0),
    };
    let dpi = (target / long * 72.0).clamp(1.0, 2400.0);
    let time = if fit.is_some() { THUMBNAIL_TIME } else { VIEWER_TIME };

    // 2. A private scratch folder for the output.
    let mut input = std::fs::File::open(path).map_err(|e| e.to_string())?;
    std::io::Seek::seek(&mut input, std::io::SeekFrom::Start(section.0)).map_err(|e| e.to_string())?;
    // Some EPS files never call showpage, which Ghostscript reads from a
    // file name as an EPS but not from a pipe: one is added, and only the
    // first page kept, so files that did call it don't come out blank.
    let tail: &'static [u8] = if eps { b"\nshowpage\n" } else { b"" };
    let input = std::io::Read::take(input, section.1).chain(tail);
    let dir = scratch()?;
    let out = dir.join("page.png");
    let mut cmd = Command::new(gs);
    cmd.args(["-q", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-dNOPROMPT", "-sDEVICE=png16m"])
        .args(["-dTextAlphaBits=4", "-dGraphicsAlphaBits=4"])
        .arg(format!("-r{dpi:.3}"));
    if eps {
        cmd.arg("-dEPSCrop");
    }
    if let Some(p) = page.or(eps.then_some(1)) {
        cmd.arg(format!("-dFirstPage={p}")).arg(format!("-dLastPage={p}"));
    }
    let mut out_arg = std::ffi::OsString::from("-sOutputFile=");
    out_arg.push(&out);
    // Safe mode lets PostScript write files in the temporary folder, so the
    // scratch folder serves as Ghostscript's.
    cmd.arg(out_arg).arg("-").env_remove("GS_OPTIONS").current_dir(&dir);
    for var in ["TMPDIR", "TEMP", "TMP"] {
        cmd.env(var, &dir);
    }
    let result = run_limited(&mut cmd, Some(input), time).and_then(|_| read_png(&out));
    let _ = std::fs::remove_dir_all(&dir);
    let (width, height, rgba) = result?;
    Ok(Drawn { width, height, rgba, dpi })
}

/// A new empty folder of our own in the temporary folder.
fn scratch() -> Result<PathBuf, String> {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("slerp-gs-{}-{n}", std::process::id()));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(&dir).map_err(|e| format!("couldn't make a scratch folder: {e}"))?;
    Ok(dir)
}

/// Runs a program with `input` (or nothing) as its standard input, a
/// processor and memory limit (Unix) and a time limit, and returns its
/// output, or its complaint when it fails.
fn run_limited(
    cmd: &mut Command,
    input: Option<impl Read + Send + 'static>,
    time: Duration,
) -> Result<Vec<u8>, String> {
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    cmd.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let cpu = time.as_secs() + 2;
        // SAFETY: only setrlimit, which is async-signal-safe, runs between
        // fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                let c = libc::rlimit { rlim_cur: cpu as _, rlim_max: cpu as _ };
                libc::setrlimit(libc::RLIMIT_CPU, &c);
                #[cfg(target_os = "linux")]
                {
                    let m = libc::rlimit { rlim_cur: MEMORY as _, rlim_max: MEMORY as _ };
                    libc::setrlimit(libc::RLIMIT_DATA, &m);
                }
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("couldn't run Ghostscript: {e}"))?;
    // The input is fed on a thread; it stops when the program ends or is
    // killed, since its end of the pipe closes.
    if let (Some(mut input), Some(mut pipe)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut input, &mut pipe);
        });
    }
    // Output is read on threads so a chatty program never blocks on a full
    // pipe; at most 64 KB of each is kept.
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut r) = r {
                let mut buf = [0u8; 8192];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if kept.len() < 64 << 10 {
                        kept.extend_from_slice(&buf[..n]);
                    }
                }
            }
            kept
        })
    };
    let out = drain(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let deadline = Instant::now() + time;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        None => Err(format!("Ghostscript took longer than {:.1} s", time.as_secs_f32())),
        Some(s) if s.success() => Ok(out),
        Some(_) => {
            // PostScript errors ("Error: /undefined in ...") go to the
            // output, Ghostscript's own complaints to the error stream.
            let text = format!("{}\n{}", String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
            let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
            let first = lines().find(|l| l.starts_with("Error:")).or_else(|| lines().next()).unwrap_or("it failed");
            Err(format!("Ghostscript couldn't draw this file: {}", first.chars().take(200).collect::<String>()))
        }
    }
}

/// Reads Ghostscript's PNG, refusing pictures too large to show.
fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|_| "Ghostscript drew nothing".to_string())?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || w as u64 * h as u64 > crate::MAX_PIXELS {
        return Err(format!("Ghostscript's picture is too large ({w} × {h})"));
    }
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("a picture too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let px = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        _ => return Err("Ghostscript's picture has an unexpected layout".into()),
    };
    Ok((w, h, rgba))
}
