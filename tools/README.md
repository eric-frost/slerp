# Slerp development tools

Tools for development live here: a fetcher that downloads the public test corpus into `local/assets/`, a KIO probe that asks KDE for thumbnails the same way Dolphin does, the glTF validation scripts, and others named below.  None is part of the shipped program.

## Test assets: `fetch-test-assets.ts`

```
deno run -A tools/fetch-test-assets.ts                  # everything
deno run -A tools/fetch-test-assets.ts gltf textures    # some categories
deno run -A tools/fetch-test-assets.ts quake/librequake # one entry
deno run -A tools/fetch-test-assets.ts --list           # show entries
deno run -A tools/fetch-test-assets.ts --force hdr      # fetch again
```

The script reads `tools/assets.jsonc` and puts each category's files in `local/assets/<category>/`.  It needs only Deno, `git` with `git-lfs`, `7z` and `tar`; it has no npm or JSR dependencies.  Downloads run eight at a time (`--jobs N` changes that).  An entry that finished before is skipped: its stamp in `local/assets/.cache/stamps/` records the entry exactly as it was fetched, so editing an entry in the manifest fetches it again on the next run.  At the end the script prints file counts and sizes per category and lists failures, and exits non-zero if anything failed.

It also writes `local/assets/LICENCES.md`, a table of every fetched path with its licence, source and whether it may be redistributed.  Everything under `local/` is ignored by Git, so nothing here is published by accident; check that table before copying any file into the repository (for example as a reference-image test input).  Only entries marked `"redistribute": true` may be committed.

Repositories are fetched as shallow, blob-less, sparse Git checkouts, so only the chosen folders are downloaded, and Git LFS files (KTX-Software keeps its test textures in LFS) arrive as real files rather than pointers.

### Manifest fields

Each object in `entries` is one download:

| Field | Meaning |
|---|---|
| `category` | Folder under `local/assets/`. |
| `name` | Unique within the category.  Archive, Git and Poly Haven entries unpack into `<category>/<name>/` unless `dest` says otherwise. |
| `url` | A file or archive to download. |
| `dest` | For a plain file, its path inside the category folder (default: the URL's file name); for other entries, the folder name. |
| `licence`, `redistribute` | The licence as found, and whether the files may be published.  Unknown or non-free licences get `false`. |
| `sha256` | Optional hash of the downloaded file; a mismatch fails the entry. |
| `extract` | Treat the download as an archive (zip, 7z, pk3, tar.gz/xz/bz2; detected from its first bytes).  `keep` lists globs (`*`, `**`, `?`, `{a,b}`) of member paths to keep, matched before `strip` removes that many leading folders.  `pak: true` also unpacks Quake `.pak` files into a folder named after each pak. |
| `git` | A sparse checkout instead of a download: `repo`, `ref`, `paths` (gitignore-style patterns; a leading `/` anchors at the repository root, a trailing `/` means a folder) and `strip`. |
| `polyhaven` | A Poly Haven asset by API id: `{"id", "format": "mtlx" | "gltf" | "blend" | ..., "res": "1k"}`.  Fetches the main file and everything it includes (textures, `.bin`). |
| `tool`, `commands` | Generate files instead of downloading them: `tool` is an archive holding a binary (`bin` is its path inside), and each command is an argument list where `{tool}`, `{src}` (the fetched `git` or `url` source) and `{out}` are substituted.  Used to encode raw `.astc` files with ARM's astcenc.  `tool` is optional and a command can be any program on the PATH, so an entry with only `commands` can read an FTP server with `curl` and unpack with `tar` (`dicom-extra`), and a `git` entry can reshape its checkout with `cp` and `rm` (`blend-extra/gltf-blender-io` copies a subfolder and the repository's root `LICENSE.txt`, which `strip` alone would drop).  Whatever the commands leave in `{out}` becomes the entry's folder. |
| `size`, `note` | Informational only. |

### Categories

| Category | What it holds |
|---|---|
| `textures` | `ktx-software/`: KTX-Software's test textures (KTX1, KTX2, Basis ETC1S and UASTC, UASTC HDR, ASTC, ETC2/EAC, arrays, 3D, cubemaps, zstd, orientation).  `ktx-cts/`: the KTX conformance inputs, including one file per vkFormat in `ktx2/`, real images in `ktx2_sample/`, and invalid files in `validate/`.  `dds/`: DDS samples covering DX9 and DX10 headers, BC1-BC7, BC6H, float and packed formats, cubemaps and a volume.  `dds-arrays/`: a real DDS 2D texture array.  `astc/`: raw `.astc` files encoded locally at several block sizes, including HDR. |
| `hdr` | Poly Haven HDRIs (studio and outdoor) at 1K and 4K. |
| `gltf` | `khronos/`: Khronos glTF sample models with every variant (Draco, Meshopt, quantized, KTX-BasisU).  `three/`: three.js examples (gltfpack Meshopt + KTX2, ARKit blend shapes, Mixamo characters).  `polyhaven/`: CC0 Poly Haven models. |
| `vrm` | VRM 0.x and 1.0 avatars and `.vrma` animations. |
| `fbx` | `ufbx/`: ufbx's test data (FBX 3000 to 7700, binary and ASCII, from Maya, Max, Blender and others, plus fuzz corpora of malformed files).  `assimp/`: assimp's FBX tests. |
| `collada`, `obj`, `stl`, `ply` | assimp's test models for each format; the Stanford bunny and dragon in `ply/`; a few printable STL files. |
| `vox` | MagicaVoxel's own sample scenes and the format description. |
| `usd` | Small USD, USDA, USDC and USDZ samples. |
| `materials` | ambientCG materials (MaterialX, USD, Godot `.tres`, Blender, maps, preview), Poly Haven materials as MaterialX and glTF, MaterialX's example library, and `.sbsar` if one was found. |
| `quake` | `librequake/id1/`: LibreQuake's loose BSP29 maps with `.lit` files, and its `pak0.pak` and `pak1.pak` both as files and unpacked into `pak0/` and `pak1/` (MDLs, sprites, `gfx/palette.lmp`, `gfx.wad`, single-player maps).  `librequake-texwads/`: WAD2 texture wads.  `avix_qcastle/` (BSP2 with `.lit`) and `signati/` (BSP29 with `.lit`) from Quaddicted.  MD2 player models with PCX skins, Xonotic and TrenchBroom MD3s with TGA skins and a `.skin` file, assimp's MD2, MD3, MD5, Quake 1 MDL and a Quake 3 `.pk3`. |
| `halflife` | assimp's Half-Life MDL v10 test models, small BSP30 test maps and WAD3 files from open-source tools, and (not redistributable) the retail `valve/c1a2d.bsp` with the full `valve/halflife.wad`. |
| `doom` | Freedoom phase 1 and 2 and a small PWAD. |
| `uasset` | Unreal `.uasset` files with embedded editor thumbnails. |
| `aseprite`, `tiled`, `splats`, `luts` | Small sets of Aseprite sprites, Tiled maps, Gaussian splats (`.ply`, `.splat`, `.spz`) and 3D LUTs (`.cube`, `.3dl`). |
| `science-extra` | FITS (plain, extension images, BZERO and BLANK, NaN, NAXIS 4, Rice-compressed), XISF from PixInsight, libXISF, XisfSharp and others, DPX and Cineon from OpenImageIO-images (and ffmpeg's Nuke checkerboards, not redistributable), and JPEG XR (Booritas, openjxr, a 128-bit float HDR screenshot), in the folders the `slerp-textures` and `slerp-jxr` tests read when present.  Files made from these (compressed FITS, DPX and JPEG XR conversions) are not fetched; the committed test files in `crates/textures/tests/data` and `crates/jxr/tests/data` come from the `make-*.py` scripts beside them. |
| `editors-extra` | Samples of creative apps' files and colour palettes, one entry per file, in the folders the `slerp-previews` tests read: `procreate/`, `affinity/` (v1, v2 and `.af`), `sketch/`, `figma/` (zipped and bare `fig-kiwi` files, plus Xfig drawings in `xfig/` as negatives), `clip/`, `pdn/`, `indd/`, `xd/`, `freecad/`, and `palettes/<format>/` (`ase`, `aco`, `act`, `gpl`, `kpl`, `pal-jasc`, `pal-riff`, `pal-other` and `hex-other` as negatives, `hex`, `swatches`, `xfig`).  Files under GPL-family licences (a third of the entries) are fetched but marked `redistribute: false`.  Files with unclear or non-free licences (most InDesign and Adobe XD samples, Lospec's palettes, some Krita bug attachments) aren't in the manifest at all; they sit in the same folders on the machine that collected them, marked `NOT-REDISTRIBUTABLE` in each folder's `SOURCES.md`, and the tests skip what's missing. |
| `retro-extra` | Redistributable samples of the retro and game-image formats, one entry per file in the folders `crates/core/tests/retro.rs` reads: `pico8/` and `tic80/` cartridges (MIT), `ansi/` (ansilove, Moebius), `xnb/` (MonoGame, Nez), `blp/` (War3Net, warcraft-rs), `pvr/` and `pkm/` (PowerVR SDK, bgfx, ARM), `bbc/` and `iff/` (MIT repositories, synthetic masks), and `atlas/` (libGDX, TexturePacker) with the pictures they name.  The much larger set that is only fine for local tests (Atari ST, C64, ZX Spectrum, CPC, MSX, Apple II, Atari 8-bit, Build engine tiles, Quake sprites, Amiga IFF and more, nearly all scene art or shareware) was collected by hand into the same folder with a `SOURCES.md` marking every file's licence and `NOT-REDISTRIBUTABLE` where it is unclear; the tests run on whatever is there. |
| `dicom` | `pydicom-test-files/` and `pydicom-data/`: pydicom's test files (not redistributable: many are NEMA samples with an unstated licence; each file's source is in its `README.txt`): the same images in explicit, implicit and big-endian VR, deflate, RLE, JPEG (baseline, 12-bit, lossless), JPEG-LS and JPEG 2000, colour and palette images, multi-frame and cine files, VOI and modality LUTs, a DICOMDIR with `dicomdirtests/`.  `tcia-lidc-idri-0001/`: a de-identified chest CT series (133 slices with extension-less names in random order), two chest radiographs and a segmentation from The Cancer Imaging Archive (CC BY 3.0).  NEMA's compressed samples and ultrasound loops, whose licence is unstated, are kept by hand in `dicom-extra/` (see its `SOURCES.md`). |
| `paths-extra` | G-code and GPS tracks, in the folders the `slerp-paths` and `slerp-core` tests read.  `gcode-preview/`: PrusaSlicer files with PNG and QOI pictures (3DBenchy, calicat, a plant sign, a screw), a spiral vase without one, and two CNC programs (MIT repository, but three prints are CC BY-NC-SA Thingiverse models, so `redistribute: false`).  `libbgcode/`: Prusa's binary G-code (heatshrink, MeatPack, pictures) beside the text G-code it decodes to (AGPL test data, `redistribute: false`).  `togeojson/`, `gpxpy/`: GPX, KML (`gx:Track`, `MultiGeometry`, polygons) and TCX files from the two projects' tests, including broken ones (BSD-2-Clause, Apache-2.0).  `fitparse/`: FIT activities from Garmin, Wahoo, Coros and Strava, with compressed timestamps, developer fields, chained and damaged files, and CSVs of the records to compare against (MIT).  `tcxparser/`: TCX runs and a 63 km ride (BSD-2-Clause).  For timing very large G-code (which `slerp thumbnail` samples rather than reads whole), `tools/make-big-gcode.ts` repeats a sliced print over a grid: 15 by 10 copies of the 3DBenchy give a 577 MB file in `local/paths-big/`. |
| `volumes` | Arrays, medical volumes and microscopy, in `ome/`, `nifti/`, `nrrd/`, `slides/` and `netcdf/`: OME Consortium's artificial OME-TIFFs (CC BY 4.0), nibabel's NIfTI and Analyze test files, the MNI152 brain template from TemplateFlow (CC0), 3D Slicer's NRRD files, OpenSlide's whole-slide samples (four CC0; two without a stated licence) and four xarray tutorial NetCDF files (no stated licence).  Files without a clear licence are listed in a `NOT-REDISTRIBUTABLE` file in their folder.  `numpy/` and the `*-generated/` folders are written by the scripts in `crates/volumes/tools/` with each format's reference library, with a `reference.json` of the values it read back; see `local/assets/volumes/SOURCES.md`. |
| `archives-extra` | Samples for the archive reader, in `pck/`, `vpk/` and `grp/`: eleven Godot packs exported with Godot 3.6, 4.3, 4.6 and 4.7 from MIT demo projects (pack formats 1 to 4, with an encrypted-directory and an encrypted-files variant), Valve VPKs from ValvePython, plumber and sourcepak-rs test folders (MIT or Apache-2.0, multi-part and single-file, versions 1 and 2, Source 2 maps), and hand-made GRP files (CC0).  Real game directories and group files (Portal, TF2, Titanfall, Duke Nukem 3D shareware, Ken-Build) are not redistributable and carry a `NOT-REDISTRIBUTABLE.txt`; `local/assets/archives-extra/SOURCES.md` lists every file.  `tools/make-archive-samples.ts` writes the Godot and GRP files. |
| `images-extra` | Samples for the ordinary image formats, one folder per format (`gif/`, `apng/`, `webp/`, `tiff/`, `ico/`, `pnm/`, `jp2/`, `jxl/`, `avif/`, `heic/`, `exif-orientation/`, `icc/`): the test suites of Pillow, image-rs, libavif, libheif, libjxl (conformance and testdata), jxl-rs, web-platform-tests and others, under their own permissive licences.  Each folder has `ref/` reference decodes made with ImageMagick, Pillow, libwebp, ffmpeg, libheif, libavif and libjxl (`scripts/make_refs.py`, `scripts/make_avif_bilinear_refs.sh`), which `crates/images/tests/corpus.rs` compares against.  Files with unclear or restrictive terms carry a `NOT-REDISTRIBUTABLE.txt`; `local/assets/images-extra/SOURCES.md` lists every file and how the generated ones were made. |
| `cad-extra` | DXF drawings in `dxf/<project>/` (ezdxf's examples and tests, netDxf, dxf-parser, three-dxf, LibreCAD, QCAD, OpenSCAD, KiCad, FreeCAD, GDAL, dxf-viewer, libredwg and more: every AutoCAD version from R1.4 to 2024, text and binary, with blocks, hatches, splines, text, dimensions, 3D faces and meshes, code pages) and Rhino models in `3dm/<project>/` (the openNURBS example files in versions 1 to 8, rhino3dm, RhinoUSD and others, with and without render meshes); `SOURCES.md` there lists licences, and the entries marked not redistributable (openNURBS, GDAL, libredwg and some others, whose drawings have no stated licence) are for local tests only.  `dxf/generated/` and `3dm/generated/` are made by `crates/cad/tools/make-dxf-samples.py` and `make-3dm-samples.py`, not fetched. |
| `source-extra` | Source engine models: `wiremod/` (Garry's Mod's Wiremod add-on, Apache-2.0: 346 models of MDL versions 44 and 48 with their `.vvd`, `.dx90.vtx` and VMT/VTF materials; skin families, body groups, a few animated) and `sourcepp-mdl/` (three Valve models of versions 44, 47 and 49 from SourcePP's test assets, testing only, `NOT-REDISTRIBUTABLE.txt`). |
| `minecraft-extra` | Minecraft schematics: `prismarine-schematic/` and `schematic4j/` (the two libraries' MIT test files: Sponge 1 to 3, Litematica 5 and 6, MCEdit; two Schematica files of unknown origin are local-only) and `synthetic/`, which `tools/make-minecraft-samples.ts` writes: a 1.7-million-block castle town (Sponge 3), a 640 x 160 x 640 terrain for timing, a three-region Litematica file with 2, 5 and 7-bit packed arrays, a structure `.nbt` and a classic `.schematic`. |

Samples gathered before the fetcher existed are in `local/assets/<crate>-extra/`.  The manifest now has a region for each of those folders that holds fetchable files, so they can be downloaded again.  What cannot be (files a script wrote, such as `science-big/`, Blender's reference exports in `blend-extra/ref/`, `textures-extra/`, the Elementalism megawad in `doom-extra/`, which has no public download link, and files collected by hand) is named in a comment at the top of the region.

## KIO probe: `kio-probe/`

`kio-probe` runs `KIO::filePreview` over the files given as arguments, using Dolphin's enabled thumbnail plugins, and prints one line per file: `OK <width>x<height> <ms> <mime> <path>` or `FAIL <ms> <mime> <path>`.  It shows exactly what Dolphin would get, without opening Dolphin.  Build it with `tools/kio-probe/build.sh`, which uses CMake and puts the binary at `local/kio-probe`.  Run it with `QT_QPA_PLATFORM=offscreen` so it never opens a window:

```
QT_QPA_PLATFORM=offscreen local/kio-probe local/assets/textures/dds/cube/*.dds
```

Options are environment variables: `LIST=1` lists every thumbnail plugin KIO knows with its MIME types; `PLUGINS=a,b` uses those plugins instead of Dolphin's list (`[PreviewSettings] Plugins` in `~/.config/dolphinrc`); `SAVE=<dir>` writes each preview there as `<file name>.png`; `SIZE=<px>` requests another size than 256.  The milliseconds column is the time since the previous result.  KIO reports files without a matching plugin at once and out of order, so for a trustworthy time per file, pass one file per run.

## glTF validation: `validate-gltf.ts` and `glb-sweep.ts`

`validate-gltf.ts` checks GLB and glTF files with the Khronos glTF Validator, the `gltf-validator` npm package (Apache-2.0), which is installed in `local/` (not in git):

```
mkdir -p local/tools/gltf-validator && cd local/tools/gltf-validator
npm install --ignore-scripts gltf-validator@2.0.0-dev.3.10
deno run --allow-read --allow-env tools/validate-gltf.ts [--json] FILE...
```

It prints the counts of errors, warnings, infos and hints per file and lists the errors and warnings, and exits non-zero when any file has errors.  That validator release doesn't know `KHR_texture_basisu`, so files with Basis KTX2 pictures get two warnings per picture (`VALUE_NOT_IN_LIST`, `IMAGE_UNRECOGNIZED_FORMAT`), as the Khronos samples themselves do.

`glb-sweep.ts` converts every 3D sample under the folders it is given with `slerp convert` (a WAD's first map as a part), validates every written file, and prints how many converted, validated with no errors, weren't 3D scenes or couldn't be loaded, with timings; `DIR/report.json` holds every file's result:

```
deno run -A tools/glb-sweep.ts --slerp local/target/release/slerp --out local/glb-sweep local/assets/gltf local/assets/fbx ...
```

## Packages and release files

| Tool | What it does |
|---|---|
| `build-deb.sh [--no-qt] [--no-build]` | Builds the Debian packages `slerp` (cargo-deb, from `[package.metadata.deb]` in `Cargo.toml`) and `slerp-qt-plugin` (CMake and dpkg-deb, from `packaging/debian/qt-plugin/`) into `local/dist/`.  `CARGO`, `CARGO_DEB`, `CARGO_TARGET_DIR` and `DIST` choose the commands and folders. |
| `source-tarball.sh` | Packs the files Git would track into `local/dist/slerp-<version>.tar.gz`, laid out as GitHub's release archives, for `packaging/aur/PKGBUILD`. |
| `test-packages.sh deb\|aur` | Installs the `.deb` files in a fresh Ubuntu 26.04 container, or builds the PKGBUILD from the tarball in Arch Linux, and runs smoke tests (version, format table, CPU and lavapipe thumbnails, MIME and menu caches, the Qt plugin's libraries), lintian or namcap, and removal.  `CONTAINER` picks docker or podman with its options, `RUN_OPTS` adds `run` options. |
| `third-party.ts [--check]` | Rewrites the generated half of `THIRD-PARTY.md`: the crates compiled into Slerp's programs for Linux, Windows and macOS, and each distinct licence text once with the copyright lines of the crates using it.  Run it after dependencies change; `build-deb.sh` refuses to package while it is stale. |
| `format-list/` | Writes `docs/formats.md` and the website's format search list, `site/formats.json`, from the format table: `cargo run --manifest-path tools/format-list/Cargo.toml` (`-- --check` fails when it is stale). |
| `readme-images.sh [SLERP]` | Makes the README's pictures in `docs/images/` from freely licensed corpus samples: a thumbnail sheet and four viewer screenshots, taken in a private Xvfb on lavapipe. |

With rootless podman, keep its storage in a folder without ACLs (they break `cp -p` inside containers).  On systemd, the build container can run in a memory-capped slice, such as the `heavy-slerp.slice` that `tools/fuzz.sh` sets up:

```
P="podman --root local/podman/storage"
P="$P --runroot $XDG_RUNTIME_DIR/slerp-podman --storage-driver vfs"
CONTAINER="$P" RUN_OPTS="--cgroup-parent=heavy-slerp.slice" \
  tools/test-packages.sh aur
```
