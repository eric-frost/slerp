//! The AV1 decoder behind AVIF: rav1d (BSD-2-Clause), the Rust port of
//! VideoLAN's dav1d, through its C-style `dav1d_*` API (the only public one).
//!
//! rav1d decodes 8, 10 and 12-bit pictures in 4:0:0, 4:2:0, 4:2:2 and 4:4:4
//! as fast as the C original when its assembly is built (the `avif-asm`
//! feature, which needs `nasm` on x86), and 4 to 5 times slower without.  A
//! single thread is as quick as many for a still; this runs it on one thread,
//! frame delay 1 (one picture in, one out), with a pixel limit set, since
//! the decoder's default is no limit at all.
//!
//! The data given is an item or sample as AVIF stores it, a temporal unit of
//! OBUs, with the sequence header from the `av1C` box put in front when the
//! data has none.

use crate::yuv::Yuv;

/// AV1's OBU types: the sequence header is 1.
#[cfg_attr(not(feature = "avif"), allow(dead_code))]
const OBU_SEQUENCE_HEADER: u8 = 1;

/// Whether `data` (OBUs in the low-overhead format) holds a sequence header.
#[cfg_attr(not(feature = "avif"), allow(dead_code))]
fn has_sequence_header(data: &[u8]) -> bool {
    let mut at = 0;
    while at < data.len() {
        let header = data[at];
        let kind = (header >> 3) & 15;
        let extension = header & 4 != 0;
        let sized = header & 2 != 0;
        if kind == OBU_SEQUENCE_HEADER {
            return true;
        }
        at += 1 + extension as usize;
        if !sized {
            // The last OBU runs to the end.
            return false;
        }
        // leb128 size.
        let (mut size, mut shift) = (0usize, 0);
        loop {
            let Some(&b) = data.get(at) else { return false };
            at += 1;
            size |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 || shift > 28 {
                break;
            }
        }
        at = at.saturating_add(size);
    }
    false
}

#[cfg(feature = "avif")]
mod imp {
    use super::*;
    use crate::yuv::{Cicp, Layout};
    use rav1d::include::dav1d::data::Dav1dData;
    use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
    use rav1d::include::dav1d::picture::Dav1dPicture;
    use rav1d::src::lib::{
        dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture, dav1d_open,
        dav1d_picture_unref, dav1d_send_data,
    };
    use std::mem::MaybeUninit;
    use std::ptr::NonNull;

    /// What `-EAGAIN` is on this platform, as rav1d reports it.
    const EAGAIN: i32 = -(libc::EAGAIN);

    /// An open decoder context.
    struct Ctx(Option<Dav1dContext>);

    // SAFETY: a context is used from one thread at a time (the animation's
    // lock sees to that) and runs no threads of its own with `n_threads` 1.
    unsafe impl Send for Ctx {}

    impl Drop for Ctx {
        fn drop(&mut self) {
            // SAFETY: the context came from dav1d_open and is closed once.
            unsafe { dav1d_close(NonNull::new(&mut self.0)) };
        }
    }

    /// A decoder fed temporal units in order.
    pub struct Stream {
        ctx: Ctx,
        /// The `av1C` box's configuration OBUs (the sequence header).
        config: Vec<u8>,
        started: bool,
    }

    impl Stream {
        /// Opens a decoder for pictures of up to `max_pixels` pixels, at
        /// operating point `operating_point` of a scalable stream (a file's
        /// `a1op` property; 0 is the full quality one).
        pub fn new(config: &[u8], max_pixels: u64, operating_point: u8) -> Result<Stream, String> {
            // SAFETY: follows the dav1d C API contract; every pointer is to a live local.
            unsafe {
                let mut s = MaybeUninit::<Dav1dSettings>::uninit();
                dav1d_default_settings(NonNull::new(s.as_mut_ptr()).ok_or("no settings")?);
                let mut s = s.assume_init();
                s.n_threads = 1;
                s.max_frame_delay = 1;
                // Only the highest layer of the operating point.
                s.all_layers = 0;
                s.operating_point = operating_point.min(31) as _;
                s.frame_size_limit = max_pixels.min(u32::MAX as u64) as _;
                let mut ctx = Ctx(None);
                let r = dav1d_open(NonNull::new(&mut ctx.0), NonNull::new(&mut s));
                if r.0 != 0 || ctx.0.is_none() {
                    return Err(format!("the AV1 decoder wouldn't start (error {})", r.0));
                }
                Ok(Stream { ctx, config: config.to_vec(), started: false })
            }
        }

        /// Gives the decoder one temporal unit and returns the picture it made.
        pub fn decode(&mut self, data: &[u8]) -> Result<Option<Yuv>, String> {
            let mut unit;
            let data = if !self.started && !has_sequence_header(data) {
                unit = Vec::with_capacity(self.config.len() + data.len());
                unit.extend_from_slice(&self.config);
                unit.extend_from_slice(data);
                &unit[..]
            } else {
                data
            };
            self.started = true;
            let c = self.ctx.0;
            if c.is_none() {
                return Err("the AV1 decoder is closed".into());
            }
            // SAFETY: follows the dav1d C API contract: `d` is created, sent and unreferenced here;
            // pictures are read before they are unreferenced.
            unsafe {
                let mut d = Dav1dData::default();
                let p = dav1d_data_create(NonNull::new(&mut d), data.len());
                if p.is_null() {
                    return Err("the AV1 decoder is out of memory".into());
                }
                std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                let mut out = None;
                let mut failure = None;
                // A unit goes in whole, and the decoder wants its pictures taken between pieces.
                for _ in 0..64 {
                    if d.sz > 0 {
                        let r = dav1d_send_data(c, NonNull::new(&mut d));
                        if r.0 < 0 && r.0 != EAGAIN {
                            failure = Some(format!("the AV1 data is damaged (error {})", r.0));
                            break;
                        }
                    }
                    let mut got_one = false;
                    loop {
                        let mut pic = Dav1dPicture::default();
                        let r = dav1d_get_picture(c, NonNull::new(&mut pic));
                        if r.0 == 0 {
                            got_one = true;
                            match copy_picture(&pic) {
                                Ok(y) => out = Some(y),
                                Err(e) => failure = Some(e),
                            }
                            dav1d_picture_unref(NonNull::new(&mut pic));
                        } else {
                            if r.0 != EAGAIN {
                                failure.get_or_insert(format!("the AV1 data is damaged (error {})", r.0));
                            }
                            break;
                        }
                    }
                    if failure.is_some() || d.sz == 0 || (!got_one && out.is_some()) {
                        break;
                    }
                }
                dav1d_data_unref(NonNull::new(&mut d));
                match failure {
                    Some(e) => Err(e),
                    None => Ok(out),
                }
            }
        }

        /// Takes pictures the decoder still holds after the last unit.
        pub fn drain(&mut self) -> Result<Option<Yuv>, String> {
            let c = self.ctx.0;
            if c.is_none() {
                return Err("the AV1 decoder is closed".into());
            }
            // SAFETY: as in `decode`.
            unsafe {
                let mut pic = Dav1dPicture::default();
                let r = dav1d_get_picture(c, NonNull::new(&mut pic));
                if r.0 == 0 {
                    let y = copy_picture(&pic);
                    dav1d_picture_unref(NonNull::new(&mut pic));
                    return y.map(Some);
                }
                Ok(None)
            }
        }
    }

    /// Copies a decoded picture out of the decoder's buffers into planes of
    /// `u16`, and reads the colour description from its sequence header.
    unsafe fn copy_picture(pic: &Dav1dPicture) -> Result<Yuv, String> {
        let (w, h, bpc) = (pic.p.w as usize, pic.p.h as usize, pic.p.bpc as usize);
        let layout = match pic.p.layout {
            0 => Layout::I400,
            1 => Layout::I420,
            2 => Layout::I422,
            _ => Layout::I444,
        };
        if w == 0 || h == 0 || !matches!(bpc, 8 | 10 | 12) {
            return Err(format!("{bpc}-bit pictures of {w} x {h} aren't supported"));
        }
        let bytes = if bpc > 8 { 2 } else { 1 };
        let (cw, ch) = Yuv::chroma_size(w as u32, h as u32, layout);
        let planes: [(usize, usize, usize, usize); 3] =
            [(0, w, h, 0), (1, cw as usize, ch as usize, 1), (1, cw as usize, ch as usize, 2)];
        let mut out: [Vec<u16>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (slot, &(stride_at, pw, ph, data_at)) in planes.iter().enumerate() {
            if layout == Layout::I400 && slot > 0 {
                break;
            }
            let base = pic.data[data_at].ok_or("the decoder gave no picture data")?.as_ptr() as *const u8;
            let stride = pic.stride[stride_at];
            let mut plane = Vec::with_capacity(pw * ph);
            for y in 0..ph {
                // SAFETY: rav1d guarantees `stride` bytes per row for `ph` rows from `base`; a row
                // holds `pw` samples of `bytes` bytes.
                let row = unsafe { std::slice::from_raw_parts(base.offset(y as isize * stride), pw * bytes) };
                if bytes == 1 {
                    plane.extend(row.iter().map(|&v| v as u16));
                } else {
                    plane.extend(row.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])));
                }
            }
            out[slot] = plane;
        }
        let [y, u, v] = out;
        let cicp = match pic.seq_hdr {
            // SAFETY: the sequence header lives as long as the picture.
            Some(sh) => {
                let sh = unsafe { sh.as_ref() };
                Cicp {
                    primaries: sh.pri as u8,
                    transfer: sh.trc as u8,
                    matrix: sh.mtrx as u8,
                    full_range: sh.color_range != 0,
                }
            }
            None => Cicp::UNSPECIFIED,
        };
        Ok(Yuv { width: w as u32, height: h as u32, bit_depth: bpc as u8, layout, y, u, v, cicp })
    }
}

#[cfg(feature = "avif")]
pub use imp::Stream;

#[cfg(not(feature = "avif"))]
pub struct Stream;

#[cfg(not(feature = "avif"))]
impl Stream {
    pub fn new(_config: &[u8], _max_pixels: u64, _operating_point: u8) -> Result<Stream, String> {
        Err(missing())
    }

    pub fn decode(&mut self, _data: &[u8]) -> Result<Option<Yuv>, String> {
        Err(missing())
    }

    pub fn drain(&mut self) -> Result<Option<Yuv>, String> {
        Err(missing())
    }
}

/// What to say when this build leaves AVIF out.
#[cfg(not(feature = "avif"))]
pub fn missing() -> String {
    "AVIF support was left out of this build of Slerp (the `avif` Cargo feature)".into()
}

/// Decodes a still picture: one item's data.
pub fn decode_still(config: &[u8], data: &[u8], max_pixels: u64, operating_point: u8) -> Result<Yuv, String> {
    let mut stream = Stream::new(config, max_pixels, operating_point)?;
    if let Some(y) = stream.decode(data)? {
        return Ok(y);
    }
    stream.drain()?.ok_or_else(|| "the AV1 data holds no picture".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_sequence_header() {
        // A temporal delimiter (type 2, sized, empty), then a sequence header (type 1) of 3 bytes.
        assert!(has_sequence_header(&[0x12, 0x00, 0x0a, 0x03, 1, 2, 3]));
        // A frame (type 6) first, no header.
        assert!(!has_sequence_header(&[0x32, 0x02, 9, 9]));
        // The extension bit adds a byte; the unit before it is empty.
        assert!(has_sequence_header(&[0x16, 0x00, 0x00, 0x0a, 0x03, 1, 2, 3]));
        // A two-byte leb128 size is stepped over.
        let mut long = vec![0x32, 0x80, 0x01];
        long.extend(vec![0u8; 128]);
        long.extend([0x0a, 0x01, 7]);
        assert!(has_sequence_header(&long));
        assert!(!has_sequence_header(&[]));
        // Garbage sizes never run past the data.
        assert!(!has_sequence_header(&[0x32, 0xff, 0xff, 0xff, 0xff, 0x7f]));
    }
}
