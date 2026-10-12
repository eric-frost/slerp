# Fuzzing Slerp's readers

Slerp's thumbnailer reads every file a user browses past, downloads included, so no reader may panic, hang, overflow the stack or allocate without bound on a hostile file.  This folder holds libFuzzer targets for the readers that take untrusted bytes, built with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz).  `tools/fuzz.sh` builds, seeds and runs them.

## Setup

cargo-fuzz needs a nightly compiler.  Slerp itself builds with stable Rust, so `tools/fuzz.sh` takes nightly from rustup (`~/.cargo/bin/rustup`) only for its own commands, which works whether or not rustup's proxies are on `PATH`.

```sh
~/.cargo/bin/rustup toolchain install nightly --profile minimal
~/.cargo/bin/cargo +nightly install cargo-fuzz \
  --root local/tools/cargo-fuzz
```

Builds go to `local/target-fuzz` (intermediates to `local/build-fuzz`); corpora, crashes and logs to `local/fuzz/` (or `SLERP_FUZZ_DIR`), which `corpus` and `artifacts` here may link to.  A cargo-fuzz on `PATH` works too.

## Use

```sh
tools/fuzz.sh list                 # the targets
tools/fuzz.sh build [target...]    # build (each target on its own)
tools/fuzz.sh seed [target...]     # seed corpora from local/assets
tools/fuzz.sh run roblox 1800      # fuzz one target for 30 minutes
JOBS=4 tools/fuzz.sh run all 600   # every target for 10 minutes, 4 at once
FORK=1 JOBS=2 tools/fuzz.sh run quake,tiff 3600  # go on past crashes
tools/fuzz.sh triage               # re-run every artifact
tools/fuzz.sh repro roblox local/fuzz/artifacts/roblox/crash-...
tools/fuzz.sh tmin roblox local/fuzz/artifacts/roblox/crash-...
```

Fuzzers run at `nice 19` with `-rss_limit_mb=2048`, at most four at once (with `FORK=1` each run is two processes, so use `JOBS=2`), in a systemd user slice, `heavy-slerp.slice`, which the script caps at 8 GB of memory.  Text formats get a libFuzzer dictionary from `dicts/<target>.dict`.  Crashes, timeouts and inputs slower than 3 s land in `local/fuzz/artifacts/<target>/`, logs in `local/fuzz/logs/`.

## How the targets work

- **One target per reader crate**, a few split by risk (`models` for binary formats, `models_text` for text ones; `palette` apart from `previews`), plus `core_sniff` and `core_thumbnail`, which take any extension the format table knows through `slerp_core::sniff` and `prepare_thumbnail`, and `contents`, the archive items and their exports.  Each target is a cargo feature naming the crates it needs, so a crate that doesn't compile stops only its own targets.
- **Inputs** are a selector byte (two for the core targets), a flags byte, then the file.  The selector picks the extension from the target's list, since many formats are known by name; the file is written to a private scratch folder for readers that take a path.  The flags switch animations on (as the viewer loads), neighbouring files off (as in GNOME's sandbox), the thumbnail size, and pick a part (a Doom map, a Source skin) or a second code path.  A target binary run with `SLERP_FUZZ_LIST_EXTS=1` prints its extensions, which `tools/fuzz-seed.ts` uses to turn samples from `local/assets` into seeds.
- **After loading**, `slerp_fuzz::finish` does the thumbnailer's CPU work: shrinking pictures, composing canvases, posing scenes and measuring their bounds.
- **Caps.**  Builds are release builds (`cargo fuzz build -O`) with overflow checks and debug assertions turned back on for Slerp's own crates in `Cargo.toml`, so arithmetic overflow in Slerp is a crash; other people's crates build as they ship, where it wraps (the laz and exr crates overflow in their own arithmetic on damaged files, which is theirs to fix and harmless in release).  The harness's global allocator aborts on one allocation over 512 MB or 1.5 GB held at once (`SLERP_FUZZ_MAX_ALLOC_MB`, `SLERP_FUZZ_MAX_LIVE_MB`), which catches allocations sized by a header before their pages are touched; libFuzzer's RSS limit wouldn't see those.  Readers get thumbnail `Limits`.
- **Panics.**  Slerp's own panics abort at once, even where its code catches them, since it shouldn't panic at all.  Panics inside other people's crates unwind as they would in a release build, to the `catch_unwind` Slerp puts around calls into crates that aren't hardened (laz, tiny-skia, earcut); one nothing catches still aborts.  `SIGPIPE` is ignored, as Rust's own `main` does.
- **Isolated code.**  C and C++ decoders (jxrlib, libheif, rav1d, openriglogic) and the PDF and SVG renderers run in a forked child (`crates/isolate`), which turns their crashes into errors.  The harness resets signal handlers in forked children, so such a crash stays the child's, and keeps the input and message in `local/fuzz/isolated/<target>/`.  libFuzzer sees no coverage inside the child, so these targets mostly exercise the Rust around it.
- **Scratch.**  `HOME` and the XDG folders point into `/tmp/slerp-fuzz/<target>-<pid>/`, so readers that cache or look for the user's files touch nothing real.

## Regressions

Each crash fixed has a regression test in the crate that crashed (`cargo test -p slerp-<crate>`): the minimised input as bytes, a small file under the crate's `tests/fuzz/`, or the same damage made to a corpus sample.  Nothing here is kept: corpora and artifacts are scratch.
