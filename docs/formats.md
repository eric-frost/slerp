# Formats

Slerp reads the 204 formats below, in 16 categories.  The list is generated from the format table (`crates/formats/src/table.rs`) by `tools/format-list`; `slerp formats` prints the same table, and `slerp associate --list` shows what handles each format on this computer.

"Thumbnails" says whether Slerp makes file-manager thumbnails of the format unless the user changes it (171 formats).  They are off for ordinary pictures and documents the desktops already preview; `slerp associate --thumbnails on <format>` or the viewer's Settings, File Types switches them on.  Extensions in italics are only confirmed by looking inside the file, because unrelated files share them.

## 3D models

| Format | Files | Thumbnails |
|---|---|---|
| glTF binary model (GLB) | `.glb` | on |
| glTF model | `.gltf` | on |
| VRM avatar | `.vrm` | on |
| VRM animation | `.vrma` | none |
| Wavefront OBJ model | `.obj` | on |
| STL model | `.stl` | on |
| Autodesk FBX model | `.fbx` | on |
| COLLADA model | `.dae` | on |
| PLY model, point cloud or splats | `.ply` | on |
| USD scene | `.usda` `.usdc` `.usd` | on |
| USDZ package | `.usdz` | on |
| MagicaVoxel model | `.vox` | on |
| Minecraft schematic or structure | `.schem` `.litematic` `.nbt` `.schematic` | on |
| Roblox model | `.rbxm` `.rbxmx` | on |
| Quake or Half-Life model (MDL) | `.mdl` | on |
| Source engine model (MDL) | `.mdl` | on |
| Quake II model (MD2) | `.md2` | on |
| Quake III model (MD3) | `.md3` | on |
| MetaHuman DNA rig | `.dna` | on |
| 3MF model | `.3mf` | on |
| OFF mesh | `.off` | on |
| 3D Studio mesh (3DS) | `.3ds` | on |
| DirectX model (.x) | `.x` | on |
| VRML world | `.wrl` `.vrml` `.wrz` | on |
| X3D scene | `.x3d` `.x3dz` `.x3dv` `.x3dvz` | on |
| Draco geometry | *`.drc`* | on |
| MikuMikuDance model (PMX) | `.pmx` | on |
| MikuMikuDance model (PMD) | `.pmd` | on |
| MikuMikuDance motion (VMD) | `.vmd` | none |
| Doom 3 model (MD5) | `.md5mesh` | on |
| Doom 3 animation (MD5) | `.md5anim` | none |
| Inter-Quake Model (IQM) | `.iqm` | on |
| Inter-Quake Export (IQE) | `.iqe` | on |
| Source engine model source (SMD) | `.smd` | on |
| Unreal skeletal mesh (PSK) | `.psk` `.pskx` | on |
| Unreal animation set (PSA) | *`.psa`* | none |
| Blockbench model | `.bbmodel` | on |
| 3ds Max scene | `.max` | on |
| SketchUp model | `.skp` | on |
| Maya scene | *`.ma`* *`.mb`* | on |

## Point clouds and splats

| Format | Files | Thumbnails |
|---|---|---|
| Gaussian splats (.splat) | `.splat` | on |
| Compressed Gaussian splats (SPZ) | `.spz` | on |
| PlayCanvas splats (SOG) | `.sog` | on |
| LAS or LAZ lidar point cloud | `.las` `.laz` | on |
| E57 laser scan | `.e57` | on |
| PCD point cloud | `.pcd` | on |
| PTS point cloud | `.pts` | on |
| PTX laser scan | `.ptx` | on |

## Scenes and projects

| Format | Files | Thumbnails |
|---|---|---|
| Godot scene | `.tscn` `.scn` `.escn` | on |
| Unity scene or prefab | `.unity` `.prefab` | on |
| Blender file | `.blend` `.blender` | on |

## Levels and maps

| Format | Files | Thumbnails |
|---|---|---|
| Roblox place | `.rbxl` `.rbxlx` | on |
| Quake or Half-Life map (BSP) | `.bsp` | on |
| Doom WAD | `.wad` | on |
| Doom resource archive (PK3) | `.pk3` `.ipk3` `.pke` | on |
| Doom resource archive (PK7) | `.pk7` `.ipk7` | on |
| Quake or Half-Life map source (.map) | *`.map`* | on |
| Hammer map source (VMF) | `.vmf` | on |

## Materials

| Format | Files | Thumbnails |
|---|---|---|
| MaterialX material | `.mtlx` | on |
| Godot material or mesh resource | `.tres` | on |
| Substance material | `.sbsar` | on |

## Textures

| Format | Files | Thumbnails |
|---|---|---|
| KTX 2 texture | `.ktx2` | on |
| KTX texture | `.ktx` | on |
| ASTC texture | `.astc` | on |
| DirectDraw Surface texture (DDS) | `.dds` | on |
| Quake or Half-Life texture WAD | `.wad` | on |
| XNA or MonoGame texture (XNB) | `.xnb` | on |
| Blizzard texture (BLP) | `.blp` | on |
| PowerVR texture (PVR) | `.pvr` | on |
| Ericsson ETC texture (PKM) | `.pkm` | on |
| Build engine tiles (ART) | `.art` | on |

## Sprites and tile maps

| Format | Files | Thumbnails |
|---|---|---|
| Aseprite sprite | `.aseprite` `.ase` | on |
| Tiled map | `.tmx` `.tmj` | on |
| LDtk project or level | `.ldtk` `.ldtkl` | on |
| Quake or Half-Life sprite (SPR) | `.spr` | on |
| Quake II sprite (SP2) | `.sp2` | on |
| libGDX or Spine texture atlas | `.atlas` | on |
| TexturePacker sprite sheet data | `.tpsheet` | on |
| Pyxel Edit document | `.pyxel` | on |
| Piskel sprite | `.piskel` | on |

## Images

| Format | Files | Thumbnails |
|---|---|---|
| 16-bit greyscale PNG (heightmaps and masks) | *`.png`* | on |
| Heightmap PNG (named *_height.png, *_disp.png and so on) | `*.height.png` `*_height.png` `*-height.png` `*.heightmap.png` `*_heightmap.png` `*-heightmap.png` `heightmap.png` `*_displacement.png` `*-displacement.png` `*_disp.png` `*_disp_*.png` `*.r16.png` | on |
| Radiance HDR image | `.hdr` `.pic` | on |
| DPX film scan | `.dpx` | on |
| Cineon film scan | `.cin` `.cineon` | on |
| Netpbm PAM image | `.pam` | on |
| farbfeld image | `.ff` | on |
| JPEG XR image | `.jxr` `.wdp` `.hdp` | on |
| GIF image | `.gif` | off |
| Animated PNG | `.apng` | off |
| WebP image | `.webp` | off |
| TIFF image | `.tif` `.tiff` | off |
| Windows icon or cursor | `.ico` `.cur` | off |
| Netpbm image (PBM, PGM, PPM, PNM) | `.pbm` `.pgm` `.ppm` `.pnm` | off |
| JPEG 2000 image | `.jp2` `.jpg2` `.jpx` `.jpf` `.j2c` `.jpc` `.j2k` | off |
| JPEG XL image | `.jxl` | off |
| AVIF image | `.avif` `.avifs` | off |
| HEIF or HEIC image | `.heic` `.heif` `.hif` *`.heics`* | off |
| PNG image | `.png` | off |
| JPEG image | `.jpg` `.jpeg` `.jpe` | off |
| BMP image | `.bmp` `.dib` | off |
| TGA image | `.tga` | off |
| PCX image | `.pcx` | off |
| Photoshop document | `.psd` `.psb` | off |
| GIMP image | `.xcf` `.xcf.gz` `.xcf.bz2` | off |
| Krita document | `.kra` `.krz` | off |
| OpenRaster image | `.ora` | off |
| OpenEXR image | `.exr` | off |
| QOI image | `.qoi` | off |
| Apple icon | `.icns` | off |
| SGI image | `.sgi` `.rgb` *`.rgba`* *`.bw`* *`.int`* *`.inta`* | off |
| X pixmap (XPM) | `.xpm` | off |
| X bitmap (XBM) | `.xbm` | off |

## Retro computer art

| Format | Files | Thumbnails |
|---|---|---|
| Atari ST DEGAS picture | `.pi1` `.pi2` `.pi3` `.pc1` `.pc2` `.pc3` | on |
| Atari ST NEOchrome picture | `.neo` | on |
| Atari ST Tiny Stuff picture | `.tn1` `.tn2` `.tn3` `.tny` | on |
| Commodore 64 Koala Painter picture | `.koa` `.kla` | on |
| Commodore 64 Art Studio picture | `.art` `.aas` | on |
| Commodore 64 Doodle picture | `.dd` `.jj` | on |
| Commodore 64 hires bitmap | `.hir` `.hpi` `.ocp` | on |
| MSX screen dump | `.sc2` `.sc5` `.sc6` `.sc7` `.sc8` `.sc12` `.sr5` `.sr6` `.sr7` `.sr8` `.sr12` `.s12` `.srs` | on |
| Apple II hi-res picture | `.hgr` | on |
| Apple II double hi-res picture | `.dhr` | on |
| Atari 8-bit picture | `.mic` `.g15` `.gr15` `.gr9` `.gr8` | on |
| BBC Micro screen dump | `.bbc` | on |
| ANSI art | `.ans` | on |
| PICO-8 cartridge | `.p8` | on |
| TIC-80 cartridge | `.tic` | on |
| Amiga IFF picture | `.iff` `.ilbm` `.lbm` | on |

## Palettes and colour tables

| Format | Files | Thumbnails |
|---|---|---|
| Colour lookup table (Cube) | `.cube` | on |
| Colour lookup table (3DL) | `.3dl` | on |
| Adobe Swatch Exchange palette (.ase) | `.ase` | on |
| Photoshop swatches (.aco) | `.aco` | on |
| Adobe colour table (.act) | `.act` | on |
| GIMP palette (.gpl) | `.gpl` | on |
| Krita palette (.kpl) | `.kpl` | on |
| JASC palette (.pal) | *`.pal`* | on |
| RIFF palette (.pal) | *`.pal`* | on |
| Hex colour list (.hex) | `.hex` | on |
| Procreate swatches | `.swatches` | on |

## Drawings and CAD

| Format | Files | Thumbnails |
|---|---|---|
| AutoCAD drawing (DXF) | `.dxf` | on |
| Rhino model (3DM) | `.3dm` | on |
| Windows Metafile (WMF) | `.wmf` `.wmz` | on |
| Enhanced Metafile (EMF) | `.emf` `.emz` | on |
| Macintosh PICT picture | `.pict` `.pct` `.pict1` `.pict2` | on |
| PDF document | `.pdf` | off |
| Adobe Illustrator drawing | `.ai` | on |
| Encapsulated PostScript (EPS) | `.eps` `.epsi` `.epsf` | off |
| PostScript document | `.ps` | off |
| SVG drawing | `.svg` `.svgz` `.svg.gz` | off |

## Documents with stored previews

| Format | Files | Thumbnails |
|---|---|---|
| Unreal Engine asset | `.uasset` | on |
| Cinema 4D scene | `.c4d` | on |
| ZBrush tool, project or document | `.ztl` `.zpr` `.zbr` | on |
| Procreate artwork | `.procreate` | on |
| Affinity document | `.afphoto` `.afdesign` `.afpub` `.af` | on |
| Sketch document | `.sketch` | on |
| Figma file | `.fig` | on |
| Clip Studio Paint illustration | `.clip` | on |
| Paint.NET image | `.pdn` | on |
| InDesign document | `.indd` | on |
| Adobe XD document | `.xd` | on |
| FreeCAD document | `.fcstd` | on |
| CorelDRAW drawing | `.cdr` `.cdt` `.cmx` | on |
| Visio drawing | `.vsd` `.vss` `.vst` `.vsdx` `.vsdm` `.vstx` `.vssx` | on |
| AutoCAD drawing | `.dwg` | on |
| Autodesk DWF or DWFx file | `.dwf` `.dwfx` | on |
| SolidWorks part, assembly or drawing | `.sldprt` `.sldasm` `.slddrw` | on |
| Autodesk Inventor file | `.ipt` `.iam` `.idw` `.ipn` | on |
| Autodesk Revit project or family | `.rvt` `.rfa` `.rte` | on |
| Solid Edge part, assembly or draft | `.psm` `.dft` *`.par`* *`.asm`* | on |
| Autodesk Fusion design | `.f3d` | on |
| Apple Keynote, Pages or Numbers document | `.key` `.pages` `.numbers` | on |
| Paint Shop Pro image, frame or tube | `.pspimage` `.pspframe` `.psptube` *`.psp`* *`.tub`* *`.pfr`* | on |
| Pixelmator Pro document | *`.pxd`* | on |
| PaintTool SAI document | `.sai` `.sai2` | on |

## Game archives

| Format | Files | Thumbnails |
|---|---|---|
| Quake PAK archive | `.pak` | on |
| id Tech 4 archive (PK4) | `.pk4` | on |
| Godot package (PCK) | *`.pck`* | on |
| Valve package (VPK) | `.vpk` | on |
| Build engine group file (GRP) | `.grp` | on |
| Unity package | `.unitypackage` | on |

## Toolpaths and GPS tracks

| Format | Files | Thumbnails |
|---|---|---|
| G-code toolpath | `.gcode` `.gco` *`.g`* *`.gc`* *`.nc`* *`.ngc`* *`.tap`* *`.cnc`* | on |
| Binary G-code | `.bgcode` | on |
| GPS track (GPX) | `.gpx` | on |
| Garmin activity (TCX) | `.tcx` | on |
| Activity file (FIT) | `.fit` | on |
| Map data (KML or KMZ) | `.kml` `.kmz` | on |
| GeoJSON | `.geojson` `.geo.json` | on |

## Medical images

| Format | Files | Thumbnails |
|---|---|---|
| DICOM medical image | `.dcm` *`.dicom`* | on |
| NIfTI medical volume | `.nii` `.nii.gz` | on |
| Analyze or NIfTI header (.hdr beside an .img) | *`.hdr`* | on |
| MetaImage volume | `.mha` `.mhd` | on |
| NRRD volume | `.nrrd` `.nhdr` | on |

## Scientific data

| Format | Files | Thumbnails |
|---|---|---|
| FITS astronomy image | `.fits` `.fit` `.fts` `.fits.fz` `.fit.fz` `.fts.fz` | on |
| PixInsight image (XISF) | `.xisf` | on |
| NumPy array | `.npy` `.npz` | on |
| OME-TIFF microscopy image | `.ome.tif` `.ome.tiff` `.ome.btf` `.ome.tf2` `.ome.tf8` | on |
| BigTIFF image | `.btf` `.tf2` `.tf8` | on |
| Aperio whole-slide image (SVS) | `.svs` | on |
| Hamamatsu whole-slide image (NDPI) | `.ndpi` | on |
| Leica whole-slide image (SCN) | `.scn` | on |
| HDF5 data | `.h5` `.hdf5` `.hdf` *`.he5`* | on |
| NetCDF data | `.nc` | on |
| MATLAB v7.3 data | *`.mat`* | on |
