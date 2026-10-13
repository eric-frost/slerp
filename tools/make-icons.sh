#!/bin/bash
# Renders data/slerp.svg into the icons the Windows and macOS builds use,
# so that building needs no SVG renderer:
#
#   data/windows/slerp.ico         16 to 256 pixels: slerp.exe, slerpw.exe,
#                                  slerp_shell.dll and the installer
#                                  (tools/build-windows.sh, packaging/windows/)
#   packaging/macos/icon-1024.png  Slerp.app's icon (tools/macos-app.sh)
#
# Both are committed.  Run this again after changing the SVG.  Needs
# Inkscape and ImageMagick.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# 1. Windows: each size drawn from the SVG rather than scaled down from a
#    big picture, so small icons stay sharp.  ImageMagick stores them as
#    bitmaps; the 256-pixel one is added as PNG, as Windows' own icons do,
#    which keeps the file small.
pngs=()
for s in 16 20 24 32 40 48 64 96 128 256; do
  inkscape data/slerp.svg -w "$s" -h "$s" -o "$tmp/$s.png" >/dev/null 2>&1
  [ "$s" = 256 ] || pngs+=("$tmp/$s.png")
done
magick "${pngs[@]}" "$tmp/small.ico"
magick "$tmp/256.png" -strip -define png:exclude-chunks=date,time "$tmp/big.png"
python3 -I - "$tmp/small.ico" "$tmp/big.png" data/windows/slerp.ico <<'PY'
import struct, sys
small, big = open(sys.argv[1], "rb").read(), open(sys.argv[2], "rb").read()
count = struct.unpack("<H", small[4:6])[0]
entries = [small[6 + 16 * i:22 + 16 * i] for i in range(count)]
images = [small[struct.unpack("<I", e[12:16])[0]:][:struct.unpack("<I", e[8:12])[0]] for e in entries]
entries.append(struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(big), 0))
images.append(big)
offset = 6 + 16 * len(entries)
out = [struct.pack("<HHH", 0, 1, len(entries))]
for e, image in zip(entries, images):
    out.append(e[:12] + struct.pack("<I", offset))
    offset += len(image)
open(sys.argv[3], "wb").write(b"".join(out + images))
PY

# 2. macOS: one 1024-pixel picture, which macos-app.sh scales into the
#    .icns sizes with sips.  Metadata that changes with every run is left
#    out, so an unchanged SVG gives an unchanged file.
inkscape data/slerp.svg -w 1024 -h 1024 -o "$tmp/1024.png" >/dev/null 2>&1
magick "$tmp/1024.png" -strip -define png:exclude-chunks=date,time packaging/macos/icon-1024.png
ls -l data/windows/slerp.ico packaging/macos/icon-1024.png
