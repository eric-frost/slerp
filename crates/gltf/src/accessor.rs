//! Reading accessors: every component type, normalized integers, strides,
//! matrix column padding, sparse substitution, and accessors without a
//! buffer view (all zeros).  Integer data that `KHR_mesh_quantization`
//! allows is turned into floats here, so nothing downstream sees it.

use crate::Ctx;
use crate::data::ViewData;
use crate::json::{AccessorJ, BufferViewJ};
use slerp_scene::LoadError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comp {
    I8,
    U8,
    I16,
    U16,
    U32,
    F32,
}

impl Comp {
    fn from_gl(v: u32) -> Option<Comp> {
        Some(match v {
            5120 => Comp::I8,
            5121 => Comp::U8,
            5122 => Comp::I16,
            5123 => Comp::U16,
            5125 => Comp::U32,
            5126 => Comp::F32,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Comp::I8 | Comp::U8 => 1,
            Comp::I16 | Comp::U16 => 2,
            Comp::U32 | Comp::F32 => 4,
        }
    }
}

/// Where each component of an element sits.  Matrix columns of 1- and
/// 2-byte components are padded to 4 bytes, as glTF requires.
struct Layout {
    comp: Comp,
    normalized: bool,
    count: usize,
    ncomp: usize,
    offsets: [usize; 16],
    elem_size: usize,
}

impl Layout {
    fn new(acc: &AccessorJ) -> Result<Layout, LoadError> {
        let comp = Comp::from_gl(acc.component_type)
            .ok_or_else(|| LoadError::invalid(format!("unknown component type {}", acc.component_type)))?;
        let (cols, rows) = match acc.kind.as_str() {
            "SCALAR" => (1, 1),
            "VEC2" => (1, 2),
            "VEC3" => (1, 3),
            "VEC4" => (1, 4),
            "MAT2" => (2, 2),
            "MAT3" => (3, 3),
            "MAT4" => (4, 4),
            other => return Err(LoadError::invalid(format!("unknown accessor type {other:?}"))),
        };
        let size = comp.size();
        let col_stride = if cols > 1 { (rows * size).next_multiple_of(4) } else { rows * size };
        let mut offsets = [0; 16];
        for c in 0..cols {
            for r in 0..rows {
                offsets[c * rows + r] = c * col_stride + r * size;
            }
        }
        let count = usize::try_from(acc.count).map_err(|_| LoadError::invalid("accessor count out of range"))?;
        Ok(Layout {
            comp,
            normalized: acc.normalized,
            count,
            ncomp: cols * rows,
            offsets,
            elem_size: cols * col_stride,
        })
    }
}

/// Reads accessors from resolved buffer views.
pub struct Accessors<'a> {
    pub json: &'a [AccessorJ],
    pub views: &'a [Option<ViewData<'a>>],
    pub view_json: &'a [BufferViewJ],
    pub ctx: &'a Ctx,
}

impl Accessors<'_> {
    fn get(&self, index: usize) -> Result<&AccessorJ, LoadError> {
        self.json.get(index).ok_or_else(|| LoadError::invalid(format!("accessor {index} doesn't exist")))
    }

    /// True when the accessor stores integers rather than floats, so values
    /// such as normals need renormalizing after quantization.
    pub fn is_quantized(&self, index: usize) -> bool {
        self.get(index).is_ok_and(|a| a.component_type != 5126)
    }

    /// Reads up to `N` components per element as floats (normalized
    /// integers mapped to -1..1 or 0..1, other integers converted as they
    /// are).  Elements with fewer components are completed from `pad`, so a
    /// VEC3 colour reads as RGBA with alpha 1.
    pub fn read<const N: usize>(&self, index: usize, pad: [f32; N]) -> Result<Vec<[f32; N]>, LoadError> {
        let acc = self.get(index)?;
        let layout = Layout::new(acc)?;
        if layout.ncomp > N {
            return Err(LoadError::invalid(format!("accessor {index} has {} components, expected {N}", layout.ncomp)));
        }
        // Bounds are checked before claiming any of the decoding budget.
        let source = acc.buffer_view.map(|view| self.view(view, acc.byte_offset, &layout)).transpose()?;
        self.ctx.reserve((layout.count as u64).saturating_mul((N * 4) as u64))?;
        let mut out = vec![pad; layout.count];
        if let Some((data, stride)) = source {
            fill(&mut out, data, stride, &layout);
        }
        if let Some(sparse) = &acc.sparse {
            let n = usize::try_from(sparse.count).map_err(|_| LoadError::invalid("sparse count out of range"))?;
            if n > layout.count {
                return Err(LoadError::invalid("sparse accessor has more values than elements"));
            }
            let indices = self.sparse_indices(&sparse.indices, n)?;
            let values_layout = Layout { count: n, ..Layout::new(acc)? };
            let (data, stride) =
                self.view_tight(sparse.values.buffer_view, sparse.values.byte_offset, &values_layout)?;
            let mut values = vec![pad; n];
            fill(&mut values, data, stride, &values_layout);
            for (i, v) in indices.into_iter().zip(values) {
                let slot = out.get_mut(i as usize).ok_or_else(|| LoadError::invalid("sparse index out of range"))?;
                *slot = v;
            }
        }
        Ok(out)
    }

    /// Reads a scalar accessor of unsigned integers, such as indices,
    /// exactly (floats would round values above 2^24).
    pub fn read_u32(&self, index: usize) -> Result<Vec<u32>, LoadError> {
        let acc = self.get(index)?;
        let layout = Layout::new(acc)?;
        if layout.ncomp != 1 || !matches!(layout.comp, Comp::U8 | Comp::U16 | Comp::U32) {
            return Err(LoadError::invalid(format!("accessor {index} isn't unsigned integer scalars")));
        }
        let source = acc.buffer_view.map(|view| self.view(view, acc.byte_offset, &layout)).transpose()?;
        self.ctx.reserve((layout.count as u64).saturating_mul(4))?;
        let mut out = match source {
            Some((data, stride)) => uints(data, stride, layout.comp, layout.count),
            None => vec![0; layout.count],
        };
        if let Some(sparse) = &acc.sparse {
            let n = usize::try_from(sparse.count).map_err(|_| LoadError::invalid("sparse count out of range"))?;
            if n > layout.count {
                return Err(LoadError::invalid("sparse accessor has more values than elements"));
            }
            let indices = self.sparse_indices(&sparse.indices, n)?;
            let values_layout = Layout { count: n, ..Layout::new(acc)? };
            let (data, stride) =
                self.view_tight(sparse.values.buffer_view, sparse.values.byte_offset, &values_layout)?;
            for (i, v) in indices.into_iter().zip(uints(data, stride, layout.comp, n)) {
                let slot = out.get_mut(i as usize).ok_or_else(|| LoadError::invalid("sparse index out of range"))?;
                *slot = v;
            }
        }
        Ok(out)
    }

    /// The bytes holding `layout.count` elements of a view starting at
    /// `offset`, and the stride between elements.
    fn view(&self, view: usize, offset: u64, layout: &Layout) -> Result<(&[u8], usize), LoadError> {
        let json =
            self.view_json.get(view).ok_or_else(|| LoadError::invalid(format!("buffer view {view} doesn't exist")))?;
        let stride = match json.byte_stride {
            Some(s) if s > 0 => usize::try_from(s).map_err(|_| LoadError::invalid("byteStride out of range"))?,
            _ => layout.elem_size,
        };
        if stride < layout.elem_size {
            return Err(LoadError::invalid(format!("buffer view {view}'s stride is smaller than its elements")));
        }
        self.range(view, offset, stride, layout)
    }

    /// Like `view`, for sparse values, which are always tightly packed.
    fn view_tight(&self, view: usize, offset: u64, layout: &Layout) -> Result<(&[u8], usize), LoadError> {
        self.range(view, offset, layout.elem_size, layout)
    }

    fn range(&self, view: usize, offset: u64, stride: usize, layout: &Layout) -> Result<(&[u8], usize), LoadError> {
        let data = self
            .views
            .get(view)
            .and_then(|v| v.as_ref())
            .ok_or_else(|| LoadError::invalid(format!("buffer view {view} has no data")))?
            .bytes();
        if layout.count == 0 {
            return Ok((&[], stride));
        }
        let span = (layout.count - 1)
            .checked_mul(stride)
            .and_then(|s| s.checked_add(layout.elem_size))
            .and_then(|s| s.checked_add(usize::try_from(offset).ok()?));
        let start = offset as usize;
        match span {
            Some(end) if end <= data.len() => Ok((&data[start..end], stride)),
            _ => Err(LoadError::invalid(format!("an accessor reads past the end of buffer view {view}"))),
        }
    }

    fn sparse_indices(&self, json: &crate::json::SparseIndicesJ, n: usize) -> Result<Vec<u32>, LoadError> {
        let comp = Comp::from_gl(json.component_type).filter(|c| matches!(c, Comp::U8 | Comp::U16 | Comp::U32));
        let comp = comp.ok_or_else(|| LoadError::invalid("bad sparse index type"))?;
        let layout = Layout { comp, normalized: false, count: n, ncomp: 1, offsets: [0; 16], elem_size: comp.size() };
        let (data, stride) = self.view_tight(json.buffer_view, json.byte_offset, &layout)?;
        Ok(uints(data, stride, comp, n))
    }
}

/// Reads `count` unsigned integers of type `comp`, `stride` bytes apart.
/// `data` must hold them all, which `Accessors::range` ensures.
fn uints(data: &[u8], stride: usize, comp: Comp, count: usize) -> Vec<u32> {
    let at = |i: usize| i * stride;
    match comp {
        Comp::U8 => (0..count).map(|i| data[at(i)] as u32).collect(),
        Comp::U16 => (0..count).map(|i| u16::from_le_bytes([data[at(i)], data[at(i) + 1]]) as u32).collect(),
        _ => (0..count).map(|i| u32_le(&data[at(i)..])).collect(),
    }
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Copies elements from `data` into `out`, converting each component.  The
/// component type is matched once, outside the loop.
fn fill<const N: usize>(out: &mut [[f32; N]], data: &[u8], stride: usize, layout: &Layout) {
    let n = layout.ncomp.min(N);
    let offsets = &layout.offsets[..n];
    let norm = layout.normalized;
    macro_rules! run {
        ($size:expr, $conv:expr) => {
            for (i, dst) in out.iter_mut().enumerate() {
                let elem = &data[i * stride..i * stride + layout.elem_size];
                for (c, &o) in offsets.iter().enumerate() {
                    let b: [u8; $size] = elem[o..o + $size].try_into().unwrap_or([0; $size]);
                    dst[c] = $conv(b);
                }
            }
        };
    }
    match (layout.comp, norm) {
        (Comp::F32, _) => run!(4, f32::from_le_bytes),
        (Comp::I8, false) => run!(1, |b: [u8; 1]| b[0] as i8 as f32),
        (Comp::I8, true) => run!(1, |b: [u8; 1]| (b[0] as i8 as f32 / 127.0).max(-1.0)),
        (Comp::U8, false) => run!(1, |b: [u8; 1]| b[0] as f32),
        (Comp::U8, true) => run!(1, |b: [u8; 1]| b[0] as f32 / 255.0),
        (Comp::I16, false) => run!(2, |b| i16::from_le_bytes(b) as f32),
        (Comp::I16, true) => run!(2, |b| (i16::from_le_bytes(b) as f32 / 32767.0).max(-1.0)),
        (Comp::U16, false) => run!(2, |b| u16::from_le_bytes(b) as f32),
        (Comp::U16, true) => run!(2, |b| u16::from_le_bytes(b) as f32 / 65535.0),
        (Comp::U32, _) => run!(4, |b| u32::from_le_bytes(b) as f32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{SparseIndicesJ, SparseJ, SparseValuesJ};

    fn acc(view: Option<usize>, offset: u64, ct: u32, kind: &str, count: u64, normalized: bool) -> AccessorJ {
        AccessorJ {
            buffer_view: view,
            byte_offset: offset,
            component_type: ct,
            normalized,
            count,
            kind: kind.into(),
            sparse: None,
        }
    }

    fn bv(stride: Option<u64>) -> BufferViewJ {
        BufferViewJ { byte_stride: stride, ..Default::default() }
    }

    #[test]
    fn components_normalization_and_stride() {
        let ctx = Ctx::new(1 << 20, 0);
        // Two elements of [i16; 3] with a 8-byte stride, normalized.
        let mut bytes = Vec::new();
        for v in [[32767i16, -32768, 0], [16384, 1, -1]] {
            v.iter().for_each(|x| bytes.extend(x.to_le_bytes()));
            bytes.extend([0xFF, 0xFF]);
        }
        let views = [Some(ViewData::Borrowed(&bytes))];
        let json = [acc(Some(0), 0, 5122, "VEC3", 2, true), acc(Some(0), 0, 5122, "VEC3", 2, false)];
        let view_json = [bv(Some(8))];
        let a = Accessors { json: &json, views: &views, view_json: &view_json, ctx: &ctx };
        let v = a.read::<3>(0, [0.0; 3]).unwrap();
        assert_eq!(v[0], [1.0, -1.0, 0.0]);
        assert!((v[1][0] - 16384.0 / 32767.0).abs() < 1e-6);
        let raw = a.read::<3>(1, [0.0; 3]).unwrap();
        assert_eq!(raw[1], [16384.0, 1.0, -1.0]);
        // Padding fills missing components.
        let padded = a.read::<4>(1, [0.0, 0.0, 0.0, 1.0]).unwrap();
        assert_eq!(padded[0][3], 1.0);
        // Too many components for the request.
        assert!(a.read::<2>(0, [0.0; 2]).is_err());
    }

    #[test]
    fn out_of_range_reads_fail() {
        let ctx = Ctx::new(1 << 20, 0);
        let bytes = [0u8; 16];
        let views = [Some(ViewData::Borrowed(&bytes))];
        let view_json = [bv(None)];
        for a in [
            acc(Some(0), 0, 5126, "VEC3", 2, false),
            acc(Some(0), 8, 5126, "VEC2", 2, false),
            acc(Some(0), u64::MAX, 5126, "SCALAR", 1, false),
            acc(Some(0), 0, 5126, "SCALAR", u64::MAX, false),
            acc(Some(1), 0, 5126, "SCALAR", 1, false),
            acc(Some(0), 0, 1234, "SCALAR", 1, false),
            acc(Some(0), 0, 5126, "VEC5", 1, false),
        ] {
            let json = [a];
            let r = Accessors { json: &json, views: &views, view_json: &view_json, ctx: &ctx };
            assert!(r.read::<4>(0, [0.0; 4]).is_err());
        }
    }

    #[test]
    fn zero_filled_accessors_are_budgeted() {
        let ctx = Ctx::new(1000, 0);
        let json = [acc(None, 0, 5126, "VEC3", 10, false), acc(None, 0, 5126, "VEC3", 1 << 40, false)];
        let a = Accessors { json: &json, views: &[], view_json: &[], ctx: &ctx };
        assert_eq!(a.read::<3>(0, [0.0; 3]).unwrap(), vec![[0.0; 3]; 10]);
        assert!(matches!(a.read::<3>(1, [0.0; 3]), Err(LoadError::TooLarge(_))));
    }

    #[test]
    fn sparse_substitution() {
        let ctx = Ctx::new(1 << 20, 0);
        // Indices [1, 3] as u16, then values [[1,2,3],[4,5,6]] as f32.
        let mut bytes = Vec::new();
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        for v in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            bytes.extend(v.to_le_bytes());
        }
        let views = [Some(ViewData::Borrowed(&bytes[..4])), Some(ViewData::Borrowed(&bytes[4..]))];
        let mut a = acc(None, 0, 5126, "VEC3", 4, false);
        a.sparse = Some(SparseJ {
            count: 2,
            indices: SparseIndicesJ { buffer_view: 0, byte_offset: 0, component_type: 5123 },
            values: SparseValuesJ { buffer_view: 1, byte_offset: 0 },
        });
        let json = [a];
        let view_json = [bv(None), bv(None)];
        let r = Accessors { json: &json, views: &views, view_json: &view_json, ctx: &ctx };
        let v = r.read::<3>(0, [0.0; 3]).unwrap();
        assert_eq!(v, vec![[0.0; 3], [1.0, 2.0, 3.0], [0.0; 3], [4.0, 5.0, 6.0]]);
    }

    #[test]
    fn matrix_columns_are_padded() {
        let ctx = Ctx::new(1 << 20, 0);
        // A MAT3 of i8: three columns of 3 bytes, each padded to 4.
        let bytes = [1u8, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        let views = [Some(ViewData::Borrowed(&bytes))];
        let json = [acc(Some(0), 0, 5120, "MAT3", 1, false)];
        let view_json = [bv(None)];
        let r = Accessors { json: &json, views: &views, view_json: &view_json, ctx: &ctx };
        assert_eq!(r.read::<9>(0, [0.0; 9]).unwrap()[0], [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn indices() {
        let ctx = Ctx::new(1 << 20, 0);
        let bytes = [0u8, 0, 1, 0, 2, 0, 0, 0];
        let views = [Some(ViewData::Borrowed(&bytes))];
        let json = [acc(Some(0), 0, 5123, "SCALAR", 3, false), acc(Some(0), 0, 5121, "SCALAR", 8, false)];
        let view_json = [bv(None)];
        let r = Accessors { json: &json, views: &views, view_json: &view_json, ctx: &ctx };
        assert_eq!(r.read_u32(0).unwrap(), vec![0, 1, 2]);
        assert_eq!(r.read_u32(1).unwrap(), vec![0, 0, 1, 0, 2, 0, 0, 0]);
    }
}
