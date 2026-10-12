//! Element types of stored arrays, and their conversion to `f32` for display.
//!
//! Every format in this crate stores its samples as plain numbers of one of a
//! handful of types, in either byte order.  The viewer and the thumbnails work
//! on `f32`, which holds every 8, 16 and 24-bit integer and every half or
//! single float exactly and the wider types to display accuracy.

/// What one stored sample is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DType {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F16,
    F32,
    F64,
    /// Two `f32`s, real and imaginary; shown as the magnitude.
    C64,
    /// Two `f64`s, real and imaginary; shown as the magnitude.
    C128,
}

/// The byte order of multi-byte samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    /// The byte order of the machine running this code.
    pub const NATIVE: Endian = if cfg!(target_endian = "big") { Endian::Big } else { Endian::Little };
}

impl DType {
    /// Bytes per sample.
    pub const fn size(self) -> usize {
        match self {
            DType::Bool | DType::U8 | DType::I8 => 1,
            DType::U16 | DType::I16 | DType::F16 => 2,
            DType::U32 | DType::I32 | DType::F32 => 4,
            DType::U64 | DType::I64 | DType::F64 | DType::C64 => 8,
            DType::C128 => 16,
        }
    }

    /// How the type is written in the inspector, in NumPy's words.
    pub const fn name(self) -> &'static str {
        match self {
            DType::Bool => "bool",
            DType::U8 => "uint8",
            DType::I8 => "int8",
            DType::U16 => "uint16",
            DType::I16 => "int16",
            DType::U32 => "uint32",
            DType::I32 => "int32",
            DType::U64 => "uint64",
            DType::I64 => "int64",
            DType::F16 => "float16",
            DType::F32 => "float32",
            DType::F64 => "float64",
            DType::C64 => "complex64",
            DType::C128 => "complex128",
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, DType::F16 | DType::F32 | DType::F64 | DType::C64 | DType::C128)
    }

    pub const fn is_signed(self) -> bool {
        matches!(self, DType::I8 | DType::I16 | DType::I32 | DType::I64) || self.is_float()
    }

    /// The range an integer type spans, which display windows use for
    /// pictures that should show the full range of their type (8-bit and
    /// 16-bit images).  `None` for floats.
    pub fn integer_range(self) -> Option<(f32, f32)> {
        Some(match self {
            DType::Bool => (0.0, 1.0),
            DType::U8 => (0.0, 255.0),
            DType::I8 => (-128.0, 127.0),
            DType::U16 => (0.0, 65535.0),
            DType::I16 => (-32768.0, 32767.0),
            DType::U32 => (0.0, 4_294_967_295.0),
            DType::I32 => (-2_147_483_648.0, 2_147_483_647.0),
            DType::U64 => (0.0, 1.8446744e19),
            DType::I64 => (-9.223372e18, 9.223372e18),
            _ => return None,
        })
    }
}

/// Converts half-precision bits to a single.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match (exp, frac) {
        (0, 0) => sign,
        // Subnormal: normalise.
        (0, f) => {
            let shift = f.leading_zeros() - 21;
            let f = (f << shift) & 0x3ff;
            sign | ((127 - 15 - shift + 1) << 23) | (f << 13)
        }
        (31, 0) => sign | 0x7f80_0000,
        (31, f) => sign | 0x7fc0_0000 | (f << 13),
        (e, f) => sign | ((e + 127 - 15) << 23) | (f << 13),
    };
    f32::from_bits(bits)
}

/// Converts `bytes` (a whole number of `dtype` samples in `endian` order) to
/// `f32` in `out`, one value per sample.  Complex samples become their
/// magnitude.  Stops at the shorter of the two.
pub fn decode(dtype: DType, endian: Endian, bytes: &[u8], out: &mut [f32]) {
    match dtype {
        DType::Bool => {
            for (o, &b) in out.iter_mut().zip(bytes) {
                *o = if b != 0 { 1.0 } else { 0.0 };
            }
        }
        DType::U8 => {
            for (o, &b) in out.iter_mut().zip(bytes) {
                *o = b as f32;
            }
        }
        DType::I8 => {
            for (o, &b) in out.iter_mut().zip(bytes) {
                *o = b as i8 as f32;
            }
        }
        DType::U16 => ints::<2>(endian, bytes, out, |a, e| match e {
            Endian::Little => u16::from_le_bytes(a) as f32,
            Endian::Big => u16::from_be_bytes(a) as f32,
        }),
        DType::I16 => ints::<2>(endian, bytes, out, |a, e| match e {
            Endian::Little => i16::from_le_bytes(a) as f32,
            Endian::Big => i16::from_be_bytes(a) as f32,
        }),
        DType::F16 => ints::<2>(endian, bytes, out, |a, e| {
            f16_to_f32(match e {
                Endian::Little => u16::from_le_bytes(a),
                Endian::Big => u16::from_be_bytes(a),
            })
        }),
        DType::U32 => ints::<4>(endian, bytes, out, |a, e| match e {
            Endian::Little => u32::from_le_bytes(a) as f32,
            Endian::Big => u32::from_be_bytes(a) as f32,
        }),
        DType::I32 => ints::<4>(endian, bytes, out, |a, e| match e {
            Endian::Little => i32::from_le_bytes(a) as f32,
            Endian::Big => i32::from_be_bytes(a) as f32,
        }),
        DType::F32 => ints::<4>(endian, bytes, out, |a, e| match e {
            Endian::Little => f32::from_le_bytes(a),
            Endian::Big => f32::from_be_bytes(a),
        }),
        DType::U64 => ints::<8>(endian, bytes, out, |a, e| match e {
            Endian::Little => u64::from_le_bytes(a) as f32,
            Endian::Big => u64::from_be_bytes(a) as f32,
        }),
        DType::I64 => ints::<8>(endian, bytes, out, |a, e| match e {
            Endian::Little => i64::from_le_bytes(a) as f32,
            Endian::Big => i64::from_be_bytes(a) as f32,
        }),
        DType::F64 => ints::<8>(endian, bytes, out, |a, e| match e {
            Endian::Little => f64::from_le_bytes(a) as f32,
            Endian::Big => f64::from_be_bytes(a) as f32,
        }),
        DType::C64 => ints::<8>(endian, bytes, out, |a, e| {
            let (re, im) = match e {
                Endian::Little => {
                    (f32::from_le_bytes([a[0], a[1], a[2], a[3]]), f32::from_le_bytes([a[4], a[5], a[6], a[7]]))
                }
                Endian::Big => {
                    (f32::from_be_bytes([a[0], a[1], a[2], a[3]]), f32::from_be_bytes([a[4], a[5], a[6], a[7]]))
                }
            };
            re.hypot(im)
        }),
        DType::C128 => ints::<16>(endian, bytes, out, |a, e| {
            let half = |s: &[u8]| -> f64 {
                let b: [u8; 8] = s.try_into().unwrap_or([0; 8]);
                match e {
                    Endian::Little => f64::from_le_bytes(b),
                    Endian::Big => f64::from_be_bytes(b),
                }
            };
            half(&a[..8]).hypot(half(&a[8..])) as f32
        }),
    }
}

/// One sample as an `f64`, keeping the precision `decode` gives up for the
/// 64-bit types.  Complex samples become their magnitude.
pub fn decode_f64(dtype: DType, endian: Endian, bytes: &[u8]) -> f64 {
    let arr = |n: usize| -> [u8; 8] {
        let mut a = [0u8; 8];
        if let Some(s) = bytes.get(..n) {
            a[..n].copy_from_slice(s);
        }
        a
    };
    let pair = |w: usize| -> (f64, f64) {
        let one = |s: &[u8]| -> f64 {
            if w == 4 {
                let b: [u8; 4] = s.try_into().unwrap_or([0; 4]);
                (if endian == Endian::Little { f32::from_le_bytes(b) } else { f32::from_be_bytes(b) }) as f64
            } else {
                let b: [u8; 8] = s.try_into().unwrap_or([0; 8]);
                if endian == Endian::Little { f64::from_le_bytes(b) } else { f64::from_be_bytes(b) }
            }
        };
        match (bytes.get(..w), bytes.get(w..2 * w)) {
            (Some(a), Some(b)) => (one(a), one(b)),
            _ => (0.0, 0.0),
        }
    };
    match dtype {
        DType::U64 => {
            let a = arr(8);
            (if endian == Endian::Little { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) }) as f64
        }
        DType::I64 => {
            let a = arr(8);
            (if endian == Endian::Little { i64::from_le_bytes(a) } else { i64::from_be_bytes(a) }) as f64
        }
        DType::F64 => {
            let a = arr(8);
            if endian == Endian::Little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }
        }
        DType::C64 => {
            let (re, im) = pair(4);
            re.hypot(im)
        }
        DType::C128 => {
            let (re, im) = pair(8);
            re.hypot(im)
        }
        _ => {
            let mut out = [0f32; 1];
            decode(dtype, endian, bytes, &mut out);
            out[0] as f64
        }
    }
}

fn ints<const N: usize>(endian: Endian, bytes: &[u8], out: &mut [f32], f: impl Fn([u8; N], Endian) -> f32) {
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(N)) {
        let mut a = [0u8; N];
        a.copy_from_slice(c);
        *o = f(a, endian);
    }
}
