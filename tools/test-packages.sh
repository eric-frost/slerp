#!/bin/bash
# Tests Slerp's Linux packages in fresh containers:
#
#   tools/test-packages.sh deb   installs local/dist/*.deb (tools/build-deb.sh)
#                                in Ubuntu, runs the smoke tests and lintian,
#                                and removes the packages again
#   tools/test-packages.sh aur   builds packaging/aur/PKGBUILD in Arch Linux
#                                from local/dist/slerp-<version>.tar.gz
#                                (tools/source-tarball.sh), writes .SRCINFO,
#                                installs, runs the smoke tests and namcap,
#                                and removes the packages
#
# Environment:
#   CONTAINER  the container command (default `docker`; any docker-compatible
#              one, such as `podman`, with its options)
#   RUN_OPTS   more options for `run`, such as a cgroup parent that caps the
#              build's memory
#   IMAGE      the image (default ubuntu:26.04 or archlinux:latest)
#   OUT        where thumbnails, logs, the Arch packages and .SRCINFO go
#              (default local/dist/test-<kind>)
#
# The smoke tests make their own small samples: a GIMP palette and an STL
# cube (drawn on lavapipe, Mesa's CPU Vulkan driver), plus a JPEG XR file
# from the source tree.
set -euo pipefail
cd "$(dirname "$0")/.."
KIND=${1:?usage: tools/test-packages.sh deb|aur}
read -r -a CONTAINER <<<"${CONTAINER:-docker}"
read -r -a RUN_OPTS <<<"${RUN_OPTS:-}"
VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
OUT=$(realpath -m "${OUT:-local/dist/test-$KIND}")
mkdir -p "$OUT"

# The checks every package must pass once installed.  $1 is the folder
# holding the JPEG XR sample.
SMOKE='
set -eu
data=$1
echo "== slerp --version"; slerp --version
echo "== slerp formats"; slerp formats >/tmp/formats.txt; head -3 /tmp/formats.txt; echo "($(wc -l </tmp/formats.txt) lines)"
printf "GIMP Palette\nName: Smoke test\nColumns: 4\n#\n255 0 0 Red\n0 255 0 Green\n0 0 255 Blue\n255 255 0 Yellow\n" >/tmp/smoke.gpl
{
	echo "solid cube"
	for f in "0 0 -1|0 0 0|1 1 0|1 0 0" "0 0 -1|0 0 0|0 1 0|1 1 0" "0 0 1|0 0 1|1 0 1|1 1 1" "0 0 1|0 0 1|1 1 1|0 1 1" \
		"0 -1 0|0 0 0|1 0 0|1 0 1" "0 -1 0|0 0 0|1 0 1|0 0 1" "0 1 0|0 1 0|0 1 1|1 1 1" "0 1 0|0 1 0|1 1 1|1 1 0" \
		"-1 0 0|0 0 0|0 0 1|0 1 1" "-1 0 0|0 0 0|0 1 1|0 1 0" "1 0 0|1 0 0|1 1 0|1 1 1" "1 0 0|1 0 0|1 1 1|1 0 1"; do
		IFS="|" read -r n a b c <<<"$f"
		printf "facet normal %s\n outer loop\n  vertex %s\n  vertex %s\n  vertex %s\n endloop\nendfacet\n" "$n" "$a" "$b" "$c"
	done
	echo "endsolid cube"
} >/tmp/smoke.stl
for f in /tmp/smoke.gpl /tmp/smoke.stl "$data/rgb24.jxr"; do
	name=$(basename "$f")
	echo "== slerp thumbnail 256 $name"
	start=$(date +%s%N)
	slerp thumbnail 256 "$f" "/out/$name.png"
	echo "$(( ($(date +%s%N) - start) / 1000000 )) ms, $(stat -c %s "/out/$name.png") bytes, $(head -c 8 "/out/$name.png" | od -An -tx1 | tr -d " ")"
done
echo "== slerp info smoke.stl"; slerp info /tmp/smoke.stl | head -8
echo "== MIME types and menu entry"
test -f /usr/share/mime/model/x-magicavoxel.xml || { echo "the MIME database lacks Slerp types"; exit 1; }
echo "update-mime-database ran: model/x-magicavoxel is known"
grep -q "slerp.desktop" /usr/share/applications/mimeinfo.cache || { echo "slerp.desktop is not in mimeinfo.cache"; exit 1; }
echo "update-desktop-database ran: slerp.desktop is in mimeinfo.cache"
plugin=$(find /usr/lib -name slerp_qimage.so | head -1)
echo "== Qt plugin: $plugin"
if ldd "$plugin" | grep "not found"; then exit 1; fi
echo "all its libraries resolve"
'

# What must be gone after removal.
GONE='
for f in /usr/bin/slerp /usr/share/thumbnailers/slerp.thumbnailer /usr/share/mime/packages/slerp.xml \
	/usr/share/applications/slerp.desktop /usr/share/mime/model/x-magicavoxel.xml; do
	if [ -e "$f" ]; then echo "still there after removal: $f"; exit 1; fi
done
echo "removed cleanly"
'

case $KIND in
deb)
	DEBS=(local/dist/slerp_"$VERSION"-*.deb local/dist/slerp-qt-plugin_"$VERSION"-*.deb)
	"${CONTAINER[@]}" run --rm "${RUN_OPTS[@]}" -v "$PWD/local/dist:/dist:ro" -v "$PWD/crates/jxr/tests/data/files:/data:ro" \
		-v "$OUT:/out" "${IMAGE:-docker.io/library/ubuntu:26.04}" bash -c "
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
echo '== apt-get install ${DEBS[*]##*/}'
apt-get install -y -qq --no-install-recommends ${DEBS[*]/#local\/dist//dist} mesa-vulkan-drivers desktop-file-utils lintian >/tmp/apt.log 2>&1 \
	|| { tail -30 /tmp/apt.log; exit 1; }
grep -E '^(Setting up|Processing triggers for) (slerp|shared-mime-info|desktop-file-utils|hicolor)' /tmp/apt.log || true
bash -c '$SMOKE' smoke /data
echo '== dpkg -L slerp'; dpkg -L slerp
echo '== dpkg -L slerp-qt-plugin'; dpkg -L slerp-qt-plugin
echo '== dpkg-deb --info'; dpkg-deb --info /dist/${DEBS[0]##*/} | sed -n '/Package:/,\$p'; dpkg-deb --info /dist/${DEBS[1]##*/} | sed -n '/Package:/,\$p'
echo '== lintian'; useradd -m lint; install -m644 ${DEBS[*]/#local\/dist//dist} /home/lint/
su lint -c 'cd && lintian --info --display-info *.deb' || true
echo '== apt-get remove'; apt-get remove -y -qq slerp-qt-plugin slerp >/dev/null 2>&1
bash -c '$GONE'
" 2>&1 | tee "$OUT/log.txt"
	;;
aur)
	TARBALL=local/dist/slerp-$VERSION.tar.gz
	[ -f "$TARBALL" ] || { echo "$TARBALL is missing: run tools/source-tarball.sh" >&2; exit 1; }
	"${CONTAINER[@]}" run --rm "${RUN_OPTS[@]}" -v "$PWD/packaging/aur:/aur:ro" -v "$PWD/$TARBALL:/src/slerp-$VERSION.tar.gz:ro" \
		-v "$PWD/crates/jxr/tests/data/files:/data:ro" -v "$OUT:/out" \
		"${IMAGE:-docker.io/library/archlinux:latest}" bash -c "
set -eu
pacman -Syu --noconfirm --needed -q base-devel namcap rust clang cmake qt6-base wayland libxkbcommon \
	vulkan-swrast desktop-file-utils >/tmp/pacman.log 2>&1 || { tail -30 /tmp/pacman.log; exit 1; }
useradd -m builder
install -d -o builder /build
install -o builder -m644 /aur/PKGBUILD /src/slerp-$VERSION.tar.gz /build/
cd /build
echo '== makepkg --printsrcinfo'; su builder -c 'makepkg --printsrcinfo' >/out/.SRCINFO; cat /out/.SRCINFO
echo '== makepkg'
start=\$(date +%s)
su builder -c 'makepkg --noconfirm 2>&1' >/out/makepkg.log || { tail -40 /out/makepkg.log; exit 1; }
echo \"built in \$(( \$(date +%s) - start )) s\"
ls -l /build/*.pkg.tar.zst
cp /build/*.pkg.tar.zst /out/
echo '== pacman -U'; pacman -U --noconfirm /build/*.pkg.tar.zst 2>&1 | grep -iE 'installing|error|warning|hook|Arming|Updating' || true
bash -c '$SMOKE' smoke /data
echo '== pacman -Ql'; pacman -Ql slerp slerp-qt-plugin
echo '== namcap PKGBUILD'; namcap /build/PKGBUILD || true
echo '== namcap packages'; for p in /build/*.pkg.tar.zst; do namcap \"\$p\" || true; done
echo '== pacman -R'; pacman -R --noconfirm slerp-qt-plugin slerp >/dev/null
bash -c '$GONE'
" 2>&1 | tee "$OUT/log.txt"
	;;
*)
	echo "usage: tools/test-packages.sh deb|aur" >&2
	exit 2
	;;
esac
