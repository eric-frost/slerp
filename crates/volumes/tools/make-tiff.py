#!/usr/bin/env python3
"""Writes TIFF test files and reference.json to local/assets/volumes/tiff-generated/,
with tifffile (pip install tifffile imagecodecs numpy).

    local/pyenv-vol/bin/python crates/volumes/tools/make-tiff.py

reference.json: for each file the axes tifffile saw (letters such as TZCYXS),
the shape, and values at a few positions, which the Rust tests compare with.
"""
import json
import os
import sys
import numpy as np
import tifffile

out = sys.argv[1] if len(sys.argv) > 1 else "local/assets/volumes/tiff-generated"
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(31)
ref = {}


def picks(a, n=4):
    res = []
    for _ in range(n):
        idx = tuple(int(rng.integers(0, s)) for s in a.shape)
        v = a[idx]
        res.append([list(idx), float(v) if np.ndim(v) == 0 else None])
    return [p for p in res if p[1] is not None]


def save(name, data, axes, lossy=False, **kw):
    path = os.path.join(out, name)
    tifffile.imwrite(path, data, **kw)
    ref[name] = {"axes": axes, "shape": list(data.shape), "dtype": str(data.dtype), "lossy": lossy, "samples": picks(data)}


def image(shape, dtype, scale=1.0):
    y, x = np.indices(shape[-2:]).astype(np.float64)
    base = (np.sin(x / 7.0) + np.cos(y / 5.0) + 2) / 4 * scale
    a = np.broadcast_to(base, shape).copy()
    a += rng.random(shape) * scale * 0.05
    if np.dtype(dtype).kind in "ui":
        info = np.iinfo(dtype)
        a = np.clip(a, 0, info.max)
    return a.astype(dtype)


# 1. Single pages: every sample type and compression.
save("gray_u8.tif", image((96, 128), np.uint8, 200), "YX", photometric="minisblack")
save("gray_u16_lzw_pred.tif", image((96, 128), np.uint16, 60000), "YX", compression="lzw", predictor=2, photometric="minisblack")
save("gray_u16_deflate.tif", image((96, 128), np.uint16, 60000), "YX", compression="zlib", photometric="minisblack")
save("gray_i16_packbits.tif", (image((96, 128), np.int16, 30000) - 5000).astype(np.int16), "YX", compression="packbits", photometric="minisblack")
save("gray_u32_zstd.tif", image((60, 70), np.uint32, 4e9), "YX", compression="zstd", photometric="minisblack")
save("gray_f32.tif", image((96, 128), np.float32, 1.0) - 0.25, "YX", photometric="minisblack")
save("gray_f32_pred3_deflate.tif", image((96, 128), np.float32, 100.0), "YX", compression="zlib", predictor=3, photometric="minisblack")
save("gray_f64_pred3_lzw.tif", image((50, 64), np.float64, 5.0), "YX", compression="lzw", predictor=3, photometric="minisblack")
save("gray_f16.tif", image((50, 64), np.float16, 5.0), "YX", photometric="minisblack")
save("gray_u16_bigendian.tif", image((96, 128), np.uint16, 60000), "YX", byteorder=">", photometric="minisblack")
save("gray_i8.tif", (image((40, 50), np.uint8, 120).astype(np.int16) - 60).astype(np.int8), "YX", photometric="minisblack")
save("bigtiff_u16.tif", image((96, 128), np.uint16, 60000), "YX", bigtiff=True, photometric="minisblack")
save("tiled_edge_u16.tif", image((75, 100), np.uint16, 60000), "YX", tile=(32, 32), photometric="minisblack", compression="deflate")
save("tiled_lzw_pred_rgb.tif", image((75, 100, 3), np.uint8, 250), "YXS", tile=(32, 32), photometric="rgb", compression="lzw", predictor=2)
save("rgb_u8_strips.tif", image((60, 80, 3), np.uint8, 250), "YXS", photometric="rgb")
save("rgb_u16_planar.tif", image((3, 60, 80), np.uint16, 60000), "SYX", photometric="rgb", planarconfig="separate", compression="zlib")
save("rgb_planar_lzw_pred.tif", image((3, 60, 80), np.uint8, 250), "SYX", photometric="rgb", planarconfig="separate", compression="lzw", predictor=2)
save("rgba_u8.tif", image((60, 80, 4), np.uint8, 250), "YXS", photometric="rgb", extrasamples=["unassalpha"])
save("rgb_jpeg_strips.tif", image((100, 130, 3), np.uint8, 250), "YXS", photometric="rgb", compression="jpeg", lossy=True)
save("gray_jpeg.tif", image((100, 130), np.uint8, 250), "YX", photometric="minisblack", compression="jpeg", lossy=True)
save("miniswhite_u8.tif", image((40, 50), np.uint8, 250), "YX", photometric="miniswhite")
cm = (np.stack([np.arange(256), 255 - np.arange(256), np.full(256, 128)]) * 257).astype(np.uint16)
tifffile.imwrite(os.path.join(out, "palette.tif"), rng.integers(0, 256, (40, 50), dtype=np.uint8), photometric="palette", colormap=cm)
ref["palette.tif"] = {"axes": "YX", "shape": [40, 50], "dtype": "uint8", "palette": True}
save("zstd_u16.tif", image((96, 128), np.uint16, 60000), "YX", compression="zstd", photometric="minisblack")

# 2. Stacks and hyperstacks.
save("stack_u16.tif", image((30, 48, 64), np.uint16, 60000), "ZYX", photometric="minisblack")
save("stack_u16_lzw.tif", image((30, 48, 64), np.uint16, 60000), "ZYX", photometric="minisblack", compression="lzw", predictor=2)
save("stack_rgb.tif", image((6, 40, 50, 3), np.uint8, 250), "ZYXS", photometric="rgb")
data5 = image((3, 2, 4, 40, 50), np.uint16, 60000)  # T C Z Y X
save("ome_tczyx.ome.tif", data5, "TCZYX", ome=True, metadata={"axes": "TCZYX", "Channel": {"Name": ["DAPI", "GFP"], "Color": [65535, 16711935]}, "PhysicalSizeX": 0.25, "PhysicalSizeY": 0.25, "PhysicalSizeZ": 1.5, "TimeIncrement": 2.0})
save("ome_zcyx.ome.tif", image((5, 3, 40, 50), np.uint16, 60000), "ZCYX", ome=True, metadata={"axes": "ZCYX"})
save("ome_tzyx_big.ome.tif", image((4, 5, 40, 50), np.uint8, 250), "TZYX", ome=True, metadata={"axes": "TZYX"}, bigtiff=True)
save("ome_rgb.ome.tif", image((3, 40, 50, 3), np.uint8, 250), "ZYXS", ome=True, photometric="rgb", metadata={"axes": "ZYXS"})
save("ome_compressed.ome.tif", image((2, 3, 40, 50), np.uint16, 60000), "ZCYX", ome=True, metadata={"axes": "ZCYX"}, compression="zlib", predictor=2)
data_ij = image((3, 4, 2, 40, 50), np.uint16, 60000)  # T Z C Y X
save("imagej_hyperstack.tif", data_ij, "TZCYX", imagej=True, metadata={"axes": "TZCYX", "spacing": 0.5, "unit": "um"}, resolution=(4.0, 4.0))
save("imagej_stack_float.tif", image((10, 30, 40), np.float32, 1.0), "ZYX", imagej=True, metadata={"axes": "ZYX"})

# 3. Pages that differ.
path = os.path.join(out, "pages_mixed.tif")
tifffile.imwrite(path, image((100, 80), np.uint8, 250), photometric="minisblack")
tifffile.imwrite(path, image((50, 40), np.uint8, 250), photometric="minisblack", append=True)
tifffile.imwrite(path, image((30, 30, 3), np.uint8, 250), photometric="rgb", append=True)
ref["pages_mixed.tif"] = {"axes": "YX", "shape": [100, 80], "dtype": "uint8", "mixed": [[100, 80], [50, 40], [30, 30]]}

# 4. A pyramid with sub-resolutions as separate pages (generic pyramidal TIFF).
big = image((1200, 1600, 3), np.uint8, 250)
path = os.path.join(out, "pyramid_generic.tif")
tifffile.imwrite(path, big, tile=(128, 128), photometric="rgb", compression="deflate")
for k in (2, 4, 8):
    tifffile.imwrite(path, big[::k, ::k], tile=(128, 128), photometric="rgb", compression="deflate", append=True, subfiletype=1)
ref["pyramid_generic.tif"] = {"axes": "YXS", "shape": [1200, 1600, 3], "dtype": "uint8", "pyramid": [[1600, 1200], [800, 600], [400, 300], [200, 150]]}

# 5. Larger files for timing.
if True:
    big_stack = np.stack([image((512, 512), np.uint16, 50000) for _ in range(120)])
    tifffile.imwrite(os.path.join(out, "big_stack_lzw.tif"), big_stack, photometric="minisblack", compression="lzw", predictor=2)
    ref["big_stack_lzw.tif"] = {"axes": "ZYX", "shape": list(big_stack.shape), "dtype": "uint16", "lossy": False, "samples": picks(big_stack)}
    tifffile.imwrite(os.path.join(out, "big_plain.tif"), big_stack[:6, :, :].astype(np.uint16), photometric="minisblack")

json.dump(ref, open(os.path.join(out, "reference.json"), "w"), indent=1)
print("wrote", len(ref), "entries")
