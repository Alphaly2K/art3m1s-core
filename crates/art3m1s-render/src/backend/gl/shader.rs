//! GL shader compilation and program linking.
//!
//! Semantic shader identities come from [`crate::shader`];
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
        let source = shader_source::program(crate::shader::SPRITE_SHADER)
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
    let source = shader_source::program(crate::shader::SPRITE_SHADER)
        .ok_or_else(|| "sprite shader asset missing".to_string())?;
    let fragment = super::hlsl::translate_effect(hlsl)?;
    unsafe { build_program_from_bodies(gl, profile, source.vertex_body, &fragment) }
}

pub unsafe fn build_glsl_effect_program(
    gl: &glow::Context,
    profile: ShaderProfile,
    source: &[u8],
) -> Result<glow::Program, String> {
    let decoded = String::from_utf8_lossy(source);
    let fragment = decoded.trim_start_matches('\u{feff}');
    let normalized;
    let fragment = if fragment.contains('\r') {
        normalized = fragment.replace("\r\n", "\n").replace('\r', "\n");
        &normalized
    } else {
        fragment
    };
    let vertex = shader_source::program(crate::shader::SPRITE_SHADER)
        .ok_or_else(|| "sprite shader asset missing".to_string())?;
    let gles100 = "#version 100\nprecision highp float;\n";
    let start = skip_glsl_comments(fragment);
    let Some(version) = source_version(start) else {
        if profile == ShaderProfile::Gles300 {
            let with_precision;
            let fragment = if start.starts_with('#') {
                fragment
            } else {
                with_precision = format!("precision mediump float;\n#line 1\n{fragment}");
                &with_precision
            };
            return unsafe {
                link_program(gl, &format!("{gles100}{}", vertex.vertex_body), fragment)
            };
        }
        return unsafe { build_program_from_bodies(gl, profile, vertex.vertex_body, fragment) };
    };
    let header = match (
        profile,
        version
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        (ShaderProfile::Gles300, ["100"]) => gles100,
        (ShaderProfile::Gles300, ["300", "es"]) => ShaderProfile::Gles300.version_header(),
        (ShaderProfile::GlCore330, ["150"] | ["150", "core"]) => "#version 150 core\n",
        (ShaderProfile::GlCore330, ["330"] | ["330", "core"]) => {
            ShaderProfile::GlCore330.version_header()
        }
        _ => {
            return Err(format!(
                "GLSL #version {version} is unsupported for {profile:?}"
            ));
        }
    };
    unsafe { link_program(gl, &format!("{header}{}", vertex.vertex_body), fragment) }
}

fn skip_glsl_comments(mut source: &str) -> &str {
    loop {
        source = source.trim_start();
        if let Some(rest) = source.strip_prefix("//") {
            source = rest.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(rest) = source.strip_prefix("/*") {
            let Some((_, tail)) = rest.split_once("*/") else {
                return source;
            };
            source = tail;
        } else {
            return source;
        }
    }
}

fn source_version(source: &str) -> Option<&str> {
    let directive = source.strip_prefix('#')?.trim_start();
    let line = directive.lines().next()?;
    let rest = line.strip_prefix("version")?;
    rest.as_bytes().first()?.is_ascii_whitespace().then(|| {
        rest.split("//")
            .next()
            .unwrap_or_default()
            .split("/*")
            .next()
            .unwrap_or_default()
            .trim()
    })
}

unsafe fn build_program_from_bodies(
    gl: &glow::Context,
    profile: ShaderProfile,
    vertex_body: &str,
    fragment_body: &str,
) -> Result<glow::Program, String> {
    let header = profile.version_header();
    unsafe {
        link_program(
            gl,
            &format!("{header}{vertex_body}"),
            &format!("{header}{fragment_body}"),
        )
    }
}

unsafe fn link_program(
    gl: &glow::Context,
    vertex: &str,
    fragment: &str,
) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;

        let shaders = [
            (glow::VERTEX_SHADER, vertex),
            (glow::FRAGMENT_SHADER, fragment),
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
            let shader = match gl.create_shader(kind) {
                Ok(shader) => shader,
                Err(error) => {
                    cleanup(gl, &compiled);
                    return Err(error);
                }
            };
            gl.shader_source(shader, src);
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

        gl.bind_attrib_location(program, 0, "a_pos");
        gl.bind_attrib_location(program, 1, "a_uv");
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
