//! Asks the Windows shell for thumbnails the way Explorer does, through
//! `IShellItemImageFactory::GetImage` with `SIIGBF_THUMBNAILONLY`, and saves
//! them as PNGs.  This goes through the thumbnail cache and whatever
//! handler is registered, so it tests the registration as much as the DLL.
//!
//! ```text
//! shell_thumb [--touch] [--direct] <size> <out dir> <file>...
//! ```
//!
//! `--touch` sets each file's modification time to now first, so the cache
//! asks the handler again instead of answering from an earlier run.
//! `--direct` skips the cache and Explorer's isolated thumbnail process:
//! it binds the registered handler in this process and calls it.
//! `shell_thumb --open-with .glb .fbx` lists what "Open with" offers.  Each
//! file prints `OK <ms> <w>x<h> <premultiplied|straight|opaque> <file>` or
//! `FAIL <ms> <error> <file>`.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows_main::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("shell_thumb asks the Windows shell for thumbnails; it only runs on Windows");
}

#[cfg(windows)]
mod windows_main {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::Instant;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
        GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::{
        ASSOC_FILTER_NONE, BHID_ThumbnailHandler, IShellItem, IShellItemImageFactory, IThumbnailProvider,
        SHAssocEnumHandlers, SHCreateItemFromParsingName, SIIGBF_THUMBNAILONLY, WTS_ALPHATYPE,
    };
    use windows::core::HSTRING;

    pub fn run() -> ExitCode {
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().is_some_and(|a| a == "--open-with") {
            return open_with(&args[1..]);
        }
        let mut touch = false;
        let mut direct = false;
        while let Some(flag) = args.first().filter(|a| a.starts_with("--")).cloned() {
            match flag.as_str() {
                "--touch" => touch = true,
                "--direct" => direct = true,
                _ => {
                    eprintln!("unknown option {flag}");
                    return ExitCode::FAILURE;
                }
            }
            args.remove(0);
        }
        let (Some(size), Some(out_dir)) = (args.first().and_then(|s| s.parse::<i32>().ok()), args.get(1)) else {
            eprintln!("usage: shell_thumb [--touch] [--direct] <size> <out dir> <file>...");
            return ExitCode::FAILURE;
        };
        let out_dir = PathBuf::from(out_dir);
        let _ = std::fs::create_dir_all(&out_dir);
        // SAFETY: once per thread, before any COM call.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let mut failed = false;
        for file in &args[2..] {
            let path = std::path::absolute(file).unwrap_or_else(|_| PathBuf::from(file));
            if touch {
                let now = std::time::SystemTime::now();
                let _ = std::fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(now));
            }
            let start = Instant::now();
            let result = if direct { direct_thumbnail(&path, size) } else { thumbnail(&path, size) };
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            match result.and_then(|bitmap| save(bitmap, &out_dir, &path)) {
                Ok((w, h, alpha)) => println!("OK {ms:.0} {w}x{h} {alpha} {}", path.display()),
                Err(e) => {
                    failed = true;
                    println!("FAIL {ms:.0} {e} {}", path.display());
                }
            }
        }
        if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
    }

    fn thumbnail(path: &Path, size: i32) -> windows::core::Result<HBITMAP> {
        // SAFETY: plain shell calls with a valid path.
        unsafe {
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
            factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_THUMBNAILONLY)
        }
    }

    /// Prints the programs Explorer's "Open with" menu offers for each
    /// extension, by the names it shows.
    fn open_with(exts: &[String]) -> ExitCode {
        // SAFETY: once per thread, before any COM call.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        for ext in exts {
            // SAFETY: plain shell enumeration; strings are freed after use.
            let handlers = unsafe { SHAssocEnumHandlers(&HSTRING::from(ext.as_str()), ASSOC_FILTER_NONE) };
            let Ok(handlers) = handlers else {
                println!("{ext}: no handlers");
                continue;
            };
            let mut names = Vec::new();
            loop {
                let mut got = [None];
                let mut n = 0;
                if unsafe { handlers.Next(&mut got, Some(&mut n)) }.is_err() || n == 0 {
                    break;
                }
                let Some(handler) = got[0].take() else { break };
                let text = |p: windows::core::Result<windows::core::PWSTR>| {
                    p.map(|p| {
                        let s = unsafe { p.to_string() }.unwrap_or_default();
                        unsafe { CoTaskMemFree(Some(p.0.cast())) };
                        s
                    })
                    .unwrap_or_default()
                };
                names.push(format!(
                    "{} ({})",
                    text(unsafe { handler.GetUIName() }),
                    text(unsafe { handler.GetName() })
                ));
            }
            println!("{ext}: {}", names.join("; "));
        }
        ExitCode::SUCCESS
    }

    /// Binds the item's thumbnail handler in this process (a null bind
    /// context does that) and asks it directly, bypassing the cache and
    /// the isolated thumbnail process.
    fn direct_thumbnail(path: &Path, size: i32) -> windows::core::Result<HBITMAP> {
        // SAFETY: plain shell calls with a valid path and out pointers.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
            let provider: IThumbnailProvider = item.BindToHandler(None, &BHID_ThumbnailHandler)?;
            let mut bitmap = HBITMAP::default();
            let mut alpha = WTS_ALPHATYPE::default();
            provider.GetThumbnail(size as u32, &mut bitmap, &mut alpha)?;
            Ok(bitmap)
        }
    }

    /// Reads the bitmap's pixels and writes `<file name>.png`.  Returns its
    /// size and what its alpha looks like: when any colour channel exceeds
    /// alpha, the colours can't be premultiplied.
    fn save(bitmap: HBITMAP, out_dir: &Path, path: &Path) -> windows::core::Result<(i32, i32, &'static str)> {
        // SAFETY: a valid bitmap handle, deleted at the end; the buffers are
        // sized for what GetDIBits writes.
        unsafe {
            let mut info = BITMAP::default();
            GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some((&mut info as *mut BITMAP).cast()));
            let (w, h) = (info.bmWidth, info.bmHeight.abs());
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            let dc = CreateCompatibleDC(None);
            let rows = GetDIBits(dc, bitmap, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            if rows == 0 {
                return Err(windows::core::Error::from_thread());
            }
            let opaque = bgra.chunks_exact(4).all(|p| p[3] == 255);
            let straight = bgra.chunks_exact(4).any(|p| p[0].max(p[1]).max(p[2]) > p[3]);
            let alpha = if opaque {
                "opaque"
            } else if straight {
                "straight"
            } else {
                "premultiplied"
            };
            let rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let out = std::fs::File::create(out_dir.join(format!("{name}.png")))
                .map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(out), w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder
                .write_header()
                .and_then(|mut wr| wr.write_image_data(&rgba))
                .map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
            Ok((w, h, alpha))
        }
    }
}
