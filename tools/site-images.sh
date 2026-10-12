#!/bin/bash
# Makes the website's pictures in site/img/: file-manager thumbnails, viewer
# screenshots, a before-and-after pair of Dolphin screenshots, two short video
# loops, a link-preview card, and images.json and CREDITS.md describing them.
# Every picture shows freely licensed samples from the test corpus
# (tools/fetch-test-assets.ts fetches them into local/assets/; run it for the
# site-extra category first).  The tables below say which sample each picture
# shows and who made it, and the manifest step writes that out for the pages.
#
# Usage: tools/site-images.sh [STEP...]
#   steps: thumbs screens filemanager loops social manifest (default: all)
#   SLERP=path picks the slerp binary (default /usr/local/bin/slerp).
#
# How each kind is made:
# - thumbs: `slerp thumbnail 512`, as a file manager asks for it; materials
#   whose file carries a preview picture are drawn on Slerp's own ball or cube
#   with `slerp render --state` instead.
# - screens: the viewer in a private X server (Xvfb) on lavapipe, Mesa's CPU
#   Vulkan driver, driven by SLERP_INPUT, as tools/readme-images.sh does.  Each
#   run gets fresh XDG_STATE_HOME and XDG_CONFIG_HOME folders (with a Breeze
#   Dark kdeglobals), so the user's own viewer settings are never read, and its
#   sample is copied to /tmp/samples/<name>/, since the inspector shows the
#   folder.
# - filemanager: Dolphin in gui-sandbox's headless sway, with a scratch HOME,
#   XDG folders, private D-Bus and thumbnail cache.  "Before" lists KDE's own
#   preview plugins without Slerp; "after" runs `slerp associate --apply` in
#   that scratch home, as a user would.  Thumbnails render on the integrated
#   GPU offscreen, since lavapipe can miss Dolphin's 2-second preview limit.
# - loops: `slerp render --state` frame by frame, cropped to 1280x800 and
#   encoded by ffmpeg as WebM (VP9) and MP4 (H.264), with a poster frame.
#
# Heavy commands run in their own scope in heavy-slerp.slice.  Work files go
# to local/site-work/.  Needs xvfb-run, Mesa's Vulkan drivers,
# ImageMagick 7 with WebP, ffmpeg, Deno, dolphin, sway and gui-sandbox.
set -euo pipefail
SELF=$(realpath "$0")
cd "$(dirname "$SELF")/.."
ROOT=$PWD
SLERP=$(realpath "${SLERP:-/usr/local/bin/slerp}")
A=$ROOT/local/assets
OUT=$ROOT/site/img
WORK=$ROOT/local/site-work/build
SAMPLES=/tmp/samples
LVP=/usr/share/vulkan/icd.d/lvp_icd.json

# ┌─────────────────╮
# │ Tables          │
# └─────────────────╯
#region Tables

# Every sample a picture shows: id | path under local/assets | format |
# category (as in docs/formats.md) | tools/assets.jsonc entry | author |
# licence (SPDX) | source page ("-" takes the entry's own URL).
CATALOGUE='
ship|site-extra/polyhaven/ship_pinnace/ship_pinnace_1k.gltf|glTF model|3D models|site-extra/polyhaven-ship_pinnace|James Ray Cock, Rico Cilliers, Nicolò Zubbini and Yann Kervran (Poly Haven)|CC0-1.0|https://polyhaven.com/a/ship_pinnace
ship2k|site-extra/polyhaven/ship_pinnace-2k/ship_pinnace_2k.gltf|glTF model|3D models|site-extra/polyhaven-ship_pinnace-2k|James Ray Cock, Rico Cilliers, Nicolò Zubbini and Yann Kervran (Poly Haven)|CC0-1.0|https://polyhaven.com/a/ship_pinnace
camera|site-extra/polyhaven/Camera_01/Camera_01_1k.gltf|glTF model|3D models|site-extra/polyhaven-Camera_01|Rajil Jose Macatangay (Poly Haven)|CC0-1.0|https://polyhaven.com/a/Camera_01
boombox|site-extra/polyhaven/boombox/boombox_1k.gltf|glTF model|3D models|site-extra/polyhaven-boombox|Thomas Paul Mouilleron (Poly Haven)|CC0-1.0|https://polyhaven.com/a/boombox
chest|site-extra/polyhaven/treasure_chest/treasure_chest_1k.gltf|glTF model|3D models|site-extra/polyhaven-treasure_chest|Rico Cilliers (Poly Haven)|CC0-1.0|https://polyhaven.com/a/treasure_chest
bust|site-extra/polyhaven/marble_bust_01/marble_bust_01_1k.gltf|glTF model|3D models|site-extra/polyhaven-marble_bust_01|Rico Cilliers (Poly Haven)|CC0-1.0|https://polyhaven.com/a/marble_bust_01
plant|site-extra/polyhaven/potted_plant_02/potted_plant_02_1k.gltf|glTF model|3D models|site-extra/polyhaven-potted_plant_02|Rico Cilliers (Poly Haven)|CC0-1.0|https://polyhaven.com/a/potted_plant_02
chess|site-extra/polyhaven/chess_set/chess_set_1k.gltf|glTF model|3D models|site-extra/polyhaven-chess_set|Riley Queen (Poly Haven)|CC0-1.0|https://polyhaven.com/a/chess_set
lantern|gltf/polyhaven/Lantern_01/Lantern_01_1k.gltf|glTF model|3D models|gltf/polyhaven-Lantern_01|Rajil Jose Macatangay (Poly Haven)|CC0-1.0|https://polyhaven.com/a/Lantern_01
stainedlamp|gltf-extra/StainedGlassLamp/glTF-KTX-BasisU/StainedGlassLamp.gltf|glTF model with KTX2 textures|3D models|gltf-extra/StainedGlassLamp|Eric Chadwick (Wayfair), from the Khronos glTF sample models|CC-BY-4.0|https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/StainedGlassLamp
olives|draco-extra/three/IridescentDishWithOlives.glb|glTF binary model (GLB) with Draco|3D models|draco-extra/three-IridescentDishWithOlives.glb|Eric Chadwick (Wayfair), from the three.js examples|CC-BY-4.0|-
tokyo|gltf/three/LittlestTokyo.glb|glTF binary model (GLB) with Draco|3D models|gltf/three-littlesttokyo|Glen Fox, from the three.js examples|CC-BY-4.0|-
nefertiti|gltf/three/Nefertiti.glb|glTF binary model (GLB)|3D models|gltf/three-nefertiti|Nora Al-Badri and Jan Nikolai Nelles (the Nefertiti Hack scan), from the three.js examples|CC0-1.0|-
teapot|usd/usdwg-teapot/Teapot.usd|USD scene|3D models|usd/usdwg-teapot|The USD Working Group|CC0-1.0|https://github.com/usd-wg/assets/tree/main/full_assets/Teapot
knight|vox/ephtracy/vox/character/chr_knight.vox|MagicaVoxel model|3D models|vox/ephtracy|ephtracy (MagicaVoxel sample models)|MIT|-
monu7|vox/ephtracy/vox/monument/monu7.vox|MagicaVoxel model|3D models|vox/ephtracy|ephtracy (MagicaVoxel sample models)|MIT|-
island|minecraft-extra/schematic4j/litematica/v5/island.litematic|Minecraft schematic (Litematica)|3D models|minecraft-extra/schematic4j-litematica-v5-island.litematic|SandroHc (schematic4j test data)|MIT|https://github.com/SandroHc/schematic4j
mansion|minecraft-extra/schematic4j/litematica/v5/mansion.litematic|Minecraft schematic (Litematica)|3D models|minecraft-extra/schematic4j-litematica-v5-mansion.litematic|SandroHc (schematic4j test data)|MIT|https://github.com/SandroHc/schematic4j
robot|site-extra/three/RobotExpressive.glb|glTF binary model (GLB)|3D models|site-extra/three-robotexpressive|Tomás Laulhé (Quaternius), edited by Don McCurdy|CC0-1.0|https://github.com/mrdoob/three.js/tree/dev/examples/models/gltf/RobotExpressive
gene|rigged-extra/pmx/gene/Gene_light.pmx|MikuMikuDance model (PMX)|3D models|rigged-extra/mmd-pmx-gene|Nagoya Institute of Technology (credit line: CG-CA Gene (c) 2023 by Nagoya Institute of Technology)|CC-BY-4.0|-
yeti|autodesk-extra/maya/exporters_yeti_apose/Yeti_Apose.ma|Maya scene|3D models|autodesk-extra/exporters_yeti_apose|The Babylon.js exporters project|Apache-2.0|-
shambler|quake/librequake/id1/pak0/progs/shambler.mdl|Quake model (MDL)|3D models|quake/librequake|LibreQuake contributors|BSD-3-Clause|https://github.com/lavenderdotpet/LibreQuake
heart|models-extra/3mf-samples/core/heartgears.3mf|3MF model|3D models|models-extra/3mf-samples-core|3MF Consortium|BSD-2-Clause|-
lizard|splats/hornedlizard.spz|Compressed Gaussian splats (SPZ)|Point clouds and splats|splats/hornedlizard-spz|Niantic|MIT|https://github.com/nianticlabs/spz
biker|splats/biker.spz|Compressed Gaussian splats (SPZ)|Point clouds and splats|splats/biker-spz|PlayCanvas|MIT|https://github.com/playcanvas/engine
guitar|splats/guitar.compressed.ply|Gaussian splats (compressed PLY)|Point clouds and splats|splats/guitar-compressed-ply|PlayCanvas|MIT|https://github.com/playcanvas/engine
diorama|blend-extra/demos/asset-demo-bundle-3.0-cube-diorama/cube_diorama/cube_diorama.blend|Blender file|Scenes and projects|blend-extra/demos-asset-demo-bundle-3.0-cube-diorama|Blender Studio|CC0-1.0|https://www.blender.org/download/demo-files/
godot|engines-extra/godot-demo-projects/3d/platformer/game.tscn|Godot scene|Scenes and projects|engines-extra/godot-demo-projects|Godot Engine contributors (godot-demo-projects)|MIT|-
freedoom|doom/freedoom/freedoom2.wad|Doom WAD|Levels and maps|doom/freedoom|Freedoom contributors|BSD-3-Clause|https://freedoom.github.io/
lqdm3|quake/librequake/id1/maps/lqdm3.bsp|Quake map (BSP)|Levels and maps|quake/librequake|LibreQuake contributors|BSD-3-Clause|https://github.com/lavenderdotpet/LibreQuake
lqe1m1|quake/librequake/id1/pak0/maps/lq_e1m1.bsp|Quake map (BSP)|Levels and maps|quake/librequake|LibreQuake contributors|BSD-3-Clause|https://github.com/lavenderdotpet/LibreQuake
village|roblox-extra/open-game-eval/village.rbxl|Roblox place|Levels and maps|roblox-extra/open-game-eval|Roblox (open-game-eval Studio templates)|MIT|-
pirate|roblox-extra/open-game-eval/pirate_island.rbxl|Roblox place|Levels and maps|roblox-extra/open-game-eval|Roblox (open-game-eval Studio templates)|MIT|-
bricks|materials/ambientcg/Bricks097/Bricks097_1K-JPG.mtlx|MaterialX material|Materials|materials/ambientcg-Bricks097|ambientCG|CC0-1.0|https://ambientcg.com/view?id=Bricks097
leaf|materials-extra/ambientcg/Leaf001/Leaf001_1K-JPG.mtlx|MaterialX material|Materials|materials-extra/ambientcg-Leaf001|ambientCG|CC0-1.0|https://ambientcg.com/view?id=Leaf001
wood|materials/ambientcg/Wood066/Wood066_1K-JPG.tres|Godot material|Materials|materials/ambientcg-Wood066|ambientCG|CC0-1.0|https://ambientcg.com/view?id=Wood066
desk|textures/ktx-software/ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2|KTX 2 texture (UASTC HDR)|Textures|textures/ktx-software|The KTX-Software project (test data; the Desk picture comes from the OpenEXR sample images)|Apache-2.0|-
ddscube|textures/dds/cube/Pretoria Gardens DX10.dds|DirectDraw Surface cube map (DDS)|Textures|textures/dds|The image-rs project (image-dds test data)|MIT OR Apache-2.0|-
venice|hdr/venice_sunset_1k.hdr|Radiance HDR image|Images|hdr/venice_sunset_1k|Greg Zaal (Poly Haven)|CC0-1.0|https://polyhaven.com/a/venice_sunset
venice4k|hdr/venice_sunset_4k.hdr|Radiance HDR image|Images|hdr/venice_sunset_4k|Greg Zaal (Poly Haven)|CC0-1.0|https://polyhaven.com/a/venice_sunset
kloof|hdr/kloofendal_48d_partly_cloudy_puresky_1k.hdr|Radiance HDR image|Images|hdr/kloofendal_48d_partly_cloudy_puresky_1k|Greg Zaal and Jarod Guest (Poly Haven)|CC0-1.0|https://polyhaven.com/a/kloofendal_48d_partly_cloudy_puresky
studio|hdr/studio_small_09_1k.hdr|Radiance HDR image|Images|hdr/studio_small_09_1k|Sergej Majboroda (Poly Haven)|CC0-1.0|https://polyhaven.com/a/studio_small_09
dungeon|site-extra/kenney-tiny-dungeon/Tiled/sampleMap.tmx|Tiled map|Sprites and tile maps|site-extra/kenney-tiny-dungeon|Kenney|CC0-1.0|https://kenney.nl/assets/tiny-dungeon
demon|site-extra/duelyst/units/f4_demonofeternity/f4_demonofeternity_idle.ase|Aseprite sprite|Sprites and tile maps|site-extra/duelyst-units|Counterplay Games (Duelyst), converted to Aseprite by valiet|CC0-1.0|https://github.com/valiet/assets_duelyst_units
golem|site-extra/duelyst/units/neutral_moltengolem/neutral_moltengolem_idle.ase|Aseprite sprite|Sprites and tile maps|site-extra/duelyst-units|Counterplay Games (Duelyst), converted to Aseprite by valiet|CC0-1.0|https://github.com/valiet/assets_duelyst_units
tpsheet|retro-extra/atlas/tpsheet-ld39/tileset-1.tpsheet|TexturePacker sprite sheet data|Sprites and tile maps|retro-extra/atlas-tpsheet-ld39-tileset-1-tpsheet|schme16 (the Ludum Dare 39 game Running out of Power)|MIT|https://github.com/schme16/LD39-Running-out-of-Power
peggy|site-extra/gdquest/.kra/Peggy.kra|Krita document|Images|site-extra/gdquest-krita-sprites|GDquest|CC0-1.0|https://opengameart.org/content/cute-painterly-characters-and-forest
jaguar|site-extra/gdquest/.kra/Jaguar.kra|Krita document|Images|site-extra/gdquest-krita-sprites|GDquest|CC0-1.0|https://opengameart.org/content/cute-painterly-characters-and-forest
candle|images23-extra/exr/openexr-images/ScanLines/CandleGlass.exr|OpenEXR image|Images|images23-extra/exr-openexr-images|Industrial Light and Magic (OpenEXR sample images)|BSD-3-Clause|-
goldengate|images23-extra/exr/openexr-images/Tiles/GoldenGate.exr|OpenEXR image|Images|images23-extra/exr-openexr-images|Industrial Light and Magic (OpenEXR sample images)|BSD-3-Clause|-
silove|retro-extra/ansi/ansilove-n-silove.ans|ANSI art|Retro computer art|retro-extra/ansi-ansilove-n-silove-ans|Bjorn Odendahl, from the ansilove examples|BSD-2-Clause|https://github.com/ansilove/ansilove
al02|retro-extra/ansi/ansilove-cl!-al02.ans|ANSI art|Retro computer art|retro-extra/ansi-ansilove-cl-al02-ans|Frederic Cambus, from the ansilove examples|BSD-2-Clause|https://github.com/ansilove/ansilove
bbc|retro-extra/bbc/funky-fresh-logo-mode2.bbc|BBC Micro screen dump|Retro computer art|retro-extra/bbc-funky-fresh-logo-mode2-bin|Kieran Connell (the Bitshifters demo funky-fresh)|MIT|https://github.com/kieranhj/funky-fresh
pear36|editors-extra/palettes/hex/cpc-pear36.hex|Hex colour list|Palettes and colour tables|editors-extra/pal-hex-cpc-pear36.hex|PineappleOnPizza (via ColorPaletteCodable)|MIT|https://github.com/dagronf/ColorPaletteCodable
db16|editors-extra/palettes/pal-jasc/cpc-DB16.pal|JASC palette (.pal)|Palettes and colour tables|editors-extra/pal-pal-jasc-cpc-DB16.pal|DawnBringer (via ColorPaletteCodable)|MIT|https://github.com/dagronf/ColorPaletteCodable
twilight|luts/Matplotlib_cyclic_twilight.cube|Colour lookup table (Cube)|Palettes and colour tables|luts/matplotlib-twilight|derlunz (a lookup table of a Matplotlib colour map)|MIT|https://github.com/derlunz/matplotlib-colormap-luts
clover|cad-extra/dxf/clover/deck.dxf|AutoCAD drawing (DXF)|Drawings and CAD|cad-extra/dxf-clover|COEX (Clover drone kit)|MIT|-
pcfan|editors-extra/solidworks/pcfan-PC_Fan.SLDASM|SolidWorks assembly|Documents with stored previews|editors-extra/solidworks-pcfan-PC_Fan.SLDASM|tomic-pasko|CC0-1.0|https://github.com/tomic-pasko/PC_Fan_Solidworks
trex|editors-extra/dwf/aspose-Tyrannosaurus.dwfx|Autodesk DWFx file|Documents with stored previews|editors-extra/dwf-aspose-Tyrannosaurus.dwfx|Aspose (Aspose.CAD examples)|MIT|https://github.com/aspose-cad/Aspose.CAD-for-Java
xstile|editors-extra/affinity/xs-tileset.af|Affinity document|Documents with stored previews|editors-extra/affinity-xs-tileset.af|Breda University of Applied Sciences (xs game engine)|MIT|https://github.com/BredaUniversityGames/xs
gcode|site-extra/nefertiti-gcode/nefertiti.gcode|G-code toolpath|Toolpaths and GPS tracks|site-extra/nefertiti-gcode|Nora Al-Badri and Jan Nikolai Nelles (the Nefertiti Hack scan), sliced into G-code for this site|CC0-1.0|https://github.com/mrdoob/three.js/tree/dev/examples/models/gltf/Nefertiti
gpx|paths-extra/gpxpy/Mojstrovka.gpx|GPS track (GPX)|Toolpaths and GPS tracks|paths-extra/gpxpy|Tomo Krajina (gpxpy test data)|Apache-2.0|-
brain|volumes/nifti/MNI152_T1_1mm_2009c.nii.gz|NIfTI medical volume|Medical images|volumes/templateflow-mni152-t1-1mm|TemplateFlow (the MNI152NLin2009cAsym brain template)|CC0-1.0|https://www.templateflow.org/
ct|dicom/tcia-lidc-idri-0001/ct/00000060.dcm|DICOM medical image|Medical images|dicom/tcia-lidc-idri-0001-ct|The Cancer Imaging Archive, LIDC-IDRI collection (Armato and others, 2011)|CC-BY-3.0|https://doi.org/10.7937/K9/TCIA.2015.LO9QL9SX
slide|volumes/slides/CMU-1-Small-Region.svs|Aperio whole-slide image (SVS)|Scientific data|volumes/openslide-cmu-1-small-region|Carnegie Mellon University (OpenSlide test data)|CC0-1.0|https://openslide.cs.cmu.edu/download/openslide-testdata/
'

# Thumbnails: name | sample | how (thumb, or material:SHAPE:TILING:DISTANCE
# for Slerp's own ball or cube) | title | alt text.
THUMBS='
ship|ship|thumb|Sailing ship|A Slerp thumbnail of a three-masted wooden sailing ship with grey sails, from a glTF model.
camera|camera|thumb|Vintage camera|A Slerp thumbnail of a black rangefinder camera with a brown leather strap, from a glTF model.
boombox|boombox|thumb|Boombox|A Slerp thumbnail of a black cassette boombox with its aerial raised, from a glTF model.
chest|chest|thumb|Treasure chest|A Slerp thumbnail of a wooden treasure chest bound with iron, from a glTF model.
bust|bust|thumb|Marble bust|A Slerp thumbnail of a weathered marble bust of a Roman man, from a glTF model.
plant|plant|thumb|Potted plant|A Slerp thumbnail of a leafy plant in a terracotta pot, from a glTF model.
chess|chess|thumb|Chess set|A Slerp thumbnail of a chess board with its pieces set up, from a glTF model.
lantern|lantern|thumb|Oil lantern|A Slerp thumbnail of a brass oil lantern with a glass chimney, from a glTF model.
stainedlamp|stainedlamp|thumb|Stained glass lamp|A Slerp thumbnail of a table lamp with a colourful stained glass shade, from a glTF model with KTX2 textures.
olives|olives|thumb|Dish of olives|A Slerp thumbnail of an iridescent glass dish holding green and black olives, from a Draco-compressed GLB file.
tokyo|tokyo|thumb|Littlest Tokyo|A Slerp thumbnail of a tiny, detailed diorama of a Tokyo street corner, from a Draco-compressed GLB file.
nefertiti|nefertiti|thumb|Bust of Nefertiti|A Slerp thumbnail of a 3D scan of the painted bust of Nefertiti, from a GLB file.
teapot|teapot|thumb|Utah teapot|A Slerp thumbnail of the classic Utah teapot, from a USD scene.
knight|knight|thumb|Voxel knight|A Slerp thumbnail of a small voxel knight holding an axe, from a MagicaVoxel model.
monu7|monu7|thumb|Voxel town|A Slerp thumbnail of a colourful voxel town on a sandy base, from a MagicaVoxel model.
island|island|thumb|Floating island|A Slerp thumbnail of a floating Minecraft island with houses, trees and a waterfall, from a Litematica schematic.
mansion|mansion|thumb|Minecraft mansion|A Slerp thumbnail of a timber-framed Minecraft mansion with green roofs, from a Litematica schematic.
robot|robot|thumb|Robot character|A Slerp thumbnail of a yellow cartoon robot in its rest pose, from a GLB file with 14 animations.
gene|gene|thumb|Anime character|A Slerp thumbnail of an anime-style character in a white jacket, from a MikuMikuDance PMX model.
yeti|yeti|thumb|Yeti|A Slerp thumbnail of a horned cartoon yeti, from a Maya scene.
shambler|shambler|thumb|Quake monster|A Slerp thumbnail of a hunched horned monster from LibreQuake, from a Quake MDL model.
heart|heart|thumb|Heart of gears|A Slerp thumbnail of a white heart split by interlocking gear teeth, from a 3MF print file.
lizard|lizard|thumb|Horned lizard|A Slerp thumbnail of a Gaussian splat capture of a horned lizard sculpture on a stone block.
biker|biker|thumb|Biker|A Slerp thumbnail of a Gaussian splat capture of a person in black motorcycle leathers.
guitar|guitar|thumb|Guitar|A Slerp thumbnail of a Gaussian splat capture of an electric guitar on a stand.
diorama|diorama|thumb|Room diorama|A Slerp thumbnail of a small room diorama with a window, from the picture saved in a Blender file.
godot|godot|thumb|Platformer level|A Slerp thumbnail of a blocky green platformer level, from a Godot scene.
freedoom|freedoom|thumb|Freedoom|A Slerp thumbnail of the Freedoom title screen, taken from a Doom WAD.
lqdm3|lqdm3|thumb|Quake level|A Slerp thumbnail of a LibreQuake deathmatch level seen from above, from a BSP map.
village|village|thumb|Roblox village|A Slerp thumbnail of a Roblox village on a grassy island, from a Roblox place file.
pirate|pirate|thumb|Pirate islands|A Slerp thumbnail of three small islands in a blue sea, from a Roblox place file.
bricks|bricks|material:sphere:2:2.0|Brick material|A brick wall material on a ball, rendered by Slerp from a MaterialX file.
leaf|leaf|material:sphere:1:2.0|Leaf material|A leaf material with cut-out edges on a ball, rendered by Slerp from a MaterialX file.
wood|wood|material:cube:1:2.6|Wood material|A dark wood material on a cube, rendered by Slerp from a Godot material file.
desk|desk|thumb|HDR texture|A Slerp thumbnail of a desk lit by a stained glass window, from a KTX2 texture compressed as UASTC HDR.
ddscube|ddscube|thumb|Cube map|A Slerp thumbnail of a park panorama laid out as a cube map cross, from a DDS texture.
venice|venice|thumb|Venice sunset sky|A Slerp thumbnail of a panorama of the Venice waterfront at sunset, from a Radiance HDR file.
kloof|kloof|thumb|Cloudy sky|A Slerp thumbnail of a panorama of a blue sky with white clouds, from a Radiance HDR file.
dungeon|dungeon|thumb|Dungeon tile map|A Slerp thumbnail of a pixel-art dungeon map with rooms, rails and characters, from a Tiled map.
demon|demon|thumb|Pixel art demon|A Slerp thumbnail of a pixel-art demon with glowing blue hands, from an Aseprite sprite.
golem|golem|thumb|Pixel art golem|A Slerp thumbnail of a pixel-art golem of cracked black stone and glowing lava, from an Aseprite sprite.
tpsheet|tpsheet|thumb|Sprite sheet|A Slerp thumbnail of a sheet of grassy road tiles with each sprite outlined, from TexturePacker sprite sheet data.
peggy|peggy|thumb|Painted pig|A Slerp thumbnail of a painted cartoon pig in a blue dress, from a Krita document.
jaguar|jaguar|thumb|Painted jaguar|A Slerp thumbnail of a painted cartoon jaguar, from a Krita document.
candle|candle|thumb|Candle and glass|A Slerp thumbnail of a lit candle beside a wine glass, from an OpenEXR image.
goldengate|goldengate|thumb|Golden Gate at dusk|A Slerp thumbnail of the Golden Gate Bridge at dusk, from an OpenEXR image.
silove|silove|thumb|ANSI art|A Slerp thumbnail of colourful ANSI art lettering, from an ANSI file.
al02|al02|thumb|ANSI art logo|A Slerp thumbnail of red and pink ANSI art lettering on purple, from an ANSI file.
bbc|bbc|thumb|BBC Micro logo|A Slerp thumbnail of a striped logo over a glowing sun, from a BBC Micro screen dump.
pear36|pear36|thumb|Colour palette|A Slerp thumbnail of a 36-colour palette shown as swatches, from a hex colour list.
db16|db16|thumb|16-colour palette|A Slerp thumbnail of a 16-colour pixel art palette shown as swatches, from a JASC palette.
twilight|twilight|thumb|Colour lookup table|A Slerp thumbnail of a photo split diagonally to show it before and after a colour lookup table.
clover|clover|thumb|Drone frame plate|A Slerp thumbnail of a laser-cut drone frame plate, from a DXF drawing.
pcfan|pcfan|thumb|PC fan|A Slerp thumbnail of a computer fan, from the picture saved in a SolidWorks assembly.
trex|trex|thumb|T. rex skeleton|A Slerp thumbnail of a line drawing of a T. rex skeleton, from the picture saved in a DWFx file.
xstile|xstile|thumb|Game tile set|A Slerp thumbnail of a pixel-art game tile set, from the picture saved in an Affinity document.
gcode|gcode|thumb|3D print toolpath|A Slerp thumbnail of a 3D printer toolpath of the bust of Nefertiti with green supports, from G-code.
gpx|gpx|thumb|GPS track|A Slerp thumbnail of a mountain hiking track coloured by height, from a GPX file.
brain|brain|thumb|Brain MRI|A Slerp thumbnail of a slice through a brain MRI template, from a NIfTI volume.
ct|ct|thumb|Chest CT|A Slerp thumbnail of a slice of a chest CT scan, from a DICOM file.
slide|slide|thumb|Tissue slide|A Slerp thumbnail of a stained tissue sample, from an Aperio whole-slide image.
'

# Viewer screenshots: name | files to copy (paths under local/assets; a
# folder copies its contents) | file to open | panels (bare, panels or
# noinspector) | view state ({L} is the lighting folder) or - | scripted input
# or - | samples shown | title | alt text.
SCREENS='
model|site-extra/polyhaven/ship_pinnace-2k|ship_pinnace_2k.gltf|bare|{"orbit": {"yaw": -38, "pitch": 7, "fov": 30, "distance": 72, "target": [0, 13, 1.5]}, "environment": "{L}/venice_sunset_4k.hdr", "background": "environment:1", "environmentRotation": 150, "keyLight": 0}|-|ship2k venice4k|A 3D model|The Slerp viewer showing a detailed wooden sailing ship lit by a sunset sky, with the side panels hidden.
contents|site-extra/polyhaven/chess_set|chess_set_1k.gltf|panels|-|drag:30,5 moveat:110,306 wait:10|chess|The Contents panel and the inspector|The Slerp viewer with the Contents panel listing the pieces of a chess set, the inspector showing its details, and the hovered queen outlined on the board.
animation|site-extra/three/RobotExpressive.glb|RobotExpressive.glb|noinspector|{"clip": "Dance", "time": 1.15, "orbit": {"yaw": 25, "pitch": 8, "fov": 35}}|-|robot|A character playing an animation|The Slerp viewer playing the Dance clip of a cartoon robot, with its list of clips and its morph target sliders.
level|quake/librequake/id1/pak0/maps/lq_e1m1.bsp quake/librequake/id1/pak0/maps/lq_e1m1.lit|lq_e1m1.bsp|bare|{"fly": {"position": [28.6512, 19.7104, 11.9888], "yaw": 45, "pitch": -26, "fov": 75}, "exposure": 1.5}|-|lqe1m1|A Quake level in first person|The Slerp viewer walking through a LibreQuake level in first person, with riveted metal walls, a curved stair and a cloudy sky.
texture|textures/ktx-software/ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2|Desk_uastc_hdr4x4_zstd_15.ktx2|panels|-|moveat:1750,1100|desk|A GPU texture and its details|The Slerp viewer showing a KTX2 texture compressed as UASTC HDR, a desk under a stained glass window, with its format details and exposure controls.
sky|hdr/venice_sunset_4k.hdr|venice_sunset_4k.hdr|panels|-|moveat:1750,1100|venice4k|An HDR sky|The Slerp viewer showing a panorama of the Venice waterfront at sunset from a Radiance HDR file, with its details and tone mapping controls.
splat|splats/hornedlizard.spz|hornedlizard.spz|bare|-|-|lizard|A Gaussian splat|The Slerp viewer showing a Gaussian splat capture of a horned lizard sculpture in a garden.
minecraft|minecraft-extra/schematic4j/litematica/v5/island.litematic|island.litematic|bare|-|scroll:3 drag:-40,8|island|A Minecraft schematic|The Slerp viewer showing a floating Minecraft island with houses, trees and a waterfall, from a Litematica schematic.
ct|dicom/tcia-lidc-idri-0001/ct|00000060.dcm|panels|-|moveat:1750,1100|ct|A CT series|The Slerp viewer showing a chest CT slice from a DICOM series, with window presets, the series slider and the DICOM tags.
mri|volumes/nifti/MNI152_T1_1mm_2009c.nii.gz|MNI152_T1_1mm_2009c.nii.gz|panels|-|moveat:1750,1100|brain|An MRI volume|The Slerp viewer showing a slice of a brain MRI volume, with plane, window and colour controls and a histogram of its values.
gcode|site-extra/nefertiti-gcode/nefertiti.gcode|nefertiti.gcode|panels|-|drag:-70,0|gcode|A G-code toolpath|The Slerp viewer showing a 3D printer toolpath of the bust of Nefertiti, with a layer slider and the print features listed by colour.
material|materials/ambientcg/Bricks097|Bricks097_1K-JPG.mtlx|panels|-|-|bricks|A material on a ball|The Slerp viewer showing a brick MaterialX material on a ball, with shape, tiling and displacement controls and the file'"'"'s own preview.
'

# The Dolphin folder: file name | sample | copy, or convert to GLB (a glTF
# with its .bin and textures would clutter the folder).
FOLDER='
boombox.glb|boombox|convert
brain_mri.nii.gz|brain|copy
Camera_01.glb|camera|convert
chest_ct.dcm|ct|copy
chr_knight.vox|knight|copy
CMU-1-Small-Region.svs|slide|copy
demon_of_eternity.ase|demon|copy
desk.ktx2|desk|copy
freedoom2.wad|freedoom|copy
guitar.compressed.ply|guitar|copy
hornedlizard.spz|lizard|copy
IridescentDishWithOlives.glb|olives|copy
LittlestTokyo.glb|tokyo|copy
lqdm3.bsp|lqdm3|copy
Mojstrovka.gpx|gpx|copy
monu7.vox|monu7|copy
nefertiti.gcode|gcode|copy
Nefertiti.glb|nefertiti|copy
PC_Fan.SLDASM|pcfan|copy
pear36.hex|pear36|copy
RobotExpressive.glb|robot|copy
shambler.mdl|shambler|copy
ship_pinnace.glb|ship|convert
silove.ans|silove|copy
treasure_chest.glb|chest|convert
twilight.cube|twilight|copy
venice_sunset_1k.hdr|venice|copy
village.rbxl|village|copy
'

# The rest of the pictures, for the manifest: path | kind | samples | title | alt text.
OTHERS='
filemanager/before.webp|file manager|FOLDER|Dolphin before Slerp|Dolphin showing a folder of 3D models, game files, textures, scans and sprites as generic file icons, before Slerp'"'"'s thumbnailer is switched on.
filemanager/after.webp|file manager|FOLDER|Dolphin with Slerp|The same Dolphin folder with Slerp'"'"'s thumbnails: ships, cameras, splats, game levels, sprites, scans and textures each shown as a picture of what is inside.
loops/turntable.webm|video|ship2k venice4k|Ship turntable|A wooden sailing ship turning slowly in front of a soft sunset sky, rendered by Slerp.
loops/turntable.mp4|video|ship2k venice4k|Ship turntable|A wooden sailing ship turning slowly in front of a soft sunset sky, rendered by Slerp.
loops/turntable-poster.webp|poster|ship2k venice4k|Ship turntable|A wooden sailing ship in front of a soft sunset sky, rendered by Slerp.
loops/dance.webm|video|robot studio|Dancing robot|A yellow cartoon robot playing its Dance animation in a bright studio, rendered by Slerp.
loops/dance.mp4|video|robot studio|Dancing robot|A yellow cartoon robot playing its Dance animation in a bright studio, rendered by Slerp.
loops/dance-poster.webp|poster|robot studio|Dancing robot|A yellow cartoon robot dancing in a bright studio, rendered by Slerp.
social/og.png|link preview|ship tokyo lizard peggy robot monu7 demon bricks brain gcode stainedlamp dungeon|Slerp link preview|The name Slerp and a one-line description beside a grid of Slerp thumbnails of a ship, a diorama, a splat, a painting, a robot, voxels, pixel art, a material, a brain scan, a toolpath, a lamp and a tile map.
'

# The thumbnails on the link-preview card, four to a row.
SOCIAL_TILES="ship tokyo lizard peggy robot monu7 demon bricks brain gcode stainedlamp dungeon"

#endregion

# ┌─────────────────╮
# │ Helpers         │
# └─────────────────╯
#region Helpers

# The rows of a table, without blank lines.
rows() { grep -v '^[[:space:]]*$' <<<"$1"; }

# A field of a sample's catalogue row: path, format, ...
sample() {
	local col
	case $2 in path) col=2;; format) col=3;; category) col=4;; entry) col=5;; esac
	rows "$CATALOGUE" | awk -F'|' -v id="$1" -v c="$col" '$1 == id { print $c; found = 1 } END { if (!found) exit 1 }' \
		|| { echo "no sample $1 in the catalogue" >&2; exit 1; }
}

# Runs a command in its own scope in heavy-slerp.slice, so a runaway render is
# killed alone rather than with the agent or desktop that started it.
heavy() { systemd-run --user --scope --quiet --slice=heavy-slerp.slice "$@"; }

# Fresh state, config and runtime folders for one job, so neither the user's
# settings nor their resident helper are used.
fresh() {
	rm -rf "$WORK/$1"
	mkdir -p "$WORK/$1/state" "$WORK/$1/config" "$WORK/$1/run"
	chmod 700 "$WORK/$1/run"
}

# Runs slerp on lavapipe with job $1's folders.
lvp() {
	local d=$WORK/$1
	shift
	heavy env VK_DRIVER_FILES=$LVP SLERP_GPU=software XDG_STATE_HOME="$d/state" XDG_CONFIG_HOME="$d/config" \
		XDG_RUNTIME_DIR="$d/run" "$SLERP" "$@"
}

# Stops the resident helpers this script started: those running $SLERP with a
# runtime folder under $WORK.  (Matching command lines with pkill -f would
# also match the shell running this.)
stop_helpers() {
	local p
	for p in $(pgrep -x slerp || true); do
		[ "$(readlink "/proc/$p/exe" 2>/dev/null)" = "$SLERP" ] || continue
		tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -q "^XDG_RUNTIME_DIR=$WORK/" && kill "$p" 2>/dev/null || true
	done
}

# Copies sample files into a folder: folders by their contents.
copy_into() {
	local dest=$1 src
	shift
	mkdir -p "$dest"
	for src in "$@"; do
		if [ -d "$A/$src" ]; then cp -r "$A/$src/." "$dest/"; else cp "$A/$src" "$dest/"; fi
	done
}

#endregion

# ┌─────────────────╮
# │ Steps           │
# └─────────────────╯
#region Steps

# 1. Thumbnails, 512 px, as WebP with transparency.
thumbs() {
	fresh thumbs
	rm -rf "$OUT/thumbs"
	mkdir -p "$WORK/thumbs/png" "$OUT/thumbs"
	local name id how title alt png shape tiling dist
	while IFS='|' read -r name id how title alt; do
		png=$WORK/thumbs/png/$name.png
		case $how in
			# The helper answers within Dolphin's 2-second limit, which lavapipe on
			# a busy machine can miss for big files, so try again, then render
			# the same picture without the limit.
			thumb)
				lvp thumbs thumbnail 512 "$A/$(sample "$id" path)" "$png" || lvp thumbs thumbnail 512 "$A/$(sample "$id" path)" "$png" \
					|| lvp thumbs render --size 512 "$A/$(sample "$id" path)" "$png" ;;
			material:*)
				IFS=: read -r _ shape tiling dist <<<"$how"
				printf '{"material": {"shape": "%s", "tiling": %s}, "orbit": {"yaw": 30, "pitch": 18, "fov": 35, "distance": %s}}\n' \
					"$shape" "$tiling" "$dist" >"$WORK/thumbs/$name.jsonc"
				lvp thumbs render --size 512 --state "$WORK/thumbs/$name.jsonc" "$A/$(sample "$id" path)" "$png" ;;
		esac
		[ -s "$png" ] || { echo "no thumbnail of $id" >&2; exit 1; }
		magick "$png" -quality 88 -define webp:alpha-quality=95 -define webp:method=6 "$OUT/thumbs/$name.webp"
	done < <(rows "$THUMBS")
	stop_helpers
}

# 2. Viewer screenshots at 1920x1200, Breeze Dark.
screens() {
	rm -rf "$OUT/screens"
	mkdir -p "$OUT/screens"
	rm -rf "$SAMPLES"
	copy_into "$SAMPLES/lighting" "$(sample venice4k path)"
	local name copy open panels state input ids title alt d
	while IFS='|' read -r name copy open panels state input ids title alt; do
		d=$WORK/screens/$name
		fresh "screens/$name"
		# shellcheck disable=SC2086 # the copy list is space-separated
		copy_into "$SAMPLES/$name" $copy
		mkdir -p "$d/state/slerp"
		case $panels in
			bare) echo '{"hidePanels": true}' ;;
			noinspector) echo '{"hideInspector": true}' ;;
			*) echo '{}' ;;
		esac >"$d/state/slerp/viewer.json"
		printf '[General]\nColorScheme=BreezeDark\n\n[Icons]\nTheme=breeze-dark\n' >"$d/config/kdeglobals"
		local vs=()
		if [ "$state" != - ]; then
			printf '%s\n' "${state//\{L\}/$SAMPLES/lighting}" >"$d/view.jsonc"
			vs=("SLERP_VIEW_STATE=$d/view.jsonc")
		fi
		[ "$input" = - ] && input=
		heavy env VK_DRIVER_FILES=$LVP SLERP_GPU=software XDG_STATE_HOME="$d/state" XDG_CONFIG_HOME="$d/config" \
			XDG_RUNTIME_DIR="$d/run" SLERP_NO_HANDOFF=1 SLERP_VIEWER_KEEP_ALIVE=0 SLERP_WINDOW_SIZE=1920x1200 \
			SLERP_QUIT_AFTER=300 SLERP_INPUT="wait:20 $input wait:40 shot:$d/shot.png ctrl+Q" "${vs[@]}" \
			xvfb-run -a -s "-screen 0 1920x1200x24" env -u WAYLAND_DISPLAY "$SLERP" view "$SAMPLES/$name/$open" \
			>"$d/log" 2>&1 || true
		[ -s "$d/shot.png" ] || { echo "no screenshot of $open (see $d/log)" >&2; exit 1; }
		magick "$d/shot.png" -quality 88 -define webp:method=6 "$OUT/screens/$name.webp"
	done < <(rows "$SCREENS")
	rm -rf "$SAMPLES"
}

# 3. Dolphin before and after, 1600x1000.  The window state hides every panel
# (Places would list this computer's disks, the terminal its user name); it
# was saved by Dolphin itself after toggling the panels off.
DOLPHIN_STATE='[State]
1600x1000 screen: Height=1000
1600x1000 screen: Width=1600
RestorePositionForNextInstance=false
State=AAAA/wAAAAD9AAAAAwAAAAAAAAC0AAADuPwCAAAAAvsAAAAWAGYAbwBsAGQAZQByAHMARABvAGMAawAAAAAwAAADuAAAAFMA////+wAAABQAcABsAGEAYwBlAHMARABvAGMAawAAAAH8AAAB7AAAAFMA////AAAAAQAAALQAAAO4/AIAAAAB+wAAABAAaQBuAGYAbwBEAG8AYwBrAAAAADAAAAO4AAABDAD///8AAAADAAAGQAAAALT8AQAAAAH7AAAAGAB0AGUAcgBtAGkAbgBhAGwARABvAGMAawAAAAAAAAAGQAAAAAAA////AAAGQAAAA7gAAAAEAAAABAAAAAgAAAAI/AAAAAEAAAACAAAAAQAAABYAbQBhAGkAbgBUAG8AbwBsAEIAYQByAQAAAAD/////AAAAAAAAAAA=
'

filemanager() {
	rm -rf "$OUT/filemanager"
	mkdir -p "$OUT/filemanager"
	fresh fm
	local folder=$WORK/fm/samples name id how
	mkdir -p "$folder"
	while IFS='|' read -r name id how; do
		if [ "$how" = convert ]; then
			lvp fm convert "$A/$(sample "$id" path)" "$folder/$name" >/dev/null
		else
			cp "$A/$(sample "$id" path)" "$folder/$name"
		fi
	done < <(rows "$FOLDER")
	stop_helpers
	# KIO's default preview plugins (every installed one but textthumbnail), without Slerp.
	local plugins
	plugins=$( { find "$(qtpaths6 --query QT_INSTALL_PLUGINS)/kf6/thumbcreator" -name '*.so' -printf '%f\n' | sed 's/\.so$//'
		find /usr/share/thumbnailers -name '*.thumbnailer' -printf '%f\n' | sed 's/\.thumbnailer$//'; } \
		| grep -v -x -e textthumbnail -e slerp | sort | paste -sd,)
	heavy "$SELF" _dolphin before "$plugins" 30
	heavy "$SELF" _dolphin after "$plugins" 30
	magick "$WORK/fm/before.png" -quality 90 -define webp:method=6 "$OUT/filemanager/before.webp"
	magick "$WORK/fm/after.png" -quality 90 -define webp:method=6 "$OUT/filemanager/after.webp"
}

# One Dolphin screenshot, run in its own scope by filemanager: $1 before or
# after, $2 the preview plugin list, $3 seconds to wait for previews.
_dolphin() {
	local which=$1 plugins=$2 wait=$3
	local R=$WORK/fm/run-$which H=$WORK/fm/home-$which
	rm -rf "$R" "$H"
	mkdir -p "$R" "$H/.config" "$H/.local/share/dolphin/view_properties/global" "$H/.local/state" "$H/.cache"
	chmod 700 "$R"
	cp -r "$WORK/fm/samples" "$H/Samples"
	printf '%s' "$DOLPHIN_STATE" >"$H/.local/state/dolphinstaterc"
	printf '[General]\nColorScheme=BreezeLight\n\n[Icons]\nTheme=breeze\n\n[KDE]\nwidgetStyle=Breeze\n' >"$H/.config/kdeglobals"
	cat >"$H/.config/dolphinrc" <<-EOF
		[General]
		GlobalViewProps=true
		ShowFullPath=false
		ShowStatusBar=Small
		Version=202

		[IconsMode]
		IconSize=192
		PreviewSize=192

		[MainWindow]
		MenuBar=Disabled

		[PreviewSettings]
		Plugins=$plugins
	EOF
	printf '[Dolphin]\nPreviewsShown=true\nSortRole=text\nVersion=4\nViewMode=0\n' \
		>"$H/.local/share/dolphin/view_properties/global/.directory"
	export HOME=$H XDG_CONFIG_HOME=$H/.config XDG_DATA_HOME=$H/.local/share XDG_STATE_HOME=$H/.local/state \
		XDG_CACHE_HOME=$H/.cache XDG_RUNTIME_DIR=$R
	unset DISPLAY
	[ "$which" = after ] && "$SLERP" associate --apply >/dev/null
	# A private D-Bus, so Dolphin never registers on the user's session bus.
	gui-sandbox start --floating -- dbus-run-session -- env QT_QPA_PLATFORM=wayland QT_QPA_PLATFORMTHEME=kde \
		XDG_CURRENT_DESKTOP=KDE KDE_SESSION_VERSION=6 KDE_FULL_SESSION=true SLERP_GPU=integrated \
		dolphin --new-window "$H/Samples" >/dev/null
	# shellcheck disable=SC1091
	. "$R/gui-sandbox.state"
	swaymsg -s "$SWAYSOCK" output HEADLESS-1 mode 1600x1000 >/dev/null
	sleep 4
	swaymsg -s "$SWAYSOCK" '[app_id=".*"] fullscreen enable' >/dev/null
	# The pointer starts over the first file, whose hover frame would show.
	gui-sandbox move 1279 719
	sleep "$wait"
	gui-sandbox shot "$WORK/fm/$which.png"
	# 1. Stop this sandbox only: its pointer helper, its compositor's process
	#    group and the thumbnail helper Dolphin started.  (gui-sandbox stop
	#    would kill every agent's pointer helper.)
	local p
	for p in $(pgrep -x gsbox-vptr || true); do
		tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -q "^XDG_RUNTIME_DIR=$R\$" && kill "$p" || true
	done
	kill -- "-$PID" 2>/dev/null || true
	sleep 1
	stop_helpers
}

# 4. Video loops, 1280x800, seamless: a turntable and a dance.
loops() {
	rm -rf "$OUT/loops"
	mkdir -p "$OUT/loops"
	rm -rf "$SAMPLES/loops"
	copy_into "$SAMPLES/loops/ship" "$(dirname "$(sample ship2k path)")"
	copy_into "$SAMPLES/loops" "$(sample venice4k path)" "$(sample studio path)" "$(sample robot path)"
	fresh loops
	local f=$WORK/loops i
	# 1. The ship turns once in 8 seconds; the lighting turns with the camera,
	#    so the ship seems to turn under fixed light, as on a turntable.  The
	#    key light is off: with this low sun it draws a long straight-edged
	#    wedge across the ground.
	mkdir -p "$f/turntable"
	for ((i = 0; i < 240; i++)); do
		printf '{"orbit": {"yaw": %s, "pitch": 8, "fov": 30, "distance": 125}, "environment": "%s", "background": "environment:1", "environmentRotation": %s, "keyLight": 0}\n' \
			"$(echo "-38 - 1.5 * $i" | bc)" "$SAMPLES/loops/venice_sunset_4k.hdr" "$(echo "150 - 1.5 * $i" | bc)" \
			>"$(printf '%s/turntable/%04d.jsonc' "$f" "$i")"
	done
	heavy "$SELF" _frames "$SAMPLES/loops/ship/$(basename "$(sample ship2k path)")" "$f/turntable"
	# 2. The robot dances its 10/3-second Dance clip twice, 100 frames each.
	#    An explicit target keeps the camera still; without one the camera
	#    frames each pose's bounds and the picture jumps.  The clip's root
	#    motion moves the robot about the frame, as the dance does.
	mkdir -p "$f/dance"
	for ((i = 0; i < 200; i++)); do
		printf '{"clip": "Dance", "time": %s, "orbit": {"yaw": 20, "pitch": 6, "fov": 30, "distance": 21, "target": [0, 2.4, 0]}, "environment": "%s", "background": "environment:1"}\n' \
			"$(printf '%.5f' "$(echo "scale=6; $((i % 100)) / 30" | bc)")" "$SAMPLES/loops/studio_small_09_1k.hdr" \
			>"$(printf '%s/dance/%04d.jsonc' "$f" "$i")"
	done
	heavy "$SELF" _frames "$SAMPLES/loops/RobotExpressive.glb" "$f/dance"
	local name
	for name in turntable dance; do
		heavy ffmpeg -loglevel error -y -framerate 30 -i "$f/$name/%04d.png" -vf crop=1280:800 -c:v libvpx-vp9 -b:v 0 \
			-crf 36 -row-mt 1 -cpu-used 2 -pix_fmt yuv420p -an "$OUT/loops/$name.webm"
		heavy ffmpeg -loglevel error -y -framerate 30 -i "$f/$name/%04d.png" -vf crop=1280:800 -c:v libx264 -preset slow \
			-crf 25 -pix_fmt yuv420p -movflags +faststart -an "$OUT/loops/$name.mp4"
		magick "$f/$name/0000.png" -gravity center -crop 1280x800+0+0 +repage -quality 86 "$OUT/loops/$name-poster.webp"
	done
	rm -rf "$SAMPLES"
}

# Renders each .jsonc view state in folder $2 of file $1 to a 1280 px PNG
# beside it; run in its own scope by loops.
_frames() {
	local file=$1 dir=$2 s
	fresh loops-run
	for s in "$dir"/*.jsonc; do
		env VK_DRIVER_FILES=$LVP SLERP_GPU=software XDG_STATE_HOME="$WORK/loops-run/state" \
			XDG_CONFIG_HOME="$WORK/loops-run/config" XDG_RUNTIME_DIR="$WORK/loops-run/run" \
			"$SLERP" render --size 1280 --state "$s" "$file" "${s%.jsonc}.png"
	done
}

# 5. The link-preview card, 1200x630: the name, one line about it and a grid
#    of thumbnails, on a plain dark ground.
social() {
	local png=$WORK/thumbs/png n i=0 tiles=()
	[ -d "$png" ] || { echo "the social step needs the thumbs step first" >&2; exit 1; }
	mkdir -p "$OUT/social" "$WORK/social"
	for n in $SOCIAL_TILES; do
		magick -size 142x142 xc:none -fill '#24272d' -draw 'roundrectangle 0,0 141,141 16,16' \
			\( "$png/$n.png" -trim +repage -resize 120x120 \) -gravity center -composite "$WORK/social/$(printf '%02d' $i).png"
		i=$((i + 1))
	done
	magick montage "$WORK/social/"[0-9][0-9].png -tile 4x3 -geometry +7+7 -background none "$WORK/social/grid.png"
	local fonts=/usr/share/fonts/truetype/ibm-plex
	magick -size 1200x630 xc:'#16181c' \
		"$WORK/social/grid.png" -gravity northeast -geometry +44+84 -composite \
		-gravity northwest -font "$fonts/IBMPlexSans-Bold.ttf" -pointsize 112 -fill '#f4f2ee' -annotate +60+150 'Slerp' \
		\( -size 440x -background none -font "$fonts/IBMPlexSans-Regular.ttf" -pointsize 31 -fill '#c9c6bf' \
			-interline-spacing 6 caption:'Thumbnails and a fast viewer for 3D models, game files, textures and more than 200 other graphics formats.' \) \
		-geometry +62+300 -composite \
		-font "$fonts/IBMPlexSans-Medium.ttf" -pointsize 22 -fill '#8d8a84' -annotate +62+530 'Open source, for Linux, Windows and macOS' \
		-depth 8 "$OUT/social/og.png"
}

# 6. images.json and CREDITS.md, from the tables above and assets.jsonc.
manifest() {
	local m=$WORK/manifest
	mkdir -p "$m"
	rows "$CATALOGUE" >"$m/catalogue.tsv"
	local name id how title alt copy open panels state input ids folder_ids rest p w h kind
	folder_ids=$(rows "$FOLDER" | cut -d'|' -f2 | sort -u | paste -sd' ')
	# path | kind | width | height | seconds | sample file | samples | title | alt
	{
		while IFS='|' read -r name id how title alt; do
			p=thumbs/$name.webp
			echo "$p|thumbnail|$(identify -format '%w|%h' "$OUT/$p")||$(basename "$(sample "$id" path)")|$id|$title|$alt"
		done < <(rows "$THUMBS")
		while IFS='|' read -r name copy open panels state input ids title alt; do
			p=screens/$name.webp
			echo "$p|viewer screenshot|$(identify -format '%w|%h' "$OUT/$p")||$open|$ids|$title|$alt"
		done < <(rows "$SCREENS")
		while IFS='|' read -r p kind ids title alt; do
			[ "$ids" = FOLDER ] && ids=$folder_ids
			case $p in
				*.webm|*.mp4)
					w=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s='|' "$OUT/$p")
					echo "$p|$kind|$w|$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$OUT/$p")||$ids|$title|$alt" ;;
				*) echo "$p|$kind|$(identify -format '%w|%h' "$OUT/$p")|||$ids|$title|$alt" ;;
			esac
		done < <(rows "$OTHERS")
	} >"$m/pictures.tsv"
	cat >"$m/manifest.ts" <<-'EOF'
		// Writes images.json and CREDITS.md from the tables of tools/site-images.sh.
		import {parse} from 'jsr:@std/jsonc@1'
		const [catalogueFile, picturesFile, assetsFile, out] = Deno.args
		const licences:Record<string, [string, string]> = {
			'CC0-1.0': ['CC0 1.0', 'https://creativecommons.org/publicdomain/zero/1.0/'],
			'CC-BY-4.0': ['CC BY 4.0', 'https://creativecommons.org/licenses/by/4.0/'],
			'CC-BY-3.0': ['CC BY 3.0', 'https://creativecommons.org/licenses/by/3.0/'],
			'MIT': ['MIT', 'https://opensource.org/license/mit'],
			'MIT OR Apache-2.0': ['MIT or Apache 2.0', 'https://opensource.org/license/mit'],
			'BSD-2-Clause': ['BSD 2-Clause', 'https://opensource.org/license/bsd-2-clause'],
			'BSD-3-Clause': ['BSD 3-Clause', 'https://opensource.org/license/bsd-3-clause'],
			'Apache-2.0': ['Apache 2.0', 'https://www.apache.org/licenses/LICENSE-2.0'],
			'public-domain': ['public domain (a NASA work)', 'https://www.nasa.gov/nasa-brand-center/images-and-media/'],
		}
		const entries = new Map<string, any>()
		for (const e of (parse(await Deno.readTextFile(assetsFile)) as any).entries) entries.set(`${e.category}/${e.name}`, e)
		const lines = (f:string) => Deno.readTextFileSync(f).split('\n').filter(l => l.trim())
		const samples = new Map<string, any>()
		for (const l of lines(catalogueFile)) {
			const [id, path, format, category, entry, author, licence, source] = l.split('|')
			const e = entries.get(entry)
			if (!e) throw new Error(`${id}: no assets.jsonc entry ${entry}`)
			if (e.redistribute !== true) throw new Error(`${id}: ${entry} may not be redistributed`)
			if (!licences[licence]) throw new Error(`${id}: unknown licence ${licence}`)
			const own = e.git?.repo ?? (e.polyhaven ? `https://polyhaven.com/a/${e.polyhaven.id}` : e.url)
			samples.set(id, {id, file: path.split('/').pop(), format, category, source: source === '-' ? own : source,
				author, licence: licences[licence][0], licenceId: licence, licenceUrl: licences[licence][1], entry})
		}
		const usedBy = new Map<string, string[]>()
		const pictures = lines(picturesFile).map(l => {
			const [path, kind, width, height, seconds, file, ids, title, alt] = l.split('|')
			const used = ids.split(' ').map(id => {
				const s = samples.get(id)
				if (!s) throw new Error(`${path}: no sample ${id}`)
				usedBy.set(id, [...(usedBy.get(id) ?? []), path])
				return s
			})
			// A picture of one subject (with perhaps its lighting) names it at the top
			// level; one of many files lists them only under samples.
			const one = used.length === 1 || ['viewer screenshot', 'video', 'poster'].includes(kind) ? used[0] : null
			const p:any = {path, kind, width: Number(width), height: Number(height)}
			if (seconds) p.seconds = Math.round(Number(seconds) * 100) / 100
			Object.assign(p, {title, alt})
			if (!one) p.credit = `Shows ${used.length} samples; credit each one listed under samples.`
			if (one) Object.assign(p, {sample: file || one.file, format: one.format, category: one.category,
				source: one.source, author: one.author, licence: one.licence, spdx: one.licenceId, licenceUrl: one.licenceUrl})
			p.samples = used.map(s => ({file: s.file, format: s.format, category: s.category, source: s.source,
				author: s.author, licence: s.licence, spdx: s.licenceId, licenceUrl: s.licenceUrl}))
			return p
		})
		const json = {about: 'Pictures for the Slerp website, made by tools/site-images.sh.  Each picture lists the samples it shows; credit them as CREDITS.md says.', pictures}
		await Deno.writeTextFile(`${out}/images.json`, JSON.stringify(json, null, '\t') + '\n')
		// CREDITS.md: one line per sample, those whose licence asks for credit first.
		const line = (s:any) => `- ${s.file}, ${s.format}.  By ${s.author}.  ${s.licence}: ${s.licenceUrl}.  Source: ${s.source}.  Shown in ${usedBy.get(s.id)!.join(', ')}.`
		const shown = [...samples.values()].filter(s => usedBy.has(s.id))
		const needs = shown.filter(s => s.licenceId !== 'CC0-1.0' && s.licenceId !== 'public-domain')
		const free = shown.filter(s => s.licenceId === 'CC0-1.0' || s.licenceId === 'public-domain')
		const md = [
			'# Picture credits',
			'',
			'The pictures in this folder show sample files made by other people, all under licences that allow showing them.  A page that shows a picture should credit the samples it shows, as listed here; images.json has the same details for each picture.  The Slerp program made every picture; its own code is MIT or Apache 2.0.',
			'',
			'## Credit required',
			'',
			'These licences (Creative Commons Attribution, MIT, BSD and Apache) ask that the author be named.',
			'',
			...needs.map(line),
			'',
			'## Credit as a courtesy',
			'',
			'These samples are CC0 or in the public domain, so no credit is required, but naming the authors is good manners.',
			'',
			...free.map(line),
			'',
		].join('\n')
		await Deno.writeTextFile(`${out}/CREDITS.md`, md)
		console.log(`${pictures.length} pictures, ${shown.length} samples`)
	EOF
	deno run --allow-read --allow-write --allow-net=jsr.io "$m/manifest.ts" "$m/catalogue.tsv" "$m/pictures.tsv" \
		"$ROOT/tools/assets.jsonc" "$OUT"
}

#endregion

# Internal steps that run in their own scope.
case ${1:-} in
	_dolphin) shift; _dolphin "$@"; exit ;;
	_frames) shift; _frames "$@"; exit ;;
esac

[ $# -gt 0 ] || set -- thumbs screens filemanager loops social manifest
mkdir -p "$WORK" "$OUT"
for step in "$@"; do
	case $step in
		thumbs|screens|filemanager|loops|social|manifest) echo "== $step"; "$step" ;;
		*) echo "unknown step: $step" >&2; exit 2 ;;
	esac
done
du -sh "$OUT"
