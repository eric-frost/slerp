# Builds small .blend files that exercise slerp-blend's modifiers, normals,
# collections and visibility, for its tests:
#
#   blender -b --factory-startup --python-exit-code 1 \
#       --python crates/blend/tools/make-test-scenes.py -- OUT_DIR
#
# writes OUT_DIR/modifiers.blend and OUT_DIR/scene.blend (zstd-compressed,
# Blender's default), plus OUT_DIR/modifiers-plain.blend uncompressed.  Run
# tools/blender-reference.py on them for Blender's own counts.  The files
# are made from scratch here, so they carry no third-party licence.

import bpy
import bmesh
import math
import os
import sys

out = sys.argv[sys.argv.index("--") + 1:][0]
os.makedirs(out, exist_ok=True)


def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def cube(name, loc, size=1.0):
    bpy.ops.mesh.primitive_cube_add(size=size, location=loc)
    ob = bpy.context.active_object
    ob.name = name
    return ob


def plane(name, loc, size=1.0):
    bpy.ops.mesh.primitive_plane_add(size=size, location=loc)
    ob = bpy.context.active_object
    ob.name = name
    return ob


def save(name, compress=True):
    bpy.ops.wm.save_as_mainfile(filepath=os.path.join(out, name), compress=compress)


# 1. Modifiers, one object each, spread along X.
reset()
x = 0.0

ob = cube("Mirror", (x, 0, 0))
ob.data.transform(__import__("mathutils").Matrix.Translation((0.6, 0, 0)))
m = ob.modifiers.new("Mirror", "MIRROR")
m.use_mirror_merge = True
x += 4

ob = cube("Array", (x, 0, 0), 0.5)
m = ob.modifiers.new("Array", "ARRAY")
m.count = 4
m.relative_offset_displace = (1.25, 0, 0)
x += 4

ob = plane("ArrayMerge", (x, 0, 0))
m = ob.modifiers.new("Array", "ARRAY")
m.count = 3
m.relative_offset_displace = (1.0, 0, 0)
m.use_merge_vertices = True
x += 4

ob = cube("Subsurf", (x, 0, 0))
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.levels = 2
bpy.ops.object.shade_smooth()
x += 4

ob = cube("SubsurfCrease", (x, 0, 0))
bm = bmesh.new()
bm.from_mesh(ob.data)
crease = bm.edges.layers.float.new("crease_edge")
for e in bm.edges[:4]:
    e[crease] = 1.0
bm.to_mesh(ob.data)
bm.free()
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.levels = 3
x += 4

ob = plane("SubsurfCorners", (x, 0, 0))
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.levels = 2
m.boundary_smooth = "PRESERVE_CORNERS"
x += 4

ob = cube("Simple", (x, 0, 0))
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.subdivision_type = "SIMPLE"
m.levels = 1
x += 4

ob = plane("Solidify", (x, 0, 0))
m = ob.modifiers.new("Solidify", "SOLIDIFY")
m.thickness = 0.2
x += 4

bpy.ops.mesh.primitive_cylinder_add(vertices=12, location=(x, 0, 0))
ob = bpy.context.active_object
ob.name = "Triangulate"
ob.modifiers.new("Triangulate", "TRIANGULATE")
x += 4

bpy.ops.mesh.primitive_grid_add(x_subdivisions=4, y_subdivisions=4, location=(x, 0, 0))
ob = bpy.context.active_object
ob.name = "Weld"
bm = bmesh.new()
bm.from_mesh(ob.data)
bmesh.ops.split_edges(bm, edges=bm.edges[:])
bm.to_mesh(ob.data)
bm.free()
m = ob.modifiers.new("Weld", "WELD")
m.merge_threshold = 0.001
x += 4

bpy.ops.mesh.primitive_cylinder_add(vertices=16, location=(x, 0, 0))
ob = bpy.context.active_object
ob.name = "EdgeSplit"
bpy.ops.object.shade_smooth()
m = ob.modifiers.new("EdgeSplit", "EDGE_SPLIT")
m.split_angle = math.radians(30)
x += 4

bpy.ops.mesh.primitive_cylinder_add(vertices=16, location=(x, 0, 0))
ob = bpy.context.active_object
ob.name = "AutoSmooth"
bpy.ops.object.shade_auto_smooth(angle=math.radians(30))
x += 4

ob = plane("Stack", (x, 0, 0))
ob.data.transform(__import__("mathutils").Matrix.Translation((0.6, 0, 0)))
ob.modifiers.new("Mirror", "MIRROR")
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.levels = 1
m = ob.modifiers.new("Solidify", "SOLIDIFY")
m.thickness = 0.1
x += 4

ob = cube("Disabled", (x, 0, 0))
m = ob.modifiers.new("Subdivision", "SUBSURF")
m.levels = 2
m.show_viewport = False
x += 4

bpy.ops.mesh.primitive_monkey_add(location=(x, 0, 0))
ob = bpy.context.active_object
ob.name = "CustomNormals"
mesh = ob.data
normals = [(0.0, 0.0, 1.0)] * len(mesh.loops)
mesh.normals_split_custom_set(normals)
x += 4

save("modifiers.blend")
save("modifiers-plain.blend", compress=False)

# 2. Collections, instances, visibility and parenting.
reset()
scene = bpy.context.scene
assets = bpy.data.collections.new("Assets")
scene.collection.children.link(assets)
bpy.ops.mesh.primitive_cube_add(size=0.5, location=(0, 0, 2))
asset = bpy.context.active_object
asset.name = "AssetCube"
for c in asset.users_collection:
    c.objects.unlink(asset)
assets.objects.link(asset)
assets.instance_offset = (0, 0, 2)
scene.view_layers[0].layer_collection.children["Assets"].exclude = True
for i in range(3):
    bpy.ops.object.collection_instance_add(collection="Assets", location=(i * 2.0, 4, 0))
    bpy.context.active_object.name = "Instance%d" % i

ob = cube("HiddenRender", (0, -4, 0))
ob.hide_render = True
ob = cube("HiddenViewport", (2, -4, 0))
ob.hide_viewport = True
ob = cube("HiddenEye", (4, -4, 0))
ob.hide_set(True)
ob = cube("Wire", (6, -4, 0))
ob.display_type = "WIRE"

parent = cube("Parent", (0, 8, 0))
parent.rotation_euler = (0, 0, math.radians(45))
parent.scale = (2, 1, 1)
child = cube("Child", (3, 8, 1), 0.5)
bpy.context.view_layer.update()
child.parent = parent
child.matrix_parent_inverse = parent.matrix_world.inverted()

ob = cube("RotModes", (6, 8, 0))
ob.rotation_mode = "ZXY"
ob.rotation_euler = (0.3, 0.6, 0.9)

save("scene.blend")
