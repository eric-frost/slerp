#!/bin/bash
# Makes the pictures README.md shows, in docs/images/: a sheet of thumbnails
# and viewer screenshots, all of freely licensed samples from the test corpus
# (tools/fetch-test-assets.ts; tools/assets.jsonc marks what may be
# redistributed, and the README credits what the pictures show).
#
# Usage: tools/readme-images.sh [SLERP]   (default local/target/release/slerp)
#
# The viewer runs in a private X server (Xvfb) on lavapipe, Mesa's CPU
# Vulkan driver, so no window appears on the desktop; it still takes its
# colours and fonts from the desktop's settings.  Needs Xvfb (xvfb-run),
# Mesa's Vulkan drivers and ImageMagick 7 with WebP.
set -euo pipefail
cd "$(dirname "$0")/.."
SLERP=$(realpath "${1:-local/target/release/slerp}")
A=local/assets
OUT=docs/images
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json SLERP_GPU=software
# A fresh state folder: the viewer's defaults (panels shown), no recent files.
export XDG_STATE_HOME="$WORK/state"

# 1. The thumbnail sheet: what a file manager shows of a mixed folder.
#    Each entry is the sample and the name shown under its thumbnail.
SHEET=(
	"$A/gltf/polyhaven/Lantern_01/Lantern_01_1k.gltf|Lantern_01.gltf"
	"$A/gltf/polyhaven/ArmChair_01/ArmChair_01_1k.gltf|ArmChair_01.gltf"
	"$A/vox/ephtracy/vox/character/chr_knight.vox|chr_knight.vox"
	"$A/splats/hornedlizard.spz|hornedlizard.spz"
	"$A/usd/usdwg-teapot/Teapot.usd|Teapot.usd"
	"$A/textures/ktx-software/ktx2/FlightHelmet_baseColor_blze.ktx2|FlightHelmet.ktx2"
	"$A/hdr/venice_sunset_1k.hdr|venice_sunset.hdr"
	"$A/materials/ambientcg/Bricks097/Bricks097_1K-JPG.mtlx|Bricks097.mtlx"
	"$A/quake/librequake/id1/maps/lqdm1.bsp|lqdm1.bsp"
	"$A/doom/freedoom/freedoom2.wad|freedoom2.wad"
	"$A/retro-extra/ansi/ansilove-bs-alove.ans|alove.ans"
	"$A/retro-extra/atlas/tpsheet-ld39/tileset-1.tpsheet|tileset-1.tpsheet"
	"$A/luts/Matplotlib_cyclic_twilight.cube|twilight.cube"
	"$A/stl/pr2_head_pan_binary.stl|pr2_head_pan.stl"
	"$A/paths-extra/gpxpy/Mojstrovka.gpx|Mojstrovka.gpx"
)
tiles=()
for entry in "${SHEET[@]}"; do
	file=${entry%%|*}
	name=${entry##*|}
	png="$WORK/$(printf '%02d' ${#tiles[@]}).png"
	"$SLERP" thumbnail 256 "$file" "$png"
	tiles+=(-label "$name" "$png")
done
montage -background '#eff0f1' -fill '#232629' -font DejaVu-Sans -pointsize 15 \
	-geometry 200x200+14+10 -tile 5x "${tiles[@]}" -quality 90 "$OUT/thumbnails.webp"

# 2. Viewer screenshots: open a file, let it settle, save the window.  The
#    samples are copied to /tmp/samples first, since the inspector shows the
#    folder a file is in.  $1 output name, $2 file, $3 scripted input before
#    the shot, $4 "bare" to start with the side panels hidden.
SAMPLES=/tmp/samples
rm -rf "$SAMPLES" && mkdir -p "$SAMPLES"
trap 'rm -rf "$WORK" "$SAMPLES"' EXIT
cp -r "$A/gltf/polyhaven/ArmChair_01" "$A/gltf/polyhaven/Lantern_01" "$A/doom/freedoom/freedoom2.wad" \
	"$A/textures/ktx-software/ktx2/FlightHelmet_baseColor_blze.ktx2" "$SAMPLES/"
shot() {
	local name=$1 file=$2 input=${3:-} bare=${4:-}
	mkdir -p "$XDG_STATE_HOME/slerp"
	if [ "$bare" = bare ]; then echo '{"hidePanels": true}'; else echo '{}'; fi >"$XDG_STATE_HOME/slerp/viewer.json"
	SLERP_NO_HANDOFF=1 SLERP_VIEWER_KEEP_ALIVE=0 SLERP_WINDOW_SIZE=1600x900 \
		SLERP_INPUT="wait:20 $input wait:40 shot:$WORK/$name.png ctrl+Q" SLERP_QUIT_AFTER=120 \
		xvfb-run -a -s "-screen 0 1700x1000x24" env -u WAYLAND_DISPLAY "$SLERP" view "$file" >/dev/null 2>&1 || true
	[ -s "$WORK/$name.png" ] || { echo "no screenshot of $file" >&2; exit 1; }
	magick "$WORK/$name.png" -resize 1200x -quality 86 "$OUT/$name.webp"
}
shot viewer-model "$SAMPLES/ArmChair_01/ArmChair_01_1k.gltf" "drag:-90,10" bare
shot viewer-level "$SAMPLES/freedoom2.wad" "" bare
shot viewer-contents "$SAMPLES/Lantern_01/Lantern_01_1k.gltf" "clickat:200,96 wait:10 drag:40,10"
shot viewer-texture "$SAMPLES/FlightHelmet_baseColor_blze.ktx2" "move:0.02,0.98"
ls -l "$OUT"
