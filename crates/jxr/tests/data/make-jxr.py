#!/usr/bin/env python3
"""Writes small JPEG XR test files with Microsoft's own encoder (JxrEncApp,
from the libjxr-tools package) and the pixels its decoder (JxrDecApp) turns
them into, for tests/decode.rs.  Needs numpy, Pillow and OpenImageIO:

    python3 -I make-jxr.py OUT_DIR

NAME.jxr comes with NAME.rgba (8-bit RGBA bytes, straight alpha) or
NAME.f32 (little-endian float32 RGBA), the picture as slerp-jxr widens it:
8-bit pictures and 16-bit or packed ones reduced to 8 bits by jxrlib's own
converters, float and half ones kept.  manifest.txt lists

    NAME width height kind

where kind is u8 or f32.
"""

import os
import subprocess
import sys
import tempfile

import numpy as np
import OpenImageIO as oiio
from PIL import Image

out = sys.argv[1]
manifest = []
tmp = tempfile.mkdtemp(prefix="slerp-jxr-")


def picture(w, h, channels):
    y, x = np.mgrid[0:h, 0:w]
    planes = [x / (w - 1), y / (h - 1), 0.5 + 0.4 * np.sin((x + y) / 5.0), 0.25 + 0.5 * ((x // 6 + y // 6) % 2)]
    return np.clip(np.stack(planes[:channels], axis=-1), 0, 1)


def run(*args):
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"{args[0]} failed: {r.stdout} {r.stderr}")


def write_tiff(path, arr):
    """8-bit and 16-bit integer or float32 TIFF.  OpenImageIO writes grey
    and RGB; its RGBA files are rejected by JxrEncApp, so ImageMagick
    writes those from raw samples."""
    h, w, c = arr.shape
    if c == 4:
        raw = f"{path}.raw"
        arr.astype(arr.dtype.newbyteorder("<")).tofile(raw)
        depth = {"uint8": 8, "uint16": 16, "float32": 32}[arr.dtype.name]
        extra = ["-define", "quantum:format=floating-point"] if arr.dtype.name == "float32" else []
        run("convert", "-size", f"{w}x{h}", "-depth", str(depth), *extra, "-endian", "LSB", f"rgba:{raw}", "-compress", "None", path)
        return
    fmt = {np.dtype("uint8"): oiio.UINT8, np.dtype("uint16"): oiio.UINT16, np.dtype("float32"): oiio.FLOAT}[arr.dtype]
    o = oiio.ImageOutput.create(path)
    spec = oiio.ImageSpec(w, h, c, fmt)
    # JxrEncApp reads only uncompressed TIFF, in one strip.
    spec.attribute("compression", "none")
    spec.attribute("tiff:rowsperstrip", h)
    if c == 4:
        spec.alpha_channel = 3
        spec.channelnames = ("R", "G", "B", "A")
    assert o.open(path, spec), o.geterror()
    o.write_image(arr)
    o.close()


def read_image(path):
    inp = oiio.ImageInput.open(path)
    assert inp is not None, oiio.geterror()
    spec = inp.spec()
    data = np.asarray(inp.read_image(oiio.FLOAT)).reshape(spec.height, spec.width, spec.nchannels)
    inp.close()
    return data


def rgba_bytes(data):
    """Float 0..1 samples in 1, 2, 3 or 4 channels to straight RGBA8."""
    h, w, c = data.shape
    b = np.round(np.clip(data, 0, 1) * 255).astype(np.uint8)
    if c == 1:
        b = np.concatenate([b, b, b, np.full_like(b, 255)], axis=-1)
    elif c == 3:
        b = np.concatenate([b, np.full((h, w, 1), 255, np.uint8)], axis=-1)
    return b


def encode(name, source, fmt, extra=()):
    """Encode `source` (a TIFF) as `fmt` and return the path of the .jxr."""
    jxr = f"{out}/{name}.jxr"
    run("JxrEncApp", "-i", source, "-o", jxr, "-c", str(fmt), "-q", "1", *extra)
    return jxr


def reference(jxr, fmt, ext="tif"):
    """The picture JxrDecApp makes from `jxr` in pixel format `fmt`."""
    path = f"{tmp}/{os.path.basename(jxr)}.{ext}"
    run("JxrDecApp", "-i", jxr, "-o", path, "-c", str(fmt))
    return read_image(path)


def ldr(name, arr, fmt, ref_fmt, extra=(), ref_ext="tif", expect=None):
    """`expect` gives the 8-bit picture directly where JxrDecApp's own
    conversion is no reference (its grey transcode path garbles 16-bit)."""
    src = f"{tmp}/{name}.tif"
    write_tiff(src, arr)
    jxr = encode(name, src, fmt, extra)
    if expect is not None:
        data = expect / 255.0
    else:
        data = reference(jxr, ref_fmt, ref_ext)
    h, w = data.shape[:2]
    rgba_bytes(data).tofile(f"{out}/{name}.rgba")
    manifest.append(f"{name} {w} {h} u8")


def floats(name, arr, fmt, extra=(), expect=None, kind="f32"):
    """Float and half pictures against their source: the codec keeps
    non-negative values exactly (it is JxrDecApp's TIFF writer that can't be
    read back as a reference).  `kind` f32lossy marks pictures stored with a
    quality below lossless, which the test compares loosely."""
    src = f"{tmp}/{name}.tif"
    write_tiff(src, arr)
    encode(name, src, fmt, extra)
    data = arr if expect is None else expect
    h, w, c = data.shape
    if c == 1:
        data = np.concatenate([data] * 3 + [np.ones_like(data)], axis=-1)
    elif c == 3:
        data = np.concatenate([data, np.ones((h, w, 1), data.dtype)], axis=-1)
    data.astype("<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {w} {h} {kind}")


W, H = 40, 24
p3 = picture(W, H, 3)
p4 = picture(W, H, 4)
u8 = lambda a: np.round(a * 255).astype(np.uint8)
u16 = lambda a: np.round(a * 65535).astype(np.uint16)

ldr("gray8", u8(p3[..., :1]), 2, 2, ref_ext="pnm")
# jxrlib drops 16-bit samples to 8 by taking the high byte.
ldr("gray16", u16(p3[..., :1]), 3, 2, ref_ext="pnm", expect=(u16(p3[..., :1]) >> 8).astype(np.float64))
ldr("rgb24", u8(p3), 9, 9, ref_ext="pnm")
ldr("bgr24", u8(p3), 0, 9, ref_ext="pnm")
ldr("rgb16", u16(p3), 10, 9, ref_ext="pnm", expect=(u16(p3) >> 8).astype(np.float64))
ldr("rgba8_planar", u8(p4), 22, 22, extra=("-a", "2"), expect=u8(p4).astype(np.float64))
ldr("rgba8_interleaved", u8(p4), 22, 22, extra=("-a", "3"), expect=u8(p4).astype(np.float64))
ldr("rgba16", u16(p4), 23, 22, expect=(u16(p4) >> 8).astype(np.float64))
ldr("rgb101010", u16(p3), 31, 9, ref_ext="pnm", expect=(u16(p3) >> 8).astype(np.float64))
ldr("rgb565", u8(p3), 30, 9, ref_ext="pnm")
ldr("rgb555", u8(p3), 29, 9, ref_ext="pnm")
# Tiles and chroma subsampling, on a picture big enough to have several.
big = picture(160, 96, 3)
ldr("rgb24_tiled", u8(big), 9, 9, extra=("-U", "2", "3"), ref_ext="pnm")
ldr("rgb24_yuv420", u8(big), 9, 9, extra=("-d", "1", "-q", "0.6"), ref_ext="pnm")
ldr("rgb24_yuv422_tiled", u8(big), 9, 9, extra=("-d", "2", "-q", "0.7", "-U", "2", "2"), ref_ext="pnm")
ldr("odd_size", u8(picture(37, 29, 3)), 9, 9, ref_ext="pnm")

# Float and half: HDR values above 1.  JxrEncApp's float TIFF reading and
# 128-bit encoding don't round-trip (a bug in the tool), so the 128-bit float
# files have no expected values: the test compares slerp-jxr with an
# independent decoder instead ("oracle").  Half-float RGB round-trips.
hdr3 = (p3 * 4.0).astype(np.float32)
# ImageMagick, which writes the RGBA TIFFs, clamps floats to 0..1.
hdr4 = np.concatenate([np.clip(hdr3, 0, 1), p4[..., 3:]], axis=-1).astype(np.float32)
half = lambda a: a.astype(np.float16).astype(np.float32)
for name, arr, fmt, extra in [("rgb_float", hdr3, 15, ()), ("rgba_float", hdr4, 28, ("-a", "2")), ("gray_float", hdr3[..., :1].copy(), 8, ())]:
    write_tiff(f"{tmp}/{name}.tif", arr)
    encode(name, f"{tmp}/{name}.tif", fmt, extra)
    manifest.append(f"{name} {W} {H} oracle")
floats("rgb_half", hdr3, 12, expect=half(hdr3))
floats("rgba_half", hdr4, 25, extra=("-a", "2"), expect=half(hdr4))
write_tiff(f"{tmp}/lossy_float.tif", hdr4)
run("JxrEncApp", "-i", f"{tmp}/lossy_float.tif", "-o", f"{out}/rgba_half_lossy.jxr", "-c", "25", "-q", "0.8", "-a", "2")
half(hdr4).astype("<f4").tofile(f"{out}/rgba_half_lossy.f32")
manifest.append(f"rgba_half_lossy {W} {H} f32lossy")

open(f"{out}/manifest.txt", "w").write("\n".join(manifest) + "\n")
print(len(manifest), "cases")
