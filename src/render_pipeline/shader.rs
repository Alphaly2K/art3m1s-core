//! Shader assets and render-pass shader selection.
//!
//! This module is deliberately above concrete GL/Metal/Vulkan backends.  The
//! render pipeline owns the choice of shader assets and pass layout; backends
//! only compile/link a selected shader for their API.

/// Shader dialect used by a backend compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderProfile {
    /// OpenGL ES 3.0 (`#version 300 es`) for ANGLE/GLES targets.
    Gles300,
    /// Desktop OpenGL 3.3 Core for offscreen validation.
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

/// Backend-independent source bundle for a linked shader program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderProgramSource {
    pub name: &'static str,
    pub vertex_body: &'static str,
    pub fragment_body: &'static str,
}

/// Render-pipeline owned shader asset resolver.
pub trait ShaderManager {
    fn program(&self, name: &str) -> Option<ShaderProgramSource>;
}

/// Built-in shaders used by the default sprite pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinShaderManager;

impl ShaderManager for BuiltinShaderManager {
    fn program(&self, name: &str) -> Option<ShaderProgramSource> {
        match name {
            SPRITE_SHADER => Some(ShaderProgramSource {
                name: SPRITE_SHADER,
                vertex_body: SPRITE_VERTEX_BODY,
                fragment_body: SPRITE_FRAGMENT_BODY,
            }),
            _ => None,
        }
    }
}

pub const SPRITE_SHADER: &str = "sprite";

const SPRITE_VERTEX_BODY: &str = r#"
layout(location = 0) in vec2 a_pos;   // unit quad 0..1
layout(location = 1) in vec2 a_uv;

uniform mat3 u_projection;  // stage pixels -> NDC
uniform mat3 u_transform;   // layer world transform in stage pixels
uniform vec2 u_size;        // drawn quad size in pixels
uniform vec2 u_uv_offset;   // normalized UV origin
uniform vec2 u_uv_scale;    // normalized UV span

out vec2 v_uv;

void main() {
    vec2 local = a_pos * u_size;
    vec3 world = u_transform * vec3(local, 1.0);
    vec3 ndc = u_projection * vec3(world.xy, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
    v_uv = u_uv_offset + a_uv * u_uv_scale;
}
"#;

const SPRITE_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_sampler;
uniform float u_opacity;
uniform vec3 u_multiply;
uniform int u_grayscale;
uniform int u_negative;

void main() {
    vec4 c = texture(u_sampler, v_uv);
    c.rgb *= u_multiply;
    if (u_grayscale != 0) {
        float g = dot(c.rgb, vec3(0.299, 0.587, 0.114));
        c.rgb = vec3(g);
    }
    if (u_negative != 0) {
        c.rgb = vec3(1.0) - c.rgb;
    }
    c.a *= u_opacity;
    frag_color = c;
}
"#;
