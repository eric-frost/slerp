//! Builds Epic's openriglogic (the DNA reader and RigLogic) from the copy in
//! `openriglogic/`, together with the C++ half of the cxx bridge.
//!
//! The copy is the `include/` and `src/` folders of
//! <https://github.com/EpicGames/openriglogic>, branch `5.8`, commit
//! 55756ac ("Import RigLogic v13.2.9-hotfix1 (UE5.8.4) snapshot"), MIT
//! licensed.  One local change: `src/dna/LODMapping.cpp` checks a level of
//! detail's index list before using it, which upstream only asserts.
//!
//! The library is compiled with the `cc` crate rather than its CMake
//! project, so building Slerp needs only a C++ compiler.  Its CMake build
//! would add the same definitions: every rotation order RigLogic can
//! output, and no SIMD paths, so the code runs on any CPU of the target.

use std::path::{Path, PathBuf};

fn main() {
    let root = Path::new("openriglogic");
    let mut sources = Vec::new();
    collect(&root.join("src"), &mut sources);
    sources.sort();

    let mut build = cxx_build::bridge("src/ffi.rs");
    build
        .file("cpp/bridge.cpp")
        .files(&sources)
        .include(root.join("include"))
        .include(root.join("src"))
        .include(".")
        .std("c++17")
        // Release semantics whatever the profile: asserts off (the reader's
        // asserts would abort on damaged files rather than report them) and
        // optimised, since reading a 50 MB head at -O0 takes seconds.
        .define("NDEBUG", None)
        .opt_level(2)
        .warnings(false)
        .extra_warnings(false);
    for order in ["XYZ", "XZY", "YXZ", "YZX", "ZXY", "ZYX"] {
        build.define(&format!("RL_BUILD_WITH_{order}_ROTATION_ORDER"), None);
    }
    build.compile("slerp_openriglogic");

    println!("cargo:rerun-if-changed=cpp/bridge.cpp");
    println!("cargo:rerun-if-changed=cpp/bridge.h");
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=openriglogic");
}

/// Every `.cpp` file under `dir`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "cpp") {
            out.push(path);
        }
    }
}
