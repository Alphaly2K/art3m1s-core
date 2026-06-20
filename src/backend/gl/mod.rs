//! 基于 glow 的 GLES 渲染后端。
//!
//! 这是合成器 [`crate::compositor::renderer`] 抽象的具体实现。它把每帧的
//! [`DrawList`](crate::compositor::DrawList) 翻译成 GLES 绘制调用：每条
//! [`DrawCommand`](crate::compositor::DrawCommand) 用一个带纹理的四边形画出，
//! 应用世界变换、不透明度、混合模式与颜色滤镜。
//!
//! ## 为什么是 GLES / ANGLE
//!
//! 着色器按 GLES (`#version 300 es`) 编写，正是 ANGLE 暴露的 API；运行在真实
//! ANGLE 上无需改动渲染代码，只需把 EGL/GLES 函数指针从 ANGLE 的
//! `libEGL`/`libGLESv2` 加载进 [`glow::Context`]。为了能在没有独立 ANGLE 库的
//! 开发机上做离屏验证，着色器的 `#version` 头是运行时可切换的（见
//! [`ShaderProfile`]）——桌面 GL Core 与 GLES 的着色器主体完全一致。
//!
//! ## 坐标
//!
//! 合成器在舞台像素坐标系里工作（原点左上、Y 向下）。渲染器通过一个正交投影把
//! 舞台坐标映射到 NDC，因此 [`DrawCommand::transform`] 可以直接当作像素空间的
//! 仿射变换使用。

use crate::compositor::renderer::{BlendMode, DrawCommand, DrawList, Renderer};
use glow::HasContext;
use std::rc::Rc;

pub mod platform;
mod provider;
mod shader;

pub use provider::{AssetSource, GlTextureProvider, PlaceholderKind};
pub use shader::ShaderProfile;

/// GLES 渲染器：持有 GL 程序、四边形几何与舞台尺寸。
///
/// 渲染器借用一个 [`glow::Context`]（用 `Rc` 共享，方便和
/// [`GlTextureProvider`] 共用同一上下文）。它不拥有窗口/EGL 上下文——那由宿主
/// （winit + glutin，或测试里的 CGL 离屏上下文）负责创建并设为当前。
pub struct GlRenderer {
    gl: Rc<glow::Context>,
    program: glow::Program,
    vao: glow::VertexArray,
    #[allow(dead_code)]
    vbo: glow::Buffer,
    stage_width: f32,
    stage_height: f32,
    /// GL 视口的物理像素尺寸。
    ///
    /// 与 [`stage_width`]/[`stage_height`]（游戏设计分辨率，用于投影矩阵）区分开：
    /// 在 Retina/HiDPI 显示器上，窗口可绘制表面的物理像素数是逻辑尺寸乘以缩放因子，
    /// 视口必须用物理像素，否则画面只占左下角并出现拉伸/花屏。默认等于舞台尺寸。
    viewport_width: i32,
    viewport_height: i32,
    // uniform location 缓存
    u_projection: Option<glow::UniformLocation>,
    u_transform: Option<glow::UniformLocation>,
    u_size: Option<glow::UniformLocation>,
    u_uv_offset: Option<glow::UniformLocation>,
    u_uv_scale: Option<glow::UniformLocation>,
    u_opacity: Option<glow::UniformLocation>,
    u_multiply: Option<glow::UniformLocation>,
    u_grayscale: Option<glow::UniformLocation>,
    u_negative: Option<glow::UniformLocation>,
    u_sampler: Option<glow::UniformLocation>,
}

impl GlRenderer {
    /// 用给定的 GL 上下文、舞台尺寸和着色器 profile 创建渲染器。
    ///
    /// # Safety
    /// 调用方必须保证 `gl` 对应的 GL 上下文当前已被设为当前上下文，且在渲染器
    /// 存活期间有效。
    pub fn new(
        gl: Rc<glow::Context>,
        stage_width: u32,
        stage_height: u32,
        profile: ShaderProfile,
    ) -> Result<Self, String> {
        unsafe {
            let program = shader::build_program(&gl, profile)?;

            // 单位四边形，两个三角形，含纹理坐标。布局：x, y, u, v。
            // 顶点位置是 0..1 的单位方块，顶点着色器再乘以 size 与 transform。
            let vertices: [f32; 24] = [
                // pos    // uv
                0.0, 0.0, 0.0, 0.0, //
                1.0, 0.0, 1.0, 0.0, //
                1.0, 1.0, 1.0, 1.0, //
                0.0, 0.0, 0.0, 0.0, //
                1.0, 1.0, 1.0, 1.0, //
                0.0, 1.0, 0.0, 1.0, //
            ];

            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast(&vertices),
                glow::STATIC_DRAW,
            );
            let stride = 4 * std::mem::size_of::<f32>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(
                1,
                2,
                glow::FLOAT,
                false,
                stride,
                2 * std::mem::size_of::<f32>() as i32,
            );
            gl.bind_vertex_array(None);

            let u = |name: &str| gl.get_uniform_location(program, name);
            let renderer = GlRenderer {
                u_projection: u("u_projection"),
                u_transform: u("u_transform"),
                u_size: u("u_size"),
                u_uv_offset: u("u_uv_offset"),
                u_uv_scale: u("u_uv_scale"),
                u_opacity: u("u_opacity"),
                u_multiply: u("u_multiply"),
                u_grayscale: u("u_grayscale"),
                u_negative: u("u_negative"),
                u_sampler: u("u_sampler"),
                gl: gl.clone(),
                program,
                vao,
                vbo,
                stage_width: stage_width as f32,
                stage_height: stage_height as f32,
                // 默认视口等于舞台尺寸；HiDPI 宿主应在拿到可绘制表面后调用
                // [`set_viewport_size`] 传入物理像素尺寸。
                viewport_width: stage_width as i32,
                viewport_height: stage_height as i32,
            };
            Ok(renderer)
        }
    }

    /// 设置 GL 视口的物理像素尺寸（用于 HiDPI/Retina）。
    ///
    /// 投影矩阵仍按舞台设计分辨率工作，因此图层坐标无需改动；这里只调整光栅化
    /// 时映射到帧缓冲的像素范围。宿主应在创建表面后以及每次 resize 后调用。
    pub fn set_viewport_size(&mut self, width: u32, height: u32) {
        self.viewport_width = width as i32;
        self.viewport_height = height as i32;
    }

    /// 把舞台像素坐标映射到 NDC 的正交投影（行主序 3x3，列向量约定）。
    ///
    /// 舞台：x∈[0,W] 映射到 [-1,1]，y∈[0,H] 映射到 [1,-1]（Y 翻转，原点左上）。
    /// 以 `mat3` 形式传入着色器，作用于 `(x, y, 1)`。
    fn projection(&self) -> [f32; 9] {
        let w = self.stage_width;
        let h = self.stage_height;
        // 列主序填充（glUniformMatrix3fv transpose=false 期望列主序）。
        [
            2.0 / w,
            0.0,
            0.0, // col 0
            0.0,
            -2.0 / h,
            0.0, // col 1
            -1.0,
            1.0,
            1.0, // col 2
        ]
    }

    /// 用 alpha 预设之外的混合模式设置 GL 混合状态。
    unsafe fn set_blend(&self, blend: BlendMode) {
        let gl = &self.gl;
        unsafe {
            gl.enable(glow::BLEND);
            match blend {
                BlendMode::Alpha => {
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                }
                BlendMode::Add => {
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE);
                }
                BlendMode::Screen => {
                    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_COLOR);
                }
                BlendMode::Multiply => {
                    gl.blend_func(glow::DST_COLOR, glow::ONE_MINUS_SRC_ALPHA);
                }
            }
        }
    }

    /// 画单条命令。调用方需已 use program / bind vao / 设好投影。
    unsafe fn draw_one(&self, cmd: &DrawCommand) {
        let gl = &self.gl;
        unsafe {
            self.set_blend(cmd.blend);

            // transform: glam::Affine2 → mat3（列主序）。
            let m = cmd.transform.matrix2;
            let t = cmd.transform.translation;
            let transform3: [f32; 9] = [
                m.x_axis.x, m.x_axis.y, 0.0, // col 0
                m.y_axis.x, m.y_axis.y, 0.0, // col 1
                t.x, t.y, 1.0, // col 2
            ];
            gl.uniform_matrix_3_f32_slice(self.u_transform.as_ref(), false, &transform3);
            // 用裁剪后的 quad 尺寸（而不是整张纹理尺寸）展开单位方块。
            gl.uniform_2_f32(
                self.u_size.as_ref(),
                cmd.clip.quad_size[0],
                cmd.clip.quad_size[1],
            );
            // UV 重映射：把 0..1 的顶点 UV 映射到裁剪子区域。
            gl.uniform_2_f32(
                self.u_uv_offset.as_ref(),
                cmd.clip.uv_offset[0],
                cmd.clip.uv_offset[1],
            );
            gl.uniform_2_f32(
                self.u_uv_scale.as_ref(),
                cmd.clip.uv_scale[0],
                cmd.clip.uv_scale[1],
            );
            gl.uniform_1_f32(self.u_opacity.as_ref(), cmd.opacity);
            let c = cmd.color;
            gl.uniform_3_f32(
                self.u_multiply.as_ref(),
                c.multiply[0],
                c.multiply[1],
                c.multiply[2],
            );
            gl.uniform_1_i32(self.u_grayscale.as_ref(), c.grayscale as i32);
            gl.uniform_1_i32(self.u_negative.as_ref(), c.negative as i32);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(
                glow::TEXTURE_2D,
                Some(glow::NativeTexture(
                    std::num::NonZeroU32::new(cmd.texture.0 as u32).expect("texture id 非零"),
                )),
            );
            gl.uniform_1_i32(self.u_sampler.as_ref(), 0);

            gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }
}

impl Renderer for GlRenderer {
    fn render(&mut self, frame: &DrawList) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, self.viewport_width, self.viewport_height);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            let proj = self.projection();
            gl.uniform_matrix_3_f32_slice(self.u_projection.as_ref(), false, &proj);

            for cmd in &frame.commands {
                self.draw_one(cmd);
            }

            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        let gl = &self.gl;
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

/// 把 `&[f32]` 当作字节切片传给 GL，避免引入 bytemuck 依赖。
fn bytemuck_cast(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(data)) }
}
