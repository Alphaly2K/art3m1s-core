use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=CMAKE");
    println!("cargo:rerun-if-env-changed=KRKRSDL3_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=KRKRSDL3_BUILD_DIR");

    let bootstrap = env::var_os("CARGO_FEATURE_NATIVE_BOOTSTRAP").is_some();
    let upstream = env::var_os("CARGO_FEATURE_NATIVE_UPSTREAM_SMOKE").is_some();
    if !bootstrap && !upstream {
        return;
    }
    assert!(
        !(bootstrap && upstream),
        "enable only one native KRKR backend feature"
    );

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(if upstream {
        "native-upstream-smoke"
    } else {
        "native-bootstrap"
    });
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
    assert!(
        !upstream || target_os == "macos",
        "native-upstream-smoke currently supports macOS only"
    );

    let _ = fs::remove_dir_all(&out_dir);

    let cmake_source = if upstream {
        manifest_dir.join("native/upstream")
    } else {
        manifest_dir.join("native")
    };

    let mut configure = Command::new(&cmake);
    configure
        .arg("-S")
        .arg(cmake_source)
        .arg("-B")
        .arg(&out_dir)
        .arg("-G")
        .arg("Ninja")
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"));

    if upstream {
        let krkr_source = env::var_os("KRKRSDL3_SOURCE_DIR")
            .expect("KRKRSDL3_SOURCE_DIR is required for native-upstream-smoke");
        let krkr_build = env::var_os("KRKRSDL3_BUILD_DIR")
            .expect("KRKRSDL3_BUILD_DIR is required for native-upstream-smoke");
        let krkr_build = PathBuf::from(krkr_build);
        configure
            .arg(format!(
                "-DKRKRSDL3_SOURCE_DIR={}",
                PathBuf::from(krkr_source).display()
            ))
            .arg(format!("-DKRKRSDL3_BUILD_DIR={}", krkr_build.display()))
            .arg(format!(
                "-DVCPKG_OVERLAY_TRIPLETS={}",
                krkr_build.join("vcpkg/triplets").display()
            ));

        if let Some(vcpkg_root) = env::var_os("VCPKG_ROOT") {
            let vcpkg_root = PathBuf::from(vcpkg_root);
            configure.arg(format!(
                "-DCMAKE_TOOLCHAIN_FILE={}",
                vcpkg_root
                    .join("scripts/buildsystems/vcpkg.cmake")
                    .display()
            ));
        }
        if target_arch == "arm64" {
            configure.arg("-DVCPKG_TARGET_TRIPLET=arm64-osx");
        }
    }

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
            if upstream {
                configure.arg("-DMACOS=TRUE");
            }
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
