//! GeoJSON: `LineString` and `MultiLineString` as lines, `Point` and
//! `MultiPoint` as waypoints, `Polygon` and `MultiPolygon` as outlines,
//! inside features, feature collections and geometry collections.  Times
//! come from the `coordTimes` or `coordinateProperties.times` that
//! converters from GPX (togeojson) write, heart rates from their `heart`
//! or `heartRates`.
//!
//! Coordinates are read straight into compact positions by a serde visitor
//! rather than through `serde_json::Value`, which would take ten times the
//! memory for a large file.

use super::{Line, LineKind, TrackPoint, TrackSet, Waypoint, parse_iso8601};
use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use slerp_scene::LoadError;

pub fn parse_geojson(data: &[u8]) -> Result<TrackSet, LoadError> {
    let root: Object = serde_json::from_slice(data).map_err(|e| LoadError::invalid(format!("GeoJSON: {e}")))?;
    let mut set = TrackSet { format: "GeoJSON".into(), ..Default::default() };
    if let Some(name) = root.properties.as_ref().and_then(|p| p.name.0.clone()).or(root.name.0.clone()) {
        set.name = Some(name);
    }
    add(&root, None, &mut set, 0);
    Ok(set)
}

/// Any GeoJSON object; which fields matter depends on `type`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Object {
    #[serde(rename = "type")]
    kind: Lenient,
    coordinates: Option<Node>,
    geometries: Vec<Object>,
    features: Vec<Object>,
    geometry: Option<Box<Object>>,
    properties: Option<Properties>,
    /// Some files name the collection at the top.
    name: Lenient,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Properties {
    #[serde(alias = "title")]
    name: Lenient,
    #[serde(rename = "coordTimes")]
    coord_times: Option<Node>,
    #[serde(rename = "heartRates")]
    heart_rates: Option<Node>,
    #[serde(rename = "coordinateProperties")]
    coordinate_properties: Option<CoordinateProperties>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CoordinateProperties {
    times: Option<Node>,
    heart: Option<Node>,
}

/// A string where one is expected, and nothing (rather than an error) for
/// anything else.
#[derive(Default)]
struct Lenient(Option<String>);

impl<'de> Deserialize<'de> for Lenient {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Node::deserialize(d)? {
            Node::Text(s) => Lenient(Some(s)),
            _ => Lenient(None),
        })
    }
}

/// Nested arrays of numbers or strings, compact at the leaves: a position
/// is one `Pos`, not an array of three values.
#[derive(Debug)]
enum Node {
    Num(f64),
    Text(String),
    Pos([f64; 3], u8),
    /// More than three numbers: a list of values, such as heart rates.
    Nums(Vec<f64>),
    List(Vec<Node>),
    Other,
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a GeoJSON value")
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Num(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Num(v as f64))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Num(v as f64))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Text(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Node, E> {
        Ok(Node::Text(v))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Node, E> {
        Ok(Node::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Other)
    }

    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Node::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items: Vec<Node> = Vec::new();
        let mut nums: Vec<f64> = Vec::new();
        while let Some(item) = seq.next_element::<Node>()? {
            match item {
                Node::Num(v) if items.is_empty() => nums.push(v),
                other => {
                    // A list that mixes numbers in keeps them as items.
                    items.extend(nums.drain(..).map(Node::Num));
                    items.push(other);
                }
            }
        }
        Ok(match nums.len() {
            0 => Node::List(items),
            1..=3 => {
                let mut pos = [0.0f64; 3];
                pos[..nums.len()].copy_from_slice(&nums);
                Node::Pos(pos, nums.len() as u8)
            }
            _ => Node::Nums(nums),
        })
    }
}

/// The deepest collections nest; deeper input is a hostile file.
const MAX_DEPTH: u32 = 64;

fn add(o: &Object, props: Option<&Properties>, set: &mut TrackSet, depth: u32) {
    if depth > MAX_DEPTH {
        return;
    }
    let kind = o.kind.0.as_deref().unwrap_or("").to_ascii_lowercase();
    let props = o.properties.as_ref().or(props);
    let name = props.and_then(|p| p.name.0.clone()).unwrap_or_default();
    let coords = o.coordinates.as_ref();
    match kind.as_str() {
        "featurecollection" => {
            for f in &o.features {
                add(f, None, set, depth + 1);
            }
        }
        "feature" => {
            if let Some(g) = &o.geometry {
                add(g, o.properties.as_ref(), set, depth + 1);
            }
        }
        "geometrycollection" => {
            for g in &o.geometries {
                add(g, props, set, depth + 1);
            }
        }
        "point" => {
            if let Some(Node::Pos(p, n)) = coords {
                set.waypoints.push(waypoint(&name, p, *n));
            }
        }
        "multipoint" => {
            for p in list(coords) {
                if let Node::Pos(p, n) = p {
                    set.waypoints.push(waypoint(&name, p, *n));
                }
            }
        }
        "linestring" => {
            let times = times_of(props).and_then(|t| first_list(t));
            let hr = heart_of(props).and_then(|t| first_list(t));
            push_line(set, &name, LineKind::Line, coords, times, hr);
        }
        "multilinestring" => {
            let times = times_of(props);
            let hr = heart_of(props);
            for (i, part) in list(coords).iter().enumerate() {
                let t = times.and_then(|t| nth_list(t, i));
                let h = hr.and_then(|t| nth_list(t, i));
                push_line(set, &name, LineKind::Line, Some(part), t, h);
            }
        }
        "polygon" => {
            for ring in list(coords) {
                push_line(set, &name, LineKind::Outline, Some(ring), None, None);
            }
        }
        "multipolygon" => {
            for polygon in list(coords) {
                for ring in list(Some(polygon)) {
                    push_line(set, &name, LineKind::Outline, Some(ring), None, None);
                }
            }
        }
        _ => {}
    }
}

fn list(n: Option<&Node>) -> &[Node] {
    match n {
        Some(Node::List(v)) => v,
        _ => &[],
    }
}

fn times_of(p: Option<&Properties>) -> Option<&Node> {
    let p = p?;
    p.coord_times.as_ref().or(p.coordinate_properties.as_ref().and_then(|c| c.times.as_ref()))
}

fn heart_of(p: Option<&Properties>) -> Option<&Node> {
    let p = p?;
    p.heart_rates.as_ref().or(p.coordinate_properties.as_ref().and_then(|c| c.heart.as_ref()))
}

/// Whether a node is one line's values rather than a list of lines'.
fn is_flat(n: &Node) -> bool {
    match n {
        Node::List(v) => v.iter().all(|x| matches!(x, Node::Num(_) | Node::Text(_) | Node::Other)),
        Node::Nums(_) | Node::Pos(..) => true,
        _ => false,
    }
}

/// One line's values, or the first line's of a list of them.
fn first_list(n: &Node) -> Option<&Node> {
    match n {
        n if is_flat(n) => Some(n),
        Node::List(v) => v.first().and_then(first_list),
        _ => None,
    }
}

/// The `i`th line's values from a list of them.
fn nth_list(n: &Node, i: usize) -> Option<&Node> {
    match n {
        Node::List(v) if !is_flat(n) => v.get(i).and_then(first_list),
        _ => None,
    }
}

/// A value from one line's values.
enum Item<'a> {
    Num(f64),
    Text(&'a str),
}

fn item(n: &Node, i: usize) -> Option<Item<'_>> {
    match n {
        Node::List(v) => match v.get(i)? {
            Node::Num(x) => Some(Item::Num(*x)),
            Node::Text(s) => Some(Item::Text(s)),
            _ => None,
        },
        Node::Nums(v) => v.get(i).map(|x| Item::Num(*x)),
        Node::Pos(p, n) => (i < *n as usize).then(|| Item::Num(p[i])),
        _ => None,
    }
}

fn waypoint(name: &str, p: &[f64; 3], n: u8) -> Waypoint {
    Waypoint { name: name.to_string(), lat: p[1], lon: p[0], ele: if n >= 3 { p[2] as f32 } else { f32::NAN } }
}

fn push_line(
    set: &mut TrackSet,
    name: &str,
    kind: LineKind,
    coords: Option<&Node>,
    times: Option<&Node>,
    hr: Option<&Node>,
) {
    let mut points = Vec::new();
    for (i, c) in list(coords).iter().enumerate() {
        let Node::Pos(p, n) = c else { continue };
        if *n < 2 {
            continue;
        }
        let mut pt = TrackPoint::new(p[1], p[0]);
        if *n >= 3 {
            pt.ele = p[2] as f32;
        }
        match times.and_then(|t| item(t, i)) {
            Some(Item::Text(s)) => pt.time = parse_iso8601(s).unwrap_or(f64::NAN),
            // Milliseconds since 1970, as some writers give them.
            Some(Item::Num(ms)) => pt.time = ms / 1000.0,
            None => {}
        }
        if let Some(Item::Num(h)) = hr.and_then(|t| item(t, i)) {
            pt.hr = h as f32;
        }
        points.push(pt);
    }
    if !points.is_empty() {
        set.lines.push(Line { name: name.to_string(), kind, points });
    }
}
