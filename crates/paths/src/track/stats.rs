//! Measuring a track: distance, climb, times, speeds, elevation and the
//! averages of what the sensors recorded; and the profile the viewer's
//! elevation chart and colours read, point by point.

use super::{LineKind, Projection, TrackSet, distance, format_distance, format_speed};
use crate::text::format_duration;

/// Elevation changes smaller than this (in metres) are noise: climb counts
/// only once the elevation has moved this far from the last turning
/// point, as GPS and barometric altimeters wander by a metre or two.
pub const CLIMB_THRESHOLD: f64 = 3.0;
/// Slower than this (metres per second, 1.8 km/h) counts as stopped.
const MOVING_SPEED: f64 = 0.5;
/// Speeds are measured over at least this many seconds, which keeps
/// single bad fixes from making a top speed.
const SPEED_WINDOW: f64 = 5.0;

/// Every journey line's points in order, with what the viewer needs per
/// point.  Outlines are left out.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// Distance from the start in metres, continuing from line to line.
    pub dist: Vec<f64>,
    /// Metres east and north of the projection's centre.
    pub pos: Vec<[f32; 2]>,
    /// Metres; NaN where unknown.
    pub ele: Vec<f32>,
    /// Seconds since 1970; NaN where unknown.
    pub time: Vec<f64>,
    /// Metres per second: measured over a few seconds where times are
    /// known, else the device's.
    pub speed: Vec<f32>,
    pub hr: Vec<f32>,
    pub cad: Vec<f32>,
    pub power: Vec<f32>,
    /// The index where each line starts.
    pub starts: Vec<usize>,
}

impl Profile {
    pub fn of(set: &TrackSet, proj: &Projection) -> Profile {
        let mut p = Profile::default();
        let mut total = 0.0;
        for line in set.journeys() {
            p.starts.push(p.dist.len());
            let start = p.dist.len();
            for (i, pt) in line.points.iter().enumerate() {
                if i > 0 {
                    total += distance(&line.points[i - 1], pt);
                }
                let (e, n) = proj.project(pt.lat, pt.lon);
                p.dist.push(total);
                p.pos.push([e as f32, n as f32]);
                p.ele.push(pt.ele);
                p.time.push(pt.time);
                p.speed.push(pt.speed);
                p.hr.push(pt.hr);
                p.cad.push(pt.cad);
                p.power.push(pt.power);
            }
            // Speeds measured over a window, where the line has times.
            let end = p.dist.len();
            let (mut lo, mut hi) = (start, start);
            for i in start..end {
                if !p.time[i].is_finite() {
                    continue;
                }
                while lo < i && (!p.time[lo].is_finite() || p.time[i] - p.time[lo] > SPEED_WINDOW * 0.5) {
                    lo += 1;
                }
                hi = hi.max(i);
                while hi + 1 < end && p.time[hi + 1].is_finite() && p.time[hi + 1] - p.time[i] <= SPEED_WINDOW * 0.5 {
                    hi += 1;
                }
                let (a, b) = (lo.min(i), hi.max(i));
                let dt = p.time[b] - p.time[a];
                if dt > 0.0 {
                    p.speed[i] = ((p.dist[b] - p.dist[a]) / dt) as f32;
                }
            }
        }
        p
    }

    pub fn len(&self) -> usize {
        self.dist.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dist.is_empty()
    }

    pub fn has(values: &[f32]) -> bool {
        values.iter().any(|v| v.is_finite())
    }

    /// Whether the elevation varies at all (flat files store zeros).
    pub fn has_elevation(&self) -> bool {
        let (lo, hi) = range(&self.ele);
        hi > lo
    }

    /// The line point `i` belongs to.
    pub fn line_of(&self, i: usize) -> usize {
        self.starts.partition_point(|&s| s <= i).saturating_sub(1)
    }
}

/// The smallest and largest finite value (NaN, NaN when there are none).
pub fn range(values: &[f32]) -> (f32, f32) {
    let mut r = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in values.iter().filter(|v| v.is_finite()) {
        r = (r.0.min(v), r.1.max(v));
    }
    if r.0 > r.1 { (f32::NAN, f32::NAN) } else { r }
}

/// What the points add up to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub distance: f64,
    pub ascent: Option<f64>,
    pub descent: Option<f64>,
    pub moving_time: Option<f64>,
    pub elapsed: Option<f64>,
    pub start: Option<f64>,
    pub avg_speed: Option<f64>,
    pub max_speed: Option<f64>,
    pub min_ele: Option<f64>,
    pub max_ele: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
    pub avg_cad: Option<f64>,
    pub avg_power: Option<f64>,
    pub max_power: Option<f64>,
    pub points: usize,
}

impl Stats {
    pub fn of(set: &TrackSet, profile: &Profile) -> Stats {
        let mut s = Stats { points: profile.len(), ..Default::default() };
        let p = profile;
        if p.is_empty() {
            return s;
        }
        s.distance = p.dist.last().copied().unwrap_or(0.0);

        // 1. Climb and descent, per line, with hysteresis.
        let ends: Vec<usize> = p.starts.iter().skip(1).copied().chain([p.len()]).collect();
        if p.has_elevation() {
            let (mut up, mut down) = (0.0, 0.0);
            for (&a, &b) in p.starts.iter().zip(&ends) {
                let (u, d) = climb(&p.ele[a..b], CLIMB_THRESHOLD);
                up += u;
                down += d;
            }
            s.ascent = Some(up);
            s.descent = Some(down);
            let (lo, hi) = range(&p.ele);
            s.min_ele = Some(lo as f64);
            s.max_ele = Some(hi as f64);
        }

        // 2. Times: elapsed from the first fix to the last, moving where
        //    faster than walking pace.
        let times: Vec<f64> = p.time.iter().copied().filter(|t| t.is_finite()).collect();
        if times.len() >= 2 {
            let (first, last) = (
                times.iter().copied().fold(f64::INFINITY, f64::min),
                times.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            );
            s.start = Some(first);
            s.elapsed = Some(last - first);
            let mut moving = 0.0;
            let mut moved = 0.0;
            for (&a, &b) in p.starts.iter().zip(&ends) {
                for i in a + 1..b {
                    let dt = p.time[i] - p.time[i - 1];
                    let dd = p.dist[i] - p.dist[i - 1];
                    if dt.is_finite() && dt > 0.0 && dd / dt >= MOVING_SPEED {
                        moving += dt;
                        moved += dd;
                    }
                }
            }
            if moving > 0.0 {
                // Chained recordings can repeat times; moving never exceeds
                // the whole.
                let moving = moving.min(last - first).max(0.0);
                s.moving_time = Some(moving);
                s.avg_speed = Some(moved / moving);
            }
        }
        let fastest = top_speed(&p.speed);
        if fastest.is_finite() {
            s.max_speed = Some(fastest as f64);
        }

        // 3. Sensors.
        let mean = |v: &[f32], zeros: bool| {
            let (sum, n) = v
                .iter()
                .filter(|x| x.is_finite() && (zeros || **x > 0.0))
                .fold((0.0f64, 0usize), |(s, n), x| (s + *x as f64, n + 1));
            (n > 0).then(|| sum / n as f64)
        };
        let max = |v: &[f32]| {
            let m = v.iter().copied().filter(|x| x.is_finite()).fold(f32::NAN, f32::max);
            m.is_finite().then_some(m as f64)
        };
        s.avg_hr = mean(&p.hr, false);
        s.max_hr = max(&p.hr);
        // Cadence averages leave out coasting, as bike computers do; power
        // counts it.
        s.avg_cad = mean(&p.cad, false);
        s.avg_power = mean(&p.power, true);
        s.max_power = max(&p.power);
        let _ = set;
        s
    }

    /// The inspector's rows.
    pub fn details(&self, set: &TrackSet) -> Vec<(String, String)> {
        let mut d: Vec<(String, String)> = Vec::new();
        let mut row = |k: &str, v: String| d.push((k.to_string(), v));
        if let Some(name) = &set.name {
            row("Name", name.clone());
        }
        if let Some(sport) = &set.sport {
            row("Sport", sport.clone());
        }
        if let Some(t) = self.start {
            row("Started", format_utc(t));
        }
        row("Distance", format_distance(self.distance));
        if let (Some(up), Some(down)) = (self.ascent, self.descent) {
            row("Climb", format!("{up:.0} m"));
            row("Descent", format!("{down:.0} m"));
        }
        if let Some(t) = self.moving_time {
            row("Moving time", format_duration(t));
        }
        if let Some(t) = self.elapsed {
            row("Elapsed time", format_duration(t));
        }
        if let Some(v) = self.avg_speed {
            row("Average speed", format_speed(v));
        }
        if let Some(v) = self.max_speed {
            row("Top speed", format_speed(v));
        }
        if let (Some(lo), Some(hi)) = (self.min_ele, self.max_ele) {
            row("Highest point", format!("{hi:.0} m"));
            row("Lowest point", format!("{lo:.0} m"));
        }
        if let Some(v) = self.avg_hr {
            row("Average heart rate", format!("{v:.0} bpm (max {:.0})", self.max_hr.unwrap_or(v)));
        }
        if let Some(v) = self.avg_cad {
            row("Average cadence", format!("{v:.0} rpm"));
        }
        if let Some(v) = self.avg_power {
            row("Average power", format!("{v:.0} W (max {:.0})", self.max_power.unwrap_or(v)));
        }
        let dev = &set.device;
        if let Some(m) = dev.distance {
            row("Device distance", format_distance(m));
        }
        if let Some(a) = dev.ascent {
            row("Device climb", format!("{a:.0} m"));
        }
        if let Some(t) = dev.timer {
            row("Device timer", format_duration(t));
        }
        if let Some(c) = dev.calories {
            row("Calories", format!("{c:.0} kcal"));
        }
        if dev.laps > 1 {
            row("Laps", dev.laps.to_string());
        }
        let lines = set.lines.iter().filter(|l| l.kind != LineKind::Outline).count();
        if lines > 1 {
            row("Tracks", lines.to_string());
        }
        let outlines = set.lines.len() - lines;
        if outlines > 0 {
            row("Outlines", outlines.to_string());
        }
        if !set.waypoints.is_empty() {
            row("Waypoints", set.waypoints.len().to_string());
        }
        row("Points", self.points.to_string());
        d
    }
}

/// The highest speed held over seven samples in a row: a bad fix spikes
/// the speeds measured across it, never for that long.
pub fn top_speed(speed: &[f32]) -> f32 {
    const HOLD: usize = 3;
    let valid = |v: f32| v.is_finite() && v < 300.0;
    let mut best = f32::NAN;
    if speed.len() <= 2 * HOLD {
        return speed.iter().copied().filter(|v| valid(*v)).fold(f32::NAN, f32::max);
    }
    for i in HOLD..speed.len() - HOLD {
        let held = speed[i - HOLD..=i + HOLD]
            .iter()
            .copied()
            .fold(f32::INFINITY, |m, v| if valid(v) { m.min(v) } else { f32::NEG_INFINITY });
        if held.is_finite() {
            best = if best.is_finite() { best.max(held) } else { held };
        }
    }
    best
}

/// Climb and descent along `ele` with a dead band of `threshold` metres
/// around the last turning point.
pub fn climb(ele: &[f32], threshold: f64) -> (f64, f64) {
    let mut values = ele.iter().filter(|e| e.is_finite()).map(|&e| e as f64);
    let Some(mut reference) = values.next() else { return (0.0, 0.0) };
    let (mut up, mut down) = (0.0, 0.0);
    for e in values {
        if e >= reference + threshold {
            up += e - reference;
            reference = e;
        } else if e <= reference - threshold {
            down += reference - e;
            reference = e;
        }
    }
    (up, down)
}

/// A UTC time as `2024-05-01 10:20 UTC`.
pub fn format_utc(t: f64) -> String {
    let secs = t.floor() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem / 60 % 60)
}
