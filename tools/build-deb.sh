#!/bin/bash
# Builds Slerp's Debian packages into local/dist/:
#
#   slerp_<version>-<revision>_<arch>.deb            the program, its
#       thumbnailer file, MIME types, menu entry, icon and notices, from the
#       [package.metadata.deb] table in Cargo.toml, by cargo-deb
#   slerp-qt-plugin_<version>-<revision>_<arch>.deb  the Qt image plugin
#       (qt/), built with CMake, so only Qt desktops pull in Qt
#
# Usage: tools/build-deb.sh [--no-qt] [--no-build]
#   --no-qt     only the program's package
#   --no-build  package the release binary already built, without building
#
# Needs Rust, cargo-deb (`cargo install cargo-deb`), dpkg-dev and Slerp's
# build dependencies (README.md, "From source"); the plugin also needs
# cmake and qt6-base-dev.  Environment:
#   CARGO             the cargo command (default `cargo`, or tools/cargo-capped)
#   CARGO_DEB         the cargo-deb program (default `cargo-deb` on the PATH,
#                     else the copy in local/tools/cargo-deb)
#   CARGO_TARGET_DIR  where cargo builds (default local/target)
#   DIST              where the packages go (default local/dist)
#
# The packages describe the system they are built on: build on the oldest
# Debian or Ubuntu release they should install on.
set -euo pipefail
cd "$(dirname "$0")/.."
umask 022

CARGO=${CARGO:-cargo}
if [ -z "${CARGO_DEB:-}" ]; then
	CARGO_DEB=$(command -v cargo-deb || echo local/tools/cargo-deb/bin/cargo-deb)
fi
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-local/target}
DIST=${DIST:-local/dist}
QT=1
BUILD=1
for arg in "$@"; do
	case $arg in
	--no-qt) QT=0 ;;
	--no-build) BUILD=0 ;;
	*) echo "usage: tools/build-deb.sh [--no-qt] [--no-build]" >&2; exit 2 ;;
	esac
done

VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
REVISION=$(sed -n '1s/^slerp ([^-]*-\([^)]*\)).*/\1/p' packaging/debian/changelog)
MAINTAINER=$(sed -n 's/^maintainer = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
mkdir -p "$DIST"

# 1. The changelog's newest entry names the package version; a mismatch
#    would give apt and the changelog two different versions.
if ! head -1 packaging/debian/changelog | grep -q "^slerp ($VERSION-"; then
	echo "packaging/debian/changelog: the newest entry must be for version $VERSION" >&2
	exit 1
fi

# 2. The notices must list the crates this build links.
if command -v deno >/dev/null; then
	deno run -A tools/third-party.ts --check 2>/dev/null \
		|| { echo "THIRD-PARTY.md is stale: run deno run -A tools/third-party.ts" >&2; exit 1; }
else
	echo "warning: deno not found, so THIRD-PARTY.md was not checked against Cargo.lock" >&2
fi

# 3. The program, then its package.  Panic messages and C++ assertions name
#    source files, so the paths of the builder's home and source folder are
#    rewritten.  cargo-deb reads the asset list from Cargo.toml, works out
#    the shared-library dependencies with dpkg-shlibdeps, and strips what
#    the release profile left (the .comment and .note sections).
REMAP="${CARGO_HOME:-$HOME/.cargo}=/cargo"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$REMAP --remap-path-prefix=$PWD=/slerp"
export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$REMAP" CXXFLAGS="${CXXFLAGS:-} -ffile-prefix-map=$REMAP"
[ "$BUILD" = 0 ] || $CARGO build --release --locked -p slerp
"$CARGO_DEB" --no-build --strip -p slerp --deb-revision "$REVISION" --output "$DIST/slerp_${VERSION}-${REVISION}_${ARCH}.deb"

[ "$QT" = 1 ] || exit 0

# 4. The Qt plugin, built to start the helper from /usr/bin/slerp.
QT_BUILD="$CARGO_TARGET_DIR/qt-deb"
cmake -S qt -B "$QT_BUILD" -DCMAKE_BUILD_TYPE=Release -DSLERP_EXECUTABLE=/usr/bin/slerp >/dev/null
cmake --build "$QT_BUILD" --target slerp_qimage
PLUGINS=$(qtpaths6 --query QT_INSTALL_PLUGINS)

# 5. Its package, put together by hand: cargo-deb packages Cargo's own
#    builds.  dpkg-shlibdeps wants a debian/control beside it (empty will
#    do) and finds the package's root by its DEBIAN folder.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/slerp-qt-plugin"
mkdir -p -m 755 "$STAGE/DEBIAN" "$WORK/debian"
touch "$WORK/debian/control"
install -Dm644 "$QT_BUILD/plugins/imageformats/slerp_qimage.so" "$STAGE$PLUGINS/imageformats/slerp_qimage.so"
strip --strip-unneeded --remove-section=.comment --remove-section=.note "$STAGE$PLUGINS/imageformats/slerp_qimage.so"
DOC="$STAGE/usr/share/doc/slerp-qt-plugin"
install -Dm644 packaging/debian/qt-plugin/copyright "$DOC/copyright"
install -Dm644 packaging/debian/qt-plugin/slerp-qt-plugin.lintian-overrides \
	"$STAGE/usr/share/lintian/overrides/slerp-qt-plugin"
gzip -9n <packaging/debian/changelog >"$DOC/changelog.Debian.gz"
chmod 644 "$DOC/changelog.Debian.gz"
DEPENDS=$(cd "$WORK" && dpkg-shlibdeps -O -e"slerp-qt-plugin$PLUGINS/imageformats/slerp_qimage.so" |
	sed -n 's/^shlibs:Depends=//p')
sed -e "s|@VERSION@|$VERSION-$REVISION|" -e "s|@ARCH@|$ARCH|" -e "s|@MAINTAINER@|$MAINTAINER|" \
	-e "s|@SIZE@|$(du -sk "$STAGE/usr" | cut -f1)|" -e "s|@DEPENDS@|$DEPENDS|" \
	packaging/debian/qt-plugin/control.in >"$STAGE/DEBIAN/control"
(cd "$STAGE" && find usr -type f -print0 | sort -z | xargs -0 md5sum) >"$STAGE/DEBIAN/md5sums"
chmod 644 "$STAGE/DEBIAN/control" "$STAGE/DEBIAN/md5sums"
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$DIST/slerp-qt-plugin_${VERSION}-${REVISION}_${ARCH}.deb" >/dev/null
ls -l "$DIST"/slerp*_"${VERSION}-${REVISION}"_"$ARCH".deb
