#!/bin/bash
# Fuzzes Slerp's readers with cargo-fuzz and libFuzzer (see fuzz/README.md).
#
#   tools/fuzz.sh list                       the targets
#   tools/fuzz.sh build [target...]          build them (all by default)
#   tools/fuzz.sh seed [target...]           seed corpora from local/assets
#   tools/fuzz.sh run <target,...|all> [secs] fuzz each for secs (default 600)
#   tools/fuzz.sh repro <target> <input>     run one input, with a backtrace
#   tools/fuzz.sh tmin <target> <input>      minimise a crashing input
#   tools/fuzz.sh merge <target>             shrink a corpus to what adds coverage
#   tools/fuzz.sh triage [target...]         re-run every artifact: still failing, and how long
#
# `run` runs the targets named (or all), JOBS at a time (default 4, at
# most 4; FORK=1 goes on past crashes), each in its own scope in
# heavy-slerp.slice at nice 19 with libFuzzer's -rss_limit_mb=2048.  Crashes go to local/fuzz/artifacts/<target>/,
# slow inputs (over 3 s) too, as slow-unit-*; faults inside forked children
# (C decoders, PDF, SVG) to local/fuzz/isolated/<target>/; logs to
# local/fuzz/logs/ (SLERP_FUZZ_DIR moves them all).
#
# Needs the nightly toolchain from the per-user rustup and cargo-fuzz in
# local/tools/cargo-fuzz or on PATH (README.md says how to install them); the
# rustup proxies stay off PATH, so Ubuntu's Rust remains the default.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FUZZ_DIR=${SLERP_FUZZ_DIR:-$ROOT/local/fuzz}
export SLERP_FUZZ_DIR=$FUZZ_DIR
NIGHTLY=$("$HOME/.cargo/bin/rustup" +nightly which cargo 2>/dev/null | xargs -r dirname)
CARGO_FUZZ=$ROOT/local/tools/cargo-fuzz/bin/cargo-fuzz
[ -x "$CARGO_FUZZ" ] || CARGO_FUZZ=$(command -v cargo-fuzz || true)
TRIPLE=x86_64-unknown-linux-gnu
export CARGO_TARGET_DIR=$ROOT/local/target-fuzz
export CARGO_BUILD_BUILD_DIR=$ROOT/local/build-fuzz
BIN_DIR=$CARGO_TARGET_DIR/$TRIPLE/release
SLICE=heavy-slerp.slice
JOBS=${JOBS:-4}
((JOBS > 4)) && JOBS=4

die() { echo "fuzz.sh: $*" >&2; exit 1; }
[ -n "$NIGHTLY" ] || die "no nightly toolchain: ~/.cargo/bin/rustup toolchain install nightly --profile minimal"
[ -x "$CARGO_FUZZ" ] || die "no cargo-fuzz: ~/.cargo/bin/cargo +nightly install cargo-fuzz --root local/tools/cargo-fuzz"

targets() { grep -A1 '^\[\[bin\]\]' "$ROOT/fuzz/Cargo.toml" | sed -n 's/^name = "\(.*\)"$/\1/p'; }

# Seconds libFuzzer lets one input run.  Targets whose readers run in a
# forked child wait for it up to its own limit (30 s for JPEG XR), so they
# get longer; a reader of their own that hangs still shows as a timeout.
timeout_for() {
  case $1 in
    documents | svg | images | jxr | dna | textures | core_thumbnail) echo 45 ;;
    *) echo 10 ;;
  esac
}

in_slice() {
  if ! systemctl --user show "$SLICE" -p MemoryMax | grep -q 'MemoryMax=[0-9]'; then
    systemctl --user set-property --runtime "$SLICE" MemoryHigh=6G MemoryMax=8G MemorySwapMax=1G CPUWeight=20 IOWeight=20
  fi
  systemd-run --user --scope --quiet --slice="$SLICE" nice -n 19 "$@"
}

# Builds each target on its own with its own feature, so a crate that
# doesn't compile (another agent mid-edit) stops only its targets.
build() {
  local list=("$@") failed=()
  [ ${#list[@]} -gt 0 ] || mapfile -t list < <(targets)
  cd "$ROOT"
  for t in "${list[@]}"; do
    PATH="$NIGHTLY:$PATH" in_slice "$CARGO_FUZZ" build -O -s none --codegen-units 16 --features "$t" "$t" || failed+=("$t")
  done
  [ ${#failed[@]} -eq 0 ] || die "failed to build: ${failed[*]}"
}

seed() {
  local list=("$@")
  [ ${#list[@]} -gt 0 ] || mapfile -t list < <(targets)
  for t in "${list[@]}"; do
    [ -x "$BIN_DIR/$t" ] || die "$t isn't built: tools/fuzz.sh build $t"
    local per=30
    case $t in core_*) per=4 ;; esac
    SLERP_FUZZ_LIST_EXTS=1 "$BIN_DIR/$t" >"$FUZZ_DIR/exts-$t.txt"
    deno run --allow-read --allow-write "$ROOT/tools/fuzz-seed.ts" \
      "$ROOT/local/assets" "$FUZZ_DIR/corpus/$t" "$FUZZ_DIR/exts-$t.txt" "$per" "${SEED_MAX_BYTES:-150000}"
  done
}

# Runs one target for $2 seconds, logging to logs/<target>.log.
run_one() {
  local t=$1 secs=$2
  shift 2
  local dict=() fork=() timeout max_len=${MAX_LEN:-262144}
  # FORK=1: libFuzzer runs the target in child processes and goes on after
  # a crash, hang or memory blow-up, so one shallow bug doesn't end a long
  # run (each is still saved as an artifact).
  [ "${FORK:-0}" = 1 ] && fork=(-fork=1 -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1)
  timeout=$(timeout_for "$t")
  # The only DNA sample is 4.5 MB (chalk's Tate body); 3ds Max scenes are
  # OLE files of 200 KB and more, which a cut-off copy doesn't open.
  case $t in
    dna) max_len=6000000 ;;
    max) max_len=1048576 ;;
  esac
  [ -f "$ROOT/fuzz/dicts/$t.dict" ] && dict=("-dict=$ROOT/fuzz/dicts/$t.dict")
  mkdir -p "$FUZZ_DIR/corpus/$t" "$FUZZ_DIR/artifacts/$t" "$FUZZ_DIR/logs"
  rm -rf /tmp/slerp-fuzz/"$t"-* 2>/dev/null || true
  echo "$(date '+%F %T') $t for ${secs}s" >>"$FUZZ_DIR/logs/runs.txt"
  in_slice "$BIN_DIR/$t" "$FUZZ_DIR/corpus/$t" \
    -artifact_prefix="$FUZZ_DIR/artifacts/$t/" \
    -max_total_time="$secs" -timeout="$timeout" -rss_limit_mb=2048 \
    -max_len="$max_len" -report_slow_units=3 -print_final_stats=1 \
    "${dict[@]}" "${fork[@]}" "$@" >>"$FUZZ_DIR/logs/$t.log" 2>&1 || {
    local status=$?
    echo "$(date '+%F %T') $t stopped with status $status (see logs/$t.log)" >>"$FUZZ_DIR/logs/runs.txt"
  }
}

# Runs one target, a comma-separated list or `all`, JOBS at a time.
run() {
  local which=${1:?target, list or all} secs=${2:-600}
  shift $(($# >= 2 ? 2 : 1))
  local list
  if [ "$which" = all ]; then
    mapfile -t list < <(targets)
  else
    IFS=, read -ra list <<<"$which"
  fi
  for t in "${list[@]}"; do
    [ -x "$BIN_DIR/$t" ] || { echo "fuzz.sh: skipping $t, not built" >&2; continue; }
    while (($(jobs -rp | wc -l) >= JOBS)); do wait -n || true; done
    run_one "$t" "$secs" "$@" &
  done
  wait
}

case ${1:-} in
  list) targets ;;
  build) shift; build "$@" ;;
  seed) shift; seed "$@" ;;
  run) shift; run "$@" ;;
  repro)
    t=${2:?target} f=${3:?input}
    RUST_BACKTRACE=1 in_slice "$BIN_DIR/$t" -timeout="$(timeout_for "$t")" -rss_limit_mb=2048 "$f"
    ;;
  tmin)
    t=${2:?target} f=${3:?input}
    in_slice "$BIN_DIR/$t" -minimize_crash=1 -max_total_time=120 -timeout="$(timeout_for "$t")" \
      -rss_limit_mb=2048 -exact_artifact_path="$f.min" "$f"
    ;;
  merge)
    t=${2:?target}
    mkdir -p "$FUZZ_DIR/corpus/$t.merged"
    in_slice "$BIN_DIR/$t" -merge=1 -timeout="$(timeout_for "$t")" -rss_limit_mb=2048 \
      "$FUZZ_DIR/corpus/$t.merged" "$FUZZ_DIR/corpus/$t"
    rm -rf "$FUZZ_DIR/corpus/$t" && mv "$FUZZ_DIR/corpus/$t.merged" "$FUZZ_DIR/corpus/$t"
    ;;
  triage)
    # Each artifact again, alone: the first line of its failure, or its time
    # when it passes (slow units are often slow only under a busy fuzzer).
    shift
    list=("$@")
    [ ${#list[@]} -gt 0 ] || mapfile -t list < <(targets)
    for t in "${list[@]}"; do
      for f in "$FUZZ_DIR/artifacts/$t"/*; do
        [ -f "$f" ] || continue
        out=$("$BIN_DIR/$t" -timeout="$(timeout_for "$t")" -rss_limit_mb=2048 "$f" 2>&1) && ok=1 || ok=0
        if [ $ok = 1 ]; then
          echo "$t $(basename "$f"): passes, $(grep -o 'in [0-9]* ms' <<<"$out")"
        else
          why=$(grep -m1 -E "panicked|slerp-fuzz==|ERROR: libFuzzer|overflow" <<<"$out" | cut -c1-160)
          echo "$t $(basename "$f"): FAILS: $why"
        fi
      done
    done
    ;;
  *) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
