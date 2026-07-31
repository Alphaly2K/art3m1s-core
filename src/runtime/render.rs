use super::CoreRuntime;
use crate::backend::gl::platform;
use crate::render_pipeline::RenderPipeline;
use crate::render_pipeline::draw::{DrawList, Renderer, TextureProvider};
use asb_interpreter::event::WaitReason;
use glow::HasContext;

impl CoreRuntime {
    /// 重新创建 FBO 并更新渲染器的 viewport/projection。
    /// 当舞台尺寸改变时调用（例如加载不同分辨率的项目）。
    pub(super) fn resize_stage(&mut self, new_width: u32, new_height: u32) -> Result<(), String> {
        // 先建新 FBO 再删旧的：创建失败时保留可用的旧目标，不留悬空句柄。
        let (new_fbo, new_fbo_tex) = unsafe {
            platform::create_fbo_target(&self.gl, new_width as i32, new_height as i32)
                .map_err(|e| format!("重新创建 FBO 失败: {e}"))?
        };

        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.fbo_tex);
        }

        self.fbo = new_fbo;
        self.fbo_tex = new_fbo_tex;
        self.stage_w = new_width;
        self.stage_h = new_height;

        // 更新渲染器的 viewport 和 projection
        self.renderer.set_viewport_size(new_width, new_height);
        self.renderer.set_stage_size(new_width, new_height);
        self.last_rendered_scene = None;
        self.last_submitted_frame = None;

        Ok(())
    }

    /// Renders the current logical scene into the persistent internal FBO.
    /// Returns false when the visual frame is identical to the submitted one.
    pub(super) fn render_current_frame(&mut self) -> bool {
        // 绑定 FBO，渲染到纹理而不是默认帧缓冲
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        }

        // 转场捕获：在渲染新帧前，若合成器需要捕捉旧画面，则从当前 FBO 读取
        let pipeline = RenderPipeline::new(&self.compositor);
        if pipeline.needs_trans_capture() {
            let pixels = unsafe {
                platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32)
            };
            pipeline.capture_trans_texture(
                &pixels,
                self.stage_w,
                self.stage_h,
                &mut self.texture_provider,
            );
        }

        // backlog / message-tags 查询快照：每帧从 text_renderer 抽取再现标签刷新，
        // 供解释器 var system=get_backlog_* / get_message_tags 的宿主查询钩子读取。
        self.sync_backlog_snapshot();

        // glyph 点击等待图标：进入点击等待（Generic/Generic0）且文本已全部显出时，
        // 把等待图标图层移动到最后一个字符旁并显示；否则隐藏。每帧驱动，避免依赖
        // script.rs 的 wait 建立/退出路径（那不在本任务白名单内）。
        self.drive_click_wait_icon();

        let (frame, _, _) = self.build_bound_scene(true, None);
        let texture_revision = self.texture_provider.content_revision();
        if !frame_requires_render(
            self.last_submitted_frame.as_ref(),
            self.last_submitted_texture_revision,
            &frame,
            texture_revision,
        ) {
            unsafe {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
            return false;
        }

        self.renderer.render(&frame);
        self.last_submitted_frame = Some(frame);
        self.last_submitted_texture_revision = texture_revision;
        self.last_rendered_scene = Some(self.compositor.scene_snapshot());
        self.last_rendered_clock_ms = self.compositor.clock_ms();

        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        true
    }

    pub(super) fn read_current_frame_into(&mut self, out_pixels: &mut [u8]) -> usize {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        }
        let written = unsafe {
            platform::read_pixels_into(
                &self.gl,
                self.stage_w as i32,
                self.stage_h as i32,
                out_pixels,
            )
        };

        // 解绑 FBO
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }

        written
    }

    /// 用上一帧场景重建转场源画面。
    ///
    /// 图像层来自上一帧，因此仍能正常淡出；文本命令则按当前合成器状态生成，
    /// 这样脚本在 `[trans]` 前隐藏/删除消息层时，旧剧情文字不会被烘进源纹理。
    pub(super) fn refresh_transition_source_frame(&mut self) {
        let Some(scene) = self.last_rendered_scene.clone() else {
            // 首帧尚无场景快照时保留 FBO 原内容，沿用原有捕获行为。
            return;
        };
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        }
        let (frame, text_layers, text_commands) =
            self.build_bound_scene(false, Some((&scene, self.last_rendered_clock_ms)));
        self.renderer.render(&frame);
        // The FBO now contains a reconstructed transition source rather than
        // the frame represented by `last_submitted_frame`.
        self.last_submitted_frame = None;
        crate::core_debug!(
            "[runtime] transition source snapshot text_layers={} text_commands={}",
            text_layers,
            text_commands
        );
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }

    fn build_bound_scene(
        &mut self,
        include_transition: bool,
        scene_snapshot: Option<(&crate::compositor::Scene, u64)>,
    ) -> (DrawList, usize, usize) {
        let text_map = self.build_text_commands();
        let text_layer_count = text_map.len();
        let text_command_count = text_map.values().map(Vec::len).sum();
        let (mut emote_map, emote_files) = self.build_emote_commands();
        let has_emote_commands = !emote_map.is_empty();
        let has_text_commands = !text_map.is_empty();
        let mut content_source = |layer_id: &str| emote_map.remove(layer_id).unwrap_or_default();
        let mut text_map = text_map;
        let mut text_source = |layer_id: &str| text_map.remove(layer_id).unwrap_or_default();
        let content_for: Option<&mut crate::render_pipeline::LayerDrawSource<'_>> =
            has_emote_commands.then_some(&mut content_source);
        let text_for: Option<&mut crate::render_pipeline::LayerDrawSource<'_>> =
            has_text_commands.then_some(&mut text_source);
        let pipeline = RenderPipeline::new(&self.compositor);
        let mut frame = if let Some((scene, clock_ms)) = scene_snapshot {
            pipeline.build_scene_with_content(
                scene,
                clock_ms,
                &mut self.texture_provider,
                content_for,
                text_for,
            )
        } else if include_transition {
            pipeline.build_composited_with_content(
                &mut self.texture_provider,
                content_for,
                text_for,
            )
        } else {
            pipeline.build_with_content(&mut self.texture_provider, content_for, text_for)
        };
        frame.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        let mut used_files = scene_snapshot
            .map(|(scene, _)| scene.collect_files())
            .unwrap_or_else(|| self.compositor.scene().collect_files());
        // 文本 atlas 不在场景树里，显式保活防止被 retain 驱逐。
        // 视频图层纹理无需保活：播放期间 set_layer_file 把它挂在场景树上。
        if let Some(renderer) = self.text_renderer.as_ref() {
            used_files.extend(renderer.retained_texture_names());
        }
        used_files.extend(emote_files);
        for f in RenderPipeline::new(&self.compositor).retained_files() {
            used_files.insert(f);
        }
        self.texture_provider.retain(&used_files);
        (frame, text_layer_count, text_command_count)
    }

    /// 每帧驱动 glyph 点击等待图标的显隐。
    ///
    /// 仅在处于点击等待（[wt]/[wt0] → `WaitReason::Generic`/`Generic0`）且当前页
    /// 文本已逐字显出完毕时显示图标；退出等待或文本仍在逐字时隐藏。位置与图层由
    /// 文本子系统的 `click_wait_icon_placement` 决定（[glyph] 未配置图标图层则不显）。
    ///
    /// page_end 判定：解释器目前不区分行末/页末等待（两者都是 Generic），此处一律
    /// 按行末处理（page_end=false，用 [glyph] 的 layer）。精确的页末检测需解释器透传
    /// rp 换页信号，见任务 skipped 说明。
    fn drive_click_wait_icon(&mut self) {
        let show =
            wait_reason_is_click_wait(self.wait_reason.as_ref()) && self.is_text_reveal_complete();
        if show {
            self.enter_click_wait_icon(false);
        } else {
            self.exit_click_wait_icon();
        }
    }
}

/// 是否处于点击等待（行末/页末点击继续）。[wt]/[wt0] 建立 Generic/Generic0；
/// 定时/停止/媒体同步类等待不算点击等待，不驱动等待图标。
fn wait_reason_is_click_wait(reason: Option<&WaitReason>) -> bool {
    matches!(
        reason,
        Some(WaitReason::Generic) | Some(WaitReason::Generic0)
    )
}

fn frame_requires_render(
    previous: Option<&DrawList>,
    previous_texture_revision: u64,
    current: &DrawList,
    current_texture_revision: u64,
) -> bool {
    previous != Some(current) || previous_texture_revision != current_texture_revision
}

#[cfg(test)]
mod tests {
    use super::{frame_requires_render, wait_reason_is_click_wait};
    use crate::render_pipeline::draw::DrawList;
    use asb_interpreter::event::WaitReason;

    #[test]
    fn click_wait_covers_generic_variants_only() {
        assert!(wait_reason_is_click_wait(Some(&WaitReason::Generic)));
        assert!(wait_reason_is_click_wait(Some(&WaitReason::Generic0)));
        assert!(!wait_reason_is_click_wait(None));
        assert!(!wait_reason_is_click_wait(Some(&WaitReason::Timed {
            milliseconds: 100,
            input: 1,
        })));
        assert!(!wait_reason_is_click_wait(Some(&WaitReason::Stop {
            reason: None,
        })));
        assert!(!wait_reason_is_click_wait(Some(&WaitReason::KeyWait {
            buttons: vec![],
        })));
    }

    #[test]
    fn unchanged_draw_list_and_textures_skip_rendering() {
        let frame = DrawList::default();
        assert!(frame_requires_render(None, 0, &frame, 0));
        assert!(!frame_requires_render(Some(&frame), 7, &frame, 7));
        assert!(frame_requires_render(Some(&frame), 7, &frame, 8));
    }
}
