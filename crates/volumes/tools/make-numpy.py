#!/usr/bin/env python3
"""Writes NumPy test files and a reference.json to local/assets/volumes/numpy/.

Run with the virtual environment that has numpy:

    local/pyenv-vol/bin/python crates/volumes/tools/make-numpy.py

reference.json lists, for each file, the shape, the dtype and values read back
with NumPy, which the Rust tests compare against.
"""
import json
import os
import sys
import numpy as np

out = sys.argv[1] if len(sys.argv) > 1 else "local/assets/volumes/numpy"
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(1234)
ref = {}


def sphere(shape, dtype, scale=1000.0):
    z, y, x = np.indices(shape).astype(np.float64)
    c = [max((s - 1) / 2, 1) for s in shape]
    r = np.sqrt(((z - c[0]) / c[0]) ** 2 + ((y - c[1]) / c[1]) ** 2 + ((x - c[2]) / c[2]) ** 2)
    return ((1 - np.clip(r, 0, 1)) * scale + rng.normal(0, 5, shape)).astype(dtype)


def save(name, arr, **kw):
    path = os.path.join(out, name)
    np.save(path, arr, **kw)
    a = np.asarray(arr)
    entry = {"shape": list(a.shape), "dtype": str(a.dtype), "fortran": bool(a.flags.f_contiguous and not a.flags.c_contiguous)}
    if a.dtype.kind in "biufc" and a.size:
        v = np.abs(a) if a.dtype.kind == "c" else a
        v = v.astype(np.float64)
        entry.update(min=float(v.min()), max=float(v.max()), mean=float(v.mean()))
        flat = v.ravel()
        # Values at a few logical positions (C order), to compare with extract().
        picks = [0, flat.size // 3, flat.size // 2, flat.size - 1]
        entry["samples"] = [[int(i), float(flat[i])] for i in picks]
    ref[name] = entry


# Pictures.
save("float32_2d.npy", (np.add.outer(np.arange(48), np.arange(64)) / 100 + rng.normal(0, 0.01, (48, 64))).astype(np.float32))
save("uint8_2d.npy", rng.integers(0, 255, (40, 56), dtype=np.uint8))
save("uint16_2d.npy", (np.add.outer(np.arange(100), np.arange(80)) * 300).astype(np.uint16))
save("rgb_uint8.npy", rng.integers(0, 255, (30, 40, 3), dtype=np.uint8))
save("rgba_float32.npy", rng.random((20, 24, 4)).astype(np.float32))
save("bool_mask.npy", sphere((1, 40, 40), np.float32)[0] > 500)
save("complex64.npy", (rng.normal(size=(32, 32)) + 1j * rng.normal(size=(32, 32))).astype(np.complex64))
save("float16_2d.npy", rng.normal(size=(24, 24)).astype(np.float16))

# Volumes: every integer and float type, both byte orders, both memory orders.
vol = sphere((24, 40, 32), np.float64)
save("int16_3d.npy", vol.astype(np.int16))
save("uint32_3d.npy", np.abs(vol).astype(np.uint32))
save("int8_3d.npy", np.clip(vol / 10, -127, 127).astype(np.int8))
save("int64_3d.npy", vol.astype(np.int64))
save("uint64_3d.npy", np.abs(vol).astype(np.uint64))
save("float64_3d.npy", vol)
save("float32_3d_fortran.npy", np.asfortranarray(vol.astype(np.float32)))
save("int16_3d_bigendian.npy", vol.astype(">i2"))
save("float64_3d_bigendian.npy", vol.astype(">f8"))
save("float32_3d_bigendian_fortran.npy", np.asfortranarray(vol.astype(">f4")))
save("float32_4d_tzyx.npy", np.stack([sphere((8, 16, 16), np.float32) * (1 + t / 4) for t in range(5)]))
save("rgb_stack_4d.npy", rng.integers(0, 255, (6, 20, 24, 3), dtype=np.uint8))
save("nonsquare_3d.npy", sphere((10, 50, 20), np.float32))
save("complex128_3d.npy", (vol + 1j * vol[::-1]).astype(np.complex128))

# No picture.
save("vector_1d.npy", np.sin(np.linspace(0, 20, 500)).astype(np.float32))
save("scalar.npy", np.array(3.5))
save("five_d.npy", rng.random((2, 3, 4, 5, 6)).astype(np.float32))
save("empty_axis.npy", np.zeros((0, 4), dtype=np.float32))

# Refused.
dt = np.dtype([("a", "<i4"), ("b", "<f8")])
save("structured.npy", np.zeros(5, dtype=dt))
save("object.npy", np.array([{"a": 1}, [1, 2]], dtype=object), allow_pickle=True)
save("strings.npy", np.array(["ab", "cde"]))
save("datetime.npy", np.array(["2020-01-01", "2021-06-01"], dtype="datetime64[D]"))

# Header versions 2.0 and 3.0.
for ver, name in [((2, 0), "v2.npy"), ((3, 0), "v3.npy")]:
    a = rng.random((16, 16)).astype(np.float32)
    with open(os.path.join(out, name), "wb") as f:
        np.lib.format.write_array(f, a, version=ver)
    ref[name] = {"shape": [16, 16], "dtype": "float32", "fortran": False, "min": float(a.min()), "max": float(a.max()), "mean": float(a.astype(np.float64).mean()), "samples": [[0, float(a.ravel()[0])], [255, float(a.ravel()[255])]]}

# Archives.
np.savez(os.path.join(out, "arrays.npz"), vector=np.arange(10, dtype=np.float32), picture=sphere((1, 32, 32), np.float32)[0], volume=vol.astype(np.int16))
np.savez_compressed(os.path.join(out, "arrays_compressed.npz"), counts=np.arange(5), image=rng.integers(0, 255, (40, 40), dtype=np.uint8), volume=vol.astype(np.float32))
with open(os.path.join(out, "reference.json"), "w") as f:
    json.dump(ref, f, indent=1)
print("wrote", len(ref), "arrays to", out)
