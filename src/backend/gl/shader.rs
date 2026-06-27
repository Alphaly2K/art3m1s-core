//! GL shader compilation and program linking.
//!
//! Shader assets live in [`crate::render_pipeline::shader`].  This module only
//! compiles and links the shader program selected by the render pipeline.

use crate::render_pipeline::shader::{BuiltinShaderManager, ShaderManager, ShaderProfile};
use glow::HasContext;

/// 编译并链接渲染器用的着色器程序。
///
/// # Safety
/// 需在当前 GL 上下文下调用。
pub unsafe fn build_program(
    gl: &glow::Context,
    profile: ShaderProfile,
) -> Result<glow::Program, String> {
    unsafe {
        let manager = BuiltinShaderManager;
        let source = manager
            .program(crate::render_pipeline::shader::SPRITE_SHADER)
            .ok_or_else(|| "sprite shader asset missing".to_string())?;
        let header = profile.version_header();
        let vert_src = format!("{header}{}", source.vertex_body);
        let frag_src = format!("{header}{}", source.fragment_body);

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
