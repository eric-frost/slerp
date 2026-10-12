#!/usr/bin/env python3
"""Writes the small XISF test files and the pixel values they must decode
to, for tests/xisf.rs.  Run with numpy, the `xisf` package, zstandard,
lz4 and numpy installed:

    python3 -I make-xisf.py OUT_DIR

NAME.xisf comes with NAME.f32, the expected samples as little-endian
float32 in (row, column, channel) order, integers divided by their type's
largest value as XISF defines the representable range.  The `xisf`
package writes the codec variants (an independent encoder of PixInsight's
format); the files written by hand here cover what that package doesn't:
big-endian samples, planar and normal storage, subblocks, embedded data,
colour filter arrays and orientations.
"""

import base64
import struct
import sys
import zlib

import numpy as np
import xisf

out = sys.argv[1]
rng = np.random.default_rng(7)
manifest = []
H, W = 24, 40


def picture(channels, dtype):
    y, x = np.mgrid[0:H, 0:W]
    planes = []
    for c in range(channels):
        base = 0.15 + 0.25 * x / W + 0.2 * y / H + 0.1 * c
        base = base + rng.normal(0, 0.0004, (H, W))
        base[5 + c, 7:12] = 0.95
        planes.append(np.clip(base, 0, 1))
    img = np.stack(planes, axis=-1)
    if np.issubdtype(dtype, np.integer):
        top = np.iinfo(dtype).max
        return np.round(img * top).astype(dtype)
    return img.astype(dtype)


def normalised(a):
    if np.issubdtype(a.dtype, np.integer):
        return (a.astype(np.float64) / np.iinfo(a.dtype).max).astype("<f4")
    return a.astype("<f4")


def save_expected(name, a, note=""):
    normalised(a).tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {a.shape[1]} {a.shape[0]} {a.shape[2]} {note}".strip())


# 1. Files from the `xisf` package, one codec each.
cases = [
    ("u16_plain", np.uint16, 1, None, False),
    ("u16_zlib_sh", np.uint16, 1, "zlib", True),
    ("u16_lz4_sh", np.uint16, 1, "lz4", True),
    ("u16_lz4hc_sh", np.uint16, 1, "lz4hc", True),
    ("u16_zstd_sh", np.uint16, 1, "zstd", True),
    ("u16_zlib", np.uint16, 1, "zlib", False),
    ("u8_rgb_zstd", np.uint8, 3, "zstd", False),
    ("f32_gray_zstd_sh", np.float32, 1, "zstd", True),
    ("f32_rgb_lz4hc_sh", np.float32, 3, "lz4hc", True),
    ("u32_gray", np.uint32, 1, "zlib", True),
    ("f64_gray", np.float64, 1, None, False),
]
for name, dtype, channels, codec, shuffle in cases:
    a = picture(channels, dtype)
    xisf.XISF.write(
        f"{out}/{name}.xisf",
        a,
        creator_app="slerp tests",
        image_metadata={"FITSKeywords": {"OBJECT": [{"value": "M 31", "comment": "target"}], "EXPTIME": [{"value": "120", "comment": ""}]}},
        codec=codec,
        shuffle=shuffle,
    )
    save_expected(name, a)


# 2. Files written by hand.
def write_raw(name, xml_attrs, block, children="", sample_data=None):
    """A monolithic file with one image whose block is attached."""
    def header(pos):
        return (
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<xisf version="1.0" xmlns="http://www.pixinsight.com/xisf">\n'
            f'<Image {xml_attrs} location="attachment:{pos:010d}:{len(block):010d}">{children}</Image>\n'
            "</xisf>"
        )

    h = header(0)
    pos = 16 + len(h.encode())
    h = header(pos).encode()
    with open(f"{out}/{name}.xisf", "wb") as f:
        f.write(b"XISF0100" + struct.pack("<I", len(h)) + b"\0\0\0\0" + h + block)


def planar_bytes(a, dtype):
    # (H, W, C) to channel-after-channel.
    return np.ascontiguousarray(np.moveaxis(a, -1, 0)).astype(dtype).tobytes()


img = picture(3, np.uint16)
write_raw(
    "u16_be_planar_rgb",
    f'geometry="{W}:{H}:3" sampleFormat="UInt16" colorSpace="RGB" pixelStorage="Planar" byteOrder="big"',
    planar_bytes(img, ">u2"),
)
save_expected("u16_be_planar_rgb", img)

write_raw(
    "u16_le_normal_rgb",
    f'geometry="{W}:{H}:3" sampleFormat="UInt16" colorSpace="RGB" pixelStorage="Normal"',
    np.ascontiguousarray(img).astype("<u2").tobytes(),
)
save_expected("u16_le_normal_rgb", img)

# Subblocks: three independent zlib streams, bytes shuffled over the whole.
g = picture(1, np.uint16)
raw = g.astype("<u2").tobytes()
n = len(raw) // 2
shuffled = bytes(raw[i * 2 + b] for b in range(2) for i in range(n))
third = len(shuffled) // 3
pieces = [shuffled[:third], shuffled[third : 2 * third], shuffled[2 * third :]]
comp = [zlib.compress(p) for p in pieces]
sub = ":".join(f"{len(c)},{len(p)}" for c, p in zip(comp, pieces))
write_raw(
    "u16_zlib_subblocks",
    f'geometry="{W}:{H}:1" sampleFormat="UInt16" compression="zlib+sh:{len(raw)}:2" subblocks="{sub}"',
    b"".join(comp),
)
save_expected("u16_zlib_subblocks", g)

# Embedded, base64, uncompressed 8-bit gray.
g8 = picture(1, np.uint8)
b64 = base64.b64encode(g8.tobytes()).decode()
xml = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<xisf version="1.0" xmlns="http://www.pixinsight.com/xisf">\n'
    f'<Image geometry="{W}:{H}:1" sampleFormat="UInt8" location="embedded"><Data encoding="base64">{b64}</Data></Image>\n'
    "</xisf>"
).encode()
with open(f"{out}/u8_embedded.xisf", "wb") as f:
    f.write(b"XISF0100" + struct.pack("<I", len(xml)) + b"\0\0\0\0" + xml)
save_expected("u8_embedded", g8)

# A mosaic with its colour filter array, and a flip.
mosaic = np.zeros((H, W, 1), np.uint16)
mosaic[0::2, 0::2] = 50000
mosaic[0::2, 1::2] = 30000
mosaic[1::2, 0::2] = 30000
mosaic[1::2, 1::2] = 8000
write_raw(
    "cfa_rggb",
    f'geometry="{W}:{H}:1" sampleFormat="UInt16"',
    mosaic.astype("<u2").tobytes(),
    children='<ColorFilterArray pattern="RGGB" width="2" height="2"/>',
)
save_expected("cfa_rggb", mosaic)

write_raw(
    "u16_flip",
    f'geometry="{W}:{H}:1" sampleFormat="UInt16" orientation="flip"',
    g.astype("<u2").tobytes(),
)
save_expected("u16_flip", g[:, ::-1, :])

write_raw(
    "u16_turn90",
    f'geometry="{W}:{H}:1" sampleFormat="UInt16" orientation="90"',
    g.astype("<u2").tobytes(),
)
# A quarter turn counter-clockwise: rotate the array by 90 degrees.
save_expected("u16_turn90", np.rot90(g, 1))

write_raw(
    "f32_nan",
    f'geometry="{W}:{H}:1" sampleFormat="Float32" bounds="0:1"',
    np.where(g > 30000, np.nan, g / 65535.0).astype("<f4").tobytes(),
)
nanf = np.where(g > 30000, np.nan, g / 65535.0).astype("<f4")
nanf.tofile(f"{out}/f32_nan.f32")
manifest.append(f"f32_nan {W} {H} 1")

open(f"{out}/manifest.txt", "w").write("\n".join(manifest) + "\n")
print(len(manifest), "cases")
