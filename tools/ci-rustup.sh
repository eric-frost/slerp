#!/bin/bash
# Installs a Rust toolchain with rustup on a CI machine and makes it the
# default, installing rustup first where the machine has none.  For the
# release workflow.
#
#   tools/ci-rustup.sh VERSION [TARGET...]
set -euo pipefail
version=$1
shift
if ! command -v rustup >/dev/null; then
	export PATH=$HOME/.cargo/bin:$PATH
	if ! command -v rustup >/dev/null; then
		curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none
	fi
	[ -z "${GITHUB_PATH:-}" ] || echo "$HOME/.cargo/bin" >>"$GITHUB_PATH"
fi
targets=()
for target in "$@"; do targets+=(--target "$target"); done
rustup toolchain install "$version" --profile minimal ${targets[@]+"${targets[@]}"}
rustup default "$version"
rustc --version
