//! Backend-neutral Artemis runtime shader ABI and compilation pipeline.
//!
//! Runtime effects are normalized from the legacy Direct3D9-shaped Artemis
//! source into one stable HLSL fragment ABI. With `runtime-shader` enabled the
//! normalized source is compiled to SPIR-V, reflected, and cross-compiled to
//! MSL. Vulkan consumes the SPIR-V directly; Metal consumes the MSL generated
//! from that same module. The legacy GL backend may keep its old translator as
//! a compatibility fallback.

use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use crate::draw::DrawCommand;

/// Opaque logical shader identity. Native pipeline and shader objects are kept
/// in the active backend and are never encoded in this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(u64);

impl ShaderId {
    pub(crate) const fn from_opaque(value: u64) -> Self {
        Self(value)
    }

    pub const fn opaque(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Fragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderSource {
    Hlsl(Arc<[u8]>),
}

impl ShaderSource {
    pub fn hlsl(source: &[u8]) -> Self {
        Self::Hlsl(Arc::from(source))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderTexture {
    Foreground,
    Mask,
    User,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderResourceKind {
    UniformBuffer,
    Texture(ShaderTexture),
    Sampler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderResourceBinding {
    pub name: String,
    pub set: u32,
    pub binding: u32,
    pub kind: ShaderResourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderValueType {
    Float,
    Float2,
    Float3,
    Float4,
    Float4x4,
}

impl ShaderValueType {
    pub const fn value_count(self) -> usize {
        match self {
            Self::Float => 1,
            Self::Float2 => 2,
            Self::Float3 => 3,
            Self::Float4 => 4,
            Self::Float4x4 => 16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderUniformReflection {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub value_type: ShaderValueType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderReflection {
    pub stage: ShaderStage,
    pub entry_point: String,
    pub resources: Vec<ShaderResourceBinding>,
    pub uniform_buffer_size: u32,
    pub uniforms: Vec<ShaderUniformReflection>,
}

#[derive(Debug, Clone)]
pub struct CompiledShader {
    pub name: String,
    pub source: ShaderSource,
    pub normalized_hlsl: Arc<str>,
    pub spirv: Arc<[u32]>,
    pub msl: Arc<str>,
    pub msl_entry_point: String,
    pub reflection: ShaderReflection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderCompileError {
    pub shader_name: String,
    pub stage: ShaderStage,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
}

impl ShaderCompileError {
    pub fn new(shader_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            shader_name: shader_name.into(),
            stage: ShaderStage::Fragment,
            line: None,
            column: None,
            message: message.into(),
        }
    }

    #[cfg_attr(not(feature = "runtime-shader"), allow(dead_code))]
    fn with_compiler_location(mut self) -> Self {
        for line in self.message.lines() {
            let fields = line.split(':').map(str::trim).collect::<Vec<_>>();
            for window in fields.windows(3) {
                if window[0].ends_with(&self.shader_name)
                    && let (Ok(line), Ok(column)) =
                        (window[1].parse::<u32>(), window[2].parse::<u32>())
                {
                    self.line = Some(line);
                    self.column = Some(column);
                    return self;
                }
            }
            for window in fields.windows(2) {
                if window[0].ends_with(&self.shader_name)
                    && let Ok(value) = window[1].parse::<u32>()
                {
                    self.line = Some(value);
                    break;
                }
            }
            if self.line.is_some() {
                break;
            }
        }
        self
    }
}

impl Display for ShaderCompileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "shader={} stage={:?}",
            self.shader_name, self.stage
        )?;
        if let Some(line) = self.line {
            write!(formatter, " line={line}")?;
        }
        if let Some(column) = self.column {
            write!(formatter, " column={column}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for ShaderCompileError {}

/// Per-draw and per-frame values packed according to SPIR-V reflection.
pub struct ShaderRuntimeParameters<'a> {
    pub transform: [f32; 16],
    /// UV offset.xy followed by UV scale.xy.
    pub uv: [f32; 4],
    /// Stage-space x, y, width, height. A missing clip uses the full stage.
    pub clip: [f32; 4],
    pub wipe: [f32; 4],
    /// RGB multiply followed by opacity.
    pub color: [f32; 4],
    /// grayscale, negative, reserved, reserved.
    pub color_flags: [f32; 4],
    /// resolution.xy, time in seconds, frame index represented as f32.
    pub frame: [f32; 4],
    pub effect_values: &'a BTreeMap<String, Vec<f32>>,
}

impl<'a> ShaderRuntimeParameters<'a> {
    pub fn from_draw(
        command: &DrawCommand,
        stage: [f32; 2],
        transform: [f32; 16],
        time: f32,
        frame_index: u64,
        effect_values: &'a BTreeMap<String, Vec<f32>>,
    ) -> Self {
        let value = |name: &str, index: usize, default: f32| {
            effect_values
                .get(name)
                .and_then(|values| values.get(index))
                .copied()
                .unwrap_or(default)
        };
        let clip = command
            .clip_bounds
            .unwrap_or([0.0, 0.0, stage[0], stage[1]]);
        let wipe = command
            .native_emote
            .map(|material| [material.wipe[0], material.wipe[1], material.wipe[2], 0.0])
            .unwrap_or([0.0; 4]);
        Self {
            transform,
            uv: [
                command.clip.uv_offset[0],
                command.clip.uv_offset[1],
                command.clip.uv_scale[0],
                command.clip.uv_scale[1],
            ],
            clip,
            wipe,
            color: [
                value("colorMultiply", 0, command.color.multiply[0]),
                value("colorMultiply", 1, command.color.multiply[1]),
                value("colorMultiply", 2, command.color.multiply[2]),
                value("alpha", 0, command.opacity),
            ],
            color_flags: [
                command.color.grayscale as u8 as f32,
                command.color.negative as u8 as f32,
                0.0,
                0.0,
            ],
            frame: [stage[0], stage[1], time, frame_index.min(16_777_216) as f32],
            effect_values,
        }
    }
}

impl ShaderReflection {
    pub fn encode_uniforms(&self, parameters: &ShaderRuntimeParameters<'_>) -> Vec<u8> {
        let mut bytes = vec![0; self.uniform_buffer_size as usize];
        for uniform in &self.uniforms {
            let builtin = match uniform.name.as_str() {
                "art3m1s_transform" => Some(parameters.transform.as_slice()),
                "art3m1s_uv" => Some(parameters.uv.as_slice()),
                "art3m1s_clip" => Some(parameters.clip.as_slice()),
                "art3m1s_wipe" => Some(parameters.wipe.as_slice()),
                "art3m1s_color" => Some(parameters.color.as_slice()),
                "art3m1s_color_flags" => Some(parameters.color_flags.as_slice()),
                "art3m1s_frame" => Some(parameters.frame.as_slice()),
                _ => None,
            };
            let values = builtin.or_else(|| {
                parameters
                    .effect_values
                    .get(&uniform.name)
                    .map(Vec::as_slice)
            });
            let Some(values) = values else {
                continue;
            };
            let start = uniform.offset as usize;
            let available = (uniform.size as usize / 4).min(uniform.value_type.value_count());
            for (index, value) in values.iter().copied().take(available).enumerate() {
                let offset = start + index * 4;
                if let Some(target) = bytes.get_mut(offset..offset + 4) {
                    target.copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        bytes
    }
}

/// Shared logical registry. Replacing a name preserves its `ShaderId`, which
/// lets backends invalidate pipelines for exactly one logical shader.
pub struct ShaderRegistry<T> {
    names: HashMap<String, ShaderId>,
    entries: HashMap<ShaderId, T>,
    next_id: u64,
}

impl<T> Default for ShaderRegistry<T> {
    fn default() -> Self {
        Self {
            names: HashMap::new(),
            entries: HashMap::new(),
            next_id: 1,
        }
    }
}

impl<T> ShaderRegistry<T> {
    pub fn insert_or_replace(&mut self, name: &str, value: T) -> (ShaderId, Option<T>) {
        let id = self.names.get(name).copied().unwrap_or_else(|| {
            let id = ShaderId::from_opaque(self.next_id);
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.names.insert(name.to_owned(), id);
            id
        });
        (id, self.entries.insert(id, value))
    }

    pub fn id(&self, name: &str) -> Option<ShaderId> {
        self.names.get(name).copied()
    }

    pub fn get(&self, id: ShaderId) -> Option<&T> {
        self.entries.get(&id)
    }

    pub fn get_by_name(&self, name: &str) -> Option<&T> {
        self.id(name).and_then(|id| self.get(id))
    }

    pub fn remove(&mut self, name: &str) -> Option<(ShaderId, T)> {
        let id = self.names.remove(name)?;
        self.entries.remove(&id).map(|entry| (id, entry))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values()
    }
}

#[cfg(feature = "runtime-shader")]
pub struct ShaderCompiler;

#[cfg(feature = "runtime-shader")]
impl ShaderCompiler {
    pub fn compile_hlsl(name: &str, source: &[u8]) -> Result<CompiledShader, ShaderCompileError> {
        let normalized = normalize_artemis_hlsl(name, source)?;
        let compiler = shaderc::Compiler::new()
            .map_err(|error| ShaderCompileError::new(name, error.to_string()))?;
        let mut options = shaderc::CompileOptions::new()
            .map_err(|error| ShaderCompileError::new(name, error.to_string()))?;
        options.set_source_language(shaderc::SourceLanguage::HLSL);
        options.set_target_env(
            shaderc::TargetEnv::Vulkan,
            shaderc::EnvVersion::Vulkan1_1 as u32,
        );
        options.set_target_spirv(shaderc::SpirvVersion::V1_3);
        options.set_hlsl_io_mapping(true);
        options.set_auto_map_locations(true);
        options.set_hlsl_register_set_and_binding("b0", "0", "0");
        options.set_hlsl_register_set_and_binding("t0", "0", "1");
        options.set_hlsl_register_set_and_binding("t1", "0", "2");
        options.set_hlsl_register_set_and_binding("t2", "0", "3");
        options.set_hlsl_register_set_and_binding("t3", "0", "4");
        options.set_hlsl_register_set_and_binding("s0", "0", "5");
        // Reflection names are part of the runtime ABI. Keep debug names and
        // avoid optimizer-driven member folding; native drivers still optimize
        // the final pipeline representation.
        options.set_generate_debug_info();
        options.set_optimization_level(shaderc::OptimizationLevel::Zero);

        let artifact = compiler
            .compile_into_spirv(
                &normalized,
                shaderc::ShaderKind::Fragment,
                name,
                "art3m1s_fragment",
                Some(&options),
            )
            .map_err(|error| {
                ShaderCompileError::new(name, error.to_string()).with_compiler_location()
            })?;
        let spirv = artifact.as_binary().to_vec();
        let reflection = reflect(name, &spirv)?;
        let (msl, msl_entry_point) = cross_compile_msl(name, &spirv)?;
        Ok(CompiledShader {
            name: name.to_owned(),
            source: ShaderSource::hlsl(source),
            normalized_hlsl: Arc::from(normalized),
            spirv: Arc::from(spirv),
            msl: Arc::from(msl),
            msl_entry_point,
            reflection,
        })
    }
}

#[cfg(feature = "runtime-shader")]
fn reflect(name: &str, spirv: &[u32]) -> Result<ShaderReflection, ShaderCompileError> {
    use spirv_cross2::reflect::{DecorationValue, TypeInner};
    use spirv_cross2::{Compiler, Module, spirv};

    let compiler = Compiler::<spirv_cross2::targets::None>::new(Module::from_words(spirv))
        .map_err(|error| ShaderCompileError::new(name, format!("SPIR-V parse failed: {error}")))?;
    let entry = compiler
        .entry_points()
        .map_err(|error| {
            ShaderCompileError::new(name, format!("entry reflection failed: {error}"))
        })?
        .find(|entry| entry.execution_model == spirv::ExecutionModel::Fragment)
        .ok_or_else(|| {
            ShaderCompileError::new(name, "compiled shader has no fragment entry point")
        })?;
    let entry_point = entry.name.as_ref().to_owned();
    let resources = compiler
        .shader_resources()
        .and_then(|resources| resources.all_resources())
        .map_err(|error| {
            ShaderCompileError::new(name, format!("resource reflection failed: {error}"))
        })?;

    let binding = |id| -> Result<(u32, u32), ShaderCompileError> {
        let literal = |decoration| -> Result<u32, ShaderCompileError> {
            let value = compiler.decoration(id, decoration).map_err(|error| {
                ShaderCompileError::new(
                    name,
                    format!("resource decoration reflection failed: {error}"),
                )
            })?;
            Ok(value
                .and_then(|value| match value {
                    DecorationValue::Literal(value) => Some(value),
                    _ => None,
                })
                .unwrap_or(0))
        };
        Ok((
            literal(spirv::Decoration::DescriptorSet)?,
            literal(spirv::Decoration::Binding)?,
        ))
    };

    let mut reflected_resources = Vec::new();
    let mut uniform_buffer_size = 0;
    let mut uniforms = Vec::new();
    for resource in &resources.uniform_buffers {
        let (set, binding_index) = binding(resource.id)?;
        reflected_resources.push(ShaderResourceBinding {
            name: resource.name.as_ref().to_owned(),
            set,
            binding: binding_index,
            kind: ShaderResourceKind::UniformBuffer,
        });
        if binding_index != 0 || set != 0 {
            continue;
        }
        let description = compiler
            .type_description(resource.base_type_id)
            .map_err(|error| {
                ShaderCompileError::new(name, format!("uniform reflection failed: {error}"))
            })?;
        let TypeInner::Struct(block) = description.inner else {
            return Err(ShaderCompileError::new(
                name,
                "art3m1s uniform binding is not a struct",
            ));
        };
        uniform_buffer_size = u32::try_from(block.size)
            .map_err(|_| ShaderCompileError::new(name, "uniform buffer is too large"))?;
        for member in block.members {
            let Some(member_name) = member.name.as_ref().map(|value| value.as_ref().to_owned())
            else {
                continue;
            };
            let description = compiler.type_description(member.id).map_err(|error| {
                ShaderCompileError::new(name, format!("uniform member reflection failed: {error}"))
            })?;
            let value_type = match description.inner {
                TypeInner::Scalar(_) => ShaderValueType::Float,
                TypeInner::Vector { width: 2, .. } => ShaderValueType::Float2,
                TypeInner::Vector { width: 3, .. } => ShaderValueType::Float3,
                TypeInner::Vector { width: 4, .. } => ShaderValueType::Float4,
                TypeInner::Matrix {
                    columns: 4,
                    rows: 4,
                    ..
                } => ShaderValueType::Float4x4,
                _ => {
                    return Err(ShaderCompileError::new(
                        name,
                        format!("unsupported uniform type for {member_name}"),
                    ));
                }
            };
            uniforms.push(ShaderUniformReflection {
                name: member_name,
                offset: member.offset,
                size: u32::try_from(member.size).unwrap_or(u32::MAX),
                value_type,
            });
        }
    }
    for resource in &resources.separate_images {
        let (set, binding_index) = binding(resource.id)?;
        let texture = match binding_index {
            1 => ShaderTexture::Foreground,
            2 => ShaderTexture::Mask,
            3 => ShaderTexture::User,
            4 => ShaderTexture::Background,
            _ => {
                return Err(ShaderCompileError::new(
                    name,
                    format!("texture binding {set}:{binding_index} is outside the art3m1s ABI"),
                ));
            }
        };
        reflected_resources.push(ShaderResourceBinding {
            name: resource.name.as_ref().to_owned(),
            set,
            binding: binding_index,
            kind: ShaderResourceKind::Texture(texture),
        });
    }
    for resource in &resources.separate_samplers {
        let (set, binding_index) = binding(resource.id)?;
        reflected_resources.push(ShaderResourceBinding {
            name: resource.name.as_ref().to_owned(),
            set,
            binding: binding_index,
            kind: ShaderResourceKind::Sampler,
        });
    }
    reflected_resources.sort_by_key(|resource| (resource.set, resource.binding));
    Ok(ShaderReflection {
        stage: ShaderStage::Fragment,
        entry_point,
        resources: reflected_resources,
        uniform_buffer_size,
        uniforms,
    })
}

#[cfg(feature = "runtime-shader")]
fn cross_compile_msl(name: &str, spirv: &[u32]) -> Result<(String, String), ShaderCompileError> {
    use spirv_cross2::compile::msl::{CompilerOptions, MslVersion};
    use spirv_cross2::{Compiler, Module, spirv};

    let compiler = Compiler::<spirv_cross2::targets::Msl>::new(Module::from_words(spirv))
        .map_err(|error| ShaderCompileError::new(name, format!("SPIR-V parse failed: {error}")))?;
    let mut options = CompilerOptions::default();
    options.version = MslVersion::from((2, 0));
    options.enable_decoration_binding = true;
    let artifact = compiler.compile(&options).map_err(|error| {
        ShaderCompileError::new(name, format!("MSL generation failed: {error}"))
    })?;
    let entry = artifact
        .cleansed_entry_point_name("art3m1s_fragment", spirv::ExecutionModel::Fragment)
        .map_err(|error| {
            ShaderCompileError::new(name, format!("MSL entry reflection failed: {error}"))
        })?
        .map(|value| value.as_ref().to_owned())
        .unwrap_or_else(|| "art3m1s_fragment".to_owned());
    Ok((artifact.as_ref().to_owned(), entry))
}

#[cfg_attr(not(feature = "runtime-shader"), allow(dead_code))]
fn normalize_artemis_hlsl(name: &str, source: &[u8]) -> Result<String, ShaderCompileError> {
    let decoded = String::from_utf8_lossy(source).replace('\r', "");
    if decoded.contains('\0') {
        return Err(ShaderCompileError::new(name, "shader source contains NUL"));
    }
    let stripped = strip_comments(&decoded);
    let ps_start = stripped
        .find("void ps")
        .ok_or_else(|| ShaderCompileError::new(name, "Artemis HLSL has no void ps() function"))?;
    let body_open = stripped[ps_start..]
        .find('{')
        .map(|offset| ps_start + offset)
        .ok_or_else(|| ShaderCompileError::new(name, "Artemis HLSL ps() has no body"))?;
    let body_close = matching_brace(&stripped, body_open)
        .ok_or_else(|| ShaderCompileError::new(name, "Artemis HLSL ps() body is not balanced"))?;
    let globals_end = stripped.find("void vs").unwrap_or(ps_start).min(ps_start);
    let (constants, uniforms) = normalize_globals(name, &stripped[..globals_end])?;
    let body_line = decoded[..body_open + 1]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let body = &decoded[body_open + 1..body_close];

    Ok(format!(
        r#"// art3m1s runtime shader ABI v1
Texture2D<float4> art3m1s_texture_fore : register(t0);
Texture2D<float4> art3m1s_texture_mask : register(t1);
Texture2D<float4> art3m1s_texture_user : register(t2);
Texture2D<float4> art3m1s_texture_back : register(t3);
SamplerState art3m1s_sampler : register(s0);

cbuffer Art3m1sDrawParameters : register(b0) {{
    column_major float4x4 art3m1s_transform;
    float4 art3m1s_uv;
    float4 art3m1s_clip;
    float4 art3m1s_wipe;
    float4 art3m1s_color;
    float4 art3m1s_color_flags;
    float4 art3m1s_frame;
{uniforms}
}};

#define art3m1s_opacity art3m1s_color.w
#define art3m1s_color_multiply art3m1s_color.xyz
#define art3m1s_resolution art3m1s_frame.xy
#define art3m1s_time art3m1s_frame.z
#define art3m1s_frame_index art3m1s_frame.w
#define alpha art3m1s_opacity
#define colorMultiply art3m1s_color_multiply
#define samplerFore art3m1s_texture_fore
#define samplerMask art3m1s_texture_mask
#define samplerUser art3m1s_texture_user
#define samplerBack art3m1s_texture_back
#define tex2D(texture_value, coordinates) texture_value.Sample(art3m1s_sampler, coordinates)

{constants}

struct Art3m1sFragmentInput {{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
    float2 model_position : TEXCOORD1;
}};

float4 art3m1s_fragment(Art3m1sFragmentInput input) : SV_Target0 {{
    float2 texCoord0 = input.uv;
    float2 texCoord1 = input.uv;
    float4 result = float4(0.0, 0.0, 0.0, 0.0);
#line {body_line} "{name}"
{body}
    return result;
}}
"#
    ))
}

#[cfg_attr(not(feature = "runtime-shader"), allow(dead_code))]
fn normalize_globals(name: &str, source: &str) -> Result<(String, String), ShaderCompileError> {
    let mut constants = String::new();
    let mut uniforms = String::new();
    for statement in source.split(';') {
        let statement = statement.trim();
        if statement.is_empty()
            || statement == "}"
            || statement.starts_with("texture ")
            || statement.starts_with("sampler ")
            || statement.contains("sampler_state")
        {
            continue;
        }
        if statement.starts_with("const ") {
            constants.push_str(statement);
            constants.push_str(";\n");
            continue;
        }
        let mut parts = statement.split_whitespace();
        let Some(kind) = parts.next() else { continue };
        let Some(uniform_name) = parts.next() else {
            return Err(ShaderCompileError::new(
                name,
                format!("invalid global declaration: {statement}"),
            ));
        };
        if !matches!(kind, "float" | "float2" | "float3" | "float4") {
            return Err(ShaderCompileError::new(
                name,
                format!("unsupported Artemis global declaration: {statement}"),
            ));
        }
        if matches!(uniform_name, "alpha" | "colorMultiply") {
            continue;
        }
        if !uniform_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(ShaderCompileError::new(
                name,
                format!("invalid uniform name: {uniform_name}"),
            ));
        }
        uniforms.push_str("    ");
        uniforms.push_str(kind);
        uniforms.push(' ');
        uniforms.push_str(uniform_name);
        uniforms.push_str(";\n");
    }
    Ok((constants, uniforms))
}

#[cfg_attr(not(feature = "runtime-shader"), allow(dead_code))]
fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg_attr(not(feature = "runtime-shader"), allow(dead_code))]
fn strip_comments(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        Quoted(u8),
    }

    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut state = State::Code;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match state {
            State::Code if byte == b'/' && next == Some(b'/') => {
                out[index] = b' ';
                out[index + 1] = b' ';
                state = State::LineComment;
                index += 2;
            }
            State::Code if byte == b'/' && next == Some(b'*') => {
                out[index] = b' ';
                out[index + 1] = b' ';
                state = State::BlockComment;
                index += 2;
            }
            State::Code if byte == b'\'' || byte == b'"' => {
                state = State::Quoted(byte);
                index += 1;
            }
            State::LineComment => {
                if byte == b'\n' {
                    state = State::Code;
                } else {
                    out[index] = b' ';
                }
                index += 1;
            }
            State::BlockComment if byte == b'*' && next == Some(b'/') => {
                out[index] = b' ';
                out[index + 1] = b' ';
                state = State::Code;
                index += 2;
            }
            State::BlockComment => {
                if byte != b'\n' {
                    out[index] = b' ';
                }
                index += 1;
            }
            State::Quoted(_quote) if byte == b'\\' && next.is_some() => index += 2,
            State::Quoted(quote) if byte == quote => {
                state = State::Code;
                index += 1;
            }
            State::Quoted(_) | State::Code => index += 1,
        }
    }
    String::from_utf8(out).expect("comment masking preserves UTF-8 outside comments")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEPIA: &str = r#"
texture textureFore;
sampler samplerFore = sampler_state { texture = <textureFore>; };
float alpha;
float red;
const float3 graydata = float3(0.3, 0.6, 0.1);
void vs(float4 position : POSITION) { }
void ps(float2 texCoord0 : TEXCOORD0, float2 texCoord1 : TEXCOORD1,
        out float4 result : COLOR0)
    {
        float4 fore = tex2D(samplerFore, texCoord1);
        float4 back = tex2D(samplerBack, texCoord0);
        float gray = dot(fore.rgb, graydata);
        fore.rgb = float3(gray * red, gray, gray);
        fore.rgb += back.rgb * 0.001;
    fore.a *= alpha;
    result = fore;
}
technique technique0 { }
"#;

    #[test]
    fn normalization_preserves_legacy_names_through_abi_aliases() {
        let normalized = normalize_artemis_hlsl("sepia", SEPIA.as_bytes()).unwrap();
        assert!(normalized.contains("float red;"));
        assert!(normalized.contains("#define samplerFore art3m1s_texture_fore"));
        assert!(normalized.contains("#define alpha art3m1s_opacity"));
        assert!(normalized.contains("float4 fore = tex2D(samplerFore, texCoord1);"));
    }

    #[test]
    fn comment_masking_preserves_byte_offsets_and_quoted_slashes() {
        let source =
            "/* 中文注释 */\nconst char* url = \"https://example.invalid\";\nvoid ps() { }";
        let stripped = strip_comments(source);
        assert_eq!(stripped.len(), source.len());
        assert_eq!(stripped.find("void ps"), source.find("void ps"));
        assert!(stripped.contains("https://example.invalid"));
    }

    #[cfg(feature = "runtime-shader")]
    #[test]
    fn compiler_produces_spirv_msl_and_reflection() {
        let shader = ShaderCompiler::compile_hlsl("sepia", SEPIA.as_bytes()).unwrap();
        assert_eq!(shader.spirv[0], 0x0723_0203);
        assert!(shader.msl.contains("fragment"));
        assert!(shader.reflection.uniform_buffer_size > 0);
        assert!(
            shader
                .reflection
                .uniforms
                .iter()
                .any(|value| value.name == "red"),
            "{:?}",
            shader.reflection
        );
        assert!(shader.reflection.resources.iter().any(|resource| {
            resource.kind == ShaderResourceKind::Texture(ShaderTexture::Foreground)
        }));
        assert!(shader.reflection.resources.iter().any(|resource| {
            resource.kind == ShaderResourceKind::Texture(ShaderTexture::Background)
                && resource.binding == 4
        }));
        assert!(shader.reflection.resources.iter().any(|resource| {
            resource.kind == ShaderResourceKind::Sampler && resource.binding == 5
        }));
    }

    #[test]
    fn registry_preserves_id_across_replacement() {
        let mut registry = ShaderRegistry::default();
        let (first, old) = registry.insert_or_replace("fx", 1);
        assert!(old.is_none());
        let (second, old) = registry.insert_or_replace("fx", 2);
        assert_eq!(first, second);
        assert_eq!(old, Some(1));
        assert_eq!(registry.get(first), Some(&2));
        assert_eq!(registry.remove("fx"), Some((first, 2)));
    }
}
