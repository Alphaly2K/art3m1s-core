use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=CMAKE");

    if env::var_os("CARGO_FEATURE_NATIVE_BOOTSTRAP").is_none() {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native-bootstrap");
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let build_type = if profile == "release" {
        "Release"
    } else {
        "Debug"
    };
    let cmake = env::var_os("CMAKE").unwrap_or_else(|| "cmake".into());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch_value = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_arch = match target_arch_value.as_str() {
        "aarch64" => "arm64",
        arch => arch,
    };

    let _ = fs::remove_dir_all(&out_dir);

    let mut configure = Command::new(&cmake);
    configure
        .arg("-S")
        .arg(manifest_dir.join("native"))
        .arg("-B")
        .arg(&out_dir)
        .arg("-G")
        .arg("Ninja")
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"));

    match target_os.as_str() {
        "macos" => {
            let deployment_target =
                env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".to_string());
            let compiler_target = format!("{target_arch}-apple-macosx{deployment_target}");
            configure
                .arg("-DCMAKE_SYSTEM_NAME=Darwin")
                .arg(format!("-DCMAKE_OSX_ARCHITECTURES={target_arch}"))
                .arg(format!("-DCMAKE_OSX_DEPLOYMENT_TARGET={deployment_target}"))
                .arg(format!("-DCMAKE_C_COMPILER_TARGET={compiler_target}"))
                .arg(format!("-DCMAKE_CXX_COMPILER_TARGET={compiler_target}"));
            if let Some(sdkroot) = apple_sdk_path("macosx") {
                configure.arg(format!("-DCMAKE_OSX_SYSROOT={sdkroot}"));
            }
        }
        "ios" => {
            let deployment_target =
                env::var("IPHONEOS_DEPLOYMENT_TARGET").unwrap_or_else(|_| "13.0".to_string());
            let simulator_suffix = if env::var("CARGO_CFG_TARGET_ABI").as_deref() == Ok("sim") {
                "-simulator"
            } else {
                ""
            };
            let compiler_target =
                format!("{target_arch}-apple-ios{deployment_target}{simulator_suffix}");
            configure
                .arg("-DCMAKE_SYSTEM_NAME=iOS")
                .arg(format!("-DCMAKE_OSX_ARCHITECTURES={target_arch}"))
                .arg(format!("-DCMAKE_OSX_DEPLOYMENT_TARGET={deployment_target}"))
                .arg(format!("-DCMAKE_C_COMPILER_TARGET={compiler_target}"))
                .arg(format!("-DCMAKE_CXX_COMPILER_TARGET={compiler_target}"));
            let sdk = if env::var("CARGO_CFG_TARGET_ABI").as_deref() == Ok("sim") {
                "iphonesimulator"
            } else {
                "iphoneos"
            };
            if let Some(sdkroot) = apple_sdk_path(sdk) {
                configure.arg(format!("-DCMAKE_OSX_SYSROOT={sdkroot}"));
            }
        }
        _ => {}
    }

    let status = configure
        .status()
        .unwrap_or_else(|error| panic!("failed to run cmake: {error}"));
    assert!(status.success(), "native-bootstrap configure failed");

    let status = Command::new(&cmake)
        .arg("--build")
        .arg(&out_dir)
        .arg("--config")
        .arg(build_type)
        .status()
        .unwrap_or_else(|error| panic!("failed to run cmake --build: {error}"));
    assert!(status.success(), "native-bootstrap build failed");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=art3m1s_krkr_host");

    if matches!(target_os.as_str(), "macos" | "ios" | "linux" | "android") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", out_dir.display());
    }
}

fn apple_sdk_path(sdk: &str) -> Option<String> {
    let output = Command::new("xcrun")
        .arg("--sdk")
        .arg(sdk)
        .arg("--show-sdk-path")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
}
