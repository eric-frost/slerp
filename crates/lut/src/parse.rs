//! Reading `.cube` and `.3dl` text.  Sizes are checked against the file's
//! length before anything is allocated: every table entry needs at least
//! six bytes of text, so a file can't claim more entries than it could
//! hold.

use crate::{Curves, Lattice, Parsed};
use slerp_scene::{Info, LoadError};

/// The largest lattice: 256 points a side is 16.7 million entries, beyond
/// any real table (65 is the usual most).
const MAX_LATTICE: usize = 256;
const MAX_CURVE: usize = 1 << 16;

/// The file as text.  Latin-1 titles and comments are common; only the
/// numbers and keywords matter, so other bytes become replacement
/// characters.
fn text(bytes: &[u8]) -> Result<std::borrow::Cow<'_, str>, LoadError> {
    if bytes.contains(&0) {
        return Err(LoadError::invalid("not a text file"));
    }
    Ok(String::from_utf8_lossy(bytes))
}

/// The meaningful part of a line, without comments.
fn content(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim()
}

fn floats<const N: usize>(rest: &str) -> Option<[f32; N]> {
    let mut out = [0f32; N];
    let mut it = rest.split_whitespace();
    for slot in &mut out {
        *slot = it.next()?.parse().ok()?;
    }
    it.next().is_none().then_some(out)
}

/*┌─────────────────╮
| .cube           |
└─────────────────╯*/
//region .cube

pub fn cube(bytes: &[u8]) -> Result<Parsed, LoadError> {
    let text = text(bytes)?;
    let mut title = None;
    let (mut size1, mut size3) = (None::<usize>, None::<usize>);
    let (mut dmin, mut dmax) = ([0f32; 3], [1f32; 3]);
    let (mut range1, mut range3) = (None::<[f32; 2]>, None::<[f32; 2]>);
    let mut entries: Vec<[f32; 3]> = Vec::new();
    let mut warnings = Vec::new();
    let mut flags = Vec::new();
    let room = bytes.len() / 6 + 1;

    for (number, line) in text.lines().enumerate() {
        let line = content(line);
        if line.is_empty() {
            continue;
        }
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let bad = |what: &str| LoadError::invalid(format!("line {}: {what}", number + 1));
        let size = |rest: &str, max: usize| -> Result<usize, LoadError> {
            let n: usize = rest.parse().map_err(|_| bad("not a table size"))?;
            if !(2..=max).contains(&n) {
                return Err(LoadError::Unsupported(format!("a table of {n} entries a side")));
            }
            Ok(n)
        };
        match word {
            "TITLE" => title = Some(rest.trim_matches('"').to_string()),
            "LUT_1D_SIZE" => size1 = Some(size(rest, MAX_CURVE)?),
            "LUT_3D_SIZE" => size3 = Some(size(rest, MAX_LATTICE)?),
            "DOMAIN_MIN" => dmin = floats::<3>(rest).ok_or_else(|| bad("DOMAIN_MIN needs three numbers"))?,
            "DOMAIN_MAX" => dmax = floats::<3>(rest).ok_or_else(|| bad("DOMAIN_MAX needs three numbers"))?,
            "LUT_1D_INPUT_RANGE" => range1 = floats::<2>(rest),
            "LUT_3D_INPUT_RANGE" => range3 = floats::<2>(rest),
            w if w.starts_with(|c: char| c.is_ascii_alphabetic()) => flags.push(w.to_string()),
            _ => match floats::<3>(line) {
                Some(rgb) => {
                    let wanted = size1.unwrap_or(0) + size3.map_or(0, |n| n * n * n);
                    if entries.len() >= wanted.max(1) {
                        if wanted == 0 {
                            return Err(bad("table entries before the table size"));
                        }
                        warnings.push("the file holds more entries than its sizes say; the rest are ignored".into());
                        break;
                    }
                    if entries.is_empty() {
                        entries.reserve(wanted.min(room));
                    }
                    entries.push(rgb);
                }
                None => return Err(bad("expected three numbers")),
            },
        }
    }

    let n1 = size1.unwrap_or(0);
    let n3 = size3.map_or(0, |n| n * n * n);
    if n1 + n3 == 0 {
        return Err(LoadError::invalid("no LUT_1D_SIZE or LUT_3D_SIZE"));
    }
    if entries.len() < n1 + n3 {
        return Err(LoadError::invalid(format!("the table has {} of its {} entries", entries.len(), n1 + n3)));
    }
    let domain = |r: Option<[f32; 2]>, warnings: &mut Vec<String>| {
        let (lo, hi) = match r {
            Some([a, b]) => ([a; 3], [b; 3]),
            None => (dmin, dmax),
        };
        if (0..3).all(|c| hi[c] > lo[c] && lo[c].is_finite() && hi[c].is_finite()) {
            (lo, hi)
        } else {
            warnings.push("an empty input range; 0 to 1 used instead".into());
            ([0.0; 3], [1.0; 3])
        }
    };
    let curves = (n1 > 0).then(|| {
        let (min, max) = domain(range1, &mut warnings);
        Curves { min, max, entries: entries[..n1].to_vec() }
    });
    let lattice = size3.map(|size| {
        let (min, max) = domain(range3, &mut warnings);
        Lattice { size, min, max, axis: None, entries: entries[n1..n1 + n3].to_vec() }
    });

    let mut details = Vec::new();
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        details.push(("Title".to_string(), t));
    }
    if let Some(l) = &lattice {
        details.push(("3D table".into(), format!("{0} × {0} × {0}", l.size)));
        if l.min != [0.0; 3] || l.max != [1.0; 3] {
            details.push(("3D input range".into(), format!("{:?} to {:?}", l.min, l.max)));
        }
    }
    if let Some(c) = &curves {
        details.push(("1D table".into(), format!("{} entries", c.entries.len())));
        if c.min != [0.0; 3] || c.max != [1.0; 3] {
            details.push(("1D input range".into(), format!("{:?} to {:?}", c.min, c.max)));
        }
    }
    if !flags.is_empty() {
        details.push(("Other keywords".into(), flags.join(", ")));
    }
    let info = Info {
        format: format!("Cube LUT ({})", if lattice.is_some() { "3D" } else { "1D" }),
        generator: None,
        warnings,
        details,
        neighbours: Vec::new(),
    };
    Ok(Parsed { info, curves, lattice })
}

//endregion

/*┌─────────────────╮
| .3dl            |
└─────────────────╯*/
//region .3dl

/// The smallest common bit depth whose largest value is at least `max`.
fn depth_for(max: f64) -> u32 {
    [8u32, 10, 12, 14, 16].into_iter().find(|&b| ((1u64 << b) - 1) as f64 >= max).unwrap_or(16)
}

pub fn three_dl(bytes: &[u8]) -> Result<Parsed, LoadError> {
    let text = text(bytes)?;
    let mut mesh: Option<(u32, u32)> = None;
    let mut shaper: Option<Vec<f64>> = None;
    let mut rows: Vec<[f64; 3]> = Vec::new();
    let room = bytes.len() / 6 + 1;
    let limit = MAX_LATTICE * MAX_LATTICE * MAX_LATTICE;

    for (number, line) in text.lines().enumerate() {
        let line = content(line);
        if line.is_empty() {
            continue;
        }
        let bad = |what: &str| LoadError::invalid(format!("line {}: {what}", number + 1));
        let first = line.split_whitespace().next().unwrap_or("");
        // Keywords: 3DMESH, Mesh, LUT8, gamma.
        if first.parse::<f64>().is_err() {
            if first == "Mesh" {
                let v: Vec<u32> = line.split_whitespace().skip(1).filter_map(|t| t.parse().ok()).collect();
                if let [i, o] = v[..]
                    && (1..=8).contains(&i)
                    && (1..=32).contains(&o)
                {
                    mesh = Some((i, o));
                }
            }
            // 3DMESH, LUT8, gamma: nothing to keep.
            continue;
        }
        let numbers: Vec<f64> = line
            .split_whitespace()
            .map(|t| t.parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| bad("expected numbers"))?;
        if numbers.iter().any(|v| !v.is_finite()) {
            return Err(bad("not a number"));
        }
        match numbers.len() {
            3 => {
                if rows.len() >= limit {
                    return Err(LoadError::TooLarge("more entries than any table holds".into()));
                }
                if rows.is_empty() {
                    rows.reserve(room.min(limit).min(1 << 16));
                }
                rows.push([numbers[0], numbers[1], numbers[2]]);
            }
            n if n > 3 && shaper.is_none() && rows.is_empty() => {
                if n > MAX_LATTICE {
                    return Err(LoadError::Unsupported(format!("a {n}-point shaper")));
                }
                shaper = Some(numbers);
            }
            _ => return Err(bad("expected three numbers")),
        }
    }

    // 1. The lattice size: from the shaper line, the Mesh line, or the
    //    number of entries.
    let size = match (&shaper, mesh) {
        (Some(s), _) => s.len(),
        (None, Some((i, _))) => (1usize << i) + 1,
        (None, None) => (rows.len() as f64).cbrt().round() as usize,
    };
    if !(2..=MAX_LATTICE).contains(&size) {
        return Err(LoadError::invalid("no table size"));
    }
    let n3 = size * size * size;
    if rows.len() < n3 {
        return Err(LoadError::invalid(format!("the table has {} of its {} entries", rows.len(), n3)));
    }
    let mut warnings = Vec::new();
    if rows.len() > n3 {
        warnings.push("the file holds more entries than its size says; the rest are ignored".into());
    }

    // 2. Output scale: the Mesh line's bit depth, or the smallest depth
    //    that holds the largest value.
    let out_bits = match mesh {
        Some((_, o)) => o,
        None => depth_for(rows[..n3].iter().flatten().fold(0.0, |m, &v| v.max(m))),
    };
    let out_max = ((1u64 << out_bits.min(32)) - 1) as f64;

    // 3. Input positions: the shaper line, as fractions of its bit depth.
    //    Evenly spaced ones are dropped, so the usual case stays exact.
    let axis = shaper.as_ref().and_then(|s| {
        let in_max = ((1u64 << depth_for(s.iter().fold(0.0, |m, &v| v.max(m)))) - 1) as f64;
        let axis: Vec<f32> = s.iter().map(|&v| (v / in_max) as f32).collect();
        let even = axis.iter().enumerate().all(|(i, &a)| (a - i as f32 / (size - 1) as f32).abs() < 1e-4);
        let rising = axis.windows(2).all(|w| w[1] >= w[0]);
        (!even && rising && axis[0] <= 0.0 + 1e-6).then_some(axis)
    });

    // 4. Entries: blue changes fastest in the file; red does here.
    let mut entries = vec![[0f32; 3]; n3];
    for (i, row) in rows[..n3].iter().enumerate() {
        let (r, g, b) = (i / (size * size), (i / size) % size, i % size);
        entries[r + size * (g + size * b)] = row.map(|v| (v / out_max) as f32);
    }
    let mut details = vec![
        ("3D table".to_string(), format!("{0} × {0} × {0}", size)),
        ("Output depth".into(), format!("{out_bits} bits")),
    ];
    if axis.is_some() {
        details.push(("Input points".into(), "unevenly spaced (shaper line)".into()));
    }
    let info = Info { format: "3DL LUT".into(), warnings, details, ..Info::default() };
    let lattice = Lattice { size, min: [0.0; 3], max: [1.0; 3], axis, entries };
    Ok(Parsed { info, curves: None, lattice: Some(lattice) })
}

//endregion
