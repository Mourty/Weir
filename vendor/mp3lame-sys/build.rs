//! Compiles LAME with the cc crate. The published mp3lame-sys runs LAME's
//! configure script on Linux instead, and libtool fails when the folder's
//! path has a space in it.

use std::env;
use std::path::{Path, PathBuf};

const LAME_DIR: &str = "lame-3.100";

const SOURCES: &[&str] = &[
    "bitstream.c",
    "encoder.c",
    "fft.c",
    "gain_analysis.c",
    "id3tag.c",
    "lame.c",
    "newmdct.c",
    "presets.c",
    "psymodel.c",
    "quantize_pvt.c",
    "vector/xmm_quantize_sub.c",
    "quantize.c",
    "reservoir.c",
    "set_get.c",
    "tables.c",
    "takehiro.c",
    "util.c",
    "vbrquantize.c",
    "VbrTag.c",
    "version.c",
];

fn main() {
    if env::var("DOCS_RS").is_ok_and(|docs| docs == "1") {
        return;
    }
    if cfg!(feature = "decoder") {
        panic!("this copy of mp3lame-sys leaves out LAME's decoder");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=config");
    println!("cargo:rerun-if-changed={LAME_DIR}");

    let lame = Path::new(LAME_DIR);
    let mut cc = cc::Build::new();
    for f in SOURCES {
        cc.file(lame.join("libmp3lame").join(f));
    }
    cc.include(lame.join("include"))
        .include(lame.join("libmp3lame"))
        .define("HAVE_CONFIG_H", None)
        .warnings(false)
        .extra_warnings(false);

    if env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc") {
        // As the published crate builds it on Windows.
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set"));
        let include = out.join("include_msvc");
        let _ = std::fs::create_dir(&include);
        std::fs::copy(lame.join("configMS.h"), include.join("config.h")).expect("copy config.h");
        cc.include(&include)
            .define("TAKEHIRO_IEEE754_HACK", None)
            .define("FLOAT8", Some("float"))
            .define("REAL_IS_FLOAT", Some("1"))
            .define("BS_FORMAT", Some("BINARY"))
            .pic(false);
        if env::var("CC")
            .is_ok_and(|c| Path::new(&c).file_stem().and_then(|s| s.to_str()) == Some("clang-cl"))
        {
            cc.flag("/W0");
        }
    } else {
        cc.include("config");
    }
    cc.compile("mp3lame");
}
