fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if (target_os == "macos" || target_os == "ios")
        && std::env::var_os("CARGO_FEATURE_METAL").is_some()
    {
        let target = std::env::var("TARGET").unwrap_or_default();
        let simulator = target.contains("ios-sim") || target.contains("simulator");
        if !simulator {
            println!("cargo:rustc-link-arg=-Wl,-weak_framework,MetalFX");
        }
    }
    if target_os == "android" {
        println!("cargo:rustc-link-lib=dylib=c++_shared");
        println!("cargo:rustc-link-lib=dylib=dl");
        println!("cargo:rustc-link-lib=dylib=m");
    }

    #[cfg(feature = "vulkan")]
    compile_vulkan_shaders();
}

#[cfg(feature = "vulkan")]
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
