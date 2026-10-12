#!/usr/bin/env python3
"""Writes MetaImage and NRRD test volumes and reference.json files to
local/assets/volumes/{mha,nrrd}-generated/, with SimpleITK and pynrrd
(pip install SimpleITK pynrrd nibabel).

    local/pyenv-vol/bin/python crates/volumes/tools/make-mha-nrrd.py

Both folders get the 48 axis orders of one asymmetric object, like
make-nifti.py, so the tests can check orientation handling.
"""
import itertools
import json
import os
import subprocess
import sys
import numpy as np
import nibabel as nib
import nrrd
import SimpleITK as sitk
from nibabel.orientations import io_orientation

root = sys.argv[1] if len(sys.argv) > 1 else "local/assets/volumes"
mha_dir, nrrd_dir = root + "/mha-generated", root + "/nrrd-generated"
for d in (mha_dir + "/orient", nrrd_dir + "/orient"):
    os.makedirs(d, exist_ok=True)
rng = np.random.default_rng(5)
mref, nref = {}, {}


def nw(path, arr, hdr, encoding="raw", **kw):
    """pynrrd's write with the encoding set in the header."""
    h = dict(hdr)
    h["encoding"] = encoding
    nrrd.write(path, arr, h, **kw)


def world_object(shape=(20, 24, 16)):
    x, y, z = np.indices(shape).astype(np.float64)
    c = [(s - 1) / 2 for s in shape]
    r = np.sqrt(((x - c[0]) / c[0]) ** 2 + ((y - c[1]) / c[1]) ** 2 + ((z - c[2]) / c[2]) ** 2)
    v = (1 - np.clip(r, 0, 1)) * 500
    v += 400 * np.exp(-(((x - 15) ** 2 + (y - 18) ** 2 + (z - 11) ** 2) / 6))
    v += 150 * np.exp(-(((x - 3) ** 2 + (y - 4) ** 2 + (z - 3) ** 2) / 6))
    v[16:18, 2:20, 8] += 300
    return v.astype(np.int16)


def samples(arr, n=3):
    out = []
    for _ in range(n):
        idx = tuple(int(rng.integers(0, s)) for s in arr.shape)
        out.append([list(idx), float(arr[idx])])
    return out


def sitk_image(arr, spacing, direction_lps, origin=(0, 0, 0)):
    img = sitk.GetImageFromArray(arr.transpose(*reversed(range(arr.ndim))))
    img.SetSpacing(spacing)
    img.SetOrigin(origin)
    if arr.ndim == 3:
        img.SetDirection(direction_lps)
    return img


base = world_object()
ras = nib.Nifti1Image(base, np.diag([1.0, 1.5, 2.0, 1.0]))
count = 0
for perm in itertools.permutations(range(3)):
    for flips in itertools.product([1, -1], repeat=3):
        ornt = np.array([[perm[i], flips[i]] for i in range(3)], dtype=float)
        img = ras.as_reoriented(ornt)
        arr = np.asarray(img.dataobj)
        A = img.affine
        zooms = [float(np.linalg.norm(A[:3, i])) for i in range(3)]
        R = A[:3, :3] / zooms
        codes = "".join(nib.aff2axcodes(A))
        # MetaImage: ITK's direction matrix is in LPS; columns are the axes.
        lps = np.diag([-1, -1, 1]) @ R
        name = f"orient/{codes}" + (".mha" if count % 2 == 0 else ".mhd")
        sitk.WriteImage(sitk_image(arr, zooms, tuple(lps.flatten())), os.path.join(mha_dir, name), count % 3 == 0)
        mref[name] = {"shape": list(arr.shape), "axcodes": codes, "zooms": zooms, "samples": samples(arr, 2)}
        # NRRD: space directions in RAS; rows of the list are the axes.
        hdr = {"space": "right-anterior-superior", "space directions": (R * zooms).T, "space origin": np.array([0.0, 0.0, 0.0]), "kinds": ["domain"] * 3}
        enc = ["raw", "gzip", "bzip2", "ascii"][count % 4]
        nname = f"orient/{codes}.nrrd"
        nw(os.path.join(nrrd_dir, nname), arr, hdr, enc)
        nref[nname] = {"shape": list(arr.shape), "axcodes": codes, "zooms": zooms, "samples": samples(arr, 2), "encoding": enc}
        count += 1

# Data types and layouts.
src = world_object((24, 20, 12)).astype(np.float64)
for dt in ["uint8", "int8", "int16", "uint16", "int32", "uint32", "float32", "float64"]:
    a = np.clip(src / 4, 0, 120).astype(dt) if dt in ("uint8", "int8") else (src / 100).astype(dt) if dt.startswith("float") else src.astype(dt)
    for ext, comp in [("mha", False), ("mha", True)]:
        name = f"dtype_{dt}{'_z' if comp else ''}.{ext}"
        sitk.WriteImage(sitk_image(a, (2.0, 2.0, 3.0), (1, 0, 0, 0, 1, 0, 0, 0, 1)), os.path.join(mha_dir, name), comp)
        mref[name] = {"shape": list(a.shape), "samples": samples(a)}
    for enc in ["raw", "gzip"]:
        name = f"dtype_{dt}_{enc}.nrrd"
        nw(os.path.join(nrrd_dir, name), a, {"spacings": [2.0, 2.0, 3.0]}, enc)
        nref[name] = {"shape": list(a.shape), "samples": samples(a)}
a = src.astype(">i2")
nw(os.path.join(nrrd_dir, "dtype_bigendian.nrrd"), a, {"endian": "big"}, "raw")
nref["dtype_bigendian.nrrd"] = {"shape": list(a.shape), "samples": samples(a)}
# Detached header and data.
nw(os.path.join(nrrd_dir, "detached.nhdr"), src.astype(np.int16), {"spacings": [1, 1, 2]}, "raw", detached_header=True)
nref["detached.nhdr"] = {"shape": list(src.shape), "samples": samples(src.astype(np.int16))}
nw(os.path.join(nrrd_dir, "detached_gz.nhdr"), src.astype(np.int16), {}, "gzip", detached_header=True)
nref["detached_gz.nhdr"] = {"shape": list(src.shape), "samples": samples(src.astype(np.int16))}
# Hex, by hand.
a = (src[:, :, :4] / 8).astype(np.uint8)
raw = a.transpose(2, 1, 0).tobytes()  # first dimension fastest
with open(os.path.join(nrrd_dir, "hex.nrrd"), "wb") as f:
    f.write(f"NRRD0004\ntype: uchar\ndimension: 3\nsizes: {a.shape[0]} {a.shape[1]} {a.shape[2]}\nencoding: hex\n\n".encode())
    f.write(raw.hex().encode())
nref["hex.nrrd"] = {"shape": list(a.shape), "samples": samples(a)}
# 4D (diffusion-like), colour, vectors.
dwi = np.stack([(src * (1 + t / 4)).astype(np.int16) for t in range(5)], axis=-1)
h = {"kinds": ["domain", "domain", "domain", "list"], "spacings": [2.0, 2.0, 3.0, np.nan]}
nw(os.path.join(nrrd_dir, "dwi.nrrd"), dwi, h, "gzip")
nref["dwi.nrrd"] = {"shape": list(dwi.shape), "samples": samples(dwi)}
rgb = np.stack([np.add.outer(np.arange(16), np.arange(14)).astype(np.uint8) * 7, np.arange(16 * 14).reshape(16, 14).astype(np.uint8), np.full((16, 14), 200, np.uint8)])  # (3, 16, 14): colour first
nw(os.path.join(nrrd_dir, "rgb.nrrd"), rgb, {"kinds": ["RGB-color", "domain", "domain"]}, "raw")
nref["rgb.nrrd"] = {"shape": list(rgb.shape)}
# MetaImage: colour, 4D.
rgbimg = sitk.GetImageFromArray(np.stack([rgb[0], rgb[1], rgb[2]], axis=-1), isVector=True)
sitk.WriteImage(rgbimg, os.path.join(mha_dir, "rgb.mha"))
mref["rgb.mha"] = {"shape": [14, 16], "rgb": True}
img4 = sitk.GetImageFromArray(dwi.transpose(3, 2, 1, 0).astype(np.int16), isVector=False)
sitk.WriteImage(img4, os.path.join(mha_dir, "series4d.mha"), True)
mref["series4d.mha"] = {"shape": list(dwi.shape), "samples": samples(dwi)}

# A large compressed one for the streaming path.
big = np.zeros((256, 256, 200), dtype=np.int16)
xx, yy = np.indices((256, 256)).astype(np.float64)
for k in range(200):
    r = np.sqrt(((xx - 128) / 120) ** 2 + ((yy - 128) / 120) ** 2 + ((k - 100) / 95) ** 2)
    big[:, :, k] = (np.clip(1 - r, 0, 1) * 1400).astype(np.int16)
sitk.WriteImage(sitk_image(big, (0.9, 0.9, 1.2), (1, 0, 0, 0, 1, 0, 0, 0, 1)), os.path.join(mha_dir, "big_compressed.mha"), True)
mref["big_compressed.mha"] = {"shape": list(big.shape), "samples": samples(big)}
nw(os.path.join(nrrd_dir, "big_gzip.nrrd"), big, {"spacings": [0.9, 0.9, 1.2]}, "gzip")
nref["big_gzip.nrrd"] = {"shape": list(big.shape), "samples": samples(big)}
json.dump(mref, open(mha_dir + "/reference.json", "w"), indent=1)
json.dump(nref, open(nrrd_dir + "/reference.json", "w"), indent=1)
print(len(mref), "mha,", len(nref), "nrrd")
