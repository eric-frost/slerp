//! Builds the vendored jxrlib decoder and the small shim over it.

fn main() {
    let v = "vendor/jxrlib";
    let sys = ["adapthuff.c", "image.c", "strcodec.c", "strPredQuant.c", "strTransform.c", "perfTimerANSI.c"];
    let dec = [
        "decode.c",
        "postprocess.c",
        "segdec.c",
        "strdec.c",
        "strInvTransform.c",
        "strPredQuantDec.c",
        "JXRTranscode.c",
    ];
    // The glue code links against the encoder too; the linker drops what
    // the shim never calls.
    let enc = ["encode.c", "segenc.c", "strenc.c", "strFwdTransform.c", "strPredQuantEnc.c"];
    let glue = ["JXRGlue.c", "JXRGlueJxr.c", "JXRGluePFC.c", "JXRMeta.c"];
    let mut build = cc::Build::new();
    build
        .include(v)
        .include(format!("{v}/common/include"))
        .include(format!("{v}/image/sys"))
        .include(format!("{v}/image/decode"))
        .include(format!("{v}/image/encode"))
        .include(format!("{v}/image/x86"))
        .include(format!("{v}/jxrgluelib"))
        .define("__ANSI__", None)
        .define("DISABLE_PERF_MEASUREMENT", None)
        // jxrlib is old C that GCC and Clang warn about at length.
        .warnings(false)
        .extra_warnings(false)
        .flag_if_supported("-w")
        .flag_if_supported("-fno-strict-aliasing")
        .opt_level(2);
    // MinGW predefines WIN32, which makes JXRMeta.h expect the Microsoft SAL
    // annotations (__in) that only windows.h provides.  The portable path the
    // Linux build takes brings its own.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        build.flag_if_supported("-UWIN32");
    }
    for f in sys {
        build.file(format!("{v}/image/sys/{f}"));
        println!("cargo:rerun-if-changed={v}/image/sys/{f}");
    }
    for f in dec {
        build.file(format!("{v}/image/decode/{f}"));
        println!("cargo:rerun-if-changed={v}/image/decode/{f}");
    }
    for f in enc {
        build.file(format!("{v}/image/encode/{f}"));
        println!("cargo:rerun-if-changed={v}/image/encode/{f}");
    }
    for f in glue {
        build.file(format!("{v}/jxrgluelib/{f}"));
        println!("cargo:rerun-if-changed={v}/jxrgluelib/{f}");
    }
    build.file("src/shim.c");
    println!("cargo:rerun-if-changed=src/shim.c");
    build.compile("slerp_jxrlib");
}
