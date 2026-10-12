//! Lookup tables: the Modality LUT and VOI LUT sequences, which map values
//! through a table instead of a formula, and palettes for PALETTE COLOR
//! images, including segmented ones.

use crate::data::{DataSet, Tag, Vr, tags, words};

/// A Modality or VOI LUT: entries for input values `first`,
/// `first + 1`, ...; inputs beyond either end take the end entries.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    pub first: i64,
    /// Bits per entry as the descriptor says.
    pub bits: u32,
    pub data: Vec<u16>,
    /// What a full-scale entry is, for VOI LUTs: `2^bits - 1`, or less when
    /// the entries never come close (descriptors that overstate bits).
    pub full_scale: f64,
    pub explanation: String,
}

/// Reads a descriptor's three values: entries (0 means 65536), first
/// input value, bits per entry.  `negative_first` says whether a large
/// first value means a negative one (descriptors are "US or SS").
fn descriptor(ds: &DataSet, tag: Tag, negative_first: bool) -> Option<(usize, i64, u32)> {
    let e = ds.get(tag)?;
    let w = ds.words(tag)?;
    if w.len() < 3 {
        return None;
    }
    let n = if w[0] == 0 { 65536 } else { w[0] as usize };
    let first = if e.vr == Vr::SS || (negative_first && w[1] > 32767) { w[1] as i16 as i64 } else { w[1] as i64 };
    let bits = (w[2] as u32).clamp(1, 16);
    Some((n, first, bits))
}

/// Table entries: bytes when 8-bit entries are packed two per word,
/// words otherwise.
fn entries(ds: &DataSet, tag: Tag, n: usize, bits: u32) -> Option<Vec<u16>> {
    let raw = ds.bytes(tag)?;
    let mut v: Vec<u16> = if bits <= 8 && raw.len() < 2 * n {
        raw.iter().map(|&b| b as u16).collect()
    } else {
        let w = words(raw, ds.big_endian);
        if bits <= 8 && w.iter().all(|&x| x & 0xFF == 0) && w.iter().any(|&x| x != 0) {
            // 8-bit entries in the high bytes.
            w.into_iter().map(|x| x >> 8).collect()
        } else {
            w
        }
    };
    v.truncate(n);
    (!v.is_empty()).then_some(v)
}

impl Lut {
    /// A LUT from a Modality LUT or VOI LUT Sequence item.
    pub fn from_item(item: &DataSet, negative_first: bool) -> Option<Lut> {
        let (n, first, bits) = descriptor(item, tags::LUT_DESCRIPTOR, negative_first)?;
        let data = entries(item, tags::LUT_DATA, n, bits)?;
        let max = data.iter().copied().max().unwrap_or(0) as f64;
        let nominal = ((1u64 << bits) - 1) as f64;
        // Some writers say 16 bits for 12-bit entries; a picture that
        // would never get brighter than a sixteenth of full scale is that.
        let full_scale =
            if max > 0.0 && max * 16.0 < nominal { 2f64.powf((max + 1.0).log2().ceil()) - 1.0 } else { nominal };
        let explanation = item.string(crate::data::tag(0x0028, 0x3003)).unwrap_or_default();
        Some(Lut { first, bits, data, full_scale, explanation })
    }

    #[inline]
    pub fn lookup(&self, x: f64) -> u16 {
        let i = (x.round() - self.first as f64).clamp(0.0, (self.data.len() - 1) as f64) as usize;
        self.data[i]
    }
}

/// A colour palette: 8-bit RGB entries for indices from `first`.
#[derive(Clone, Debug)]
pub struct Palette {
    pub first: i64,
    pub entries: Vec<[u8; 3]>,
}

impl Palette {
    /// Reads the red, green and blue palette tables, plain or segmented.
    pub fn read(ds: &DataSet) -> Option<Palette> {
        let signed = ds.u32(tags::PIXEL_REPRESENTATION) == Some(1);
        let channel = |desc: Tag, data: Tag, segmented: Tag| -> Option<(i64, u32, Vec<u16>)> {
            let (n, first, bits) = descriptor(ds, desc, signed)?;
            let v = match entries(ds, data, n, bits) {
                Some(v) => v,
                None => expand_segmented(&ds.words(segmented)?, n)?,
            };
            Some((first, bits, v))
        };
        let (first, bits, r) =
            channel(tags::RED_PALETTE_DESCRIPTOR, tags::RED_PALETTE_DATA, tags::RED_SEGMENTED_PALETTE)?;
        let (_, _, g) =
            channel(tags::GREEN_PALETTE_DESCRIPTOR, tags::GREEN_PALETTE_DATA, tags::GREEN_SEGMENTED_PALETTE)?;
        let (_, _, b) = channel(tags::BLUE_PALETTE_DESCRIPTOR, tags::BLUE_PALETTE_DATA, tags::BLUE_SEGMENTED_PALETTE)?;
        let n = r.len().min(g.len()).min(b.len());
        // 16-bit entries become 8 bits, unless every entry already fits in
        // 8 (writers that store 8-bit values in 16-bit entries).
        let max = r[..n].iter().chain(&g[..n]).chain(&b[..n]).copied().max().unwrap_or(0);
        let shift = if bits > 8 && max > 255 { bits - 8 } else { 0 };
        let entries = (0..n).map(|i| [(r[i] >> shift) as u8, (g[i] >> shift) as u8, (b[i] >> shift) as u8]).collect();
        Some(Palette { first, entries })
    }

    #[inline]
    pub fn lookup(&self, index: i64) -> [u8; 3] {
        let i = (index - self.first).clamp(0, self.entries.len().saturating_sub(1) as i64) as usize;
        self.entries.get(i).copied().unwrap_or([0, 0, 0])
    }
}

/// Expands segmented palette data (PS3.3 C.7.9.2): discrete runs, linear
/// ramps and indirect references to earlier segments, to at most `n`
/// entries.
pub fn expand_segmented(words: &[u16], n: usize) -> Option<Vec<u16>> {
    let mut out: Vec<u16> = Vec::with_capacity(n.min(65536));
    expand_range(words, 0, words.len(), n, &mut out, 0);
    (!out.is_empty()).then_some(out)
}

fn expand_range(words: &[u16], start: usize, end: usize, n: usize, out: &mut Vec<u16>, depth: u32) {
    let mut i = start;
    while i + 1 < end && out.len() < n {
        let (op, len) = (words[i], words[i + 1] as usize);
        match op {
            0 => {
                let values = words.get(i + 2..(i + 2 + len).min(end)).unwrap_or(&[]);
                out.extend(values.iter().take(n - out.len()));
                i += 2 + len;
            }
            1 => {
                let Some(&y1) = words.get(i + 2) else { return };
                let y0 = out.last().copied().unwrap_or(y1) as f64;
                for k in 1..=len.min(n - out.len()) {
                    out.push((y0 + (y1 as f64 - y0) * k as f64 / len as f64).round() as u16);
                }
                i += 3;
            }
            2 if depth == 0 => {
                // Replays `len` segments starting at a word offset.
                let (Some(&lo), Some(&hi)) = (words.get(i + 2), words.get(i + 3)) else { return };
                let at = (lo as usize) | (hi as usize) << 16;
                let mut j = at;
                for _ in 0..len {
                    let Some(&op2) = words.get(j) else { break };
                    let Some(&l2) = words.get(j + 1) else { break };
                    let size = match op2 {
                        0 => 2 + l2 as usize,
                        1 => 3,
                        _ => break,
                    };
                    expand_range(words, j, (j + size).min(words.len()), n, out, depth + 1);
                    j += size;
                }
                i += 4;
            }
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmented_palettes() {
        // Discrete 0, 10; ramp to 30 over 2; indirect replay of the first
        // segment.
        let words = [0, 2, 0, 10, 1, 2, 30, 2, 1, 0, 0];
        assert_eq!(expand_segmented(&words, 100), Some(vec![0, 10, 20, 30, 0, 10]));
        assert_eq!(expand_segmented(&words, 3), Some(vec![0, 10, 20]));
        // Nonsense stops without panicking.
        assert_eq!(expand_segmented(&[7, 7, 7], 10), None);
        assert_eq!(expand_segmented(&[0, 60000, 1], 10), Some(vec![1]));
        assert_eq!(expand_segmented(&[2, 5, 0xFFFF, 0xFFFF], 10), None);
    }
}
