//! Garmin's Training Center XML: activities (each lap's track a line),
//! courses (a route with course points as waypoints), with altitude,
//! time, heart rate, cadence, and speed and power from Garmin's
//! `ActivityExtension` (`TPX`).  Lap totals become the device's totals.

use super::xml::{self, Handler, attr, leaf, number, parent};
use super::{Line, LineKind, TrackPoint, TrackSet, Waypoint, parse_iso8601};
use quick_xml::events::BytesStart;
use std::io::BufRead;

pub fn parse_tcx(reader: impl BufRead) -> TrackSet {
    let mut t = Tcx { set: TrackSet { format: "TCX".into(), ..Default::default() }, ..Default::default() };
    if let Some(w) = xml::walk(reader, &mut t) {
        t.set.warnings.push(w);
    }
    // What was read of a file cut short still counts.
    if let Some(l) = t.line.take()
        && !l.points.is_empty()
    {
        t.set.lines.push(l);
    }
    let d = &mut t.set.device;
    d.timer = (t.lap_time > 0.0).then_some(t.lap_time);
    d.distance = (t.lap_distance > 0.0).then_some(t.lap_distance);
    d.calories = (t.lap_calories > 0.0).then_some(t.lap_calories);
    t.set
}

#[derive(Default)]
struct Tcx {
    set: TrackSet,
    line: Option<Line>,
    point: Option<TrackPoint>,
    waypoint: Option<Waypoint>,
    /// The course's name.
    course: String,
    lap_time: f64,
    lap_distance: f64,
    lap_calories: f64,
    /// Points read so far, kept or not.
    read: usize,
}

impl Handler for Tcx {
    fn start(&mut self, path: &[u8], e: &BytesStart) {
        match leaf(path) {
            b"Activity" => {
                if let Some(s) = attr(e, b"Sport").filter(|s| !s.is_empty()) {
                    self.set.sport = Some(s);
                }
            }
            b"Lap" => self.set.device.laps += 1,
            b"Track" => {
                let course = parent(path) == b"Course";
                let kind = if course { LineKind::Route } else { LineKind::Track };
                self.line = Some(Line { name: self.course.clone(), kind, points: Vec::new() });
            }
            b"Trackpoint" => self.point = Some(TrackPoint::new(f64::NAN, f64::NAN)),
            b"CoursePoint" => {
                self.waypoint = Some(Waypoint { name: String::new(), lat: f64::NAN, lon: f64::NAN, ele: f32::NAN })
            }
            _ => {}
        }
    }

    fn end(&mut self, path: &[u8], text: &str) {
        let name = leaf(path);
        let text = text.trim();
        let value = || number(text);
        if let Some(p) = &mut self.point {
            let v = || value().map(|v| v as f32);
            match name {
                b"Trackpoint" => {
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
                b"Time" => p.time = parse_iso8601(text).unwrap_or(f64::NAN),
                b"LatitudeDegrees" => p.lat = value().unwrap_or(f64::NAN),
                b"LongitudeDegrees" => p.lon = value().unwrap_or(f64::NAN),
                b"AltitudeMeters" => p.ele = v().unwrap_or(f32::NAN),
                b"Value" if parent(path) == b"HeartRateBpm" => p.hr = v().unwrap_or(f32::NAN),
                b"Cadence" | b"RunCadence" => p.cad = v().unwrap_or(p.cad),
                b"Speed" => p.speed = v().unwrap_or(p.speed),
                b"Watts" => p.power = v().unwrap_or(p.power),
                _ => {}
            }
            return;
        }
        if let Some(w) = &mut self.waypoint {
            match name {
                b"CoursePoint" => {
                    let w = self.waypoint.take().expect("checked");
                    self.set.waypoints.push(w);
                }
                b"Name" => w.name = text.to_string(),
                b"LatitudeDegrees" => w.lat = value().unwrap_or(f64::NAN),
                b"LongitudeDegrees" => w.lon = value().unwrap_or(f64::NAN),
                b"AltitudeMeters" => w.ele = value().map_or(f32::NAN, |v| v as f32),
                _ => {}
            }
            return;
        }
        match (parent(path), name) {
            (_, b"Track") => {
                if let Some(l) = self.line.take()
                    && !l.points.is_empty()
                {
                    self.set.lines.push(l);
                }
            }
            (b"Lap", b"TotalTimeSeconds") => self.lap_time += value().unwrap_or(0.0),
            (b"Lap", b"DistanceMeters") => self.lap_distance += value().unwrap_or(0.0),
            (b"Lap", b"Calories") => self.lap_calories += value().unwrap_or(0.0),
            (b"Course", b"Name") => {
                self.course = text.to_string();
                if self.set.name.is_none() && !text.is_empty() {
                    self.set.name = Some(text.to_string());
                }
            }
            (b"Creator", b"Name") => self.set.creator = Some(text.to_string()).filter(|c| !c.is_empty()),
            (b"Author", b"Name") if self.set.creator.is_none() => {
                self.set.creator = Some(text.to_string()).filter(|c| !c.is_empty())
            }
            _ => {}
        }
    }
}
