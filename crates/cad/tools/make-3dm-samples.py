# Makes the generated Rhino .3dm test files for Slerp's cad crate with the rhino3dm Python wheel (MIT licensed,
# McNeel's openNURBS compiled for Python).  The files are our own work and are released as CC0-1.0.
# Each group is written in the file versions 4 to 8 with File3dm.Write(path, version):
#   mesh_vertex_colors      meshes with per-vertex colours, with and without stored normals, and a lone triangle
#   mesh_material_layer     layer colours and materials: colour/material from layer, from object, a transparent one
#   brep_box_no_render_mesh a box Brep with no meshes at all (a viewer must tessellate it itself)
#   brep_box_render_mesh    the same box with a render mesh stored on every face
#   extrusion               box, cylinder and pipe Extrusion objects, and a swept-up profile Extrusion
#   curves_points           line, arc, circle, polyline, NURBS curve, points, point cloud, text dot
#   blocks                  an instance definition used by three instance references (moved, rotated, scaled)
#   brep_sphere_cylinder    a sphere and a cylinder Brep with render meshes, to test curved trimmed faces
# rhino3dm cannot create meshes from Breps (that needs Rhino itself), so Breps only get a render mesh where this
# script builds one by hand.
#
# Run (needs `pip install rhino3dm`; a venv in local/cad-work/venv, say):
#   python make-3dm-samples.py <output-folder>
# Prints one line per file: name, header version, size, object types.

import math, os, sys
from pathlib import Path

import rhino3dm as r

VERSIONS = [4, 5, 6, 7, 8]
KEEP = []  # BrepFace.SetMesh() shares the mesh with the face; freeing the Python object too crashes rhino3dm, so meshes live until exit


def box_mesh(x0, y0, z0, x1, y1, z1, colors=False, normals=True) -> r.Mesh:
	"""Box of six quads with 24 vertices (4 per face, so each face has its own flat normals)."""
	m = r.Mesh()
	corners = [(x0, y0, z0), (x1, y0, z0), (x1, y1, z0), (x0, y1, z0), (x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1)]
	faces = [(0, 3, 2, 1), (4, 5, 6, 7), (0, 1, 5, 4), (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7)]
	face_normals = [(0, 0, -1), (0, 0, 1), (0, -1, 0), (1, 0, 0), (0, 1, 0), (-1, 0, 0)]
	palette = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (255, 0, 255), (0, 255, 255)]
	for f, (quad, n) in enumerate(zip(faces, face_normals)):
		base = len(m.Vertices)
		for k, ci in enumerate(quad):
			m.Vertices.Add(*corners[ci])
			if colors:  # each corner gets a different shade of the face colour
				c = palette[f]
				m.VertexColors.Add(int(c[0] * (0.4 + 0.2 * k)), int(c[1] * (0.4 + 0.2 * k)), int(c[2] * (0.4 + 0.2 * k)))
		m.Faces.AddFace(base, base + 1, base + 2, base + 3)
	if normals:
		m.Normals.ComputeNormals()  # four own vertices per face, so the normals come out flat
	m.Compact()
	return m


def grid_mesh(n=12, size=10.0, colored=True) -> r.Mesh:
	"""A wavy n x n grid of triangles with a rainbow of vertex colours."""
	m = r.Mesh()
	for j in range(n + 1):
		for i in range(n + 1):
			x, y = i * size / n, j * size / n
			m.Vertices.Add(x, y, math.sin(x * 0.8) * math.cos(y * 0.8))
			if colored:
				h = (i + j) / (2 * n)
				m.VertexColors.Add(int(255 * h), int(255 * (1 - abs(2 * h - 1))), int(255 * (1 - h)))
	for j in range(n):
		for i in range(n):
			a = j * (n + 1) + i
			m.Faces.AddFace(a, a + 1, a + n + 2)
			m.Faces.AddFace(a, a + n + 2, a + n + 1)
	m.Normals.ComputeNormals()
	m.Compact()
	return m


def new_model(version):
	model = r.File3dm()
	model.Settings.ModelUnitSystem = r.UnitSystem.Millimeters
	model.ApplicationName = "Slerp sample generator"
	default = r.Layer()
	default.Name = "Default"
	default.Color = (0, 0, 0, 255)
	model.Layers.Add(default)  # index 0, which attrs() uses unless told otherwise
	return model


def attrs(model, layer=0, **kw):
	a = r.ObjectAttributes()
	a.LayerIndex = layer
	for k, v in kw.items():
		setattr(a, k, v)
	return a


def add_layer(model, name, rgb, material_index=-1, visible=True):
	lay = r.Layer()
	lay.Name = name
	lay.Color = (*rgb, 255)
	lay.Visible = visible
	if material_index >= 0:
		lay.RenderMaterialIndex = material_index
	return model.Layers.Add(lay)


def add_material(model, name, rgb, transparency=0.0, shine=0.3):
	mat = r.Material()
	mat.Name = name
	mat.DiffuseColor = (*rgb, 255)
	mat.AmbientColor = (*rgb, 255)
	mat.Transparency = transparency
	mat.Shine = shine * 255.0
	return model.Materials.Add(mat)


def g_mesh_vertex_colors(model):
	model.Objects.AddMesh(box_mesh(0, 0, 0, 5, 5, 5, colors=True), attrs(model))
	model.Objects.AddMesh(grid_mesh(), attrs(model))
	model.Objects.AddMesh(box_mesh(8, 0, 0, 12, 4, 4, normals=False), attrs(model))  # no normals stored
	tri = r.Mesh()  # a single triangle with colours, normals absent
	for p, c in [((0, 8, 0), (255, 0, 0)), ((4, 8, 0), (0, 255, 0)), ((2, 12, 0), (0, 0, 255))]:
		tri.Vertices.Add(*p)
		tri.VertexColors.Add(*c)
	tri.Faces.AddFace(0, 1, 2)
	model.Objects.AddMesh(tri, attrs(model))


def g_mesh_material_layer(model):
	red_mat = add_material(model, "RedPlastic", (200, 30, 30), shine=0.8)
	blue_mat = add_material(model, "BlueGlass", (40, 80, 220), transparency=0.5, shine=0.9)
	green_mat = add_material(model, "GreenMatte", (40, 180, 70), shine=0.0)
	painted = add_layer(model, "Painted", (230, 140, 20), material_index=red_mat)
	plain = add_layer(model, "PlainLayerColour", (30, 160, 200))
	child = add_layer(model, "Painted::Child", (160, 40, 200))
	FromLayer, FromObject = r.ObjectColorSource.ColorFromLayer, r.ObjectColorSource.ColorFromObject
	MFromLayer, MFromObject = r.ObjectMaterialSource.MaterialFromLayer, r.ObjectMaterialSource.MaterialFromObject
	model.Objects.AddMesh(box_mesh(0, 0, 0, 4, 4, 4), attrs(model, painted, ColorSource=FromLayer, MaterialSource=MFromLayer))  # layer material
	model.Objects.AddMesh(box_mesh(6, 0, 0, 10, 4, 4), attrs(model, painted, ColorSource=FromObject, ObjectColor=(255, 255, 0, 255), MaterialSource=MFromObject, MaterialIndex=green_mat))
	model.Objects.AddMesh(box_mesh(12, 0, 0, 16, 4, 4), attrs(model, plain, ColorSource=FromLayer, MaterialSource=MFromLayer))  # no material: layer colour
	model.Objects.AddMesh(box_mesh(18, 0, 0, 22, 4, 4), attrs(model, plain, MaterialSource=MFromObject, MaterialIndex=blue_mat))  # transparent
	model.Objects.AddMesh(box_mesh(0, 6, 0, 4, 10, 4), attrs(model, child, ColorSource=FromLayer, MaterialSource=MFromLayer))  # child layer
	model.Objects.AddMesh(box_mesh(6, 6, 0, 10, 10, 4, colors=True), attrs(model, plain, ColorSource=FromObject, ObjectColor=(10, 10, 10, 255)))  # vertex colours vs object colour
	hidden = add_layer(model, "HiddenLayer", (255, 0, 0), visible=False)
	model.Objects.AddMesh(box_mesh(12, 6, 0, 16, 10, 4), attrs(model, hidden))
	a = attrs(model, plain)
	a.Visible = False
	model.Objects.AddMesh(box_mesh(18, 6, 0, 22, 10, 4), a)  # hidden object on a visible layer


def box_brep():
	return r.Brep.CreateFromBox(r.Box(r.BoundingBox(0, 0, 0, 6, 4, 3)))


def g_brep_box_no_render_mesh(model):
	add_layer(model, "Boxes", (120, 120, 120))
	model.Objects.AddBrep(box_brep(), attrs(model, 1))
	b2 = box_brep()
	b2.Translate(r.Vector3d(10, 0, 0))
	model.Objects.AddBrep(b2, attrs(model, 0, ColorSource=r.ObjectColorSource.ColorFromObject, ObjectColor=(220, 40, 40, 255)))


def g_brep_box_render_mesh(model):
	b = box_brep()
	for i in range(len(b.Faces)):
		face = b.Faces[i]
		bb = face.GetBoundingBox()
		m = r.Mesh()  # the face is planar: two triangles over its four corners
		pts = [(bb.Min.X, bb.Min.Y, bb.Min.Z), (bb.Max.X, bb.Min.Y, bb.Min.Z), (bb.Max.X, bb.Max.Y, bb.Min.Z), (bb.Min.X, bb.Max.Y, bb.Min.Z)]
		if abs(bb.Max.X - bb.Min.X) < 1e-9:
			pts = [(bb.Min.X, bb.Min.Y, bb.Min.Z), (bb.Min.X, bb.Max.Y, bb.Min.Z), (bb.Min.X, bb.Max.Y, bb.Max.Z), (bb.Min.X, bb.Min.Y, bb.Max.Z)]
		elif abs(bb.Max.Y - bb.Min.Y) < 1e-9:
			pts = [(bb.Min.X, bb.Min.Y, bb.Min.Z), (bb.Max.X, bb.Min.Y, bb.Min.Z), (bb.Max.X, bb.Min.Y, bb.Max.Z), (bb.Min.X, bb.Min.Y, bb.Max.Z)]
		for p in pts:
			m.Vertices.Add(*p)
		m.Faces.AddFace(0, 1, 2, 3)
		m.Normals.ComputeNormals()
		m.Compact()
		KEEP.append(m)
		face.SetMesh(m, r.MeshType.Render)
	model.Objects.AddBrep(b, attrs(model))


def g_extrusion(model):
	pl = r.Polyline(5)
	for p in [(0, 0, 0), (6, 0, 0), (6, 3, 0), (0, 3, 0), (0, 0, 0)]:
		pl.Add(*p)
	model.Objects.AddExtrusion(r.Extrusion.Create(pl.ToPolylineCurve(), 4.0, True), attrs(model))  # capped profile extrusion
	cyl = r.Extrusion.CreateCylinderExtrusion(r.Cylinder(r.Circle(r.Point3d(12, 0, 0), 2.0), 5.0), True, True)
	model.Objects.AddExtrusion(cyl, attrs(model))
	bx = r.Extrusion.CreateBoxExtrusion(r.Box(r.BoundingBox(20, 0, 0, 23, 3, 3)), True)
	model.Objects.AddExtrusion(bx, attrs(model))
	pipe = r.Extrusion.CreatePipeExtrusion(r.Cylinder(r.Circle(r.Point3d(30, 0, 0), 2.5), 4.0), 1.0, True, True)
	model.Objects.AddExtrusion(pipe, attrs(model))


def g_curves_points(model):
	model.Objects.AddLine(r.Point3d(0, 0, 0), r.Point3d(10, 5, 2), attrs(model))
	model.Objects.AddArc(r.Arc(r.Circle(r.Point3d(0, 0, 0), 4.0), math.pi * 1.3), attrs(model))
	model.Objects.AddCircle(r.Circle(r.Point3d(12, 0, 0), 3.0), attrs(model))
	pl = r.Polyline(4)
	for p in [(0, 8, 0), (3, 12, 1), (6, 8, 2), (9, 12, 3)]:
		pl.Add(*p)
	model.Objects.AddPolyline(pl, attrs(model, ColorSource=r.ObjectColorSource.ColorFromObject, ObjectColor=(255, 100, 0, 255)))
	nc = r.NurbsCurve(3, False, 4, 6)  # dimension, rational, order (degree + 1), control point count
	for i, p in enumerate([(14, 8, 0), (16, 12, 0), (18, 8, 0), (20, 12, 0), (22, 8, 0), (24, 12, 0)]):
		nc.Points[i] = r.Point4d(*p, 1.0)
	for i, k in enumerate([0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0, 3.0]):  # clamped cubic: order + points - 2 knots in openNURBS
		if i < len(nc.Knots):
			nc.Knots[i] = k
	model.Objects.AddCurve(nc, attrs(model))
	model.Objects.AddPoint(r.Point3d(0, -4, 0), attrs(model))
	model.Objects.AddPoint(r.Point3d(2, -4, 0), attrs(model, ColorSource=r.ObjectColorSource.ColorFromObject, ObjectColor=(255, 0, 0, 255)))
	pc = r.PointCloud()
	for i in range(200):
		a = i * 0.31
		pc.Add(r.Point3d(10 + math.cos(a) * (1 + i * 0.02), -4 + math.sin(a) * (1 + i * 0.02), i * 0.02), r.Vector3d(0, 0, 1), (int(255 * i / 200), 80, 255 - int(255 * i / 200), 255))
	model.Objects.AddPointCloud(pc, attrs(model))
	model.Objects.AddTextDot("text dot", r.Point3d(20, -4, 0), attrs(model))


def g_blocks(model):
	lines = [r.LineCurve(r.Point3d(0, 0, 0), r.Point3d(4, 0, 0)), r.LineCurve(r.Point3d(4, 0, 0), r.Point3d(4, 2, 0)), r.LineCurve(r.Point3d(0, 0, 0), r.Point3d(0, 2, 0))]
	index = model.InstanceDefinitions.Add("L-shape", "an L made of three lines", "", "", r.Point3d(0, 0, 0), tuple(lines), tuple(attrs(model) for _ in lines))
	idef = model.InstanceDefinitions[index]
	for dx, ang, s in [(0, 0, 1.0), (8, 30, 1.0), (18, 90, 2.0)]:
		xf = r.Transform.Multiply(r.Transform.Translation(dx, 0, 0), r.Transform.Multiply(r.Transform.Rotation(math.radians(ang), r.Vector3d(0, 0, 1), r.Point3d(0, 0, 0)), r.Transform.Scale(r.Point3d(0, 0, 0), s)))
		model.Objects.AddInstanceObject(r.InstanceReference(idef.Id, xf), attrs(model))


def g_brep_sphere_cylinder(model):
	sph = r.Brep.CreateFromSphere(r.Sphere(r.Point3d(0, 0, 0), 3.0))
	cyl = r.Brep.CreateFromCylinder(r.Cylinder(r.Circle(r.Point3d(10, 0, 0), 2.0), 6.0), True, True)
	for b in (sph, cyl):
		for i in range(len(b.Faces)):  # a coarse grid mesh over each face's UV domain
			face = b.Faces[i]
			du, dv = face.Domain(0), face.Domain(1)
			n = 10
			m = r.Mesh()
			for j in range(n + 1):
				for k in range(n + 1):
					p = face.PointAt(du.T0 + (du.T1 - du.T0) * k / n, dv.T0 + (dv.T1 - dv.T0) * j / n)
					m.Vertices.Add(p.X, p.Y, p.Z)
			for j in range(n):
				for k in range(n):
					a = j * (n + 1) + k
					m.Faces.AddFace(a, a + 1, a + n + 2, a + n + 1)
			m.Normals.ComputeNormals()
			m.Compact()
			KEEP.append(m)
			face.SetMesh(m, r.MeshType.Render)
		model.Objects.AddBrep(b, attrs(model))


GROUPS = [
	("mesh_vertex_colors", g_mesh_vertex_colors),
	("mesh_material_layer", g_mesh_material_layer),
	("brep_box_no_render_mesh", g_brep_box_no_render_mesh),
	("brep_box_render_mesh", g_brep_box_render_mesh),
	("extrusion", g_extrusion),
	("curves_points", g_curves_points),
	("blocks", g_blocks),
	("brep_sphere_cylinder", g_brep_sphere_cylinder),
]


def summary(path:Path) -> str:
	with open(path, "rb") as f:
		head = f.read(32)
	ver = head[24:32].decode("latin1").strip() if head[:24] == b"3D Geometry File Format " else "NOT 3DM"
	back = r.File3dm.Read(str(path))
	if back is None:
		return f"{path.name:46s} v{ver:>3s} {path.stat().st_size:>9,d} B  READ BACK FAILED"
	counts = {}
	render_meshes = 0
	for o in back.Objects:
		g = o.Geometry
		t = type(g).__name__
		counts[t] = counts.get(t, 0) + 1
		if t == "Brep":
			render_meshes += sum(1 for i in range(len(g.Faces)) if g.Faces[i].GetMesh(r.MeshType.Render) is not None)
	extra = f"  render meshes on {render_meshes} faces" if render_meshes else ""
	return f"{path.name:46s} v{ver:>3s} {path.stat().st_size:>9,d} B  " + ", ".join(f"{k}:{v}" for k, v in sorted(counts.items())) + extra


def main():
	if len(sys.argv) < 2:
		sys.exit("usage: make-3dm-samples.py <output-folder>")
	out = Path(sys.argv[1])
	out.mkdir(parents=True, exist_ok=True)
	for name, fn in GROUPS:
		for version in VERSIONS:
			model = new_model(version)
			try:
				fn(model)
			except Exception as e:  # report and go on
				print(f"!! {name}_v{version}: {type(e).__name__}: {e}")
				continue
			path = out / f"{name}_v{version}.3dm"
			if not model.Write(str(path), version):
				print(f"!! {name}_v{version}: Write failed")
				continue
			print(summary(path))


if __name__ == "__main__":
	main()
	sys.stdout.flush()
	os._exit(0)  # skip interpreter teardown: freeing the face meshes there crashes the wheel
