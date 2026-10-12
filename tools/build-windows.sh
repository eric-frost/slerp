#!/bin/bash
# Cross-compiles Slerp for 64-bit Windows on Linux and packs it as a zip:
# slerp.exe, Explorer's thumbnail handler slerp_shell.dll, the
# console-less viewer launcher slerpw.exe, a short README.txt, and the
# licences with THIRD-PARTY.md, which the bundled crates' licences require.
#
#   tools/build-windows.sh                 # release build, then local/dist/*.zip
#   tools/build-windows.sh cargo check     # any cargo command, cross-compiling
#
# Needs a rustup toolchain with the x86_64-pc-windows-gnu standard library
# (Ubuntu's own Rust ships none, so rustup's proxies in ~/.cargo/bin are used
# without being on PATH), apt's mingw-w64 posix-threads compilers, and
# libclang for bindgen.  Builds run inside heavy-slerp.slice through
# tools/cargo-capped, in the shared target directory local/target (or
# CARGO_TARGET_DIR).
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

# 4. The release build: the command line and viewer, Explorer's thumbnail
#    handler and the console-less viewer launcher, none needing DLLs.
cargo build --release --locked --target "$TARGET" -p slerp -p slerp-winshell
out=${CARGO_TARGET_DIR:-local/target}/$TARGET/release
files="slerp.exe slerpw.exe slerp_shell.dll"
for file in $files; do
  dlls=$($MINGW-objdump -p "$out/$file" | sed -n 's/^\s*DLL Name: //p' | sort -u)
  echo "$file imports: $(echo $dlls)"
  for dll in $dlls; do
    case "${dll,,}" in
      libstdc++*|libgcc*|libwinpthread*) echo "error: $file links $dll dynamically" >&2; exit 1 ;;
    esac
  done
done
exports=$($MINGW-objdump -p "$out/slerp_shell.dll" | grep -cE ' Dll(GetClassObject|CanUnloadNow|(Unr|R)egisterServer)$' || true)
if [ "$exports" != 4 ]; then
  echo "error: slerp_shell.dll exports $exports of the 4 COM entry points" >&2
  exit 1
fi

# 5. The zip, with data/windows/README.txt in Windows line endings.
dist=local/dist
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$dist" "$stage/Slerp"
for file in $files; do cp "$out/$file" "$stage/Slerp/"; done
cp README.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md "$stage/Slerp/"
sed 's/$/\r/' data/windows/README.txt > "$stage/Slerp/README.txt"
zip_path=$(realpath "$dist")/slerp-windows-x86_64.zip
rm -f "$zip_path"
(cd "$stage" && zip -qr9 "$zip_path" Slerp)
ls -l "$zip_path"
