//! Safe little-endian reads from a record's bytes.  A damaged record may be
//! shorter than its type promises, so every read past the end gives zero
//! instead of panicking, and every slice is clamped.

/// One metafile record: its function or type number and its bytes.
#[derive(Clone, Copy)]
pub struct Rec<'a> {
    pub func: u16,
    p: &'a [u8],
}

impl<'a> Rec<'a> {
    pub fn new(func: u16, p: &'a [u8]) -> Rec<'a> {
        Rec { func, p }
    }

    pub fn len(&self) -> usize {
        self.p.len()
    }

    pub fn u8(&self, o: usize) -> u8 {
        self.p.get(o).copied().unwrap_or(0)
    }

    pub fn u16(&self, o: usize) -> u16 {
        o.checked_add(2).and_then(|e| self.p.get(o..e)).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn i16(&self, o: usize) -> i16 {
        self.u16(o) as i16
    }

    pub fn u32(&self, o: usize) -> u32 {
        o.checked_add(4).and_then(|e| self.p.get(o..e)).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&self, o: usize) -> i32 {
        self.u32(o) as i32
    }

    pub fn f32(&self, o: usize) -> f32 {
        let v = f32::from_bits(self.u32(o));
        if v.is_finite() { v } else { 0.0 }
    }

    /// Up to `n` bytes from offset `o`.
    pub fn bytes(&self, o: usize, n: usize) -> &'a [u8] {
        let start = o.min(self.p.len());
        let end = o.saturating_add(n).min(self.p.len());
        &self.p[start..end]
    }

    /// Everything from offset `o` on.
    pub fn tail(&self, o: usize) -> &'a [u8] {
        &self.p[o.min(self.p.len())..]
    }
}
