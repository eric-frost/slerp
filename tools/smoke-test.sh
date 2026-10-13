#!/bin/bash
# Quick checks of a built slerp program, for the release workflow and
# tools/test-packages.sh: its version, its format table, and thumbnails of
# two samples it writes itself, a GIMP palette (decoded on the CPU) and an
# STL cube (drawn on the GPU, or a software renderer such as Mesa's
# lavapipe or Windows' WARP), plus any FILE given.
#
#   tools/smoke-test.sh SLERP OUT [FILE...]
#
# The samples and thumbnails go in OUT.  SMOKE_GPU=optional only reports a
# cube that fails to draw, for machines that may have no renderer at all.
# It runs in macOS's bash 3.2 and Git Bash on Windows too.
set -euo pipefail
slerp=$1
out=$2
shift 2
mkdir -p "$out"

ms() {
	if [ -n "${EPOCHREALTIME:-}" ]; then echo $((${EPOCHREALTIME/[.,]/} / 1000)); else echo 0; fi
}

echo "== slerp --version"
"$slerp" --version
echo "== slerp formats"
"$slerp" formats >"$out/formats.txt"
lines=$(wc -l <"$out/formats.txt")
echo "$((lines)) lines"
[ "$lines" -gt 100 ] || { echo "the format table is too short"; exit 1; }

printf "GIMP Palette\nName: Smoke test\nColumns: 4\n#\n255 0 0 Red\n0 255 0 Green\n0 0 255 Blue\n255 255 0 Yellow\n" >"$out/smoke.gpl"
{
	echo "solid cube"
	for f in "0 0 -1|0 0 0|1 1 0|1 0 0" "0 0 -1|0 0 0|0 1 0|1 1 0" "0 0 1|0 0 1|1 0 1|1 1 1" "0 0 1|0 0 1|1 1 1|0 1 1" \
		"0 -1 0|0 0 0|1 0 0|1 0 1" "0 -1 0|0 0 0|1 0 1|0 0 1" "0 1 0|0 1 0|0 1 1|1 1 1" "0 1 0|0 1 0|1 1 1|1 1 0" \
		"-1 0 0|0 0 0|0 0 1|0 1 1" "-1 0 0|0 0 0|0 1 1|0 1 0" "1 0 0|1 0 0|1 1 0|1 1 1" "1 0 0|1 0 0|1 1 1|1 0 1"; do
		IFS="|" read -r n a b c <<<"$f"
		printf "facet normal %s\n outer loop\n  vertex %s\n  vertex %s\n  vertex %s\n endloop\nendfacet\n" "$n" "$a" "$b" "$c"
	done
	echo "endsolid cube"
} >"$out/smoke.stl"

# ${@+"$@"} rather than "$@": bash 3.2 calls an empty "$@" unbound.
for f in "$out/smoke.gpl" "$out/smoke.stl" ${@+"$@"}; do
	name=$(basename "$f")
	echo "== slerp thumbnail 256 $name"
	start=$(ms)
	if "$slerp" thumbnail 256 "$f" "$out/$name.png" &&
		[ "$(head -c 8 "$out/$name.png" | od -An -tx1 | tr -d ' \n')" = 89504e470d0a1a0a ]; then
		echo "$(($(ms) - start)) ms, $(wc -c <"$out/$name.png") bytes of PNG"
	elif [ "$name" = smoke.stl ] && [ "${SMOKE_GPU:-}" = optional ]; then
		echo "warning: no thumbnail of the cube; this machine may have no GPU or software renderer"
	else
		echo "error: no thumbnail of $name"
		exit 1
	fi
done
