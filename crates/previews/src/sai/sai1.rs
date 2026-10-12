//! SAI 1's encrypted page file system and its `thumbnail` file.

use super::USER_KEY;
use crate::source::Source;
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{Limits, LoadError};

const PAGE: u64 = 4096;
/// A table page every this many pages.
const SPAN: u64 = 512;
/// Pages followed for one file; a thumbnail takes a few dozen.
const MAX_CHAIN: usize = 1 << 16;
/// The largest thumbnail believed.
const MAX_SIDE: u32 = 4096;

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("SAI document: {what}"))
}

fn key_sum(v: u32) -> u32 {
    USER_KEY[(v >> 24) as usize & 255]
        .wrapping_add(USER_KEY[(v >> 16) as usize & 255])
        .wrapping_add(USER_KEY[(v >> 8) as usize & 255])
        .wrapping_add(USER_KEY[v as usize & 255])
}

fn words(page: &[u8]) -> Vec<u32> {
    page.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect()
}

/// Decrypts a table page; `index` is its page number.
fn decrypt_table(index: u32, data: &mut [u32]) {
    let mut prev = index & !0x1FF;
    for w in data.iter_mut() {
        let cipher = *w;
        let x = prev ^ cipher ^ key_sum(prev);
        *w = x.rotate_left(16);
        prev = cipher;
    }
}

/// Decrypts a data page with the checksum its table gives it.
fn decrypt_data(mut vector: u32, data: &mut [u32]) {
    for w in data.iter_mut() {
        let cipher = *w;
        *w = cipher.wrapping_sub(vector ^ key_sum(vector));
        vector = cipher;
    }
}

/// SAI's page checksum: rotate left and exclusive-or every word, then set
/// the lowest bit.
fn checksum(data: &[u32]) -> u32 {
    data.iter().fold(0u32, |sum, &w| sum.rotate_left(1) ^ w) | 1
}

/// Whether the first page decrypts to a table whose own checksum (its first
/// word, over the rest) adds up: a 32-bit check no other file passes by
/// chance.
pub fn looks_like_sai1(head: &[u8]) -> bool {
    let Some(page) = head.get(..PAGE as usize) else { return false };
    let mut w = words(page);
    decrypt_table(0, &mut w);
    w[0] != 0 && checksum(&w[1..]) == w[0]
}

struct Sai<'a> {
    src: &'a mut Source,
    /// The table page last read, by number.
    table: Option<(u64, Vec<u32>)>,
}

impl Sai<'_> {
    /// The table entry (checksum, next page) of page `n`.
    fn entry(&mut self, n: u64) -> Result<(u32, u32), LoadError> {
        let t = n / SPAN * SPAN;
        if self.table.as_ref().is_none_or(|(i, _)| *i != t) {
            let mut w = words(&self.src.exact(t * PAGE, PAGE)?);
            decrypt_table(t as u32, &mut w);
            self.table = Some((t, w));
        }
        let w = &self.table.as_ref().expect("read above").1;
        let i = (n % SPAN) as usize * 2;
        Ok((w[i], w[i + 1]))
    }

    /// Data page `n`, decrypted and checked against its checksum.
    fn page(&mut self, n: u64) -> Result<Vec<u8>, LoadError> {
        if n.is_multiple_of(SPAN) {
            return Err(bad("a file runs into a table page"));
        }
        let (sum, _) = self.entry(n)?;
        let mut w = words(&self.src.exact(n * PAGE, PAGE)?);
        decrypt_data(sum, &mut w);
        if sum == 0 || checksum(&w) != sum {
            return Err(bad("a damaged page"));
        }
        Ok(w.iter().flat_map(|v| v.to_le_bytes()).collect())
    }

    /// The first `size` bytes of the file starting at page `first`,
    /// following the tables' chain.
    fn file(&mut self, first: u64, size: u64) -> Result<Vec<u8>, LoadError> {
        let mut out = Vec::with_capacity(size.min(64 << 20) as usize);
        let mut n = first;
        for _ in 0..MAX_CHAIN {
            let page = self.page(n)?;
            let take = (size - out.len() as u64).min(PAGE) as usize;
            out.extend_from_slice(&page[..take]);
            if out.len() as u64 >= size {
                return Ok(out);
            }
            let (_, next) = self.entry(n)?;
            if next == 0 {
                break;
            }
            n = next as u64;
        }
        Err(bad("a file ends before its size"))
    }
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    if src.len() < 3 * PAGE {
        return Err(bad("too short"));
    }
    let mut sai = Sai { src, table: None };
    // 1. The root folder: 64 entries of 64 bytes in page 2; a zero flags
    //    word ends the list.
    let root = sai.page(2)?;
    let entry = root
        .chunks_exact(64)
        .take_while(|e| e[..4] != [0; 4])
        .find(|e| {
            let name = &e[4..36];
            let end = name.iter().position(|&b| b == 0).unwrap_or(32);
            &name[..end] == b"thumbnail" && e[38] == 0x80
        })
        .ok_or_else(|| crate::no_preview("SAI document"))?;
    let first = u32::from_le_bytes([entry[40], entry[41], entry[42], entry[43]]) as u64;
    let size = u32::from_le_bytes([entry[44], entry[45], entry[46], entry[47]]) as u64;
    // 2. The thumbnail: width, height, `BM32`, BGRA pixels.
    let head = sai.file(first, 12.min(size))?;
    if head.len() < 12 || &head[8..12] != b"23MB" && &head[8..12] != b"BM32" {
        return Err(bad("a thumbnail that isn't BM32"));
    }
    let w = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let h = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || 12 + w as u64 * h as u64 * 4 > size {
        return Err(bad("a thumbnail of an odd size"));
    }
    let bytes = sai.file(first, 12 + w as u64 * h as u64 * 4)?;
    let mut rgba = bytes[12..].to_vec();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let mut t = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    if let Some(n) = limits.max_texture_size
        && w.max(h) > n
    {
        let small = slerp_scene::textures::thumbnail(&t, n);
        t = Texture::from_rgba8(small.width, small.height, small.pixels, ColorSpace::Srgb);
    }
    t.source.format = "BGRA".into();
    t.source
        .header
        .push(("Shown".into(), "The thumbnail SAI saved in the file (SAI 1 keeps no merged picture)".into()));
    Ok(t)
}
