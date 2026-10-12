#!/bin/bash
# Packs the source tree into local/dist/slerp-<version>.tar.gz: every file
# Git would track (committed or not, less what .gitignore leaves out) under
# slerp-<version>/, the layout of GitHub's release archives.  The AUR
# package and tools/test-packages.sh build from it.  Symbolic links that
# point outside the tree (fuzz/corpus, which lives in local/) are left out.
#
# Usage: tools/source-tarball.sh   (DIST picks another output folder)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
DIST=${DIST:-local/dist}
OUT="$DIST/slerp-$VERSION.tar.gz"
mkdir -p "$DIST"

git ls-files -z --cached --others --exclude-standard . |
	while IFS= read -r -d '' f; do
		[ -e "$f" ] || [ -L "$f" ] || continue
		if [ -L "$f" ]; then case $(readlink "$f") in /*) continue ;; esac; fi
		printf '%s\0' "$f"
	done |
	tar --create --no-recursion --null --sort=name --owner=0 --group=0 --numeric-owner --mode=a+rX,go-w \
		--transform "s,^,slerp-$VERSION/," --files-from=- \
		--mtime="@$(date -d "$(sed -n '5s/^ -- .*>  //p' packaging/debian/changelog)" +%s)" |
	gzip -9n >"$OUT"
echo "$OUT ($(du -h "$OUT" | cut -f1))"
