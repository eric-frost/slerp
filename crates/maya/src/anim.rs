//! Transform channels driven by animation curves, read at the scene's
//! current time.  Maya saves no value for an attribute something drives,
//! so an animated transform's pose exists only as its curves.
//!
//! An `animCurveTA`/`TL`/`TU` holds keys as time and value pairs (`.ktv`).
//! The value is the key's at the current time (`:time1.o`), or between two
//! keys interpolated linearly, which is exact at keys and close between
//! them; before the first key and after the last the curve holds.  Binary
//! files keep times in their own unit, so there the first key is used.

use crate::graph::Graph;

/// Most keys read from one curve.
const MAX_KEYS: usize = 1_000_000;

/// Whether a node is an animation curve with time input.
pub fn is_curve(kind: &str) -> bool {
    kind.starts_with("animCurveT") || matches!(kind, "PCTA" | "PCTL" | "PCTU")
}

/// The value of `node.attr` when an animation curve drives it.
pub fn driven(g: &Graph, node: usize, attr: &str, time: Option<f64>) -> Option<f64> {
    let c = g.input(node, attr)?;
    is_curve(&g.nodes[c.src].kind).then(|| evaluate(g, c.src, time)).flatten()
}

/// Whether something other than an animation curve drives `node.attr`
/// (a constraint, an expression, a blend), which Slerp can't evaluate.
pub fn driven_otherwise(g: &Graph, node: usize, attrs: &[&str]) -> bool {
    attrs.iter().any(|a| g.inputs(node, a).any(|c| !is_curve(&g.nodes[c.src].kind)))
}

/// The scene's current time in frames, for Maya ASCII files.
pub fn current_time(g: &Graph) -> Option<f64> {
    if !g.degrees {
        return None;
    }
    g.find("time1").and_then(|t| g.nodes[t].num("o"))
}

fn evaluate(g: &Graph, curve: usize, time: Option<f64>) -> Option<f64> {
    let keys = g.nodes[curve].dense("ktv", 2, MAX_KEYS)?;
    let keys: Vec<(f64, f64)> =
        keys.chunks_exact(2).map(|k| (k[0], k[1])).filter(|k| k.0.is_finite() && k.1.is_finite()).collect();
    let first = *keys.first()?;
    let Some(t) = time else { return Some(first.1) };
    if t <= first.0 {
        return Some(first.1);
    }
    for w in keys.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return Some(v0 + (v1 - v0) * f);
        }
    }
    keys.last().map(|k| k.1)
}
