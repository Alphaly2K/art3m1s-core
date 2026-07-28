//! 基于 glow 的 GLES 渲染后端。
//!
//! 这是渲染管线 draw-list 抽象的具体实现。它把每帧的
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

use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, Renderer, ShaderGroup, TextureId,
    TextureInfo,
};
use glow::HasContext;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;

pub mod platform;
mod provider;
mod shader;

pub use crate::render_pipeline::ShaderProfile;
pub use provider::{AssetSource, GlTextureProvider, PlaceholderKind};

fn next_shader_group(
    frame: &DrawList,
    start: usize,
    end: usize,
    group_limit: usize,
) -> Option<(usize, ShaderGroup)> {
    frame
        .shader_groups
        .iter()
        .enumerate()
        .take(group_limit)
        .filter(|(_, group)| group.start == start && group.end > start && group.end <= end)
        .max_by_key(|(group_index, group)| (group.end, *group_index))
        .map(|(group_index, group)| (group_index, group.clone()))
}

fn shader_group_blend(group: &ShaderGroup) -> BlendMode {
    if group.effect.name != crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER {
        return BlendMode::PremultipliedAlpha;
    }
    let mode = group
        .effect
        .uniforms
        .get("blendMode")
        .and_then(|values| values.first())
        .copied()
        .unwrap_or(0.0) as i32;
    match mode {
        1 => BlendMode::PremultipliedAdd,
        2 => BlendMode::Screen,
        3 => BlendMode::Multiply,
        _ => BlendMode::PremultipliedAlpha,
    }
}

/// GLES 渲染器：持有 GL 程序、四边形几何与舞台尺寸。
///
/// 渲染器借用一个 [`glow::Context`]（用 `Rc` 共享，方便和
/// [`GlTextureProvider`] 共用同一上下文）。它不拥有窗口/EGL 上下文——那由宿主
/// （winit + glutin，或测试里的 CGL 离屏上下文）负责创建并设为当前。
pub struct GlRenderer {
    gl: Rc<glow::Context>,
    program: glow::Program,
    program_bindings: ProgramBindings,
    custom_programs: HashMap<String, CustomProgram>,
    profile: ShaderProfile,
    white_texture: glow::Texture,
    transparent_texture: glow::Texture,
    group_targets: Vec<GroupTarget>,
    mask_group_targets: Vec<GroupTarget>,
    vao: glow::VertexArray,
    #[allow(dead_code)]
    vbo: glow::Buffer,
    mesh_vao: glow::VertexArray,
    mesh_vbo: glow::Buffer,
    stage_width: f32,
    stage_height: f32,
    /// GL 视口的物理像素尺寸。
    ///
    /// 与 [`stage_width`]/[`stage_height`]（游戏设计分辨率，用于投影矩阵）区分开：
    /// 在 Retina/HiDPI 显示器上，窗口可绘制表面的物理像素数是逻辑尺寸乘以缩放因子，
    /// 视口必须用物理像素，否则画面只占左下角并出现拉伸/花屏。默认等于舞台尺寸。
    viewport_width: i32,
    viewport_height: i32,
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
            let program_bindings = ProgramBindings::new(&gl, program);
            let white_texture = create_solid_texture(&gl, [255, 255, 255, 255])?;
            let transparent_texture = create_solid_texture(&gl, [0, 0, 0, 0])?;
            let alpha_mask_program = shader::build_builtin_program(
                &gl,
                profile,
                crate::render_pipeline::shader::ALPHA_MASK_SHADER,
            )?;
            let mut custom_programs = HashMap::new();
            custom_programs.insert(
                crate::render_pipeline::shader::ALPHA_MASK_SHADER.to_owned(),
                CustomProgram {
                    program: alpha_mask_program,
                    bindings: ProgramBindings::new(&gl, alpha_mask_program),
                },
            );
            let group_composite_program = shader::build_builtin_program(
                &gl,
                profile,
                crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER,
            )?;
            custom_programs.insert(
                crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER.to_owned(),
                CustomProgram {
                    program: group_composite_program,
                    bindings: ProgramBindings::new(&gl, group_composite_program),
                },
            );
            // [trans type=2] 规则图像转场内置 shader，与 alpha-mask 同样在
            // 初始化时编译并注册到 custom_programs。
            let rule_trans_program = shader::build_builtin_program(
                &gl,
                profile,
                crate::render_pipeline::shader::RULE_TRANS_SHADER,
            )?;
            custom_programs.insert(
                crate::render_pipeline::shader::RULE_TRANS_SHADER.to_owned(),
                CustomProgram {
                    program: rule_trans_program,
                    bindings: ProgramBindings::new(&gl, rule_trans_program),
                },
            );

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

            let mesh_vao = gl.create_vertex_array()?;
            let mesh_vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(mesh_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh_vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, 0, glow::DYNAMIC_DRAW);
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

            let renderer = GlRenderer {
                gl: gl.clone(),
                program,
                program_bindings,
                custom_programs,
                profile,
                white_texture,
                transparent_texture,
                group_targets: Vec::new(),
                mask_group_targets: Vec::new(),
                vao,
                vbo,
                mesh_vao,
                mesh_vbo,
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

    /// 获取当前视口宽度。
    pub fn viewport_width(&self) -> u32 {
        self.viewport_width as u32
    }

    /// 获取当前视口高度。
    pub fn viewport_height(&self) -> u32 {
        self.viewport_height as u32
    }

    /// 设置舞台设计分辨率（用于投影矩阵）。
    ///
    /// 当加载不同分辨率的项目时调用，更新投影矩阵以匹配新的舞台尺寸。
    pub fn set_stage_size(&mut self, width: u32, height: u32) {
        self.stage_width = width as f32;
        self.stage_height = height as f32;
    }

    /// Register one Artemis `[lyshader]` HLSL effect under its script id.
    pub fn register_hlsl_shader(&mut self, name: &str, source: &[u8]) -> Result<(), String> {
        let program = unsafe { shader::build_effect_program(&self.gl, self.profile, source)? };
        let bindings = ProgramBindings::new(&self.gl, program);
        if let Some(old) = self
            .custom_programs
            .insert(name.to_string(), CustomProgram { program, bindings })
        {
            unsafe {
                self.gl.delete_program(old.program);
            }
        }
        Ok(())
    }

    /// 确保 `targets[depth]` 存在且尺寸匹配，返回其 FBO 与颜色纹理。
    /// 尺寸变化时销毁重建。
    unsafe fn ensure_target_at(
        gl: &glow::Context,
        targets: &mut Vec<GroupTarget>,
        depth: usize,
        width: i32,
        height: i32,
    ) -> Result<(glow::Framebuffer, glow::Texture), String> {
        while targets.len() <= depth {
            let (framebuffer, texture) = unsafe { platform::create_fbo_target(gl, width, height)? };
            targets.push(GroupTarget {
                framebuffer,
                texture,
                width,
                height,
            });
        }

        if targets[depth].width != width || targets[depth].height != height {
            let old = targets[depth];
            unsafe {
                gl.delete_framebuffer(old.framebuffer);
                gl.delete_texture(old.texture);
            }
            let (framebuffer, texture) = unsafe { platform::create_fbo_target(gl, width, height)? };
            targets[depth] = GroupTarget {
                framebuffer,
                texture,
                width,
                height,
            };
        }
        let target = targets[depth];
        Ok((target.framebuffer, target.texture))
    }

    unsafe fn ensure_group_target(
        &mut self,
        depth: usize,
    ) -> Result<(glow::Framebuffer, glow::Texture), String> {
        unsafe {
            Self::ensure_target_at(
                &self.gl,
                &mut self.group_targets,
                depth,
                self.stage_width as i32,
                self.stage_height as i32,
            )
        }
    }

    unsafe fn ensure_mask_group_target(
        &mut self,
        depth: usize,
    ) -> Result<(glow::Framebuffer, glow::Texture), String> {
        unsafe {
            Self::ensure_target_at(
                &self.gl,
                &mut self.mask_group_targets,
                depth,
                self.stage_width as i32,
                self.stage_height as i32,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn render_range(
        &mut self,
        frame: &DrawList,
        mesh_ranges: &[Option<(i32, i32)>],
        mask_mesh_ranges: &[Option<(i32, i32)>],
        start: usize,
        end: usize,
        group_limit: usize,
        depth: usize,
        viewport: (i32, i32),
        top_left_target: bool,
    ) {
        let mut index = start;
        while index < end {
            let group = next_shader_group(frame, index, end, group_limit);

            let Some((group_index, group)) = group else {
                unsafe {
                    self.draw_one(
                        &frame.commands[index],
                        mesh_ranges[index],
                        top_left_target,
                        viewport,
                    );
                }
                index += 1;
                continue;
            };

            let parent_framebuffer = framebuffer_from_raw(unsafe {
                self.gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING)
            });
            let group_target = unsafe { self.ensure_group_target(depth) };
            let Ok((group_framebuffer, group_texture)) = group_target else {
                crate::core_warn!(
                    "shader 组 FBO 分配失败，整组按无效果直绘: {}",
                    group_target.unwrap_err()
                );
                unsafe {
                    self.render_range(
                        frame,
                        mesh_ranges,
                        mask_mesh_ranges,
                        group.start,
                        group.end,
                        group_index,
                        depth,
                        viewport,
                        top_left_target,
                    );
                }
                index = group.end;
                continue;
            };
            unsafe {
                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(group_framebuffer));
                self.gl
                    .viewport(0, 0, self.stage_width as i32, self.stage_height as i32);
                self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
                self.gl.clear(glow::COLOR_BUFFER_BIT);
                self.render_range(
                    frame,
                    mesh_ranges,
                    mask_mesh_ranges,
                    group.start,
                    group.end,
                    group_index,
                    depth + 1,
                    (self.stage_width as i32, self.stage_height as i32),
                    false,
                );

                let mut effect = group.effect.clone();
                if let Some([mask_start, mask_end]) = group.mask_range
                    && let Some((mask_framebuffer, mask_texture)) = self
                        .ensure_mask_group_target(depth)
                        .map_err(|e| {
                            crate::core_warn!("mask FBO 分配失败，该组遮罩不生效: {e}");
                        })
                        .ok()
                {
                    self.gl
                        .bind_framebuffer(glow::FRAMEBUFFER, Some(mask_framebuffer));
                    self.gl
                        .viewport(0, 0, self.stage_width as i32, self.stage_height as i32);
                    self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
                    self.gl.clear(glow::COLOR_BUFFER_BIT);
                    for mask_index in mask_start..mask_end {
                        self.draw_one(
                            &frame.mask_commands[mask_index],
                            mask_mesh_ranges[mask_index],
                            false,
                            (self.stage_width as i32, self.stage_height as i32),
                        );
                    }
                    effect.mask_texture = Some(TextureId(mask_texture.0.get() as u64));
                }

                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, parent_framebuffer);
                self.gl.viewport(0, 0, viewport.0, viewport.1);
                self.draw_one(
                    &DrawCommand {
                        texture: TextureId(group_texture.0.get() as u64),
                        size: TextureInfo {
                            width: self.stage_width as u32,
                            height: self.stage_height as u32,
                        },
                        transform: glam::Affine2::IDENTITY,
                        opacity: 1.0,
                        // Rendering into a transparent group target produces
                        // premultiplied RGB. Composite that target without
                        // multiplying its alpha a second time.
                        blend: shader_group_blend(&group),
                        color: ColorFilter::default(),
                        clip: ClipRect {
                            uv_offset: [0.0, 0.0],
                            uv_scale: [1.0, 1.0],
                            quad_size: [self.stage_width, self.stage_height],
                        },
                        clip_bounds: group.clip_bounds,
                        shader: Some(effect),
                        mesh: None,
                        stencil: None,
                    },
                    None,
                    top_left_target,
                    viewport,
                );
            }
            index = group.end;
        }
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

    fn texture_target_projection(&self) -> [f32; 9] {
        let w = self.stage_width;
        let h = self.stage_height;
        [
            2.0 / w,
            0.0,
            0.0, // col 0
            0.0,
            2.0 / h,
            0.0, // col 1
            -1.0,
            -1.0,
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
                    gl.blend_func_separate(
                        glow::SRC_ALPHA,
                        glow::ONE_MINUS_SRC_ALPHA,
                        glow::ONE,
                        glow::ONE_MINUS_SRC_ALPHA,
                    );
                }
                BlendMode::PremultipliedAlpha => {
                    gl.blend_func_separate(
                        glow::ONE,
                        glow::ONE_MINUS_SRC_ALPHA,
                        glow::ONE,
                        glow::ONE_MINUS_SRC_ALPHA,
                    );
                }
                BlendMode::PremultipliedAdd => {
                    gl.blend_func(glow::ONE, glow::ONE);
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
    unsafe fn draw_one(
        &self,
        cmd: &DrawCommand,
        mesh_range: Option<(i32, i32)>,
        top_left_target: bool,
        viewport: (i32, i32),
    ) {
        let gl = &self.gl;
        unsafe {
            let custom_program = cmd
                .shader
                .as_ref()
                .and_then(|effect| self.custom_programs.get(&effect.name));
            let (program, bindings) = custom_program
                .map(|custom| (custom.program, &custom.bindings))
                .unwrap_or((self.program, &self.program_bindings));
            gl.use_program(Some(program));
            gl.uniform_matrix_3_f32_slice(
                bindings.projection.as_ref(),
                false,
                &if top_left_target {
                    self.projection()
                } else {
                    self.texture_target_projection()
                },
            );

            if let Some([x, y, w, h]) = cmd.clip_bounds {
                if w <= 0.0 || h <= 0.0 {
                    return;
                }
                let sx = viewport.0 as f32 / self.stage_width;
                let sy = viewport.1 as f32 / self.stage_height;
                let left = (x * sx).floor() as i32;
                let bottom = if top_left_target {
                    ((self.stage_height - (y + h)) * sy).floor() as i32
                } else {
                    (y * sy).floor() as i32
                };
                let width = (w * sx).ceil().max(1.0) as i32;
                let height = (h * sy).ceil().max(1.0) as i32;
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(left, bottom, width, height);
            } else {
                gl.disable(glow::SCISSOR_TEST);
            }

            self.set_blend(cmd.blend);

            // transform: glam::Affine2 → mat3（列主序）。
            let m = cmd.transform.matrix2;
            let t = cmd.transform.translation;
            let transform3: [f32; 9] = [
                m.x_axis.x, m.x_axis.y, 0.0, // col 0
                m.y_axis.x, m.y_axis.y, 0.0, // col 1
                t.x, t.y, 1.0, // col 2
            ];
            gl.uniform_matrix_3_f32_slice(bindings.transform.as_ref(), false, &transform3);
            let has_mesh = mesh_range.is_some();
            // Mesh positions are already in local pixels. The quad path keeps
            // using unit vertices expanded by the clipped sprite dimensions.
            gl.uniform_2_f32(
                bindings.size.as_ref(),
                if has_mesh { 1.0 } else { cmd.clip.quad_size[0] },
                if has_mesh { 1.0 } else { cmd.clip.quad_size[1] },
            );
            // UV 重映射：把 0..1 的顶点 UV 映射到裁剪子区域。
            gl.uniform_2_f32(
                bindings.uv_offset.as_ref(),
                cmd.clip.uv_offset[0],
                cmd.clip.uv_offset[1],
            );
            gl.uniform_2_f32(
                bindings.uv_scale.as_ref(),
                cmd.clip.uv_scale[0],
                cmd.clip.uv_scale[1],
            );
            gl.uniform_1_f32(bindings.opacity.as_ref(), cmd.opacity);
            let c = cmd.color;
            gl.uniform_3_f32(
                bindings.multiply.as_ref(),
                c.multiply[0],
                c.multiply[1],
                c.multiply[2],
            );
            gl.uniform_1_i32(bindings.grayscale.as_ref(), c.grayscale as i32);
            gl.uniform_1_i32(bindings.negative.as_ref(), c.negative as i32);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, texture_from_id(cmd.texture));

            if custom_program.is_some() {
                gl.uniform_1_i32(bindings.texture_fore.as_ref(), 0);
                let mask_texture = cmd
                    .shader
                    .as_ref()
                    .and_then(|effect| effect.mask_texture)
                    .and_then(texture_from_id)
                    .unwrap_or(self.white_texture);
                bind_texture(gl, 1, mask_texture);
                gl.uniform_1_i32(bindings.texture_mask.as_ref(), 1);
                bind_texture(gl, 2, self.transparent_texture);
                gl.uniform_1_i32(bindings.texture_back.as_ref(), 2);

                let user_texture = cmd
                    .shader
                    .as_ref()
                    .and_then(|effect| effect.user_texture)
                    .and_then(texture_from_id)
                    .unwrap_or(self.transparent_texture);
                bind_texture(gl, 3, user_texture);
                gl.uniform_1_i32(bindings.texture_user.as_ref(), 3);

                gl.uniform_1_f32(bindings.alpha.as_ref(), cmd.opacity);
                gl.uniform_3_f32(
                    bindings.color_multiply.as_ref(),
                    c.multiply[0],
                    c.multiply[1],
                    c.multiply[2],
                );
                if let Some(effect) = &cmd.shader {
                    for (name, values) in &effect.uniforms {
                        gl.uniform_1_f32_slice(
                            gl.get_uniform_location(program, name).as_ref(),
                            values,
                        );
                    }
                }
            } else {
                gl.uniform_1_i32(bindings.sampler.as_ref(), 0);
            }

            if let Some((first, count)) = mesh_range {
                gl.bind_vertex_array(Some(self.mesh_vao));
                gl.draw_arrays(glow::TRIANGLES, first, count);
                gl.bind_vertex_array(Some(self.vao));
            } else {
                gl.draw_arrays(glow::TRIANGLES, 0, 6);
            }
        }
    }
}

impl Renderer for GlRenderer {
    fn render(&mut self, frame: &DrawList) {
        let gl = self.gl.clone();
        unsafe {
            gl.viewport(0, 0, self.viewport_width, self.viewport_height);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            let mut mesh_vertices = Vec::new();
            let mesh_ranges = frame
                .commands
                .iter()
                .map(|command| {
                    command
                        .mesh
                        .as_ref()
                        .filter(|mesh| !mesh.vertices.is_empty())
                        .map(|mesh| {
                            let first = mesh_vertices.len() as i32;
                            mesh_vertices.extend_from_slice(&mesh.vertices);
                            (first, mesh.vertices.len() as i32)
                        })
                })
                .collect::<Vec<_>>();
            let mask_mesh_ranges = frame
                .mask_commands
                .iter()
                .map(|command| {
                    command
                        .mesh
                        .as_ref()
                        .filter(|mesh| !mesh.vertices.is_empty())
                        .map(|mesh| {
                            let first = mesh_vertices.len() as i32;
                            mesh_vertices.extend_from_slice(&mesh.vertices);
                            (first, mesh.vertices.len() as i32)
                        })
                })
                .collect::<Vec<_>>();
            if !mesh_vertices.is_empty() {
                let floats = std::slice::from_raw_parts(
                    mesh_vertices.as_ptr() as *const f32,
                    mesh_vertices.len() * 4,
                );
                gl.bind_vertex_array(Some(self.mesh_vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.mesh_vbo));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck_cast(floats),
                    glow::DYNAMIC_DRAW,
                );
            }
            gl.bind_vertex_array(Some(self.vao));

            self.render_range(
                frame,
                &mesh_ranges,
                &mask_mesh_ranges,
                0,
                frame.commands.len(),
                frame.shader_groups.len(),
                0,
                (self.viewport_width, self.viewport_height),
                true,
            );

            gl.disable(glow::SCISSOR_TEST);
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
            for program in self.custom_programs.values() {
                gl.delete_program(program.program);
            }
            gl.delete_texture(self.white_texture);
            gl.delete_texture(self.transparent_texture);
            for target in &self.group_targets {
                gl.delete_framebuffer(target.framebuffer);
                gl.delete_texture(target.texture);
            }
            for target in &self.mask_group_targets {
                gl.delete_framebuffer(target.framebuffer);
                gl.delete_texture(target.texture);
            }
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_vertex_array(self.mesh_vao);
            gl.delete_buffer(self.mesh_vbo);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GroupTarget {
    framebuffer: glow::Framebuffer,
    texture: glow::Texture,
    width: i32,
    height: i32,
}

struct CustomProgram {
    program: glow::Program,
    bindings: ProgramBindings,
}

struct ProgramBindings {
    projection: Option<glow::UniformLocation>,
    transform: Option<glow::UniformLocation>,
    size: Option<glow::UniformLocation>,
    uv_offset: Option<glow::UniformLocation>,
    uv_scale: Option<glow::UniformLocation>,
    opacity: Option<glow::UniformLocation>,
    multiply: Option<glow::UniformLocation>,
    grayscale: Option<glow::UniformLocation>,
    negative: Option<glow::UniformLocation>,
    sampler: Option<glow::UniformLocation>,
    texture_back: Option<glow::UniformLocation>,
    texture_fore: Option<glow::UniformLocation>,
    texture_mask: Option<glow::UniformLocation>,
    texture_user: Option<glow::UniformLocation>,
    alpha: Option<glow::UniformLocation>,
    color_multiply: Option<glow::UniformLocation>,
}

impl ProgramBindings {
    fn new(gl: &glow::Context, program: glow::Program) -> Self {
        let get = |name| unsafe { gl.get_uniform_location(program, name) };
        Self {
            projection: get("u_projection"),
            transform: get("u_transform"),
            size: get("u_size"),
            uv_offset: get("u_uv_offset"),
            uv_scale: get("u_uv_scale"),
            opacity: get("u_opacity"),
            multiply: get("u_multiply"),
            grayscale: get("u_grayscale"),
            negative: get("u_negative"),
            sampler: get("u_sampler"),
            texture_back: get("u_texture_back"),
            texture_fore: get("u_texture_fore"),
            texture_mask: get("u_texture_mask"),
            texture_user: get("u_texture_user"),
            alpha: get("alpha"),
            color_multiply: get("colorMultiply"),
        }
    }
}

fn texture_from_id(id: crate::render_pipeline::draw::TextureId) -> Option<glow::Texture> {
    NonZeroU32::new(id.0 as u32).map(glow::NativeTexture)
}

fn framebuffer_from_raw(raw: i32) -> Option<glow::Framebuffer> {
    NonZeroU32::new(raw as u32).map(glow::NativeFramebuffer)
}

unsafe fn bind_texture(gl: &glow::Context, unit: u32, texture: glow::Texture) {
    unsafe {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    }
}

unsafe fn create_solid_texture(gl: &glow::Context, rgba: [u8; 4]) -> Result<glow::Texture, String> {
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            1,
            1,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&rgba)),
        );
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(texture)
    }
}

/// 把 `&[f32]` 当作字节切片传给 GL，避免引入 bytemuck 依赖。
fn bytemuck_cast(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(data)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_pipeline::draw::ShaderEffect;
    use std::collections::BTreeMap;

    fn shader_group(start: usize, end: usize, name: &str) -> ShaderGroup {
        ShaderGroup {
            start,
            end,
            effect: ShaderEffect {
                name: name.to_owned(),
                uniforms: BTreeMap::new(),
                mask_texture: None,
                user_texture: None,
            },
            clip_bounds: None,
            mask_range: None,
        }
    }

    #[test]
    fn group_composite_uses_premultiplied_blend_variant() {
        let mut group = shader_group(0, 1, crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER);
        assert_eq!(shader_group_blend(&group), BlendMode::PremultipliedAlpha);
        group.effect.uniforms.insert("blendMode".into(), vec![1.0]);
        assert_eq!(shader_group_blend(&group), BlendMode::PremultipliedAdd);
    }

    #[test]
    fn identical_shader_ranges_descend_in_build_order() {
        let mut frame = DrawList::new();
        frame.push_shader_group(shader_group(0, 1, "inner"));
        frame.push_shader_group(shader_group(0, 1, "middle"));
        frame.push_shader_group(shader_group(0, 1, "outer"));

        let (outer_index, outer) =
            next_shader_group(&frame, 0, 1, frame.shader_groups.len()).unwrap();
        let (middle_index, middle) = next_shader_group(&frame, 0, 1, outer_index).unwrap();
        let (inner_index, inner) = next_shader_group(&frame, 0, 1, middle_index).unwrap();

        assert_eq!(outer.effect.name, "outer");
        assert_eq!(middle.effect.name, "middle");
        assert_eq!(inner.effect.name, "inner");
        assert_eq!(inner_index, 0);
        assert!(next_shader_group(&frame, 0, 1, inner_index).is_none());
    }
}
