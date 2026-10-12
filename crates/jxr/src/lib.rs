//! JPEG XR (`.jxr`, `.wdp`, `.hdp`): Microsoft's HD Photo, which Windows
//! uses for HDR screenshots and game captures (16-bit float scRGB) and some
//! cameras and scanners for 16-bit stills.
//!
//! Decoding is done by Microsoft's reference decoder, jxrlib (BSD-2-Clause,
//! vendored in `vendor/jxrlib`), through the small shim in `src/shim.c`.
//! jxrlib is a large body of C that trusts much of what a file says and has
//! a long record of memory-safety bugs, and thumbnails decode whatever a
//! user browses past, including downloads.  So on Unix the decode runs in a
//! forked child process with a CPU limit and a memory limit and sends plain
//! pixel data back through a pipe; a crash, a hang or a runaway allocation
//! costs one thumbnail.  Windows has no fork; there the decode runs in the
//! calling process, which for thumbnails is the helper rather than Explorer.
//!
//! The pixel formats jxrlib's converter handles come out as 8-bit RGBA
//! (8-bit formats, and 16-bit and packed ones reduced to 8) or as 32-bit
//! float RGBA (half, fixed-point and float formats, as HDR screenshots
//! store them).  CMYK and N-channel files are refused with a message.

use std::time::Duration;

/// Why a file couldn't be decoded.
#[derive(Debug, thiserror::Error)]
pub enum JxrError {
    #[error("{0}")]
    Unsupported(String),
    #[error("invalid JPEG XR file: {0}")]
    Invalid(String),
    #[error("JPEG XR picture too large: {0}")]
    TooLarge(String),
}

/// The memory layout of the converted pixels, as the shim reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Gray8,
    Rgb24,
    Bgr24,
    /// B, G, R and an unused byte.
    Bgr32,
    Rgba32,
    Bgra32,
    PremultipliedRgba32,
    PremultipliedBgra32,
    GrayF32,
    RgbF32,
    /// Three floats and an unused one.
    RgbxF32,
    RgbaF32,
    PremultipliedRgbaF32,
    /// Half-precision floats, which Windows HDR screenshots use.
    GrayF16,
    RgbF16,
    /// Three halves and an unused one.
    RgbxF16,
    RgbaF16,
}

impl Layout {
    fn from_code(code: i32) -> Option<Layout> {
        Some(match code {
            0 => Layout::Gray8,
            1 => Layout::Rgb24,
            2 => Layout::Bgr24,
            3 => Layout::Bgr32,
            4 => Layout::Rgba32,
            5 => Layout::Bgra32,
            6 => Layout::PremultipliedRgba32,
            7 => Layout::PremultipliedBgra32,
            8 => Layout::GrayF32,
            9 => Layout::RgbF32,
            10 => Layout::RgbxF32,
            11 => Layout::RgbaF32,
            12 => Layout::PremultipliedRgbaF32,
            13 => Layout::GrayF16,
            14 => Layout::RgbF16,
            15 => Layout::RgbxF16,
            16 => Layout::RgbaF16,
            _ => return None,
        })
    }

    /// Bytes of one pixel.
    pub fn bytes(self) -> usize {
        match self {
            Layout::Gray8 => 1,
            Layout::Rgb24 | Layout::Bgr24 => 3,
            Layout::Bgr32
            | Layout::Rgba32
            | Layout::Bgra32
            | Layout::PremultipliedRgba32
            | Layout::PremultipliedBgra32 => 4,
            Layout::GrayF32 => 4,
            Layout::GrayF16 => 2,
            Layout::RgbF16 => 6,
            Layout::RgbxF16 | Layout::RgbaF16 => 8,
            Layout::RgbF32 => 12,
            Layout::RgbxF32 | Layout::RgbaF32 | Layout::PremultipliedRgbaF32 => 16,
        }
    }

    /// Whether the samples are floating point (linear light, HDR) rather
    /// than 8-bit (display-referred).
    pub fn is_float(self) -> bool {
        matches!(
            self,
            Layout::GrayF32
                | Layout::RgbF32
                | Layout::RgbxF32
                | Layout::RgbaF32
                | Layout::PremultipliedRgbaF32
                | Layout::GrayF16
                | Layout::RgbF16
                | Layout::RgbxF16
                | Layout::RgbaF16
        )
    }
}

/// A decoded picture, rows tightly packed in `layout`.
#[derive(Debug)]
pub struct Decoded {
    /// The decoded size, which is the file's size shrunk by the factor asked
    /// for.
    pub width: u32,
    pub height: u32,
    pub full_width: u32,
    pub full_height: u32,
    pub layout: Layout,
    pub has_alpha: bool,
    /// The file's pixel format by name.
    pub native: String,
    pub dpi: Option<(f32, f32)>,
    pub pixels: Vec<u8>,
}

/// RGBA pixels with straight (not premultiplied) alpha.
#[derive(Debug)]
pub enum Rgba {
    U8(Vec<u8>),
    F32(Vec<f32>),
}

impl Decoded {
    /// Widens the pixels to RGBA: grey replicated, BGR swapped, premultiplied
    /// alpha divided out.
    pub fn to_rgba(&self) -> Rgba {
        let n = self.width as usize * self.height as usize;
        let src = &self.pixels;
        if self.layout.is_float() {
            // A sample as a float, whichever width the layout stores.
            let half = matches!(self.layout, Layout::GrayF16 | Layout::RgbF16 | Layout::RgbxF16 | Layout::RgbaF16);
            let f = |i: usize| -> f32 {
                if half {
                    half_to_f32(u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]))
                } else {
                    f32::from_le_bytes([src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]])
                }
            };
            let mut out = Vec::with_capacity(n * 4);
            for p in 0..n {
                let px: [f32; 4] = match self.layout {
                    Layout::GrayF32 | Layout::GrayF16 => [f(p), f(p), f(p), 1.0],
                    Layout::RgbF32 | Layout::RgbF16 => [f(p * 3), f(p * 3 + 1), f(p * 3 + 2), 1.0],
                    Layout::RgbxF32 | Layout::RgbxF16 => [f(p * 4), f(p * 4 + 1), f(p * 4 + 2), 1.0],
                    // Reduced-size decoding can overshoot a constant alpha a little (1.03).
                    Layout::RgbaF32 | Layout::RgbaF16 => {
                        let a = f(p * 4 + 3);
                        [f(p * 4), f(p * 4 + 1), f(p * 4 + 2), if a.is_nan() { 1.0 } else { a.clamp(0.0, 1.0) }]
                    }
                    _ => {
                        let a = f(p * 4 + 3);
                        let k = if a > 0.0 { 1.0 / a } else { 0.0 };
                        [f(p * 4) * k, f(p * 4 + 1) * k, f(p * 4 + 2) * k, a]
                    }
                };
                out.extend_from_slice(&px);
            }
            return Rgba::F32(out);
        }
        let mut out = Vec::with_capacity(n * 4);
        for p in 0..n {
            let px: [u8; 4] = match self.layout {
                Layout::Gray8 => [src[p], src[p], src[p], 255],
                Layout::Rgb24 => [src[p * 3], src[p * 3 + 1], src[p * 3 + 2], 255],
                Layout::Bgr24 => [src[p * 3 + 2], src[p * 3 + 1], src[p * 3], 255],
                Layout::Bgr32 => [src[p * 4 + 2], src[p * 4 + 1], src[p * 4], 255],
                Layout::Rgba32 => [src[p * 4], src[p * 4 + 1], src[p * 4 + 2], src[p * 4 + 3]],
                Layout::Bgra32 => [src[p * 4 + 2], src[p * 4 + 1], src[p * 4], src[p * 4 + 3]],
                Layout::PremultipliedRgba32 => {
                    unpremultiply([src[p * 4], src[p * 4 + 1], src[p * 4 + 2]], src[p * 4 + 3])
                }
                _ => unpremultiply([src[p * 4 + 2], src[p * 4 + 1], src[p * 4]], src[p * 4 + 3]),
            };
            out.extend_from_slice(&px);
        }
        Rgba::U8(out)
    }
}

/// IEEE half precision to single, subnormals, infinities and NaN included.
fn half_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let shift = man.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((man << shift) & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

fn unpremultiply(c: [u8; 3], a: u8) -> [u8; 4] {
    if a == 0 {
        return [0, 0, 0, 0];
    }
    let k = |v: u8| ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
    [k(c[0]), k(c[1]), k(c[2]), a]
}

/// JPEG XR files start with `II` and 0xBC and a version of 0 or 1 (the TIFF-like
/// container); bare codestreams start with `WMPHOTO`.
pub fn sniff(head: &[u8]) -> bool {
    (head.len() >= 4 && head[0] == b'I' && head[1] == b'I' && head[2] == 0xbc && head[3] <= 1)
        || head.starts_with(b"WMPHOTO")
}

/// Seconds a decode may take before it is abandoned.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Decodes a JPEG XR file in memory.  With a `min_size` the picture comes
/// back reduced by a power of two, as much as leaves its longer side at
/// least that long (JPEG XR skips the finest detail, so this is cheaper than
/// decoding it whole, but jxrlib reduces by at most 16); 0 gives the whole
/// picture.  Refuses pictures whose converted pixels would take more than
/// `max_bytes`.
pub fn decode(bytes: &[u8], min_size: u32, max_bytes: usize) -> Result<Decoded, JxrError> {
    if !sniff(bytes) {
        return Err(JxrError::Invalid("not a JPEG XR file".into()));
    }
    let wire = isolated(bytes, min_size, max_bytes)?;
    parse_wire(&wire, max_bytes)
}

/*┌─────────────────╮
| The C interface |
└─────────────────╯*/
//region The C interface

#[repr(C)]
struct Out {
    status: i32,
    width: i32,
    height: i32,
    layout: i32,
    stride: i32,
    alpha: i32,
    full_width: i32,
    full_height: i32,
    dpi_x: f32,
    dpi_y: f32,
    native: [u8; 40],
    message: [u8; 64],
}

unsafe extern "C" {
    fn slerp_jxr_decode(
        data: *const u8,
        len: usize,
        min_size: u32,
        max_bytes: u64,
        out: *mut Out,
        pixels: *mut *mut u8,
    ) -> i32;
    fn slerp_jxr_free(pixels: *mut u8);
}

fn text_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// What the child sends: a tag byte (0 for a picture, 1 for an error
/// message), then for a picture this header and the tightly packed rows.
const HEADER_BYTES: usize = 4 * 8 + 4 * 2 + 40;

/// Runs the decoder where it is, returning the bytes the child would send.
fn decode_here(bytes: &[u8], min_size: u32, max_bytes: usize) -> Result<Vec<u8>, String> {
    // SAFETY: `Out` is plain old data that the shim fills in; `pixels` is
    // set to a buffer the shim allocated, which is freed below.
    unsafe {
        let mut out: Out = std::mem::zeroed();
        let mut pixels: *mut u8 = std::ptr::null_mut();
        let status = slerp_jxr_decode(bytes.as_ptr(), bytes.len(), min_size, max_bytes as u64, &mut out, &mut pixels);
        if status != 0 || pixels.is_null() {
            let message = text_of(&out.message);
            return Err(if message.is_empty() { format!("jxrlib error {status}") } else { message });
        }
        let layout = Layout::from_code(out.layout);
        let (w, h, stride) = (out.width as usize, out.height as usize, out.stride as usize);
        let Some(layout) = layout.filter(|_| w > 0 && h > 0 && stride > 0) else {
            slerp_jxr_free(pixels);
            return Err("the decoder returned an unknown layout".into());
        };
        let row = w * layout.bytes();
        let mut wire = Vec::with_capacity(HEADER_BYTES + row * h);
        for v in [out.width, out.height, out.layout, out.alpha, out.full_width, out.full_height] {
            wire.extend((v as u32).to_le_bytes());
        }
        wire.extend(0u32.to_le_bytes());
        wire.extend(0u32.to_le_bytes());
        wire.extend(out.dpi_x.to_le_bytes());
        wire.extend(out.dpi_y.to_le_bytes());
        wire.extend(out.native);
        for y in 0..h {
            wire.extend_from_slice(std::slice::from_raw_parts(pixels.add(y * stride), row));
        }
        slerp_jxr_free(pixels);
        Ok(wire)
    }
}

fn parse_wire(wire: &[u8], max_bytes: usize) -> Result<Decoded, JxrError> {
    if wire.len() < HEADER_BYTES {
        return Err(JxrError::Invalid("the decoder's answer was cut short".into()));
    }
    let u = |i: usize| u32::from_le_bytes([wire[i * 4], wire[i * 4 + 1], wire[i * 4 + 2], wire[i * 4 + 3]]);
    let f = |i: usize| f32::from_le_bytes([wire[i * 4], wire[i * 4 + 1], wire[i * 4 + 2], wire[i * 4 + 3]]);
    let layout = Layout::from_code(u(2) as i32).ok_or_else(|| JxrError::Invalid("unknown pixel layout".into()))?;
    let (width, height) = (u(0), u(1));
    let expected = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(layout.bytes()));
    let body = &wire[HEADER_BYTES..];
    if expected != Some(body.len()) || body.len() > max_bytes {
        return Err(JxrError::Invalid("the decoder's answer has the wrong size".into()));
    }
    let dpi = (f(8).is_finite() && f(9).is_finite() && f(8) > 0.0 && f(9) > 0.0).then(|| (f(8), f(9)));
    Ok(Decoded {
        width,
        height,
        full_width: u(4),
        full_height: u(5),
        layout,
        has_alpha: u(3) != 0,
        native: text_of(&wire[40..HEADER_BYTES]),
        dpi,
        pixels: body.to_vec(),
    })
}

//endregion

/*┌─────────────────╮
| Isolation       |
└─────────────────╯*/
//region Isolation

/// Decodes in a child process, which gets a CPU limit and a memory limit
/// and is killed after [`TIMEOUT`].
#[cfg(unix)]
fn isolated(bytes: &[u8], min_size: u32, max_bytes: usize) -> Result<Vec<u8>, JxrError> {
    use std::time::Instant;

    // 1. A pipe for the answer.
    let mut fds = [0i32; 2];
    // SAFETY: fds has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(JxrError::Invalid(format!("couldn't make a pipe: {}", std::io::Error::last_os_error())));
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    for fd in fds {
        // SAFETY: fd is a descriptor this function owns.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // The memory the child may add to what it inherits: a few times the
    // picture, since the decoder keeps working buffers beside the output.
    let budget = (max_bytes as u64).saturating_mul(3).saturating_add(384 << 20);
    let data_limit = current_data_bytes().map(|d| d.saturating_add(budget));

    // SAFETY: the child only runs the decoder (C that allocates, which glibc
    // and macOS make safe after fork), writes to the pipe and leaves with
    // _exit, never returning into the caller's code or running its
    // destructors, and touches no locks other threads might have held.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        // SAFETY: closing descriptors this function owns.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(JxrError::Invalid(format!(
            "couldn't start a decoder process: {}",
            std::io::Error::last_os_error()
        )));
    }
    if pid == 0 {
        // 2. The child: limits first, then the decode.
        // SAFETY: closing the end the child doesn't use; setrlimit gets a
        // valid struct; the descriptors duplicated onto are the child's own.
        unsafe {
            libc::close(read_fd);
            // jxrlib prints complaints about metadata tags it doesn't know;
            // they must not reach the parent's output.
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if null >= 0 {
                libc::dup2(null, 1);
                libc::dup2(null, 2);
                libc::close(null);
            }
            let cpu = libc::rlimit { rlim_cur: TIMEOUT.as_secs() as _, rlim_max: TIMEOUT.as_secs() as _ };
            libc::setrlimit(libc::RLIMIT_CPU, &cpu);
            #[cfg(target_os = "linux")]
            if let Some(limit) = data_limit {
                let data = libc::rlimit { rlim_cur: limit as _, rlim_max: limit as _ };
                libc::setrlimit(libc::RLIMIT_DATA, &data);
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_here(bytes, min_size, max_bytes)))
            .unwrap_or_else(|_| Err("the decoder panicked".to_string()));
        let (tag, body) = match result {
            Ok(wire) => (0u8, wire),
            Err(message) => (1u8, message.into_bytes()),
        };
        let ok = write_all(write_fd, &[tag]) && write_all(write_fd, &body);
        // SAFETY: _exit ends the child without running the parent's
        // destructors or atexit handlers.
        unsafe { libc::_exit(if ok { 0 } else { 2 }) };
    }

    // 3. The parent reads until the child closes the pipe, it runs out of
    //    time, or it sends more than a picture can hold.
    // SAFETY: closing the end the parent doesn't use.
    unsafe { libc::close(write_fd) };
    let _ = data_limit;
    let deadline = Instant::now() + TIMEOUT;
    let cap = max_bytes.saturating_add(HEADER_BYTES + 1024);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let failure = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Some(format!("the JPEG XR decoder took longer than {} s", TIMEOUT.as_secs()));
        }
        let mut pfd = libc::pollfd { fd: read_fd, events: libc::POLLIN, revents: 0 };
        let ms = left.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: pfd is one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Some(format!("couldn't wait for the decoder: {}", std::io::Error::last_os_error()));
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: buf is valid for writes of its length.
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Some(format!("couldn't read the decoder's answer: {}", std::io::Error::last_os_error()));
        }
        if n == 0 {
            break None;
        }
        if out.len() + n as usize > cap {
            break Some("the decoder's answer was too large".to_string());
        }
        out.extend_from_slice(&buf[..n as usize]);
    };
    // SAFETY: closing the parent's end.
    unsafe { libc::close(read_fd) };

    // 4. Reap the child, killing it first if it is still running.
    if failure.is_some() {
        // SAFETY: pid is our own child, not yet reaped.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let mut status = 0i32;
    // SAFETY: pid is our own child; status is a valid out pointer.
    let reaped = loop {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break r;
        }
    };
    if let Some(message) = failure {
        return Err(JxrError::Invalid(message));
    }
    if reaped == pid && libc::WIFSIGNALED(status) {
        return Err(JxrError::Invalid(format!(
            "the JPEG XR decoder crashed (signal {}) on this file",
            libc::WTERMSIG(status)
        )));
    }
    match out.split_first() {
        Some((0, body)) => Ok(body.to_vec()),
        Some((1, message)) => Err(classify(String::from_utf8_lossy(message).into_owned())),
        _ => Err(JxrError::Invalid("the JPEG XR decoder stopped without an answer".into())),
    }
}

/// Without fork the decode runs here.
#[cfg(not(unix))]
fn isolated(bytes: &[u8], min_size: u32, max_bytes: usize) -> Result<Vec<u8>, JxrError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_here(bytes, min_size, max_bytes)))
        .unwrap_or_else(|_| Err("the decoder panicked".to_string()))
        .map_err(classify)
}

/// Sorts the shim's messages into the error kinds callers care about.
fn classify(message: String) -> JxrError {
    if message.contains("too large") || message.contains("out of memory") {
        JxrError::TooLarge(message)
    } else if message.contains("not shown") || message.contains("unsupported") {
        JxrError::Unsupported(message)
    } else {
        JxrError::Invalid(message)
    }
}

/// Bytes of heap and private data the process holds now, which a limit on
/// the child's data segment must allow for since it inherits them (Linux).
#[cfg(unix)]
fn current_data_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmData:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(unix)]
fn write_all(fd: i32, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        // SAFETY: data is valid for reads of its length.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert!(sniff(b"II\xbc\x01\x08\0\0\0"));
        assert!(sniff(b"II\xbc\x00rest"));
        assert!(sniff(b"WMPHOTO\0"));
        assert!(!sniff(b"II*\0\x08\0\0\0"));
        assert!(!sniff(b"II"));
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode(b"not a picture", 0, 1 << 20).is_err());
        let mut fake = b"II\xbc\x01".to_vec();
        fake.extend([0xff; 64]);
        assert!(decode(&fake, 0, 1 << 20).is_err());
    }

    #[test]
    fn halves() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
        assert!(half_to_f32(0x7c00).is_infinite() && half_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn premultiplied_alpha_is_divided_out() {
        assert_eq!(unpremultiply([100, 50, 0], 100), [255, 128, 0, 100]);
        assert_eq!(unpremultiply([9, 9, 9], 0), [0, 0, 0, 0]);
    }
}
