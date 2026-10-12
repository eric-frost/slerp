//! Classic NetCDF (CDF-1, CDF-2 and CDF-5): a header of dimensions,
//! attributes and variables, then big-endian data.  Variables with two to
//! four dimensions are the file's pictures; the leading dimensions become
//! slices or time points, and latitude grids that count upwards are shown
//! with north up.
//!
//! NetCDF-4 files are HDF5 files and are read by [`crate::hdf5`].

use crate::dtype::{DType, Endian};
use crate::source::{ByteSource, FileSource, read_exact_at};
use crate::volume::{Axis, Role, Volume};
use slerp_scene::LoadError;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Largest header read.
const MAX_HEADER: u64 = 64 << 20;
const MAX_ITEMS: u64 = 1 << 16;

#[derive(Clone, Debug)]
pub struct Dim {
    pub name: String,
    /// 0 for the record (unlimited) dimension.
    pub len: u64,
}

#[derive(Clone, Debug)]
pub enum AttrValue {
    Text(String),
    Numbers(Vec<f64>),
}

#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub dims: Vec<usize>,
    pub attrs: Vec<(String, AttrValue)>,
    pub nc_type: u32,
    pub vsize: u64,
    pub begin: u64,
}

#[derive(Clone, Debug)]
pub struct Header {
    pub version: u8,
    pub numrecs: u64,
    pub dims: Vec<Dim>,
    pub attrs: Vec<(String, AttrValue)>,
    pub vars: Vec<Var>,
}

/// A cursor over header bytes that never reads past the end.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
    wide: bool,
}

impl Cur<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("the header ends early")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap_or([0; 4])))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap_or([0; 8])))
    }

    /// A count: 4 bytes, or 8 in CDF-5.
    fn count(&mut self) -> Result<u64, String> {
        if self.wide { self.u64() } else { Ok(self.u32()? as u64) }
    }

    fn name(&mut self) -> Result<String, String> {
        let n = self.count()?;
        if n > 1 << 20 {
            return Err("a name is absurdly long".into());
        }
        let n = n as usize;
        let s = String::from_utf8_lossy(self.take(n)?).into_owned();
        self.take((4 - n % 4) % 4)?;
        Ok(s)
    }
}

fn type_size(t: u32) -> Option<usize> {
    Some(match t {
        1 | 2 | 7 => 1,
        3 | 8 => 2,
        4 | 5 | 9 => 4,
        6 | 10 | 11 => 8,
        _ => return None,
    })
}

fn dtype_of(t: u32) -> Option<DType> {
    Some(match t {
        1 => DType::I8,
        7 => DType::U8,
        3 => DType::I16,
        8 => DType::U16,
        4 => DType::I32,
        9 => DType::U32,
        5 => DType::F32,
        6 => DType::F64,
        10 => DType::I64,
        11 => DType::U64,
        _ => return None,
    })
}

fn attributes(c: &mut Cur) -> Result<Vec<(String, AttrValue)>, String> {
    let tag = c.u32()?;
    let n = c.count()?;
    if tag == 0 {
        return Ok(Vec::new());
    }
    if tag != 12 || n > MAX_ITEMS {
        return Err("the attribute list is damaged".into());
    }
    let mut out = Vec::new();
    for _ in 0..n {
        let name = c.name()?;
        let t = c.u32()?;
        let count = c.count()?;
        let size = type_size(t).ok_or("an attribute has an unknown type")?;
        let bytes =
            count.checked_mul(size as u64).filter(|&b| b < 1 << 24).ok_or("an attribute is absurdly large")? as usize;
        let data = c.take(bytes)?.to_vec();
        c.take((4 - bytes % 4) % 4)?;
        let value = if t == 2 {
            AttrValue::Text(String::from_utf8_lossy(&data).trim_end_matches('\0').to_string())
        } else {
            let dt = dtype_of(t).ok_or("an attribute has an unknown type")?;
            AttrValue::Numbers(data.chunks_exact(size).map(|b| crate::dtype::decode_f64(dt, Endian::Big, b)).collect())
        };
        out.push((name, value));
    }
    Ok(out)
}

/// Parses a classic NetCDF header.
pub fn parse_header(b: &[u8]) -> Result<Header, String> {
    if b.len() < 8 || &b[..3] != b"CDF" || !matches!(b[3], 1 | 2 | 5) {
        return Err("this isn't a classic NetCDF file".into());
    }
    let version = b[3];
    let mut c = Cur { b, at: 4, wide: version == 5 };
    let numrecs = c.count()?;
    let numrecs = if numrecs == 0xFFFF_FFFF || numrecs == u64::MAX { 0 } else { numrecs };
    // Dimensions.
    let tag = c.u32()?;
    let n = c.count()?;
    let mut dims = Vec::new();
    if tag == 10 {
        if n > MAX_ITEMS {
            return Err("the file has too many dimensions".into());
        }
        for _ in 0..n {
            let name = c.name()?;
            let len = c.count()?;
            dims.push(Dim { name, len });
        }
    } else if tag != 0 {
        return Err("the dimension list is damaged".into());
    }
    let attrs = attributes(&mut c)?;
    // Variables.
    let tag = c.u32()?;
    let n = c.count()?;
    let mut vars = Vec::new();
    if tag == 11 {
        if n > MAX_ITEMS {
            return Err("the file has too many variables".into());
        }
        for _ in 0..n {
            let name = c.name()?;
            let nd = c.count()?;
            if nd > 16 {
                return Err("a variable has too many dimensions".into());
            }
            let mut vdims = Vec::new();
            for _ in 0..nd {
                let id = c.count()? as usize;
                if id >= dims.len() {
                    return Err("a variable names a dimension that doesn't exist".into());
                }
                vdims.push(id);
            }
            let vattrs = attributes(&mut c)?;
            let nc_type = c.u32()?;
            let vsize = c.count()?;
            let begin = if version == 1 { c.u32()? as u64 } else { c.u64()? };
            vars.push(Var { name, dims: vdims, attrs: vattrs, nc_type, vsize, begin });
        }
    } else if tag != 0 {
        return Err("the variable list is damaged".into());
    }
    Ok(Header { version, numrecs, dims, attrs, vars })
}

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 4 && &head[..3] == b"CDF" && matches!(head[3], 1 | 2 | 5)
}

impl Header {
    fn is_record(&self, v: &Var) -> bool {
        v.dims.first().is_some_and(|&d| self.dims[d].len == 0)
    }

    /// The bytes of one record of all record variables together.
    fn record_size(&self) -> u64 {
        self.vars.iter().filter(|v| self.is_record(v)).map(|v| v.vsize).sum()
    }

    fn only_record_var(&self) -> bool {
        self.vars.iter().filter(|v| self.is_record(v)).count() == 1
    }

    fn lens(&self, v: &Var) -> Vec<u64> {
        v.dims
            .iter()
            .enumerate()
            .map(|(i, &d)| if i == 0 && self.dims[d].len == 0 { self.numrecs } else { self.dims[d].len })
            .collect()
    }

    /// Variables shown as pictures: numeric, with at least two dimensions
    /// of at least four each at the end, and not coordinate variables.
    pub fn picture_vars(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for (i, v) in self.vars.iter().enumerate() {
            let lens = self.lens(v);
            let n = lens.len();
            if !(2..=4).contains(&n) || dtype_of(v.nc_type).is_none() || lens[n - 2] < 4 || lens[n - 1] < 4 {
                continue;
            }
            if self.dims.iter().any(|d| d.name == v.name) {
                continue;
            }
            out.push(i);
        }
        out
    }
}

/// Reads variable data where records of several variables interleave.
struct RecordSource {
    file: File,
    begin: u64,
    /// Bytes of one record of this variable, and the distance between
    /// records.
    inner: u64,
    recsize: u64,
    records: u64,
}

impl ByteSource for RecordSource {
    fn len(&self) -> u64 {
        self.inner * self.records
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(crate::source::eof)?;
        if end > self.len() {
            return Err(crate::source::eof());
        }
        let (mut at, mut done) = (offset, 0usize);
        while done < buf.len() {
            let rec = at / self.inner;
            let inside = at % self.inner;
            let n = ((self.inner - inside) as usize).min(buf.len() - done);
            read_exact_at(&self.file, &mut buf[done..done + n], self.begin + rec * self.recsize + inside)?;
            at += n as u64;
            done += n;
        }
        Ok(())
    }
}

fn is_lat(name: &str, units: &str) -> bool {
    let n = name.to_ascii_lowercase();
    units.contains("degrees_north")
        || units.contains("degree_north")
        || units.contains("degrees_N")
        || n == "lat"
        || n == "latitude"
        || n == "y"
}

fn text(attrs: &[(String, AttrValue)], key: &str) -> String {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| if let AttrValue::Text(t) = v { Some(t.clone()) } else { None })
        .unwrap_or_default()
}

fn number(attrs: &[(String, AttrValue)], key: &str) -> Option<f64> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| if let AttrValue::Numbers(n) = v { n.first().copied() } else { None })
}

/// Roles for the leading dimensions of a picture variable: time points, then
/// slices.
pub fn leading_role(name: &str, taken_t: bool) -> Role {
    let n = name.to_ascii_lowercase();
    if !taken_t && (n.contains("time") || n == "t" || n.starts_with("t_") || n == "frame" || n == "step") {
        Role::T
    } else {
        Role::Z
    }
}

/// Roles for the leading dimensions of a picture variable: a dimension named
/// like time is the time axis; of the others the last is the slice axis and
/// a first one beside it counts as a volume (another time-like axis).
fn assign_leading_roles(lead: &mut [Axis]) {
    let time = lead.iter().position(|a| leading_role(&a.name, false) == Role::T);
    let mut rest: Vec<usize> = (0..lead.len()).filter(|&i| Some(i) != time).collect();
    for a in lead.iter_mut() {
        a.role = Role::Other;
    }
    if let Some(t) = time {
        lead[t].role = Role::T;
    } else if rest.len() >= 2 {
        // No dimension says it is time: the outermost one steps through volumes.
        lead[rest.remove(0)].role = Role::T;
    }
    if let Some(&z) = rest.last() {
        lead[z].role = Role::Z;
    }
}

/// Opens a NetCDF file's variable `part` (a picture variable's name), or the
/// first picture variable.
pub fn open(path: &Path, part: Option<&str>) -> Result<(Volume, Vec<String>, String), LoadError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut buf = vec![0u8; len.min(MAX_HEADER) as usize];
    read_exact_at(&file, &mut buf, 0)?;
    let h = parse_header(&buf).map_err(LoadError::Invalid)?;
    let pics = h.picture_vars();
    let names: Vec<String> = pics.iter().map(|&i| h.vars[i].name.clone()).collect();
    let idx = match part {
        Some(p) => pics
            .iter()
            .copied()
            .find(|&i| h.vars[i].name == p)
            .ok_or_else(|| LoadError::Unsupported(format!("no variable called {p}")))?,
        None => *pics
            .first()
            .ok_or_else(|| LoadError::Unsupported("the file has no variable that looks like a picture".into()))?,
    };
    let v = &h.vars[idx];
    let dt = dtype_of(v.nc_type).ok_or_else(|| LoadError::Unsupported("the variable's type".into()))?;
    let lens = h.lens(v);
    let esize = dt.size() as u64;
    let total: u64 = lens
        .iter()
        .try_fold(1u64, |a, &l| a.checked_mul(l))
        .ok_or_else(|| LoadError::Invalid("the variable is absurdly large".into()))?;
    let bytes = total.checked_mul(esize).ok_or_else(|| LoadError::Invalid("the variable is absurdly large".into()))?;
    let source: Arc<dyn ByteSource> = if h.is_record(v) {
        let inner = bytes / h.numrecs.max(1);
        let recsize = if h.only_record_var() { inner } else { h.record_size() };
        Arc::new(RecordSource { file: File::open(path)?, begin: v.begin, inner, recsize, records: h.numrecs })
    } else {
        if v.begin.checked_add(bytes).is_none() {
            return Err(LoadError::Invalid("the variable lies outside the file".into()));
        }
        Arc::new(FileSource::new(File::open(path)?, v.begin, len.saturating_sub(v.begin)))
    };
    // Axes: strides in elements, last dimension fastest.
    let n = lens.len();
    let mut axes = Vec::new();
    let mut stride = 1usize;
    let mut rev = Vec::new();
    for i in (0..n).rev() {
        let l = usize::try_from(lens[i]).map_err(|_| LoadError::Invalid("a dimension is absurdly large".into()))?;
        let dname = &h.dims[v.dims[i]].name;
        let role = if i == n - 1 {
            Role::X
        } else if i == n - 2 {
            Role::Y
        } else {
            Role::Other
        };
        let mut a = Axis::new(l, stride, role).named(dname.clone());
        a.spacing = 0.0;
        rev.push(a);
        stride =
            stride.checked_mul(l.max(1)).ok_or_else(|| LoadError::Invalid("the variable is absurdly large".into()))?;
    }
    rev.reverse();
    axes.extend(rev);
    assign_leading_roles(&mut axes[..n.saturating_sub(2)]);
    let mut vol = Volume::new(v.name.clone(), format!("NetCDF CDF-{}", h.version), axes, dt, Endian::Big, source);
    // Latitude grids that count upwards are drawn with north up.
    if n >= 2 {
        let lat_dim = h.dims[v.dims[n - 2]].name.clone();
        let units = h.vars.iter().find(|c| c.name == lat_dim).map(|c| text(&c.attrs, "units")).unwrap_or_default();
        if is_lat(&lat_dim, &units)
            && let Some(c) = h.vars.iter().find(|c| c.name == lat_dim && c.dims.len() == 1)
            && let Some(cdt) = dtype_of(c.nc_type)
        {
            let cl = h.dims[c.dims[0]].len;
            if cl >= 2 {
                let sz = cdt.size() as u64;
                let (mut a, mut b2) = ([0u8; 8], [0u8; 8]);
                if read_exact_at(&file, &mut a[..sz as usize], c.begin).is_ok()
                    && read_exact_at(&file, &mut b2[..sz as usize], c.begin + (cl - 1) * sz).is_ok()
                {
                    let first = crate::dtype::decode_f64(cdt, Endian::Big, &a[..sz as usize]);
                    let last = crate::dtype::decode_f64(cdt, Endian::Big, &b2[..sz as usize]);
                    if last > first {
                        vol.axes[n - 2].reverse = true;
                    }
                }
            }
        }
    }
    vol.value_name = text(&v.attrs, "units");
    if let Some(s) = number(&v.attrs, "scale_factor").filter(|s| *s != 0.0) {
        vol.slope = s;
    }
    if let Some(o) = number(&v.attrs, "add_offset") {
        vol.inter = o;
    }
    vol.fill = number(&v.attrs, "_FillValue").or_else(|| number(&v.attrs, "missing_value"));
    vol.fact("Variable", v.name.clone());
    vol.fact("Long name", text(&v.attrs, "long_name"));
    vol.fact("Standard name", text(&v.attrs, "standard_name"));
    vol.fact("Units", text(&v.attrs, "units"));
    vol.fact(
        "Shape",
        lens.iter().zip(&v.dims).map(|(l, d)| format!("{} {}", h.dims[*d].name, l)).collect::<Vec<_>>().join(" x "),
    );
    for (k, val) in &h.attrs {
        match val {
            AttrValue::Text(t) if !t.is_empty() => {
                vol.facts.push((format!("Global {k}"), t.chars().take(300).collect()))
            }
            AttrValue::Numbers(n) if n.len() == 1 => vol.facts.push((format!("Global {k}"), n[0].to_string())),
            _ => {}
        }
    }
    for (k, val) in &v.attrs {
        if matches!(k.as_str(), "units" | "long_name" | "standard_name") {
            continue;
        }
        match val {
            AttrValue::Text(t) if !t.is_empty() => vol.facts.push((k.clone(), t.chars().take(300).collect())),
            AttrValue::Numbers(n) if !n.is_empty() => {
                vol.facts.push((k.clone(), n.iter().take(6).map(|x| x.to_string()).collect::<Vec<_>>().join(", ")))
            }
            _ => {}
        }
    }
    vol.fit().map_err(LoadError::Invalid)?;
    Ok((vol, names, v.name.clone()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a CDF-1 file with one fixed variable `v(y=4, x=5)` of shorts and a
    /// record variable `r(t, x)` of floats.
    pub fn sample() -> Vec<u8> {
        fn name(s: &str, out: &mut Vec<u8>) {
            out.extend((s.len() as u32).to_be_bytes());
            out.extend(s.as_bytes());
            out.extend(vec![0u8; (4 - s.len() % 4) % 4]);
        }
        let mut h = b"CDF\x01".to_vec();
        h.extend(3u32.to_be_bytes()); // numrecs
        h.extend(10u32.to_be_bytes());
        h.extend(3u32.to_be_bytes());
        for (n, l) in [("y", 4u32), ("x", 5), ("t", 0)] {
            name(n, &mut h);
            h.extend(l.to_be_bytes());
        }
        h.extend(0u32.to_be_bytes());
        h.extend(0u32.to_be_bytes()); // no global attributes
        h.extend(11u32.to_be_bytes());
        h.extend(2u32.to_be_bytes());
        let var_header_len = |nm: &str, nd: usize| 4 + nm.len() + (4 - nm.len() % 4) % 4 + 4 + 4 * nd + 8 + 4 + 4 + 4;
        let hdr = h.len() + var_header_len("v", 2) + var_header_len("r", 2);
        // v: shorts, 4 x 5 = 40 bytes; r: floats per record 5 = 20 bytes.
        for (nm, dims, ty, vsize, begin) in
            [("v", vec![0u32, 1], 3u32, 40u32, hdr as u32), ("r", vec![2u32, 1], 5, 20, (hdr + 40) as u32)]
        {
            name(nm, &mut h);
            h.extend((dims.len() as u32).to_be_bytes());
            for d in dims {
                h.extend(d.to_be_bytes());
            }
            h.extend(0u32.to_be_bytes());
            h.extend(0u32.to_be_bytes());
            h.extend(ty.to_be_bytes());
            h.extend(vsize.to_be_bytes());
            h.extend(begin.to_be_bytes());
        }
        assert_eq!(h.len(), hdr);
        for i in 0..20i16 {
            h.extend(i.to_be_bytes());
        }
        for rec in 0..3 {
            for x in 0..5 {
                h.extend(((rec * 10 + x) as f32).to_be_bytes());
            }
        }
        h
    }

    #[test]
    fn reads_fixed_and_record_variables() {
        let b = sample();
        let h = parse_header(&b).unwrap();
        assert_eq!(h.dims.len(), 3);
        assert_eq!(h.vars.len(), 2);
        assert_eq!(h.numrecs, 3);
        assert!(h.is_record(&h.vars[1]));
        assert_eq!(h.record_size(), 20);
        assert_eq!(h.lens(&h.vars[1]), vec![3, 5]);
    }

    #[test]
    fn rejects_garbage_headers() {
        let b = sample();
        for cut in [0, 3, 7, 12, 40, 100] {
            assert!(parse_header(&b[..cut.min(b.len())]).is_err() || cut >= b.len());
        }
        let mut bad = b.clone();
        bad[8] = 0xff;
        let _ = parse_header(&bad);
        assert!(parse_header(b"CDF\x03xxxxxxxxxxxx").is_err());
        assert!(parse_header(b"HDF5").is_err());
    }

    #[test]
    fn leading_dimensions_become_time_or_slices() {
        assert_eq!(leading_role("time", false), Role::T);
        assert_eq!(leading_role("time", true), Role::Z);
        assert_eq!(leading_role("lev", false), Role::Z);
        assert_eq!(leading_role("depth", false), Role::Z);
    }
}
