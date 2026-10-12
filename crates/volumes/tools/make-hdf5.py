#!/usr/bin/env python3
"""Writes HDF5, MATLAB v7.3 and NetCDF test files and reference.json to
local/assets/volumes/hdf5-generated/ and netcdf-generated/, with h5py and
netCDF4 (pip install h5py netCDF4).

    local/pyenv-vol/bin/python crates/volumes/tools/make-hdf5.py
"""
import json
import os
import sys
import numpy as np
import h5py
import netCDF4

root = sys.argv[1] if len(sys.argv) > 1 else "local/assets/volumes"
h5dir, ncdir = root + "/hdf5-generated", root + "/netcdf-generated"
os.makedirs(h5dir, exist_ok=True)
os.makedirs(ncdir, exist_ok=True)
rng = np.random.default_rng(77)
ref = {}


def field(shape, dtype, scale=1.0):
    idx = np.indices(shape).astype(np.float64)
    base = np.zeros(shape)
    for k, g in enumerate(idx):
        base += np.sin(g / (4.0 + 3 * k))
    a = (base - base.min()) / (base.max() - base.min() + 1e-9) * scale + rng.random(shape) * scale * 0.02
    return a.astype(dtype)


def picks(a, n=4):
    out = []
    for _ in range(n):
        idx = tuple(int(rng.integers(0, s)) for s in a.shape)
        out.append([list(idx), float(a[idx])])
    return out


def note(name, dataset, a, **extra):
    ref.setdefault(name, {})[dataset] = {"shape": list(a.shape), "dtype": str(a.dtype), "samples": picks(a) if a.size else [], **extra}


# 1. Plain HDF5.
vol = field((20, 48, 64), np.int16, 3000)
img = field((60, 80), np.float32, 1.0)
with h5py.File(f"{h5dir}/basic.h5", "w") as f:
    f["image"] = img
    f["volume_chunked_gzip"] = vol
    f.create_dataset("volume_chunked_shuffle_gzip", data=vol, chunks=(5, 16, 16), compression="gzip", shuffle=True)
    f.create_dataset("volume_chunked_plain", data=vol, chunks=(5, 16, 16))
    f.create_dataset("volume_gzip", data=vol, chunks=(10, 24, 32), compression="gzip", compression_opts=4)
    g = f.create_group("results/run1")
    g["map"] = field((30, 40), np.float64, 5.0)
    f["rgb"] = rng.integers(0, 255, (30, 40, 3), dtype=np.uint8)
    f["vector"] = np.linspace(0, 1, 100)
    f["counts"] = np.arange(1000, dtype=np.uint32).reshape(20, 50)
    f["big_endian"] = img.astype(">f4")
    f["half"] = img.astype(np.float16)
    f["flags"] = img > 0.5
    f["names"] = np.array([b"alpha", b"beta"])
    f["compound"] = np.zeros(4, dtype=[("a", "<i4"), ("b", "<f8")])
    f["scalar"] = np.float64(3.5)
    f.create_dataset("compact", data=np.arange(12, dtype=np.int16).reshape(3, 4), chunks=None)
for n, a in [("image", img), ("volume_chunked_gzip", vol), ("volume_chunked_shuffle_gzip", vol), ("volume_chunked_plain", vol), ("volume_gzip", vol), ("results/run1/map", field((30, 40), np.float64, 5.0))]:
    note("basic.h5", n, a)
ref["basic.h5"]["results/run1/map"] = {"shape": [30, 40], "dtype": "float64", "samples": []}
with h5py.File(f"{h5dir}/basic.h5", "r") as f:
    ref["basic.h5"]["results/run1/map"] = {"shape": [30, 40], "dtype": "float64", "samples": picks(np.asarray(f["results/run1/map"]))}
    for n in ["rgb", "counts", "big_endian", "half", "vector", "compact", "image", "volume_chunked_gzip"]:
        a = np.asarray(f[n])
        ref["basic.h5"][n] = {"shape": list(a.shape), "dtype": str(a.dtype), "samples": picks(a)}
    for n in ["volume_chunked_shuffle_gzip", "volume_chunked_plain", "volume_gzip"]:
        a = np.asarray(f[n])
        ref["basic.h5"][n] = {"shape": list(a.shape), "dtype": str(a.dtype), "samples": picks(a)}

# 2. Newer file formats: v4 chunk indexes (fixed array, extensible array, B-tree v2, single
# chunk), new-style groups with compact and dense links.  libver "v110" writes layout version 4;
# "latest" may write the newest layout version, which Slerp reads only as far as the library does.
for name, libver in [("latest110.h5", ("v110", "v110")), ("latest.h5", "latest")]:
    with h5py.File(f"{h5dir}/{name}", "w", libver=libver) as f:
        f.create_dataset("fixed_array", data=vol, chunks=(5, 16, 16), compression="gzip")
        f.create_dataset("fixed_array_paged", data=field((30, 64, 64), np.int16, 2000), chunks=(1, 8, 8), compression="gzip")
        f.create_dataset("extensible", data=vol, chunks=(5, 16, 16), maxshape=(None, 48, 64), compression="gzip")
        f.create_dataset("btree2", data=vol, chunks=(5, 16, 16), maxshape=(None, None, None), compression="gzip")
        f.create_dataset("single_chunk", data=img, chunks=img.shape, compression="gzip")
        f.create_dataset("contiguous", data=img)
        g = f.create_group("many")
        for i in range(30):
            g.create_dataset(f"arr_{i:02d}", data=np.full((6, 6), i, dtype=np.float32))
        g2 = f.create_group("few")
        for i in range(3):
            g2.create_dataset(f"d{i}", data=np.full((5, 5), i, dtype=np.int16))
    with h5py.File(f"{h5dir}/{name}", "r") as f:
        for n in ["fixed_array", "fixed_array_paged", "extensible", "btree2", "single_chunk", "contiguous", "many/arr_07", "many/arr_29", "few/d2"]:
            a = np.asarray(f[n])
            ref.setdefault(name, {})[n] = {"shape": list(a.shape), "dtype": str(a.dtype), "samples": picks(a)}

# 3. MATLAB v7.3: HDF5 with a 512-byte user block, arrays stored column-major (dimensions reversed).
m = field((24, 36, 10), np.float64, 100.0)  # a MATLAB array of 24 x 36 x 10
with h5py.File(f"{h5dir}/matlab73.mat", "w", userblock_size=512) as f:
    d = f.create_dataset("scan", data=np.asfortranarray(m).T.copy(order="C"))  # stored dims (10, 36, 24)
    d.attrs["MATLAB_class"] = np.bytes_("double")
    d2 = f.create_dataset("image", data=np.asfortranarray(img).T.copy(order="C"), chunks=(80, 60), compression="gzip")
    d2.attrs["MATLAB_class"] = np.bytes_("single")
with open(f"{h5dir}/matlab73.mat", "r+b") as fh:
    fh.write(b"MATLAB 7.3 MAT-file, Platform: GLNXA64, Created on: Thu Oct  8 12:00:00 2026 HDF5 schema 1.00 .".ljust(116, b" "))
    fh.seek(124)
    fh.write(bytes([0, 2, ord("I"), ord("M")]))
ref["matlab73.mat"] = {"scan": {"shape": [24, 36, 10], "dtype": "float64", "matlab": True, "samples": picks(m)}, "image": {"shape": [60, 80], "dtype": "float32", "matlab": True, "samples": picks(img)}}

# 4. NetCDF-4 (HDF5 with dimension scales) and classic NetCDF (CDF-1, CDF-2, CDF-5).
lat = np.linspace(-60, 60, 30)
lon = np.linspace(0, 358, 60)
temp = field((4, 30, 60), np.float32, 40.0) - 10
for name, fmt in [("temp_nc4.nc", "NETCDF4"), ("temp_classic.nc", "NETCDF3_CLASSIC"), ("temp_64bit.nc", "NETCDF3_64BIT_OFFSET"), ("temp_cdf5.nc", "NETCDF3_64BIT_DATA")]:
    with netCDF4.Dataset(f"{ncdir}/{name}", "w", format=fmt) as ds:
        ds.title = "Test temperature"
        ds.createDimension("time", None)
        ds.createDimension("lat", 30)
        ds.createDimension("lon", 60)
        t = ds.createVariable("time", "f8", ("time",))
        t.units = "days since 2020-01-01"
        la = ds.createVariable("lat", "f4", ("lat",))
        la.units = "degrees_north"
        lo = ds.createVariable("lon", "f4", ("lon",))
        lo.units = "degrees_east"
        kw = {"zlib": True, "complevel": 4, "chunksizes": (1, 15, 30)} if fmt == "NETCDF4" else {}
        v = ds.createVariable("tas", "f4", ("time", "lat", "lon"), fill_value=-999.0, **kw)
        v.units = "degC"
        v.long_name = "Near-surface air temperature"
        t[:] = np.arange(4)
        la[:] = lat
        lo[:] = lon
        masked = np.ma.masked_array(temp, mask=np.zeros(temp.shape, bool))
        masked.mask[:, :3, :5] = True
        v[:] = masked
        if fmt != "NETCDF4":
            s = ds.createVariable("scaled", "i2", ("lat", "lon"))
            s.scale_factor = 0.01
            s.add_offset = 20.0
            s.set_auto_maskandscale(False)
            s[:] = (np.clip(temp[0], -10, 30) * 100).astype(np.int16)
    arr = np.ma.masked_array(temp, mask=masked.mask).filled(np.nan)
    ref[name] = {"tas": {"shape": [4, 30, 60], "dtype": "float32", "samples": [s for s in picks(np.nan_to_num(arr, nan=-999.0)) if s[1] != -999.0]}}
json.dump(ref, open(h5dir + "/reference.json", "w"), indent=1)
print("wrote", sum(len(v) for v in ref.values()), "datasets")
