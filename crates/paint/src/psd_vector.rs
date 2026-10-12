//! Photoshop's shape layers: a solid colour fill (`SoCo`) cut out by a
//! vector mask (`vmsk`/`vsms`), which Photoshop stores as paths rather than
//! pixels.  The colour comes from the fill's action descriptor, the paths
//! from the mask's path records (cubic Bézier knots in fractions of the
//! document's size); both are drawn here so shape layers show when the
//! file stores no pixels for them, and vector masks apply when the file
//! stores no rendered copy.  Gradient and pattern fills and outlines
//! (strokes) aren't drawn.

/// A vector mask: path components combined in order by their operations.
#[derive(Clone, Debug, Default)]
pub struct VectorMask {
    components: Vec<Component>,
    /// The initial fill rule record: an empty mask shows everything.
    initial_fill: bool,
    pub invert: bool,
    pub disabled: bool,
}

/// Subpaths filled together by the even-odd rule, then merged with what
/// the components before made: 0 exclude, 1 union, 2 subtract, 3 intersect.
#[derive(Clone, Debug)]
struct Component {
    op: i16,
    /// Knots as (control in, anchor, control out) in document pixels, and
    /// whether the subpath is closed.
    subpaths: Vec<(bool, Vec<[f64; 6]>)>,
}

const MAX_KNOTS: usize = 1 << 16;
const MAX_DEPTH: u32 = 16;
const MAX_ITEMS: usize = 4096;
/// Sub-scanlines per pixel row; coverage across a row is exact.
const SUB: usize = 16;

/// Reads `vmsk`/`vsms` data: version, flags, then 26-byte path records.
/// Each knot is (control in, anchor, control out) as (y, x) pairs of
/// 8.24 fixed-point fractions of the document's height and width.
pub fn parse_vector_mask(data: &[u8], doc_w: u32, doc_h: u32) -> Option<VectorMask> {
    let flags = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?);
    let mut mask = VectorMask { invert: flags & 1 != 0, disabled: flags & 4 != 0, ..Default::default() };
    let fixed = |b: &[u8]| i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64 / (1 << 24) as f64;
    let mut remaining = 0usize;
    let mut knots = 0usize;
    for rec in data.get(8..)?.chunks_exact(26) {
        let selector = u16::from_be_bytes([rec[0], rec[1]]);
        match selector {
            0 | 3 => {
                remaining = u16::from_be_bytes([rec[2], rec[3]]) as usize;
                let op = i16::from_be_bytes([rec[4], rec[5]]);
                let sub = (selector == 0, Vec::with_capacity(remaining.min(1024)));
                match mask.components.last_mut() {
                    Some(c) if op == -1 => c.subpaths.push(sub),
                    _ => mask.components.push(Component { op, subpaths: vec![sub] }),
                }
            }
            1 | 2 | 4 | 5 if remaining > 0 => {
                knots += 1;
                if knots > MAX_KNOTS {
                    return None;
                }
                let p = |i: usize| {
                    let y = fixed(&rec[2 + i * 8..6 + i * 8]) * doc_h as f64;
                    let x = fixed(&rec[6 + i * 8..10 + i * 8]) * doc_w as f64;
                    (x, y)
                };
                let (cin, anchor, cout) = (p(0), p(1), p(2));
                if let Some((_, path)) = mask.components.last_mut().and_then(|c| c.subpaths.last_mut()) {
                    path.push([cin.0, cin.1, anchor.0, anchor.1, cout.0, cout.1]);
                }
                remaining -= 1;
            }
            8 => mask.initial_fill = u16::from_be_bytes([rec[2], rec[3]]) != 0,
            _ => {}
        }
    }
    Some(mask)
}

impl VectorMask {
    /// Merges `plane` (coverage of one component) into `mask` as Photoshop
    /// does; a subtraction or intersection first works on the inverted
    /// empty mask.
    fn merge(mask: f32, plane: f32, op: i16, first: bool) -> f32 {
        match op {
            0 => mask + plane - 2.0 * mask * plane,
            2 => ((if first { 1.0 - mask } else { mask }) - plane).max(0.0),
            3 => (if first { 1.0 - mask } else { mask }) * plane,
            _ => mask + plane - mask * plane,
        }
    }

    fn start(&self) -> f32 {
        if self.initial_fill && self.components.is_empty() { 1.0 } else { 0.0 }
    }

    /// The mask's value away from every path.
    pub fn outside(&self) -> f32 {
        let mut m = self.start();
        for (i, c) in self.components.iter().enumerate() {
            m = Self::merge(m, 0.0, c.op, i == 0);
        }
        let m = m.clamp(0.0, 1.0);
        if self.invert { 1.0 - m } else { m }
    }

    /// The bounding box of the paths, in whole pixels.
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let pts = self
            .components
            .iter()
            .flat_map(|c| c.subpaths.iter())
            .flat_map(|(_, k)| k.iter())
            .flat_map(|k| [(k[0], k[1]), (k[2], k[3]), (k[4], k[5])]);
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in pts {
            if x.is_finite() && y.is_finite() {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
        (x0 <= x1 && y0 <= y1).then(|| {
            let c = |v: f64| v.clamp(-1e7, 1e7) as i32;
            (c(x0.floor()), c(y0.floor()), c(x1.ceil()), c(y1.ceil()))
        })
    }

    /// Coverage (0 to 1) of each pixel of the `w` × `h` rectangle whose
    /// top-left corner is (`x`, `y`) on the canvas.
    pub fn coverage(&self, x: i32, y: i32, w: usize, h: usize) -> Vec<f32> {
        let mut mask = vec![self.start(); w * h];
        for (i, c) in self.components.iter().enumerate() {
            let plane = fill_even_odd(&c.edges(), x as f64, y as f64, w, h);
            for (m, p) in mask.iter_mut().zip(plane) {
                *m = Self::merge(*m, p, c.op, i == 0);
            }
        }
        for m in &mut mask {
            *m = m.clamp(0.0, 1.0);
            if self.invert {
                *m = 1.0 - *m;
            }
        }
        mask
    }
}

impl Component {
    /// Line segments approximating the subpaths; open ones are closed with
    /// a straight line, as a fill closes them.
    fn edges(&self) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for (closed, path) in &self.subpaths {
            let n = path.len();
            if n < 2 {
                continue;
            }
            let curves = if *closed { n } else { n - 1 };
            for i in 0..curves {
                let (a, b) = (path[i], path[(i + 1) % n]);
                let pts = [(a[2], a[3]), (a[4], a[5]), (b[0], b[1]), (b[2], b[3])];
                // Enough steps that each is a pixel or two long at most.
                let len = (pts[1].0 - pts[0].0).hypot(pts[1].1 - pts[0].1)
                    + (pts[2].0 - pts[1].0).hypot(pts[2].1 - pts[1].1)
                    + (pts[3].0 - pts[2].0).hypot(pts[3].1 - pts[2].1);
                let steps = if len.is_finite() { ((len / 1.5).ceil() as usize).clamp(1, 512) } else { 1 };
                let mut prev = pts[0];
                for s in 1..=steps {
                    let t = s as f64 / steps as f64;
                    let u = 1.0 - t;
                    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let p = (
                        b0 * pts[0].0 + b1 * pts[1].0 + b2 * pts[2].0 + b3 * pts[3].0,
                        b0 * pts[0].1 + b1 * pts[1].1 + b2 * pts[2].1 + b3 * pts[3].1,
                    );
                    out.push([prev.0, prev.1, p.0, p.1]);
                    prev = p;
                }
            }
            if !closed {
                out.push([path[n - 1][2], path[n - 1][3], path[0][2], path[0][3]]);
            }
        }
        out.retain(|e| e.iter().all(|v| v.is_finite()) && e[1] != e[3]);
        out
    }
}

/// Area coverage of a polygon filled by the even-odd rule over a `w` × `h`
/// rectangle at (`x`, `y`): `SUB` sub-scanlines per row, each crossing
/// span's exact horizontal extent.
fn fill_even_odd(edges: &[[f64; 4]], x: f64, y: f64, w: usize, h: usize) -> Vec<f32> {
    let mut cov = vec![0f32; w * h];
    // Edges sorted by their top, and the ones crossing the current row.
    let mut sorted: Vec<[f64; 4]> = edges.to_vec();
    sorted.sort_by(|a, b| a[1].min(a[3]).total_cmp(&b[1].min(b[3])));
    let (mut next, mut active) = (0, Vec::<[f64; 4]>::new());
    let mut xs: Vec<f64> = Vec::new();
    let weight = 1.0 / SUB as f64;
    for row in 0..h {
        let (top, bottom) = (y + row as f64, y + row as f64 + 1.0);
        while next < sorted.len() && sorted[next][1].min(sorted[next][3]) < bottom {
            active.push(sorted[next]);
            next += 1;
        }
        active.retain(|e| e[1].max(e[3]) > top);
        if active.is_empty() {
            continue;
        }
        let line = &mut cov[row * w..(row + 1) * w];
        for s in 0..SUB {
            let sy = top + (s as f64 + 0.5) * weight;
            xs.clear();
            for e in &active {
                let (x0, y0, x1, y1) = (e[0], e[1], e[2], e[3]);
                if (y0 <= sy) != (y1 <= sy) {
                    xs.push(x0 + (sy - y0) / (y1 - y0) * (x1 - x0) - x);
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.chunks_exact(2) {
                let (a, b) = (pair[0].max(0.0), pair[1].min(w as f64));
                if a >= b {
                    continue;
                }
                let (ia, ib) = (a.floor() as usize, (b.ceil() as usize).min(w));
                for (px, c) in line.iter_mut().enumerate().take(ib).skip(ia) {
                    let span = b.min(px as f64 + 1.0) - a.max(px as f64);
                    if span > 0.0 {
                        *c += (span * weight) as f32;
                    }
                }
            }
        }
    }
    for c in &mut cov {
        *c = c.min(1.0);
    }
    cov
}

/// The colour of a solid-colour fill layer (`SoCo`): version, then an
/// action descriptor whose `Clr ` item is an RGB or grey colour.
pub fn parse_solid_color(data: &[u8]) -> Option<[f32; 3]> {
    let mut r = Cur { d: data, at: 4 };
    let items = descriptor(&mut r, 0)?;
    let colour = items.iter().find(|(k, _)| k == "Clr ")?;
    let Value::Object(class, fields) = &colour.1 else { return None };
    let get = |k: &str| {
        fields.iter().find(|(n, _)| n == k).and_then(|(_, v)| match v {
            Value::Number(x) => Some(*x),
            _ => None,
        })
    };
    match class.as_str() {
        "RGBC" => Some(
            [get("Rd  ")?, get("Grn ")?, get("Bl  ").or_else(|| get("Bl "))?]
                .map(|v| (v / 255.0).clamp(0.0, 1.0) as f32),
        ),
        "Grsc" => {
            // Grey is a percentage of ink.
            let g = (1.0 - get("Gry ")? / 100.0).clamp(0.0, 1.0) as f32;
            Some([g; 3])
        }
        _ => None,
    }
}

enum Value {
    Number(f64),
    Object(String, Vec<(String, Value)>),
    Other,
}

struct Cur<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A key: a length, or 0 for a four-character code.
    fn key(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let n = if n == 0 { 4 } else { n };
        if n > 1024 {
            return None;
        }
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn unicode(&mut self) -> Option<()> {
        let n = self.u32()? as usize;
        self.take(n.checked_mul(2)?).map(|_| ())
    }
}

fn descriptor(r: &mut Cur, depth: u32) -> Option<Vec<(String, Value)>> {
    if depth > MAX_DEPTH {
        return None;
    }
    r.unicode()?;
    r.key()?;
    let count = r.u32()? as usize;
    if count > MAX_ITEMS {
        return None;
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let key = r.key()?;
        let v = value(r, depth)?;
        out.push((key, v));
    }
    Some(out)
}

fn value(r: &mut Cur, depth: u32) -> Option<Value> {
    let kind: [u8; 4] = r.take(4)?.try_into().ok()?;
    Some(match &kind {
        b"doub" => Value::Number(r.f64()?),
        b"UntF" => {
            r.take(4)?;
            Value::Number(r.f64()?)
        }
        b"long" => Value::Number(r.u32()? as i32 as f64),
        b"comp" => {
            r.take(8)?;
            Value::Other
        }
        b"bool" => {
            r.take(1)?;
            Value::Other
        }
        b"TEXT" => {
            r.unicode()?;
            Value::Other
        }
        b"enum" => {
            r.key()?;
            r.key()?;
            Value::Other
        }
        b"type" | b"GlbC" => {
            r.unicode()?;
            r.key()?;
            Value::Other
        }
        b"Objc" | b"GlbO" => {
            // The class id comes before the items; read it to name the object.
            let start = r.at;
            r.unicode()?;
            let class = r.key()?;
            r.at = start;
            Value::Object(class, descriptor(r, depth + 1)?)
        }
        b"VlLs" => {
            let n = r.u32()? as usize;
            if n > MAX_ITEMS {
                return None;
            }
            for _ in 0..n {
                value(r, depth + 1)?;
            }
            Value::Other
        }
        b"UnFl" => {
            r.take(4)?;
            let n = r.u32()? as usize;
            r.take(n.checked_mul(8)?)?;
            Value::Other
        }
        b"tdta" | b"alis" | b"Pth " => {
            let n = r.u32()? as usize;
            r.take(n)?;
            Value::Other
        }
        // References and object arrays are complex and never hold a fill
        // colour: stop reading this descriptor.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knot(sel: u16, x: f64, y: f64) -> Vec<u8> {
        let mut r = sel.to_be_bytes().to_vec();
        let f = |v: f64| ((v * (1 << 24) as f64) as i32).to_be_bytes();
        for _ in 0..3 {
            r.extend_from_slice(&f(y));
            r.extend_from_slice(&f(x));
        }
        r
    }

    /// A closed subpath's length record: knot count and operation.
    fn closed(n: u16, op: i16) -> Vec<u8> {
        let mut r = vec![0u8, 0];
        r.extend_from_slice(&n.to_be_bytes());
        r.extend_from_slice(&op.to_be_bytes());
        r.resize(26, 0);
        r
    }

    fn square(d: &mut Vec<u8>, op: i16, a: f64, b: f64) {
        d.extend(closed(4, op));
        for (x, y) in [(a, a), (b, a), (b, b), (a, b)] {
            d.extend(knot(2, x, y));
        }
    }

    #[test]
    fn a_square_mask_covers_its_square() {
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0];
        square(&mut d, 1, 0.25, 0.75);
        let m = parse_vector_mask(&d, 8, 8).unwrap();
        assert_eq!(m.bounds(), Some((2, 2, 6, 6)));
        assert_eq!(m.outside(), 0.0);
        let c = m.coverage(0, 0, 8, 8);
        assert_eq!(c[3 * 8 + 3], 1.0);
        assert_eq!(c[0], 0.0);
        assert!((c.iter().sum::<f32>() - 16.0).abs() < 0.01);
        // Edges half way through pixels cover half of them.
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0];
        square(&mut d, 1, 0.3125, 0.6875);
        let c = parse_vector_mask(&d, 8, 8).unwrap().coverage(0, 0, 8, 8);
        assert!((c[2 * 8 + 3] - 0.5).abs() < 0.01 && (c[2 * 8 + 2] - 0.25).abs() < 0.01);
    }

    #[test]
    fn path_operations() {
        // A hole: two subpaths of one component, filled even-odd.
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0];
        square(&mut d, 1, 0.0, 1.0);
        square(&mut d, -1, 0.25, 0.75);
        let c = parse_vector_mask(&d, 8, 8).unwrap().coverage(0, 0, 8, 8);
        assert_eq!((c[0], c[3 * 8 + 3]), (1.0, 0.0));
        // Subtracting first cuts the square out of everything.
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0];
        square(&mut d, 2, 0.25, 0.75);
        let m = parse_vector_mask(&d, 8, 8).unwrap();
        assert_eq!(m.outside(), 1.0);
        let c = m.coverage(0, 0, 8, 8);
        assert_eq!((c[0], c[3 * 8 + 3]), (1.0, 0.0));
        // Inverted, and an empty mask that starts filled.
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 1];
        square(&mut d, 1, 0.25, 0.75);
        let m = parse_vector_mask(&d, 8, 8).unwrap();
        assert_eq!((m.outside(), m.coverage(0, 0, 8, 8)[3 * 8 + 3]), (1.0, 0.0));
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0, 0, 8, 0, 1];
        d.resize(34, 0);
        let m = parse_vector_mask(&d, 8, 8).unwrap();
        assert_eq!((m.outside(), m.bounds()), (1.0, None));
        // Damaged: knots without a length record, a truncated record.
        assert!(parse_vector_mask(&[0, 0, 0, 3], 8, 8).is_none());
        let mut d = vec![0, 0, 0, 3, 0, 0, 0, 0];
        d.extend(knot(2, 0.5, 0.5));
        d.extend_from_slice(&closed(9, 1)[..10]);
        assert_eq!(parse_vector_mask(&d, 8, 8).unwrap().outside(), 0.0);
    }

    #[test]
    fn solid_colour_descriptor() {
        let mut d = 16u32.to_be_bytes().to_vec();
        let key = |d: &mut Vec<u8>, k: &[u8; 4]| {
            d.extend_from_slice(&0u32.to_be_bytes());
            d.extend_from_slice(k);
        };
        // Descriptor: name, class, one item `Clr ` = Objc RGBC {Rd, Grn, Bl}.
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(&[0, 0]);
        key(&mut d, b"null");
        d.extend_from_slice(&1u32.to_be_bytes());
        key(&mut d, b"Clr ");
        d.extend_from_slice(b"Objc");
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(&[0, 0]);
        key(&mut d, b"RGBC");
        d.extend_from_slice(&3u32.to_be_bytes());
        for (k, v) in [(b"Rd  ", 255.0f64), (b"Grn ", 0.0), (b"Bl  ", 51.0)] {
            key(&mut d, k);
            d.extend_from_slice(b"doub");
            d.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(parse_solid_color(&d), Some([1.0, 0.0, 0.2]));
        assert_eq!(parse_solid_color(&d[..20]), None);
    }
}
