//! IRAF's pixel list compression (`PLIO_1`), used for masks and other
//! images with few distinct values.  The data is a "line list" of 16-bit
//! instructions, each with a 12-bit operand and a 4-bit... opcode in the high
//! bits, that describe runs of zeros, runs of a high value, and single
//! pixels that step the high value up or down.

use crate::TextureError;
use crate::util::invalid;

/// Opcodes (the instruction word shifted right by 12 bits).
const ZN: u16 = 0;
const SH: u16 = 1;
const IH: u16 = 2;
const DH: u16 = 3;
const HN: u16 = 4;
const PN: u16 = 5;
const IS: u16 = 6;
const DS: u16 = 7;

/// Decodes a line list into `count` pixels.  `list` holds the instruction
/// words, already converted to host order.
pub(crate) fn decode(list: &[i16], count: usize) -> Result<Vec<i32>, TextureError> {
    let bad = || invalid("PLIO: damaged line list");
    if list.len() < 3 {
        return Err(bad());
    }
    // The header: word 3 (1-based) positive means a new-style list whose
    // length is spread over words 4 and 5 and whose first instruction is
    // word `list[1]`; otherwise the length is word 4 and instructions start
    // right after the 7-word header.
    let (len, first) = if list[2] > 0 {
        let hi = *list.get(4).ok_or_else(bad)? as i64;
        let lo = *list.get(3).ok_or_else(bad)? as i64;
        (((hi << 15) + lo) as usize, list[1] as usize)
    } else {
        (*list.get(3).ok_or_else(bad)? as usize, 8)
    };
    let len = len.min(list.len());
    let mut out = vec![0i32; count];
    let mut op = 0usize;
    let mut x1: i64 = 1;
    let mut pv: i32 = 1;
    let mut skip = false;
    let mut ip = first.max(1);
    while ip <= len {
        if skip {
            skip = false;
            ip += 1;
            continue;
        }
        let word = list[ip - 1] as u16;
        let opcode = word >> 12;
        let data = (word & 4095) as i32;
        match opcode {
            ZN | HN | PN => {
                let x2 = x1 + data as i64 - 1;
                let i1 = x1.max(1);
                let i2 = x2.min(count as i64);
                if i2 >= i1 {
                    let n = (i2 - i1 + 1) as usize;
                    let end = (op + n).min(count);
                    let value = if opcode == HN { pv } else { 0 };
                    out[op..end].fill(value);
                    if opcode == PN && i2 == x2 && end > op {
                        out[end - 1] = pv;
                    }
                    op = end;
                }
                x1 = x2 + 1;
            }
            SH => {
                let next = *list.get(ip).ok_or_else(bad)? as i32;
                pv = (next << 12) + data;
                skip = true;
            }
            IH => pv = pv.wrapping_add(data),
            DH => pv = pv.wrapping_sub(data),
            IS | DS => {
                pv = if opcode == IS { pv.wrapping_add(data) } else { pv.wrapping_sub(data) };
                if x1 >= 1 && x1 <= count as i64 && op < count {
                    out[op] = pv;
                    op += 1;
                }
                x1 += 1;
            }
            _ => return Err(bad()),
        }
        if op >= count && x1 > count as i64 {
            break;
        }
        ip += 1;
    }
    Ok(out)
}
