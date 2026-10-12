# Exports a .blend file as USD with its armatures (UsdSkel skeletons and
# skins), shape keys (blend shapes) and animation, for slerp-materials'
# UsdSkel tests.  Ubuntu's Blender is built without USD, so this needs an
# official Blender build (blender.org's Linux tarball):
#
#   blender -b FILE.blend --factory-startup --python-exit-code 1 \
#       --python crates/materials/tools/blender-usd-export.py -- OUT.usdc
#
# OUT may end in .usda, .usdc or .usdz.  The files made from the glTF-Blender-IO
# test scenes and the human base meshes are in local/assets/usd-extra/blender/.

import bpy
import sys

out = sys.argv[sys.argv.index("--") + 1]
bpy.ops.wm.usd_export(
    filepath=out,
    export_animation=True,
    export_armatures=True,
    export_shapekeys=True,
    export_textures_mode="NEW",
)
