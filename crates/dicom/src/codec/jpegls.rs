//! A JPEG-LS decoder (ITU-T T.87 / ISO 14495-1): lossless and
//! near-lossless, 2 to 16 bits, one to four components in any of the three
//! interleave modes, preset coding parameters and restart markers.  The
//! context modelling follows the standard's annex A; mapping tables (a
//! palette extension nobody uses in DICOM) aren't supported.

use super::{Decoded, Samples};
use crate::image::MAX_FRAME_SAMPLES as MAX_SAMPLES;

const J: [u32; 32] =
    [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/*┌─────────────────╮
| Bit reader      |
└─────────────────╯*/
//region Bit reader

/// Reads entropy-coded bits.  After a 0xFF byte the next byte carries only
/// seven bits (its top bit is a stuffed zero); 0xFF followed by a byte
/// with the top bit set is a marker, which ends the data.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    count: u32,
    last_ff: bool,
    /// Past the scan's data: zeros are supplied, and decoding reports the
    /// overrun if it reads too many.
    overrun: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Bits { data, pos, acc: 0, count: 0, last_ff: false, overrun: 0 }
    }

    fn fill(&mut self) {
        while self.count <= 56 {
            let byte = if self.pos < self.data.len() {
                let b = self.data[self.pos];
                if self.last_ff && b & 0x80 != 0 {
                    // A marker: leave it for the caller.
                    self.pos -= 1;
                    self.last_ff = false;
                    self.data = &self.data[..self.pos];
                    continue;
                }
                self.pos += 1;
                Some(b)
            } else {
                None
            };
            match byte {
                Some(b) => {
                    let bits = if self.last_ff { 7 } else { 8 };
                    self.acc |= ((b as u64) & ((1 << bits) - 1)) << (64 - bits - self.count);
                    self.count += bits;
                    self.last_ff = b == 0xFF;
                }
                None => {
                    self.overrun += 1;
                    self.count += 8;
                }
            }
        }
    }

    fn bit(&mut self) -> u32 {
        if self.count == 0 {
            self.fill();
        }
        let b = (self.acc >> 63) as u32;
        self.acc <<= 1;
        self.count -= 1;
        b
    }

    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.count < n {
            self.fill();
        }
        let v = (self.acc >> (64 - n)) as u32;
        self.acc <<= n;
        self.count -= n;
        v
    }

    /// Counts zero bits up to the next one bit, which is consumed.  Stops
    /// at `max` zeros (an invalid code), returning `max`.
    fn zeros(&mut self, max: u32) -> u32 {
        let mut n = 0;
        loop {
            if self.count == 0 {
                self.fill();
            }
            let lz = (self.acc.leading_zeros()).min(self.count);
            if lz < self.count {
                n += lz;
                self.acc = self.acc.checked_shl(lz + 1).unwrap_or(0);
                self.count -= lz + 1;
                return n.min(max);
            }
            n += self.count;
            self.acc = 0;
            self.count = 0;
            if n >= max || self.past_end() > 64 {
                return max;
            }
        }
    }

    /// Bits decoding has used beyond the end of the data (zeros made up).
    fn past_end(&self) -> i64 {
        self.overrun as i64 * 8 - self.count as i64
    }

    /// Byte position after the data read so far, for finding the next
    /// marker (restarts).
    fn byte_pos(&self) -> usize {
        self.pos
    }
}

//endregion

/*┌─────────────────╮
| Parameters      |
└─────────────────╯*/
//region Parameters

#[derive(Clone, Copy, Debug)]
struct Params {
    maxval: i32,
    t1: i32,
    t2: i32,
    t3: i32,
    reset: i32,
}

fn clamp_t(v: i32, lo: i32, hi: i32) -> i32 {
    if v > hi || v < lo { lo } else { v }
}

/// The default thresholds of T.87 C.2.4.1.1.
fn default_params(maxval: i32, near: i32) -> Params {
    let (t1, t2, t3) = if maxval >= 128 {
        let factor = (maxval.min(4095) + 128) / 256;
        let t1 = clamp_t(factor + 2 + 3 * near, near + 1, maxval);
        let t2 = clamp_t(factor * (7 - 3) + 3 + 5 * near, t1, maxval);
        let t3 = clamp_t(factor * (21 - 4) + 4 + 7 * near, t2, maxval);
        (t1, t2, t3)
    } else {
        let factor = 256 / (maxval + 1);
        let t1 = clamp_t((3 / factor + 3 * near).max(2), near + 1, maxval);
        let t2 = clamp_t((7 / factor + 5 * near).max(3), t1, maxval);
        let t3 = clamp_t((21 / factor + 7 * near).max(4), t2, maxval);
        (t1, t2, t3)
    };
    Params { maxval, t1, t2, t3, reset: 64 }
}

//endregion

/*┌─────────────────╮
| Markers         |
└─────────────────╯*/
//region Markers

struct Frame {
    precision: u32,
    height: u32,
    width: u32,
    /// Component identifiers in frame order.
    ids: Vec<u8>,
}

/// Decodes a JPEG-LS stream.
pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err("not a JPEG-LS stream".into());
    }
    let mut pos = 2;
    let mut frame: Option<Frame> = None;
    let mut preset: Option<(i32, i32, i32, i32, i32)> = None;
    let mut restart = 0u32;
    let mut planes: Vec<Vec<u16>> = Vec::new();
    let mut ilv_used = 0;
    let mut scans = 0;
    loop {
        // Find the next marker, skipping fill bytes.
        while pos < data.len() && data[pos] != 0xFF {
            pos += 1;
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = data.get(pos) else { break };
        pos += 1;
        if marker < 0xC0 {
            // A stray 0xFF in leftover scan bytes, not a marker.
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        if (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let len =
            data.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or("truncated marker")?;
        let seg = data.get(pos + 2..pos + len).ok_or("truncated marker segment")?;
        if len < 2 {
            return Err("bad marker length".into());
        }
        match marker {
            0xF7 => {
                // SOF55: precision, lines, columns, components.
                if seg.len() < 6 {
                    return Err("short SOF55".into());
                }
                let precision = seg[0] as u32;
                let height = u16::from_be_bytes([seg[1], seg[2]]) as u32;
                let width = u16::from_be_bytes([seg[3], seg[4]]) as u32;
                let n = seg[5] as usize;
                if !(2..=16).contains(&precision) || n == 0 || n > 4 || seg.len() < 6 + 3 * n {
                    return Err(format!("unsupported JPEG-LS frame ({precision} bits, {n} components)"));
                }
                if width == 0 || height == 0 {
                    return Err("JPEG-LS frame without a size".into());
                }
                if (width as u64) * (height as u64) * (n as u64) > MAX_SAMPLES {
                    return Err("JPEG-LS frame too large".into());
                }
                let ids = (0..n).map(|i| seg[6 + 3 * i]).collect();
                planes = vec![vec![0u16; (width * height) as usize]; n];
                frame = Some(Frame { precision, height, width, ids });
            }
            0xF8 => {
                // LSE: preset parameters (ID 1); mapping tables (2, 3) and
                // oversize dimensions (4) aren't supported.
                match seg.first() {
                    Some(1) if seg.len() >= 11 => {
                        let r = |i: usize| u16::from_be_bytes([seg[i], seg[i + 1]]) as i32;
                        preset = Some((r(1), r(3), r(5), r(7), r(9)));
                    }
                    Some(id) => return Err(format!("unsupported JPEG-LS extension {id}")),
                    None => return Err("empty LSE segment".into()),
                }
            }
            0xDD => {
                if seg.len() >= 2 {
                    restart = u16::from_be_bytes([seg[0], seg[1]]) as u32;
                }
            }
            0xDA => {
                let f = frame.as_ref().ok_or("scan before the frame header")?;
                let ns = *seg.first().ok_or("empty SOS")? as usize;
                if ns == 0 || ns > 4 || seg.len() < 1 + 2 * ns + 3 {
                    return Err("bad JPEG-LS scan header".into());
                }
                let mut comps = Vec::with_capacity(ns);
                for i in 0..ns {
                    let id = seg[1 + 2 * i];
                    if seg[2 + 2 * i] != 0 {
                        return Err("JPEG-LS mapping tables aren't supported".into());
                    }
                    comps.push(f.ids.iter().position(|&c| c == id).ok_or("scan names an unknown component")?);
                }
                let near = seg[1 + 2 * ns] as i32;
                let ilv = seg[2 + 2 * ns];
                let pt = (seg[3 + 2 * ns] & 0x0F) as u32;
                if ilv > 2 || (ilv == 0 && ns != 1) {
                    return Err(format!("bad JPEG-LS interleave mode {ilv}"));
                }
                let maxval_default = ((1u32 << f.precision) - 1) as i32;
                let mut params = default_params(maxval_default, near);
                if let Some((maxval, t1, t2, t3, reset)) = preset {
                    let maxval = if maxval > 0 { maxval } else { maxval_default };
                    let d = default_params(maxval, near);
                    params = Params {
                        maxval,
                        t1: if t1 > 0 { t1 } else { d.t1 },
                        t2: if t2 > 0 { t2 } else { d.t2 },
                        t3: if t3 > 0 { t3 } else { d.t3 },
                        reset: if reset > 0 { reset } else { 64 },
                    };
                }
                if near < 0 || near > params.maxval.min(255) / 2 || params.t1 > params.t2 || params.t2 > params.t3 {
                    return Err("bad JPEG-LS parameters".into());
                }
                let start = pos + len;
                let mut scan = Scan::new(params, near, f.width as usize, ns, ilv);
                let end = scan.decode(data, start, f.height as usize, restart, &comps, &mut planes)?;
                if pt > 0 {
                    for &c in &comps {
                        for v in &mut planes[c] {
                            *v = (((*v as u32) << pt).min(65535)) as u16;
                        }
                    }
                }
                ilv_used = ilv;
                scans += 1;
                pos = end;
                continue;
            }
            _ => {}
        }
        pos += len;
    }
    let f = frame.ok_or("no JPEG-LS frame header")?;
    if scans == 0 {
        return Err("no JPEG-LS scan".into());
    }
    let _ = ilv_used;
    let n = planes.len();
    let pixels = (f.width * f.height) as usize;
    let data = if n == 1 {
        planes.pop().unwrap_or_default()
    } else {
        let mut v = vec![0u16; pixels * n];
        for (c, plane) in planes.iter().enumerate() {
            for (i, &s) in plane.iter().enumerate() {
                v[i * n + c] = s;
            }
        }
        v
    };
    let data =
        if f.precision <= 8 { Samples::U8(data.into_iter().map(|v| v as u8).collect()) } else { Samples::U16(data) };
    Ok(Decoded { width: f.width, height: f.height, comps: n as u32, precision: f.precision, data, rgb: false })
}

//endregion

/*┌─────────────────╮
| Scan decoding   |
└─────────────────╯*/
//region Scan decoding

struct Scan {
    p: Params,
    near: i32,
    range: i32,
    qbpp: u32,
    limit: u32,
    width: usize,
    ns: usize,
    ilv: u8,
    a: [i32; 367],
    b: [i32; 365],
    c: [i32; 365],
    n: [i32; 367],
    nn: [i32; 2],
    run_index: [usize; 4],
}

impl Scan {
    fn new(p: Params, near: i32, width: usize, ns: usize, ilv: u8) -> Scan {
        let range = (p.maxval + 2 * near) / (2 * near + 1) + 1;
        let qbpp = 32 - (range as u32 - 1).leading_zeros();
        let bpp = (32 - (p.maxval as u32).leading_zeros()).max(2);
        let limit = 2 * (bpp + bpp.max(8));
        let mut s = Scan {
            p,
            near,
            range,
            qbpp,
            limit,
            width,
            ns,
            ilv,
            a: [0; 367],
            b: [0; 365],
            c: [0; 365],
            n: [0; 367],
            nn: [0; 2],
            run_index: [0; 4],
        };
        s.reset_contexts();
        s
    }

    fn reset_contexts(&mut self) {
        let a = ((self.range + 32) >> 6).max(2);
        self.a = [a; 367];
        self.n = [1; 367];
        self.b = [0; 365];
        self.c = [0; 365];
        self.nn = [0; 2];
        self.run_index = [0; 4];
    }

    fn quantize(&self, d: i32) -> i32 {
        let (near, p) = (self.near, &self.p);
        if d <= -p.t3 {
            -4
        } else if d <= -p.t2 {
            -3
        } else if d <= -p.t1 {
            -2
        } else if d < -near {
            -1
        } else if d <= near {
            0
        } else if d < p.t1 {
            1
        } else if d < p.t2 {
            2
        } else if d < p.t3 {
            3
        } else {
            4
        }
    }

    /// Brings a reconstructed value back into range (T.87 A.4.5).
    fn fix(&self, mut x: i32) -> i32 {
        let span = self.range * (2 * self.near + 1);
        if x < -self.near {
            x += span;
        } else if x > self.p.maxval + self.near {
            x -= span;
        }
        x.clamp(0, self.p.maxval)
    }

    /// Reads a Golomb-coded mapped error value with the code length limit.
    fn golomb(&self, bits: &mut Bits, k: u32, limit: u32) -> Result<i32, String> {
        let max_prefix = limit.saturating_sub(self.qbpp + 1);
        let prefix = bits.zeros(max_prefix + 1);
        if prefix < max_prefix {
            Ok(((prefix << k) | bits.read(k)) as i32)
        } else if prefix == max_prefix {
            Ok(bits.read(self.qbpp) as i32 + 1)
        } else {
            Err("bad JPEG-LS code".into())
        }
    }

    /// Decodes a sample in regular mode.
    fn regular(&mut self, bits: &mut Bits, qs: i32, pred: i32) -> Result<i32, String> {
        let sign = if qs < 0 { -1 } else { 1 };
        let q = (qs * sign) as usize;
        let mut k = 0;
        while (self.n[q] << k) < self.a[q] && k < 24 {
            k += 1;
        }
        let px = (pred + sign * self.c[q]).clamp(0, self.p.maxval);
        let m = self.golomb(bits, k, self.limit)?;
        let mut err = if m & 1 == 0 { m >> 1 } else { -((m + 1) >> 1) };
        if self.near == 0 && k == 0 && 2 * self.b[q] <= -self.n[q] {
            err = -err - 1;
        }
        // A coder keeps errors within half the range; damaged data can
        // claim any, which the context sums below would overflow on.
        if err.abs() > self.range {
            return Err("bad JPEG-LS code".into());
        }
        // Context update (A.6).  A sums up to RESET errors, and RESET may be
        // as large as the range.
        self.b[q] += err * (2 * self.near + 1);
        self.a[q] = self.a[q].saturating_add(err.abs());
        if self.n[q] == self.p.reset {
            self.a[q] >>= 1;
            self.b[q] >>= 1;
            self.n[q] >>= 1;
        }
        self.n[q] += 1;
        if self.b[q] <= -self.n[q] {
            self.b[q] += self.n[q];
            if self.c[q] > -128 {
                self.c[q] -= 1;
            }
            if self.b[q] <= -self.n[q] {
                self.b[q] = -self.n[q] + 1;
            }
        } else if self.b[q] > 0 {
            self.b[q] -= self.n[q];
            if self.c[q] < 127 {
                self.c[q] += 1;
            }
            if self.b[q] > 0 {
                self.b[q] = 0;
            }
        }
        Ok(self.fix(px + sign * err * (2 * self.near + 1)))
    }

    /// Decodes the run-interruption sample (A.7.2).  Sample-interleaved
    /// scans code each component's interruption sample as if its
    /// neighbours differed (`always_type0`), as CharLS, the reference
    /// implementation, does.
    fn interruption(
        &mut self,
        bits: &mut Bits,
        ra: i32,
        rb: i32,
        ri: usize,
        always_type0: bool,
    ) -> Result<i32, String> {
        let rtype = (!always_type0 && (ra - rb).abs() <= self.near) as usize;
        let q = 365 + rtype;
        let temp = if rtype == 0 { self.a[q] } else { self.a[q].saturating_add(self.n[q] >> 1) };
        let mut k = 0;
        while (self.n[q] << k) < temp && k < 24 {
            k += 1;
        }
        let limit = self.limit.saturating_sub(J[self.run_index[ri]] + 1);
        let em = self.golomb(bits, k, limit)?;
        let t = em + rtype as i32;
        let map = t & 1;
        let abs = (t + map) / 2;
        let err = if (k != 0 || 2 * self.nn[rtype] >= self.n[q]) == (map == 1) { -abs } else { abs };
        if abs > self.range {
            return Err("bad JPEG-LS code".into());
        }
        if err < 0 {
            self.nn[rtype] += 1;
        }
        self.a[q] = self.a[q].saturating_add((em + 1 - rtype as i32) >> 1);
        if self.n[q] == self.p.reset {
            self.a[q] >>= 1;
            self.n[q] >>= 1;
            self.nn[rtype] >>= 1;
        }
        self.n[q] += 1;
        let (px, sign) = if rtype == 1 { (ra, 1) } else { (rb, if rb < ra { -1 } else { 1 }) };
        Ok(self.fix(px + sign * err * (2 * self.near + 1)))
    }

    /// Decodes a run in single-component lines: returns how many samples
    /// repeat `ra` from `x`.
    fn run_length(&mut self, bits: &mut Bits, x: usize, ri: usize) -> Result<(usize, bool), String> {
        let left = self.width - x;
        let mut count = 0usize;
        while bits.bit() == 1 {
            let step = (1usize << J[self.run_index[ri]]).min(left - count);
            count += step;
            if step == 1 << J[self.run_index[ri]] && self.run_index[ri] < 31 {
                self.run_index[ri] += 1;
            }
            if count == left {
                return Ok((count, true));
            }
            if bits.past_end() > 64 {
                return Err("JPEG-LS data ends early".into());
            }
        }
        count += bits.read(J[self.run_index[ri]]) as usize;
        if count > left {
            return Err("JPEG-LS run past the end of the line".into());
        }
        Ok((count, false))
    }

    /// Decodes the scan into `planes` and returns the position after it.
    fn decode(
        &mut self,
        data: &[u8],
        start: usize,
        height: usize,
        restart: u32,
        comps: &[usize],
        planes: &mut [Vec<u16>],
    ) -> Result<usize, String> {
        let w = self.width;
        let ns = self.ns;
        // Lines with one sample of padding at each end, per component:
        // previous and current.
        let mut prev = vec![vec![0i32; w + 2]; ns];
        let mut cur = vec![vec![0i32; w + 2]; ns];
        let mut bits = Bits::new(data, start);
        let mut lines_since_restart = 0u32;
        for y in 0..height {
            if restart > 0 && lines_since_restart == restart {
                // A restart marker: realign to it, reset the contexts and
                // start the next interval as if at the top.
                let mut p = bits.byte_pos();
                while p + 1 < data.len() && !(data[p] == 0xFF && (0xD0..=0xD7).contains(&data[p + 1])) {
                    p += 1;
                }
                bits = Bits::new(data, (p + 2).min(data.len()));
                self.reset_contexts();
                lines_since_restart = 0;
                for line in prev.iter_mut() {
                    line.fill(0);
                }
            }
            lines_since_restart += 1;
            // The padding: left of a line is the first sample above it,
            // right of the line above is its last sample.
            for c in 0..ns {
                cur[c][0] = prev[c][1];
                prev[c][w + 1] = prev[c][w];
            }
            let line = match self.ilv {
                2 if ns > 1 => self.line_sample_interleaved(&mut bits, &prev, &mut cur),
                _ => (0..ns).try_for_each(|c| self.line(&mut bits, &prev[c], &mut cur[c], c)),
            };
            line.map_err(|e| format!("{e} (line {y} of {height})"))?;
            for c in 0..ns {
                let row = &mut planes[comps[c]][y * w..(y + 1) * w];
                for (o, &v) in row.iter_mut().zip(&cur[c][1..=w]) {
                    *o = v as u16;
                }
            }
            std::mem::swap(&mut prev, &mut cur);
            // Some encoders end the data a few bits early (a final 0xFF with
            // no stuffed bit after it), so a little slack is allowed.
            if bits.past_end() > 64 {
                return Err(format!("JPEG-LS data ends early (line {y} of {height})"));
            }
        }
        Ok(bits.byte_pos())
    }

    /// One line of one component (interleave modes 0 and 1).  `prev` and
    /// `cur` are padded by one sample on each side.
    fn line(&mut self, bits: &mut Bits, prev: &[i32], cur: &mut [i32], ri: usize) -> Result<(), String> {
        let w = self.width;
        let mut x = 0;
        while x < w {
            let i = x + 1;
            let (ra, rb, rc, rd) = (cur[i - 1], prev[i], prev[i - 1], prev[i + 1]);
            let (d1, d2, d3) = (rd - rb, rb - rc, rc - ra);
            if d1.abs() <= self.near && d2.abs() <= self.near && d3.abs() <= self.near {
                // Run mode.
                let (count, eol) = self.run_length(bits, x, ri)?;
                for v in &mut cur[i..i + count] {
                    *v = ra;
                }
                x += count;
                if eol || x >= w {
                    break;
                }
                let i = x + 1;
                cur[i] = self.interruption(bits, ra, prev[i], ri, false)?;
                self.run_index[ri] = self.run_index[ri].saturating_sub(1);
                x += 1;
                continue;
            }
            let qs = (self.quantize(d1) * 9 + self.quantize(d2)) * 9 + self.quantize(d3);
            cur[i] = self.regular(bits, qs, predict(ra, rb, rc))?;
            x += 1;
        }
        Ok(())
    }

    /// One line of every component, sample-interleaved (mode 2): run mode
    /// only when every component is flat.
    fn line_sample_interleaved(
        &mut self,
        bits: &mut Bits,
        prev: &[Vec<i32>],
        cur: &mut [Vec<i32>],
    ) -> Result<(), String> {
        let (w, ns) = (self.width, self.ns);
        let mut x = 0;
        while x < w {
            let i = x + 1;
            let mut qs = [0i32; 4];
            let mut flat = true;
            for c in 0..ns {
                let (ra, rb, rc, rd) = (cur[c][i - 1], prev[c][i], prev[c][i - 1], prev[c][i + 1]);
                let (d1, d2, d3) = (rd - rb, rb - rc, rc - ra);
                qs[c] = (self.quantize(d1) * 9 + self.quantize(d2)) * 9 + self.quantize(d3);
                flat &= qs[c] == 0;
            }
            if flat {
                let (count, eol) = self.run_length(bits, x, 0)?;
                for line in cur.iter_mut().take(ns) {
                    let ra = line[i - 1];
                    line[i..i + count].fill(ra);
                }
                x += count;
                if eol || x >= w {
                    break;
                }
                let i = x + 1;
                for c in 0..ns {
                    let (ra, rb) = (cur[c][i - 1], prev[c][i]);
                    cur[c][i] = self.interruption(bits, ra, rb, 0, true)?;
                }
                self.run_index[0] = self.run_index[0].saturating_sub(1);
                x += 1;
                continue;
            }
            for c in 0..ns {
                let (ra, rb, rc) = (cur[c][i - 1], prev[c][i], prev[c][i - 1]);
                cur[c][i] = self.regular(bits, qs[c], predict(ra, rb, rc))?;
            }
            x += 1;
        }
        Ok(())
    }
}

/// The median edge detector (A.4.1).
fn predict(ra: i32, rb: i32, rc: i32) -> i32 {
    if rc >= ra.max(rb) {
        ra.min(rb)
    } else if rc <= ra.min(rb) {
        ra.max(rb)
    } else {
        ra + rb - rc
    }
}

//endregion

#[cfg(test)]
mod tests {
    /// Found by fuzzing: a damaged stream whose Golomb codes claim errors
    /// far outside the sample range overflowed the context sums.
    #[test]
    fn huge_error_codes_are_refused() {
        let stream = [
            0xff, 0xd8, 0xff, 0xf7, 0x00, 0x0b, 0x10, 0x02, 0x00, 0x02, 0x00, 0x01, 0x01, 0x11, 0x00, 0xff, 0xda, 0x00,
            0x08, 0x01, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x62, 0x00, 0x09, 0x00, 0x00,
            0x62, 0x00, 0x00, 0x00, 0x00, 0xdd, 0xe0, 0x00, 0x00, 0x00, 0x62, 0x00, 0x00, 0x00, 0x74, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x6e, 0x09, 0x00, 0x00, 0x00, 0x00, 0x27, 0xff, 0x09, 0x00, 0x00, 0x00, 0x00, 0x62,
        ];
        let _ = super::decode(&stream);
    }
}
