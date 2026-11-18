fn main() {
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos"),
        "the Artemis Metal Host requires macOS"
    );
    println!("cargo:rerun-if-changed=src/abi.c");
    println!("cargo:rerun-if-changed=../../doc/abi/art3m1s_core_v2.draft.h");
    println!("cargo:rerun-if-changed=../../doc/abi/art3m1s_metal_host_v1.h");
    println!("cargo:rustc-link-lib=framework=IOSurface");
    println!(
        "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libart3m1s_artemis_metal_host.dylib"
    );
    cc::Build::new()
        .file("src/abi.c")
        .include("../../doc/abi")
        .warnings_into_errors(true)
        .compile("artemis_metal_host_abi");
}
