//! 着色器源码与程序构建。
//!
//! 顶点/片元着色器主体与 GL 方言无关；只有 `#version` 头和精度限定符随
//! [`ShaderProfile`] 切换，使同一套着色器既能跑在 ANGLE 的 GLES 上，也能在桌面
//! GL Core 上做离屏验证。

use glow::HasContext;

/// 着色器方言。运行在 ANGLE 上用 [`ShaderProfile::Gles300`]；在没有 ANGLE 的开发
/// 机上做离屏像素测试时用 [`ShaderProfile::GlCore330`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderProfile {
    /// OpenGL ES 3.0（`#version 300 es`）—— ANGLE 目标。
    Gles300,
    /// 桌面 OpenGL 3.3 Core（`#version 330 core`）—— 离屏测试用。
    GlCore330,
}

impl ShaderProfile {
    fn version_header(self) -> &'static str {
        match self {
            ShaderProfile::Gles300 => "#version 300 es\nprecision highp float;\n",
            ShaderProfile::GlCore330 => "#version 330 core\n",
        }
    }
}

const VERTEX_BODY: &str = r#"
layout(location = 0) in vec2 a_pos;   // 单位方块 0..1
layout(location = 1) in vec2 a_uv;

uniform mat3 u_projection;  // 舞台像素 → NDC
uniform mat3 u_transform;   // 图层世界变换（像素空间）
uniform vec2 u_size;        // 绘制区域像素尺寸（裁剪时=clip宽高，否则=纹理尺寸）
uniform vec2 u_uv_offset;   // UV 起点（归一化 0..1）
uniform vec2 u_uv_scale;    // UV 跨度（归一化 0..1）

out vec2 v_uv;

void main() {
    // 单位方块按绘制区域尺寸展开，再过世界变换到舞台像素，最后投影到 NDC。
    vec2 local = a_pos * u_size;
    vec3 world = u_transform * vec3(local, 1.0);
    vec3 ndc = u_projection * vec3(world.xy, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
    // 把 0..1 的顶点 UV 映射到裁剪子区域。
    v_uv = u_uv_offset + a_uv * u_uv_scale;
}
"#;

const FRAGMENT_BODY: &str = r#"
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

/// 编译并链接渲染器用的着色器程序。
///
/// # Safety
/// 需在当前 GL 上下文下调用。
pub unsafe fn build_program(
    gl: &glow::Context,
    profile: ShaderProfile,
) -> Result<glow::Program, String> {
    unsafe {
        let header = profile.version_header();
        let vert_src = format!("{header}{VERTEX_BODY}");
        let frag_src = format!("{header}{FRAGMENT_BODY}");

        let program = gl.create_program()?;

        let shaders = [
            (glow::VERTEX_SHADER, vert_src),
            (glow::FRAGMENT_SHADER, frag_src),
        ];
        let mut compiled = Vec::with_capacity(2);
        for (kind, src) in shaders {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(program);
                return Err(format!("着色器编译失败: {log}"));
            }
            gl.attach_shader(program, shader);
            compiled.push(shader);
        }

        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
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
