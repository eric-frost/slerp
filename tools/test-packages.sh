#!/bin/bash
# Tests Slerp's Linux release files in fresh containers:
#
#   tools/test-packages.sh deb [IMAGE...]   installs the .deb files in each
#       image (default ubuntu:26.04) with apt, runs the smoke tests and
#       lintian, and removes the packages again
#   tools/test-packages.sh rpm [IMAGE...]   the same for the .rpm files with
#       dnf (default fedora:latest), with rpmlint
#   tools/test-packages.sh arch [IMAGE...]  the same for the Arch packages
#       with pacman (default archlinux:latest), with namcap
#   tools/test-packages.sh aur [IMAGE]      builds packaging/aur/PKGBUILD in
#       Arch Linux from slerp-<version>.tar.gz (tools/source-tarball.sh),
#       writes .SRCINFO and the packages to OUT, then tests them as `arch`
#   tools/test-packages.sh tar [IMAGE...]   unpacks
#       slerp-<version>-linux-<arch>.tar.gz in each image (default debian:12),
#       runs its install.sh into /usr/local, the smoke tests and
#       install.sh --uninstall
#
# The packages come from tools/build-linux.sh, for this machine's
# architecture.  Environment:
#   CONTAINER  the container command (default `docker`; any docker-compatible
#              one, such as `podman`, with its options)
#   RUN_OPTS   more options for `run`, such as a cgroup parent that caps the
#              build's memory
#   DIST       where the packages are (default local/dist)
#   OUT        where thumbnails, logs, .SRCINFO and built packages go
#              (default local/dist/test-<kind>), one folder per image
#   QT_CHECK   the slerp-qt-check program (qt/tests/check.cpp), which reads
#              a model through Qt's image reader as Gwenview does, to show
#              that Qt loads the plugin; without it only the plugin's
#              libraries are checked.  Build it on the oldest system tested.
#   LINT=0     skips lintian, rpmlint and namcap
#
# The smoke tests (tools/smoke-test.sh) make their own small samples: a GIMP
# palette and an STL cube (drawn on lavapipe, Mesa's CPU Vulkan driver),
# plus a JPEG XR file from the source tree; then the MIME and menu caches
# and the Qt plugin are checked.  Exit status is non-zero when any image
# fails.
set -euo pipefail
cd "$(dirname "$0")/.."
KIND=${1:?usage: tools/test-packages.sh deb|rpm|arch|aur|tar [IMAGE...]}
shift
IMAGES=("$@")
read -r -a CONTAINER <<<"${CONTAINER:-docker}"
read -r -a RUN_OPTS <<<"${RUN_OPTS:-}"
VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
REVISION=$(sed -n '1s/^slerp ([^-]*-\([^)]*\)).*/\1/p' packaging/debian/changelog)
MACHINE=$(uname -m)
case $MACHINE in x86_64) DEB_ARCH=amd64 ;; aarch64) DEB_ARCH=arm64 ;; *) DEB_ARCH=$MACHINE ;; esac
DIST=$(realpath "${DIST:-local/dist}")
OUT=$(realpath -m "${OUT:-local/dist/test-$KIND}")
LINT=${LINT:-1}
mkdir -p "$OUT"

# The checks every installed copy must pass.  $1 is the folder holding the
# JPEG XR sample, $2 the prefix Slerp was installed under.
SMOKE='
set -eu
data=$1
prefix=$2
bash /smoke-test.sh slerp /out "$data/rgb24.jxr"
echo "== slerp info smoke.stl"; slerp info /out/smoke.stl | head -8
echo "== MIME types and menu entry"
test -f "$prefix/share/mime/model/x-magicavoxel.xml" || { echo "the MIME database lacks Slerp types"; exit 1; }
echo "update-mime-database ran: model/x-magicavoxel is known"
grep -q "slerp.desktop" "$prefix/share/applications/mimeinfo.cache" || { echo "slerp.desktop is not in mimeinfo.cache"; exit 1; }
echo "update-desktop-database ran: slerp.desktop is in mimeinfo.cache"
plugin=$(find /usr/lib /usr/lib64 -name slerp_qimage.so 2>/dev/null | head -1)
echo "== Qt plugin: ${plugin:-missing}"
[ -n "$plugin" ] || exit 1
if ldd "$plugin" | grep "not found"; then exit 1; fi
echo "all its libraries resolve"
if [ -x /check/slerp-qt-check ]; then
	echo "== Qt reads smoke.stl through the plugin"
	QT_QPA_PLATFORM=offscreen /check/slerp-qt-check read --size 128 /out/smoke.stl | tee /tmp/qt.txt
	grep -q "^OK .*fmt=stl " /tmp/qt.txt || { echo "Qt did not read the model through the plugin"; exit 1; }
fi
'

# What must be gone after removal, under the prefix in $1.
GONE='
for f in "$1/bin/slerp" "$1/share/thumbnailers/slerp.thumbnailer" "$1/share/mime/packages/slerp.xml" \
	"$1/share/applications/slerp.desktop" "$1/share/mime/model/x-magicavoxel.xml"; do
	if [ -e "$f" ]; then echo "still there after removal: $f"; exit 1; fi
done
if [ -n "$(find /usr/lib /usr/lib64 -name slerp_qimage.so 2>/dev/null)" ]; then echo "the Qt plugin is still there"; exit 1; fi
echo "removed cleanly"
'

# The files to install, found here so that a missing one fails at once.
need() {
	[ -f "$DIST/$1" ] || { echo "$DIST/$1 is missing: run tools/build-linux.sh" >&2; exit 1; }
	echo "/dist/$1"
}

# The commands that install, inspect and remove the packages in an image.
case $KIND in
deb)
	FILES="$(need "slerp_${VERSION}-${REVISION}_${DEB_ARCH}.deb") $(need "slerp-qt-plugin_${VERSION}-${REVISION}_${DEB_ARCH}.deb")"
	DEFAULT=ubuntu:26.04
	INSTALL="export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends $FILES mesa-vulkan-drivers desktop-file-utils qt6-qpa-plugins >/tmp/apt.log 2>&1 \
	|| { tail -30 /tmp/apt.log; exit 1; }
grep -E '^(Setting up|Processing triggers for) (slerp|shared-mime-info|desktop-file-utils|hicolor)' /tmp/apt.log || true"
	INSPECT="echo '== dpkg -L slerp slerp-qt-plugin'; dpkg -L slerp slerp-qt-plugin
echo '== dpkg-deb --info'; for f in $FILES; do dpkg-deb --info \$f | sed -n '/Package:/,\$p'; done
if [ $LINT = 1 ]; then
	echo '== lintian'; apt-get install -y -qq --no-install-recommends lintian >/dev/null 2>&1
	useradd -m lint; install -m644 $FILES /home/lint/
	su lint -c 'cd && lintian --info --display-info *.deb' || true
fi"
	REMOVE="apt-get remove -y -qq slerp-qt-plugin slerp >/dev/null 2>&1"
	;;
rpm)
	FILES="$(need "slerp-${VERSION}-${REVISION}.${MACHINE}.rpm") $(need "slerp-qt-plugin-${VERSION}-${REVISION}.${MACHINE}.rpm")"
	DEFAULT=fedora:latest
	INSTALL="dnf install -y -q $FILES mesa-vulkan-drivers desktop-file-utils qt6-qtbase-gui >/tmp/dnf.log 2>&1 \
	|| { tail -30 /tmp/dnf.log; exit 1; }"
	INSPECT="echo '== rpm -ql slerp slerp-qt-plugin'; rpm -ql slerp slerp-qt-plugin
echo '== rpm -qR'; rpm -qR slerp slerp-qt-plugin
if [ $LINT = 1 ]; then
	echo '== rpmlint'; dnf install -y -q rpmlint >/dev/null 2>&1 && rpmlint $FILES || true
fi"
	REMOVE="dnf remove -y -q slerp-qt-plugin slerp >/dev/null"
	;;
arch | aur)
	DEFAULT=archlinux:latest
	if [ "$KIND" = aur ]; then
		need "slerp-$VERSION.tar.gz" >/dev/null
		FILES="/out/slerp-$VERSION-$REVISION-$MACHINE.pkg.tar.zst /out/slerp-qt-plugin-$VERSION-$REVISION-$MACHINE.pkg.tar.zst"
	else
		FILES="$(need "slerp-$VERSION-$REVISION-$MACHINE.pkg.tar.zst") $(need "slerp-qt-plugin-$VERSION-$REVISION-$MACHINE.pkg.tar.zst")"
	fi
	INSTALL="pacman -Syu --noconfirm --needed -q vulkan-swrast desktop-file-utils >/tmp/pacman.log 2>&1 \
	|| { tail -30 /tmp/pacman.log; exit 1; }
pacman -U --noconfirm $FILES 2>&1 | grep -iE 'installing|error|warning|hook|Arming|Updating' || true"
	INSPECT="echo '== pacman -Ql'; pacman -Ql slerp slerp-qt-plugin
if [ $LINT = 1 ]; then
	pacman -S --noconfirm --needed -q namcap >/dev/null 2>&1
	echo '== namcap packages'; for p in $FILES; do namcap \$p || true; done
fi"
	REMOVE="pacman -R --noconfirm slerp-qt-plugin slerp >/dev/null"
	;;
tar)
	FILES=$(need "slerp-$VERSION-linux-$MACHINE.tar.gz")
	DEFAULT=debian:12
	INSTALL="export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends libvulkan1 mesa-vulkan-drivers shared-mime-info desktop-file-utils \
	libqt6gui6 qt6-qpa-plugins >/tmp/apt.log 2>&1 || { tail -30 /tmp/apt.log; exit 1; }
mkdir /opt/slerp && tar -xzf $FILES -C /opt/slerp
echo '== ls'; ls /opt/slerp/*
echo '== install.sh'; /opt/slerp/*/install.sh"
	INSPECT=""
	REMOVE="echo '== install.sh --uninstall'; /opt/slerp/*/install.sh --uninstall"
	;;
*)
	echo "usage: tools/test-packages.sh deb|rpm|arch|aur|tar [IMAGE...]" >&2
	exit 2
	;;
esac
PREFIX=/usr
[ "$KIND" = tar ] && PREFIX=/usr/local

# The Arch packages built from the PKGBUILD, as the builder user, before
# they are installed.
BUILD=""
if [ "$KIND" = aur ]; then
	BUILD="pacman -Syu --noconfirm --needed -q base-devel rust clang cmake qt6-base wayland libxkbcommon >/tmp/pacman.log 2>&1 \
	|| { tail -30 /tmp/pacman.log; exit 1; }
useradd -m builder
install -d -o builder /build
install -o builder -m644 /aur/PKGBUILD /dist/slerp-$VERSION.tar.gz /build/
cd /build
echo '== makepkg --printsrcinfo'; su builder -c 'makepkg --printsrcinfo' >/out/.SRCINFO; cat /out/.SRCINFO
echo '== makepkg'
start=\$(date +%s)
su builder -c 'makepkg --noconfirm 2>&1' >/out/makepkg.log || { tail -40 /out/makepkg.log; exit 1; }
echo \"built in \$(( \$(date +%s) - start )) s\"
cp /build/*.pkg.tar.zst /out/
ls -l /out/*.pkg.tar.zst
cd /"
	[ ${#IMAGES[@]} -le 1 ] || { echo "aur builds in one image" >&2; exit 2; }
fi

[ ${#IMAGES[@]} -gt 0 ] || IMAGES=("$DEFAULT")
CHECK=()
if [ -n "${QT_CHECK:-}" ]; then
	CHECK=(-v "$(realpath "$QT_CHECK"):/check/slerp-qt-check:ro")
fi
failed=()
for image in "${IMAGES[@]}"; do
	# Short names are Docker Hub's official images, which podman needs
	# spelt out.
	[[ $image == */* ]] || image=docker.io/library/$image
	out=$OUT/$(basename "$image" | tr ':' '-')
	mkdir -p "$out"
	echo "######## $KIND in $image"
	if "${CONTAINER[@]}" run --rm "${RUN_OPTS[@]}" -v "$DIST:/dist:ro" -v "$PWD/packaging/aur:/aur:ro" \
		-v "$PWD/tools/smoke-test.sh:/smoke-test.sh:ro" -v "$PWD/crates/jxr/tests/data/files:/data:ro" -v "$out:/out" \
		"${CHECK[@]}" "$image" bash -c "
set -eu
$BUILD
$INSTALL
bash -c '$SMOKE' smoke /data $PREFIX
$INSPECT
echo '== remove'
$REMOVE
bash -c '$GONE' gone $PREFIX
" 2>&1 | tee "$out/log.txt"; then
		echo "######## $KIND in $image: passed"
	else
		echo "######## $KIND in $image: FAILED"
		failed+=("$image")
	fi
done
if [ ${#failed[@]} -gt 0 ]; then
	echo "failed: ${failed[*]}" >&2
	exit 1
fi
