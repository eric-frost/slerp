# Writes Blender's own view of a .blend file, to check slerp-blend against:
# a GLB of the visible, renderable objects with modifiers applied, and a JSON
# file of per-object facts from the evaluated scene.
#
#   blender -b FILE --factory-startup --python-exit-code 1 \
#       --python crates/blend/tools/blender-reference.py -- OUT_BASE
#
# writes OUT_BASE.glb and OUT_BASE.json.  Textures are left out of the GLB
# (only factors and texture slots are compared), which keeps it small.

import bpy
import json
import sys

argv = sys.argv[sys.argv.index("--") + 1:]
base = argv[0]

depsgraph = bpy.context.evaluated_depsgraph_get()
scene = bpy.context.scene
facts = {"version": bpy.app.version_string, "file_version": list(bpy.data.version), "objects": []}

for inst in depsgraph.object_instances:
    ob = inst.object
    if ob.type != "MESH":
        continue
    orig = ob.original
    mesh = ob.data
    mesh.calc_loop_triangles()
    world = inst.matrix_world
    lo = [float("inf")] * 3
    hi = [float("-inf")] * 3
    for v in mesh.vertices:
        p = world @ v.co
        for k in range(3):
            lo[k] = min(lo[k], p[k])
            hi[k] = max(hi[k], p[k])
    facts["objects"].append({
        "name": orig.name,
        "instance": inst.is_instance,
        "visible": orig.visible_get() if not inst.is_instance else True,
        "render": not orig.hide_render,
        "display": orig.display_type,
        "triangles": len(mesh.loop_triangles),
        "vertices": len(mesh.vertices),
        "min": lo if mesh.vertices else None,
        "max": hi if mesh.vertices else None,
        "materials": [s.material.name if s.material else None for s in ob.material_slots],
        "modifiers": [m.type for m in orig.modifiers if m.show_viewport],
    })

with open(base + ".json", "w") as f:
    json.dump(facts, f, indent=1)

settings = dict(
    filepath=base + ".glb",
    export_format="GLB",
    use_active_scene=True,
    use_visible=True,
    use_renderable=True,
    export_apply=True,
    export_yup=True,
    export_image_format="NONE",
    export_cameras=False,
    export_lights=False,
    export_animations=False,
    export_skins=False,
    export_morph=False,
    export_extras=False,
)
try:
    bpy.ops.export_scene.gltf(**settings)
except Exception as e:
    # The exporter trips over some node trees; geometry still compares.
    print("retrying without materials:", e)
    bpy.ops.export_scene.gltf(export_materials="NONE", **settings)
