#!/bin/bash
# Builds Slerp.app on a Mac: the slerp binary as the viewer, with
# the Quick Look thumbnail extension inside, ad-hoc signed, then packed as
# a zip and as a disk image with a link to Applications.  Run it on the Mac
# itself, or from Linux through tools/build-macos.sh.
#
#   tools/macos-app.sh             # local/dist/Slerp.app, Slerp-macos-arm64.zip and .dmg
#   tools/macos-app.sh install     # the same, then install and register it
#   tools/macos-app.sh uninstall   # remove the installed app
#
# Needs Rust and Apple's Command Line Tools (clang, swiftc), not Xcode.
# Installing uses only the user's own ~/Applications, without admin
# rights.  SLERP_MAC_JOBS sets cargo's job count (2 by default, for the
# 8 GB Mac Mini).  The icon comes from packaging/macos/icon-1024.png
# (tools/make-icons.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
JOBS=${SLERP_MAC_JOBS:-2}
CARGO=$HOME/.cargo/bin/cargo
[ -x "$CARGO" ] || CARGO=cargo
APP=local/dist/Slerp.app
APPEX=Contents/PlugIns/SlerpThumbnails.appex
EXTENSION_ID=com.slerp3d.Slerp.Thumbnails
INSTALLED=$HOME/Applications/Slerp.app
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
mode=${1:-}
# The oldest macOS the app runs on (LSMinimumSystemVersion in the
# Info.plists), for rustc, the C and C++ the crates compile, and swiftc
# alike.  Left unset, the C compiler targets the SDK's own version.
export MACOSX_DEPLOYMENT_TARGET=11.0

# Forgets an app and its extension: LaunchServices (file types, Open
# With), PluginKit (the extension), the extension's running process and
# Quick Look's cached thumbnails.
forget() {
  pluginkit -r "$1/$APPEX" 2>/dev/null || true
  "$LSREGISTER" -u "$1" 2>/dev/null || true
}
reset_quick_look() {
  pkill -x SlerpThumbnails 2>/dev/null || true
  qlmanage -r >/dev/null 2>&1 || true
  qlmanage -r cache >/dev/null 2>&1 || true
}

if [ "$mode" = uninstall ]; then
  forget "$INSTALLED"
  forget "$APP"
  rm -rf "$INSTALLED"
  reset_quick_look
  echo "removed $INSTALLED"
  exit
fi

# Panic messages and C++ assertions name source files, so the paths of
# the builder's home and source folder are rewritten, as tools/build-linux.sh
# does.  /home on a Mac is a link, and cargo sees the resolved path, so
# both forms are rewritten.
src=$(pwd -P)
remap="${CARGO_HOME:-$HOME/.cargo}=/cargo"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$remap --remap-path-prefix=$PWD=/slerp --remap-path-prefix=$src=/slerp"
export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$remap -ffile-prefix-map=$PWD=/slerp -ffile-prefix-map=$src=/slerp"
export CXXFLAGS="${CXXFLAGS:-} -ffile-prefix-map=$remap -ffile-prefix-map=$PWD=/slerp -ffile-prefix-map=$src=/slerp"

# 1. The viewer and command line, then the static library the extension
#    links.  The library's link flags (system frameworks and libraries)
#    come from rustc, which prints them only when it builds the library,
#    so they are kept next to it.
mkdir -p local/dist
"$CARGO" build --release --locked --jobs "$JOBS"
libs=local/target/release/libslerp_macos.libs
[ -s "$libs" ] || "$CARGO" clean --release -p slerp-macos
"$CARGO" rustc --release --locked --jobs "$JOBS" -p slerp-macos --lib --crate-type staticlib \
  -- --print native-static-libs 2>&1 | tee local/dist/staticlib.log
flags=$(sed -n 's/.*native-static-libs: //p' local/dist/staticlib.log | tail -1)
[ -n "$flags" ] && echo "$flags" > "$libs"
rm -f local/dist/staticlib.log
# rustc repeats some flags (each "-framework X" pair counts as one), and
# swiftc links libSystem itself.
link_flags=$(sed 's/-framework /-framework:/g' "$libs" | tr ' ' '\n' | awk 'NF && $0 != "-lSystem" && !seen[$0]++' |
  sed 's/-framework:/-framework /' | paste -sd ' ' -)

# 2. The app: the slerp binary is the main executable, which opens the
#    viewer when started without a command, as Finder does.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ZIP=local/dist/Slerp-macos-arm64.zip
DMG=local/dist/Slerp-macos-arm64.dmg
rm -rf "$APP" "$ZIP" "$DMG"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp local/target/release/slerp "$APP/Contents/MacOS/Slerp"
sed "s/@VERSION@/$version/g" data/macos/Info.plist > "$APP/Contents/Info.plist"
# The licences and the bundled crates' notices, which their licences require.
cp LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md "$APP/Contents/Resources/"
set=local/dist/Slerp.iconset
rm -rf "$set"
mkdir "$set"
for s in 16 32 128 256 512; do
  sips -z $s $s packaging/macos/icon-1024.png --out "$set/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) packaging/macos/icon-1024.png --out "$set/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set" -o "$APP/Contents/Resources/Slerp.icns"
rm -rf "$set"

# 3. The Quick Look extension, as Xcode would make it: the Swift provider
#    and the Rust library linked into one executable whose entry point is
#    Foundation's NSExtensionMain, in an .appex bundle inside the app.
ext=$APP/$APPEX
mkdir -p "$ext/Contents/MacOS"
sed "s/@VERSION@/$version/g" data/macos/Thumbnails-Info.plist > "$ext/Contents/Info.plist"
# shellcheck disable=SC2086 # the link flags are separate words
swiftc -O -swift-version 5 -parse-as-library -module-name SlerpThumbnails \
  -target "$(uname -m)-apple-macos$MACOSX_DEPLOYMENT_TARGET" -application-extension \
  -import-objc-header crates/macos/include/slerp.h \
  crates/macos/swift/ThumbnailProvider.swift local/target/release/libslerp_macos.a \
  $link_flags -framework QuickLookThumbnailing \
  -Xlinker -e -Xlinker _NSExtensionMain -Xlinker -dead_strip \
  -o "$ext/Contents/MacOS/SlerpThumbnails"
strip -x "$ext/Contents/MacOS/SlerpThumbnails"
for file in "$APP/Contents/MacOS/Slerp" "$ext/Contents/MacOS/SlerpThumbnails"; do
  if grep -q -a -F -e "$PWD/crates/" -e "$src/crates/" -e "$src/src/" -e "${remap%=*}/registry/" "$file"; then
    echo "error: $file names a path in $src or ${remap%=*}" >&2
    exit 1
  fi
done

# 4. Ad-hoc signatures, inside out, since Apple Silicon runs only signed
#    code and the app's signature covers the extension's.  The extension
#    gets the App Sandbox entitlement Quick Look requires.
codesign --force --sign - --entitlements data/macos/Thumbnails.entitlements "$ext"
codesign --force --sign - "$APP"
codesign --verify --strict --deep "$APP"

# 5. The zip, which ditto unpacks with the signatures intact, and the disk
#    image: the app beside a link to Applications to drag it onto.
#    hdiutil sometimes finds its device busy on CI machines, so it gets
#    three tries.
ditto -c -k --keepParent "$APP" "$ZIP"
stage=$(mktemp -d)
ditto "$APP" "$stage/Slerp.app"
ln -s /Applications "$stage/Applications"
for try in 1 2 3; do
  hdiutil create -quiet -volname Slerp -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$DMG" && break
  [ "$try" = 3 ] && exit 1
  sleep 10
done
rm -rf "$stage"
ls -l local/dist

# 6. Installing: a copy in ~/Applications, registered with LaunchServices
#    for its file types and with PluginKit for the extension, which is
#    switched on.  LaunchServices registers any app it finds on disk, so
#    the build in local/dist is unregistered and removed (the zip keeps
#    it); otherwise Slerp appears twice in Open With.
if [ "$mode" = install ]; then
  forget "$INSTALLED"
  rm -rf "$INSTALLED"
  mkdir -p "$(dirname "$INSTALLED")"
  ditto "$APP" "$INSTALLED"
  forget "$APP"
  rm -rf "$APP"
  "$LSREGISTER" -f -R "$INSTALLED"
  pluginkit -a "$INSTALLED/$APPEX"
  pluginkit -e use -i "$EXTENSION_ID"
  reset_quick_look
  pluginkit -m -v -i "$EXTENSION_ID"
  # The user's choice of formats (the viewer's Settings > File Types, or
  # `slerp associate`), applied again for this version: default apps, and
  # the extension switched off when every format's thumbnails are.
  "$INSTALLED/Contents/MacOS/Slerp" associate --apply || true
fi
