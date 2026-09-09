// 构建脚本：针对 Android target 链接 C++ 标准库。
//
// ffmpeg-next (ffmpeg-sys-next) 在 "build" feature 下会从源码编译 FFmpeg，
// 某些编解码器模块即使静态编译也会引用 C++ ABI 符号（如 __cxa_pure_virtual）。
// Cargo.toml 里的 [target.*] rustflags 不会被 Cargo 应用（rustflags 只能放
// .cargo/config.toml 或环境变量），所以必须通过 build.rs 显式链接。
//
// 用 c++_shared（动态）而不是 c++_static（静态），原因：
// - 静态链接时链接器按符号解析顺序处理 .a，libc++ 若先于 ffmpeg 静态库
//   处理，里面的对象文件不会被拉入，导致 __cxa_pure_virtual 仍然未定义。
// - c++_shared 没有顺序问题，且 NDK 自带 libc++_shared.so，打包进 APK 即可。

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if (target_os == "macos" || target_os == "ios")
        && std::env::var_os("CARGO_FEATURE_METAL_BACKEND").is_some()
    {
        // MetalFX is optional at runtime (macOS 13/iOS 16 and supported GPU),
        // so keep the framework weak-linked and let the Objective-C wrapper
        // fall back when the class is absent.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,MetalFX");
    }
    if target_os == "android" {
        // 链接 NDK 自带的动态 C++ 标准库（libc++_shared.so）。
        println!("cargo:rustc-link-lib=dylib=c++_shared");
        // Android 动态加载（dlopen/dlsym）必需的库。
        println!("cargo:rustc-link-lib=dylib=dl");
        // 数学函数库（FFmpeg 依赖）。
        println!("cargo:rustc-link-lib=dylib=m");
    }

    #[cfg(feature = "vulkan-backend")]
    if std::env::var_os("CARGO_FEATURE_VULKAN_BACKEND").is_some() {
        compile_vulkan_shaders();
    }
}

#[cfg(feature = "vulkan-backend")]
fn compile_vulkan_shaders() {
    use naga::back::spv;
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    use std::path::{Path, PathBuf};

    let source_path = Path::new("src/backend/vulkan/shaders.wgsl");
    println!("cargo:rerun-if-changed={}", source_path.display());
    let source = std::fs::read_to_string(source_path).expect("read Vulkan WGSL shaders");
    let module = naga::front::wgsl::parse_str(&source).expect("parse Vulkan WGSL shaders");
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .expect("validate Vulkan WGSL shaders");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    // Keep naga's Y-up clip-space rewrite. VulkanBackend's CPU projection
    // is authored for WGSL/Metal (stage y=0 -> clip y=+1); this flag then
    // flips Position.y for SPIR-V. Turning it off would invert sprites.
    let options = spv::Options::default();
    assert!(
        options
            .flags
            .contains(spv::WriterFlags::ADJUST_COORDINATE_SPACE),
        "Vulkan vertex uniforms assume naga ADJUST_COORDINATE_SPACE",
    );
    for (entry_point, stage, file) in [
        (
            "sprite_vertex",
            naga::ShaderStage::Vertex,
            "vulkan_sprite.vert.spv",
        ),
        (
            "sprite_fragment",
            naga::ShaderStage::Fragment,
            "vulkan_sprite.frag.spv",
        ),
        (
            "alpha_mask_fragment",
            naga::ShaderStage::Fragment,
            "vulkan_alpha_mask.frag.spv",
        ),
        (
            "group_composite_fragment",
            naga::ShaderStage::Fragment,
            "vulkan_group.frag.spv",
        ),
        (
            "rule_transition_fragment",
            naga::ShaderStage::Fragment,
            "vulkan_rule.frag.spv",
        ),
    ] {
        let words = spv::write_vec(
            &module,
            &info,
            &options,
            Some(&spv::PipelineOptions {
                shader_stage: stage,
                entry_point: entry_point.into(),
            }),
        )
        .unwrap_or_else(|error| panic!("compile Vulkan shader {entry_point}: {error}"));
        let bytes = words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        std::fs::write(output.join(file), bytes).expect("write Vulkan SPIR-V shader");
    }
}
