# Writes the UsdSkel and point instancer test files of slerp-materials, with
# Pixar's own results to compare against, using Pixar's USD (the usd-core
# package from PyPI, Apache-2.0 style TOST licence):
#
#   python3 -m venv local/pyenv-usd
#   local/pyenv-usd/bin/pip install usd-core
#   local/pyenv-usd/bin/python crates/materials/tools/make-usd-fixtures.py \
#       crates/materials/tests/data/usd [local/assets/usd-extra/generated]
#
# writes into the first folder (committed):
#   skel.usda, skel.usdc      a skinned, blend-shaped, animated rig: sparse
#                             and reordered animation joints, a mesh's own
#                             joint order, an inherited geom bind transform
#                             and animation source, a rigidly bound cube,
#                             sparse blend shapes with normal offsets and an
#                             inbetween, weights beyond 1, half scales, and a
#                             mesh moved by animated transform operations
#   skel.reference.txt        UsdSkel's baked world-space points of each
#                             prim at several times (and the cube's corners):
#                             lines of a prim name, a time code and the
#                             points' coordinates
#   bind.usda                 the same rig without animation, whose bind and
#                             rest poses agree
#   instancer.usda, .usdc     point instancers: a two-mesh prototype with its
#                             own transform, a prototype under an `over`,
#                             half orientations, scales, ids with invisible
#                             and inactive ones, and an instancer of instancers
#   instancer.reference.txt   UsdGeom's transform of each instance shown:
#                             lines of the instancer's path, the instance's
#                             index and the matrix's 16 numbers, row by row
#
# and with a second folder, larger files for timing and pictures (not
# committed): forest.usdc, 250,000 instances of three trees.
#
# The files are made from scratch here, so they carry no third-party licence.

import math
import os
import random
import sys

from pxr import Gf, Sdf, Usd, UsdGeom, UsdSkel

out = sys.argv[1]
big = sys.argv[2] if len(sys.argv) > 2 else None
os.makedirs(out, exist_ok=True)


def rows(m):
    """A Gf.Matrix4d as USD text."""
    return "( " + ", ".join("(" + ", ".join(repr(float(m[r][c])) for c in range(4)) + ")" for r in range(4)) + " )"


def translate(x, y, z):
    return Gf.Matrix4d().SetTranslate(Gf.Vec3d(x, y, z))


def vecs(points):
    return "[" + ", ".join("(%r, %r, %r)" % tuple(float(v) for v in p) for p in points) + "]"


def ints(values):
    return "[" + ", ".join(str(v) for v in values) + "]"


def floats(values):
    return "[" + ", ".join(repr(float(v)) for v in values) + "]"


def quat_text(axis, degrees):
    q = Gf.Rotation(Gf.Vec3d(*axis), degrees).GetQuat()
    i = q.GetImaginary()
    return "(%r, %r, %r, %r)" % (q.GetReal(), i[0], i[1], i[2])


#region Rig

# A square column from y = 0.5 to 2.5 in its own space, three rings of four
# points, moved down by half a unit into the skeleton's bind space by the
# geom bind transform its parent authors.
ring = [(-0.25, 0.0, -0.25), (0.25, 0.0, -0.25), (0.25, 0.0, 0.25), (-0.25, 0.0, 0.25)]
bar_points = [(x, y + 0.5 + level, z) for level in range(3) for (x, y, z) in ring]
bar_faces = []
for level in range(2):
    a, b = level * 4, level * 4 + 4
    for k in range(4):
        bar_faces.append([a + k, a + (k + 1) % 4, b + (k + 1) % 4, b + k])
bar_faces.append([3, 2, 1, 0])
bar_faces.append([8, 9, 10, 11])

# The mesh's own joint order: tip, root, mid.  Ring 0 follows the root,
# ring 1 the root and mid equally, ring 2 mostly the tip.
bar_indices = [1, 0] * 4 + [1, 2] * 4 + [0, 2] * 4
bar_weights = [1.0, 0.0] * 4 + [0.5, 0.5] * 4 + [0.7, 0.3] * 4

# Skeleton: root at the origin, mid one unit up, tip two, and a side joint.
bind = [translate(0, 0, 0), translate(0, 1, 0), translate(0, 2, 0), translate(1, 0, 0)]
rest = [translate(0, 0, 0), translate(0, 1, 0), translate(0, 1, 0), translate(1, 0, 0)]


def rig(animated):
    """The rig as USD text, with or without its animation."""
    anim = ""
    source = ""
    if animated:
        source = "    rel skel:animationSource = </Rig/Anim>\n"
        # Animation joints are sparse (no side joint) and in another order.
        anim = '''
    def SkelAnimation "Anim"
    {
        uniform token[] joints = ["root/mid", "root/mid/tip", "root"]
        float3[] translations.timeSamples = {
            1: [(0, 1, 0), (0, 1, 0), (0, 0, 0)],
            11: [(0, 1, 0), (0, 1, 0), (0.25, 0, 0)],
            21: [(0, 1, 0), (0, 1, 0), (0.5, 0, 0)],
        }
        quatf[] rotations.timeSamples = {
            1: [%s, %s, %s],
            11: [%s, %s, %s],
            21: [%s, %s, %s],
        }
        half3[] scales.timeSamples = {
            1: [(1, 1, 1), (1, 1, 1), (1, 1, 1)],
            21: [(1, 1.25, 1), (1, 1, 1), (1, 1, 1)],
        }
        uniform token[] blendShapes = ["Twist", "Bulge"]
        float[] blendShapeWeights.timeSamples = {
            1: [0, 0],
            11: [0.5, 1],
            21: [1.5, 0.25],
        }
    }
''' % (
            quat_text((0, 0, 1), 0), quat_text((1, 0, 0), 0), quat_text((0, 1, 0), 0),
            quat_text((0, 0, 1), 45), quat_text((1, 0, 0), -30), quat_text((0, 1, 0), 10),
            quat_text((0, 0, 1), 90), quat_text((1, 0, 0), 20), quat_text((0, 1, 0), 20),
        )
    bulge = [(-0.15, 0, -0.15), (0.15, 0, -0.15), (0.15, 0, 0.15), (-0.15, 0, 0.15)]
    twist = [(0, 0, 0)] * 8 + [(0.2, 0, -0.1), (0.1, 0, 0.2), (-0.2, 0, 0.1), (-0.1, 0, -0.2)]
    half = [(0, 0, 0)] * 8 + [(0.05, 0.1, 0), (0, 0.1, 0.05), (-0.05, 0.1, 0), (0, 0.1, -0.05)]
    return '''#usda 1.0
(
    defaultPrim = "Rig"
    metersPerUnit = 1
    upAxis = "Y"
    timeCodesPerSecond = 30
    startTimeCode = 1
    endTimeCode = 21
)

def SkelRoot "Rig" (
    prepend apiSchemas = ["SkelBindingAPI"]
)
{
%s    rel skel:skeleton = </Rig/Skel>

    def Skeleton "Skel"
    {
        uniform token[] joints = ["root", "root/mid", "root/mid/tip", "root/side"]
        uniform matrix4d[] bindTransforms = [%s]
        uniform matrix4d[] restTransforms = [%s]
    }
%s
    def Xform "Geo" (
        prepend apiSchemas = ["SkelBindingAPI"]
    )
    {
        float3 xformOp:translate = (0, -0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        matrix4d primvars:skel:geomBindTransform = %s

        def Mesh "Bar" (
            prepend apiSchemas = ["SkelBindingAPI"]
        )
        {
            int[] faceVertexCounts = %s
            int[] faceVertexIndices = %s
            point3f[] points = %s
            uniform token[] skel:joints = ["root/mid/tip", "root", "root/mid"]
            int[] primvars:skel:jointIndices = %s (
                elementSize = 2
                interpolation = "vertex"
            )
            float[] primvars:skel:jointWeights = %s (
                elementSize = 2
                interpolation = "vertex"
            )
            uniform token[] skel:blendShapes = ["Bulge", "Twist"]
            rel skel:blendShapeTargets = [<Bulge>, <Twist>]

            def BlendShape "Bulge"
            {
                uniform vector3f[] offsets = %s
                uniform vector3f[] normalOffsets = %s
                uniform int[] pointIndices = [4, 5, 6, 7]
            }

            def BlendShape "Twist"
            {
                uniform vector3f[] offsets = %s
                uniform vector3f[] inbetweens:half = %s (
                    weight = 0.5
                )
            }
        }

        def Mesh "Fin" (
            prepend apiSchemas = ["SkelBindingAPI"]
        )
        {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(1, 0.5, 0), (1.5, 0.5, 0), (1.5, 1.5, 0), (1, 1.5, 0)]
            int[] primvars:skel:jointIndices = [3, 3, 1, 1] (
                elementSize = 1
                interpolation = "vertex"
            )
            float[] primvars:skel:jointWeights = [1, 1, 1, 1] (
                elementSize = 1
                interpolation = "vertex"
            )
        }

        def Cube "Rigid" (
            prepend apiSchemas = ["SkelBindingAPI"]
        )
        {
            double size = 0.5
            matrix4d primvars:skel:geomBindTransform = %s
            int[] primvars:skel:jointIndices = [1] (
                elementSize = 1
                interpolation = "constant"
            )
            float[] primvars:skel:jointWeights = [1] (
                elementSize = 1
                interpolation = "constant"
            )
        }
    }
}

def Mesh "Spinner"
{
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0, 0.5, 0)]
    float3 xformOp:translate.timeSamples = {
        1: (3, 0, 0),
        21: (3, 2, 0),
    }
    float xformOp:rotateY.timeSamples = {
        1: 0,
        11: 45,
        21: 90,
    }
    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateY"]
}
''' % (
        source,
        ", ".join(rows(m) for m in bind),
        ", ".join(rows(m) for m in rest),
        anim,
        rows(translate(0, -0.5, 0)),
        ints(len(f) for f in bar_faces),
        ints(i for f in bar_faces for i in f),
        vecs(bar_points),
        ints(bar_indices),
        floats(bar_weights),
        vecs(bulge),
        vecs([(0, 0, 0)] * 4),
        vecs(twist),
        vecs(half),
        rows(translate(0, 1, 0)),
    )


def write(name, text):
    path = os.path.join(out, name)
    with open(path, "w") as f:
        f.write(text)
    return path


skel_usda = write("skel.usda", rig(True))
Sdf.Layer.FindOrOpen(skel_usda).Export(os.path.join(out, "skel.usdc"))
write("bind.usda", rig(False))

# Reference: UsdSkel's baked points, in world space, at times before,
# between and after the samples.
stage = Usd.Stage.Open(Usd.Stage.Open(skel_usda).Flatten())
UsdSkel.BakeSkinning(stage.Traverse())
lines = ["# prim, time code, world-space points (UsdSkel.BakeSkinning, USD %d.%d)" % Usd.GetVersion()[1:]]
for path in ["/Rig/Geo/Bar", "/Rig/Geo/Fin", "/Rig/Geo/Rigid", "/Spinner"]:
    prim = stage.GetPrimAtPath(path)
    for t in [0, 1, 6, 11, 16, 21, 25]:
        m = UsdGeom.XformCache(Usd.TimeCode(t)).GetLocalToWorldTransform(prim)
        if prim.IsA(UsdGeom.Mesh):
            local = [Gf.Vec3d(p) for p in UsdGeom.Mesh(prim).GetPointsAttr().Get(Usd.TimeCode(t))]
        else:
            local = [Gf.Vec3d(x, y, z) for x in (-0.25, 0.25) for y in (-0.25, 0.25) for z in (-0.25, 0.25)]
        numbers = " ".join("%.6f" % v for p in local for v in m.Transform(p))
        lines.append("%s %d %s" % (prim.GetName(), t, numbers))
with open(os.path.join(out, "skel.reference.txt"), "w") as f:
    f.write("\n".join(lines) + "\n")

#endregion

#region Instancers

box = ([(-0.5, 0, -0.5), (0.5, 0, -0.5), (0.5, 0, 0.5), (-0.5, 0, 0.5),
        (-0.5, 1, -0.5), (0.5, 1, -0.5), (0.5, 1, 0.5), (-0.5, 1, 0.5)],
       [[0, 1, 2, 3], [7, 6, 5, 4], [0, 4, 5, 1], [1, 5, 6, 2], [2, 6, 7, 3], [3, 7, 4, 0]])


def mesh(name, scale, offset, color, indent):
    pad = " " * indent
    points = [(x * scale[0] + offset[0], y * scale[1] + offset[1], z * scale[2] + offset[2]) for (x, y, z) in box[0]]
    return '''%sdef Mesh "%s"
%s{
%s    int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
%s    int[] faceVertexIndices = %s
%s    point3f[] points = %s
%s    color3f[] primvars:displayColor = [%s]
%s}
''' % (pad, name, pad, pad, pad, ints(i for f in box[1] for i in f), pad, vecs(points), pad,
       "(%r, %r, %r)" % color, pad)


def orient(axis, degrees):
    q = Gf.Rotation(Gf.Vec3d(*axis), degrees).GetQuat()
    i = q.GetImaginary()
    return "(%r, %r, %r, %r)" % (q.GetReal(), i[0], i[1], i[2])


instancer_text = '''#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

def Xform "World"
{
    def PointInstancer "Trees" (
        inactiveIds = [14]
    )
    {
        rel prototypes = [</World/Trees/Protos/Tree>, </Library/Rock>]
        int[] protoIndices = [0, 1, 0, 1, 0, 0]
        point3f[] positions = [(0, 0, 0), (5, 0, 0), (0, 0, 5), (5, 0, 5), (10, 0, 0), (10, 0, 10)]
        quath[] orientations = [%s, %s, %s, %s, %s, %s]
        float3[] scales = [(1, 1, 1), (2, 1, 2), (1, 2, 1), (1, 1, 1), (1, 1, 1), (0.5, 0.5, 0.5)]
        int64[] ids = [10, 11, 12, 13, 14, 15]
        int64[] invisibleIds = [13]

        def "Protos"
        {
            def Xform "Tree"
            {
                float3 xformOp:translate = (0, 0.25, 0)
                uniform token[] xformOpOrder = ["xformOp:translate"]

%s
%s            }
        }
    }

    def PointInstancer "Rows"
    {
        rel prototypes = </World/Trees>
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 30), (0, 0, 60)]
    }
}

over "Library"
{
    def Xform "Rock"
    {
%s    }
}
''' % (
    orient((0, 1, 0), 0), orient((0, 1, 0), 90), orient((0, 1, 0), 45), orient((1, 0, 0), 30),
    orient((0, 0, 1), 10), orient((0, 1, 0), 180),
    mesh("Trunk", (0.2, 1, 0.2), (0, 0, 0), (0.4, 0.25, 0.1), 16),
    mesh("Crown", (1, 1.5, 1), (0, 1, 0), (0.1, 0.6, 0.2), 16),
    mesh("Stone", (0.8, 0.5, 0.6), (0, 0, 0), (0.5, 0.5, 0.5), 8),
)
inst_usda = write("instancer.usda", instancer_text)
Sdf.Layer.FindOrOpen(inst_usda).Export(os.path.join(out, "instancer.usdc"))

stage = Usd.Stage.Open(inst_usda)
lines = ["# instancer, instance, transform row by row (UsdGeom, USD %d.%d)" % Usd.GetVersion()[1:]]
for path in ["/World/Trees", "/World/Rows"]:
    pi = UsdGeom.PointInstancer(stage.GetPrimAtPath(path))
    t = Usd.TimeCode.Default()
    # The transforms (prototype root's own included) of the instances the
    # mask keeps, in order.
    xforms = pi.ComputeInstanceTransformsAtTime(t, t)
    mask = pi.ComputeMaskAtTime(t)
    count = len(pi.GetProtoIndicesAttr().Get(t))
    shown = [k for k in range(count) if not mask or mask[k]]
    for k, m in zip(shown, xforms):
        lines.append("%s %d %s" % (path, k, " ".join("%.6f" % m[r][c] for r in range(4) for c in range(4))))
with open(os.path.join(out, "instancer.reference.txt"), "w") as f:
    f.write("\n".join(lines) + "\n")

#endregion

#region Forest

# 250,000 trees of three kinds on a 50 by 50 metre patch, for timing and
# for pictures of thinning (the viewer keeps 100,000).
if big:
    os.makedirs(big, exist_ok=True)
    rnd = random.Random(7)
    layer = Sdf.Layer.CreateNew(os.path.join(big, "forest.usdc"))
    stage = Usd.Stage.Open(layer)
    stage.SetMetadata("metersPerUnit", 1.0)
    UsdGeom.SetStageUpAxis(stage, UsdGeom.Tokens.y)
    root = UsdGeom.Xform.Define(stage, "/Forest")
    stage.SetDefaultPrim(root.GetPrim())
    ground = UsdGeom.Mesh.Define(stage, "/Forest/Ground")
    ground.CreatePointsAttr([(-26, 0, -26), (26, 0, -26), (26, 0, 26), (-26, 0, 26)])
    ground.CreateFaceVertexCountsAttr([4])
    ground.CreateFaceVertexIndicesAttr([3, 2, 1, 0])
    ground.CreateDisplayColorAttr([(0.35, 0.3, 0.2)])
    pi = UsdGeom.PointInstancer.Define(stage, "/Forest/Trees")
    protos = []
    for k, (h, color) in enumerate([(1.0, (0.1, 0.5, 0.15)), (1.6, (0.15, 0.4, 0.1)), (0.7, (0.4, 0.55, 0.1))]):
        tree = UsdGeom.Xform.Define(stage, "/Forest/Trees/Protos/Tree%d" % k)
        for name, sx, sy, oy, c in [("Trunk", 0.06, 0.4 * h, 0.0, (0.35, 0.22, 0.1)), ("Crown", 0.35, h, 0.35 * h, color)]:
            m = UsdGeom.Mesh.Define(stage, tree.GetPath().AppendChild(name))
            ring = 12
            pts = [(0, oy + sy, 0)]
            for i in range(ring):
                a = 2 * math.pi * i / ring
                pts.append((sx * math.cos(a), oy, sx * math.sin(a)))
            pts.append((0, oy, 0))
            faces, idx = [], []
            for i in range(ring):
                faces += [3, 3]
                idx += [0, 1 + (i + 1) % ring, 1 + i, ring + 1, 1 + i, 1 + (i + 1) % ring]
            m.CreatePointsAttr(pts)
            m.CreateFaceVertexCountsAttr(faces)
            m.CreateFaceVertexIndicesAttr(idx)
            m.CreateDisplayColorAttr([c])
        protos.append(tree.GetPath())
    pi.CreatePrototypesRel().SetTargets(protos)
    n = 250_000
    pi.CreatePositionsAttr([(rnd.uniform(-25, 25), 0, rnd.uniform(-25, 25)) for _ in range(n)])
    pi.CreateProtoIndicesAttr([rnd.randrange(3) for _ in range(n)])
    pi.CreateOrientationsAttr([Gf.Quath(Gf.Rotation(Gf.Vec3d(0, 1, 0), rnd.uniform(0, 360)).GetQuat()) for _ in range(n)])
    pi.CreateScalesAttr([(s, s * rnd.uniform(0.8, 1.3), s) for s in (rnd.uniform(0.6, 1.4) for _ in range(n))])
    layer.Save()

#endregion
