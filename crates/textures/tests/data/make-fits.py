#!/usr/bin/env python3
"""Writes the small FITS test files and the pixel values astropy reads
back from them, for tests/fits.rs.  Run with astropy and numpy installed:

    python3 -I make-fits.py OUT_DIR

Each case NAME gives NAME.fits and NAME.f32, the expected pixels as
little-endian float32 in file order (plane after plane, the file's first
row first, NaN for undefined pixels), and a line in manifest.txt:

    NAME width height planes hdu

where hdu is the index of the unit the default picture comes from.  Tile
compression is done by astropy's cfitsio, an independent encoder, and the
expected pixels are what cfitsio decodes, so the tests check our decoders
against the reference implementation.
"""

import sys
import warnings

import numpy as np
from astropy.io import fits

warnings.simplefilter("ignore")
out = sys.argv[1]
rng = np.random.default_rng(20261008)
manifest = []


def smooth(h, w, planes=1):
    """A picture with gradients, a few stars and noise, so every
    compressor has something to do."""
    y, x = np.mgrid[0:h, 0:w]
    base = 200 + 30 * x / w + 20 * y / h
    out = []
    for p in range(planes):
        img = base * (1 + p * 0.5)
        for _ in range(4):
            cx, cy = rng.uniform(0, w), rng.uniform(0, h)
            img = img + 3000 * np.exp(-((x - cx) ** 2 + (y - cy) ** 2) / 6.0)
        out.append(img + rng.normal(0, 8, (h, w)))
    return np.array(out) if planes > 1 else out[0]


def save(name, hdul, expected, w, h, planes=1, hdu=0):
    hdul.writeto(f"{out}/{name}.fits", overwrite=True)
    np.asarray(expected, dtype="<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {w} {h} {planes} {hdu}")


def expected_of(path, index):
    with fits.open(path, do_not_scale_image_data=False, uint=True) as f:
        data = np.array(f[index].data)
    return data.astype(np.float64).astype(np.float32)


def plain(name, data, header=None, ext=False):
    """data written as the primary image, or as an IMAGE extension behind
    an empty primary unit."""
    hdu = fits.ImageHDU(data) if ext else fits.PrimaryHDU(data)
    for k, v in (header or {}).items():
        hdu.header[k] = v
    hdul = fits.HDUList([fits.PrimaryHDU(), hdu] if ext else [hdu])
    hdul.writeto(f"{out}/{name}.fits", overwrite=True)
    index = 1 if ext else 0
    exp = expected_of(f"{out}/{name}.fits", index)
    h, w = exp.shape[-2:]
    planes = exp.shape[0] if exp.ndim == 3 else 1
    np.asarray(exp, dtype="<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {w} {h} {planes} {index}")


H, W = 40, 64
img = smooth(H, W)
plain("u8", np.clip(img / 20, 0, 255).astype(np.uint8))
plain("i16", (img - 300).astype(np.int16))
plain("u16", img.astype(np.uint16) * 10)
plain("i32", (img * 1000).astype(np.int32))
plain("i64", (img * 100000).astype(np.int64))
plain("f32", img.astype(np.float32) / 1000)
f64 = img.copy()
f64[3, 5] = np.nan
f64[20, 20:30] = np.nan
plain("f64_nan", f64)
blank = (img).astype(np.int16)
blank[2:5, 3:9] = -32768
plain("i16_blank", blank, {"BLANK": -32768})
plain("i16_scaled", (img).astype(np.int16), {"BSCALE": 0.5, "BZERO": 100.0})
plain("rgb_f32", (smooth(H, W, 3) / 3000).astype(np.float32))
plain("cube_u16", (smooth(H, W, 5)).astype(np.uint16))
plain("ext1_u16", img.astype(np.uint16), ext=True)
plain("odd_size", smooth(37, 101).astype(np.int16))
plain("tall", smooth(300, 9).astype(np.int16))

# Two images: the first extension is a small one, the second a bigger float.
two = fits.HDUList(
    [
        fits.PrimaryHDU(),
        fits.ImageHDU((img[:16, :24]).astype(np.int16), name="SCI", ver=1),
        fits.ImageHDU(img.astype(np.float32), name="ERR", ver=2),
    ]
)
two.writeto(f"{out}/two_images.fits", overwrite=True)
np.asarray(expected_of(f"{out}/two_images.fits", 1), dtype="<f4").tofile(f"{out}/two_images.f32")
np.asarray(expected_of(f"{out}/two_images.fits", 2), dtype="<f4").tofile(f"{out}/two_images_2.f32")
manifest.append("two_images 24 16 1 1")
manifest.append("two_images_2 64 40 1 2")

# A one-shot-colour mosaic: expected is the raw mosaic; the test checks the
# colour a flat scene demosaics to.
mosaic = np.zeros((H, W), np.uint16)
mosaic[0::2, 0::2] = 40000  # red
mosaic[0::2, 1::2] = 20000  # green
mosaic[1::2, 0::2] = 20000
mosaic[1::2, 1::2] = 5000  # blue
plain("bayer_rggb", mosaic, {"BAYERPAT": "RGGB", "ROWORDER": "TOP-DOWN"})

# Tile compression.  The tile shape is (rows, columns) in numpy order.
def comp(name, data, kind, tile=(8, 32), **kw):
    hdu = fits.CompImageHDU(data, compression_type=kind, tile_shape=tile, **kw)
    hdul = fits.HDUList([fits.PrimaryHDU(), hdu])
    hdul.writeto(f"{out}/{name}.fits", overwrite=True)
    exp = expected_of(f"{out}/{name}.fits", 1)
    h, w = exp.shape[-2:]
    planes = exp.shape[0] if exp.ndim == 3 else 1
    np.asarray(exp, dtype="<f4").tofile(f"{out}/{name}.f32")
    manifest.append(f"{name} {w} {h} {planes} 1")


i16 = (img - 100).astype(np.int16)
i32 = (img * 1000).astype(np.int32)
f32 = (img / 7).astype(np.float32)
f32n = f32.copy()
f32n[5:8, 5:8] = np.nan
comp("rice_i16", i16, "RICE_1")
comp("rice_i16_rows", i16, "RICE_1", tile=(1, W))
comp("rice_i32", i32, "RICE_1", tile=(10, 17))
comp("rice_u8", np.clip(img / 20, 0, 255).astype(np.uint8), "RICE_1")
comp("rice_u16", img.astype(np.uint16) * 3, "RICE_1")
comp("rice_f32_dither1", f32, "RICE_1", quantize_level=16.0, quantize_method=1, dither_seed=7)
comp("rice_f32_dither2", f32, "RICE_1", quantize_level=16.0, quantize_method=2, dither_seed=10000)
comp("rice_f32_nodither", f32, "RICE_1", quantize_level=16.0, quantize_method=-1)
comp("rice_f32_nan", f32n, "RICE_1", quantize_level=16.0, quantize_method=1, dither_seed=1)
comp("gzip1_i16", i16, "GZIP_1")
comp("gzip2_i16", i16, "GZIP_2")
comp("gzip2_i32", i32, "GZIP_2", tile=(5, 64))
comp("gzip2_f32_lossless", f32, "GZIP_2", quantize_level=0)
comp("gzip1_f32_quant", f32, "GZIP_1", quantize_level=8.0, quantize_method=1, dither_seed=3)
comp("plio_mask", (np.digitize(img, [230, 260, 500])).astype(np.int16) * 7, "PLIO_1")
comp("hcomp_i16", i16, "HCOMPRESS_1", tile=(16, 16), hcomp_scale=0)
comp("hcomp_i16_scaled", i16, "HCOMPRESS_1", tile=(16, 16), hcomp_scale=12.0, hcomp_smooth=0)
comp("hcomp_i16_smooth", i16, "HCOMPRESS_1", tile=(16, 16), hcomp_scale=12.0, hcomp_smooth=1)
comp("hcomp_odd", smooth(37, 101).astype(np.int16), "HCOMPRESS_1", tile=(37, 101), hcomp_scale=0)
comp("hcomp_f32", f32, "HCOMPRESS_1", tile=(16, 16), quantize_level=16.0, quantize_method=1, dither_seed=5, hcomp_scale=0)

# Compressed colour and cubes: tiles are one plane thick.
cube16 = smooth(H, W, 3).astype(np.int16)
comp("rice_rgb", cube16, "RICE_1", tile=(1, 8, 32))
comp("gzip2_cube4", smooth(H, W, 4).astype(np.int16), "GZIP_2", tile=(1, 40, 64))
comp("hcomp_rgb", cube16, "HCOMPRESS_1", tile=(1, 16, 16), hcomp_scale=0)
comp("rice_f32_rgb_dither", (smooth(H, W, 3) / 7).astype(np.float32), "RICE_1", tile=(1, 8, 32), quantize_level=16.0, quantize_method=1)

# Smoothing of lossy HCOMPRESS, and an impulse whose many coefficients
# exercise every bit plane and the sign bits.
yy, xx = np.mgrid[0:128, 0:128]
big = (500 + 200 * np.sin(xx / 9.0) * np.cos(yy / 13.0) + rng.normal(0, 15, (128, 128))).astype(np.int16)
big[40:44, 60:64] += 3000
comp("hcomp_smooth8", big, "HCOMPRESS_1", tile=(32, 32), hcomp_scale=8.0, hcomp_smooth=1)
comp("hcomp_smooth64", big, "HCOMPRESS_1", tile=(32, 32), hcomp_scale=64.0, hcomp_smooth=1)
comp("hcomp_unsmooth8", big, "HCOMPRESS_1", tile=(32, 32), hcomp_scale=8.0, hcomp_smooth=0)
impulse = np.zeros((8, 8), np.int16)
impulse[3, 5] = 50
comp("hcomp_impulse", impulse, "HCOMPRESS_1", tile=(8, 8), hcomp_scale=0)

open(f"{out}/manifest.txt", "w").write("\n".join(manifest) + "\n")
print(len(manifest), "cases")
