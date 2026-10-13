#!/bin/bash
# Builds Slerp's Linux release files into local/dist/ from one build of the
# program and its Qt image plugin (qt/):
#
#   deb  slerp_<version>-<revision>_<arch>.deb  the program, its thumbnailer
#            file, MIME types, menu entry, icon and notices, from the
#            [package.metadata.deb] table in Cargo.toml, by cargo-deb;
#        slerp-qt-plugin_<version>-<revision>_<arch>.deb  the Qt plugin,
#            kept apart so that only Qt desktops pull in Qt
#   rpm  slerp-<version>-<revision>.<arch>.rpm and
#        slerp-qt-plugin-<version>-<revision>.<arch>.rpm  the same two with
#            Fedora's paths, by nfpm from packaging/rpm/
#   tar  slerp-<version>-linux-<arch>.tar.gz  the program, the data files
#            and the plugin with an install script (packaging/linux/), for
#            other distributions
#
# Usage: tools/build-linux.sh [--no-qt] [--no-build] [deb] [rpm] [tar]
#   deb rpm tar  the files to make; all three when none is named
#   --no-qt      leave out the Qt plugin
#   --no-build   package what an earlier run built, without building
#
# Needs Rust and Slerp's build dependencies (README.md, "From source"); the
# plugin also needs cmake and qt6-base-dev; debs need cargo-deb
# (`cargo install cargo-deb`) and dpkg-dev, rpms nfpm
# (https://nfpm.goreleaser.com).  Environment:
#   CARGO             the cargo command (default `cargo`, or tools/cargo-capped)
#   CARGO_DEB         the cargo-deb program (default `cargo-deb` on the PATH,
#                     else local/tools/cargo-deb/bin/cargo-deb)
#   NFPM              the nfpm program (default `nfpm` on the PATH, else
#                     local/tools/nfpm/nfpm)
#   CARGO_TARGET_DIR  where cargo builds (default local/target)
#   DIST              where the files go (default local/dist)
#
# The program and the plugin need the C and C++ libraries of the system
# they are built on, or newer: build on the oldest release they should run
# on.  The release workflow builds on Ubuntu 22.04 (glibc 2.35, Qt 6.2).
set -euo pipefail
cd "$(dirname "$0")/.."
umask 022

CARGO=${CARGO:-cargo}
CARGO_DEB=${CARGO_DEB:-$(command -v cargo-deb || echo local/tools/cargo-deb/bin/cargo-deb)}
NFPM=${NFPM:-$(command -v nfpm || echo local/tools/nfpm/nfpm)}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-local/target}
DIST=${DIST:-local/dist}
QT=1
BUILD=1
KINDS=()
for arg in "$@"; do
	case $arg in
	--no-qt) QT=0 ;;
	--no-build) BUILD=0 ;;
	deb | rpm | tar) KINDS+=("$arg") ;;
	*) echo "usage: tools/build-linux.sh [--no-qt] [--no-build] [deb] [rpm] [tar]" >&2; exit 2 ;;
	esac
done
[ ${#KINDS[@]} -gt 0 ] || KINDS=(deb rpm tar)
wants() { [[ " ${KINDS[*]} " == *" $1 "* ]]; }

VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
REVISION=$(sed -n '1s/^slerp ([^-]*-\([^)]*\)).*/\1/p' packaging/debian/changelog)
MAINTAINER=$(sed -n 's/^maintainer = "\(.*\)"/\1/p' Cargo.toml | head -1)
MACHINE=$(uname -m)
BINARY=$CARGO_TARGET_DIR/release/slerp
QT_BUILD=$CARGO_TARGET_DIR/qt-release
PLUGIN=$QT_BUILD/plugins/imageformats/slerp_qimage.so
mkdir -p "$DIST"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

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

# 3. The program.  Panic messages and C++ assertions name source files, so
#    the paths of the builder's home and source folder are rewritten.
if [ "$BUILD" = 1 ]; then
	SRC=$(pwd -P)
	REMAP="${CARGO_HOME:-$HOME/.cargo}=/cargo"
	export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$REMAP --remap-path-prefix=$SRC=/slerp"
	export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$REMAP -ffile-prefix-map=$SRC=/slerp"
	export CXXFLAGS="${CXXFLAGS:-} -ffile-prefix-map=$REMAP -ffile-prefix-map=$SRC=/slerp"
	$CARGO build --release --locked -p slerp
	if grep -q -a -F -e "$SRC/crates/" -e "$SRC/src/" -e "${REMAP%=*}/registry/" "$BINARY"; then
		echo "error: $BINARY names a path in $SRC or ${REMAP%=*}" >&2
		exit 1
	fi
fi

# 4. The Qt plugin, built to start the helper from /usr/bin/slerp (it falls
#    back to `slerp` on the PATH).  It is stripped as cargo-deb strips the
#    program: Qt reads its metadata from a section of its own.
if [ "$QT" = 1 ] && [ "$BUILD" = 1 ]; then
	cmake -S qt -B "$QT_BUILD" -DCMAKE_BUILD_TYPE=Release -DSLERP_EXECUTABLE=/usr/bin/slerp >/dev/null
	cmake --build "$QT_BUILD" --target slerp_qimage
	strip --strip-unneeded --remove-section=.comment --remove-section=.note "$PLUGIN"
fi
[ "$QT" = 0 ] || [ -f "$PLUGIN" ] || { echo "$PLUGIN is missing: build without --no-build" >&2; exit 1; }

# 5. The Debian packages.  cargo-deb reads the asset list from Cargo.toml,
#    works out the shared-library dependencies with dpkg-shlibdeps, and
#    strips what the release profile left (the .comment and .note
#    sections).  The plugin's package is put together by hand, since
#    cargo-deb packages Cargo's own builds: dpkg-shlibdeps wants a
#    debian/control beside it (empty will do) and finds the package's root
#    by its DEBIAN folder.
if wants deb; then
	ARCH=$(dpkg --print-architecture)
	"$CARGO_DEB" --no-build --strip -p slerp --deb-revision "$REVISION" \
		--output "$DIST/slerp_${VERSION}-${REVISION}_${ARCH}.deb"
	if [ "$QT" = 1 ]; then
		PLUGINS=$(qtpaths6 --query QT_INSTALL_PLUGINS 2>/dev/null || /usr/lib/qt6/bin/qtpaths --query QT_INSTALL_PLUGINS)
		STAGE="$WORK/deb/slerp-qt-plugin"
		mkdir -p -m 755 "$STAGE/DEBIAN" "$WORK/deb/debian"
		touch "$WORK/deb/debian/control"
		install -Dm644 "$PLUGIN" "$STAGE$PLUGINS/imageformats/slerp_qimage.so"
		DOC="$STAGE/usr/share/doc/slerp-qt-plugin"
		install -Dm644 packaging/debian/qt-plugin/copyright "$DOC/copyright"
		install -Dm644 packaging/debian/qt-plugin/slerp-qt-plugin.lintian-overrides \
			"$STAGE/usr/share/lintian/overrides/slerp-qt-plugin"
		gzip -9n <packaging/debian/changelog >"$DOC/changelog.Debian.gz"
		chmod 644 "$DOC/changelog.Debian.gz"
		DEPENDS=$(cd "$WORK/deb" && dpkg-shlibdeps -O -e"slerp-qt-plugin$PLUGINS/imageformats/slerp_qimage.so" |
			sed -n 's/^shlibs:Depends=//p')
		sed -e "s|@VERSION@|$VERSION-$REVISION|" -e "s|@ARCH@|$ARCH|" -e "s|@MAINTAINER@|$MAINTAINER|" \
			-e "s|@SIZE@|$(du -sk "$STAGE/usr" | cut -f1)|" -e "s|@DEPENDS@|$DEPENDS|" \
			packaging/debian/qt-plugin/control.in >"$STAGE/DEBIAN/control"
		(cd "$STAGE" && find usr -type f -print0 | sort -z | xargs -0 md5sum) >"$STAGE/DEBIAN/md5sums"
		chmod 644 "$STAGE/DEBIAN/control" "$STAGE/DEBIAN/md5sums"
		dpkg-deb --root-owner-group -Zxz --build "$STAGE" \
			"$DIST/slerp-qt-plugin_${VERSION}-${REVISION}_${ARCH}.deb" >/dev/null
	fi
	ls -l "$DIST"/slerp*_"${VERSION}-${REVISION}_${ARCH}".deb
fi

# 6. The rpms, from packaging/rpm/*.yaml with the paths and versions filled
#    in from the environment.  The program is stripped as in the deb.
if wants rpm; then
	install -Dm755 "$BINARY" "$WORK/rpm/slerp"
	strip --strip-unneeded --remove-section=.comment --remove-section=.note "$WORK/rpm/slerp"
	gzip -9n <packaging/slerp.1 >"$WORK/rpm/slerp.1.gz"
	export SLERP_VERSION=$VERSION SLERP_RELEASE=$REVISION SLERP_ARCH=$MACHINE SLERP_MAINTAINER=$MAINTAINER
	export SLERP_BINARY=$WORK/rpm/slerp SLERP_MAN=$WORK/rpm/slerp.1.gz SLERP_PLUGIN=$PLUGIN
	"$NFPM" package --config packaging/rpm/slerp.yaml --packager rpm --target "$DIST/"
	[ "$QT" = 0 ] || "$NFPM" package --config packaging/rpm/slerp-qt-plugin.yaml --packager rpm --target "$DIST/"
	ls -l "$DIST"/slerp*-"$VERSION-$REVISION.$MACHINE.rpm"
fi

# 7. The tarball: a folder laid out like /usr/local, the plugin and
#    packaging/linux/install.sh, which copies them into place.
if wants tar; then
	NAME=slerp-$VERSION-linux-$MACHINE
	T=$WORK/tar/$NAME
	install -Dm755 "$BINARY" "$T/bin/slerp"
	strip --strip-unneeded --remove-section=.comment --remove-section=.note "$T/bin/slerp"
	install -Dm644 data/slerp.thumbnailer -t "$T/share/thumbnailers"
	install -Dm644 data/slerp.xml -t "$T/share/mime/packages"
	install -Dm644 data/slerp.desktop -t "$T/share/applications"
	install -Dm644 data/slerp.svg -t "$T/share/icons/hicolor/scalable/apps"
	install -Dm644 packaging/slerp.1 -t "$T/share/man/man1"
	install -Dm644 README.md THIRD-PARTY.md LICENSE-MIT LICENSE-APACHE -t "$T/share/doc/slerp"
	[ "$QT" = 0 ] || install -Dm644 "$PLUGIN" "$T/qt-plugin/slerp_qimage.so"
	install -Dm755 packaging/linux/install.sh "$T/install.sh"
	tar --create --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$(git log -1 --format=%ct 2>/dev/null || date +%s)" \
		-C "$WORK/tar" "$NAME" | gzip -9n >"$DIST/$NAME.tar.gz"
	ls -l "$DIST/$NAME.tar.gz"
fi
