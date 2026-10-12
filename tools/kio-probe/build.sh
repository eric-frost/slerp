#!/bin/sh
# Builds the KIO probe into <project>/local/kio-probe, with its CMake build tree in local/target-kio-probe.
set -e
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cmake -S "$here" -B "$root/local/target-kio-probe" -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$root/local/target-kio-probe" -j >/dev/null
cp "$root/local/target-kio-probe/kio-probe" "$root/local/kio-probe"
echo "built $root/local/kio-probe"
