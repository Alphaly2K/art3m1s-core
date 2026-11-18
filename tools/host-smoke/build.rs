fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=metal_abi.c");
        println!("cargo:rerun-if-changed=../../doc/abi/art3m1s_core_v2.draft.h");
        cc::Build::new()
            .file("metal_abi.c")
            .include("../../doc/abi")
            .warnings_into_errors(true)
            .compile("artemis_metal_abi");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
    }
}
