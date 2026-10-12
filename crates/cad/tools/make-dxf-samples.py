# Makes the generated DXF test drawings for Slerp's cad crate with ezdxf (MIT licensed).
# The files are our own work and are released as CC0-1.0.  One file per feature group, saved in several DXF
# versions where the group exists in them (R12, R2000, R2007, R2013, R2018), plus binary DXF copies of a few
# groups, a code-page set (R12/R2000 files written in cp1251, cp932, ...), a 3D-only drawing, a drawing with one
# stray entity 1e6 units away, empty drawings, and one big drawing for timing.
#
# Run (needs `pip install ezdxf`; a venv in local/cad-work/venv, say):
#   python make-dxf-samples.py <output-folder> [--big-mb 45] [--no-big]
# The whole run takes about 30 seconds; the big drawing needs about 650 MB of RAM.  --no-big skips it.  To keep a run
# from endangering other programs on a shared machine, start it in its own cgroup:
#   systemd-run --user --scope --quiet --slice=heavy-heavy-slerp.slice python make-dxf-samples.py <folder>
# Prints one line per file: name, DXF version, size, entity counts.

import logging, math, random, struct, sys, zlib
from pathlib import Path

import ezdxf
from ezdxf import colors
from ezdxf.enums import TextEntityAlignment as TA
from ezdxf.entities.mtext import MTextColumns
from ezdxf.math import Matrix44, Vec2, Vec3

logging.getLogger("ezdxf").setLevel(logging.ERROR)  # R12 files make ezdxf warn that $INSUNITS is not written

R12, R2000, R2007, R2013, R2018 = "R12", "R2000", "R2007", "R2013", "R2018"
ALL = [R12, R2000, R2007, R2013, R2018]
MODERN = [R2000, R2007, R2013, R2018]


def cell(i:int, cols:int=4, size:float=30.0):
	"""Origin of the i-th cell in a grid that grows to the right and downward."""
	return ((i % cols) * size, -(i // cols) * size)


def cross(layout, x, y, r=0.3, color=8):
	"""Small marker so alignment points are visible."""
	layout.add_line((x - r, y), (x + r, y), dxfattribs={"color": color})
	layout.add_line((x, y - r), (x, y + r), dxfattribs={"color": color})


#region Groups.  Each takes (doc, version) and fills the model space (and blocks).
def g_basic_shapes(doc, v):
	msp = doc.modelspace()
	for c in range(1, 8):  # ACI colours 1..7
		msp.add_line((0, c * 2), (10, c * 2 + 1), dxfattribs={"color": c})
	msp.add_line((12, 0), (12, 14), dxfattribs={"color": 256})  # BYLAYER
	msp.add_line((13, 0), (13, 14), dxfattribs={"color": 0})  # BYBLOCK outside a block
	for k in range(5):
		msp.add_circle((20 + k * 6, 6), 2 + k * 0.3, dxfattribs={"color": 1 + k})
	for k, (a, b) in enumerate([(0, 90), (45, 270), (270, 90), (350, 10), (0, 359.9), (180, 180.1)]):  # wraps over 0
		msp.add_arc((20 + k * 6, 16), 2.4, a, b, dxfattribs={"color": 1 + k})
	for k, pdmode in enumerate([0, 2, 3, 4, 32, 33, 34, 35, 66, 98]):
		msp.add_point((k * 3, -6), dxfattribs={"color": 2})
	doc.header["$PDMODE"] = 35
	doc.header["$PDSIZE"] = -3.0
	msp.add_solid([(0, -12), (4, -12), (0, -16), (4, -16)], dxfattribs={"color": 3})  # bow-tie vertex order
	msp.add_solid([(6, -12), (10, -12), (6, -16)], dxfattribs={"color": 4})  # triangle (3 points)
	msp.add_trace([(12, -12), (16, -12), (12, -16), (16, -16)], dxfattribs={"color": 5})
	msp.add_line((20, -16, -5), (28, -12, 7), dxfattribs={"color": 6})  # 3D line
	msp.add_circle((34, -14, 3), 2, dxfattribs={"color": 3})  # circle at z = 3
	msp.add_circle((0, 0), 0.0001, dxfattribs={"color": 7})  # tiny
	msp.add_line((5, 30), (5, 30), dxfattribs={"color": 2})  # zero-length line


def g_layers_linetypes(doc, v):
	msp = doc.modelspace()
	doc.layers.add("VISIBLE", color=2)
	off = doc.layers.add("OFF_LAYER", color=1)
	off.off()
	fr = doc.layers.add("FROZEN_LAYER", color=3)
	fr.freeze()
	lk = doc.layers.add("LOCKED_LAYER", color=4)
	lk.lock()
	doc.layers.add("COLOR_NEG", color=5)
	pl = doc.layers.add("PLOTOFF", color=6)
	if v != R12:
		pl.dxf.plot = 0
	if "CUSTOM" not in doc.linetypes:
		doc.linetypes.add("CUSTOM", pattern=[1.25, 0.5, -0.25, 0.0, -0.5], description="dash gap dot gap")
	for i, lay in enumerate(["VISIBLE", "OFF_LAYER", "FROZEN_LAYER", "LOCKED_LAYER", "COLOR_NEG", "PLOTOFF"]):
		msp.add_circle((i * 6, 0), 2, dxfattribs={"layer": lay})
		msp.add_line((i * 6 - 2, -4), (i * 6 + 2, -4), dxfattribs={"layer": lay})
	names = [n for n in ("CONTINUOUS", "DASHED", "DASHDOT", "CENTER", "DOT", "PHANTOM", "HIDDEN", "CUSTOM") if n in doc.linetypes]
	for i, lt in enumerate(names):  # entity-level linetypes
		msp.add_line((0, 4 + i * 1.5), (40, 4 + i * 1.5), dxfattribs={"linetype": lt, "color": 7})
	for i, lt in enumerate(names[:4]):  # layers carrying a linetype
		doc.layers.add("LT_" + lt, color=3, linetype=lt)
		msp.add_line((0, 18 + i), (40, 18 + i), dxfattribs={"layer": "LT_" + lt})
	for i, s in enumerate([0.25, 0.5, 2.0]):  # per-entity linetype scale
		msp.add_line((0, 24 + i), (40, 24 + i), dxfattribs={"linetype": "DASHED", "ltscale": s, "color": 4})
	doc.header["$LTSCALE"] = 2.0
	if v != R12:  # lineweights (hundredths of mm), 25 = default, -1 = BYLAYER, -2 = BYBLOCK, -3 = default
		for i, lw in enumerate([0, 5, 25, 50, 100, 211, -1, -2, -3]):
			msp.add_line((45, i * 1.5), (60, i * 1.5), dxfattribs={"lineweight": lw, "color": 7})
		doc.header["$LWDISPLAY"] = 1


def _leaf_block(doc, name="LEAF"):
	b = doc.blocks.new(name)
	b.add_line((0, 0), (4, 0), dxfattribs={"color": 0})  # BYBLOCK: takes the colour of the INSERT
	b.add_circle((2, 2), 1.5, dxfattribs={"color": 256})  # BYLAYER
	b.add_line((0, 0), (0, 4), dxfattribs={"color": 3})  # fixed
	b.add_arc((2, 2), 2.5, 0, 90, dxfattribs={"color": 0, "linetype": "BYBLOCK"} if "BYBLOCK" in doc.linetypes else {"color": 0})
	return b


def g_blocks_nested(doc, v):
	msp = doc.modelspace()
	_leaf_block(doc)
	mid = doc.blocks.new("MID")
	mid.add_blockref("LEAF", (0, 0), dxfattribs={"color": 0})  # BYBLOCK insert inside a block
	mid.add_blockref("LEAF", (6, 0), dxfattribs={"color": 5, "rotation": 30, "xscale": 0.7, "yscale": 0.7})
	mid.add_blockref("LEAF", (12, 0), dxfattribs={"color": 256})
	mid.add_line((0, -1), (16, -1), dxfattribs={"color": 0})
	root = doc.blocks.new("ROOT", base_point=(1, 1))  # block base point is not the origin
	root.add_blockref("MID", (0, 0), dxfattribs={"color": 0})
	root.add_blockref("MID", (0, 8), dxfattribs={"rotation": 90, "xscale": 0.5, "yscale": 0.5, "color": 1})
	root.add_circle((8, 4), 3, dxfattribs={"layer": "0", "color": 0})
	doc.layers.add("BLOCKS_ON_THIS_LAYER", color=6)
	msp.add_blockref("ROOT", (0, 0), dxfattribs={"color": 1})
	msp.add_blockref("ROOT", (30, 0), dxfattribs={"color": 5, "layer": "BLOCKS_ON_THIS_LAYER"})
	msp.add_blockref("ROOT", (60, 0), dxfattribs={"rotation": 45, "xscale": 1.5, "yscale": 1.5})
	msp.add_blockref("ROOT", (0, -30), dxfattribs={"color": 0})  # BYBLOCK in the model space
	msp.add_blockref("MID", (40, -30), dxfattribs={"color": 3})
	empty = doc.blocks.new("EMPTY_BLOCK")
	msp.add_blockref("EMPTY_BLOCK", (80, -30))
	msp.add_blockref("LEAF", (60, -30), dxfattribs={"xscale": 3, "yscale": 3, "zscale": 3, "rotation": 20})


def g_attribs(doc, v):
	msp = doc.modelspace()
	b = doc.blocks.new("TAGGED")
	b.add_lwpolyline([(0, 0), (8, 0), (8, 4), (0, 4)], close=True) if v != R12 else b.add_polyline2d([(0, 0), (8, 0), (8, 4), (0, 4)], close=True)
	b.add_attdef("NAME", (0.5, 2.8), "default name", dxfattribs={"height": 0.8, "prompt": "Name?"})
	b.add_attdef("HIDDEN", (0.5, 1.8), "hidden value", dxfattribs={"height": 0.6, "flags": 1})  # invisible
	b.add_attdef("CONST", (0.5, 0.8), "constant", dxfattribs={"height": 0.6, "flags": 2})  # constant
	b.add_attdef("NUM", (5, 0.8), "42", dxfattribs={"height": 0.6, "flags": 0, "rotation": 0})
	outer = doc.blocks.new("OUTER")
	outer.add_blockref("TAGGED", (0, 0)).add_auto_attribs({"NAME": "inner in block", "HIDDEN": "x", "NUM": "1"})
	outer.add_circle((4, 6), 1)
	for i, rot in enumerate([0, 30, 90]):
		ins = msp.add_blockref("TAGGED", (i * 14, 0), dxfattribs={"rotation": rot, "xscale": 1.0 + i * 0.25, "yscale": 1.0 + i * 0.25})
		ins.add_auto_attribs({"NAME": f"Item {i}", "HIDDEN": "secret", "NUM": str(i * 10)})
	ins = msp.add_blockref("TAGGED", (0, -12), dxfattribs={"xscale": -1})  # mirrored insert: the attributes get mirrored too
	ins.add_auto_attribs({"NAME": "mirrored insert", "NUM": "7"})
	msp.add_blockref("OUTER", (30, -12))  # nested INSERT carrying attribute INSERTs
	# attribute whose value is empty, and with alignment
	ins = msp.add_blockref("TAGGED", (0, -24))
	ins.add_auto_attribs({"NAME": "", "NUM": "0"})


def g_minsert(doc, v):
	msp = doc.modelspace()
	t = doc.blocks.new("TILE")
	t.add_lwpolyline([(0, 0), (3, 0), (3, 3), (0, 3)], close=True) if v != R12 else t.add_polyline2d([(0, 0), (3, 0), (3, 3), (0, 3)], close=True)
	t.add_line((0, 0), (3, 3), dxfattribs={"color": 1})
	t.add_circle((1, 2), 0.5, dxfattribs={"color": 3})
	msp.add_blockref("TILE", (0, 0)).grid(size=(3, 4), spacing=(4, 5))  # (rows, columns), (row spacing, column spacing)
	ins = msp.add_blockref("TILE", (30, 0), dxfattribs={"rotation": 30})
	ins.grid(size=(3, 3), spacing=(4.5, 4.5))  # the array rotates with the insert
	ins = msp.add_blockref("TILE", (60, 0), dxfattribs={"xscale": -1, "yscale": 0.5})
	ins.grid(size=(2, 5), spacing=(5, -4))  # mirrored, negative column spacing
	ins = msp.add_blockref("TILE", (0, -30), dxfattribs={"xscale": 0.5, "yscale": 0.5})
	ins.grid(size=(10, 10), spacing=(2, 2))  # 100 copies
	msp.add_blockref("TILE", (30, -30)).grid(size=(1, 6), spacing=(0, 4))  # a single row
	msp.add_blockref("TILE", (60, -30)).grid(size=(5, 1), spacing=(4, 0))  # a single column


def g_inserts_transform(doc, v):
	msp = doc.modelspace()
	f = doc.blocks.new("F_SHAPE")  # asymmetric, so mirroring is visible
	f.add_line((0, 0), (0, 6), dxfattribs={"color": 1})
	f.add_line((0, 6), (4, 6), dxfattribs={"color": 3})
	f.add_line((0, 3), (3, 3), dxfattribs={"color": 5})
	f.add_arc((4, 2), 2, 270, 90, dxfattribs={"color": 2})
	f.add_text("F", dxfattribs={"height": 1.2, "insert": (1, 0.5)})
	cases = [
		{}, {"rotation": 30}, {"rotation": 90}, {"rotation": 180}, {"xscale": -1}, {"yscale": -1}, {"xscale": -1, "yscale": -1},
		{"xscale": 2, "yscale": 0.5}, {"xscale": -1, "rotation": 30}, {"yscale": -2, "rotation": 60}, {"xscale": 0.5, "yscale": 0.5},
		{"zscale": -1}, {"extrusion": (0, 0, -1)}, {"extrusion": (0, 0, -1), "rotation": 30}, {"xscale": -1, "extrusion": (0, 0, -1)},
		{"xscale": 0.05, "yscale": 0.05}, {"xscale": 3, "yscale": 3},
	]
	for i, a in enumerate(cases):
		x, y = cell(i, 5, 14)
		cross(msp, x, y)
		msp.add_blockref("F_SHAPE", (x, y), dxfattribs=a)
		msp.add_text(str(a)[:30], dxfattribs={"height": 0.5, "insert": (x, y - 1.2), "color": 8})


def g_lwpolyline(doc, v):
	msp = doc.modelspace()
	msp.add_lwpolyline([(0, 0), (5, 0), (5, 5), (0, 5)], close=True, dxfattribs={"const_width": 0.5})
	msp.add_lwpolyline([(8, 0, 0.0, 1.0), (14, 0, 1.0, 0.1), (14, 5, 0.1, 1.5), (8, 5, 0.0, 0.0)], format="xyse")  # per-vertex widths
	msp.add_lwpolyline([(18, 0, 0, 0, 1), (24, 0, 0, 0, -1), (24, 5, 0, 0, 0.5), (18, 5, 0, 0, 0)], format="xyseb", close=True)  # bulges
	msp.add_lwpolyline([(28, 0, 0.3, 0.3, 0.41421356), (32, 4, 0.3, 0.3, 0.41421356), (28, 4, 0.3, 0.3, 0)], format="xyseb")  # arc + width
	msp.add_lwpolyline([(0, -10), (6, -10), (3, -6)], close=True, dxfattribs={"elevation": 4.0})
	msp.add_lwpolyline([(10, -10), (20, -10)], dxfattribs={"const_width": 2.0})
	msp.add_lwpolyline([(22, -10)])  # one vertex
	msp.add_lwpolyline([(24, -10), (24, -10), (28, -6), (28, -6)])  # duplicate vertices
	pl = msp.add_lwpolyline([(30, -10), (34, -10), (34, -6), (30, -6)], format="xyb", close=True)
	pl.dxf.flags |= ezdxf.const.LWPOLYLINE_PLINEGEN  # linetype generation across vertices
	pl.dxf.linetype = "DASHED"
	msp.add_lwpolyline([(0, -20, 0, 0, 0), (6, -20, 0, 0, 5.0), (12, -20, 0, 0, 0)], format="xyseb")  # bulge > 1 (more than a half circle)
	msp.add_lwpolyline([(14, -20, 0, 0, 1e-6), (20, -20, 0, 0, 1e-6)], format="xyseb")  # nearly flat bulge
	msp.add_lwpolyline([(0, -30), (10, -30), (10, -26), (0, -26)], close=True, dxfattribs={"extrusion": (0, 0, -1), "color": 3})


def g_polyline2d(doc, v):
	"""The old heavyweight POLYLINE (VERTEX/SEQEND), the only polyline in R12."""
	msp = doc.modelspace()
	msp.add_polyline2d([(0, 0), (5, 0), (5, 5), (0, 5)], close=True)
	msp.add_polyline2d([(8, 0, 0.0, 1.0), (14, 0, 1.0, 0.1), (14, 5, 0.1, 1.5), (8, 5, 0.0, 0.0)], format="xyse")
	msp.add_polyline2d([(18, 0, 0, 0, 1), (24, 0, 0, 0, -1), (24, 5, 0, 0, 0.5), (18, 5, 0, 0, 0)], format="xyseb", close=True)
	p = msp.add_polyline2d([(28, 0), (30, 4), (34, 1), (36, 5)], dxfattribs={"default_start_width": 0.4, "default_end_width": 0.4})
	p = msp.add_polyline2d([(0, -10), (3, -6), (6, -10), (9, -6), (12, -10)])
	p.dxf.flags |= 4  # curve-fit vertices added (the stored points are the fit control frame)
	p = msp.add_polyline2d([(16, -10), (19, -6), (22, -10), (25, -6), (28, -10)])
	p.dxf.flags |= 2  # splined fit
	p.dxf.smooth_type = 6  # cubic
	msp.add_polyline3d([(0, -20, 0), (4, -18, 3), (8, -22, 6), (12, -19, 2)])
	msp.add_polyline3d([(16, -20, 0), (20, -20, 0), (20, -16, 4), (16, -16, 4)], close=True)
	msp.add_polyline2d([(30, -20), (34, -20), (34, -16)], dxfattribs={"elevation": (0, 0, 5)})


def g_splines(doc, v):
	msp = doc.modelspace()
	fit = [(0, 0), (3, 4), (6, -2), (10, 3), (14, 0)]
	msp.add_spline(fit_points=fit, dxfattribs={"color": 1})  # only fit points
	cp = [(0, 8), (3, 14), (7, 8), (11, 14), (15, 8)]
	msp.add_open_spline(cp, degree=3, dxfattribs={"color": 2})  # control points, open uniform
	msp.add_open_spline(cp, degree=2, dxfattribs={"color": 3}).dxf.layer = "0"
	s = msp.add_open_spline([(x, y - 12) for x, y in cp], degree=4, dxfattribs={"color": 4})
	w = [1, 2, 0.5, 3, 1]
	msp.add_rational_spline([(x, y - 24) for x, y in cp], weights=w, degree=3, dxfattribs={"color": 5})  # NURBS, weights
	# quarter-circle rational spline: degree 2, weights 1, sqrt(2)/2, 1
	r2 = math.sqrt(2) / 2
	msp.add_rational_spline([(20, 0), (20, 6), (14, 6)], weights=[1, r2, 1], degree=2, knots=[0, 0, 0, 1, 1, 1], dxfattribs={"color": 6})
	# fit points with end tangents
	sp = msp.add_spline(fit_points=[(20, -8), (24, -4), (28, -9), (32, -4)], dxfattribs={"color": 3})
	sp.dxf.start_tangent = Vec3(0, 1, 0)
	sp.dxf.end_tangent = Vec3(1, 0, 0)
	# both fit points and control points (as AutoCAD writes them)
	from ezdxf.math import BSpline
	fp = [(20, -16), (24, -12), (28, -17), (32, -12)]
	sp = msp.add_spline(dxfattribs={"color": 4})
	sp.fit_points = fp
	bs = BSpline.from_fit_points(fp, degree=3)
	sp.control_points = bs.control_points
	sp.knots = bs.knots()
	sp.dxf.degree = 3
	# closed (periodic) spline
	pts = [(40, 0), (44, 4), (48, 0), (44, -4)]
	ring = pts + pts[:3]
	sp = msp.add_open_spline(ring, degree=3, knots=[float(i) for i in range(len(ring) + 4)], dxfattribs={"color": 1})
	sp.dxf.flags |= 1 | 2  # closed + periodic
	# degree 1 (a polyline in disguise), and a high degree
	msp.add_open_spline([(40, 8), (44, 12), (48, 8), (52, 12)], degree=1, dxfattribs={"color": 2})
	msp.add_open_spline([(40, -10)] + [(41 + i * 2, -10 + 3 * math.sin(i)) for i in range(9)], degree=7, dxfattribs={"color": 5})
	# tiny and huge extents
	msp.add_spline(fit_points=[(60, 0), (60.001, 0.002), (60.002, 0), (60.003, 0.001)], dxfattribs={"color": 3})
	# spline in a rotated plane (3D control points)
	msp.add_open_spline([(0, -40, 0), (4, -38, 3), (8, -42, 6), (12, -38, 9), (16, -42, 12)], degree=3, dxfattribs={"color": 6})


def g_ellipses(doc, v):
	msp = doc.modelspace()
	msp.add_ellipse((0, 0), major_axis=(5, 0), ratio=0.5, dxfattribs={"color": 1})  # full
	msp.add_ellipse((14, 0), major_axis=(5, 0), ratio=0.5, start_param=0, end_param=math.pi, dxfattribs={"color": 2})  # half
	msp.add_ellipse((28, 0), major_axis=(5, 0), ratio=0.5, start_param=math.pi * 1.5, end_param=math.pi * 0.5, dxfattribs={"color": 3})  # wraps over 0
	msp.add_ellipse((0, -14), major_axis=(3, 3), ratio=0.4, dxfattribs={"color": 4})  # rotated 45 degrees
	msp.add_ellipse((14, -14), major_axis=(0, 5), ratio=0.3, start_param=0.3, end_param=2.2, dxfattribs={"color": 5})  # vertical major axis
	msp.add_ellipse((28, -14), major_axis=(5, 0), ratio=1.0, dxfattribs={"color": 6})  # a circle
	msp.add_ellipse((0, -28), major_axis=(5, 0), ratio=0.02, dxfattribs={"color": 7})  # nearly flat
	msp.add_ellipse((14, -28), major_axis=(5, 0), ratio=0.5, dxfattribs={"color": 1, "extrusion": (0, 0, -1)})  # extruded: reversed
	msp.add_ellipse((28, -28), major_axis=(5, 0), ratio=0.5, start_param=0, end_param=math.pi, dxfattribs={"color": 2, "extrusion": (0, 0, -1)})
	msp.add_ellipse((0, -42, 5), major_axis=(4, 0, 0), ratio=0.5, dxfattribs={"color": 3, "extrusion": (0, math.sqrt(0.5), math.sqrt(0.5))})  # tilted plane
	msp.add_ellipse((14, -42), major_axis=(5, 0), ratio=0.5, start_param=1.0, end_param=1.001, dxfattribs={"color": 4})  # tiny arc


def g_text(doc, v):
	msp = doc.modelspace()
	doc.styles.add("TT_ARIAL", font="arial.ttf")
	doc.styles.add("SHX_SIMPLEX", font="simplex.shx")
	doc.styles.add("NARROW", font="txt")
	doc.styles.get("NARROW").dxf.width = 0.6
	doc.styles.add("OBLIQUE", font="txt").dxf.oblique = 20
	aligns = [TA.LEFT, TA.CENTER, TA.RIGHT, TA.ALIGNED, TA.MIDDLE, TA.FIT, TA.BOTTOM_LEFT, TA.BOTTOM_CENTER, TA.BOTTOM_RIGHT,
	          TA.MIDDLE_LEFT, TA.MIDDLE_CENTER, TA.MIDDLE_RIGHT, TA.TOP_LEFT, TA.TOP_CENTER, TA.TOP_RIGHT]
	for i, a in enumerate(aligns):
		x, y = cell(i, 5, 16)
		cross(msp, x, y)
		t = msp.add_text(a.name, dxfattribs={"height": 1.5, "style": "STANDARD"})
		if a in (TA.ALIGNED, TA.FIT):
			t.set_placement((x - 4, y), p2=(x + 4, y + 1), align=a)
		else:
			t.set_placement((x, y), align=a)
	y0 = -36
	y = y0 - 4
	for h in [0.2, 0.5, 1, 2, 4]:  # increasing heights, one below the other
		msp.add_text(f"height {h}", dxfattribs={"height": h, "insert": (0, y)})
		y -= h * 1.5 + 0.5
	for i, rot in enumerate([0, 30, 90, 180, 270, 45]):
		msp.add_text(f"rot {rot}", dxfattribs={"height": 1, "insert": (30 + i * 8, y0 - 6), "rotation": rot, "color": 1 + i})
	y -= 4
	for row in [  # (text, x, extra DXF attributes)
		[("oblique 30", 0, {"oblique": 30}), ("oblique -30", 20, {"oblique": -30}), ("width 0.5", 40, {"width": 0.5}), ("width 2.0", 55, {"width": 2.0})],
		[("mirrored X", 0, {"text_generation_flag": 2}), ("mirrored Y (upside down)", 20, {"text_generation_flag": 4})],
		[("style TT_ARIAL", 0, {"style": "TT_ARIAL"}), ("style SHX_SIMPLEX", 20, {"style": "SHX_SIMPLEX"}),
		 ("style NARROW (0.6)", 45, {"style": "NARROW"}), ("style OBLIQUE (20)", 65, {"style": "OBLIQUE"})],
		[("%%u underlined%%u %%onot overlined%%o 45%%d +-%%p diam %%c %%%% percent", 0, {})],
		[("^I caret ^M control ^J", 0, {}), ("", 30, {}), ("tiny text", 40, {"height": 0.01})],  # empty string, tiny height
	]:
		for text, x, extra in row:
			msp.add_text(text, dxfattribs=dict({"height": 1.5, "insert": (x, y)}, **extra))
		y -= 4
	if v in (R2007, R2013, R2018):
		msp.add_text("unicode: \u00c4\u00d6\u00dc\u00df \u20ac \u03a9 \u2248 \u4e2d\u6587 \u0416", dxfattribs={"height": 1.5, "insert": (0, y), "style": "TT_ARIAL"})


MTEXT_SAMPLES = [
	"Line one\\PLine two\\PLine three",
	"{\\C1;red}{\\C3; green} {\\c16711680;true colour blue}",
	"\\fArial|b1|i0;bold arial \\fTimes New Roman|b0|i1;italic times\\f|b0|i0; back to default",
	"\\H2x;double height \\H0.5x;half \\H3;absolute height 3",
	"\\W2;wide \\W0.5;narrow \\W1;normal",
	"\\Q30;oblique \\Q0;straight",
	"\\T2;spaced out \\T1;normal tracking",
	"\\S1/2; fraction  \\S1^2; tolerance stack  \\S1#2; diagonal",
	"\\Lunderlined\\l \\Ooverlined\\o \\Kstrike\\k",
	"nbsp\\~here \\\\ backslash \\{ brace \\} \\U+00E4 \\U+4E2D",
	"%%d degrees %%p plus-minus %%c diameter",
	"{\\C1;nested {\\C2;inner {\\C3;innermost} inner} outer} plain",
	"\\A1;centered line vs \\A0;bottom \\A2;top",
	"\\pxi3,l3,r3;indent paragraph\\Pnext paragraph\\pxqc;centered paragraph",
	"\\Fromans|c0;font code",
	"tab\\tbetween",
]


def g_mtext(doc, v):
	msp = doc.modelspace()
	for i, s in enumerate(MTEXT_SAMPLES):
		x, y = cell(i, 4, 25)
		cross(msp, x, y)
		m = msp.add_mtext(s, dxfattribs={"insert": (x, y), "char_height": 1.0, "width": 22.0})
	for i in range(1, 10):  # attachment points 1..9
		x, y = cell(i - 1 + 20, 5, 14)
		cross(msp, x, y)
		msp.add_mtext(f"attach {i}\\Psecond line", dxfattribs={"insert": (x, y), "char_height": 1.0, "attachment_point": i, "width": 10})
	x, y = 0, -140
	msp.add_mtext("rotated 30 degrees with width 12, wrapping long text that must be broken into lines", dxfattribs={"insert": (x, y), "char_height": 1.0, "width": 12, "rotation": 30})
	msp.add_mtext("line spacing factor 2.0\\Psecond\\Pthird", dxfattribs={"insert": (30, y), "char_height": 1.0, "line_spacing_factor": 2.0, "width": 15})
	msp.add_mtext("text direction vector (0,1)", dxfattribs={"insert": (55, y), "char_height": 1.0, "text_direction": (0, 1, 0)})
	m = msp.add_mtext("no width: single line that never wraps, although long", dxfattribs={"insert": (0, y - 20), "char_height": 1.0})
	if v in (R2007, R2013, R2018):
		m = msp.add_mtext("background fill red\\Pframe", dxfattribs={"insert": (0, y - 30), "char_height": 1.0, "width": 15})
		m.set_bg_color(1, scale=1.5)
		m = msp.add_mtext("background colour from drawing", dxfattribs={"insert": (30, y - 30), "char_height": 1.0, "width": 15})
		m.set_bg_color(None, scale=1.5)
		m = msp.add_mtext("true colour background", dxfattribs={"insert": (55, y - 30), "char_height": 1.0, "width": 15})
		m.set_bg_color((60, 160, 90), scale=1.2)
	if v in (R2007, R2013, R2018):  # three static columns; R2018 stores them in the entity, older versions as linked MTEXTs
		m = msp.add_mtext("", dxfattribs={"insert": (0, y - 50), "char_height": 1.0})
		m.setup_columns(MTextColumns.new_static_columns(count=3, width=14, gutter_width=2, height=12), linked=(v != R2018))
		linked = m.columns.linked_columns if v != R2018 else []
		for col in linked:  # linked columns are separate MTEXT entities; they need handles, and the first column writes them out
			doc.entitydb.add(col)
		for i, col in enumerate([m] + linked):
			col.text = f"Column {i + 1} of text that wraps inside the column width and flows on."
		if v == R2018:
			m.text = " ".join(["Columns text flows from one column into the next when the first is full."] * 6)


def g_dimensions(doc, v):
	msp = doc.modelspace()
	msp.add_line((0, 0), (10, 0)); msp.add_line((0, 0), (0, 6)); msp.add_line((10, 0), (10, 6))
	msp.add_linear_dim(base=(5, 9), p1=(0, 6), p2=(10, 6), dimstyle="EZDXF").render()
	msp.add_linear_dim(base=(-3, 3), p1=(0, 0), p2=(0, 6), angle=90, dimstyle="EZDXF").render()
	msp.add_linear_dim(base=(15, 3), p1=(10, 0), p2=(10, 6), angle=90, dimstyle="EZDXF", text="<> mm (override)").render()
	msp.add_linear_dim(base=(25, 8), p1=(20, 0), p2=(30, 4), angle=30, dimstyle="EZDXF").render()  # rotated linear
	msp.add_aligned_dim(p1=(20, -4), p2=(30, -10), distance=2, dimstyle="EZDXF").render()
	msp.add_linear_dim(base=(5, -6), p1=(0, -10), p2=(10, -10), dimstyle="EZDXF", override={"dimtad": 0, "dimtxt": 0.8, "dimclrd": 1, "dimasz": 0.4}).render()
	msp.add_circle((50, 4), 4)
	msp.add_diameter_dim(center=(50, 4), radius=4, angle=45, dimstyle="EZDXF").render()
	msp.add_arc((50, -10), 4, 20, 140)
	msp.add_radius_dim(center=(50, -10), radius=4, angle=80, dimstyle="EZDXF").render()
	msp.add_radius_dim(center=(60, -10), radius=3, angle=-30, dimstyle="EZDXF", override={"dimtix": 1}).render()
	msp.add_angular_dim_cra(center=(70, 0), radius=5, start_angle=20, end_angle=100, distance=2, dimstyle="EZDXF").render()
	msp.add_angular_dim_3p(base=(70, -12), center=(70, -20), p1=(76, -20), p2=(70, -14), dimstyle="EZDXF").render()
	msp.add_ordinate_dim(feature_location=(5, 25), offset=(0, 3), dtype=0, origin=(0, 20), dimstyle="EZDXF").render()  # x
	msp.add_ordinate_dim(feature_location=(8, 28), offset=(3, 0), dtype=1, origin=(0, 20), dimstyle="EZDXF").render()  # y
	if v != R2000:
		msp.add_arc_dim_3p(base=(86, -2), center=(80, -10), p1=(86, -10), p2=(80, -4), dimstyle="EZ_CURVED").render()
	# a dimension style with other arrows and text style
	if "WIDE" not in doc.dimstyles:
		ds = doc.dimstyles.duplicate_entry("EZDXF", "WIDE")
		ds.dxf.dimtxt = 1.2
		ds.dxf.dimasz = 0.8
	msp.add_linear_dim(base=(5, -16), p1=(0, -20), p2=(10, -20), dimstyle="WIDE").render()


def g_hatches(doc, v):
	msp = doc.modelspace()
	sq = lambda x, y, s: [(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
	h = msp.add_hatch(color=1)  # solid, polyline boundary
	h.paths.add_polyline_path(sq(0, 0, 8), is_closed=True)
	h = msp.add_hatch(color=2)  # solid with a hole
	h.paths.add_polyline_path(sq(12, 0, 8), is_closed=True, flags=ezdxf.const.BOUNDARY_PATH_EXTERNAL)
	h.paths.add_polyline_path(sq(14, 2, 4), is_closed=True, flags=ezdxf.const.BOUNDARY_PATH_OUTERMOST)
	h = msp.add_hatch(color=3)  # pattern ANSI31
	h.set_pattern_fill("ANSI31", scale=0.5, angle=0)
	h.paths.add_polyline_path(sq(24, 0, 8), is_closed=True)
	h = msp.add_hatch(color=4)  # pattern cross + holes
	h.set_pattern_fill("ANSI37", scale=0.4)
	h.paths.add_polyline_path(sq(36, 0, 8), is_closed=True)
	h.paths.add_polyline_path(sq(38, 2, 2), is_closed=True)
	h.paths.add_polyline_path(sq(41, 5, 2), is_closed=True)
	h = msp.add_hatch(color=5)  # user-defined pattern: two line families, dashed second family
	h.set_pattern_fill("MYGRID", scale=1.0, definition=[[0, (0, 0), (0, 1), []], [90, (0, 0), (0, 1), [0.5, -0.5]]])
	h.paths.add_polyline_path(sq(48, 0, 8), is_closed=True)
	h = msp.add_hatch(color=6)  # rotated pattern, large scale
	h.set_pattern_fill("ANSI32", scale=1.5, angle=30)
	h.paths.add_polyline_path(sq(60, 0, 8), is_closed=True)
	h = msp.add_hatch(color=1)  # polyline boundary with bulges
	h.paths.add_polyline_path([(0, -20, 0), (8, -20, 1), (8, -14, 0), (0, -14, -0.5)], is_closed=True)
	h = msp.add_hatch(color=2)  # edge path: line + arc ("D" shape)
	ep = h.paths.add_edge_path()
	ep.add_line((12, -20), (12, -12))
	ep.add_arc((12, -16), radius=4, start_angle=-90, end_angle=90, ccw=True)
	h = msp.add_hatch(color=3)  # edge path: full circle as two arcs, and an ellipse hole
	ep = h.paths.add_edge_path(flags=ezdxf.const.BOUNDARY_PATH_EXTERNAL)
	ep.add_arc((28, -16), radius=4, start_angle=0, end_angle=180, ccw=True)
	ep.add_arc((28, -16), radius=4, start_angle=180, end_angle=360, ccw=True)
	ep2 = h.paths.add_edge_path(flags=ezdxf.const.BOUNDARY_PATH_OUTERMOST)
	ep2.add_ellipse((28, -16), major_axis=(2, 0), ratio=0.5)
	h = msp.add_hatch(color=4)  # edge path with an elliptic arc and a spline
	ep = h.paths.add_edge_path()
	ep.add_ellipse((44, -16), major_axis=(4, 0), ratio=0.6, start_angle=0, end_angle=180, ccw=True)
	ep.add_spline(control_points=[(48, -16), (47, -20), (45, -12), (40, -16)], degree=3, knot_values=[0, 0, 0, 0, 1, 1, 1, 1])
	h = msp.add_hatch(color=5)  # triangle by edges
	ep = h.paths.add_edge_path()
	ep.add_line((52, -20), (60, -20)); ep.add_line((60, -20), (56, -12)); ep.add_line((56, -12), (52, -20))
	# hatch styles with islands: normal / outer / ignore
	for i, style in enumerate([0, 1, 2]):
		x = i * 14
		h = msp.add_hatch(color=1 + i, dxfattribs={"hatch_style": style})
		h.set_pattern_fill("ANSI31", scale=0.5)
		h.paths.add_polyline_path(sq(x, -34, 12), is_closed=True, flags=ezdxf.const.BOUNDARY_PATH_EXTERNAL)
		h.paths.add_polyline_path(sq(x + 2, -32, 8), is_closed=True, flags=ezdxf.const.BOUNDARY_PATH_OUTERMOST)
		h.paths.add_polyline_path(sq(x + 4, -30, 4), is_closed=True, flags=ezdxf.const.BOUNDARY_PATH_DEFAULT)  # island in island
	if v != R2000:
		h = msp.add_hatch()  # gradient
		h.set_gradient((255, 0, 0), (0, 0, 255), rotation=0, name="LINEAR")
		h.paths.add_polyline_path(sq(0, -52, 10), is_closed=True)
		h = msp.add_hatch()
		h.set_gradient((255, 255, 0), (0, 128, 0), rotation=45, name="CYLINDER", centered=0.5)
		h.paths.add_polyline_path(sq(14, -52, 10), is_closed=True)
		h = msp.add_hatch()
		h.set_gradient((255, 255, 255), (30, 30, 200), name="SPHERICAL")
		h.paths.add_polyline_path(sq(28, -52, 10), is_closed=True)
	h = msp.add_hatch(color=7)  # true colour solid
	h.rgb = (255, 128, 0)
	h.paths.add_polyline_path(sq(42, -52, 10), is_closed=True)
	h = msp.add_hatch(color=2)  # self-intersecting boundary (bow-tie)
	h.paths.add_polyline_path([(56, -52), (66, -42), (66, -52), (56, -42)], is_closed=True)
	h = msp.add_hatch(color=1, dxfattribs={"elevation": (0, 0, 3)})  # elevation and tiny
	h.paths.add_polyline_path(sq(0, -70, 0.05), is_closed=True)


def g_leaders(doc, v):
	msp = doc.modelspace()
	msp.add_leader([(0, 0), (4, 3), (8, 3)], dimstyle="EZDXF")
	l = msp.add_leader([(12, 0), (14, 4), (18, 2), (22, 5)], dimstyle="EZDXF")
	l.dxf.path_type = 1  # spline path
	l = msp.add_leader([(0, -10), (4, -7)], dimstyle="EZDXF", override={"dimasz": 0.8})
	l.dxf.has_hookline = 0
	msp.add_text("annotation", dxfattribs={"height": 1, "insert": (8, 3.2)})
	if v in (R2007, R2013, R2018):
		from ezdxf.render import ConnectionSide
		ml = msp.add_multileader_mtext("Standard")
		ml.set_content("MULTILEADER\\Pwith two lines", char_height=1.0)
		ml.add_leader_line(ConnectionSide.left, [(0, -22), (-3, -20)])
		ml.add_leader_line(ConnectionSide.left, [(0, -22), (-3, -24)])
		ml.build(insert=Vec2(6, -22))
		blk = doc.blocks.new("MARKER")
		blk.add_circle((0, 0), 1)
		blk.add_line((-1, 0), (1, 0))
		ml = msp.add_multileader_block("Standard")
		ml.set_content("MARKER")
		ml.add_leader_line(ConnectionSide.right, [(32, -22), (36, -20)])
		ml.build(insert=Vec2(30, -22))


def g_faces_3d(doc, v):
	msp = doc.modelspace()
	cube = [(0, 0, 0), (4, 0, 0), (4, 4, 0), (0, 4, 0), (0, 0, 4), (4, 0, 4), (4, 4, 4), (0, 4, 4)]
	quads = [(0, 3, 2, 1), (4, 5, 6, 7), (0, 1, 5, 4), (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7)]
	for i, q in enumerate(quads):  # 3DFACE box, each face its own colour
		msp.add_3dface([cube[k] for k in q], dxfattribs={"color": 1 + i})
	for i, q in enumerate(quads[:2]):  # invisible edges
		f = msp.add_3dface([(8 + cube[k][0], cube[k][1], cube[k][2] + 6 * i) for k in q], dxfattribs={"color": 7})
		f.dxf.invisible = 5 if i == 0 else 15
	msp.add_3dface([(14, 0, 0), (18, 0, 0), (16, 4, 2)], dxfattribs={"color": 3})  # triangle: 4th point = 3rd
	pf = msp.add_polyface(dxfattribs={"color": 2})  # polyface mesh: pyramid with coloured faces
	apex = (26, 2, 5)
	base = [(24, 0, 0), (28, 0, 0), (28, 4, 0), (24, 4, 0)]
	pf.append_face(base, dxfattribs={"color": 1})
	for i in range(4):
		pf.append_face([base[i], base[(i + 1) % 4], apex], dxfattribs={"color": 2 + i})
	pf.optimize()
	pf2 = msp.add_polyface()  # polyface cube sharing vertices
	pf2.append_faces([[(32 + cube[k][0], cube[k][1], cube[k][2]) for k in q] for q in quads])
	pf2.optimize()
	m, n = 8, 10
	pm = msp.add_polymesh(size=(m, n), dxfattribs={"color": 4})  # polygon mesh: wavy sheet
	for i in range(m):
		for j in range(n):
			pm.set_mesh_vertex((i, j), (40 + i * 1.2, j * 1.2, 1.5 * math.sin(i * 0.8) * math.cos(j * 0.7)))
	pm2 = msp.add_polymesh(size=(8, 8), dxfattribs={"color": 5})  # closed in both directions: torus
	pm2.close(m_close=True, n_close=True)
	for i in range(8):
		for j in range(8):
			a, b = i / 8 * 2 * math.pi, j / 8 * 2 * math.pi
			pm2.set_mesh_vertex((i, j), (56 + (2.5 + math.cos(b)) * math.cos(a), (2.5 + math.cos(b)) * math.sin(a), math.sin(b)))
	msp.add_polyline3d([(0, 10, 0), (2, 12, 3), (4, 10, 6), (6, 12, 3)], dxfattribs={"color": 6})
	if v != R12:
		me = msp.add_mesh(dxfattribs={"color": 3})  # MESH: a cube, with an edge crease
		with me.edit_data() as md:
			md.vertices = [(70 + x, y, z) for x, y, z in cube]
			md.faces = [list(q) for q in quads]
			md.edges = [(0, 1), (1, 2)]
			md.edge_crease_values = [3.0, 3.0]
		me2 = msp.add_mesh(dxfattribs={"color": 4})  # MESH with triangles and an n-gon
		with me2.edit_data() as md:
			md.vertices = [(80, 0, 0), (84, 0, 0), (86, 3, 0), (84, 6, 0), (80, 6, 0), (78, 3, 0), (82, 3, 4)]
			md.faces = [[0, 1, 2, 3, 4, 5], [0, 1, 6], [1, 2, 6], [2, 3, 6], [3, 4, 6], [4, 5, 6], [5, 0, 6]]
		me2.dxf.subdivision_levels = 1


def g_ocs_extrusion(doc, v):
	"""Entities in an object coordinate system: the stored coordinates are OCS, the extrusion is the normal."""
	msp = doc.modelspace()
	s = math.sqrt(1 / 3)
	normals = [(0, 0, 1), (0, 0, -1), (1, 0, 0), (0, 1, 0), (s, s, s), (0.01, 0.01, 0.9999), (-0.02, 0.0, -0.9998)]
	for i, n in enumerate(normals):
		x, y = cell(i, 4, 20)
		ext = Vec3(n).normalize()
		attr = {"extrusion": ext, "color": 1 + i}
		first = len(msp)
		msp.add_circle((3, 3), 2, dxfattribs=attr)
		msp.add_arc((3, 3), 3, 20, 200, dxfattribs=attr)
		msp.add_lwpolyline([(0, 0), (6, 0), (6, 6, 0, 0, 0.5), (0, 6)], close=True, dxfattribs=dict(attr, elevation=1.0))
		msp.add_text(f"n={tuple(round(c, 2) for c in n)}", dxfattribs={"height": 0.6, "insert": (0, -1), "extrusion": ext, "color": 1 + i})
		msp.add_point((3, 3, 2), dxfattribs=attr)
		msp.add_solid([(1, 1), (2, 1), (1, 2), (2, 2)], dxfattribs=attr)
		h = msp.add_hatch(color=1 + i, dxfattribs={"extrusion": ext, "elevation": (0, 0, 0.5)})
		h.paths.add_polyline_path([(3.5, 1), (5, 1), (5, 2.5), (3.5, 2.5)], is_closed=True)
		msp.add_polyline2d([(0, 8), (3, 9), (6, 8)], dxfattribs=attr)
		move = Matrix44.translate(x, y, 0)  # move the whole cell in WCS to its grid position
		for e in list(msp)[first:]:
			e.transform(move)


def g_truecolor(doc, v):
	msp = doc.modelspace()
	for i in range(12):
		c = colors.int2rgb(int(0xFFFFFF * i / 11))
		msp.add_line((0, i * 1.5), (10, i * 1.5), dxfattribs={"true_color": colors.rgb2int((255 - i * 20, i * 20, 128))})
	for i in range(6):
		msp.add_circle((16 + i * 5, 6), 2).rgb = (50 * i, 255 - 50 * i, 100)
	lay = doc.layers.add("TRUECOLOR_LAYER")
	lay.rgb = (200, 30, 160)
	msp.add_line((0, -4), (40, -4), dxfattribs={"layer": "TRUECOLOR_LAYER"})
	msp.add_line((0, -5), (40, -5), dxfattribs={"layer": "TRUECOLOR_LAYER", "color": 1})  # ACI overrides layer's true colour
	h = msp.add_hatch()
	h.rgb = (10, 200, 220)
	h.paths.add_polyline_path([(0, -12), (8, -12), (8, -7), (0, -7)], is_closed=True)
	if v != R12:
		for i in range(5):  # transparency 0 (opaque) .. nearly clear
			e = msp.add_circle((14 + i * 3, -9), 2.2, dxfattribs={"true_color": colors.rgb2int((255, 0, 0))})
			e.transparency = i / 5
		e = msp.add_line((0, -15), (40, -15), dxfattribs={"true_color": colors.rgb2int((0, 0, 0))})
		e = msp.add_line((0, -16), (40, -16), dxfattribs={"true_color": colors.rgb2int((255, 255, 255))})
	t = msp.add_text("true colour text", dxfattribs={"height": 1.5, "insert": (0, -20)})
	t.rgb = (255, 128, 0)
	b = doc.blocks.new("TC_BLOCK")
	b.add_circle((0, 0), 1).rgb = (0, 128, 255)
	b.add_line((-1, -1), (1, 1))
	msp.add_blockref("TC_BLOCK", (50, -20)).rgb = (255, 0, 255)


def g_rays_xlines(doc, v):
	msp = doc.modelspace()
	for i, d in enumerate([(1, 0), (0, 1), (1, 1), (-1, 0.5), (0.2, -1)]):
		msp.add_ray((i * 4, 0), d, dxfattribs={"color": 1 + i})
		msp.add_xline((i * 4, -20), d, dxfattribs={"color": 1 + i})
	msp.add_line((0, -10), (20, -10))
	msp.add_circle((10, -10), 5)
	msp.add_ray((0, 0, 0), (0, 0, 1), dxfattribs={"color": 7})  # along Z: a point seen from above
	msp.add_xline((30, 0, 5), (1, 1, 1), dxfattribs={"color": 6})


def g_acis(doc, v):
	"""3DSOLID and REGION with ACIS data (SAT text up to R2010, SAB binary from R2013)."""
	from ezdxf.acis import api as acis
	from ezdxf.render import forms
	msp = doc.modelspace()
	box = forms.cube(center=False)
	box.scale(4, 4, 4)
	cyl = forms.cylinder(count=16, radius=2, top_radius=1, top_center=(0, 0, 5))
	cyl.translate(8, 2, 0)
	for k, mesh in enumerate([box, cyl]):
		solid = msp.add_3dsolid(dxfattribs={"color": 1 + k})
		acis.export_dxf(solid, [acis.body_from_mesh(mesh)])
	box2 = forms.cube(center=False)
	box2.scale(3, 2, 1)
	box2.translate(16, 0, 0)
	solid = msp.add_3dsolid(dxfattribs={"color": 3})
	acis.export_dxf(solid, [acis.body_from_mesh(box2)])
	reg = msp.add_region(dxfattribs={"color": 5})
	flat = forms.cube(center=False)
	flat.scale(4, 3, 0.0001)
	flat.translate(24, 0, 0)
	try:
		acis.export_dxf(reg, [acis.body_from_mesh(flat)])
	except Exception:
		msp.delete_entity(reg)
	msp.add_line((0, -4), (28, -4))


def _png(path, w=64, h=32):
	rows = b"".join(b"\x00" + b"".join(bytes((int(255 * x / w), int(255 * y / h), 128)) for x in range(w)) for y in range(h))
	def chunk(t, d):
		c = struct.pack(">I", len(d)) + t + d
		return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
	path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def g_images(doc, v, outdir=None):
	msp = doc.modelspace()
	if outdir is not None:
		_png(outdir / "image_sample.png")
	idef = doc.add_image_def(filename="image_sample.png", size_in_pixel=(64, 32))
	msp.add_image(idef, insert=(0, 0), size_in_units=(8, 4))
	msp.add_image(idef, insert=(12, 0), size_in_units=(8, 4), rotation=30)
	im = msp.add_image(idef, insert=(26, 0), size_in_units=(8, 4))
	im.set_boundary_path([(8, 4), (56, 4), (32, 28)])  # clipped to a triangle (pixel coordinates)
	im = msp.add_image(idef, insert=(38, 0), size_in_units=(8, 4))
	im.dxf.brightness = 70
	im.dxf.contrast = 30
	im.dxf.fade = 40
	msp.add_image(idef, insert=(50, 0), size_in_units=(2, 1))
	missing = doc.add_image_def(filename="no_such_file.jpg", size_in_pixel=(100, 100))
	msp.add_image(missing, insert=(0, -8), size_in_units=(6, 6))
	for i in range(5):
		msp.add_line((0, -20 + i), (30, -20 + i), dxfattribs={"color": 1 + i})
	w = msp.add_wipeout([(5, -22), (15, -22), (15, -17), (5, -17)])  # masks the lines under it
	w2 = msp.add_wipeout([(20, -22), (28, -22), (24, -16)])
	try:
		ul = doc.add_underlay_def(filename="plan.pdf", name="1", fmt="pdf")
		msp.add_underlay(ul, insert=(40, -20), scale=(0.1, 0.1, 0.1))
	except Exception:
		pass


def g_paperspace(doc, v):
	msp = doc.modelspace()
	for i in range(8):  # model content
		msp.add_circle((i * 10, 5), 4, dxfattribs={"color": 1 + i})
		msp.add_text(f"M{i}", dxfattribs={"height": 2, "insert": (i * 10 - 1, 4.5)})
	msp.add_lwpolyline([(0, 0), (70, 0), (70, 10), (0, 10)], close=True) if v != R12 else msp.add_polyline2d([(0, 0), (70, 0), (70, 10), (0, 10)], close=True)
	psp = doc.layout("Layout1") if v != R12 else doc.paperspace()
	psp.add_lwpolyline([(0, 0), (420, 0), (420, 297), (0, 297)], close=True) if v != R12 else psp.add_polyline2d([(0, 0), (420, 0), (420, 297), (0, 297)], close=True)
	psp.add_text("PAPER SPACE TITLE BLOCK", dxfattribs={"height": 10, "insert": (20, 270), "color": 7})
	psp.add_line((0, 40), (420, 40))
	vp = psp.add_viewport(center=(120, 150), size=(200, 100), view_center_point=(35, 5), view_height=15)
	vp.dxf.status = 1
	vp2 = psp.add_viewport(center=(330, 150), size=(120, 100), view_center_point=(70, 5), view_height=20)
	vp2.dxf.status = 2
	vp3 = psp.add_viewport(center=(120, 70), size=(80, 40), view_center_point=(5, 5), view_height=10)
	vp3.dxf.status = 3
	if v != R12:
		lay = doc.layouts.new("Second layout")
		lay.add_text("second sheet", dxfattribs={"height": 8, "insert": (10, 10)})
		lay.add_viewport(center=(100, 100), size=(100, 100), view_center_point=(35, 5), view_height=70)
		lay.add_circle((200, 100), 20)
		lay2 = doc.layouts.new("A3 portrait")
		lay2.page_setup(size=(297, 420), margins=(10, 10, 10, 10), units="mm", rotation=0)
		lay2.add_viewport(center=(148, 200), size=(260, 300), view_center_point=(35, 5), view_height=120)


CODEPAGE_TEXT = {
	"ANSI_1251": ("cp1251", "\u041f\u0440\u0438\u0432\u0435\u0442, \u043c\u0438\u0440! \u0427\u0435\u0440\u0442\u0451\u0436 \u21161", "\u0421\u043b\u043e\u0439\u0420\u0443\u0441"),
	"ANSI_1252": ("cp1252", "Gr\u00f6\u00dfe \u00fcber \u00e9t\u00e9 \u00e0 la fa\u00e7on \u20ac 5\u00b0", "Couche_\u00e9t\u00e9"),
	"ANSI_1250": ("cp1250", "P\u0159\u00edli\u0161 \u017elu\u0165ou\u010dk\u00fd k\u016f\u0148", "Vrstva_\u010d"),
	"ANSI_1253": ("cp1253", "\u0393\u03b5\u03b9\u03ac \u03c3\u03bf\u03c5 \u039a\u03cc\u03c3\u03bc\u03b5", "\u0395\u03c0\u03af\u03c0\u03b5\u03b4\u03bf"),
	"ANSI_1254": ("cp1254", "\u0130stanbul \u015f\u011f\u0131", "Katman_\u015f"),
	"ANSI_932": ("cp932", "\u65e5\u672c\u8a9e\u306e\u56f3\u9762 \u30c6\u30b9\u30c8 \uff71\uff72\uff73", "\u30ec\u30a4\u30e4\u30fc"),
	"ANSI_936": ("gbk", "\u4e2d\u6587\u56fe\u7eb8\u6d4b\u8bd5", "\u56fe\u5c42"),
	"ANSI_949": ("cp949", "\ud55c\uae00 \ub3c4\uba74", "\ub808\uc774\uc5b4"),
	"ANSI_950": ("cp950", "\u4e2d\u6587\u6e2c\u8a66\u5716\u9762", "\u5716\u5c64"),
}


def make_codepage(doc, v, page):
	py, text, layer = CODEPAGE_TEXT[page]
	doc.encoding = py
	doc.header["$DWGCODEPAGE"] = page
	msp = doc.modelspace()
	doc.layers.add(layer, color=3)
	doc.styles.add("TXT_" + page[5:], font="txt")
	msp.add_text(text, dxfattribs={"height": 2, "insert": (0, 0), "layer": layer})
	msp.add_text("ASCII neighbour", dxfattribs={"height": 1, "insert": (0, -3)})
	blk = doc.blocks.new("blk_" + layer)  # non-ASCII block name
	blk.add_text(text, dxfattribs={"height": 1})
	blk.add_attdef("TAG", (0, -2), text, dxfattribs={"height": 1})
	msp.add_blockref(blk.name, (0, -8)).add_auto_attribs({"TAG": text})
	msp.add_circle((30, 0), 2, dxfattribs={"layer": layer})
	if v != R12:
		msp.add_mtext(text + "\\P" + text, dxfattribs={"insert": (0, -14), "char_height": 1.5, "width": 30, "layer": layer})
#endregion


GROUPS = [  # name, function, versions
	("basic_shapes", g_basic_shapes, ALL),
	("layers_linetypes", g_layers_linetypes, ALL),
	("blocks_nested", g_blocks_nested, ALL),
	("attribs", g_attribs, ALL),
	("minsert", g_minsert, ALL),
	("inserts_transform", g_inserts_transform, ALL),
	("lwpolyline", g_lwpolyline, MODERN),
	("polyline2d", g_polyline2d, ALL),
	("splines", g_splines, MODERN),
	("ellipses", g_ellipses, MODERN),
	("text", g_text, ALL),
	("mtext", g_mtext, MODERN),
	("dimensions", g_dimensions, MODERN),
	("hatches", g_hatches, MODERN),
	("leaders", g_leaders, MODERN),
	("faces_3d", g_faces_3d, ALL),
	("ocs_extrusion", g_ocs_extrusion, MODERN),
	("truecolor", g_truecolor, MODERN),
	("rays_xlines", g_rays_xlines, MODERN),
	("acis_solids", g_acis, [R2000, R2007, R2013, R2018]),
	("images", g_images, MODERN),
	("paperspace", g_paperspace, ALL),
]
BINARY = [("basic_shapes", R12), ("blocks_nested", R12), ("faces_3d", R12), ("text", R12), ("polyline2d", R12), ("layers_linetypes", R12),
          ("blocks_nested", R2018), ("attribs", R2018), ("splines", R2018), ("hatches", R2018), ("mtext", R2018), ("lwpolyline", R2018),
          ("dimensions", R2018), ("ellipses", R2018), ("acis_solids", R2018), ("minsert", R2000), ("inserts_transform", R2000)]


def new_doc(ver):
	return ezdxf.new(ver, setup=True)


def summary(path:Path):
	try:
		d = ezdxf.readfile(str(path))
	except Exception as e:  # noqa
		return f"{path.name:46s} READ ERROR {e}"
	cnt = {}
	for lay in d.layouts:
		for e in lay:
			cnt[e.dxftype()] = cnt.get(e.dxftype(), 0) + 1
	top = ", ".join(f"{k}:{n}" for k, n in sorted(cnt.items(), key=lambda kv: -kv[1])[:7])
	kind = "bin" if path.read_bytes()[:18] == b"AutoCAD Binary DXF" else "asc"
	return f"{path.name:46s} {d.acad_release:>7s} {kind} {path.stat().st_size:>11,d} B  {sum(cnt.values()):>7,d} ent  {top}"


def write(doc, path:Path, fmt="asc"):
	doc.saveas(str(path), fmt=fmt)
	print(summary(path), flush=True)


def make_big(out:Path, target_mb:float):
	"""R2018 drawing with random LINEs, ARCs and LWPOLYLINEs on 40 layers; sized by trial to about target_mb."""
	rnd = random.Random(20260908)
	doc = ezdxf.new(R2018)
	for i in range(40):
		doc.layers.add(f"LAYER_{i:02d}", color=1 + i % 255)
	msp = doc.modelspace()
	lay = lambda: f"LAYER_{rnd.randrange(40):02d}"
	# One LINE is ~185 B, an ARC ~215 B and a 10-vertex LWPOLYLINE ~420 B in this form.
	n_unit = int(target_mb * 1e6 / (6 * 185 + 2 * 215 + 1 * 420))
	for _ in range(n_unit):
		cx, cy = rnd.uniform(0, 5000), rnd.uniform(0, 3000)
		for _ in range(6):
			msp.add_line((cx + rnd.uniform(-20, 20), cy + rnd.uniform(-20, 20)), (cx + rnd.uniform(-20, 20), cy + rnd.uniform(-20, 20)), dxfattribs={"layer": lay()})
		for _ in range(2):
			msp.add_arc((cx, cy), rnd.uniform(1, 15), rnd.uniform(0, 360), rnd.uniform(0, 360), dxfattribs={"layer": lay()})
		x, y = cx, cy
		pts = []
		for _ in range(10):
			x += rnd.uniform(-3, 3); y += rnd.uniform(-3, 3)
			pts.append((x, y, 0, 0, rnd.choice((0, 0, 0, 0.3, -0.3))))
		msp.add_lwpolyline(pts, format="xyseb", dxfattribs={"layer": lay()})
	doc.saveas(str(out))
	print(summary(out), flush=True)


def main():
	args = [a for a in sys.argv[1:] if not a.startswith("--")]
	if not args:
		sys.exit(__doc__ or "usage: make-dxf-samples.py <output-folder> [--big-mb N] [--no-big]")
	out = Path(args[0])
	out.mkdir(parents=True, exist_ok=True)
	big_mb = 45.0
	if "--big-mb" in sys.argv:
		big_mb = float(sys.argv[sys.argv.index("--big-mb") + 1])
	for name, fn, versions in GROUPS:
		for ver in versions:
			doc = new_doc(ver)
			try:
				if fn is g_images:
					fn(doc, ver, outdir=out)
				else:
					fn(doc, ver)
			except Exception as e:  # keep going: report which group/version failed
				print(f"!! {name}_{ver}: {type(e).__name__}: {e}", flush=True)
				continue
			write(doc, out / f"{name}_{ver}.dxf")
	by_name = {n: f for n, f, _ in GROUPS}
	for name, ver in BINARY:
		doc = new_doc(ver)
		try:
			by_name[name](doc, ver)
		except Exception as e:
			print(f"!! {name}_{ver}_bin: {type(e).__name__}: {e}", flush=True)
			continue
		write(doc, out / f"{name}_{ver}_bin.dxf", fmt="bin")
	extra(out)
	if "--no-big" not in sys.argv:
		make_big(out / f"big_lines_arcs_lwpolylines_R2018.dxf", big_mb)


def extra(out:Path):
	# code pages: R12 (always a code page) and R2000 (code page until R2004), each in its own file
	for page in CODEPAGE_TEXT:
		doc = new_doc(R12)
		make_codepage(doc, R12, page)
		write(doc, out / f"codepage_{page[5:]}_R12.dxf")
	for page in ("ANSI_1251", "ANSI_932"):
		doc = new_doc(R2000)
		make_codepage(doc, R2000, page)
		write(doc, out / f"codepage_{page[5:]}_R2000.dxf")
	# binary DXF with a code page
	doc = new_doc(R12)
	make_codepage(doc, R12, "ANSI_1251")
	write(doc, out / "codepage_1251_R12_bin.dxf", fmt="bin")
	# R2007+ is UTF-8, whatever $DWGCODEPAGE says
	doc = new_doc(R2018)
	msp = doc.modelspace()
	msp.add_text("UTF-8 in R2018: \u041f\u0440\u0438\u0432\u0435\u0442 \u65e5\u672c\u8a9e \u00e9", dxfattribs={"height": 2})
	path = out / "codepage_utf8_R2018.dxf"
	doc.saveas(str(path))
	path.write_bytes(path.read_bytes().replace(b"ANSI_1252", b"ANSI_1251", 1))  # ezdxf always writes ANSI_1252 for UTF-8 files; real Russian AutoCAD writes its own page
	print(summary(path), "(header says ANSI_1251, text is UTF-8)")
	# 3D-only: nothing lies in the XY plane, no entity is 2D
	for ver in (R2000, R2018):
		doc = new_doc(ver)
		msp = doc.modelspace()
		pts = [(5 * math.cos(t / 4), 5 * math.sin(t / 4), t * 0.4) for t in range(80)]
		msp.add_polyline3d(pts, dxfattribs={"color": 1})
		for i in range(0, 16):
			a0, a1 = i * math.pi / 8, (i + 1) * math.pi / 8
			for j in range(6):
				b0, b1 = j * math.pi / 6, (j + 1) * math.pi / 6
				p = lambda a, b: (12 + 4 * math.sin(b) * math.cos(a), 4 * math.sin(b) * math.sin(a), 4 * math.cos(b) + 20)
				msp.add_3dface([p(a0, b0), p(a1, b0), p(a1, b1), p(a0, b1)], dxfattribs={"color": 1 + (i + j) % 7})
		for i in range(10):
			msp.add_line((0, 0, i * 2), (10, 3, i * 2 + 1), dxfattribs={"color": 3})
			msp.add_point((i, -i, i * 3))
		if ver != R12:
			me = msp.add_mesh()
			with me.edit_data() as md:
				md.vertices = [(30, 0, 0), (36, 0, 0), (36, 0, 6), (30, 0, 6), (33, 6, 3)]
				md.faces = [[0, 1, 2, 3], [0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]]
		write(doc, out / f"only_3d_{ver}.dxf")
	# outlier framing: a tidy drawing near the origin plus one stray entity 1e6 units away
	for ver in (R12, R2000, R2018):
		doc = new_doc(ver)
		msp = doc.modelspace()
		for i in range(10):
			msp.add_circle((i * 10, 5), 4)
			msp.add_text(f"{i}", dxfattribs={"height": 2, "insert": (i * 10 - 1, 4)})
		msp.add_line((0, 0), (100, 0))
		msp.add_line((1000000, 1000000), (1000000, 1000001), dxfattribs={"color": 1})
		doc.header["$EXTMIN"] = (0, 0, 0)
		doc.header["$EXTMAX"] = (1000000, 1000001, 0)
		write(doc, out / f"far_outlier_{ver}.dxf")
	doc = new_doc(R2018)  # header extents left at AutoCAD's "empty" values 1e20 / -1e20
	msp = doc.modelspace()
	for i in range(10):
		msp.add_circle((i * 10, 5), 4)
	msp.add_point((-2500000, 3000000))
	doc.header["$EXTMIN"] = (1e20, 1e20, 1e20)
	doc.header["$EXTMAX"] = (-1e20, -1e20, -1e20)
	write(doc, out / "far_outlier_empty_extents_R2018.dxf")
	# empty drawings
	for ver in (R12, R2000, R2018):
		write(new_doc(ver), out / f"empty_{ver}.dxf")
	doc = ezdxf.new(R2018, setup=False)
	write(doc, out / "empty_bare_R2018.dxf")  # no standard linetypes or styles at all
	doc = new_doc(R2018)  # block definitions only, nothing in model space
	b = doc.blocks.new("UNUSED")
	b.add_circle((0, 0), 5)
	write(doc, out / "empty_modelspace_blocks_only_R2018.dxf")


if __name__ == "__main__":
	main()
