//! GPX 1.0 and 1.1: tracks (each segment a line, since a new segment
//! means the receiver lost its fix), routes and waypoints, with
//! elevation, time, GPX 1.0's speed, and the sensor values Garmin's
//! TrackPointExtension (`gpxtpx:hr`, `cad`), Strava, Wahoo and others put
//! in `<extensions>` (`hr`, `heartrate`, `cad`, `cadence`, `power`,
//! `PowerInWatts`, `speed`).

use super::xml::{self, Handler, attr, leaf, number, parent, within};
use super::{Line, LineKind, TrackPoint, TrackSet, Waypoint, parse_iso8601};
use quick_xml::events::BytesStart;
use std::io::BufRead;

pub fn parse_gpx(reader: impl BufRead) -> TrackSet {
    let mut g = Gpx { set: TrackSet { format: "GPX".into(), ..Default::default() }, ..Default::default() };
    if let Some(w) = xml::walk(reader, &mut g) {
        g.set.warnings.push(w);
    }
    // What was read of a file cut short still counts.
    g.flush_line();
    g.set
}

#[derive(Default)]
struct Gpx {
    set: TrackSet,
    /// The track or route being read, and its name.
    name: String,
    line: Option<Line>,
    point: Option<TrackPoint>,
    waypoint: Option<Waypoint>,
    /// Points read so far, kept or not.
    read: usize,
}

impl Handler for Gpx {
    fn start(&mut self, path: &[u8], e: &BytesStart) {
        match leaf(path) {
            b"gpx" => {
                let version = attr(e, b"version").unwrap_or_default();
                self.set.format = if version.is_empty() { "GPX".into() } else { format!("GPX {version}") };
                self.set.creator = attr(e, b"creator").filter(|c| !c.trim().is_empty());
            }
            b"trk" | b"rte" => self.name.clear(),
            b"trkseg" => {
                self.flush_line();
                self.line = Some(Line { name: self.name.clone(), kind: LineKind::Track, points: Vec::new() });
            }
            b"trkpt" | b"rtept" => {
                if leaf(path) == b"rtept" && self.line.is_none() {
                    self.line = Some(Line { name: self.name.clone(), kind: LineKind::Route, points: Vec::new() });
                }
                let lat = attr(e, b"lat").and_then(|v| number(&v)).unwrap_or(f64::NAN);
                let lon = attr(e, b"lon").and_then(|v| number(&v)).unwrap_or(f64::NAN);
                self.point = Some(TrackPoint::new(lat, lon));
            }
            b"wpt" => {
                let lat = attr(e, b"lat").and_then(|v| number(&v)).unwrap_or(f64::NAN);
                let lon = attr(e, b"lon").and_then(|v| number(&v)).unwrap_or(f64::NAN);
                self.waypoint = Some(Waypoint { name: String::new(), lat, lon, ele: f32::NAN });
            }
            _ => {}
        }
    }

    fn end(&mut self, path: &[u8], text: &str) {
        let name = leaf(path);
        let text = text.trim();
        if let Some(p) = &mut self.point {
            let value = || number(text).map(|v| v as f32);
            match name {
                b"trkpt" | b"rtept" => {
                    let p = self.point.take().expect("checked");
                    self.read += 1;
                    if self.read > xml::MAX_POINTS {
                        if self.read == xml::MAX_POINTS + 1 {
                            self.set.warnings.push(format!("only the first {} points are read", xml::MAX_POINTS));
                        }
                    } else if let Some(l) = &mut self.line {
                        l.points.push(p);
                    }
                }
                b"ele" => p.ele = value().unwrap_or(f32::NAN),
                b"time" if !within(path, b"extensions") => p.time = parse_iso8601(text).unwrap_or(f64::NAN),
                b"speed" => p.speed = value().unwrap_or(p.speed),
                b"hr" | b"heartrate" | b"HeartRate" | b"HeartRateBpm" => p.hr = value().unwrap_or(p.hr),
                b"cad" | b"cadence" | b"Cadence" | b"RunCadence" => p.cad = value().unwrap_or(p.cad),
                b"power" | b"Power" | b"PowerInWatts" | b"watts" | b"Watts" => p.power = value().unwrap_or(p.power),
                b"Value" if parent(path) == b"HeartRateBpm" => p.hr = value().unwrap_or(p.hr),
                _ => {}
            }
            return;
        }
        if let Some(w) = &mut self.waypoint {
            match name {
                b"wpt" => {
                    let w = self.waypoint.take().expect("checked");
                    self.set.waypoints.push(w);
                }
                b"name" => w.name = text.to_string(),
                b"ele" => w.ele = number(text).map_or(f32::NAN, |v| v as f32),
                _ => {}
            }
            return;
        }
        match name {
            b"name" => match parent(path) {
                b"trk" | b"rte" => {
                    self.name = text.to_string();
                    if let Some(l) = &mut self.line {
                        l.name = self.name.clone();
                    }
                }
                b"metadata" | b"gpx" => self.set.name = Some(text.to_string()).filter(|n| !n.is_empty()),
                _ => {}
            },
            b"trkseg" | b"rte" | b"trk" => self.flush_line(),
            b"type" if parent(path) == b"trk" && self.set.sport.is_none() => {
                self.set.sport = Some(text.to_string()).filter(|s| !s.is_empty());
            }
            _ => {}
        }
    }
}

impl Gpx {
    fn flush_line(&mut self) {
        if let Some(l) = self.line.take()
            && !l.points.is_empty()
        {
            self.set.lines.push(l);
        }
    }
}
