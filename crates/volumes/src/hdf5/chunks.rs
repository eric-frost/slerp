//! Chunked datasets: finding the chunks through whichever index the file
//! uses, undoing the filters, and serving the array's rows from them.

use super::{Dataset, Filter, H5, UNDEF, le};
use crate::source::{ByteSource, Inflater, Wrapper, eof};
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

/// Most chunks an index may list.
const MAX_CHUNKS: u64 = 1 << 22;
/// Decoded chunks kept for reuse, in bytes.
const CACHE_BYTES: usize = 256 << 20;

/// How a chunked dataset's chunks are found.
#[derive(Clone, Debug)]
pub(crate) enum Index {
    BTreeV1(u64),
    Single {
        addr: u64,
        filtered: Option<(u64, u32)>,
    },
    Implicit(u64),
    Fixed {
        page_bits: u8,
        addr: u64,
    },
    /// Not read (see `enumerate`), but recognised so the message says why.
    Extensible,
    BTreeV2 {
        addr: u64,
    },
}

/// A dataset that was never written: all zeros.
pub(crate) struct Zeros(pub u64);

impl ByteSource for Zeros {
    fn len(&self) -> u64 {
        self.0
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset.checked_add(buf.len() as u64).is_none_or(|e| e > self.0) {
            return Err(eof());
        }
        buf.fill(0);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ChunkRef {
    /// Chunk position in elements along each axis.
    coord: Vec<u64>,
    addr: u64,
    size: u64,
    mask: u32,
}

/// The chunks of a dataset, served as its flat row-major array.
pub(crate) struct ChunkedSource {
    h: Arc<H5>,
    shape: Vec<u64>,
    chunk: Vec<u64>,
    esize: usize,
    filters: Vec<Filter>,
    refs: Vec<ChunkRef>,
    /// Chunk grid position (row-major index) to `refs`.
    lookup: HashMap<u64, usize>,
    grid: Vec<u64>,
    chunk_bytes: usize,
    row_bytes: u64,
    total: u64,
    cache: Mutex<Vec<(usize, Arc<Vec<u8>>)>>,
}

fn mul(a: u64, b: u64) -> Result<u64, String> {
    a.checked_mul(b).ok_or_else(|| "a size overflows".to_string())
}

impl ChunkedSource {
    pub(crate) fn new(h: Arc<H5>, d: &Dataset, chunk: Vec<u64>, index: Index) -> Result<ChunkedSource, String> {
        let shape = d.shape.clone();
        if chunk.len() != shape.len() || shape.is_empty() || chunk.contains(&0) {
            return Err("a chunked dataset's chunk shape is invalid".into());
        }
        let esize = d.elem_size;
        let mut cells = 1u64;
        for &c in &chunk {
            cells = mul(cells, c)?;
        }
        let chunk_bytes = mul(cells, esize as u64)?;
        if chunk_bytes > 1 << 30 {
            return Err("a chunk is too large".into());
        }
        let grid: Vec<u64> = shape.iter().zip(&chunk).map(|(s, c)| s.div_ceil(*c)).collect();
        let mut n_chunks = 1u64;
        for &g in &grid {
            n_chunks = mul(n_chunks, g.max(1))?;
        }
        if n_chunks > MAX_CHUNKS {
            return Err("a dataset has too many chunks".into());
        }
        let row_bytes =
            shape[1..].iter().try_fold(esize as u64, |a, &s| a.checked_mul(s)).ok_or("a dataset is absurdly large")?;
        let total = mul(row_bytes, shape[0])?;
        let mut me = ChunkedSource {
            h,
            shape,
            chunk,
            esize,
            filters: d.filters.clone(),
            refs: Vec::new(),
            lookup: HashMap::new(),
            grid,
            chunk_bytes: chunk_bytes as usize,
            row_bytes,
            total,
            cache: Mutex::new(Vec::new()),
        };
        me.refs = me.enumerate(&index, n_chunks)?;
        for (i, r) in me.refs.iter().enumerate() {
            // A chunk the index places outside the dataset is never asked
            // for (and its position would overflow the grid index).
            let cells = r.coord.iter().zip(&me.chunk).map(|(&c, &k)| c / k);
            if cells.clone().zip(&me.grid).any(|(cell, &g)| cell >= g.max(1)) {
                continue;
            }
            let lin = cells.zip(&me.grid).fold(0u64, |lin, (cell, &g)| lin * g.max(1) + cell);
            me.lookup.insert(lin, i);
        }
        Ok(me)
    }

    /// Lists the chunks the index knows.
    fn enumerate(&self, index: &Index, n_chunks: u64) -> Result<Vec<ChunkRef>, String> {
        let rank = self.shape.len();
        let zero = vec![0u64; rank];
        let out = match index {
            Index::Single { addr, filtered } => {
                if *addr == UNDEF {
                    Vec::new()
                } else {
                    let (size, mask) = filtered.unwrap_or((self.chunk_bytes as u64, 0));
                    vec![ChunkRef { coord: zero, addr: *addr, size, mask }]
                }
            }
            Index::Implicit(base) => {
                if *base == UNDEF {
                    Vec::new()
                } else {
                    (0..n_chunks).map(|i| ChunkRef { coord: self.coord_of(i), addr: base + i * self.chunk_bytes as u64, size: self.chunk_bytes as u64, mask: 0 }).collect()
                }
            }
            Index::BTreeV1(root) => self.btree_v1(*root)?,
            Index::Fixed { page_bits, addr } => self.fixed_array(*addr, *page_bits, n_chunks)?,
            Index::BTreeV2 { addr } => self.btree_v2(*addr)?,
            Index::Extensible => return Err("chunk indexes of the extensible array kind (datasets with one unlimited dimension in the newest file format) aren't read".into()),
        };
        Ok(out)
    }

    /// The position in elements of chunk number `i` of the grid.
    fn coord_of(&self, i: u64) -> Vec<u64> {
        let mut rest = i;
        let mut c = vec![0u64; self.shape.len()];
        for k in (0..self.shape.len()).rev() {
            let g = self.grid[k].max(1);
            c[k] = (rest % g) * self.chunk[k];
            rest /= g;
        }
        c
    }

    fn btree_v1(&self, root: u64) -> Result<Vec<ChunkRef>, String> {
        let h = &self.h;
        let nd = self.shape.len() + 1;
        let keysz = 8 + 8 * nd;
        let mut out = Vec::new();
        if root == UNDEF {
            return Ok(out);
        }
        let mut stack = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(a) = stack.pop() {
            if !seen.insert(a) || seen.len() > 1 << 20 {
                continue;
            }
            let head = h.rd(a, 8 + 2 * h.so)?;
            if &head[..4] != b"TREE" || head[4] != 1 {
                return Err("a chunk B-tree node is damaged".into());
            }
            let (level, used) = (head[5], le(&head, 6, 2) as usize);
            if used > 1 << 12 {
                return Err("a chunk B-tree node is too large".into());
            }
            let body = h.rd(a + (8 + 2 * h.so) as u64, used * (keysz + h.so) + keysz)?;
            for i in 0..used {
                let k = &body[i * (keysz + h.so)..];
                let child = h.off(&body, i * (keysz + h.so) + keysz);
                if level > 0 {
                    stack.push(child);
                } else {
                    let (size, mask) = (le(k, 0, 4), le(k, 4, 4) as u32);
                    let coord: Vec<u64> = (0..nd - 1).map(|d| le(k, 8 + 8 * d, 8)).collect();
                    if child != UNDEF {
                        out.push(ChunkRef { coord, addr: child, size, mask });
                    }
                }
                if out.len() as u64 > MAX_CHUNKS {
                    return Err("a dataset has too many chunks".into());
                }
            }
        }
        Ok(out)
    }

    fn fixed_array(&self, addr: u64, page_bits: u8, n_chunks: u64) -> Result<Vec<ChunkRef>, String> {
        let h = &self.h;
        if addr == UNDEF {
            return Ok(Vec::new());
        }
        let hd = h.rd(addr, 8 + h.sl + h.so + 4)?;
        if &hd[..4] != b"FAHD" || hd[4] != 0 {
            return Err("a fixed array header is damaged".into());
        }
        let (client, esz, bits) = (hd[5], hd[6] as usize, hd[7]);
        let nelmts = h.len_at(&hd, 8);
        let dblk = h.off(&hd, 8 + h.sl);
        let _ = page_bits;
        if nelmts > n_chunks.max(1) * 2 || nelmts > MAX_CHUNKS || dblk == UNDEF {
            return Err("a fixed array is invalid".into());
        }
        let filtered = client == 1;
        let size_bytes = esz.saturating_sub(h.so + 4);
        if esz < h.so || (filtered && (size_bytes == 0 || size_bytes > 8)) {
            return Err("a fixed array's entry size is invalid".into());
        }
        let entry = |b: &[u8], at: usize| -> (u64, u64, u32) {
            let a = h.off(b, at);
            if filtered {
                (a, le(b, at + h.so, size_bytes), le(b, at + h.so + size_bytes, 4) as u32)
            } else {
                (a, self.chunk_bytes as u64, 0)
            }
        };
        let page_n = 1u64 << bits.min(40);
        let mut out = Vec::new();
        let push = |out: &mut Vec<ChunkRef>, i: u64, e: (u64, u64, u32)| {
            if e.0 != UNDEF && i < n_chunks {
                out.push(ChunkRef { coord: self.coord_of(i), addr: e.0, size: e.1, mask: e.2 });
            }
        };
        let prefix = 6 + h.so; // signature, version, client, header address
        if nelmts <= page_n {
            let b = h.rd(dblk, prefix + nelmts as usize * esz)?;
            if &b[..4] != b"FADB" {
                return Err("a fixed array data block is damaged".into());
            }
            for i in 0..nelmts {
                push(&mut out, i, entry(&b, prefix + i as usize * esz));
            }
        } else {
            let npages = nelmts.div_ceil(page_n);
            let bitmap = (npages as usize).div_ceil(8);
            let b = h.rd(dblk, prefix + bitmap + 4)?;
            if &b[..4] != b"FADB" {
                return Err("a fixed array data block is damaged".into());
            }
            // Pages lie one after another, each with its own checksum, whether
            // or not they were written.
            let start = dblk + (prefix + bitmap + 4) as u64;
            let page_len = page_n * esz as u64 + 4;
            for p in 0..npages {
                let init = b[prefix + (p / 8) as usize] >> (7 - p % 8) & 1 == 1;
                let n = page_n.min(nelmts - p * page_n);
                if !init {
                    continue;
                }
                let page = h.rd(start + p * page_len, n as usize * esz)?;
                for k in 0..n {
                    push(&mut out, p * page_n + k, entry(&page, k as usize * esz));
                }
            }
        }
        Ok(out)
    }

    fn btree_v2(&self, addr: u64) -> Result<Vec<ChunkRef>, String> {
        let h = &self.h;
        let (typ, rec_size, recs) = h.btree2(addr)?;
        let rank = self.shape.len();
        let mut out = Vec::new();
        for r in recs {
            let (a, size, mask, at) = match typ {
                10 => (h.off(&r, 0), self.chunk_bytes as u64, 0u32, h.so),
                11 => {
                    let sz_bytes = rec_size.saturating_sub(h.so + 4 + 8 * rank);
                    if sz_bytes == 0 || sz_bytes > 8 {
                        return Err("a chunk B-tree record is invalid".into());
                    }
                    (h.off(&r, 0), le(&r, h.so, sz_bytes), le(&r, h.so + sz_bytes, 4) as u32, h.so + sz_bytes + 4)
                }
                _ => return Err("a chunk B-tree has an unexpected type".into()),
            };
            let coord: Vec<u64> = (0..rank).map(|k| le(&r, at + 8 * k, 8).saturating_mul(self.chunk[k])).collect();
            if a != UNDEF {
                out.push(ChunkRef { coord, addr: a, size, mask });
            }
        }
        Ok(out)
    }

    /// One chunk, decoded.
    fn decode(&self, i: usize) -> io::Result<Arc<Vec<u8>>> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        {
            let mut cache = self.cache.lock().map_err(|_| io::Error::other("the chunk cache is poisoned"))?;
            if let Some(pos) = cache.iter().position(|(k, _)| *k == i) {
                let hit = cache.remove(pos);
                let data = hit.1.clone();
                cache.push(hit);
                return Ok(data);
            }
        }
        let r = &self.refs[i];
        if r.size > 1 << 31 {
            return Err(bad("a chunk is absurdly large"));
        }
        let mut data = self.h.rd(r.addr, r.size as usize).map_err(|e| bad(&e))?;
        // Filters were applied in pipeline order; undo them backwards, skipping
        // the ones the chunk's mask says were skipped.
        for (k, f) in self.filters.iter().enumerate().rev() {
            if r.mask >> k & 1 == 1 {
                continue;
            }
            data = match f.id {
                1 => {
                    let mut out = Vec::with_capacity(self.chunk_bytes);
                    Inflater::new(&data[..], Wrapper::Zlib, self.chunk_bytes as u64)
                        .take(self.chunk_bytes as u64)
                        .read_to_end(&mut out)?;
                    out
                }
                2 => unshuffle(&data, f.values.first().copied().map_or(self.esize, |v| v as usize)),
                3 => {
                    data.truncate(data.len().saturating_sub(4));
                    data
                }
                32000 => lzf(&data, self.chunk_bytes).map_err(|e| bad(&e))?,
                other => return Err(bad(&format!("the {} filter isn't supported", super::filter_name(other)))),
            };
        }
        // A short chunk (damage) keeps what it has.
        data.resize(self.chunk_bytes, 0);
        let data = Arc::new(data);
        let mut cache = self.cache.lock().map_err(|_| io::Error::other("the chunk cache is poisoned"))?;
        cache.push((i, data.clone()));
        let mut bytes = cache.iter().map(|c| c.1.len()).sum::<usize>();
        while bytes > CACHE_BYTES && cache.len() > 1 {
            bytes -= cache.remove(0).1.len();
        }
        Ok(data)
    }
}

/// Undoes the shuffle filter: the data holds all first bytes, then all second
/// bytes, and so on.
fn unshuffle(data: &[u8], es: usize) -> Vec<u8> {
    if es <= 1 || data.len() < es {
        return data.to_vec();
    }
    let n = data.len() / es;
    let mut out = vec![0u8; data.len()];
    for b in 0..es {
        for i in 0..n {
            out[i * es + b] = data[b * n + i];
        }
    }
    out[n * es..].copy_from_slice(&data[n * es..]);
    out
}

/// Decodes LZF (the `lzf` filter h5py can write), at most `max` bytes.
fn lzf(inp: &[u8], max: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(max.min(1 << 24));
    let mut i = 0;
    while i < inp.len() && out.len() < max {
        let ctrl = inp[i] as usize;
        i += 1;
        if ctrl < 32 {
            let n = ctrl + 1;
            let end = i.checked_add(n).filter(|&e| e <= inp.len()).ok_or("LZF data is cut short")?;
            out.extend_from_slice(&inp[i..end]);
            i = end;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                len += *inp.get(i).ok_or("LZF data is cut short")? as usize;
                i += 1;
            }
            let back = ((ctrl & 0x1f) << 8) + *inp.get(i).ok_or("LZF data is cut short")? as usize + 1;
            i += 1;
            if back > out.len() {
                return Err("LZF data refers before its start".into());
            }
            for _ in 0..len + 2 {
                let b = out[out.len() - back];
                out.push(b);
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    out.truncate(max);
    Ok(out)
}

impl ByteSource for ChunkedSource {
    fn len(&self) -> u64 {
        self.total
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(eof)?;
        if end > self.total {
            return Err(eof());
        }
        if buf.is_empty() {
            return Ok(());
        }
        // The rows (slices along the first axis) the range touches, built
        // piece by piece so a large read never holds more than a few rows.
        let rb = self.row_bytes;
        let rank = self.shape.len();
        let mut at = offset;
        while at < end {
            let r0 = at / rb;
            // Rows in this batch: up to 32 MB.
            let rows = (((32u64 << 20) / rb).max(1)).min((end - 1) / rb - r0 + 1);
            let batch_start = r0 * rb;
            let mut tmp = vec![0u8; (rows * rb) as usize];
            // Chunks whose first-axis range overlaps these rows.
            let first_chunk_row = r0 / self.chunk[0];
            let last_chunk_row = (r0 + rows - 1) / self.chunk[0];
            for cr in first_chunk_row..=last_chunk_row {
                // All chunks in this chunk row: the grid over the other axes.
                let per_row: u64 = self.grid[1..].iter().product::<u64>().max(1);
                for g in 0..per_row {
                    let mut lin = cr;
                    let mut rest = g;
                    let mut idx = vec![0u64; rank - 1];
                    for k in (0..rank - 1).rev() {
                        let gk = self.grid[k + 1].max(1);
                        idx[k] = rest % gk;
                        rest /= gk;
                    }
                    for (k, &i) in idx.iter().enumerate() {
                        lin = lin * self.grid[k + 1].max(1) + i;
                    }
                    let Some(&ci) = self.lookup.get(&lin) else { continue };
                    let data = self.decode(ci)?;
                    self.copy_chunk(&self.refs[ci].coord, &data, r0, rows, &mut tmp);
                }
            }
            let from = (at - batch_start) as usize;
            let n = ((rows * rb - (at - batch_start)) as usize).min((end - at) as usize);
            buf[(at - offset) as usize..(at - offset) as usize + n].copy_from_slice(&tmp[from..from + n]);
            at += n as u64;
        }
        Ok(())
    }
}

impl ChunkedSource {
    /// Copies the part of a decoded chunk that falls in rows `r0..r0+rows` of
    /// the array into `tmp`, which holds exactly those rows.
    fn copy_chunk(&self, coord: &[u64], data: &[u8], r0: u64, rows: u64, tmp: &mut [u8]) {
        let rank = self.shape.len();
        let es = self.esize as u64;
        // Strides of the chunk and of the rows buffer, in bytes, per axis.
        let mut cstride = vec![0u64; rank];
        let mut rstride = vec![0u64; rank];
        let (mut c, mut r) = (es, es);
        for k in (0..rank).rev() {
            cstride[k] = c;
            rstride[k] = r;
            c *= self.chunk[k];
            r *= self.shape[k];
        }
        // The overlap along each axis: chunk-local start and length.
        let mut lo = vec![0u64; rank];
        let mut len = vec![0u64; rank];
        for k in 0..rank {
            let (a, b) = if k == 0 {
                (coord[0].max(r0), (coord[0] + self.chunk[0]).min(r0 + rows).min(self.shape[0]))
            } else {
                (coord[k], (coord[k] + self.chunk[k]).min(self.shape[k]))
            };
            if b <= a {
                return;
            }
            lo[k] = a - coord[k];
            len[k] = b - a;
        }
        // Walk every position of the overlap except along the last axis, which is copied as a run.
        let run = (len[rank - 1] * es) as usize;
        let mut pos = vec![0u64; rank - 1];
        loop {
            let mut src = (lo[rank - 1]) * es;
            let mut dst = (coord[rank - 1] + lo[rank - 1]) * es;
            for k in 0..rank - 1 {
                src += (lo[k] + pos[k]) * cstride[k];
                let global = coord[k] + lo[k] + pos[k] - if k == 0 { r0 } else { 0 };
                dst += global * rstride[k];
            }
            let (s, d) = (src as usize, dst as usize);
            if s + run <= data.len() && d + run <= tmp.len() {
                tmp[d..d + run].copy_from_slice(&data[s..s + run]);
            }
            // Next position (odometer over axes 0..rank-1).
            let mut k = rank - 1;
            loop {
                if k == 0 {
                    return;
                }
                k -= 1;
                pos[k] += 1;
                if pos[k] < len[k] {
                    break;
                }
                pos[k] = 0;
                if k == 0 {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_round_trips() {
        let vals: Vec<u8> = (0..40).collect();
        // Shuffle by hand: 10 elements of 4 bytes.
        let mut sh = vec![0u8; 40];
        for i in 0..10 {
            for b in 0..4 {
                sh[b * 10 + i] = vals[i * 4 + b];
            }
        }
        assert_eq!(unshuffle(&sh, 4), vals);
        assert_eq!(unshuffle(&[1, 2, 3], 4), vec![1, 2, 3]);
    }

    #[test]
    fn lzf_decodes_literals_and_back_references() {
        // "abcabcabc": literal "abc" then a copy of length 6 from 3 back.
        let enc = [2, b'a', b'b', b'c', (6 - 2) << 5, 2];
        assert_eq!(lzf(&enc, 100).unwrap(), b"abcabcabc");
        assert!(lzf(&[0xe0, 0xff], 100).is_err());
        assert!(lzf(&[0x20, 9], 100).is_err());
    }
}
