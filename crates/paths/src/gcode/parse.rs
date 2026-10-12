//! A streaming G-code interpreter: it reads one line at a time, keeps the
//! machine's state (position, extruder, absolute or relative modes, units,
//! arc plane) and records each move, so a file of any size is read without
//! holding its text.
//!
//! What it understands: `G0`/`G1` moves, `G2`/`G3` arcs in either the
//! centre (`I J K`) or radius (`R`) form on any plane (`G17`-`G19`),
//! absolute and relative positioning (`G90`/`G91`, `G90.1`/`G91.1` for arc
//! centres), absolute and relative extrusion (`M82`/`M83`), `G92` position
//! changes, `G28` homing, inches and millimetres (`G20`/`G21`), firmware
//! retraction (`G10`/`G11`, which move nothing), tool changes, modal moves
//! as CNC programs write them (axis words without a `G`), line numbers and
//! checksums, parenthesised comments, and G-code without spaces
//! (`G1X10Y5E.4`, as binary G-code's MeatPack leaves it).
//!
//! Layers come from the slicers' comments (`;LAYER_CHANGE`, `;LAYER:n`,
//! `; CHANGE_LAYER`, Simplify3D's `; layer n`), or, in files without them,
//! from the height of the extrusions.  Feature types come from `;TYPE:`,
//! `; FEATURE:` and `; feature`, and widths from `;WIDTH:` and
//! `; LINE_WIDTH:`; elsewhere the width follows from how much was
//! extruded.

use super::{Feature, Layer, Meta, Move, MoveKind, Toolpath};
use crate::text::{contains_ci, parse_duration, parse_number, starts_with_ci, trim};

/// Which moves a parse stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Every move, travel included, for the viewer.
    All,
    /// Extrusions and feed moves, for thumbnails, which never show travel.
    Drawn,
}

/// How far an arc's short moves may stray from the true arc, in
/// millimetres.
const ARC_TOLERANCE: f64 = 0.01;
/// The most short moves one arc becomes.
const ARC_MAX_STEPS: usize = 720;
/// Extrusions this much higher or lower than their layer's start a new
/// one, in files without layer comments.  Above a spiral vase's rise per
/// move, below any real layer height.
const LAYER_STEP: f32 = 0.03;
/// Lines whose comments are searched for the slicer's name.
const HEADER_LINES: u64 = 400;

pub struct Parser {
    tp: Toolpath,
    keep: Keep,
    max_points: usize,
    pos: [f64; 3],
    e: f64,
    abs_xyz: bool,
    abs_e: bool,
    /// `M82` or `M83` has set the extruder's mode.
    e_explicit: bool,
    abs_arc: bool,
    /// Millimetres per unit: 1, or 25.4 after `G20`.
    scale: f64,
    /// 0 for XY (`G17`), 1 for ZX (`G18`), 2 for YZ (`G19`).
    plane: u8,
    /// The motion mode in force, for CNC programs that give it once.
    motion: Option<u8>,
    feature: Feature,
    width_hint: u16,
    height_hint: f32,
    tool: u8,
    /// Layer comments seen: they decide the layers from now on.
    markers: bool,
    /// The last stored point is the current position.
    connected: bool,
    /// Lines were skipped (see `skip`): the position and extruder are those
    /// of an earlier layer until the next move or position comment says
    /// otherwise.
    fresh: bool,
    full: bool,
    feed_length: f64,
    feed_min: [f32; 3],
    feed_max: [f32; 3],
}

impl Parser {
    /// A parser storing what `keep` says, and at most `max_points` points;
    /// beyond that the rest of the file is still read for its comments,
    /// and a warning says the toolpath was cut short.
    pub fn new(keep: Keep, max_points: usize) -> Parser {
        Parser::with_capacity(keep, max_points, 0)
    }

    /// As `new`, with room for the moves of a file of `bytes` bytes (about
    /// one per 30 bytes), so the lists don't grow by copying.  A thumbnail
    /// of a file too large to keep whole keeps every few layers instead,
    /// spread over the whole print, rather than the first few.
    pub fn with_capacity(keep: Keep, max_points: usize, bytes: u64) -> Parser {
        let estimate = (bytes / 30) as usize;
        let thinned = if keep == Keep::Drawn && estimate > max_points {
            estimate.div_ceil(max_points.max(1)).min(1000) as u32
        } else {
            1
        };
        let guess = estimate.min(max_points).min(50_000_000);
        let tp = Toolpath {
            points: Vec::with_capacity(guess),
            moves: Vec::with_capacity(guess),
            layers: vec![Layer::default()],
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            format: "G-code".into(),
            thinned,
            ..Default::default()
        };
        Parser {
            tp,
            keep,
            max_points,
            pos: [0.0; 3],
            e: 0.0,
            abs_xyz: true,
            abs_e: true,
            e_explicit: false,
            abs_arc: false,
            scale: 1.0,
            plane: 0,
            motion: None,
            feature: Feature::Other,
            width_hint: 0,
            height_hint: 0.0,
            tool: 0,
            markers: false,
            connected: false,
            // Nothing is known of where the tool starts: the first move
            // begins nowhere, rather than at the origin.
            fresh: true,
            full: false,
            feed_length: 0.0,
            feed_min: [f32::INFINITY; 3],
            feed_max: [f32::NEG_INFINITY; 3],
        }
    }

    /// Metadata from outside the G-code text (binary G-code's blocks).
    pub fn meta_line(&mut self, line: &[u8]) {
        parse_meta(&mut self.tp.meta, line, true);
    }

    pub fn meta(&self) -> &Meta {
        &self.tp.meta
    }

    pub fn set_format(&mut self, format: impl Into<String>) {
        self.tp.format = format.into();
    }

    pub fn warn(&mut self, w: impl Into<String>) {
        self.tp.warnings.push(w.into());
    }

    /// Says that lines were left out: what follows starts a layer whose
    /// position and extruder aren't known from what came before.  Sampling
    /// (see `sample`) calls this at each jump in the file.
    pub fn skip(&mut self) {
        self.connected = false;
        self.fresh = true;
        self.tp.sampled = true;
    }

    /*┌─────────────────╮
    | Lines           |
    └─────────────────╯*/
    //region Lines

    /// Interprets one line.
    pub fn line(&mut self, line: &[u8]) {
        self.tp.lines += 1;
        let s = line.trim_ascii_start();
        let Some(&first) = s.first() else { return };
        if first == b';' {
            self.comment(&s[1..]);
            return;
        }
        if !first.is_ascii_alphabetic() {
            return;
        }
        let words = match Words::parse(s) {
            Some(w) => w,
            None => {
                // Klipper macros and other commands that aren't G-code
                // words, though some carry a slicer's comment.
                if let Some(i) = memchr::memchr(b';', s) {
                    self.comment(&s[i + 1..]);
                }
                return;
            }
        };
        self.execute(&words);
        if let Some(i) = words.comment {
            // `G1 X1 ; TYPE:...` is rare, but metadata after a command
            // isn't (`M73 P0 R12 ; remaining time`): only layer and type
            // comments matter here.
            let c = trim(&s[i..]);
            if c.starts_with(b"TYPE:") || c.starts_with(b"LAYER") {
                self.comment(c);
            }
        }
    }

    fn comment(&mut self, c: &[u8]) {
        let c = trim(c);
        let Some(&first) = c.first() else { return };
        if is_layer_comment(c) {
            self.layer_marker();
            return;
        }
        let value = |prefix: &[u8]| -> Option<&[u8]> { c.strip_prefix(prefix).map(trim) };
        let number = |v: &[u8]| parse_number(v).map(|(n, _)| n as f32).filter(|n| n.is_finite());
        let hot = match first {
            b'T' => {
                if let Some(v) = value(b"TYPE:") {
                    self.feature = Feature::from_name(&String::from_utf8_lossy(v));
                    true
                } else if let Some(v) = value(b"TIME:") {
                    // Cura's estimate in seconds (`;TIME_ELAPSED:` is per
                    // layer and doesn't match).
                    if let Some(t) = parse_number(v).map(|x| x.0) {
                        self.tp.meta.print_time = Some(t);
                    }
                    true
                } else {
                    false
                }
            }
            b'W' => match value(b"WIDTH:") {
                Some(v) => {
                    self.width_hint = number(v).map_or(0, |w| (w * 1000.0).clamp(0.0, 65535.0) as u16);
                    true
                }
                None => false,
            },
            b'H' => match value(b"HEIGHT:") {
                Some(v) => {
                    self.set_height_hint(number(v).unwrap_or(0.0));
                    true
                }
                None => false,
            },
            b'L' => {
                if let Some(v) = value(b"LINE_WIDTH:") {
                    self.width_hint = number(v).map_or(0, |w| (w * 1000.0).clamp(0.0, 65535.0) as u16);
                    true
                } else if let Some(v) = value(b"LAYER_HEIGHT:") {
                    self.set_height_hint(number(v).unwrap_or(0.0));
                    true
                } else {
                    false
                }
            }
            b'F' => match value(b"FEATURE:") {
                Some(v) => {
                    self.feature = Feature::from_name(&String::from_utf8_lossy(v));
                    true
                }
                None => false,
            },
            b'Z' => match value(b"Z:") {
                // PrusaSlicer and OrcaSlicer say each layer's height here.
                // It matters after a skip, when the height is unknown.
                Some(v) => {
                    if let Some(z) = parse_number(v).map(|x| x.0).filter(|z| z.is_finite() && z.abs() < 1e6)
                        && self.fresh
                    {
                        self.pos[2] = z * self.scale;
                    }
                    true
                }
                None => false,
            },
            // Simplify3D: `; feature inner perimeter`.
            b'f' if c.starts_with(b"feature ") => {
                self.feature = Feature::from_name(&String::from_utf8_lossy(&c[8..]));
                true
            }
            _ => false,
        };
        if !hot {
            parse_meta(&mut self.tp.meta, c, self.tp.lines <= HEADER_LINES);
        }
    }

    fn set_height_hint(&mut self, h: f32) {
        self.height_hint = if h.is_finite() && h > 0.0 && h < 10.0 { h } else { 0.0 };
        if let Some(l) = self.tp.layers.last_mut()
            && !l.extruded
        {
            l.height_hint = self.height_hint;
        }
    }

    fn layer_marker(&mut self) {
        self.markers = true;
        if self.tp.layers.last().is_some_and(|l| l.extruded) {
            let start = self.tp.points.len() as u32;
            self.tp.layers.push(Layer { start, height_hint: self.height_hint, ..Default::default() });
        }
    }

    //endregion

    /*┌─────────────────╮
    | Commands        |
    └─────────────────╯*/
    //region Commands

    fn execute(&mut self, w: &Words) {
        // 1. Modal and non-motion G codes.
        let mut motion = None;
        let mut other_g = false;
        let mut home = false;
        let mut set_position = false;
        for &g in &w.g[..w.ng] {
            // Rounding by adding a half and truncating: `round()` is a libm
            // call on baseline x86-64, and this runs for every line.
            let code = (g * 10.0 + 0.5) as i64;
            match code {
                0 | 10 | 20 | 30 => motion = Some((code / 10) as u8),
                170 => self.plane = 0,
                180 => self.plane = 1,
                190 => self.plane = 2,
                200 => self.scale = 25.4,
                210 => self.scale = 1.0,
                280 => home = true,
                // The extruder follows these only until `M82` or `M83` has
                // chosen its mode: after that, `G90` doesn't undo `M83`.
                900 => {
                    self.abs_xyz = true;
                    self.abs_e |= !self.e_explicit;
                }
                910 => {
                    self.abs_xyz = false;
                    self.abs_e &= self.e_explicit;
                }
                901 => self.abs_arc = true,
                911 => self.abs_arc = false,
                920 => set_position = true,
                // A canned cycle's end cancels the motion mode.
                800 => self.motion = None,
                _ => {}
            }
            if !matches!(code, 0 | 10 | 20 | 30) {
                other_g = true;
            }
        }
        // 2. M codes.  `T` is a tool change only on a line of its own:
        //    `M104 S200 T1` sets a temperature.
        for &m in &w.m[..w.nm] {
            match (m + 0.5) as i64 {
                82 => {
                    self.abs_e = true;
                    self.e_explicit = true;
                }
                83 => {
                    self.abs_e = false;
                    self.e_explicit = true;
                }
                _ => {}
            }
        }
        if w.ng == 0
            && w.nm == 0
            && let Some(t) = w.get(b'T')
        {
            let t = t.clamp(0.0, 255.0) as u8;
            if t != self.tool {
                self.tp.tool_changes += 1;
                self.tool = t;
            }
        }

        // 3. Position changes that aren't moves.
        let axes = [w.get(b'X'), w.get(b'Y'), w.get(b'Z')];
        if home {
            let named = [b'X', b'Y', b'Z'].map(|l| w.get(l).is_some() || w.bare & (1 << (l - b'A')) != 0);
            let all = !named.iter().any(|&n| n);
            for (i, &n) in named.iter().enumerate() {
                if all || n {
                    self.pos[i] = 0.0;
                }
            }
            self.connected = false;
            return;
        }
        if set_position {
            for (i, a) in axes.iter().enumerate() {
                if let Some(v) = a {
                    self.pos[i] = v * self.scale;
                    self.connected = false;
                }
            }
            if let Some(e) = w.get(b'E') {
                self.e = e * self.scale;
            }
            return;
        }

        // 4. Moves.  CNC programs give the motion mode once and then only
        //    coordinates.
        if motion.is_some() {
            self.motion = motion;
        } else if other_g || w.nm > 0 {
            return;
        }
        let Some(mode) = motion.or(self.motion) else { return };
        let has_arc_words = [b'I', b'J', b'K', b'R'].iter().any(|&l| w.get(l).is_some());
        let e_word = w.get(b'E');
        if axes.iter().all(|a| a.is_none()) && e_word.is_none() && !(mode >= 2 && has_arc_words) {
            return;
        }
        let mut target = self.pos;
        for (i, a) in axes.iter().enumerate() {
            if let Some(v) = a {
                let v = v * self.scale;
                target[i] = if self.abs_xyz { v } else { self.pos[i] + v };
            }
        }
        if !target.iter().all(|v| v.is_finite() && v.abs() < 1e9) {
            return;
        }
        let mut de = 0.0;
        if let Some(e) = e_word {
            let e = e * self.scale;
            let new = if self.abs_e { e } else { self.e + e };
            // After a skip the absolute extruder position of the layer
            // before is no baseline.
            de = if self.fresh && self.abs_e { 0.0 } else { new - self.e };
            self.e = new;
        }
        // A laser's `S0` is a move with the beam off.
        let beam_off = w.get(b'S').is_some_and(|s| s == 0.0);
        let kind = match mode {
            0 => MoveKind::Travel,
            _ if de > 1e-7 => MoveKind::Extrude,
            _ if beam_off => MoveKind::Travel,
            _ => MoveKind::Feed,
        };
        if mode >= 2 && self.arc(w, target, de, kind, mode == 2) {
            return;
        }
        self.push(target, kind, de);
    }

    /// Divides a `G2` (clockwise) or `G3` arc into short moves.  Returns
    /// false when the arc makes no sense, to be drawn as a straight move.
    fn arc(&mut self, w: &Words, target: [f64; 3], de: f64, kind: MoveKind, clockwise: bool) -> bool {
        // The plane's two axes, ordered so that a × b points along the third
        // (the axis the arc is seen down for clockwise), and which offset
        // word goes with each.
        let (a, b, c, oa, ob) = match self.plane {
            1 => (2, 0, 1, b'K', b'I'),
            2 => (1, 2, 0, b'J', b'K'),
            _ => (0, 1, 2, b'I', b'J'),
        };
        let (sa, sb) = (self.pos[a], self.pos[b]);
        let (ta, tb) = (target[a], target[b]);
        let (ca, cb) = if let Some(r) = w.get(b'R') {
            // The centre from the radius: on the perpendicular bisector of
            // the chord, the side and the long or short way round by the
            // direction and the radius's sign.
            let r = r * self.scale;
            let (x, y) = (ta - sa, tb - sb);
            let d2 = x * x + y * y;
            if d2 < 1e-12 || !r.is_finite() {
                return false;
            }
            let mut h = 4.0 * r * r - d2;
            if h < 0.0 {
                // A radius a little short of half the chord is rounding.
                if h < -1e-3 * d2 {
                    return false;
                }
                h = 0.0;
            }
            let mut k = -h.sqrt() / d2.sqrt();
            if !clockwise {
                k = -k;
            }
            if r < 0.0 {
                k = -k;
            }
            (sa + 0.5 * (x - y * k), sb + 0.5 * (y + x * k))
        } else {
            let (i, j) = (w.get(oa).unwrap_or(0.0) * self.scale, w.get(ob).unwrap_or(0.0) * self.scale);
            if self.abs_arc { (i, j) } else { (sa + i, sb + j) }
        };
        let r = ((sa - ca).powi(2) + (sb - cb).powi(2)).sqrt();
        if !r.is_finite() || !(1e-6..=1e6).contains(&r) {
            return false;
        }
        let start = (sb - cb).atan2(sa - ca);
        let end = (tb - cb).atan2(ta - ca);
        let tau = std::f64::consts::TAU;
        let mut sweep = if clockwise { start - end } else { end - start };
        while sweep <= 1e-9 {
            sweep += tau;
        }
        while sweep > tau + 1e-9 {
            sweep -= tau;
        }
        let max_step =
            if r > ARC_TOLERANCE { 2.0 * (1.0 - ARC_TOLERANCE / r).acos() } else { std::f64::consts::FRAC_PI_4 };
        let steps = ((sweep / max_step.max(1e-6)).ceil() as usize).clamp(1, ARC_MAX_STEPS);
        let sign = if clockwise { -1.0 } else { 1.0 };
        let (sc, tc) = (self.pos[c], target[c]);
        for k in 1..=steps {
            let t = k as f64 / steps as f64;
            let mut p = target;
            if k < steps {
                let angle = start + sign * sweep * t;
                p[a] = ca + r * angle.cos();
                p[b] = cb + r * angle.sin();
                p[c] = sc + (tc - sc) * t;
            }
            self.push(p, kind, de / steps as f64);
        }
        true
    }

    /// Records the move from the current position to `to`.
    fn push(&mut self, to: [f64; 3], kind: MoveKind, de: f64) {
        // After a skip nothing is known of where the tool came from.
        let from = if std::mem::take(&mut self.fresh) { to } else { self.pos };
        self.pos = to;
        let len = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
        // Retractions and other moves that go nowhere draw nothing.
        if len < 1e-9 {
            return;
        }
        match kind {
            MoveKind::Travel => self.tp.travel_length += len,
            MoveKind::Feed => {
                self.feed_length += len;
                include(&mut self.feed_min, &mut self.feed_max, from);
                include(&mut self.feed_min, &mut self.feed_max, to);
            }
            MoveKind::Extrude => {
                self.on_extrusion(to[2] as f32);
                self.tp.feature_length[self.feature.index()] += len;
                let (min, max) = (&mut self.tp.min, &mut self.tp.max);
                include(min, max, from);
                include(min, max, to);
            }
            MoveKind::Jump => {}
        }
        if self.keep == Keep::Drawn
            && (kind == MoveKind::Travel || !(self.tp.layers.len() - 1).is_multiple_of(self.tp.thinned as usize))
        {
            self.connected = false;
            return;
        }
        if self.full {
            return;
        }
        if self.tp.points.len() + 2 > self.max_points {
            self.full = true;
            let n = self.tp.points.len();
            self.tp.warnings.push(format!("only the first {n} moves are shown"));
            return;
        }
        if !self.connected {
            self.tp.points.push(to_f32(from));
            self.tp.moves.push(Move { tool: self.tool, ..Move::default() });
            self.connected = true;
        }
        let e_per_mm = if kind == MoveKind::Extrude && len > 1e-9 {
            ((de / len) * 1e5 + 0.5).clamp(0.0, 65535.0) as u16
        } else {
            0
        };
        self.tp.points.push(to_f32(to));
        self.tp.moves.push(Move { kind, feature: self.feature, tool: self.tool, width: self.width_hint, e_per_mm });
    }

    fn on_extrusion(&mut self, z: f32) {
        let start = self.tp.points.len() as u32;
        let hint = self.height_hint;
        let markers = self.markers;
        let Some(layer) = self.tp.layers.last_mut() else { return };
        if !layer.extruded {
            layer.extruded = true;
            layer.z = z;
            if layer.height_hint == 0.0 {
                layer.height_hint = hint;
            }
            return;
        }
        if !markers && (z - layer.z).abs() > LAYER_STEP {
            self.tp.layers.push(Layer { z, start, extruded: true, height_hint: hint, height: 0.0 });
        }
    }

    //endregion

    /*┌─────────────────╮
    | Finishing       |
    └─────────────────╯*/
    //region Finishing

    /// Settles layers, heights and whether this is a CNC program, and hands
    /// back the toolpath.
    pub fn finish(mut self) -> Toolpath {
        let tp = &mut self.tp;
        // 1. A file that never extruded is a mill's or laser's: its feed
        //    moves are cuts.
        let extruded: f64 = tp.feature_length.iter().sum();
        if extruded <= 0.0 && self.feed_length > 0.0 {
            tp.cnc = true;
            tp.feature_length = [0.0; super::Feature::COUNT];
            tp.feature_length[Feature::Cut.index()] = self.feed_length;
            tp.min = self.feed_min;
            tp.max = self.feed_max;
            for m in &mut tp.moves {
                if m.kind == MoveKind::Feed {
                    m.feature = Feature::Cut;
                }
            }
        } else {
            tp.travel_length += self.feed_length;
        }

        // 2. Layers: a layer without extrusions joins the one before (the
        //    start G-code's moves join the first), and so does one at the
        //    same height (a purge line before the first layer comment).
        let mut layers: Vec<Layer> = Vec::with_capacity(tp.layers.len());
        for l in tp.layers.drain(..) {
            match layers.last_mut() {
                None => layers.push(Layer { start: 0, ..l }),
                Some(_) if !l.extruded => {}
                Some(prev) if !prev.extruded => *prev = Layer { start: prev.start, ..l },
                Some(prev) if (l.z - prev.z).abs() < 1e-3 => {}
                Some(_) => layers.push(l),
            }
        }

        // 3. Heights: the slicer's, else the step from the layer below,
        //    else the file's layer height.
        let fallback = tp.meta.layer_height.filter(|h| *h > 0.005 && *h < 5.0).map_or(0.2, |h| h as f32);
        let mut below = 0.0f32;
        for l in &mut layers {
            // The layers of a sample aren't neighbours: their gaps say
            // nothing about how thick one is.
            let step = if tp.sampled { 0.0 } else { l.z - below };
            l.height = if l.height_hint > 0.005 && l.height_hint < 5.0 {
                l.height_hint
            } else if step > 0.005 && step < 2.0 {
                step
            } else {
                fallback
            };
            below = l.z;
        }
        tp.layers = layers;
        if tp.cnc {
            // Depth passes aren't layers: one layer holds the program.
            tp.layers.truncate(1);
            if let Some(l) = tp.layers.first_mut() {
                l.start = 0;
            }
        }
        self.tp
    }

    //endregion
}

/// Whether a comment (the text after the `;`) starts a layer, in the
/// words slicers use: `LAYER_CHANGE` (PrusaSlicer, OrcaSlicer), `LAYER:3`
/// (Cura), `CHANGE_LAYER` (Bambu Studio), `BEGIN_LAYER` and Simplify3D's
/// `layer 3, Z = 0.7`.
pub fn is_layer_comment(c: &[u8]) -> bool {
    c == b"LAYER_CHANGE"
        || c == b"CHANGE_LAYER"
        || c.starts_with(b"LAYER:")
        || c.starts_with(b"BEGIN_LAYER")
        || (c.starts_with(b"layer ") && c.get(6).is_some_and(u8::is_ascii_digit))
}

/// Whether a whole line is such a comment.
pub fn is_layer_marker(line: &[u8]) -> bool {
    let l = trim(line);
    l.first() == Some(&b';') && is_layer_comment(trim(&l[1..]))
}

fn to_f32(p: [f64; 3]) -> [f32; 3] {
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

fn include(min: &mut [f32; 3], max: &mut [f32; 3], p: [f64; 3]) {
    for i in 0..3 {
        min[i] = min[i].min(p[i] as f32);
        max[i] = max[i].max(p[i] as f32);
    }
}

/*┌─────────────────╮
| Words           |
└─────────────────╯*/
//region Words

/// One line's words.  G and M may repeat (`G0 G90 G54`); other letters
/// keep their last value.
struct Words {
    g: [f64; 8],
    ng: usize,
    m: [f64; 4],
    nm: usize,
    values: [f64; 26],
    set: u32,
    /// Letters given without a number (`G28 X` homes X).
    bare: u32,
    /// Where a `;` comment starts, if any.
    comment: Option<usize>,
}

impl Words {
    /// Parses a line's words, or `None` when its first word isn't a letter
    /// and a number (a macro name such as `SET_PRINT_STATS_INFO`).
    fn parse(s: &[u8]) -> Option<Words> {
        let mut w = Words { g: [0.0; 8], ng: 0, m: [0.0; 4], nm: 0, values: [0.0; 26], set: 0, bare: 0, comment: None };
        let mut i = 0;
        let mut first = true;
        while i < s.len() {
            let c = s[i];
            match c {
                b';' => {
                    w.comment = Some(i + 1);
                    break;
                }
                // A checksum ends the command.
                b'*' => break,
                b'(' => match memchr::memchr(b')', &s[i..]) {
                    Some(j) => i += j + 1,
                    None => break,
                },
                b'a'..=b'z' | b'A'..=b'Z' => {
                    let letter = c.to_ascii_uppercase();
                    i += 1;
                    while i < s.len() && s[i] == b' ' {
                        i += 1;
                    }
                    match parse_number(&s[i..]) {
                        Some((v, n)) => {
                            i += n;
                            w.add(letter, v);
                            first = false;
                        }
                        None if first => return None,
                        None => w.bare |= 1 << (letter - b'A'),
                    }
                }
                _ => i += 1,
            }
        }
        if first { None } else { Some(w) }
    }

    fn add(&mut self, letter: u8, v: f64) {
        match letter {
            b'G' if self.ng < self.g.len() => {
                self.g[self.ng] = v;
                self.ng += 1;
            }
            b'M' if self.nm < self.m.len() => {
                self.m[self.nm] = v;
                self.nm += 1;
            }
            b'G' | b'M' => {}
            _ => {
                let k = (letter - b'A') as usize;
                self.values[k] = v;
                self.set |= 1 << k;
            }
        }
    }

    fn get(&self, letter: u8) -> Option<f64> {
        let k = (letter - b'A') as usize;
        (self.set & (1 << k) != 0).then(|| self.values[k])
    }
}

//endregion

/*┌─────────────────╮
| Metadata        |
└─────────────────╯*/
//region Metadata

/// Reads what a comment (or a binary G-code metadata line) says about the
/// print: the slicer, time, filament, material, printer and settings.
/// `header` is true for the first lines, where slicers name themselves.
pub fn parse_meta(meta: &mut Meta, c: &[u8], header: bool) {
    let c = trim(c);
    if c.is_empty() {
        return;
    }
    if header
        && meta.generator.is_none()
        && let Some(g) = generator(c)
    {
        meta.generator = Some(g);
        return;
    }
    let Some(sep) = c.iter().position(|&b| b == b'=' || b == b':') else { return };
    if sep > 80 {
        return;
    }
    let key = String::from_utf8_lossy(trim(&c[..sep])).to_ascii_lowercase();
    let value = String::from_utf8_lossy(trim(&c[sep + 1..])).into_owned();
    let first_number = |v: &str| parse_number(v.trim().as_bytes()).map(|x| x.0).filter(|x| x.is_finite());
    // Comma-separated per extruder: their sum.
    let sum = |v: &str| {
        let parts: Vec<f64> = v.split(',').filter_map(|p| parse_number(p.trim().as_bytes()).map(|x| x.0)).collect();
        (!parts.is_empty()).then(|| parts.iter().sum::<f64>()).filter(|x| x.is_finite())
    };
    let text = |v: &str| {
        let v = v.trim().trim_matches('"').trim();
        // Per-extruder lists: the distinct names.
        let mut names: Vec<&str> = Vec::new();
        for n in v.split(';').map(str::trim).filter(|n| !n.is_empty()) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        (!names.is_empty()).then(|| names.join(", "))
    };
    match key.as_str() {
        "estimated printing time (normal mode)"
        | "estimated printing time"
        | "total estimated time"
        | "print.time"
        | "print time"
        | "estimated_time(s)"
        | "build time" => {
            if let Some(t) = parse_duration(&value) {
                meta.print_time = Some(t);
            }
        }
        "model printing time" => {
            // Bambu Studio: `model printing time: 50m 19s; total estimated
            // time: 56m 41s`.
            let total = value.split(';').find_map(|p| p.trim().strip_prefix("total estimated time:"));
            if let Some(t) = parse_duration(total.unwrap_or(&value)) {
                meta.print_time = Some(t);
            }
        }
        "filament used [mm]" | "total filament length [mm]" | "material#1 used" => {
            meta.filament_mm = sum(&value).or(meta.filament_mm);
        }
        "filament length" => meta.filament_mm = first_number(&value).or(meta.filament_mm),
        "filament used" => {
            // Cura: `1.2345m` (per extruder: `1.2m, 0m`).
            if value.contains('m') {
                meta.filament_mm = sum(&value).map(|m| m * 1000.0).or(meta.filament_mm);
            }
        }
        "filament used [g]" | "total filament used [g]" | "total filament weight [g]" | "plastic weight" => {
            meta.filament_g = sum(&value).or(meta.filament_g);
        }
        "filament used [cm3]" | "total filament volume [cm^3]" => meta.filament_cm3 = sum(&value),
        "extruder_train.0.material.volume_used" => meta.filament_cm3 = first_number(&value).map(|v| v / 1000.0),
        "filament_type" => meta.material = text(&value).or(meta.material.take()),
        "filament_settings_id" if meta.material.is_none() => meta.material = text(&value),
        "printer_model" | "target_machine.name" | "machine_name" => meta.printer = text(&value),
        "printer_settings_id" if meta.printer.is_none() => meta.printer = text(&value),
        "layer_height" | "layer height" | "layerheight" => meta.layer_height = first_number(&value),
        "nozzle_diameter" | "extruder_train.0.nozzle.diameter" | "nozzle diameter" | "extruderdiameter" => {
            meta.nozzle = first_number(&value)
        }
        "filament_diameter" | "filament diameter" | "filamentdiameters" => {
            meta.filament_diameter = first_number(&value)
        }
        "flavor" | "gcode_flavor" => meta.flavor = text(&value),
        // Binary G-code's file metadata.
        "producer" if meta.generator.is_none() => meta.generator = text(&value),
        _ => {}
    }
}

/// The slicer named in a header comment: "generated by PrusaSlicer 2.9.2
/// on ...", "Generated with Cura_SteamEngine 5.4.0", "Sliced by ideaMaker
/// 4.4", "BambuStudio 01.09.00.70".
fn generator(c: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(c);
    let lower = s.to_ascii_lowercase();
    let mut name = None;
    for marker in ["generated by ", "generated with ", "sliced by ", "created by "] {
        if let Some(i) = lower.find(marker) {
            name = Some(s[i + marker.len()..].to_string());
            break;
        }
    }
    if name.is_none() && (starts_with_ci(c, b"BambuStudio") || starts_with_ci(c, b"KISSlicer")) {
        name = Some(s.to_string());
    }
    let mut name = name?;
    // "... on 2025-05-24 at 15:21:40 UTC"
    if let Some(i) = name.find(" on ").filter(|&i| name[i + 4..].starts_with(|c: char| c.is_ascii_digit())) {
        name.truncate(i);
    }
    let name = name.replace("Cura_SteamEngine", "Cura").replace("(R)", "").replace(" Version ", " ");
    let name = name.trim().trim_end_matches(['.', ',']).trim();
    if name.is_empty() || name.len() > 80 || contains_ci(name.as_bytes(), b"thumbnail") {
        return None;
    }
    Some(name.to_string())
}

//endregion
