//! Compiles the vendored NP2 `i286c` core (`vendor/np2/`) plus the hootrip FFI
//! shim (`csrc/np2ffi.c`) into a static lib linked by this crate. See
//! `vendor/np2/README-VENDORED.md` for the extraction recipe this mirrors.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let np2 = manifest.join("../../vendor/np2");
    let cpu = np2.join("cpu");
    let glue = np2.join("glue");
    let include = np2.join("include");

    let core_srcs = [
        "i286c.c", "i286c_0f.c", "i286c_8x.c", "i286c_ea.c", "i286c_f6.c",
        "i286c_fe.c", "i286c_mn.c", "i286c_rp.c", "i286c_sf.c", "v30patch.c",
    ];
    let glue_srcs = ["np2mem.c", "np2io.c"];

    let mut build = cc::Build::new();
    build
        .include(&glue) // <np2types.h> first
        .include(&include) // stub <pccore.h>, <io/iocore.h>, ...
        .include(&cpu) // <cpucore.h> + quote-included .h/.mcr
        .define("BYTESEX_LITTLE", None)
        .flag_if_supported("-Wno-unused")
        .flag_if_supported("-Wno-parentheses")
        .warnings(false);

    for f in core_srcs {
        build.file(cpu.join(f));
    }
    for f in glue_srcs {
        build.file(glue.join(f));
    }
    build.file(manifest.join("csrc/np2ffi.c"));

    build.compile("np2i286c");

    println!("cargo:rerun-if-changed=csrc/np2ffi.c");
    println!("cargo:rerun-if-changed={}", cpu.display());
    println!("cargo:rerun-if-changed={}", glue.display());
    println!("cargo:rerun-if-changed={}", include.display());
}
