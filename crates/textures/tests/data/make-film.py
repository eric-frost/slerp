#!/usr/bin/env python3
"""Writes the small DPX, Cineon, PAM and farbfeld test files and the sample
values an independent reader (OpenImageIO, for DPX and Cineon) finds in
them, for tests/film.rs.  Run with numpy and OpenImageIO installed:

    python3 -I make-film.py OUT_DIR

DPX files are written by OpenImageIO.  OpenImageIO has no Cineon writer, so
the Cineon files are written here from the format's layout, then read back
through OpenImageIO's Cineon reader, which is what the expected values are
(a mismatch between the two would fail this script).  NAME.f32 holds the
codes as OpenImageIO reads them, divided by the largest code (float32,
rows top to bottom, channels interleaved).  manifest.txt has one line per
file:

    NAME width height channels transfer [note]
"""

import struct
import sys

import numpy as np
import OpenImageIO as oiio

out = sys.argv[1]
manifest = []
W, H = 32, 20
rng = np.random.default_rng(11)


def picture(channels):
    y, x = np.mgrid[0:H, 0:W]
    planes = [x / (W - 1), y / (H - 1), 0.5 + 0.4 * np.sin((x + y) / 5.0), 0.3 + 0.7 * ((x // 4 + y // 4) % 2)]
    img = np.stack(planes[:channels], axis=-1) if channels > 1 else planes[0][..., None]
    return np.clip(img, 0, 1).astype(np.float32)


def read_back(path):
    inp = oiio.ImageInput.open(path)
    assert inp is not None, oiio.geterror()
    spec = inp.spec()
    data = inp.read_image(oiio.FLOAT)
    inp.close()
    return np.asarray(data).reshape(spec.height, spec.width, spec.nchannels), spec


def record(name, path, transfer, note=""):
    data, spec = read_back(path)
    data.astype("<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {spec.width} {spec.height} {spec.nchannels} {transfer} {note}".strip())


# 1. DPX from OpenImageIO.
def refill_12_bit(path, endian, filled_b):
    """OpenImageIO writes 12-bit samples only as a continuous stream.  Make
    the filled forms from the same codes: each sample in a 16-bit unit,
    either shifted up four bits (method A) or kept in the low bits (B)."""
    codes = np.round(read_back(path)[0] * 4095).astype(np.uint16)
    e = ">" if endian == "big" else "<"
    raw = bytearray(open(path, "rb").read())
    offset = struct.unpack(e + "I", raw[4:8])[0]
    struct.pack_into(e + "H", raw, 0x324, 2 if filled_b else 1)
    data = (codes if filled_b else codes << 4).reshape(-1).astype(e + "u2").tobytes()
    raw[offset : offset + len(data)] = data
    open(path, "wb").write(bytes(raw))


def dpx(name, bits, endian, packing, channels, transfer="display", attrs=None, ext="dpx"):
    path = f"{out}/{name}.dpx"
    o = oiio.ImageOutput.create(path)
    spec = oiio.ImageSpec(W, H, channels, oiio.UINT16 if bits > 8 else oiio.UINT8)
    spec.attribute("oiio:BitsPerSample", bits)
    spec.attribute("oiio:Endian", endian)
    if packing:
        spec.attribute("dpx:Packing", packing)
    spec.attribute("dpx:Transfer", {"display": "User defined", "linear": "Linear", "log": "Logarithmic"}[transfer])
    for k, v in (attrs or {}).items():
        spec.attribute(k, v)
    assert o.open(path, spec), o.geterror()
    o.write_image(picture(channels))
    o.close()
    if bits == 12 and packing and packing.startswith("Filled"):
        data, _ = read_back(path)
        refill_12_bit(path, endian, packing.endswith("B"))
    note = "" if not attrs or "PixelAspectRatio" not in attrs else f"aspect {attrs['PixelAspectRatio']}"
    record(name, path, transfer, note)


for endian in ("big", "little"):
    tag = "be" if endian == "big" else "le"
    dpx(f"dpx_8_{tag}", 8, endian, None, 3)
    dpx(f"dpx_10_{tag}_a", 10, endian, "Filled, method A", 3)
    dpx(f"dpx_10_{tag}_b", 10, endian, "Filled, method B", 3)
    dpx(f"dpx_10_{tag}_packed", 10, endian, "Packed", 3)
    dpx(f"dpx_12_{tag}_a", 12, endian, "Filled, method A", 3)
    dpx(f"dpx_12_{tag}_b", 12, endian, "Filled, method B", 3)
    dpx(f"dpx_12_{tag}_packed", 12, endian, "Packed", 3)
    dpx(f"dpx_16_{tag}", 16, endian, None, 3)
dpx("dpx_10_rgba", 10, "big", "Filled, method A", 4)
dpx("dpx_16_rgba", 16, "little", None, 4)
dpx("dpx_8_rgba", 8, "big", None, 4)
dpx("dpx_10_gray", 10, "big", "Filled, method A", 1)
dpx("dpx_16_linear", 16, "big", None, 3, transfer="linear")
dpx("dpx_10_log", 10, "big", "Filled, method A", 3, transfer="log")
dpx("dpx_10_eol", 10, "big", "Filled, method A", 3, attrs={"dpx:EndOfLinePadding": 12})
dpx("dpx_10_aspect", 10, "big", "Filled, method A", 3, attrs={"PixelAspectRatio": 2.0})
dpx(
    "dpx_10_fields",
    10,
    "little",
    "Filled, method A",
    3,
    attrs={
        "dpx:FramePosition": 7,
        "dpx:SequenceLength": 100,
        "dpx:FrameRate": 24.0,
        "dpx:SlateInfo": "Slate here",
        "dpx:TimeCode": "01:02:03:04",
        "dpx:Copyright": "(c) test",
        "dpx:Project": "A Project",
        "dpx:FrameId": "FID 1",
    },
)


# 2. Cineon, written by hand.
def cineon(name, bits, channels, interleave, packing_code, eol=0, eoc=0, little=False):
    """A Cineon file: the generic header's 1024 bytes, then the film section
    (empty here), then the samples."""
    img = picture(channels)
    top = (1 << bits) - 1
    codes = np.round(img * top).astype(np.uint32)
    e = "<" if little else ">"
    head = bytearray(2048)

    def put(at, fmt, *vals):
        struct.pack_into(e + fmt, head, at, *vals)

    head[0:4] = bytes([0xD7, 0x5F, 0x2A, 0x80]) if little else bytes([0x80, 0x2A, 0x5F, 0xD7])
    head[24:32] = b"V4.5".ljust(8, b"\0")
    head[32:32 + 12] = b"frame.cin".ljust(12, b"\0")
    head[192] = 0  # orientation
    head[193] = channels
    for c in range(channels):
        base = 196 + 28 * c
        head[base] = 0
        head[base + 1] = [1, 2, 3][c] if channels == 3 else 0
        head[base + 2] = bits
        put(base + 4, "II", W, H)
        put(base + 12, "ffff", 0.0, 0.0, float(top), 2.048)
    head[680] = interleave
    head[681] = packing_code
    head[682] = 0
    head[683] = 0
    put(684, "I", eol)
    put(688, "I", eoc)

    def row_bytes(samples):
        if bits == 8:
            return bytes_from(samples, 1)
        if bits == 16:
            return b"".join(struct.pack(e + "H", int(v)) for v in samples)
        if bits == 10 and packing_code == 5:
            # Three samples to a 32-bit word, left justified.
            out = bytearray()
            for i in range(0, len(samples), 3):
                chunk = list(samples[i : i + 3]) + [0, 0]
                word = (int(chunk[0]) << 22) | (int(chunk[1]) << 12) | (int(chunk[2]) << 2)
                out += struct.pack(e + "I", word)
            return bytes(out)
        if bits == 12 and packing_code == 3:
            return b"".join(struct.pack(e + "H", int(v) << 4) for v in samples)
        raise ValueError((bits, packing_code))

    def bytes_from(samples, size):
        return bytes(int(v) for v in samples)

    body = bytearray()
    if interleave == 0:
        for y in range(H):
            body += row_bytes(codes[y].reshape(-1)) + bytes(eol)
    elif interleave == 1:
        for y in range(H):
            for c in range(channels):
                body += row_bytes(codes[y, :, c]) + bytes(eol)
    else:
        for c in range(channels):
            for y in range(H):
                body += row_bytes(codes[y, :, c]) + bytes(eol)
            body += bytes(eoc)
    put(4, "I", 2048)
    path = f"{out}/{name}.cin"
    with open(path, "wb") as f:
        f.write(bytes(head) + bytes(body))
    # The independent reader must agree with what was written, where it
    # reads that layout at all (OpenImageIO ignores end-of-line padding and mis-reads line-interleaved files).
    if eol == 0 and eoc == 0 and interleave == 0:
        try:
            data, spec = read_back(path)
        except ValueError:
            print(name, "is not read by OpenImageIO; expected values come from the source")
        else:
            assert np.allclose(data, codes / top, atol=1.0 / top), (name, np.abs(data - codes / top).max())
    (codes / top).astype("<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {W} {H} {channels} log")


cineon("cin_10_rgb", 10, 3, 0, 5)
cineon("cin_10_rgb_le", 10, 3, 0, 5, little=True)
cineon("cin_10_rgb_eol", 10, 3, 0, 5, eol=8)
cineon("cin_10_planes", 10, 3, 2, 5, eol=0, eoc=4)
cineon("cin_10_lines", 10, 3, 1, 5)
cineon("cin_8_rgb", 8, 3, 0, 1)
cineon("cin_12_rgb", 12, 3, 0, 3)
cineon("cin_16_rgb", 16, 3, 0, 3)
cineon("cin_10_gray", 10, 1, 0, 5)

# 3. PAM and farbfeld.
rgba = (picture(4) * 255).round().astype(np.uint8)
wide = (picture(4) * 65535).round().astype(np.uint16)


def pam(name, header, body):
    with open(f"{out}/{name}.pam", "wb") as f:
        f.write(header.encode() + body)


pam("pam_rgba8", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n", rgba.tobytes())
pam("pam_rgb16", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 3\nMAXVAL 65535\nTUPLTYPE RGB\nENDHDR\n", wide[..., :3].astype(">u2").tobytes())
pam("pam_gray8", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 1\nMAXVAL 255\nTUPLTYPE GRAYSCALE\nENDHDR\n", rgba[..., 0].tobytes())
pam("pam_graya8", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 2\nMAXVAL 255\nTUPLTYPE GRAYSCALE_ALPHA\nENDHDR\n", rgba[..., :2].tobytes())
pam("pam_bw", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 1\nMAXVAL 1\nTUPLTYPE BLACKANDWHITE\nENDHDR\n", (rgba[..., 0] > 127).astype(np.uint8).tobytes())
pam("pam_gray16", f"P7\nWIDTH {W}\nHEIGHT {H}\nDEPTH 1\nMAXVAL 65535\nTUPLTYPE GRAYSCALE\nENDHDR\n", wide[..., 0].astype(">u2").tobytes())
np.save(f"{out}/.rgba8.npy", rgba)
with open(f"{out}/farbfeld.ff", "wb") as f:
    f.write(b"farbfeld" + struct.pack(">II", W, H) + wide.astype(">u2").tobytes())
rgba.tofile(f"{out}/rgba8.raw")
wide.astype("<u2").tofile(f"{out}/rgba16.raw")

# ImageMagick reads the PAM and farbfeld files as written (a check on the
# writers above, not on slerp).
import subprocess


def magick(path, depth, channels):
    r = subprocess.run(["convert", path, "-depth", str(depth), "-endian", "MSB", f"{channels}:-"], capture_output=True)
    assert r.returncode == 0, r.stderr
    return r.stdout


assert magick(f"{out}/pam_rgba8.pam", 8, "rgba") == rgba.tobytes()
assert magick(f"{out}/pam_rgb16.pam", 16, "rgb") == wide[..., :3].astype(">u2").tobytes()
assert magick(f"{out}/farbfeld.ff", 16, "rgba") == wide.astype(">u2").tobytes()

open(f"{out}/manifest.txt", "w").write("\n".join(manifest) + "\n")
print(len(manifest), "cases")
