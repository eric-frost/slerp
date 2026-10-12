//! G-code tests: the interpreter's modes and arcs, layers and features,
//! metadata, embedded pictures, binary G-code, the meshes, and damaged
//! files.

use super::*;
use crate::text::Lines;
use std::path::{Path, PathBuf};

/// Parses `text` as if a first move had put the tool at the origin: files
/// begin by positioning the tool, and a position that is unknown starts no
/// segment (see `unknown_start_begins_nowhere`).
fn parse(text: &str) -> Toolpath {
    parse_raw(&format!("G0 X0 Y0 Z0\n{text}"))
}

/// Parses `text` as it is.
fn parse_raw(text: &str) -> Toolpath {
    let mut p = Parser::new(Keep::All, 10_000_000);
    let mut lines = Lines::new(1 << 16);
    let mut f = |l: &[u8]| {
        p.line(l);
        true
    };
    lines.push(text.as_bytes(), &mut f);
    lines.finish(&mut f);
    p.finish()
}

fn asset(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(rel);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// The points reached by moves of `kind`.
fn ends(tp: &Toolpath, kind: MoveKind) -> Vec<[f32; 3]> {
    tp.points.iter().zip(&tp.moves).filter(|(_, m)| m.kind == kind).map(|(p, _)| *p).collect()
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
}

/*┌─────────────────╮
| Interpreter     |
└─────────────────╯*/
//region Interpreter

#[test]
fn absolute_and_relative_extrusion() {
    let tp = parse(
        "G90\nM82\nG1 X10 E1\nG1 X20 E1\nM83\nG1 X30 E0.5\nG1 X35 E-0.5\nG92 E0\nM82\nG1 X40 E0.5\nG1 E0.2\nG1 X45 E0.6\n",
    );
    let kinds: Vec<MoveKind> = tp.moves.iter().map(|m| m.kind).collect();
    // The jump to the start, extrusion, a move with E unchanged, a
    // relative extrusion, a wipe (moving retraction), extrusion after G92,
    // the retraction alone moves nothing, then extrusion again.
    assert_eq!(
        kinds,
        [
            MoveKind::Jump,
            MoveKind::Extrude,
            MoveKind::Feed,
            MoveKind::Extrude,
            MoveKind::Feed,
            MoveKind::Extrude,
            MoveKind::Extrude
        ]
    );
    assert_eq!(tp.points.last().copied(), Some([45.0, 0.0, 0.0]));
    assert!(!tp.cnc);
    // 10 + 10 + 5 + 5 mm extruded; the X20 and X35 moves are travel.
    assert!((tp.feature_length.iter().sum::<f64>() - 30.0).abs() < 1e-9);
    assert!((tp.travel_length - 15.0).abs() < 1e-9);
}

#[test]
fn relative_positioning_inches_and_homing() {
    let tp = parse("G91\nG1 X5 Y5 E1\nG1 X5 E1\nG90\nG20\nG1 X1 Y1 E3\nG28 X\nG21\nG1 X1 E100\n");
    let e = ends(&tp, MoveKind::Extrude);
    assert!(close(e[0], [5.0, 5.0, 0.0]));
    assert!(close(e[1], [10.0, 5.0, 0.0]));
    assert!(close(e[2], [25.4, 25.4, 0.0]));
    // Homing X restarts the path at X0, keeping Y.
    assert!(close(e[3], [1.0, 25.4, 0.0]));
    let jumps = ends(&tp, MoveKind::Jump);
    assert!(jumps.iter().any(|p| close(*p, [0.0, 25.4, 0.0])));
}

#[test]
fn arcs_in_both_forms() {
    let radius_to = |pts: &[[f32; 3]], c: [f32; 2]| -> Vec<f32> {
        pts.iter().map(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt()).collect()
    };
    // Clockwise half circle from (0,0) to (10,0) round (5,0): over the top.
    let tp = parse("G1 X0 Y0\nG2 X10 Y0 I5 J0 E1\n");
    let arc = ends(&tp, MoveKind::Extrude);
    assert!(arc.len() > 10);
    assert!(radius_to(&arc, [5.0, 0.0]).iter().all(|r| (r - 5.0).abs() < 0.02));
    assert!(arc.iter().map(|p| p[1]).fold(f32::MIN, f32::max) > 4.99);
    assert!(close(*arc.last().unwrap(), [10.0, 0.0, 0.0]));
    // Anticlockwise goes under.
    let tp = parse("G1 X0 Y0\nG3 X10 Y0 I5 J0 E1\n");
    assert!(ends(&tp, MoveKind::Extrude).iter().map(|p| p[1]).fold(f32::MAX, f32::min) < -4.99);
    // Radius form: positive R takes the short way (centre (5,0)),
    // negative the long way (centre (0,5)).
    let tp = parse("G1 X0 Y0\nG2 X5 Y5 R5 E1\n");
    let arc = ends(&tp, MoveKind::Extrude);
    assert!(radius_to(&arc, [5.0, 0.0]).iter().all(|r| (r - 5.0).abs() < 0.02));
    let tp = parse("G1 X0 Y0\nG2 X5 Y5 R-5 E1\n");
    let arc = ends(&tp, MoveKind::Extrude);
    assert!(radius_to(&arc, [0.0, 5.0]).iter().all(|r| (r - 5.0).abs() < 0.02));
    // A full circle, as a helix rising 2 mm.
    let tp = parse("G1 X0 Y0 Z0\nG2 X0 Y0 Z2 I5 J0 E1\n");
    let arc = ends(&tp, MoveKind::Extrude);
    assert!(arc.len() > 30);
    assert!(close(*arc.last().unwrap(), [0.0, 0.0, 2.0]));
    assert!(arc.iter().any(|p| (p[0] - 10.0).abs() < 0.05));
    let mid = arc[arc.len() / 2];
    assert!((mid[2] - 1.0).abs() < 0.1);
    // Absolute centres (G90.1), and the XZ plane (G18).
    let tp = parse("G1 X2 Y0\nG90.1\nG2 X12 Y0 I7 J0 E1\n");
    assert!(radius_to(&ends(&tp, MoveKind::Extrude), [7.0, 0.0]).iter().all(|r| (r - 5.0).abs() < 0.02));
    let tp = parse("G18\nG1 X0 Z0\nG2 X10 Z0 I5 K0 E1\n");
    let arc = ends(&tp, MoveKind::Extrude);
    assert!(arc.iter().all(|p| p[1] == 0.0));
    assert!(arc.iter().map(|p| p[2].abs()).fold(0.0, f32::max) > 4.99);
    // Nonsense arcs become straight moves or nothing, never a panic.
    let tp = parse("G1 X0 Y0\nG2 X10 Y0 R1 E1\nG2 X20 Y0 I1e300 E2\nG3 X30 I0 J0 E3\nG2 X0 Y0 R0 E4\n");
    assert!(tp.points.len() < 10_000);
}

#[test]
fn layers_from_comments_and_heights() {
    let text = "\
G1 Z0.2\nG1 X1 E1 ; purge\n;LAYER_CHANGE\n;Z:0.2\n;HEIGHT:0.2\n;TYPE:External perimeter\n;WIDTH:0.45\n\
G1 X10 E2\nG1 Z0.6\nG1 Z0.2\nG1 X0 E3\n;LAYER_CHANGE\n;Z:0.4\n;HEIGHT:0.2\nG1 Z0.4\n;TYPE:Internal infill\n\
G1 X10 E4\n;LAYER_CHANGE\n;Z:0.6\nG1 Z0.6\nG1 X0 E5\n";
    let tp = parse(text);
    assert_eq!(tp.layers.len(), 3, "{:?}", tp.layers);
    assert_eq!(tp.layers[0].start, 0);
    assert!((tp.layers[0].z - 0.2).abs() < 1e-6);
    assert!((tp.layers[2].z - 0.6).abs() < 1e-6);
    assert!(tp.layers.iter().all(|l| (l.height - 0.2).abs() < 1e-5));
    assert!(tp.has_feature(Feature::OuterWall) && tp.has_feature(Feature::SparseInfill));
    let m = tp.moves.iter().find(|m| m.feature == Feature::OuterWall).unwrap();
    assert_eq!(m.width, 450);
    // Every point belongs to exactly one layer.
    let total: usize = (0..tp.layers.len()).map(|i| tp.layer_range(i).len()).sum();
    assert_eq!(total, tp.points.len());
}

#[test]
fn layers_from_heights_alone() {
    // Z hops while travelling don't make layers; printing higher does.
    let tp = parse("G1 Z0.3\nG1 X10 E1\nG1 Z1.0\nG1 X0\nG1 Z0.3\nG1 Y10 E2\nG1 Z0.5\nG1 X10 E3\nG1 Z0.7\nG1 Y0 E4\n");
    assert_eq!(tp.layers.len(), 3);
    assert!((tp.layers[0].height - 0.3).abs() < 1e-5);
    assert!((tp.layers[1].height - 0.2).abs() < 1e-5);
}

#[test]
fn slicer_dialects() {
    // Cura.
    let tp = parse(
        ";FLAVOR:Griffin\n;TIME:3723\n;Filament used: 1.5m\n;Generated with Cura_SteamEngine 5.4.0\n;LAYER:0\n;TYPE:WALL-OUTER\nG1 X1 E1\n;TYPE:SKIN\nG1 X2 E2\n;LAYER:1\nG1 Z0.4\n;TYPE:WALL-INNER\nG1 X3 E3\n",
    );
    assert_eq!(tp.meta.generator.as_deref(), Some("Cura 5.4.0"));
    assert_eq!(tp.meta.print_time, Some(3723.0));
    assert_eq!(tp.meta.filament_mm, Some(1500.0));
    assert_eq!(tp.filament_diameter(), 2.85);
    assert!(tp.has_feature(Feature::OuterWall) && tp.has_feature(Feature::SolidInfill));
    assert!(tp.has_feature(Feature::InnerWall));
    // Bambu Studio.
    let tp = parse(
        "; HEADER_BLOCK_START\n; BambuStudio 01.09.00.70\n; model printing time: 50m 19s; total estimated time: 56m 41s\n; total filament length [mm] : 1234.5\n; total filament weight [g] : 3.6\n; CHANGE_LAYER\n; Z_HEIGHT: 0.2\n; LAYER_HEIGHT: 0.2\n; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\nG1 Z0.2\nG1 X5 E1\n; CHANGE_LAYER\n; LAYER_HEIGHT: 0.2\nG1 Z0.4\n; FEATURE: Sparse infill\nG1 X0 E2\n",
    );
    assert_eq!(tp.meta.generator.as_deref(), Some("BambuStudio 01.09.00.70"));
    assert_eq!(tp.meta.print_time, Some(3401.0));
    assert_eq!(tp.meta.filament_mm, Some(1234.5));
    assert_eq!(tp.meta.filament_g, Some(3.6));
    assert_eq!(tp.layers.len(), 2);
    assert_eq!(tp.moves.iter().find(|m| m.kind == MoveKind::Extrude).unwrap().width, 420);
    // PrusaSlicer, with its settings at the end of the file.
    let tp = parse(
        "; generated by PrusaSlicer 2.9.2 on 2025-05-24 at 15:21:40 UTC\nG1 X1 E1\n; filament used [mm] = 100.5, 20.5\n; filament used [g] = 0.3\n; estimated printing time (normal mode) = 1h 2m 3s\n; filament_type = PETG;PETG\n; printer_model = MK4S\n; nozzle_diameter = 0.6,0.6\n; filament_diameter = 1.75\n; layer_height = 0.15\n",
    );
    assert_eq!(tp.meta.generator.as_deref(), Some("PrusaSlicer 2.9.2"));
    assert_eq!(tp.meta.filament_mm, Some(121.0));
    assert_eq!(tp.meta.print_time, Some(3723.0));
    assert_eq!(tp.meta.material.as_deref(), Some("PETG"));
    assert_eq!(tp.meta.printer.as_deref(), Some("MK4S"));
    assert_eq!(tp.meta.nozzle, Some(0.6));
    // Simplify3D.
    let tp = parse(
        "; G-Code generated by Simplify3D(R) Version 4.1.2\n; layer 1, Z = 0.3\n; feature outer perimeter\nG1 Z0.3\nG1 X5 E1\n; layer 2, Z = 0.5\n; feature solid layer\nG1 Z0.5\nG1 X0 E2\n",
    );
    assert_eq!(tp.meta.generator.as_deref(), Some("Simplify3D 4.1.2"));
    assert_eq!(tp.layers.len(), 2);
    assert!(tp.has_feature(Feature::OuterWall) && tp.has_feature(Feature::SolidInfill));
}

#[test]
fn width_from_extrusion() {
    // 0.45 mm wide, 0.2 mm high: area (0.45 - 0.2) 0.2 + π 0.01 = 0.0814;
    // over 10 mm that is 0.814 mm³, or 0.3384 mm of 1.75 mm filament.
    let tp = parse(";LAYER_CHANGE\n;HEIGHT:0.2\nG1 Z0.2\nG1 X10 E0.3384\n");
    let m = tp.moves.iter().find(|m| m.kind == MoveKind::Extrude).unwrap();
    assert_eq!(m.width, 0);
    let w = tp.width_of(m, 0.2);
    assert!((w - 0.45).abs() < 0.01, "{w}");
}

#[test]
fn cnc_lasers_and_odd_syntax() {
    // A mill: no extrusion, so feed moves are cuts, and modal moves
    // without a G work.
    let tp = parse("%\nO1000\n(Facing)\nG21 G90 G17\nG0 X0 Y0 Z5\nG1 Z-1 F100\nX10\nY10\nX0 (back)\nM30\n%\n");
    assert!(tp.cnc);
    assert_eq!(ends(&tp, MoveKind::Feed).len(), 4);
    // The plunge counts as a cut: 6 + 10 + 10 + 10 mm.
    assert!((tp.feature_length[Feature::Cut.index()] - 36.0).abs() < 1e-6);
    // A laser's S0 moves are travel.
    let tp = parse("M4 S0\nG0 X0 Y0\nG1 X10 S1000\nG1 X20 S0\nG1 X30 S800\nM5\n");
    assert!(tp.cnc);
    assert!((tp.feature_length[Feature::Cut.index()] - 20.0).abs() < 1e-6);
    // No spaces (MeatPack), line numbers, checksums, firmware retraction,
    // macros and messages.
    let tp = parse(
        "N1 G1X10Y5E.4*93\nG10\nG1X20E.8\nG11\nG10 L2 P1 X50\nEXCLUDE_OBJECT_START NAME=cube_1\nM117 X99 Y99\nT1\nG1 Y10 E1.2\n",
    );
    assert!(!tp.cnc);
    let e = ends(&tp, MoveKind::Extrude);
    assert!(close(e[0], [10.0, 5.0, 0.0]));
    assert!(close(e[1], [20.0, 5.0, 0.0]));
    assert!(close(e[2], [20.0, 10.0, 0.0]));
    assert_eq!(tp.tool_changes, 1);
    assert_eq!(tp.moves.last().unwrap().tool, 1);
}

#[test]
fn caps_what_it_keeps() {
    let mut text = String::new();
    for i in 0..5000 {
        text.push_str(&format!("G1 X{} Y{} E{}\n", i % 100, i / 100, i));
    }
    let mut p = Parser::new(Keep::All, 1000);
    for l in text.lines() {
        p.line(l.as_bytes());
    }
    let tp = p.finish();
    assert!(tp.points.len() <= 1000);
    assert!(tp.warnings.iter().any(|w| w.contains("first")));
    // Thumbnails don't keep travel.
    let mut p = Parser::new(Keep::Drawn, 1000);
    for l in "G0 X5\nG1 X10 E1\nG0 X0 Y5\nG1 X10 E2\n".lines() {
        p.line(l.as_bytes());
    }
    let tp = p.finish();
    assert!(tp.moves.iter().all(|m| m.kind != MoveKind::Travel));
    assert_eq!(tp.moves.iter().filter(|m| m.kind == MoveKind::Jump).count(), 2);
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, 100, 200, 255]).collect();
    enc.write_header().unwrap().write_image_data(&data).unwrap();
    out
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn scan(text: &str) -> Vec<Embedded> {
    let mut s = Scanner::new(true);
    let mut lines = Lines::new(1 << 24);
    lines.push(text.as_bytes(), &mut |l| s.line(l));
    s.flush();
    s.found
}

#[test]
fn finds_every_slicers_pictures() {
    let small = base64(&png_bytes(4, 3));
    let big = base64(&png_bytes(40, 30));
    let lines = |b64: &str| {
        b64.as_bytes().chunks(78).map(|c| format!("; {}\n", std::str::from_utf8(c).unwrap())).collect::<String>()
    };
    // PrusaSlicer and Bambu's layout, two sizes; the largest wins.
    let text = format!(
        "; THUMBNAIL_BLOCK_START\n;\n; thumbnail begin 4x3 {}\n{}; thumbnail end\n;\n; thumbnail_PNG begin 40x30 {}\n{}; thumbnail_PNG end\n; THUMBNAIL_BLOCK_END\nG28\nG1 X1\n; thumbnail begin 9x9 1\n; AAAA\n; thumbnail end\n",
        small.len(),
        lines(&small),
        big.len(),
        lines(&big)
    );
    let found = scan(&text);
    assert_eq!(found.len(), 2, "pictures after the first move aren't read");
    let t = best(found).unwrap();
    assert_eq!((t.width(), t.height()), (40, 30));
    // Creality's `; png begin`, Luban's data URL, Weedo's hex lines.
    let found = scan(&format!("; png begin 4*3 {} 4 100\n{}; png end\n", small.len(), lines(&small)));
    assert_eq!(best(found).map(|t| t.width()), Some(4));
    let found = scan(&format!(";thumbnail: data:image/png;base64,{big}\n"));
    assert_eq!(best(found).map(|t| t.width()), Some(40));
    let hex: String = png_bytes(5, 5).iter().map(|b| format!("{b:02x}")).collect();
    let found = scan(&format!("W221\nW220 {}\nW220 {}\nW222\n", &hex[..20], &hex[20..]));
    assert_eq!(best(found).map(|t| t.width()), Some(5));
    // MKS: RGB565, low byte first, 100 pixels square.
    let mut mks = String::from(";simage:");
    let red = "00f8".repeat(100 * 100);
    mks.push_str(&red[..400]);
    mks.push('\n');
    for chunk in red.as_bytes()[400..].chunks(400) {
        mks.push_str(&format!("M10086 ;{}\n", std::str::from_utf8(chunk).unwrap()));
    }
    mks.push_str("M10086 ;\n");
    let t = best(scan(&mks)).unwrap();
    assert_eq!((t.width(), t.height()), (100, 100));
    let slerp_scene::textures::Pixels::Rgba8(px) = t.base() else { panic!() };
    assert_eq!(&px[..4], &[255, 0, 0, 255]);
    // A block cut off by a command, and garbage, give nothing.
    assert!(best(scan(&format!("; thumbnail begin 4x3 9\n{}G1 X1\n", &lines(&small)[..20]))).is_none());
    assert!(best(scan("; thumbnail begin 4x3 9\n; !!!!\n; thumbnail end\n")).is_none());
}

#[test]
fn reads_the_samples_pictures() {
    let Some(benchy) = asset("paths-extra/gcode-preview/3DBenchy.gcode") else { return };
    let found = embedded_thumbnails(&benchy).unwrap();
    // Three QOI pictures and a PNG, each the size its header says.
    assert_eq!(found.len(), 4);
    for e in &found {
        let t = e.decode().unwrap();
        assert_eq!((t.width(), t.height()), (e.width, e.height));
    }
    assert_eq!(best(found).map(|t| (t.width(), t.height())), Some((380, 285)));
    if let Some(b) = asset("paths-extra/libbgcode/mini_cube_ps2.8.1.bgcode") {
        let sink = read_bgcode(&b, None).unwrap();
        assert_eq!(sink.thumbnails.len(), 2);
        for e in &sink.thumbnails {
            let t = e.decode().unwrap();
            assert_eq!((t.width(), t.height()), (e.width, e.height));
        }
    }
}

//endregion

/*┌─────────────────╮
| Binary G-code   |
└─────────────────╯*/
//region Binary G-code

/// Writes a binary G-code file: metadata, a thumbnail, and the G-code in
/// two blocks with the given compression and encoding.
fn write_bgcode(text: &str, compression: u16, encoding: u16, checksum: bool) -> Vec<u8> {
    let mut out = b"GCDE".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(checksum as u16).to_le_bytes());
    let block = |out: &mut Vec<u8>, kind: u16, compression: u16, params: &[u8], data: &[u8]| {
        let stored = match compression {
            0 => data.to_vec(),
            1 => miniz_oxide::deflate::compress_to_vec_zlib(data, 6),
            2 => super::heatshrink::tests::encode(data, 11, 4),
            _ => super::heatshrink::tests::encode(data, 12, 4),
        };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&compression.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        if compression != 0 {
            out.extend_from_slice(&(stored.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(params);
        out.extend_from_slice(&stored);
        if checksum {
            out.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        }
    };
    block(&mut out, 0, 0, &0u16.to_le_bytes(), b"Producer=TestSlicer 1.0\n");
    block(&mut out, 3, compression, &0u16.to_le_bytes(), b"printer_model=MK4\nnozzle_diameter=0.4\n");
    let mut params = 0u16.to_le_bytes().to_vec();
    params.extend_from_slice(&40u16.to_le_bytes());
    params.extend_from_slice(&30u16.to_le_bytes());
    block(&mut out, 5, 0, &params, &png_bytes(40, 30));
    block(&mut out, 4, compression, &0u16.to_le_bytes(), b"estimated printing time (normal mode)=1h 1m 1s\n");
    let half = text.len() / 2;
    let half = text[..half].rfind('\n').map_or(half, |i| i + 1);
    for part in [&text[..half], &text[half..]] {
        let data = match encoding {
            0 => part.as_bytes().to_vec(),
            _ => super::meatpack::tests::encode(part.as_bytes()),
        };
        block(&mut out, 1, compression, &encoding.to_le_bytes(), &data);
    }
    out
}

fn sample_print() -> String {
    sample_print_with(false)
}

/// Five layers of a round wall, with infill inside when asked.
fn sample_print_with(infill: bool) -> String {
    let mut s = String::from("M83\n");
    for layer in 1..6 {
        s.push_str(&format!(";LAYER_CHANGE\n;TYPE:External perimeter\nG1 Z{:.1}\n", layer as f32 * 0.2));
        for i in 0..40 {
            let a = i as f32 / 40.0 * std::f32::consts::TAU;
            s.push_str(&format!("G1X{:.3}Y{:.3}E.05\n", 10.0 * a.cos(), 10.0 * a.sin()));
        }
        if infill {
            s.push_str(";TYPE:Internal infill\nG1 X5 Y5 E.1\nG1 X-5 Y-5 E.1\nG1 X-5 Y5\nG1 X5 Y-5 E.1\n");
        }
    }
    s
}

#[test]
fn binary_gcode_matches_its_text() {
    let text = sample_print();
    let reference = parse_raw(&text);
    let dir = std::env::temp_dir().join(format!("slerp-paths-bgcode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (compression, encoding, checksum) in [(0, 0, false), (1, 0, true), (2, 1, false), (3, 1, true), (1, 2, false)] {
        let path = dir.join(format!("c{compression}e{encoding}.bgcode"));
        std::fs::write(&path, write_bgcode(&text, compression, encoding, checksum)).unwrap();
        let tp = crate::read_toolpath(crate::PathFormat::Bgcode, &path, Keep::All, 1 << 20).unwrap();
        assert_eq!(tp.points.len(), reference.points.len(), "compression {compression} encoding {encoding}");
        assert_eq!(tp.layers.len(), 5);
        assert!(tp.points.iter().zip(&reference.points).all(|(a, b)| close(*a, *b)));
        assert_eq!(tp.meta.generator.as_deref(), Some("TestSlicer 1.0"));
        assert_eq!(tp.meta.printer.as_deref(), Some("MK4"));
        assert_eq!(tp.meta.print_time, Some(3661.0));
        let sink = read_bgcode(&path, None).unwrap();
        assert_eq!(best(sink.thumbnails).map(|t| t.width()), Some(40));
        // Damaged copies: cut anywhere, bytes changed anywhere.
        let bytes = std::fs::read(&path).unwrap();
        for cut in (0..bytes.len()).step_by(37) {
            std::fs::write(&path, &bytes[..cut]).unwrap();
            let _ = crate::read_toolpath(crate::PathFormat::Bgcode, &path, Keep::All, 1 << 20);
            let _ = read_bgcode(&path, None);
        }
        let mut rng = 0x9e3779b97f4a7c15u64;
        for _ in 0..50 {
            let mut m = bytes.clone();
            for _ in 0..4 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let i = (rng % m.len() as u64) as usize;
                m[i] = (rng >> 32) as u8;
            }
            std::fs::write(&path, &m).unwrap();
            let _ = crate::read_toolpath(crate::PathFormat::Bgcode, &path, Keep::All, 1 << 20);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn prusaslicers_binary_gcode_matches_its_text() {
    let (Some(b), Some(t)) =
        (asset("paths-extra/libbgcode/mini_cube_b.bgcode"), asset("paths-extra/libbgcode/mini_cube_b_ref.gcode"))
    else {
        return;
    };
    let binary = crate::read_toolpath(crate::PathFormat::Bgcode, &b, Keep::All, 1 << 24).unwrap();
    let text = crate::read_toolpath(crate::PathFormat::Gcode, &t, Keep::All, 1 << 24).unwrap();
    assert!(binary.format.contains("heatshrink") && binary.format.contains("MeatPack"), "{}", binary.format);
    assert_eq!(binary.points, text.points);
    assert_eq!(binary.moves, text.moves);
    assert_eq!(binary.layers, text.layers);
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

#[test]
fn tubes_merge_straight_runs_and_split_at_corners() {
    let tp = parse(
        ";LAYER_CHANGE\n;HEIGHT:0.2\n;WIDTH:0.4\nG1 Z0.2\nG1 X0 Y0\nG1 X1 E.1\nG1 X2 E.2\nG1 X3 E.3\nG1 Y3 E.4\n",
    );
    let scene = toolpath_scene(&tp, &ToolpathLook::all(), Target::Viewer);
    assert_eq!(scene.nodes.len(), 1);
    // Two runs (the corner splits them), the three collinear moves merged:
    // each run 4 sides × 2 triangles a segment plus two caps of 2.
    assert_eq!(scene.triangle_count(), 2 * (8 + 4));
    let prim = &scene.meshes[0].primitives[0];
    // The bead hangs below the nozzle: the top of the tube is at Z 0.2 mm.
    let top = prim.positions.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    let bottom = prim.positions.iter().map(|p| p.y).fold(f32::MAX, f32::min);
    assert!((top - 0.0002).abs() < 1e-7 && bottom.abs() < 1e-7);
    // 0.4 mm wide: the first tube runs along X, ±0.2 mm across it, and
    // the second along Y, 3 mm long, centred on the print.
    let z = prim.positions.iter().map(|p| p.z).fold(f32::MIN, f32::max)
        - prim.positions.iter().map(|p| p.z).fold(f32::MAX, f32::min);
    assert!((z - 0.0032).abs() < 1e-6, "{z}");
}

#[test]
fn viewer_gets_a_node_per_layer_and_thumbnails_hide_the_inside() {
    let tp = parse(&sample_print_with(true));
    assert!(tp.has_feature(Feature::SparseInfill));
    let viewer = toolpath_scene(&tp, &ToolpathLook::all(), Target::Viewer);
    assert_eq!(viewer.nodes.len(), tp.layers.len());
    let outside = ToolpathLook::outside(&tp);
    assert!(!outside.shown[Feature::SparseInfill.index()]);
    let thumb = toolpath_scene(&tp, &outside, Target::Thumbnail(256));
    assert_eq!(thumb.nodes.len(), 1);
    assert!(thumb.triangle_count() < viewer.triangle_count());
    // Travel lines when asked for.
    let travel = toolpath_scene(&tp, &ToolpathLook { travel: true, ..ToolpathLook::all() }, Target::Viewer);
    assert!(travel.meshes.iter().flat_map(|m| &m.primitives).any(|p| p.topology == slerp_scene::Topology::Lines));
}

//endregion

/*┌─────────────────╮
| Damaged files   |
└─────────────────╯*/
//region Damaged files

/// Truncated and mutated copies of the sample files, and hostile ones, go
/// through the interpreter, the picture scanner and the mesh builder
/// without a panic.
#[test]
fn damaged_gcode_never_panics() {
    let mut samples: Vec<Vec<u8>> = vec![sample_print().into_bytes()];
    for name in ["paths-extra/gcode-preview/calicat.gcode", "paths-extra/gcode-preview/mach3.gcode"] {
        if let Some(p) = asset(name) {
            samples.push(std::fs::read(p).unwrap());
        }
    }
    let run = |bytes: &[u8]| {
        let mut p = Parser::new(Keep::All, 200_000);
        let mut s = Scanner::new(false);
        let mut lines = Lines::new(1 << 16);
        let mut f = |l: &[u8]| {
            p.line(l);
            s.line(l);
            true
        };
        lines.push(bytes, &mut f);
        lines.finish(&mut f);
        s.flush();
        for e in &s.found {
            let _ = e.decode();
        }
        let tp = p.finish();
        let _ = toolpath_scene(&tp, &ToolpathLook::all(), Target::Viewer);
        let _ = toolpath_scene(&tp, &ToolpathLook::outside(&tp), Target::Thumbnail(128));
        let _ = cut_drawing(&tp, 64);
    };
    let mut rng = 0x2545f4914f6cdd1du64;
    for bytes in &samples {
        for cut in (0..bytes.len()).step_by((bytes.len() / 40).max(1)) {
            run(&bytes[..cut]);
        }
        for _ in 0..30 {
            let mut m = bytes.clone();
            for _ in 0..20 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let i = (rng % m.len().max(1) as u64) as usize;
                if let Some(b) = m.get_mut(i) {
                    *b = b"GXYZEIJR-.0123456789;\n \x00\xff"[(rng >> 40) as usize % 24];
                }
            }
            run(&m);
        }
    }
    // Absurd numbers, arcs and lines.
    run(b"G1 X99999999999999999999999999 Y-1e308 E1\nG2 X1 Y1 I0.0000001 J0 E2\nG3 X1e8 Y0 R1e7 E3\nG2 X0 Y0 I1000000 E4\n");
    run(&[b'G'; 1 << 20]);
    run(&b"; thumbnail begin 99999x99999 1\n; ".repeat(1000));
    let mut long = b"G1 X1 E1 ;".to_vec();
    long.extend(std::iter::repeat_n(b'x', 4 << 20));
    run(&long);
}

//endregion

/// A file too large to keep whole is thinned to every few layers for a
/// thumbnail, spread over the whole print, each drawn as thick as the
/// layers it stands for.
#[test]
fn huge_files_thin_their_layers() {
    let text = sample_print();
    let mut p = Parser::with_capacity(Keep::Drawn, 120, 120 * 30 * 3);
    let mut lines = Lines::new(1 << 16);
    lines.push(text.as_bytes(), &mut |l| {
        p.line(l);
        true
    });
    let tp = p.finish();
    assert_eq!(tp.thinned, 3);
    assert_eq!(tp.layers.len(), 5);
    let kept: Vec<usize> = (0..5).filter(|&i| !tp.layer_range(i).is_empty()).collect();
    assert_eq!(kept, [0, 3]);
    assert!((tp.max[2] - 1.0).abs() < 1e-6, "the bounds still cover every layer");
    let scene = toolpath_scene(&tp, &ToolpathLook::all(), Target::Thumbnail(256));
    let ys: Vec<f32> =
        scene.meshes.iter().flat_map(|m| &m.primitives).flat_map(|p| p.positions.iter().map(|v| v.y)).collect();
    // Each bead reaches down three layers' worth, 0.6 mm: layer 4's from
    // 0.8 to 0.2 mm, layer 1's from 0.2 mm to below the bed.
    let near = |v: f32| ys.iter().any(|y| (y - v).abs() < 1e-6);
    assert!(near(0.0008) && near(0.0002) && near(-0.0004));
    assert!(!near(0.0006), "layer 3 was thinned out");
}

/*┌─────────────────╮
| Sampling        |
└─────────────────╯*/
//region Sampling

/// A file of `layers` layers, each a spiral of about `moves` moves, with
/// the slicer's estimate at the end.
fn write_tall_print(path: &Path, layers: u32, moves: u32) {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(w, "; generated by TestSlicer 1.0\nM83\nG21\nG90").unwrap();
    for layer in 1..=layers {
        let z = layer as f32 * 0.2;
        writeln!(
            w,
            ";LAYER_CHANGE\n;Z:{z:.2}\n;HEIGHT:0.2\n;TYPE:External perimeter\n;WIDTH:0.45\nG1 Z{z:.2}\nG1 X20 Y10"
        )
        .unwrap();
        for i in 0..moves {
            let a = i as f32 * 0.01;
            writeln!(w, "G1 X{:.3} Y{:.3} E.02", 10.0 + 10.0 * a.cos(), 10.0 + 10.0 * a.sin()).unwrap();
        }
    }
    writeln!(w, "; estimated printing time (normal mode) = 1h 2m 3s").unwrap();
}

#[test]
fn sampling_spreads_over_the_whole_print() {
    let dir = std::env::temp_dir().join(format!("slerp-paths-sample-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tall.gcode");
    write_tall_print(&path, 300, 6000);
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 30 << 20, "{len}");
    assert!(has_layer_markers(&path).unwrap());

    let whole = crate::read_toolpath(crate::PathFormat::Gcode, &path, Keep::Drawn, 50_000_000).unwrap();
    assert_eq!(whole.layers.len(), 300);
    assert!(!whole.sampled);
    // Above the size sampling starts at, thumbnails read a sample.
    assert!(len < SAMPLE_FILE_BYTES + (10 << 20));
    let mut p = Parser::new(Keep::Drawn, 50_000_000);
    let windows = read_sampled(&path, len, &mut p).unwrap();
    let tp = p.finish();
    assert!(tp.sampled && windows >= 16, "{windows}");
    assert!(tp.points.len() * 2 < whole.points.len(), "{} of {}", tp.points.len(), whole.points.len());
    assert!(tp.layers.len() > 60 && tp.layers.len() < 300, "{}", tp.layers.len());
    // The first and the last layers are there, and the layers in order,
    // none twice.
    assert!((tp.layers[0].z - 0.2).abs() < 1e-4);
    assert!((tp.layers.last().unwrap().z - 60.0).abs() < 1e-3);
    assert!(tp.layers.windows(2).all(|w| w[1].z > w[0].z + 0.1));
    // The estimate at the end of the file is read, and the totals aren't
    // claimed.
    assert_eq!(tp.meta.print_time, Some(3723.0));
    assert_eq!(tp.meta.generator.as_deref(), Some("TestSlicer 1.0"));
    assert!(tp.details().iter().all(|(k, _)| k != "Moves" && k != "Layers"));
    // Drawn, the sampled layers are as thick as the gaps they stand for:
    // the print spans its whole height, and there is no gap in it.
    let scene = toolpath_scene(&tp, &ToolpathLook::all(), Target::Thumbnail(256));
    let ys: Vec<f32> =
        scene.meshes.iter().flat_map(|m| &m.primitives).flat_map(|p| p.positions.iter().map(|v| v.y)).collect();
    let (lo, hi) = (ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max));
    assert!(lo < 0.0002 && lo > -0.0002, "{lo}");
    assert!((hi - 0.0602).abs() < 1e-4 || (hi - 0.060).abs() < 1e-3, "{hi}");
    let mut spans: Vec<(f32, f32)> =
        tp.layers.iter().enumerate().map(|(i, l)| (if i == 0 { 0.0 } else { tp.layers[i - 1].z }, l.z)).collect();
    spans.dedup();
    assert!(spans.windows(2).all(|w| (w[0].1 - w[1].0).abs() < 1e-4), "slabs touch");

    // Without layer comments there is nothing to sample by.
    let plain = dir.join("plain.gcode");
    std::fs::write(&plain, "G1 X1 E1\n".repeat(1000)).unwrap();
    assert!(!has_layer_markers(&plain).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

//endregion

/// `G90` after `M83` leaves the extruder relative, and `G91` before any
/// `M82` or `M83` makes it so.
#[test]
fn extruder_mode_belongs_to_m82_and_m83() {
    let tp = parse("M83\nG21\nG90\nG1 X1 E.5\nG1 X2 E.5\nG1 X3 E.5\n");
    assert_eq!(ends(&tp, MoveKind::Extrude).len(), 3);
    let tp = parse("M82\nG91\nG1 X1 E.5\nG1 X1 E.5\n");
    assert_eq!(ends(&tp, MoveKind::Extrude).len(), 1, "absolute E stays absolute under G91");
    let tp = parse("G91\nG1 X1 E.5\nG1 X1 E.5\nG90\nG1 X5 E.5\nG1 X6 E1.5\n");
    // Relative under G91 (two extrusions); absolute after G90, where the
    // repeated .5 takes back what the first two put out, and 1.5 extrudes.
    assert_eq!(ends(&tp, MoveKind::Extrude).len(), 3);
}

/// A program that starts with a plunge, as milling programs do, draws no
/// line from an origin it never visited, and its bounds are only its cuts.
#[test]
fn unknown_start_begins_nowhere() {
    let mut p = Parser::new(Keep::All, 1000);
    for l in ["G21", "G1 Z3.8 F200", "G0 X60 Y50", "G1 Z-0.8", "G1 X70 Y50", "G1 X70 Y60", "G1 Z3.8"] {
        p.line(l.as_bytes());
    }
    let tp = p.finish();
    assert!(tp.cnc);
    assert_eq!((tp.min[0], tp.min[1]), (60.0, 50.0), "{:?}", tp.min);
    assert_eq!((tp.max[0], tp.max[1]), (70.0, 60.0), "{:?}", tp.max);
    // The first plunge, the cut and the retract are all there.
    assert!((tp.feature_length[Feature::Cut.index()] - (4.6 + 10.0 + 10.0 + 4.6)).abs() < 1e-3);
    // An extrusion first, as in a file that skips its positioning, starts
    // at its own first point.
    let mut p = Parser::new(Keep::All, 1000);
    p.line(b"M83");
    p.line(b"G1 X10 Y10 E1");
    p.line(b"G1 X20 Y10 E1");
    let tp = p.finish();
    assert_eq!(tp.points.first().copied(), Some([10.0, 10.0, 0.0]));
    assert_eq!(tp.points.len(), 2);
}
