//! Decoding MeatPack, which packs the fifteen most common G-code
//! characters (digits, `.`, space, newline, `G` and `X`) into four bits
//! each, two to a byte, low half first.  A half of `1111` means that
//! character follows as a whole byte.  Two `0xFF` bytes and a command byte
//! switch packing on (`0xFB`) or off (`0xFA`), reset (`0xF9`), or turn the
//! space's code into `E` (`0xF7`; `0xF6` turns it back), for G-code
//! written without spaces.  After a packed newline the other half of the
//! byte is padding.
//!
//! Binary G-code uses it for its G-code blocks, in a variant that keeps
//! comment lines (whose characters simply go as whole bytes).

/// MeatPack's state, which carries from one block to the next as it does
/// in a printer's serial stream.
#[derive(Default)]
pub struct MeatPack {
    packing: bool,
    no_spaces: bool,
    /// `0xFF` bytes seen in a row (0 or 1).
    signal: u8,
    command_next: bool,
    /// Whole characters still to come.
    pending: u8,
    /// A packed character to emit after the next whole one.
    second: Option<u8>,
}

impl MeatPack {
    /// Decodes `input`, appending the text to `out`.
    pub fn decode(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &c in input {
            if c == 0xff && !self.command_next {
                if self.signal == 1 {
                    self.signal = 0;
                    self.command_next = true;
                } else {
                    self.signal = 1;
                }
                continue;
            }
            if self.command_next {
                self.command_next = false;
                match c {
                    0xfb => self.packing = true,
                    0xfa => self.packing = false,
                    0xf9 => *self = MeatPack::default(),
                    0xf7 => self.no_spaces = true,
                    0xf6 => self.no_spaces = false,
                    _ => {}
                }
                continue;
            }
            if self.signal == 1 {
                // A lone 0xFF is data: two whole characters follow.
                self.signal = 0;
                self.byte(0xff, out);
            }
            self.byte(c, out);
        }
    }

    fn byte(&mut self, c: u8, out: &mut Vec<u8>) {
        if !self.packing {
            out.push(c);
            return;
        }
        if self.pending > 0 {
            out.push(c);
            if let Some(s) = self.second.take() {
                out.push(s);
            }
            self.pending -= 1;
            return;
        }
        let (low, high) = (c & 0xf, c >> 4);
        if low == 0xf {
            self.pending = 1;
            if high == 0xf {
                self.pending = 2;
            } else {
                self.second = Some(self.char_of(high));
            }
            return;
        }
        let first = self.char_of(low);
        out.push(first);
        if first != b'\n' {
            if high == 0xf {
                self.pending = 1;
            } else {
                out.push(self.char_of(high));
            }
        }
    }

    fn char_of(&self, code: u8) -> u8 {
        match code {
            0..=9 => b'0' + code,
            10 => b'.',
            11 if self.no_spaces => b'E',
            11 => b' ',
            12 => b'\n',
            13 => b'G',
            _ => b'X',
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::MeatPack;

    /// A MeatPack encoder for tests, packing without spaces as binary
    /// G-code does, with comments and other characters as whole bytes.
    pub fn encode(text: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, 0xff, 0xfb, 0xff, 0xff, 0xf7];
        let code = |c: u8| -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'.' => Some(10),
                b'E' => Some(11),
                b'\n' => Some(12),
                b'G' => Some(13),
                b'X' => Some(14),
                _ => None,
            }
        };
        let mut i = 0;
        while i < text.len() {
            let a = text[i];
            let b = text.get(i + 1).copied();
            let (ca, cb) = (code(a), b.and_then(code));
            if a == b'\n' && ca.is_some() {
                out.push(12);
                i += 1;
                continue;
            }
            match (ca, b, cb) {
                (Some(x), Some(_), Some(y)) => {
                    out.push(x | y << 4);
                }
                (Some(x), Some(bb), None) => {
                    out.push(x | 0xf0);
                    out.push(bb);
                }
                (None, Some(_), Some(y)) => {
                    out.push(0x0f | y << 4);
                    out.push(a);
                }
                (None, Some(bb), None) => {
                    out.push(0xff);
                    out.push(a);
                    out.push(bb);
                }
                (Some(x), None, _) => {
                    out.push(x | 0xc0);
                    i += 1;
                    continue;
                }
                (None, None, _) => {
                    out.push(0xff);
                    out.push(a);
                    out.push(b'\n');
                    i += 1;
                    continue;
                }
            }
            i += 2;
        }
        out
    }

    #[test]
    fn round_trips_without_spaces() {
        let text = b"G1X10.5Y3E.25\n;TYPE:External perimeter\nM83\nG92E0\nG1X1Y2\n";
        let mut out = Vec::new();
        MeatPack::default().decode(&encode(text), &mut out);
        assert_eq!(String::from_utf8_lossy(&out), String::from_utf8_lossy(text));
    }

    #[test]
    fn stays_unpacked_until_told() {
        let mut out = Vec::new();
        let mut mp = MeatPack::default();
        mp.decode(b"G1 X1\n", &mut out);
        assert_eq!(out, b"G1 X1\n");
        out.clear();
        mp.decode(&[0xff, 0xff, 0xfb, 0x1d, 0x0c], &mut out);
        assert_eq!(out, b"G1\n");
    }
}
