//! Decoding heatshrink, the small LZSS compressor binary G-code uses for
//! its G-code blocks.  The stream is bits, most significant first: a 1
//! bit and eight bits are a literal byte; a 0 bit, `window` bits and
//! `lookahead` bits copy (count + 1) bytes from (index + 1) bytes back.
//! Positions before the start of the output read as zero bytes, as the
//! reference decoder's zeroed window gives them.

/// Decodes `input` with the given window and lookahead sizes (in bits),
/// producing at most `limit` bytes.
pub fn decode(input: &[u8], window: u32, lookahead: u32, limit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(limit.min(input.len().saturating_mul(4)).min(64 << 20));
    let mut bits = Bits { data: input, pos: 0 };
    while out.len() < limit {
        let Some(tag) = bits.take(1) else { break };
        if tag == 1 {
            let Some(byte) = bits.take(8) else { break };
            out.push(byte as u8);
        } else {
            let (Some(index), Some(count)) = (bits.take(window), bits.take(lookahead)) else { break };
            let back = index as usize + 1;
            for _ in 0..=count {
                if out.len() >= limit {
                    break;
                }
                let b = if back <= out.len() { out[out.len() - back] } else { 0 };
                out.push(b);
            }
        }
    }
    out
}

struct Bits<'a> {
    data: &'a [u8],
    /// The position in bits.
    pos: usize,
}

impl Bits<'_> {
    /// The next `n` bits (at most 24) as a number, or `None` past the end.
    fn take(&mut self, n: u32) -> Option<u32> {
        if self.pos + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            v = v << 1 | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

#[cfg(test)]
pub mod tests {
    /// A deliberately simple heatshrink encoder for tests: literals, and
    /// back references found by brute force.
    pub fn encode(input: &[u8], window: u32, lookahead: u32) -> Vec<u8> {
        let mut out = BitWriter::default();
        let max_back = 1usize << window;
        let max_len = 1usize << lookahead;
        let mut i = 0;
        while i < input.len() {
            let mut best = (0usize, 0usize);
            for back in 1..=max_back.min(i) {
                let mut len = 0;
                while len < max_len && i + len < input.len() && input[i + len - back] == input[i + len] {
                    len += 1;
                }
                if len > best.1 {
                    best = (back, len);
                }
            }
            if best.1 >= 2 {
                out.put(0, 1);
                out.put((best.0 - 1) as u32, window);
                out.put((best.1 - 1) as u32, lookahead);
                i += best.1;
            } else {
                out.put(1, 1);
                out.put(input[i] as u32, 8);
                i += 1;
            }
        }
        out.finish()
    }

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bits: u32,
    }

    impl BitWriter {
        fn put(&mut self, v: u32, n: u32) {
            for k in (0..n).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let bit = (v >> k) & 1;
                let last = self.bytes.last_mut().unwrap();
                *last |= (bit as u8) << (7 - self.bits % 8);
                self.bits += 1;
            }
        }

        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    #[test]
    fn round_trips() {
        let text = b"G1 X10 Y10 E0.5\nG1 X20 Y10 E0.5\nG1 X20 Y20 E0.5\n;TYPE:Perimeter\n".repeat(20);
        for window in [11, 12] {
            let packed = encode(&text, window, 4);
            assert!(packed.len() < text.len() / 2);
            assert_eq!(super::decode(&packed, window, 4, text.len()), text);
        }
    }

    #[test]
    fn references_before_the_start_read_zeros() {
        // A back reference first thing: index 3, count 1 → two zero bytes.
        let mut w = BitWriter::default();
        w.put(0, 1);
        w.put(3, 11);
        w.put(1, 4);
        let out = super::decode(&w.finish(), 11, 4, 100);
        assert_eq!(out, [0, 0]);
        assert_eq!(super::decode(&[0xff; 64], 12, 4, 10).len(), 10);
    }
}
