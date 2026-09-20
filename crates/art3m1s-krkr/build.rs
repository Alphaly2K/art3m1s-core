use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=CMAKE");
    println!("cargo:rerun-if-env-changed=KRKRSDL3_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=KRKRSDL3_BUILD_DIR");
    println!("cargo:rerun-if-env-changed=ART3M1S_KRKR_REQUIRE_UPSTREAM");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK");
    println!("cargo:rerun-if-env-changed=NDK_HOME");
    println!("cargo:rerun-if-env-changed=CARGO_NDK_ANDROID_PLATFORM");
    println!("cargo:rerun-if-env-changed=CARGO_NDK_SYSROOT_PATH");

    let bootstrap = env::var_os("CARGO_FEATURE_NATIVE_BOOTSTRAP").is_some();
    let upstream_feature = env::var_os("CARGO_FEATURE_NATIVE_UPSTREAM").is_some();
    let upstream_smoke = env::var_os("CARGO_FEATURE_NATIVE_UPSTREAM_SMOKE").is_some();
    let upstream_requested = upstream_feature || upstream_smoke;
    if !bootstrap && !upstream_requested {
        return;
    }

    let krkr_source = env::var_os("KRKRSDL3_SOURCE_DIR");
    let krkr_build = env::var_os("KRKRSDL3_BUILD_DIR");
    let upstream_available = krkr_source.is_some() && krkr_build.is_some();
    let require_upstream = upstream_smoke || env::var_os("ART3M1S_KRKR_REQUIRE_UPSTREAM").is_some();
    assert!(
        !require_upstream || upstream_available,
        "KRKRSDL3_SOURCE_DIR and KRKRSDL3_BUILD_DIR are required for the upstream KRKR backend"
    );
    let upstream = upstream_requested && upstream_available;
    if upstream_requested && !upstream {
        println!(
            "cargo:warning=KRKRSDL3 source/build directories are not configured; \
             falling back to the bootstrap KRKR shim"
        );
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(if upstream {
        "native-upstream"
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
        "the upstream KRKR backend currently supports macOS only"
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
        let krkr_source = krkr_source.expect("checked above");
        let krkr_build = PathBuf::from(krkr_build.expect("checked above"));
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
        "android" => {
            // The real Android runtime follows krkrsdl3_build's Gradle/vcpkg
            // pipeline. The macOS-only upstream guard above means this branch
            // currently cross-compiles only the small ABI bootstrap shim.
            let ndk_root = android_ndk_root().unwrap_or_else(|| {
                panic!(
                    "Android NDK not found; set ANDROID_NDK_HOME/ANDROID_NDK_ROOT or build with cargo-ndk"
                )
            });
            let toolchain = ndk_root.join("build/cmake/android.toolchain.cmake");
            let abi = android_abi(&target_arch_value);
            let platform = android_platform();
            configure
                .arg(format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()))
                .arg(format!("-DANDROID_ABI={abi}"))
                .arg(format!("-DANDROID_PLATFORM={platform}"))
                .arg("-DANDROID_STL=c++_shared");
        }
        _ => {}
    }

    let status = configure
        .status()
        .unwrap_or_else(|error| panic!("failed to run cmake: {error}"));
    assert!(status.success(), "KRKR native shim configure failed");

    let status = Command::new(&cmake)
        .arg("--build")
        .arg(&out_dir)
        .arg("--config")
        .arg(build_type)
        .status()
        .unwrap_or_else(|error| panic!("failed to run cmake --build: {error}"));
    assert!(status.success(), "KRKR native shim build failed");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=art3m1s_krkr_host");
    println!("cargo::metadata=native_dir={}", out_dir.display());

    if matches!(target_os.as_str(), "macos" | "ios" | "linux" | "android") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", out_dir.display());
    }
}

fn android_abi(target_arch: &str) -> &'static str {
    match target_arch {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86" => "x86",
        "x86_64" => "x86_64",
        arch => panic!("unsupported Android target architecture: {arch}"),
    }
}

fn android_platform() -> String {
    let platform = env::var("CARGO_NDK_ANDROID_PLATFORM").unwrap_or_else(|_| "21".to_string());
    if platform.starts_with("android-") {
        platform
    } else {
        format!("android-{platform}")
    }
}

fn android_ndk_root() -> Option<PathBuf> {
    for name in [
        "ANDROID_NDK_HOME",
        "ANDROID_NDK_ROOT",
        "ANDROID_NDK",
        "NDK_HOME",
    ] {
        if let Some(root) = env::var_os(name).map(PathBuf::from)
            && is_android_ndk_root(&root)
        {
            return Some(root);
        }
    }

    // cargo-ndk always exposes its selected sysroot to build scripts. Walking
    // upwards also works when it discovered the NDK through the Android SDK
    // instead of one of the conventional NDK environment variables.
    let sysroot = env::var_os("CARGO_NDK_SYSROOT_PATH").map(PathBuf::from)?;
    sysroot
        .ancestors()
        .find(|candidate| is_android_ndk_root(candidate))
        .map(PathBuf::from)
}

fn is_android_ndk_root(path: &std::path::Path) -> bool {
    path.join("build/cmake/android.toolchain.cmake").is_file()
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
