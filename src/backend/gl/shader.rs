//! GL shader compilation and program linking.
//!
//! Semantic shader identities come from [`crate::render_pipeline::shader`];
//! GLSL source and compiler profiles stay private to this backend.

use super::shader_source;
use glow::HasContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderProfile {
    Gles300,
    GlCore330,
}

impl ShaderProfile {
    pub fn version_header(self) -> &'static str {
        match self {
            ShaderProfile::Gles300 => "#version 300 es\nprecision highp float;\n",
            ShaderProfile::GlCore330 => "#version 330 core\n",
        }
    }
}

/// 编译并链接渲染器用的着色器程序。
///
/// # Safety
/// 需在当前 GL 上下文下调用。
pub unsafe fn build_program(
    gl: &glow::Context,
    profile: ShaderProfile,
) -> Result<glow::Program, String> {
    unsafe {
        let source = shader_source::program(crate::render_pipeline::shader::SPRITE_SHADER)
            .ok_or_else(|| "sprite shader asset missing".to_string())?;
        build_program_from_bodies(gl, profile, source.vertex_body, source.fragment_body)
    }
}

pub unsafe fn build_builtin_program(
    gl: &glow::Context,
    profile: ShaderProfile,
    name: &str,
) -> Result<glow::Program, String> {
    let source = shader_source::program(name)
        .ok_or_else(|| format!("built-in shader asset missing: {name}"))?;
    unsafe { build_program_from_bodies(gl, profile, source.vertex_body, source.fragment_body) }
}

pub unsafe fn build_yuv420p_program(
    gl: &glow::Context,
    profile: ShaderProfile,
) -> Result<glow::Program, String> {
    unsafe { build_program_from_bodies(gl, profile, YUV420P_VERTEX_BODY, YUV420P_FRAGMENT_BODY) }
}

pub unsafe fn build_effect_program(
    gl: &glow::Context,
    profile: ShaderProfile,
    hlsl: &[u8],
) -> Result<glow::Program, String> {
    let source = shader_source::program(crate::render_pipeline::shader::SPRITE_SHADER)
        .ok_or_else(|| "sprite shader asset missing".to_string())?;
    let fragment = super::hlsl::translate_effect(hlsl)?;
    unsafe { build_program_from_bodies(gl, profile, source.vertex_body, &fragment) }
}

unsafe fn build_program_from_bodies(
    gl: &glow::Context,
    profile: ShaderProfile,
    vertex_body: &str,
    fragment_body: &str,
) -> Result<glow::Program, String> {
    unsafe {
        let header = profile.version_header();
        let vert_src = format!("{header}{vertex_body}");
        let frag_src = format!("{header}{fragment_body}");
        let program = gl.create_program()?;

        let shaders = [
            (glow::VERTEX_SHADER, vert_src),
            (glow::FRAGMENT_SHADER, frag_src),
        ];
        let mut compiled = Vec::with_capacity(2);
        let cleanup = |gl: &glow::Context, compiled: &[glow::Shader]| {
            for &shader in compiled {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.delete_program(program);
        };
        for (kind, src) in shaders {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                cleanup(gl, &compiled);
                return Err(format!("着色器编译失败: {log}"));
            }
            gl.attach_shader(program, shader);
            compiled.push(shader);
        }

        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            cleanup(gl, &compiled);
            return Err(format!("着色器程序链接失败: {log}"));
        }

        // 链接后即可分离并删除中间 shader 对象。
        for shader in compiled {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }

        Ok(program)
    }
}

const YUV420P_VERTEX_BODY: &str = r#"
out vec2 v_uv;

void main() {
    vec2 uv = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = uv;
    gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const YUV420P_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;

void main() {
    // Software decoders normally emit limited-range BT.601 YUV420P for the
    // WMV/MPEG-4 content used by this engine.
    float y = (texture(u_y, v_uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    float u = texture(u_u, v_uv).r - 0.5;
    float v = texture(u_v, v_uv).r - 0.5;
    float r = y + 1.5748 * v;
    float g = y - 0.1873 * u - 0.4681 * v;
    float b = y + 1.8556 * u;
    frag_color = vec4(clamp(vec3(r, g, b), 0.0, 1.0), 1.0);
}
"#;
