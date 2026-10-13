#!/bin/bash
# Cross-compiles Slerp for 64-bit Windows on Linux and packs it as a zip:
# slerp.exe, Explorer's thumbnail handler slerp_shell.dll, the
# console-less viewer launcher slerpw.exe, a short README.txt, and the
# licences with THIRD-PARTY.md, which the bundled crates' licences require.
# The three programs carry Slerp's icon and version information
# (data/windows/slerp.rc).  The installer is made from the zip's files on
# Windows, with packaging/windows/slerp.iss.
#
#   tools/build-windows.sh                 # release build, then local/dist/Slerp-windows-x64.zip
#   tools/build-windows.sh cargo check     # any cargo command, cross-compiling
#
# Needs a rustup toolchain with the x86_64-pc-windows-gnu standard library
# (Ubuntu's own Rust ships none, so rustup's proxies in ~/.cargo/bin are used
# without being on PATH), apt's mingw-w64 posix-threads compilers and
# binutils (for windres), libclang for bindgen, and zip.  Builds run inside
# heavy-slerp.slice through tools/cargo-capped, in the shared target
# directory local/target (or CARGO_TARGET_DIR).
set -euo pipefail
cd "$(dirname "$0")/.."

TOOLCHAIN=${SLERP_WINDOWS_TOOLCHAIN:-1.93.1}
TARGET=x86_64-pc-windows-gnu
MINGW=x86_64-w64-mingw32
GCC_VERSION=$(basename "$(ls -d /usr/lib/gcc/$MINGW/*-posix | sort -V | tail -1)")

# 1. C and C++ dependencies (Basis Universal, meshoptimizer, ufbx, libwebp,
#    zstd) compile with MinGW's posix-threads compilers, whose libstdc++
#    supports std::thread.
export CC_x86_64_pc_windows_gnu=$MINGW-gcc-posix
export CXX_x86_64_pc_windows_gnu=$MINGW-g++-posix
export AR_x86_64_pc_windows_gnu=$MINGW-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$MINGW-gcc-posix

# 2. bindgen parses Basis Universal's C header as MinGW would.
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu="--target=$MINGW -I/usr/$MINGW/include \
-I/usr/lib/gcc/$MINGW/$GCC_VERSION/include"

# 3. Link the MinGW runtime (libgcc, libstdc++, winpthread) statically, so
#    slerp.exe needs no DLLs beside it.  crt-static covers libgcc and
#    winpthread.  The cc crate asks for libstdc++ as a dynamic library,
#    which rustc wraps in -Bdynamic whatever the driver is told, so it is
#    asked for as a static one (not bundled into each C++ crate's rlib)
#    from MinGW's own library folder.
export CXXSTDLIB_x86_64_pc_windows_gnu=static:-bundle=stdc++
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static \
-L native=/usr/lib/gcc/$MINGW/$GCC_VERSION"

cargo() {
  PATH="$HOME/.cargo/bin:$PATH" tools/cargo-capped "+$TOOLCHAIN" "$@"
}

if [ "${1:-}" = cargo ]; then
  shift
  cargo "$@" --target "$TARGET"
  exit
fi

target_dir=$(realpath -m "${CARGO_TARGET_DIR:-local/target}")
out=$target_dir/$TARGET/release
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)

# 4. The icon and version information, compiled into an object that every
#    program and the DLL link.  Explorer shows the icon for the files and
#    in "Open with", and the version on the Details tab of Properties.  The
#    numeric version has four parts and no pre-release suffix.
numeric=${version%%[-+]*}
mkdir -p "$out"
sed -e "s/@VERSION@/$version/g" -e "s/@VERSION_COMMAS@/${numeric//./,},0/g" data/windows/slerp.rc >"$out/slerp-resources.rc"
$MINGW-windres --include-dir data/windows -O coff -i "$out/slerp-resources.rc" -o "$out/slerp-resources.o"

# 5. Panic messages and C++ assertions name source files, so the paths of
#    the builder's home and source folder are rewritten, as
#    tools/build-linux.sh does.  These flags, and the resources, are for
#    release builds only, so `build-windows.sh cargo ...` keeps its own
#    build output.
src=$(pwd -P)
remap="${CARGO_HOME:-$HOME/.cargo}=/cargo"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="$CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS \
--remap-path-prefix=$remap --remap-path-prefix=$src=/slerp -C link-arg=$out/slerp-resources.o"
export CFLAGS_x86_64_pc_windows_gnu="-ffile-prefix-map=$remap -ffile-prefix-map=$src=/slerp"
export CXXFLAGS_x86_64_pc_windows_gnu=$CFLAGS_x86_64_pc_windows_gnu

# 6. The release build: the command line and viewer, Explorer's thumbnail
#    handler and the console-less viewer launcher, none needing DLLs.
cargo build --release --locked --target "$TARGET" -p slerp -p slerp-winshell
files="slerp.exe slerpw.exe slerp_shell.dll"
for file in $files; do
  dlls=$($MINGW-objdump -p "$out/$file" | sed -n 's/^\s*DLL Name: //p' | sort -u)
  echo "$file imports: $(echo $dlls)"
  for dll in $dlls; do
    case "${dll,,}" in
      libstdc++*|libgcc*|libwinpthread*) echo "error: $file links $dll dynamically" >&2; exit 1 ;;
    esac
  done
  if ! $MINGW-objdump -h "$out/$file" | grep -q ' \.rsrc '; then
    echo "error: $file has no icon or version information" >&2
    exit 1
  fi
  if grep -q -a -F -e "$src/crates/" -e "$src/src/" -e "${remap%=*}/registry/" "$out/$file"; then
    echo "error: $file names a path in $src or ${remap%=*}" >&2
    exit 1
  fi
done
exports=$($MINGW-objdump -p "$out/slerp_shell.dll" | grep -cE ' Dll(GetClassObject|CanUnloadNow|(Unr|R)egisterServer)$' || true)
if [ "$exports" != 4 ]; then
  echo "error: slerp_shell.dll exports $exports of the 4 COM entry points" >&2
  exit 1
fi

# 7. The zip, with data/windows/README.txt in Windows line endings.
dist=local/dist
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$dist" "$stage/Slerp"
for file in $files; do cp "$out/$file" "$stage/Slerp/"; done
cp README.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md "$stage/Slerp/"
sed 's/$/\r/' data/windows/README.txt > "$stage/Slerp/README.txt"
zip_path=$(realpath "$dist")/Slerp-windows-x64.zip
rm -f "$zip_path"
(cd "$stage" && zip -qr9 "$zip_path" Slerp)
ls -l "$zip_path"
