//! Tests of the whole DXF reader on small synthetic files.

use super::*;
use crate::drawing::FOREGROUND;

/// Builds a text DXF from `(code, value)` groups.
fn dxf(groups: &[(i32, &str)]) -> Vec<u8> {
    let mut s = String::new();
    for (c, v) in groups {
        s.push_str(&format!("{c:3}\n{v}\n"));
    }
    s.into_bytes()
}

/// Wraps entities in an ENTITIES section.
fn entities(body: &[(i32, &str)]) -> Vec<u8> {
    let mut g = vec![(0, "SECTION"), (2, "ENTITIES")];
    g.extend_from_slice(body);
    g.extend_from_slice(&[(0, "ENDSEC"), (0, "EOF")]);
    dxf(&g)
}

fn load_ok(data: &[u8]) -> Drawing {
    load(data, 1_000_000).expect("loads")
}

#[test]
fn sniffing() {
    assert!(sniff(b"  0\nSECTION\n  2\nHEADER\n"));
    assert!(sniff(b"0\r\nSECTION\r\n2\r\nENTITIES\r\n"));
    assert!(sniff(b"999\nmade by someone\n0\nSECTION\n2\nENTITIES\n"));
    assert!(sniff(pairs::BINARY_SENTINEL));
    assert!(!sniff(b"0\nSECTION\n2\nWIDGETS\n"));
    assert!(!sniff(b"{\"json\": true}"));
    assert!(!sniff(b""));
    assert!(!sniff(b"%PDF-1.4\n"));
}

#[test]
fn binary_sentinel_without_a_carriage_return() {
    let mut d = b"AutoCAD Binary DXF\n\x1a\0".to_vec();
    for (code, v) in [(0u16, "SECTION"), (2, "ENTITIES"), (0, "LINE")] {
        d.extend(code.to_le_bytes());
        d.extend(v.as_bytes());
        d.push(0);
    }
    for (code, v) in [(10u16, 0.0f64), (20, 0.0), (11, 4.0), (21, 4.0)] {
        d.extend(code.to_le_bytes());
        d.extend(v.to_le_bytes());
    }
    assert!(sniff(&d));
    assert_eq!(load_ok(&d).polys.len(), 1);
    assert!(!sniff(b"AutoCAD Binary DXF but not quite"));
}

#[test]
fn fragments_without_section_lines() {
    let d = load_ok(b"2\nENTITIES\n0\nLINE\n10\n0\n20\n0\n11\n3\n21\n4\n0\nCIRCLE\n10\n0\n20\n0\n40\n2\n");
    assert_eq!((d.polys.len(), d.arcs.len()), (1, 1));
    assert!(sniff(b"2\nENTITIES\n0\nLINE\n"));
    assert!(!sniff(b"2\nENTITIES\n"));
}

#[test]
fn lines_and_layers() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "TABLES"),
        (0, "TABLE"),
        (2, "LAYER"),
        (0, "LAYER"),
        (2, "Walls"),
        (70, "0"),
        (62, "1"),
        (0, "LAYER"),
        (2, "Hidden"),
        (70, "0"),
        (62, "-3"),
        (0, "LAYER"),
        (2, "Frozen"),
        (70, "1"),
        (62, "5"),
        (0, "ENDTAB"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "LINE"),
        (8, "Walls"),
        (10, "0"),
        (20, "0"),
        (11, "10"),
        (21, "0"),
        (0, "LINE"),
        (8, "Walls"),
        (62, "256"),
        (10, "0"),
        (20, "0"),
        (11, "0"),
        (21, "5"),
        (0, "LINE"),
        (8, "Hidden"),
        (62, "4"),
        (10, "0"),
        (20, "5"),
        (11, "10"),
        (21, "5"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]));
    assert_eq!(d.polys.len(), 3);
    let names: Vec<_> = d.layers.iter().map(|l| (l.name.as_str(), l.visible)).collect();
    assert_eq!(names, [("0", true), ("Walls", true), ("Hidden", false), ("Frozen", false)]);
    // By-layer colour is the layer's red; the explicit one is cyan.
    assert_eq!(d.polys[0].color, 0xFF0000);
    assert_eq!(d.polys[2].color, 0x00FFFF);
    assert!(d.flat);
    let b = d.bounds.unwrap();
    assert_eq!((b.min.x, b.max.x, b.max.y), (0.0, 10.0, 5.0));
    assert!(d.info.format.starts_with("DXF"));
}

#[test]
fn colours_follow_the_rules() {
    let d = load_ok(&entities(&[
        (0, "LINE"),
        (62, "7"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "LINE"),
        (420, "16711680"),
        (10, "0"),
        (20, "1"),
        (11, "1"),
        (21, "1"),
        (0, "LINE"),
        (420, "16777215"),
        (10, "0"),
        (20, "2"),
        (11, "1"),
        (21, "2"),
        (0, "LINE"),
        (62, "3"),
        (10, "0"),
        (20, "3"),
        (11, "1"),
        (21, "3"),
    ]));
    let colors: Vec<u32> = d.polys.iter().map(|p| p.color).collect();
    assert_eq!(colors, [FOREGROUND, 0xFF0000, FOREGROUND, 0x00FF00]);
}

#[test]
fn circles_arcs_and_ellipses_stay_analytic() {
    let d = load_ok(&entities(&[
        (0, "CIRCLE"),
        (10, "5"),
        (20, "5"),
        (40, "2"),
        (0, "ARC"),
        (10, "0"),
        (20, "0"),
        (40, "3"),
        (50, "0"),
        (51, "90"),
        (0, "ELLIPSE"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (11, "4"),
        (21, "0"),
        (31, "0"),
        (40, "0.5"),
        (41, "0"),
        (42, "6.283185307179586"),
    ]));
    assert_eq!(d.arcs.len(), 3);
    let c = &d.arcs[0];
    assert_eq!((c.c.x, c.u.x, c.v.y), (5.0, 2.0, 2.0));
    assert!((c.t1 - c.t0 - std::f64::consts::TAU).abs() < 1e-12);
    let a = &d.arcs[1];
    assert!((a.t1 - a.t0 - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    let e = &d.arcs[2];
    assert_eq!((e.u.x, e.v.y), (4.0, 2.0));
}

#[test]
fn extrusion_mirrors_arcs() {
    // An arc extruded along -Z is a mirror image: its centre's x flips.
    let d = load_ok(&entities(&[
        (0, "ARC"),
        (10, "5"),
        (20, "2"),
        (40, "1"),
        (50, "0"),
        (51, "90"),
        (210, "0"),
        (220, "0"),
        (230, "-1"),
    ]));
    let a = &d.arcs[0];
    assert!((a.c.x + 5.0).abs() < 1e-12 && (a.c.y - 2.0).abs() < 1e-12, "{:?}", a.c);
    // The world point at t0 is (-6, 2): the mirror of (6, 2).
    let p = a.c + a.u * a.t0.cos() + a.v * a.t0.sin();
    assert!((p.x + 4.0).abs() < 1e-9 || (p.x + 6.0).abs() < 1e-9, "{p:?}");
}

#[test]
fn lwpolyline_with_bulge_and_width() {
    let d = load_ok(&entities(&[
        (0, "LWPOLYLINE"),
        (90, "3"),
        (70, "0"),
        (10, "0"),
        (20, "0"),
        (42, "1"),
        (10, "10"),
        (20, "0"),
        (10, "10"),
        (20, "10"),
    ]));
    assert_eq!(d.polys.len(), 1);
    // The semicircle adds points.
    assert!(d.polys[0].len > 10);
    let wide = load_ok(&entities(&[
        (0, "LWPOLYLINE"),
        (90, "2"),
        (70, "0"),
        (43, "0.5"),
        (10, "0"),
        (20, "0"),
        (10, "10"),
        (20, "0"),
    ]));
    assert_eq!(wide.polys.len(), 0);
    assert_eq!(wide.fills.len(), 1);
    // Half a unit wide: the strip spans y from -0.25 to 0.25.
    let ys: Vec<f64> = wide.fill_pts.iter().map(|p| p.y).collect();
    assert!(ys.iter().cloned().fold(f64::MIN, f64::max) > 0.24);
    assert!(ys.iter().cloned().fold(f64::MAX, f64::min) < -0.24);
}

#[test]
fn old_polyline_with_vertices() {
    let d = load_ok(&entities(&[
        (0, "POLYLINE"),
        (66, "1"),
        (70, "1"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (0, "VERTEX"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (0, "VERTEX"),
        (10, "4"),
        (20, "0"),
        (30, "0"),
        (0, "VERTEX"),
        (10, "4"),
        (20, "3"),
        (30, "0"),
        (0, "SEQEND"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
    ]));
    assert_eq!(d.polys.len(), 2);
    // Closed: the first point is repeated.
    assert_eq!(d.polys[0].len, 4);
}

#[test]
fn polyface_meshes_are_3d() {
    let d = load_ok(&entities(&[
        (0, "POLYLINE"),
        (66, "1"),
        (70, "64"),
        (71, "4"),
        (72, "2"),
        (0, "VERTEX"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (70, "192"),
        (0, "VERTEX"),
        (10, "1"),
        (20, "0"),
        (30, "0"),
        (70, "192"),
        (0, "VERTEX"),
        (10, "1"),
        (20, "1"),
        (30, "0"),
        (70, "192"),
        (0, "VERTEX"),
        (10, "0"),
        (20, "0"),
        (30, "1"),
        (70, "192"),
        (0, "VERTEX"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (70, "128"),
        (71, "1"),
        (72, "2"),
        (73, "3"),
        (0, "VERTEX"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (70, "128"),
        (71, "1"),
        (72, "3"),
        (73, "-4"),
        (74, "2"),
        (0, "SEQEND"),
    ]));
    assert_eq!(d.fills.len(), 1);
    assert!(d.fills[0].lit);
    assert_eq!(d.fills[0].len, 9, "a triangle and a quad");
    assert!(!d.flat);
}

#[test]
fn three_d_faces() {
    let d = load_ok(&entities(&[
        (0, "3DFACE"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (11, "1"),
        (21, "0"),
        (31, "0"),
        (12, "1"),
        (22, "1"),
        (32, "1"),
        (13, "0"),
        (23, "1"),
        (33, "1"),
        (0, "3DFACE"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (11, "1"),
        (21, "0"),
        (31, "0"),
        (12, "1"),
        (22, "1"),
        (32, "1"),
    ]));
    assert_eq!(d.fills.len(), 2);
    assert_eq!((d.fills[0].len, d.fills[1].len), (6, 3));
    assert!(!d.flat);
}

#[test]
fn solids_and_traces() {
    let d = load_ok(&entities(&[
        (0, "SOLID"),
        (10, "0"),
        (20, "0"),
        (11, "2"),
        (21, "0"),
        (12, "0"),
        (22, "2"),
        (13, "2"),
        (23, "2"),
        (0, "TRACE"),
        (10, "5"),
        (20, "0"),
        (11, "7"),
        (21, "0"),
        (12, "5"),
        (22, "2"),
    ]));
    assert_eq!(d.fills.len(), 2);
    assert_eq!(d.fills[0].len, 6);
    assert!(d.flat);
}

#[test]
fn hatches_with_islands() {
    let square = |x0: &'static str, x1: &'static str, y0: &'static str, y1: &'static str| {
        vec![
            (92, "7"),
            (72, "0"),
            (73, "1"),
            (93, "4"),
            (10, x0),
            (20, y0),
            (10, x1),
            (20, y0),
            (10, x1),
            (20, y1),
            (10, x0),
            (20, y1),
            (97, "0"),
        ]
    };
    let mut g = vec![(0, "HATCH"), (2, "SOLID"), (70, "1"), (91, "2")];
    g.extend(square("0", "10", "0", "10"));
    g.extend(square("3", "7", "3", "7"));
    g.extend([(75, "0"), (76, "1")]);
    let d = load_ok(&entities(&g));
    assert_eq!(d.fills.len(), 1);
    // Area of the triangles: 100 - 16 = 84.
    let area: f64 =
        d.fill_verts(&d.fills[0]).chunks_exact(3).map(|t| ((t[1] - t[0]).cross(t[2] - t[0]).z / 2.0).abs()).sum();
    assert!((area - 84.0).abs() < 1e-9, "{area}");
    // A pattern is drawn as its boundary.
    let mut g = vec![(0, "HATCH"), (2, "ANSI31"), (70, "0"), (91, "1")];
    g.extend(square("0", "10", "0", "10"));
    g.extend([(75, "0"), (76, "1")]);
    let d = load_ok(&entities(&g));
    assert_eq!((d.fills.len(), d.polys.len()), (0, 1));
}

#[test]
fn hatch_with_edge_loop() {
    // A half disc: an arc and a line.
    let d = load_ok(&entities(&[
        (0, "HATCH"),
        (2, "SOLID"),
        (70, "1"),
        (91, "1"),
        (92, "1"),
        (93, "2"),
        (72, "2"),
        (10, "0"),
        (20, "0"),
        (40, "5"),
        (50, "0"),
        (51, "180"),
        (73, "1"),
        (72, "1"),
        (10, "-5"),
        (20, "0"),
        (11, "5"),
        (21, "0"),
        (97, "0"),
        (75, "0"),
        (76, "1"),
    ]));
    assert_eq!(d.fills.len(), 1);
    let area: f64 =
        d.fill_verts(&d.fills[0]).chunks_exact(3).map(|t| ((t[1] - t[0]).cross(t[2] - t[0]).z / 2.0).abs()).sum();
    assert!((area - std::f64::consts::PI * 25.0 / 2.0).abs() < 0.3, "{area}");
}

#[test]
fn splines_are_flattened() {
    let d = load_ok(&entities(&[
        (0, "SPLINE"),
        (70, "8"),
        (71, "2"),
        (73, "3"),
        (40, "0"),
        (40, "0"),
        (40, "0"),
        (40, "1"),
        (40, "1"),
        (40, "1"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (10, "5"),
        (20, "10"),
        (30, "0"),
        (10, "10"),
        (20, "0"),
        (30, "0"),
    ]));
    assert_eq!(d.polys.len(), 1);
    assert!(d.polys[0].len > 20);
    assert!((d.polys[0].max[1] - 5.0).abs() < 0.01, "{:?}", d.polys[0].max);
}

#[test]
fn text_becomes_strokes() {
    let d = load_ok(&entities(&[
        (0, "TEXT"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (40, "2"),
        (1, "Hi"),
        (0, "MTEXT"),
        (10, "0"),
        (20, "10"),
        (30, "0"),
        (40, "1"),
        (71, "1"),
        (1, "{\\fArial|b1;Two}\\Plines"),
    ]));
    assert!(d.polys.len() >= 8, "{}", d.polys.len());
    let b = d.bounds.unwrap();
    assert!(b.max.y > 10.0 - 0.1 && b.min.y < 9.0 - 1.0);
    assert!(d.flat);
}

#[test]
fn block_references_nest_and_inherit() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "TABLES"),
        (0, "TABLE"),
        (2, "LAYER"),
        (0, "LAYER"),
        (2, "Furniture"),
        (62, "1"),
        (0, "ENDTAB"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "BLOCKS"),
        // A unit square on layer 0 with colour by block, and a line by layer.
        (0, "BLOCK"),
        (2, "SQ"),
        (10, "1"),
        (20, "1"),
        (30, "0"),
        (0, "LINE"),
        (8, "0"),
        (62, "0"),
        (10, "1"),
        (20, "1"),
        (11, "2"),
        (21, "1"),
        (0, "LINE"),
        (8, "Other"),
        (10, "1"),
        (20, "1"),
        (11, "1"),
        (21, "2"),
        (0, "ENDBLK"),
        // Two squares in one.
        (0, "BLOCK"),
        (2, "PAIR"),
        (0, "INSERT"),
        (2, "SQ"),
        (10, "0"),
        (20, "0"),
        (0, "INSERT"),
        (2, "sq"),
        (10, "5"),
        (20, "0"),
        (41, "2"),
        (42, "2"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (8, "Furniture"),
        (2, "PAIR"),
        (10, "100"),
        (20, "100"),
        (50, "90"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]));
    assert_eq!(d.polys.len(), 4);
    let furniture = d.layers.iter().position(|l| l.name == "Furniture").unwrap() as u32;
    let other = d.layers.iter().position(|l| l.name == "Other").unwrap() as u32;
    // The layer-0 lines took the layer of the reference, and its colour
    // (red, by block); the other lines keep their layer.
    let on_furniture: Vec<_> = d.polys.iter().filter(|p| p.layer == furniture).collect();
    let on_other: Vec<_> = d.polys.iter().filter(|p| p.layer == other).collect();
    assert_eq!((on_furniture.len(), on_other.len()), (2, 2));
    assert!(on_furniture.iter().all(|p| p.color == 0xFF0000));
    // Hiding the reference's layer hides everything inside it.
    assert!(d.polys.iter().all(|p| p.owner == furniture));
    // Rotated 90 degrees about (100,100): the first square's base point
    // (1,1) lands on the insertion point.
    let first = d.poly_pts(&d.polys[0]);
    assert!((first[0].x - 100.0).abs() < 1e-9 && (first[0].y - 100.0).abs() < 1e-9, "{:?}", first[0]);
    assert!((first[1].x - 100.0).abs() < 1e-9 && (first[1].y - 101.0).abs() < 1e-9, "{:?}", first[1]);
    // The second square is twice the size, rotated: from (100,110) going
    // left... its first line runs 2 units along -x... checked by length.
    let second = d.poly_pts(&d.polys[2]);
    assert!((second[0].distance(second[1]) - 2.0).abs() < 1e-9);
}

#[test]
fn minsert_arrays() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "DOT"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (2, "DOT"),
        (10, "0"),
        (20, "0"),
        (70, "3"),
        (71, "2"),
        (44, "10"),
        (45, "20"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]));
    assert_eq!(d.polys.len(), 6);
    let starts: Vec<(f64, f64)> = d.polys.iter().map(|p| (d.poly_pts(p)[0].x, d.poly_pts(p)[0].y)).collect();
    assert!(starts.contains(&(20.0, 20.0)) && starts.contains(&(0.0, 0.0)));
}

#[test]
fn attributes_are_drawn_in_the_space_of_the_reference() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "TAG"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "ATTDEF"),
        (10, "0"),
        (20, "1"),
        (40, "1"),
        (1, "default"),
        (2, "NAME"),
        (70, "0"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (2, "TAG"),
        (10, "50"),
        (20, "50"),
        (66, "1"),
        (0, "ATTRIB"),
        (10, "50"),
        (20, "51"),
        (40, "1"),
        (1, "Bob"),
        (2, "NAME"),
        (70, "0"),
        (0, "ATTRIB"),
        (10, "50"),
        (20, "55"),
        (40, "1"),
        (1, "secret"),
        (2, "SECRET"),
        (70, "1"),
        (0, "SEQEND"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]));
    // The line, plus the strokes of "Bob" (and neither the definition nor
    // the invisible attribute).
    let b = d.bounds.unwrap();
    assert!(b.max.y < 53.0 && b.max.y > 51.5, "{b:?}");
    assert!(d.polys.len() > 3);
}

#[test]
fn dimensions_draw_their_block() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "*D1"),
        (70, "1"),
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (0, "LINE"),
        (10, "0"),
        (20, "5"),
        (11, "10"),
        (21, "5"),
        (0, "SOLID"),
        (10, "0"),
        (20, "5"),
        (11, "1"),
        (21, "5.2"),
        (12, "1"),
        (22, "4.8"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "DIMENSION"),
        (2, "*D1"),
        (70, "0"),
        (10, "5"),
        (20, "5"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]));
    assert_eq!((d.polys.len(), d.fills.len()), (1, 1));
}

#[test]
fn rays_are_clipped_to_the_drawing() {
    let mut g = vec![];
    for i in 0..30 {
        g.extend([(0, "LINE"), (10, "0"), (20, "0"), (11, "10"), (21, "10")]);
        let _ = i;
    }
    g.extend([(0, "XLINE"), (10, "5"), (20, "5"), (11, "1"), (21, "0")]);
    g.extend([(0, "RAY"), (10, "5"), (20, "0"), (11, "0"), (21, "1")]);
    let d = load_ok(&entities(&g));
    let rays: Vec<_> = d.polys.iter().skip(30).collect();
    assert_eq!(rays.len(), 2);
    // The infinite line runs a quarter beyond each side.
    assert!((rays[0].min[0] + 2.5).abs() < 1e-9 && (rays[0].max[0] - 12.5).abs() < 1e-9, "{:?}", rays[0]);
    // The ray starts at its origin and runs up.
    assert!(rays[1].min[1].abs() < 1e-9 && rays[1].max[1] > 12.0);
}

#[test]
fn unsupported_entities_get_one_warning() {
    let d = load_ok(&entities(&[
        (0, "3DSOLID"),
        (8, "0"),
        (1, "ACIS data"),
        (0, "REGION"),
        (8, "0"),
        (0, "IMAGE"),
        (8, "0"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
    ]));
    let notes: Vec<_> = d.info.warnings.iter().filter(|w| w.starts_with("Not drawn")).collect();
    assert_eq!(notes.len(), 1, "{:?}", d.info.warnings);
    assert!(notes[0].contains("3DSOLID") && notes[0].contains("REGION") && notes[0].contains("IMAGE"), "{}", notes[0]);
}

#[test]
fn paper_space_is_skipped() {
    let d = load_ok(&entities(&[
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
        (0, "LINE"),
        (67, "1"),
        (10, "0"),
        (20, "0"),
        (11, "100"),
        (21, "100"),
    ]));
    assert_eq!(d.polys.len(), 1);
}

#[test]
fn stray_far_away_entities_do_not_set_the_framing() {
    let mut g = vec![];
    let xs: Vec<String> = (0..50).map(|i| (i * 2).to_string()).collect();
    for x in &xs {
        g.extend([(0, "LINE"), (10, x.as_str()), (20, "0"), (11, x.as_str()), (21, "5")]);
    }
    g.extend([(0, "LINE"), (10, "1000000"), (20, "1000000"), (11, "1000001"), (21, "1000001")]);
    let d = load_ok(&entities(&g));
    assert_eq!(d.outliers, 1);
    assert!(d.bounds.unwrap().max.x < 100.0);
    assert!(d.info.warnings.iter().any(|w| w.contains("far from the rest")));
}

#[test]
fn old_files_use_their_code_page() {
    let mut data = dxf(&[
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$ACADVER"),
        (1, "AC1009"),
        (9, "$DWGCODEPAGE"),
        (3, "ANSI_1251"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "TABLES"),
        (0, "TABLE"),
        (2, "LAYER"),
    ]);
    data.extend(b"  0\nLAYER\n  2\n");
    data.extend([0xD1, 0xF2, 0xE5, 0xED, 0xFB]); // Стены in cp1251... "Стены"
    data.extend(b"\n 62\n1\n  0\nENDTAB\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n  0\nLINE\n  8\n");
    data.extend([0xD1, 0xF2, 0xE5, 0xED, 0xFB]);
    data.extend(b"\n 10\n0\n 20\n0\n 11\n1\n 21\n1\n  0\nENDSEC\n  0\nEOF\n");
    let d = load_ok(&data);
    assert_eq!(d.layers[1].name, "Стены");
    assert_eq!(d.polys[0].layer, 1);
    assert_eq!(d.polys[0].color, 0xFF0000);
}

#[test]
fn binary_files_read_like_text() {
    let mut d = pairs::BINARY_SENTINEL.to_vec();
    let s = |d: &mut Vec<u8>, code: u16, v: &str| {
        d.extend(code.to_le_bytes());
        d.extend(v.as_bytes());
        d.push(0);
    };
    let f = |d: &mut Vec<u8>, code: u16, v: f64| {
        d.extend(code.to_le_bytes());
        d.extend(v.to_le_bytes());
    };
    s(&mut d, 0, "SECTION");
    s(&mut d, 2, "ENTITIES");
    s(&mut d, 0, "LINE");
    s(&mut d, 8, "0");
    f(&mut d, 10, 1.0);
    f(&mut d, 20, 2.0);
    f(&mut d, 11, 3.0);
    f(&mut d, 21, 4.0);
    d.extend(62u16.to_le_bytes());
    d.extend(3i16.to_le_bytes());
    s(&mut d, 0, "ENDSEC");
    s(&mut d, 0, "EOF");
    let drawing = load_ok(&d);
    assert!(drawing.info.format.contains("binary"));
    assert_eq!(drawing.polys.len(), 1);
    assert_eq!(drawing.polys[0].color, 0x00FF00);
    assert_eq!(drawing.bounds.unwrap().max.x, 3.0);
}

#[test]
fn missing_terminators_and_sections_still_load() {
    // No ENDSEC, no EOF.
    let d = load_ok(b"0\nSECTION\n2\nENTITIES\n0\nLINE\n10\n0\n20\n0\n11\n5\n21\n5\n");
    assert_eq!(d.polys.len(), 1);
    // Blocks without ENDBLK.
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "A"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "BLOCK"),
        (2, "B"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "0"),
        (21, "1"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (2, "A"),
        (0, "INSERT"),
        (2, "B"),
        (0, "ENDSEC"),
    ]));
    assert_eq!(d.polys.len(), 2);
}

#[test]
fn blocks_listed_after_the_entities_work() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (2, "LATE"),
        (10, "0"),
        (20, "0"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "LATE"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
    ]));
    assert_eq!(d.polys.len(), 1);
}

//region Damaged files

#[test]
fn self_referencing_blocks_stop() {
    let d = load_ok(&dxf(&[
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "LOOP"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "0"),
        (0, "INSERT"),
        (2, "LOOP"),
        (10, "1"),
        (0, "ENDBLK"),
        (0, "BLOCK"),
        (2, "PING"),
        (0, "INSERT"),
        (2, "PONG"),
        (0, "ENDBLK"),
        (0, "BLOCK"),
        (2, "PONG"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
        (0, "INSERT"),
        (2, "PING"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "INSERT"),
        (2, "LOOP"),
        (0, "INSERT"),
        (2, "PING"),
        (0, "ENDSEC"),
    ]));
    assert_eq!(d.polys.len(), 2);
    assert!(d.info.warnings.iter().any(|w| w.contains("looping")), "{:?}", d.info.warnings);
}

#[test]
fn exponential_nesting_is_bounded_by_the_budget() {
    // Each block holds two references to the one below: 2^30 lines if
    // anything expanded them blindly.
    let mut g: Vec<(i32, String)> = vec![(0, "SECTION".into()), (2, "BLOCKS".into())];
    g.extend([
        (0, "BLOCK".into()),
        (2, "B0".into()),
        (0, "LINE".into()),
        (10, "0".into()),
        (20, "0".into()),
        (11, "1".into()),
        (21, "1".into()),
        (0, "ENDBLK".into()),
    ]);
    for i in 1..24 {
        g.extend([
            (0, "BLOCK".into()),
            (2, format!("B{i}")),
            (0, "INSERT".into()),
            (2, format!("B{}", i - 1)),
            (0, "INSERT".into()),
            (2, format!("B{}", i - 1)),
            (0, "ENDBLK".into()),
        ]);
    }
    g.extend([
        (0, "ENDSEC".into()),
        (0, "SECTION".into()),
        (2, "ENTITIES".into()),
        (0, "INSERT".into()),
        (2, "B23".into()),
        (0, "ENDSEC".into()),
    ]);
    let groups: Vec<(i32, &str)> = g.iter().map(|(c, v)| (*c, v.as_str())).collect();
    let start = std::time::Instant::now();
    let d = load(&dxf(&groups), 100_000).expect("loads");
    assert!(start.elapsed().as_secs() < 5);
    assert!(d.polys.len() <= 100_000 / 2 + 1);
    assert!(d.info.warnings.iter().any(|w| w.contains("too large")));
}

#[test]
fn huge_arrays_and_counts_are_capped() {
    let d = load(
        &dxf(&[
            (0, "SECTION"),
            (2, "BLOCKS"),
            (0, "BLOCK"),
            (2, "A"),
            (0, "LINE"),
            (10, "0"),
            (20, "0"),
            (11, "1"),
            (21, "0"),
            (0, "ENDBLK"),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "INSERT"),
            (2, "A"),
            (70, "30000"),
            (71, "30000"),
            (44, "2"),
            (45, "2"),
            (0, "LWPOLYLINE"),
            (90, "2000000000"),
            (10, "0"),
            (20, "0"),
            (10, "1"),
            (20, "1"),
            (0, "HATCH"),
            (91, "2000000000"),
            (92, "1"),
            (93, "2000000000"),
            (0, "SPLINE"),
            (71, "99999"),
            (73, "2000000000"),
            (10, "0"),
            (20, "0"),
            (10, "1"),
            (20, "1"),
            (0, "ENDSEC"),
        ]),
        50_000,
    )
    .expect("loads");
    assert!(d.pts.len() <= 50_000);
}

#[test]
fn absurd_numbers_are_dropped() {
    let d = load_ok(&entities(&[
        (0, "LINE"),
        (10, "1e300"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
        (0, "LINE"),
        (10, "nan"),
        (20, "inf"),
        (11, "1"),
        (21, "1"),
        (0, "CIRCLE"),
        (10, "0"),
        (20, "0"),
        (40, "1e308"),
        (0, "LINE"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
    ]));
    // NaN and infinity read as 0, so the second line is the diagonal.
    assert_eq!(d.polys.len(), 2);
}

#[test]
fn truncated_and_mutated_files_never_panic() {
    let sample = entities(&[
        (0, "LWPOLYLINE"),
        (90, "3"),
        (10, "0"),
        (20, "0"),
        (42, "0.5"),
        (10, "10"),
        (20, "0"),
        (10, "10"),
        (20, "10"),
        (0, "HATCH"),
        (2, "SOLID"),
        (70, "1"),
        (91, "1"),
        (92, "7"),
        (72, "0"),
        (73, "1"),
        (93, "3"),
        (10, "0"),
        (20, "0"),
        (10, "4"),
        (20, "0"),
        (10, "4"),
        (20, "4"),
        (97, "0"),
        (0, "TEXT"),
        (10, "0"),
        (20, "0"),
        (40, "1"),
        (1, "text"),
        (0, "SPLINE"),
        (71, "2"),
        (73, "3"),
        (10, "0"),
        (20, "0"),
        (10, "1"),
        (20, "1"),
        (10, "2"),
        (20, "0"),
        (0, "POLYLINE"),
        (70, "64"),
        (71, "3"),
        (72, "1"),
        (0, "VERTEX"),
        (70, "192"),
        (0, "VERTEX"),
        (70, "128"),
        (71, "1"),
        (72, "2"),
        (73, "3"),
        (0, "SEQEND"),
        (0, "INSERT"),
        (2, "NOPE"),
    ]);
    for n in 0..sample.len() {
        let _ = load(&sample[..n], 100_000);
    }
    // Flip bytes pseudo-randomly.
    let mut x: u64 = 0x1234_5678_9abc_def0;
    for _ in 0..2000 {
        let mut data = sample.clone();
        for _ in 0..4 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let i = (x >> 33) as usize % data.len();
            data[i] = (x >> 20) as u8;
        }
        let _ = load(&data, 100_000);
    }
}

//endregion
