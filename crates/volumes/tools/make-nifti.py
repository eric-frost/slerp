#!/usr/bin/env python3
"""Writes NIfTI and Analyze test volumes and reference.json to
local/assets/volumes/nifti-generated/, with nibabel (pip install nibabel).

    local/pyenv-vol/bin/python crates/volumes/tools/make-nifti.py

The orient/ folder holds one small asymmetric object stored in all 48 axis
orders and directions; the tests check every one shows the same pictures.
"""
import itertools
import json
import os
import sys
import numpy as np
import nibabel as nib
from nibabel.orientations import axcodes2ornt, io_orientation, ornt_transform

out = sys.argv[1] if len(sys.argv) > 1 else "local/assets/volumes/nifti-generated"
os.makedirs(out + "/orient", exist_ok=True)
rng = np.random.default_rng(99)
ref = {}


def world_object(shape=(20, 24, 16)):
    """An asymmetric head-like object in RAS index space: ellipsoid, a bright
    blob towards R, A and S, and a dim one towards L, P and I."""
    x, y, z = np.indices(shape).astype(np.float64)
    c = [(s - 1) / 2 for s in shape]
    r = np.sqrt(((x - c[0]) / c[0]) ** 2 + ((y - c[1]) / c[1]) ** 2 + ((z - c[2]) / c[2]) ** 2)
    v = (1 - np.clip(r, 0, 1)) * 500
    v += 400 * np.exp(-(((x - 15) ** 2 + (y - 18) ** 2 + (z - 11) ** 2) / 6))
    v += 150 * np.exp(-(((x - 3) ** 2 + (y - 4) ** 2 + (z - 3) ** 2) / 6))
    # A bar along y at the right, so mirror errors show up as a different picture.
    v[16:18, 2:20, 8] += 300
    return v.astype(np.int16)


def aff_for(shape, zooms=(1.0, 1.5, 2.0)):
    a = np.diag([*zooms, 1.0])
    a[:3, 3] = [-(shape[0] - 1) * zooms[0] / 2, -(shape[1] - 1) * zooms[1] / 2, -(shape[2] - 1) * zooms[2] / 2]
    return a


def record(name, img, samples=3, extra=None):
    hdr = img.header
    shape = list(img.shape)
    entry = {"shape": shape, "zooms": [float(z) for z in hdr.get_zooms()], "axcodes": "".join(c or "?" for c in nib.aff2axcodes(img.affine)) if len(shape) >= 3 else ""}
    data = np.asanyarray(img.dataobj)
    if data.dtype.names is None and data.dtype.kind in "biuf":
        flat = np.asarray(data, dtype=np.float64)
        entry["min"] = float(flat.min()); entry["max"] = float(flat.max()); entry["mean"] = float(flat.mean())
        picks = []
        for _ in range(samples):
            idx = tuple(int(rng.integers(0, s)) for s in shape)
            picks.append([list(idx), float(flat[idx])])
        entry["samples"] = picks
    if extra:
        entry.update(extra)
    ref[name] = entry


base = world_object()
ras = nib.Nifti1Image(base, aff_for(base.shape))
ras.header.set_xyzt_units("mm")

# 1. All 48 storage orders of the same object.
ras_ornt = io_orientation(ras.affine)
count = 0
for perm in itertools.permutations(range(3)):
    for flips in itertools.product([1, -1], repeat=3):
        ornt = np.array([[perm[i], flips[i]] for i in range(3)], dtype=float)
        img = ras.as_reoriented(ornt)
        codes = "".join(nib.aff2axcodes(img.affine))
        name = f"orient/{codes}.nii" + (".gz" if count % 5 == 0 else "")
        nib.save(img, os.path.join(out, name))
        record(name, nib.load(os.path.join(out, name)), samples=2)
        count += 1

# 2. Data types and byte orders.
src = world_object((24, 20, 12)).astype(np.float64)
aff = aff_for(src.shape, (2.0, 2.0, 3.0))
for dt in ["uint8", "int8", "int16", "uint16", "int32", "uint32", "int64", "uint64", "float32", "float64", ">i2", ">f4", ">f8", ">i4", ">u2"]:
    data = np.clip(src / 4, 0, 120).astype(dt) if dt in ("uint8", "int8") else src.astype(dt)
    if dt in ("float32", ">f4", "float64", ">f8"):
        data = (src / 100 + 0.25).astype(dt)
    hdr = nib.Nifti1Header(endianness=">" if dt.startswith(">") else "<")
    img = nib.Nifti1Image(data, aff, hdr)
    img.set_data_dtype(np.dtype(dt))
    name = f"dtype_{dt.replace('>', 'be_')}.nii"
    nib.save(img, os.path.join(out, name))
    record(name, nib.load(os.path.join(out, name)))
cplx = (src + 1j * src[::-1]).astype(np.complex64)
nib.save(nib.Nifti1Image(cplx, aff), os.path.join(out, "dtype_complex64.nii"))
rgb_dt = np.dtype([("R", "u1"), ("G", "u1"), ("B", "u1")])
rgb = np.zeros((16, 14, 6), dtype=rgb_dt)
rgb["R"] = np.arange(16)[:, None, None] * 16
rgb["G"] = np.arange(14)[None, :, None] * 18
rgb["B"] = np.arange(6)[None, None, :] * 40
im = nib.Nifti1Image(rgb, np.eye(4))
nib.save(im, os.path.join(out, "rgb24.nii"))
ref["rgb24.nii"] = {"shape": [16, 14, 6], "rgb": [[[3, 5, 2], [48, 90, 80]], [[15, 13, 5], [240, 234, 200]]]}

# 3. Scaling: int16 on disk, floats in memory.
f = (src / 3 - 20).astype(np.float64)
img = nib.Nifti1Image(f, aff)
img.set_data_dtype(np.int16)
nib.save(img, os.path.join(out, "scaled_int16.nii"))
record("scaled_int16.nii", nib.load(os.path.join(out, "scaled_int16.nii")), extra={"slope": float(nib.load(os.path.join(out, "scaled_int16.nii")).dataobj.slope)})

# 4. Time series, a diffusion-like set, and odd dimensions.
t4 = np.stack([(src * (1 + t / 5)).astype(np.int16) for t in range(6)], axis=-1)
img = nib.Nifti1Image(t4, aff)
img.header["pixdim"][4] = 2.5
nib.save(img, os.path.join(out, "series4d.nii.gz"))
record("series4d.nii.gz", nib.load(os.path.join(out, "series4d.nii.gz")))
nib.save(nib.Nifti1Image(world_object((48, 40, 12))[:, :, 6].astype(np.int16), np.eye(4)), os.path.join(out, "flat2d.nii"))
record("flat2d.nii", nib.load(os.path.join(out, "flat2d.nii")))
t5 = rng.integers(0, 100, (6, 5, 4, 3, 2)).astype(np.int16)
nib.save(nib.Nifti1Image(t5, np.eye(4)), os.path.join(out, "five_d.nii"))
record("five_d.nii", nib.load(os.path.join(out, "five_d.nii")))

# 5. Pairs, Analyze and NIfTI-2.
for cls, name in [(nib.Nifti1Pair, "pair1"), (nib.Nifti2Pair, "pair2"), (nib.AnalyzeImage, "analyze")]:
    img = cls(src.astype(np.int16), aff)
    nib.save(img, os.path.join(out, name + ".img"))
    record(name + ".hdr", nib.load(os.path.join(out, name + ".hdr")))
nib.save(nib.Nifti2Image(src.astype(np.float32), aff), os.path.join(out, "nifti2.nii.gz"))
record("nifti2.nii.gz", nib.load(os.path.join(out, "nifti2.nii.gz")))
img = nib.Nifti1Pair(src.astype(np.int16), aff)
nib.save(img, os.path.join(out, "gzpair.img"))
os.system(f"gzip -f '{out}/gzpair.img'")
record("gzpair.hdr", img)

# 6. Something big enough to need the streaming reader.
N = 500
big = np.zeros((512, 512, N), dtype=np.int16)
x, y = np.indices((512, 512)).astype(np.float64)
for k in range(N):
    r = np.sqrt(((x - 256) / 230) ** 2 + ((y - 256) / 230) ** 2 + ((k - N / 2) / (N / 2.2)) ** 2)
    big[:, :, k] = (np.clip(1 - r, 0, 1) * 1500).astype(np.int16)
nib.save(nib.Nifti1Image(big, np.diag([0.5, 0.5, 0.8, 1])), os.path.join(out, "big_512x512x500.nii"))
record("big_512x512x500.nii", nib.load(os.path.join(out, "big_512x512x500.nii")))
nib.save(nib.Nifti1Image(big[::2, ::2, ::2].copy(), np.diag([1.0, 1.0, 1.6, 1])), os.path.join(out, "mid_256x256x250.nii.gz"))
record("mid_256x256x250.nii.gz", nib.load(os.path.join(out, "mid_256x256x250.nii.gz")))

with open(os.path.join(out, "reference.json"), "w") as fh:
    json.dump(ref, fh, indent=1)
print("wrote", len(ref), "entries")
