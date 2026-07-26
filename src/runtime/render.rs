use super::CoreRuntime;
use crate::backend::gl::platform;
use crate::render_pipeline::RenderPipeline;
use crate::render_pipeline::draw::{Renderer, TextureProvider};
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

        Ok(())
    }

    pub(super) fn render_current_frame(&mut self) -> Vec<u8> {
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

        let text_map = self.build_text_commands();
        let (emote_map, emote_files) = self.build_emote_commands();
        let content_for: Option<&crate::render_pipeline::LayerDrawSource<'_>> =
            if emote_map.is_empty() {
                None
            } else {
                Some(&|layer_id: &str| emote_map.get(layer_id).cloned().unwrap_or_default())
            };
        let text_for: Option<&crate::render_pipeline::LayerDrawSource<'_>> = if text_map.is_empty()
        {
            None
        } else {
            Some(&|layer_id: &str| text_map.get(layer_id).cloned().unwrap_or_default())
        };
        let mut frame = RenderPipeline::new(&self.compositor).build_composited_with_content(
            &mut self.texture_provider,
            content_for,
            text_for,
        );
        frame.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        self.renderer.render(&frame);
        let mut used_files = self.compositor.scene().collect_files();
        // 文本 atlas 不在场景树里，显式保活防止被 retain 驱逐。
        // 视频图层纹理无需保活：播放期间 set_layer_file 把它挂在场景树上。
        used_files.insert(crate::text::glyph::ATLAS_NAME.to_string());
        used_files.extend(emote_files);
        for f in RenderPipeline::new(&self.compositor).retained_files() {
            used_files.insert(f);
        }
        self.texture_provider.retain(&used_files);
        unsafe {
            self.gl.finish();
        }

        // 从 FBO 读取像素（使用 glReadPixels，对所有后端都可靠）
        let pixels =
            unsafe { platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32) };

        // 解绑 FBO
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }

        pixels
    }
}
