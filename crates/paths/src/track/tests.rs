//! Track tests: measuring (distance, projection, climb, speeds), each
//! format's reader on small documents and real samples, FIT's message
//! layouts and compressed timestamps, the drawings, and damaged files.

use super::stats::{climb, top_speed};
use super::*;
use std::path::{Path, PathBuf};

fn asset(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(rel);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/*┌─────────────────╮
| Measuring       |
└─────────────────╯*/
//region Measuring

#[test]
fn distances_and_projection() {
    let a = TrackPoint::new(45.0, 7.0);
    let b = TrackPoint::new(46.0, 7.0);
    // A degree of latitude on the mean sphere.
    assert!((distance(&a, &b) - 111_195.08).abs() < 1.0);
    let c = TrackPoint::new(45.0, 8.0);
    assert!((distance(&a, &c) - 111_195.08 * 45f64.to_radians().cos()).abs() < 50.0);
    let p = Projection::new(45.0, 7.0);
    let (e, n) = p.project(45.0 + 1000.0 / 111_195.08, 7.0);
    assert!(e.abs() < 1e-6 && (n - 1000.0).abs() < 1e-3);
    let (e, _) = p.project(45.0, 7.0 + 1000.0 / (111_195.08 * 45f64.to_radians().cos()));
    assert!((e - 1000.0).abs() < 1e-3);
    // A track over the antimeridian is centred on it, not on Greenwich.
    let set = TrackSet {
        lines: vec![Line {
            name: String::new(),
            kind: LineKind::Track,
            points: vec![TrackPoint::new(-17.0, 179.5), TrackPoint::new(-17.0, -179.5)],
        }],
        ..Default::default()
    };
    let p = set.projection();
    assert!((p.lon0.abs() - 180.0).abs() < 1e-9, "{}", p.lon0);
    let (e1, _) = p.project(-17.0, 179.5);
    let (e2, _) = p.project(-17.0, -179.5);
    assert!((e2 - e1 - 106_330.0).abs() < 200.0, "{}", e2 - e1);
}

#[test]
fn climb_ignores_noise() {
    // A 100 m climb in 1 m steps with a metre of jitter, then down 50 m.
    let mut ele: Vec<f32> = (0..=100).map(|i| i as f32 + if i % 2 == 0 { 0.8 } else { -0.8 }).collect();
    ele.extend((0..=50).map(|i| 100.0 - i as f32));
    let (up, down) = climb(&ele, 3.0);
    assert!((up - 100.0).abs() < 4.0, "{up}");
    assert!((down - 50.0).abs() < 4.0, "{down}");
    // Jitter alone climbs nothing.
    let flat: Vec<f32> = (0..1000).map(|i| if i % 3 == 0 { 101.0 } else { 99.5 }).collect();
    assert_eq!(climb(&flat, 3.0), (0.0, 0.0));
    assert_eq!(climb(&[f32::NAN, 5.0, f32::NAN], 3.0), (0.0, 0.0));
}

#[test]
fn top_speed_ignores_spikes() {
    let mut v = vec![10.0f32; 100];
    v[50] = 80.0;
    v[51] = 70.0;
    assert_eq!(top_speed(&v), 10.0);
    for s in &mut v[20..30] {
        *s = 15.0;
    }
    assert_eq!(top_speed(&v), 15.0);
    assert_eq!(top_speed(&[3.0, 4.0]), 4.0);
}

/// A straight track north at 5 m/s, 1 fix a second, rising 1 m each, with
/// a 60 s stop in the middle.
fn steady_track() -> TrackSet {
    let mut points = Vec::new();
    let mut t = 1_700_000_000.0;
    let mut lat = 45.0;
    for i in 0..200 {
        let mut p = TrackPoint::new(lat, 7.0);
        p.time = t;
        p.ele = 100.0 + i as f32;
        p.hr = 140.0;
        p.power = if i % 2 == 0 { 200.0 } else { 0.0 };
        points.push(p);
        if i == 100 {
            t += 60.0;
        } else {
            t += 1.0;
            lat += 5.0 / 111_195.08;
        }
    }
    TrackSet { lines: vec![Line { name: "Test".into(), kind: LineKind::Track, points }], ..Default::default() }
}

#[test]
fn statistics() {
    let set = steady_track();
    let proj = set.projection();
    let prof = Profile::of(&set, &proj);
    let s = Stats::of(&set, &prof);
    assert!((s.distance - 5.0 * 198.0).abs() < 0.5, "{}", s.distance);
    assert_eq!(s.elapsed, Some(258.0));
    assert_eq!(s.moving_time, Some(198.0));
    assert!((s.avg_speed.unwrap() - 5.0).abs() < 0.01);
    assert!((s.max_speed.unwrap() - 5.0).abs() < 0.01);
    assert!((s.ascent.unwrap() - 198.0).abs() < 3.0);
    assert_eq!(s.max_ele, Some(299.0));
    assert_eq!(s.avg_hr, Some(140.0));
    assert_eq!(s.avg_power, Some(100.0));
    assert!(s.details(&set).iter().any(|(k, v)| k == "Distance" && v == "990 m"));
    // The profile runs north: y grows.
    assert!(prof.pos[199][1] > prof.pos[0][1] + 900.0);
}

//endregion

/*┌─────────────────╮
| Formats         |
└─────────────────╯*/
//region Formats

#[test]
fn gpx_tracks_routes_waypoints_and_extensions() {
    let gpx = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="Test &amp; Co" xmlns="http://www.topografix.com/GPX/1/1"
     xmlns:gpxtpx="http://www.garmin.com/xmlschemas/TrackPointExtension/v1">
  <metadata><name>Morning ride</name></metadata>
  <wpt lat="45.1" lon="7.1"><ele>500</ele><name>Summit</name></wpt>
  <trk><name>Ride</name><type>cycling</type>
    <trkseg>
      <trkpt lat="45.0" lon="7.0"><ele>100</ele><time>2024-05-01T10:00:00Z</time>
        <extensions><gpxtpx:TrackPointExtension><gpxtpx:hr>120</gpxtpx:hr><gpxtpx:cad>85</gpxtpx:cad></gpxtpx:TrackPointExtension><power>250</power></extensions></trkpt>
      <trkpt lat="45.001" lon="7.0"><ele>101</ele><time>2024-05-01T10:00:05Z</time></trkpt>
    </trkseg>
    <trkseg><trkpt lat="45.002" lon="7.0"/><trkpt lat="45.003" lon="7.0"/></trkseg>
  </trk>
  <rte><name>Plan</name><rtept lat="45" lon="7"/><rtept lat="45.01" lon="7.01"/></rte>
</gpx>"#;
    let set = parse_gpx(gpx.as_bytes());
    assert_eq!(set.format, "GPX 1.1");
    assert_eq!(set.creator.as_deref(), Some("Test & Co"));
    assert_eq!(set.name.as_deref(), Some("Morning ride"));
    assert_eq!(set.sport.as_deref(), Some("cycling"));
    assert_eq!(set.lines.len(), 3);
    assert_eq!(set.lines[0].name, "Ride");
    assert_eq!(set.lines[2].kind, LineKind::Route);
    let p = set.lines[0].points[0];
    assert_eq!((p.ele, p.hr, p.cad, p.power), (100.0, 120.0, 85.0, 250.0));
    assert_eq!(p.time, 1714557600.0);
    assert_eq!(set.waypoints.len(), 1);
    assert_eq!(set.waypoints[0].name, "Summit");
    // GPX 1.0 speed.
    let gpx10 =
        r#"<gpx version="1.0"><trk><trkseg><trkpt lat="1" lon="2"><speed>3.5</speed></trkpt></trkseg></trk></gpx>"#;
    assert_eq!(parse_gpx(gpx10.as_bytes()).lines[0].points[0].speed, 3.5);
}

#[test]
fn tcx_activities_and_courses() {
    let tcx = r#"<?xml version="1.0"?>
<TrainingCenterDatabase xmlns="http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2" xmlns:ns3="http://www.garmin.com/xmlschemas/ActivityExtension/v2">
 <Activities><Activity Sport="Biking"><Id>2024-05-01T10:00:00Z</Id>
  <Lap StartTime="2024-05-01T10:00:00Z"><TotalTimeSeconds>600</TotalTimeSeconds><DistanceMeters>4000</DistanceMeters><Calories>100</Calories>
   <Track>
    <Trackpoint><Time>2024-05-01T10:00:00Z</Time><Position><LatitudeDegrees>45</LatitudeDegrees><LongitudeDegrees>7</LongitudeDegrees></Position>
     <AltitudeMeters>200</AltitudeMeters><DistanceMeters>0</DistanceMeters><HeartRateBpm><Value>130</Value></HeartRateBpm><Cadence>90</Cadence>
     <Extensions><ns3:TPX><ns3:Speed>6.5</ns3:Speed><ns3:Watts>210</ns3:Watts></ns3:TPX></Extensions></Trackpoint>
    <Trackpoint><Time>2024-05-01T10:00:01Z</Time><Position><LatitudeDegrees>45.0001</LatitudeDegrees><LongitudeDegrees>7</LongitudeDegrees></Position></Trackpoint>
    <Trackpoint><Time>2024-05-01T10:00:02Z</Time></Trackpoint>
   </Track></Lap>
  <Creator><Name>Edge 530</Name></Creator></Activity></Activities>
 <Courses><Course><Name>Loop</Name><Track><Trackpoint><Position><LatitudeDegrees>46</LatitudeDegrees><LongitudeDegrees>8</LongitudeDegrees></Position></Trackpoint></Track>
  <CoursePoint><Name>Water</Name><Position><LatitudeDegrees>46.1</LatitudeDegrees><LongitudeDegrees>8.1</LongitudeDegrees></Position></CoursePoint></Course></Courses>
</TrainingCenterDatabase>"#;
    let mut set = parse_tcx(tcx.as_bytes());
    assert_eq!(set.sport.as_deref(), Some("Biking"));
    assert_eq!(set.creator.as_deref(), Some("Edge 530"));
    assert_eq!(set.device.timer, Some(600.0));
    assert_eq!(set.device.distance, Some(4000.0));
    assert_eq!(set.device.laps, 1);
    assert_eq!(set.lines.len(), 2);
    let p = set.lines[0].points[0];
    assert_eq!((p.ele, p.hr, p.cad, p.speed, p.power), (200.0, 130.0, 90.0, 6.5, 210.0));
    assert_eq!(set.lines[1].kind, LineKind::Route);
    assert_eq!(set.lines[1].name, "Loop");
    assert_eq!(set.waypoints[0].name, "Water");
    // The trackpoint without a position is dropped when tidied.
    set.tidy(1000);
    assert_eq!(set.lines[0].points.len(), 2);
}

#[test]
fn kml_geometries_and_gx_tracks() {
    let kml = r##"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2" xmlns:gx="http://www.google.com/kml/ext/2.2"><Document><name>Trip</name>
 <Placemark><name>Path</name><LineString><coordinates>7.0,45.0,100 7.1,45.1,150
   7.2 , 45.2 , 120</coordinates></LineString></Placemark>
 <Placemark><Point><coordinates>7.5,45.5</coordinates></Point><name>Hut</name></Placemark>
 <Placemark><name>Field</name><Polygon><outerBoundaryIs><LinearRing><coordinates>7,45 7.01,45 7.01,45.01 7,45</coordinates></LinearRing></outerBoundaryIs>
   <innerBoundaryIs><LinearRing><coordinates>7.002,45.002 7.003,45.002 7.003,45.003 7.002,45.002</coordinates></LinearRing></innerBoundaryIs></Polygon></Placemark>
 <Placemark><name>Both</name><MultiGeometry><Point><coordinates>8,46</coordinates></Point><LineString><coordinates>8,46 8.1,46.1</coordinates></LineString></MultiGeometry></Placemark>
 <Placemark><name>Recorded</name><gx:Track>
   <when>2024-05-01T10:00:00Z</when><when>2024-05-01T10:00:10Z</when>
   <gx:coord>9.0 47.0 300</gx:coord><gx:coord>9.001 47.001 310</gx:coord>
   <ExtendedData><SchemaData schemaUrl="#s"><gx:SimpleArrayData name="heartrate"><gx:value>100</gx:value><gx:value>110</gx:value></gx:SimpleArrayData></SchemaData></ExtendedData>
 </gx:Track></Placemark>
</Document></kml>"##;
    let set = parse_kml(kml.as_bytes());
    assert_eq!(set.name.as_deref(), Some("Trip"));
    let names: Vec<(&str, LineKind, usize)> =
        set.lines.iter().map(|l| (l.name.as_str(), l.kind, l.points.len())).collect();
    assert_eq!(
        names,
        [
            ("Path", LineKind::Line, 3),
            ("Field", LineKind::Outline, 4),
            ("Field", LineKind::Outline, 4),
            ("Both", LineKind::Line, 2),
            ("Recorded", LineKind::Track, 2)
        ]
    );
    let p = set.lines[0].points[2];
    assert!((p.lat - 45.2).abs() < 1e-9 && (p.lon - 7.2).abs() < 1e-9 && p.ele == 120.0);
    let r = set.lines[4].points[1];
    assert_eq!((r.ele, r.hr, r.time), (310.0, 110.0, 1714557610.0));
    let wp: Vec<&str> = set.waypoints.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(wp, ["Hut", "Both"]);
}

/// A zip with one stored and one deflated entry.
fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in entries {
        let stored = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
        let offset = out.len() as u32;
        let method: u16 = if *deflate { 8 } else { 0 };
        let mut header = Vec::new();
        header.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        header.extend_from_slice(&[20, 0, 0, 0]);
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&[0; 8]);
        header.extend_from_slice(&(stored.len() as u32).to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&header);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&stored);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(stored.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn kmz_archives() {
    let kml = br#"<kml><Placemark><LineString><coordinates>7,45 7.1,45.1</coordinates></LineString></Placemark></kml>"#;
    for deflate in [false, true] {
        let z = zip(&[("files/icon.png", b"\x89PNG", false), ("doc.kml", kml, deflate)]);
        let set = parse_kmz(&z).unwrap();
        assert_eq!(set.format, "KMZ");
        assert_eq!(set.lines[0].points.len(), 2);
        assert_eq!(crate::sniff(Path::new("a.kmz"), &z), Some(crate::PathFormat::Kmz));
        // Damaged archives fail cleanly.
        for cut in 0..z.len() {
            let _ = parse_kmz(&z[..cut]);
        }
    }
    assert!(parse_kmz(&zip(&[("a.txt", b"x", false)])).is_err());
}

#[test]
fn geojson_geometries() {
    let json = r#"{"type":"FeatureCollection","name":"Walks","features":[
      {"type":"Feature","properties":{"name":"Walk","coordTimes":["2024-05-01T10:00:00Z","2024-05-01T10:00:10Z"]},
       "geometry":{"type":"LineString","coordinates":[[7.0,45.0,100],[7.001,45.001,110]]}},
      {"type":"Feature","properties":{"name":"Two","coordinateProperties":{"times":[["2024-05-01T11:00:00Z","2024-05-01T11:00:05Z"],["2024-05-01T12:00:00Z"]],"heart":[[90,95],[100]]}},
       "geometry":{"type":"MultiLineString","coordinates":[[[8,46],[8.1,46.1]],[[9,47]]]}},
      {"type":"Feature","properties":{"name":12},"geometry":{"type":"Point","coordinates":[7.5,45.5,300]}},
      {"type":"Feature","properties":null,"geometry":{"type":"Polygon","coordinates":[[[7,45],[7.1,45],[7.1,45.1],[7,45]]]}},
      {"type":"Feature","geometry":{"type":"GeometryCollection","geometries":[{"type":"MultiPoint","coordinates":[[1,2],[3,4]]},
        {"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[1,1],[0,0]]]]}]}},
      {"type":"Feature","geometry":null},
      {"type":"Feature","geometry":{"type":"LineString","coordinates":"oops"}}
    ]}"#;
    let set = parse_geojson(json.as_bytes()).unwrap();
    assert_eq!(set.name.as_deref(), Some("Walks"));
    let lines: Vec<(&str, LineKind, usize)> =
        set.lines.iter().map(|l| (l.name.as_str(), l.kind, l.points.len())).collect();
    assert_eq!(
        lines,
        [
            ("Walk", LineKind::Line, 2),
            ("Two", LineKind::Line, 2),
            ("Two", LineKind::Line, 1),
            ("", LineKind::Outline, 4),
            ("", LineKind::Outline, 4)
        ]
    );
    assert_eq!(set.lines[0].points[1].time, 1714557610.0);
    assert_eq!(set.lines[0].points[1].ele, 110.0);
    assert_eq!(set.lines[1].points[1].hr, 95.0);
    assert_eq!(set.lines[2].points[0].time, 1714564800.0);
    assert_eq!(set.waypoints.len(), 3);
    // Pure polygons are outlines; nothing at all, or nonsense, is an error
    // or an empty set, never a panic.
    let polygons = r#"{"type":"Polygon","coordinates":[[[7,45],[7.1,45],[7.1,45.1],[7,45]]]}"#;
    let t = crate::Track::new(parse_geojson(polygons.as_bytes()).unwrap(), 1000).unwrap();
    assert!(t.profile.is_empty());
    assert!(track_thumbnail(&t.set, &t.profile, 64).is_some());
    let _ = t.scene(&TrackLook::for_profile(&t.profile), true);
    assert!(crate::Track::new(parse_geojson(br#"{"type":"FeatureCollection","features":[]}"#).unwrap(), 10).is_err());
    let _ = parse_geojson(b"[1,2,3]");
    assert!(parse_geojson(b"{\"type\":").is_err());
    let deep = "[".repeat(10_000);
    assert!(parse_geojson(format!("{{\"type\":\"LineString\",\"coordinates\":{deep}").as_bytes()).is_err());
}

//endregion

/*┌─────────────────╮
| FIT             |
└─────────────────╯*/
//region FIT

/// Writes FIT records with a valid header and a zero CRC.
#[derive(Default)]
struct FitWriter {
    data: Vec<u8>,
}

impl FitWriter {
    fn def(&mut self, local: u8, global: u16, big: bool, fields: &[(u8, u8, u8)], dev: &[(u8, u8, u8)]) {
        self.data.push(0x40 | local | if dev.is_empty() { 0 } else { 0x20 });
        self.data.push(0);
        self.data.push(big as u8);
        self.data.extend_from_slice(&if big { global.to_be_bytes() } else { global.to_le_bytes() });
        self.data.push(fields.len() as u8);
        for f in fields {
            self.data.extend_from_slice(&[f.0, f.1, f.2]);
        }
        if !dev.is_empty() {
            self.data.push(dev.len() as u8);
            for f in dev {
                self.data.extend_from_slice(&[f.0, f.1, f.2]);
            }
        }
    }

    fn rec(&mut self, header: u8, bytes: &[u8]) {
        self.data.push(header);
        self.data.extend_from_slice(bytes);
    }

    fn finish(&self) -> Vec<u8> {
        let mut out = vec![14, 0x20];
        out.extend_from_slice(&2195u16.to_le_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        out.extend_from_slice(b".FIT");
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&[0, 0]);
        out
    }
}

fn semicircles(deg: f64) -> i32 {
    (deg / 180.0 * 2_147_483_648.0).round() as i32
}

fn sample_fit() -> Vec<u8> {
    let mut w = FitWriter::default();
    // file_id: Wahoo, product name.
    w.def(0, 0, false, &[(1, 2, 0x84), (8, 8, 0x07)], &[]);
    w.rec(0, &[32, 0, b'E', b'L', b'E', b'M', b'N', b'T', 0, 0]);
    // Records with a timestamp, position, enhanced altitude, heart rate,
    // and a developer field of three bytes.
    w.def(1, 20, false, &[(253, 4, 0x86), (0, 4, 0x85), (1, 4, 0x85), (78, 4, 0x86), (3, 1, 0x02)], &[(0, 3, 0)]);
    let rec = |t: u32, lat: f64, hr: u8| {
        let mut b = t.to_le_bytes().to_vec();
        b.extend_from_slice(&semicircles(lat).to_le_bytes());
        b.extend_from_slice(&semicircles(7.0).to_le_bytes());
        b.extend_from_slice(&3250u32.to_le_bytes());
        b.push(hr);
        b.extend_from_slice(&[9, 9, 9]);
        b
    };
    w.rec(1, &rec(1000, 45.0, 120));
    // A record whose position is FIT's "invalid": no point.
    let mut bad = rec(1001, 45.0, 121);
    bad[4..8].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
    w.rec(1, &bad);
    // Compressed timestamps, local type 2: position and power, no time
    // field, in big-endian.
    w.def(2, 20, true, &[(0, 4, 0x85), (1, 4, 0x85), (7, 2, 0x84)], &[]);
    let ct = |lat: f64, power: u16| {
        let mut b = semicircles(lat).to_be_bytes().to_vec();
        b.extend_from_slice(&semicircles(7.0).to_be_bytes());
        b.extend_from_slice(&power.to_be_bytes());
        b
    };
    // 1001's low five bits are 9: offset 20 gives 1012, then offset 3 rolls
    // over to 1027.
    w.rec(0x80 | (2 << 5) | 20, &ct(45.001, 200));
    w.rec(0x80 | (2 << 5) | 3, &ct(45.002, 210));
    // A session: 10 m, 25 s elapsed, cycling.
    w.def(3, 18, false, &[(7, 4, 0x86), (9, 4, 0x86), (5, 1, 0x00), (22, 2, 0x84)], &[]);
    let mut s = 25000u32.to_le_bytes().to_vec();
    s.extend_from_slice(&1000u32.to_le_bytes());
    s.push(2);
    s.extend_from_slice(&12u16.to_le_bytes());
    w.rec(3, &s);
    w.finish()
}

#[test]
fn fit_messages_and_compressed_timestamps() {
    let data = sample_fit();
    assert!(is_fit(&data));
    let set = parse_fit(&data).unwrap();
    assert_eq!(set.creator.as_deref(), Some("Wahoo ELEMNT"));
    assert_eq!(set.sport.as_deref(), Some("Cycling"));
    assert_eq!(set.device.distance, Some(10.0));
    assert_eq!(set.device.elapsed, Some(25.0));
    assert_eq!(set.device.ascent, Some(12.0));
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
    let pts = &set.lines[0].points;
    assert_eq!(pts.len(), 3);
    let epoch = 631_065_600.0;
    assert_eq!(pts[0].time, 1000.0 + epoch);
    assert_eq!(pts[1].time, 1012.0 + epoch);
    assert_eq!(pts[2].time, 1027.0 + epoch);
    assert!((pts[0].lat - 45.0).abs() < 1e-6 && (pts[0].lon - 7.0).abs() < 1e-6);
    assert_eq!((pts[0].ele, pts[0].hr), (150.0, 120.0));
    assert_eq!(pts[2].power, 210.0);
    assert!((pts[2].lat - 45.002).abs() < 1e-6);
    // Two files chained: both read.
    let mut two = data.clone();
    two.extend_from_slice(&data);
    assert_eq!(parse_fit(&two).unwrap().point_count(), 6);
    // Cut anywhere: whatever came before stays, nothing panics.
    for cut in 0..data.len() {
        if let Ok(set) = parse_fit(&data[..cut]) {
            assert!(set.point_count() <= 3);
        }
    }
}

/// A recording where one byte was dropped mid-file (as in a Strava
/// Android export) loses the records around the gap, not the rest of the
/// ride.
#[test]
fn fit_resynchronises_after_damage() {
    let mut w = FitWriter::default();
    w.def(1, 20, false, &[(253, 4, 0x86), (0, 4, 0x85), (1, 4, 0x85), (3, 1, 0x02)], &[]);
    let mut starts = Vec::new();
    for i in 0..40u32 {
        starts.push(w.data.len());
        let mut b = (1000 + i).to_le_bytes().to_vec();
        b.extend_from_slice(&semicircles(45.0 + i as f64 * 1e-4).to_le_bytes());
        b.extend_from_slice(&semicircles(7.0).to_le_bytes());
        b.push(100 + i as u8);
        w.rec(1, &b);
    }
    let whole = parse_fit(&w.finish()).unwrap();
    assert_eq!(whole.point_count(), 40);
    assert!(whole.warnings.is_empty());
    // Drop a byte from the middle of record 15 (past its header).
    w.data.remove(starts[15] + 6);
    let set = parse_fit(&w.finish()).unwrap();
    assert!(set.point_count() >= 38, "{}", set.point_count());
    assert!(set.warnings.iter().any(|m| m.contains("skipped")), "{:?}", set.warnings);
    // The records after the gap are the right ones.
    let last = set.lines[0].points.last().unwrap();
    assert_eq!(last.hr, 139.0);
    assert!((last.lat - (45.0 + 39.0 * 1e-4)).abs() < 1e-6);
    // Noise in place of records ends the stream cleanly.
    let mut noise = w.finish();
    let at = noise.len() - 200;
    for (i, b) in noise[at..].iter_mut().enumerate() {
        *b = (i * 37 % 251) as u8;
    }
    let _ = parse_fit(&noise);
}

/// Strava's Android app (version 201.10) dropped bytes from its records, in
/// several places.  The ride is still 11 km, as its own summary says.
#[test]
fn damaged_real_recording_keeps_its_route() {
    let Some(fit) = asset("paths-extra/fitparse/strava-android-app-201.10-b1218918.fit") else { return };
    let t = crate::Track::new(parse_fit(&std::fs::read(fit).unwrap()).unwrap(), 1_000_000).unwrap();
    assert!(t.stats.points > 2500, "{}", t.stats.points);
    let device = t.set.device.distance.unwrap();
    assert!((t.stats.distance - device).abs() < device * 0.03, "{} against {device}", t.stats.distance);
    assert!(t.set.warnings.iter().any(|w| w.contains("skipped")));
    assert!(t.stats.elapsed.unwrap() < 7200.0);
}

#[test]
fn garmin_records_match_their_csv() {
    let (Some(fit), Some(csv)) = (
        asset("paths-extra/fitparse/garmin-edge-500-activity.fit"),
        asset("paths-extra/fitparse/garmin-edge-500-activity-records.csv"),
    ) else {
        return;
    };
    let set = parse_fit(&std::fs::read(fit).unwrap()).unwrap();
    let pts: Vec<TrackPoint> = set.lines.iter().flat_map(|l| l.points.iter().copied()).collect();
    let text = std::fs::read_to_string(csv).unwrap();
    let rows: Vec<Vec<&str>> =
        text.lines().skip(1).map(|l| l.split(',').collect::<Vec<_>>()).filter(|r| !r[5].is_empty()).collect();
    // The CSV repeats the last position for the few records that have
    // none; those aren't points.
    assert!(pts.len() <= rows.len() && pts.len() + 20 > rows.len(), "{} {}", pts.len(), rows.len());
    // Walk both in order; a CSV row that isn't a point must repeat the
    // position before it.
    let deg = |v: &str| v.parse::<f64>().unwrap() * 180.0 / 2_147_483_648.0;
    let mut next = 0;
    let mut skipped = 0;
    for (i, r) in rows.iter().enumerate() {
        let Some(p) = pts.get(next) else { break };
        let same = (p.lat - deg(r[5])).abs() < 1e-9 && (p.lon - deg(r[6])).abs() < 1e-9;
        if !same {
            assert!(i > 0 && r[5] == rows[i - 1][5] && r[6] == rows[i - 1][6], "row {i}");
            skipped += 1;
            continue;
        }
        next += 1;
        assert_eq!(p.hr, r[1].parse::<f32>().unwrap());
        assert!((p.ele - r[7].parse::<f32>().unwrap()).abs() < 0.01);
        assert!((p.speed - r[3].parse::<f32>().unwrap()).abs() < 0.001);
    }
    assert_eq!(next, pts.len());
    assert_eq!(skipped + pts.len(), rows.len());
    // "Sun Sep 25 06:00:22 PDT 2011".
    assert_eq!(pts[0].time, 1316955622.0);
}

//endregion

/*┌─────────────────╮
| Drawings        |
└─────────────────╯*/
//region Drawings

#[test]
fn thumbnails_keep_the_tracks_shape() {
    let set = steady_track();
    let proj = set.projection();
    let prof = Profile::of(&set, &proj);
    // About 1 km north and nothing east: a tall picture, no thinner than
    // a third of its height.
    let img = track_thumbnail(&set, &prof, 256).unwrap();
    assert_eq!((img.width, img.height), (85, 256));
    // The middle is drawn, the corners are clear.
    let at = |x: u32, y: u32| img.pixels[((y * img.width + x) * 4 + 3) as usize];
    assert!(at(42, 128) > 200);
    assert_eq!(at(0, 0), 0);
    let scene = crate::Track::new(set, 1000).unwrap().scene(&TrackLook::for_profile(&prof), true);
    assert!(scene.triangle_count() > 0);
    assert_eq!(scene.nodes[0].transform.scale.y, default_exaggeration(&prof));
}

//endregion

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

#[test]
fn damaged_tracks_never_panic() {
    let mut samples: Vec<(&str, Vec<u8>)> = vec![("fit", sample_fit())];
    for (kind, name) in [
        ("gpx", "paths-extra/togeojson/run.gpx"),
        ("tcx", "paths-extra/togeojson/sample.tcx"),
        ("kml", "paths-extra/togeojson/gxmultitrack.kml"),
        ("fit", "paths-extra/fitparse/2013-02-06-12-11-14.fit"),
    ] {
        if let Some(p) = asset(name) {
            samples.push((kind, std::fs::read(p).unwrap()));
        }
    }
    samples.push((
        "geojson",
        br#"{"type":"Feature","geometry":{"type":"LineString","coordinates":[[7,45,1],[7.1,45.1,2],[1e300,-1e300],[null,1]]}}"#
            .to_vec(),
    ));
    let run = |kind: &str, bytes: &[u8]| {
        let set = match kind {
            "gpx" => parse_gpx(bytes),
            "tcx" => parse_tcx(bytes),
            "kml" => parse_kml(bytes),
            "geojson" => match parse_geojson(bytes) {
                Ok(s) => s,
                Err(_) => return,
            },
            _ => match parse_fit(bytes) {
                Ok(s) => s,
                Err(_) => return,
            },
        };
        if let Ok(t) = crate::Track::new(set, 100_000) {
            let _ = track_thumbnail(&t.set, &t.profile, 64);
            let _ = t.scene(&TrackLook::for_profile(&t.profile), true);
            let _ = t.stats.details(&t.set);
        }
    };
    let mut rng = 0x853c49e6748fea9bu64;
    for (kind, bytes) in &samples {
        for cut in (0..bytes.len()).step_by((bytes.len() / 60).max(1)) {
            run(kind, &bytes[..cut]);
        }
        for _ in 0..40 {
            let mut m = bytes.clone();
            for _ in 0..8 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let i = (rng % m.len() as u64) as usize;
                m[i] = (rng >> 32) as u8;
            }
            run(kind, &m);
        }
    }
    // Absurd coordinates are dropped; absurd counts stay bounded.
    run("gpx", br#"<gpx><trk><trkseg><trkpt lat="1e308" lon="nan"/><trkpt lat="91" lon="0"/><trkpt lat="0" lon="0"><ele>1e30</ele></trkpt><trkpt lat="0.001" lon="0"><ele>-1e30</ele></trkpt></trkseg></trk></gpx>"#);
    let mut many = String::from("<gpx><trk><trkseg>");
    for i in 0..200_000 {
        many.push_str(&format!("<trkpt lat=\"{}\" lon=\"0\"/>", i as f64 * 1e-5));
    }
    many.push_str("</trkseg></trk></gpx>");
    let set = parse_gpx(many.as_bytes());
    let t = crate::Track::new(set, 50_000).unwrap();
    assert!(t.set.point_count() <= 50_001);
    // A FIT definition claiming 255 fields of 255 bytes, repeated.
    let mut w = FitWriter::default();
    let fields: Vec<(u8, u8, u8)> = (0..255).map(|i| (i as u8, 255, 0x0d)).collect();
    w.def(0, 20, false, &fields, &[]);
    for _ in 0..10 {
        w.rec(0, &[0u8; 1000]);
    }
    run("fit", &w.finish());
}

//endregion

#[test]
fn sniffs_every_format() {
    use crate::{PathFormat, sniff};
    let s = |name: &str, head: &[u8]| sniff(Path::new(name), head);
    assert_eq!(
        s("a.gpx", b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- x -->\n<gpx version=\"1.1\">"),
        Some(PathFormat::Gpx)
    );
    assert_eq!(s("a.xml", b"<TrainingCenterDatabase xmlns=\"x\">"), Some(PathFormat::Tcx));
    assert_eq!(s("a.kml", b"<?xml?><kml:kml xmlns:kml=\"x\">"), Some(PathFormat::Kml));
    assert_eq!(s("a.fit", &sample_fit()[..32]), Some(PathFormat::Fit));
    assert_eq!(s("a.geojson", b"{\"type\":\"Point\"}"), Some(PathFormat::GeoJson));
    assert_eq!(s("a.json", b"{\"type\": \"FeatureCollection\", \"features\": []}"), Some(PathFormat::GeoJson));
    assert_eq!(s("a.json", b"{\"name\": \"package\"}"), None);
    assert_eq!(s("a.bgcode", b"GCDE\x01\0\0\0\x01\0"), Some(PathFormat::Bgcode));
    assert_eq!(
        s("a.gcode", b"; generated by PrusaSlicer\n;\n; thumbnail begin 16x16 5\n; iVBOR"),
        Some(PathFormat::Gcode)
    );
    assert_eq!(s("a.nc", b"G21\nG90\nG0 X1 Y1\nG1 Z-1\n"), Some(PathFormat::Gcode));
    assert_eq!(s("a.nc", b"CDF\x01\0\0\0"), None);
    assert_eq!(s("a.g", b"hello world\nthis is text\n"), None);
    assert_eq!(s("a.txt", b"G1 X1\nG1 X2\n"), None);
    assert_eq!(s("a.xml", b"<svg>"), None);
}

#[test]
fn glitches_are_cleaned() {
    let mut set = steady_track();
    let pts = &mut set.lines[0].points;
    // A teleporting fix, one with an absurd elevation and heart rate, and
    // a first point that is off by hundreds of kilometres.
    pts[50].lat += 2.0;
    pts[60].ele = 1e9;
    pts[60].hr = 4000.0;
    pts[0].lon += 3.0;
    let n = pts.len();
    set.tidy(10_000);
    let pts = &set.lines[0].points;
    assert_eq!(pts.len(), n - 2, "the two spikes are gone");
    assert!(pts.iter().all(|p| (p.lon - 7.0).abs() < 1e-9));
    let p60 = pts.iter().find(|p| p.time == 1_700_000_000.0 + 59.0 + 1.0).unwrap();
    assert!(p60.ele.is_nan() && p60.hr.is_nan());
    // A real out-and-back over two samples stays.
    let mut set = steady_track();
    set.lines[0].points[70].lat += 0.02;
    set.lines[0].points[71].lat += 0.02;
    let n = set.point_count();
    set.tidy(10_000);
    assert_eq!(set.point_count(), n);
}

/// Found by fuzzing: a FIT record time near the end of the 32-bit range
/// overflowed the check for times that jump back.
#[test]
fn fit_times_at_the_end_of_the_range() {
    let data = [
        0x0e, 0x10, 0x00, 0x00, 0x6f, 0x15, 0x00, 0x00, 0x2e, 0x46, 0x49, 0x54, 0x00, 0x40, 0x45, 0x00, 0x00, 0x14,
        0x00, 0x06, 0xfd, 0x04, 0x86, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0x0a, 0x81, 0x01, 0x84, 0x06,
        0x01, 0x00, 0x05, 0xb9, 0x00, 0x65, 0x0a, 0x81, 0x01, 0x63, 0xe3, 0x18, 0x05, 0xfe, 0xff, 0xff, 0xff, 0xff,
        0xb9, 0xa8, 0x0a, 0x01,
    ];
    let _ = super::parse_fit(&data);
}
