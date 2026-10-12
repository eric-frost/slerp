//! GPS tracks: activities and routes from GPX, TCX, FIT, KML/KMZ and
//! GeoJSON files, read into one model ([`TrackSet`]), measured
//! ([`Stats`]), projected to local metres ([`Projection`]) and drawn: flat
//! for thumbnails (`draw`), in 3D over their own elevation for the viewer
//! (`scene`).  Nothing here goes over the network: no map tiles.

mod draw;
mod fit;
mod geojson;
mod gpx;
mod kml;
mod scene;
mod stats;
mod tcx;
mod time;
mod xml;
mod zip;

#[cfg(test)]
mod tests;

pub use draw::track_thumbnail;
pub use fit::{is_fit, parse_fit};
pub use geojson::parse_geojson;
pub use gpx::parse_gpx;
pub use kml::{parse_kml, parse_kmz};
pub use scene::{
    ColorBy, TrackLook, base_elevation, default_exaggeration, metric_color, point_colors, robust_range, track_scene,
};
pub use stats::{Profile, Stats, range as value_range};
pub use tcx::parse_tcx;
pub use time::parse_iso8601;

/// The mean Earth radius in metres (IUGG).
pub const EARTH_RADIUS: f64 = 6_371_008.8;

/// One fix.  Values a file doesn't give are NaN.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPoint {
    /// Degrees north.
    pub lat: f64,
    /// Degrees east.
    pub lon: f64,
    /// Metres above sea level.
    pub ele: f32,
    /// Seconds since 1970 (UTC).
    pub time: f64,
    /// Beats per minute.
    pub hr: f32,
    /// Revolutions (or steps) per minute.
    pub cad: f32,
    /// Watts.
    pub power: f32,
    /// Metres per second, as the device measured it.
    pub speed: f32,
}

impl TrackPoint {
    pub fn new(lat: f64, lon: f64) -> TrackPoint {
        TrackPoint {
            lat,
            lon,
            ele: f32::NAN,
            time: f64::NAN,
            hr: f32::NAN,
            cad: f32::NAN,
            power: f32::NAN,
            speed: f32::NAN,
        }
    }

    /// A position on Earth: latitude and longitude in range.
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite() && self.lon.is_finite() && self.lat.abs() <= 90.0 && self.lon.abs() <= 360.0
    }

    /// Forgets readings no sensor gives: an elevation beyond what Earth
    /// has, a heart rate of a thousand.  Damaged files and broken devices
    /// write them, and one would stretch every chart and total.
    pub fn sanitize(&mut self) {
        let keep = |v: &mut f32, lo: f32, hi: f32| {
            if !(lo..=hi).contains(v) {
                *v = f32::NAN;
            }
        };
        keep(&mut self.ele, -1000.0, 10_000.0);
        keep(&mut self.hr, 20.0, 260.0);
        keep(&mut self.cad, 0.0, 300.0);
        keep(&mut self.power, 0.0, 5000.0);
        keep(&mut self.speed, 0.0, 120.0);
        if !(0.0..=1e11).contains(&self.time) {
            self.time = f64::NAN;
        }
    }
}

/// What a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// A recorded track (GPX `trk`, TCX and FIT activities, `gx:Track`).
    Track,
    /// A planned route (GPX `rte`, TCX courses).
    Route,
    /// A plain line (KML and GeoJSON line strings).
    Line,
    /// A polygon's outline.
    Outline,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub name: String,
    pub kind: LineKind,
    pub points: Vec<TrackPoint>,
}

/// A named point: a GPX waypoint, a KML or GeoJSON point, a TCX course
/// point.
#[derive(Clone, Debug)]
pub struct Waypoint {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub ele: f32,
}

/// Totals a device recorded (a FIT session, TCX laps), shown beside the
/// ones measured from the points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceTotals {
    pub distance: Option<f64>,
    pub elapsed: Option<f64>,
    pub timer: Option<f64>,
    pub ascent: Option<f64>,
    pub descent: Option<f64>,
    pub calories: Option<f64>,
    pub laps: u32,
}

/// Everything a track file holds.
#[derive(Clone, Debug, Default)]
pub struct TrackSet {
    pub lines: Vec<Line>,
    pub waypoints: Vec<Waypoint>,
    pub name: Option<String>,
    /// The program or device that wrote the file.
    pub creator: Option<String>,
    pub sport: Option<String>,
    pub device: DeviceTotals,
    pub warnings: Vec<String>,
    /// Such as "GPX 1.1" or "FIT".
    pub format: String,
}

impl TrackSet {
    /// The number of points in every line.
    pub fn point_count(&self) -> usize {
        self.lines.iter().map(|l| l.points.len()).sum()
    }

    /// Drops invalid points and empty lines, and keeps at most
    /// `max_points` points (spread evenly).
    pub fn tidy(&mut self, max_points: usize) {
        for line in &mut self.lines {
            line.points.retain(TrackPoint::is_valid);
            line.points.iter_mut().for_each(TrackPoint::sanitize);
            if line.kind != LineKind::Outline {
                remove_spikes(&mut line.points);
            }
        }
        self.lines.retain(|l| !l.points.is_empty());
        self.waypoints.retain(|w| TrackPoint::new(w.lat, w.lon).is_valid());
        let total = self.point_count();
        if total > max_points && max_points > 0 {
            let step = total.div_ceil(max_points);
            for line in &mut self.lines {
                let last = line.points.len() - 1;
                let kept: Vec<TrackPoint> = line
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % step == 0 || *i == last)
                    .map(|(_, p)| *p)
                    .collect();
                line.points = kept;
            }
            self.warnings.push(format!("{total} points thinned to every {step}th"));
        }
    }

    /// Whether anything can be drawn.
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.points.is_empty()) && self.waypoints.is_empty()
    }

    /// Points of the lines that measure a journey (not outlines).
    pub fn journeys(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| l.kind != LineKind::Outline)
    }

    /// The projection centred on everything in the file.
    pub fn projection(&self) -> Projection {
        let mut lat = (f64::INFINITY, f64::NEG_INFINITY);
        let mut lons = Vec::new();
        let mut add = |la: f64, lo: f64| {
            lat = (lat.0.min(la), lat.1.max(la));
            lons.push(lo);
        };
        for l in &self.lines {
            for p in &l.points {
                add(p.lat, p.lon);
            }
        }
        for w in &self.waypoints {
            add(w.lat, w.lon);
        }
        if lons.is_empty() {
            return Projection::new(0.0, 0.0);
        }
        Projection::new((lat.0 + lat.1) * 0.5, centre_longitude(&mut lons))
    }
}

/// Removes single fixes that jump away and back: further than a
/// kilometre from both neighbours, which lie close to each other.  GPS
/// receivers and damaged files produce them; the real route never leaves
/// and returns within one sample.
pub fn remove_spikes(points: &mut Vec<TrackPoint>) {
    const FAR: f64 = 1000.0;
    if points.len() < 3 {
        return;
    }
    let n = points.len();
    let mut keep = vec![true; n];
    for i in 0..n {
        let before = if i > 0 { Some(&points[i - 1]) } else { None };
        let after = points.get(i + 1);
        // The ends compare with the two points on one side.
        let (a, b) = match (before, after) {
            (Some(a), Some(b)) => (a, b),
            (None, Some(b)) if n > 2 => (b, &points[2]),
            (Some(a), None) if n > 2 => (a, &points[n - 3]),
            _ => continue,
        };
        let p = &points[i];
        let (da, db) = (distance(p, a), distance(p, b));
        if da > FAR && db > FAR && distance(a, b) < 0.5 * da.min(db) {
            keep[i] = false;
        }
    }
    let mut it = keep.into_iter();
    points.retain(|_| it.next().unwrap_or(true));
}

/// The middle of a set of longitudes, across the antimeridian when the
/// points straddle it.
fn centre_longitude(lons: &mut [f64]) -> f64 {
    let norm = |l: f64| (l + 180.0).rem_euclid(360.0) - 180.0;
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for l in lons.iter_mut() {
        *l = norm(*l);
        min = min.min(*l);
        max = max.max(*l);
    }
    if max - min <= 180.0 {
        return (min + max) * 0.5;
    }
    // Shift the western half by a turn and try again.
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for l in lons.iter() {
        let s = if *l < 0.0 { l + 360.0 } else { *l };
        min = min.min(s);
        max = max.max(s);
    }
    norm((min + max) * 0.5)
}

/// Latitude and longitude to metres east and north of a centre: the
/// equirectangular projection, scaled for the centre's latitude, which is
/// accurate to a fraction of a percent over the size of any ride or hike.
/// Computed in f64; only the local metres become f32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    pub lat0: f64,
    pub lon0: f64,
    cos0: f64,
}

impl Projection {
    pub fn new(lat0: f64, lon0: f64) -> Projection {
        Projection { lat0, lon0, cos0: lat0.to_radians().cos().max(1e-6) }
    }

    /// Metres east and north of the centre.
    pub fn project(&self, lat: f64, lon: f64) -> (f64, f64) {
        let mut dlon = lon - self.lon0;
        dlon = (dlon + 180.0).rem_euclid(360.0) - 180.0;
        let east = EARTH_RADIUS * self.cos0 * dlon.to_radians();
        let north = EARTH_RADIUS * (lat - self.lat0).to_radians();
        (east, north)
    }
}

/// The great-circle distance in metres between two fixes (haversine).
pub fn distance(a: &TrackPoint, b: &TrackPoint) -> f64 {
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dla = la2 - la1;
    let dlo = (b.lon - a.lon).to_radians();
    let h = (dla * 0.5).sin().powi(2) + la1.cos() * la2.cos() * (dlo * 0.5).sin().powi(2);
    2.0 * EARTH_RADIUS * h.sqrt().min(1.0).asin()
}

/// Metres per second as km/h, with a precision that reads well.
pub fn format_speed(v: f64) -> String {
    format!("{:.1} km/h", v * 3.6)
}

/// Metres as `850 m` or `12.4 km`.
pub fn format_distance(m: f64) -> String {
    if m < 1000.0 { format!("{m:.0} m") } else { format!("{:.2} km", m / 1000.0) }
}
