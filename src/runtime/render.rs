use super::CoreRuntime;
use crate::backend::gl::platform;
use crate::render_pipeline::draw::{Renderer, TextureProvider};
use crate::render_pipeline::RenderPipeline;
use glow::HasContext;

impl CoreRuntime {
    /// 重新创建 FBO 并更新渲染器的 viewport/projection。
    /// 当舞台尺寸改变时调用（例如加载不同分辨率的项目）。
    pub(super) fn resize_stage(&mut self, new_width: u32, new_height: u32) -> Result<(), String> {
        // 删除旧的 FBO 和纹理
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.fbo_tex);
        }

        // 创建新的 FBO
        let (new_fbo, new_fbo_tex) = unsafe {
            platform::create_fbo_target(&self.gl, new_width as i32, new_height as i32)
                .map_err(|e| format!("重新创建 FBO 失败: {e}"))?
        };

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
        let text_for: Option<&dyn Fn(&str) -> Vec<crate::render_pipeline::draw::DrawCommand>> =
            if text_map.is_empty() {
                None
            } else {
                Some(&|layer_id: &str| text_map.get(layer_id).cloned().unwrap_or_default())
            };
        let frame = RenderPipeline::new(&self.compositor)
            .build_composited_with_text(&mut self.texture_provider, text_for);
        self.renderer.render(&frame);
        let mut used_files = self.compositor.scene().collect_files();
        used_files.insert(":text/atlas".to_string());
        used_files.insert("__video_fullscreen__".to_string());
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
