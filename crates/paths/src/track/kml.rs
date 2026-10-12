//! KML and KMZ (KML zipped, with its pictures): placemarks' line strings,
//! polygons (drawn as outlines), points (waypoints), Google's `gx:Track`
//! (times and positions in parallel lists, with heart rate, cadence and
//! power from its `gx:SimpleArrayData`), `gx:MultiTrack` and
//! `MultiGeometry`.

use super::xml::{self, Handler, attr, leaf, parent, within};
use super::{Line, LineKind, TrackPoint, TrackSet, Waypoint, parse_iso8601, zip};
use quick_xml::events::BytesStart;
use slerp_scene::LoadError;
use std::io::BufRead;

/// The largest KML taken out of a KMZ.
const MAX_KML: usize = 512 << 20;

pub fn parse_kml(reader: impl BufRead) -> TrackSet {
    let mut k = Kml { set: TrackSet { format: "KML".into(), ..Default::default() }, ..Default::default() };
    if let Some(w) = xml::walk(reader, &mut k) {
        k.set.warnings.push(w);
    }
    k.finish_placemark();
    k.set
}

/// Reads the KML document in a KMZ archive: `doc.kml`, or else the first
/// `.kml` entry.
pub fn parse_kmz(data: &[u8]) -> Result<TrackSet, LoadError> {
    let entries = zip::entries(data);
    let entry = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("doc.kml"))
        .or_else(|| entries.iter().find(|e| e.name.to_ascii_lowercase().ends_with(".kml")))
        .ok_or_else(|| LoadError::invalid("no KML document in the KMZ archive"))?;
    let kml = zip::read(data, entry, MAX_KML)?;
    let mut set = parse_kml(kml.as_slice());
    set.format = "KMZ".into();
    Ok(set)
}

#[derive(Default)]
struct Kml {
    set: TrackSet,
    /// The placemark being read: its name and what it holds.
    in_placemark: bool,
    name: String,
    lines: Vec<Line>,
    points: Vec<(f64, f64, f32)>,
    /// A `gx:Track`'s times and positions so far.
    whens: Vec<f64>,
    coords: Vec<TrackPoint>,
    /// The `gx:SimpleArrayData` being read and its values.
    array: Option<String>,
    values: Vec<f32>,
    arrays: Vec<(String, Vec<f32>)>,
}

impl Handler for Kml {
    fn start(&mut self, path: &[u8], e: &BytesStart) {
        match leaf(path) {
            b"Placemark" => {
                self.finish_placemark();
                self.in_placemark = true;
            }
            b"Track" => {
                self.whens.clear();
                self.coords.clear();
                self.arrays.clear();
            }
            b"SimpleArrayData" => {
                self.array = attr(e, b"name").map(|n| n.to_ascii_lowercase());
                self.values.clear();
            }
            _ => {}
        }
    }

    fn end(&mut self, path: &[u8], text: &str) {
        match leaf(path) {
            b"name" => match parent(path) {
                b"Placemark" => self.name = text.trim().to_string(),
                b"Document" | b"Folder" if self.set.name.is_none() && !text.trim().is_empty() => {
                    self.set.name = Some(text.trim().to_string())
                }
                _ => {}
            },
            b"coordinates" => {
                let tuples = parse_coordinates(text);
                let in_polygon = within(path, b"Polygon") || parent(path) == b"LinearRing";
                match parent(path) {
                    b"Point" => self.points.extend(tuples),
                    b"LineString" | b"LinearRing" => {
                        let kind = if in_polygon { LineKind::Outline } else { LineKind::Line };
                        let points = tuples
                            .into_iter()
                            .map(|(lat, lon, ele)| TrackPoint { ele, ..TrackPoint::new(lat, lon) })
                            .collect();
                        self.lines.push(Line { name: String::new(), kind, points });
                    }
                    _ => {}
                }
            }
            b"when" if parent(path) == b"Track" => self.whens.push(parse_iso8601(text).unwrap_or(f64::NAN)),
            b"coord" if parent(path) == b"Track" => {
                let mut it = text.split_ascii_whitespace().map(|v| v.parse::<f64>().unwrap_or(f64::NAN));
                let (lon, lat) = (it.next().unwrap_or(f64::NAN), it.next().unwrap_or(f64::NAN));
                let ele = it.next().map_or(f32::NAN, |v| v as f32);
                if self.coords.len() < super::xml::MAX_POINTS {
                    self.coords.push(TrackPoint { ele, ..TrackPoint::new(lat, lon) });
                }
            }
            b"value" if self.array.is_some() => self.values.push(text.trim().parse().unwrap_or(f32::NAN)),
            b"SimpleArrayData" => {
                if let Some(name) = self.array.take() {
                    self.arrays.push((name, std::mem::take(&mut self.values)));
                }
            }
            b"Track" => {
                let mut points = std::mem::take(&mut self.coords);
                for (i, p) in points.iter_mut().enumerate() {
                    p.time = self.whens.get(i).copied().unwrap_or(f64::NAN);
                    for (name, values) in &self.arrays {
                        let v = values.get(i).copied().unwrap_or(f32::NAN);
                        match name.as_str() {
                            "heartrate" | "heart_rate" | "hr" => p.hr = v,
                            "cadence" | "cad" => p.cad = v,
                            "power" | "watts" => p.power = v,
                            "speed" => p.speed = v,
                            _ => {}
                        }
                    }
                }
                self.lines.push(Line { name: String::new(), kind: LineKind::Track, points });
            }
            b"Placemark" => self.finish_placemark(),
            _ => {}
        }
    }
}

impl Kml {
    fn finish_placemark(&mut self) {
        if !self.in_placemark && self.lines.is_empty() && self.points.is_empty() {
            return;
        }
        self.in_placemark = false;
        let name = std::mem::take(&mut self.name);
        for mut l in self.lines.drain(..) {
            if l.points.is_empty() {
                continue;
            }
            l.name = name.clone();
            self.set.lines.push(l);
        }
        for (lat, lon, ele) in self.points.drain(..) {
            self.set.waypoints.push(Waypoint { name: name.clone(), lat, lon, ele });
        }
    }
}

/// KML's coordinates: `lon,lat[,alt]` tuples separated by white space
/// (some writers put spaces after the commas too).
pub fn parse_coordinates(text: &str) -> Vec<(f64, f64, f32)> {
    let mut out = Vec::new();
    let mut tuple: Vec<f64> = Vec::with_capacity(3);
    let mut number = String::new();
    let mut after_comma = false;
    let flush_number = |number: &mut String, tuple: &mut Vec<f64>| {
        if !number.is_empty() {
            tuple.push(number.parse().unwrap_or(f64::NAN));
            number.clear();
        }
    };
    for c in text.chars() {
        match c {
            ',' => {
                flush_number(&mut number, &mut tuple);
                after_comma = true;
            }
            c if c.is_whitespace() => {
                if !number.is_empty() {
                    flush_number(&mut number, &mut tuple);
                    after_comma = false;
                    // A comma may still follow ("lon , lat"): the tuple ends
                    // at the next number that doesn't come after one.
                }
            }
            c => {
                if !after_comma && !tuple.is_empty() && number.is_empty() {
                    push_tuple(&mut out, &mut tuple);
                }
                after_comma = false;
                number.push(c);
            }
        }
    }
    flush_number(&mut number, &mut tuple);
    push_tuple(&mut out, &mut tuple);
    out
}

fn push_tuple(out: &mut Vec<(f64, f64, f32)>, tuple: &mut Vec<f64>) {
    if tuple.len() >= 2 && out.len() < super::xml::MAX_POINTS {
        let ele = tuple.get(2).map_or(f32::NAN, |&v| v as f32);
        out.push((tuple[1], tuple[0], ele));
    }
    tuple.clear();
}
