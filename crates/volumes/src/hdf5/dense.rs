//! Version 2 B-trees and fractal heaps: where HDF5 keeps the members of big
//! groups ("dense" link storage) and, in files written with the newest
//! settings, the chunk index of datasets with several unlimited dimensions.

use super::{H5, UNDEF, le};

/// Most records a B-tree is read for.
const MAX_RECORDS: u64 = 1 << 20;

fn bytes_for(max: u64) -> usize {
    ((64 - max.leading_zeros() as usize).div_ceil(8)).max(1)
}

/// The header of a version 2 B-tree.
struct Tree {
    typ: u8,
    node_size: usize,
    rec_size: usize,
    depth: usize,
    root: u64,
    root_nrec: u64,
    total: u64,
}

impl H5 {
    /// Every record of the version 2 B-tree whose header is at `addr`, in
    /// tree order, with the tree's type and record size.
    pub(crate) fn btree2(&self, addr: u64) -> Result<(u8, usize, Vec<Vec<u8>>), String> {
        let h = self.rd(addr, 16 + 2 * self.so.max(self.sl) + 8)?;
        if &h[..4] != b"BTHD" || h[4] != 0 {
            return Err("a B-tree header is damaged".into());
        }
        let t = Tree {
            typ: h[5],
            node_size: le(&h, 6, 4) as usize,
            rec_size: le(&h, 10, 2) as usize,
            depth: le(&h, 12, 2) as usize,
            root: self.off(&h, 16),
            root_nrec: le(&h, 16 + self.so, 2),
            total: self.len_at(&h, 18 + self.so),
        };
        if t.rec_size == 0 || t.node_size < 16 || t.node_size > 1 << 24 || t.depth > 16 || t.total > MAX_RECORDS {
            return Err("a B-tree header is invalid".into());
        }
        let mut out = Vec::new();
        if t.root == UNDEF || t.total == 0 {
            return Ok((t.typ, t.rec_size, out));
        }
        // Sizes of the record counts in nodes of each depth.
        let mut nrec_bytes = Vec::new();
        let mut cum = Vec::new();
        let mut max_nrec = Vec::new();
        for d in 0..=t.depth {
            let m = if d == 0 {
                (t.node_size.saturating_sub(10)) / t.rec_size
            } else {
                let p = self.so + nrec_bytes[d - 1] + if d - 1 > 0 { bytes_for(cum[d - 1]) } else { 0 };
                t.node_size.saturating_sub(10 + p) / (t.rec_size + p)
            };
            let c = if d == 0 { m as u64 } else { (m as u64 + 1).saturating_mul(cum[d - 1]).saturating_add(m as u64) };
            max_nrec.push(m);
            cum.push(c);
            nrec_bytes.push(bytes_for(m as u64));
        }
        // Records are collected in no particular order; callers read their contents.
        let mut stack = vec![(t.root, t.root_nrec, t.depth)];
        let mut order: Vec<Vec<u8>> = Vec::new();
        let mut visited = 0usize;
        while let Some((a, nrec, depth)) = stack.pop() {
            visited += 1;
            if visited > 1 << 20 {
                return Err("a B-tree has too many nodes".into());
            }
            if nrec as usize > max_nrec[depth] {
                return Err("a B-tree node claims too many records".into());
            }
            let ptr = if depth > 0 {
                self.so + nrec_bytes[depth - 1] + if depth - 1 > 0 { bytes_for(cum[depth - 1]) } else { 0 }
            } else {
                0
            };
            let need = 6 + nrec as usize * t.rec_size + if depth > 0 { (nrec as usize + 1) * ptr } else { 0 };
            if need > t.node_size {
                return Err("a B-tree node is larger than its size".into());
            }
            let node = self.rd(a, need)?;
            if (depth == 0 && &node[..4] != b"BTLF") || (depth > 0 && &node[..4] != b"BTIN") {
                return Err("a B-tree node is damaged".into());
            }
            for i in 0..nrec as usize {
                order.push(node[6 + i * t.rec_size..6 + (i + 1) * t.rec_size].to_vec());
            }
            if depth > 0 {
                let base = 6 + nrec as usize * t.rec_size;
                for i in 0..=nrec as usize {
                    let at = base + i * ptr;
                    stack.push((self.off(&node, at), le(&node, at + self.so, nrec_bytes[depth - 1]), depth - 1));
                }
            }
            if order.len() as u64 > MAX_RECORDS {
                return Err("a B-tree has too many records".into());
            }
        }
        out.extend(order);
        Ok((t.typ, t.rec_size, out))
    }
}

/*┌─────────────────╮
| Fractal heaps   |
└─────────────────╯*/
//region Fractal heaps

pub(crate) struct Heap {
    id_len: usize,
    filtered: bool,
    flags: u8,
    max_managed: u64,
    width: u64,
    start: u64,
    max_direct: u64,
    bits: u32,
    root: u64,
    rows: u64,
}

impl H5 {
    pub(crate) fn fractal_heap(&self, addr: u64) -> Result<Heap, String> {
        let (so, sl) = (self.so, self.sl);
        let b = self.rd(addr, 5 + 5 + 4 + sl + so + 4 * sl + so + 5 * sl + 2 + 2 * sl + 2 + 2 + so + 2 + 4)?;
        if &b[..4] != b"FRHP" || b[4] != 0 {
            return Err("a fractal heap header is damaged".into());
        }
        let mut p = 5;
        let id_len = le(&b, p, 2) as usize;
        let filters_len = le(&b, p + 2, 2);
        let flags = b[p + 4];
        let max_managed = le(&b, p + 5, 4);
        p += 9 + sl + so + 4 * sl + so + 5 * sl;
        let width = le(&b, p, 2);
        let start = self.len_at(&b, p + 2);
        let max_direct = self.len_at(&b, p + 2 + sl);
        let bits = le(&b, p + 2 + 2 * sl, 2) as u32;
        let root = self.off(&b, p + 6 + 2 * sl);
        let rows = le(&b, p + 6 + 2 * sl + so, 2);
        if width == 0
            || width > 1 << 12
            || start == 0
            || !start.is_power_of_two()
            || max_direct < start
            || !max_direct.is_power_of_two()
            || bits == 0
            || bits > 64
        {
            return Err("a fractal heap's parameters are invalid".into());
        }
        Ok(Heap { id_len, filtered: filters_len > 0, flags, max_managed, width, start, max_direct, bits, root, rows })
    }

    /// The bytes of the object a heap ID names.
    pub(crate) fn heap_object(&self, hp: &Heap, id: &[u8]) -> Result<Vec<u8>, String> {
        let kind = (id.first().ok_or("an empty heap ID")? >> 4) & 3;
        if id[0] >> 6 != 0 {
            return Err("a heap ID has an unknown version".into());
        }
        match kind {
            1 => {
                // Tiny: the data is in the ID itself.
                let n = (id[0] & 0x0f) as usize + 1;
                return id
                    .get(1..1 + n)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| "a tiny heap object is cut short".to_string());
            }
            0 => {}
            _ => return Err("huge heap objects aren't read".into()),
        }
        if hp.filtered {
            return Err("filtered fractal heaps aren't read".into());
        }
        let payload = le(id, 1, (id.len() - 1).min(8));
        let mask = |bits: u32| if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
        let offset = payload & mask(hp.bits);
        let len_bits = ((hp.id_len.saturating_sub(1)) * 8).saturating_sub(hp.bits as usize).min(64) as u32;
        let length = (payload >> hp.bits.min(63)) & mask(len_bits);
        if length > hp.max_managed.max(1 << 20) {
            return Err("a heap object is absurdly large".into());
        }
        let (block, block_off, block_size) = self.heap_block(hp, offset)?;
        let inside = offset - block_off;
        if inside + length > block_size {
            return Err("a heap object runs past its block".into());
        }
        self.rd(block + inside, length as usize)
    }

    /// The direct block that holds heap offset `offset`: its address, where it
    /// starts in the heap and its size.
    fn heap_block(&self, hp: &Heap, offset: u64) -> Result<(u64, u64, u64), String> {
        if hp.root == UNDEF {
            return Err("a fractal heap is empty".into());
        }
        if hp.rows == 0 {
            return Ok((hp.root, 0, hp.start));
        }
        let off_bytes = (hp.bits as usize).div_ceil(8);
        let (mut addr, mut rows, mut base) = (hp.root, hp.rows, 0u64);
        for _ in 0..16 {
            let head = 5 + self.so + off_bytes;
            let entries =
                rows.checked_mul(hp.width).filter(|&n| n <= 1 << 16).ok_or("an indirect block is absurdly large")?
                    as usize;
            let b = self.rd_upto(addr, head + entries * (self.so + 12) + 4)?;
            if &b[..4] != b"FHIB" {
                return Err("a heap's indirect block is damaged".into());
            }
            let bsz_of = |r: u64| if r == 0 { hp.start } else { hp.start << (r - 1).min(40) };
            let mut cursor = base;
            let mut index = None;
            'rows: for r in 0..rows {
                let bsz = bsz_of(r);
                for c in 0..hp.width {
                    if offset < cursor + bsz {
                        index = Some((r, c, bsz, cursor));
                        break 'rows;
                    }
                    cursor += bsz;
                }
            }
            let (r, c, bsz, start) = index.ok_or("a heap offset is outside the heap")?;
            // Entry sizes: direct rows may carry filter fields; indirect rows don't.
            let direct = |row: u64| bsz_of(row) <= hp.max_direct;
            let entry_size = |row: u64| if direct(row) && hp.filtered { self.so + self.sl + 4 } else { self.so };
            let mut at = head;
            for rr in 0..r {
                at += hp.width as usize * entry_size(rr);
            }
            at += c as usize * entry_size(r);
            if at + self.so > b.len() {
                return Err("an indirect heap block is cut short".into());
            }
            let child = self.off(&b, at);
            if child == UNDEF {
                return Err("a heap block was never written".into());
            }
            if direct(r) {
                return Ok((child, start, bsz));
            }
            // A child indirect block spans `bsz` bytes of heap.
            let unit = hp.width * hp.start;
            let ratio = bsz / unit.max(1);
            if ratio == 0 || !ratio.is_power_of_two() {
                return Err("an indirect heap block has an invalid size".into());
            }
            rows = ratio.trailing_zeros() as u64 + 1;
            addr = child;
            base = start;
        }
        Err("a fractal heap is nested too deeply".into())
    }

    /// The link messages stored in a group's dense storage.
    pub(crate) fn dense_links(&self, heap: u64, tree: u64) -> Result<Vec<Vec<u8>>, String> {
        let hp = self.fractal_heap(heap)?;
        let (typ, rec_size, recs) = self.btree2(tree)?;
        if typ != 5 && typ != 6 {
            return Err("a group's name index has an unexpected type".into());
        }
        let _ = hp.flags;
        let mut out = Vec::new();
        let mut first_error = None;
        let n = recs.len();
        for r in recs {
            let id = r.get(rec_size.saturating_sub(hp.id_len)..).ok_or("a name index record is short")?;
            match self.heap_object(&hp, id) {
                Ok(o) => out.push(o),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if out.is_empty() && n > 0 {
            return Err(first_error.unwrap_or_else(|| "a group's links couldn't be read".into()));
        }
        Ok(out)
    }
}

//endregion
