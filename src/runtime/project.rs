use super::CoreRuntime;
use super::callbacks::FfiCallbacks;
use super::magic_path;
use crate::Project;
use crate::backend::gl::GlTextureProvider;
use crate::runtime::save_io;
use crate::text::GlyphTextRenderer;
use asb_interpreter::tags::{ExecutionContext, TagHandler, TagResult};
use asb_interpreter::{CallbackResult, Event};
use std::sync::Arc;

struct RuntimeResetHandler;

impl TagHandler for RuntimeResetHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> asb_interpreter::Result<TagResult> {
        ctx.variables.reset();
        Ok(TagResult::Emit(Event::GoTitle))
    }
}

impl CoreRuntime {
    /// Load a project from an in-memory system.ini string.
    pub fn load_project(&mut self, ini_content: &str, platform: &str) -> Result<(), String> {
        let project =
            Project::open_from_data("", ini_content, platform).map_err(|e| e.to_string())?;

        let new_width = project.config().stage_width;
        let new_height = project.config().stage_height;

        // 如果分辨率改变，重新创建 FBO 和更新渲染器
        if new_width != self.stage_w || new_height != self.stage_h {
            self.resize_stage(new_width, new_height)?;
        }

        self.project_savepath = project.config().savepath.clone();
        self.save_screenshot = None;
        self.interpreter = project.create_interpreter();
        self.interpreter.register_tag("reset", RuntimeResetHandler);

        self.wire_engine_callbacks();
        self.wire_file_loader();
        self.wire_texture_source(ini_content);
        self.wire_event_callback();
        self.load_default_font();
        self.register_builtin_textures();
        self.seed_savepath_and_sysload();
        self.sync_control_status_variables();

        // Boot
        project
            .start_boot(&mut self.interpreter)
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    fn wire_engine_callbacks(&mut self) {
        self.interpreter
            .set_engine_callbacks(Box::new(FfiCallbacks {
                input: Arc::clone(&self.input),
                magic_paths: Arc::clone(&self.magic_paths),
                volumes: Arc::clone(&self.volumes),
                debug_skip_active: Arc::clone(&self.debug_skip_active),
                script_status: Arc::clone(&self.script_status),
            }));
    }

    fn wire_file_loader(&mut self) {
        // Override the file loader with magic-path-aware FFI version.
        // Scripts can reference files via `:name/rest` notation; the
        // default loader (from create_interpreter) doesn't resolve these.
        let magic_paths_loader = Arc::clone(&self.magic_paths);
        self.interpreter
            .set_file_loader(Box::new(move |name: &str| {
                let resolved = magic_path::resolve_path(&magic_paths_loader, name);
                crate::ffi::request_file(&resolved).map_err(|m| {
                    asb_interpreter::Error::IoError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        m,
                    ))
                })
            }));
    }

    fn wire_texture_source(&mut self, ini_content: &str) {
        // Re-create texture provider with magic-path-aware FFI source
        let gl_for_tex = self.gl.clone();
        let project_name = ini_content
            .lines()
            .find(|l| l.starts_with("TITLE="))
            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default();
        let magic_paths_tex = Arc::clone(&self.magic_paths);
        self.texture_provider =
            GlTextureProvider::new(gl_for_tex).with_source(move |name: &str| -> Option<Vec<u8>> {
                let resolved = magic_path::resolve_path(&magic_paths_tex, name);
                for try_path in [format!("{resolved}.png"), resolved.clone()] {
                    match crate::ffi::request_asset(&try_path) {
                        Some(bytes) => {
                            crate::core_debug!(
                                "[{project_name}] TEX HIT: {name} → {try_path} ({})",
                                bytes.len()
                            );
                            return Some(bytes);
                        }
                        None => {
                            crate::core_debug!(
                                "[{project_name}] TEX TRY: {name} → {try_path} MISS"
                            );
                        }
                    }
                }
                crate::core_debug!("[{project_name}] TEX MISS: {name} → {resolved}");
                None
            });
    }

    fn wire_event_callback(&mut self) {
        let events_cb = Arc::clone(&self.events);
        let exit_requested_cb = Arc::clone(&self.exit_requested);
        self.interpreter.set_callback(move |e| {
            if matches!(e, Event::Exit) {
                crate::core_info!("[CoreRuntime] Event::Exit received, setting exit flag");
                exit_requested_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // Only fullscreen videos block script execution. Layer videos are visual effects
            // owned by the scene and may loop indefinitely.
            let pause = event_requires_host_pause(&e);
            events_cb.lock().unwrap().push(e);
            if pause {
                CallbackResult::Pause
            } else {
                CallbackResult::Continue
            }
        });
    }

    fn load_default_font(&mut self) {
        match crate::load_font_ffi("font/sourcehansans-medium.otf") {
            Ok(font) => {
                let mut text = GlyphTextRenderer::new();
                let _ = text.set_font(font);
                self.set_text_renderer(Box::new(text));
            }
            Err(e) => {
                crate::core_warn!("[CoreRuntime] 字体加载失败: {e}");
            }
        }
    }

    fn register_builtin_textures(&mut self) {
        self.texture_provider
            .upload_rgba(":bg/black", 2, 2, &[0, 0, 0, 255].repeat(4));
        self.texture_provider
            .upload_rgba(":bg/white", 2, 2, &[255, 255, 255, 255].repeat(4));
    }

    fn seed_savepath_and_sysload(&mut self) {
        // Seed `s.savepath` —— 真实 Artemis 由引擎按 system.ini 的 SAVEPATH 种入此系统
        // 变量；脚本到处用 `e:var("s.savepath").."/"..file` 拼存档/缩略图路径，且 boot
        // 期间（boot.lua）就会读取它来检测既有存档，故必须在 start_boot 之前种好。
        //
        // 我们把它当作沙箱内的逻辑相对子目录前缀：所有存档路径形如
        // `<savepath>/save0001.dat`，由宿主统一解析到 appSupport 基准下（方案 A1 +
        // save-files-in-app-sandbox）。原始 SAVEPATH 可能含反斜杠/CSIDL（如
        // hamidashi 的 `まどそふと\ハミダシクリエイティブ`），这里规范化为正斜杠
        // 相对路径，缺省退回 `save`。
        let savepath = save_io::sanitize_savepath(self.project_savepath.as_deref());
        self.interpreter.set_variable(
            "s.savepath",
            asb_interpreter::Value::String(savepath.clone()),
        );
        self.savepath = savepath;

        // 读回上次 syssave() 落下的全局/系统域（saveg.dat / system.dat），使
        // boot.lua 期间 system_dataloading() 能拿到既有的 sys/gscr/conf。
        // 必须在 start_boot 之前，且在 s.savepath 种好之后（save_path_for 依赖它）。
        self.sysload();
    }
}

fn event_requires_host_pause(e: &Event) -> bool {
    matches!(
        e,
        Event::Wait { .. } | Event::YesNo { .. } | Event::ShowDialog { .. }
    ) || matches!(e, Event::VideoPlay { id, .. } if id.is_none())
        || matches!(e, Event::Trans { trans_type, .. } if *trans_type != 0)
}

#[cfg(test)]
mod tests {
    use super::{RuntimeResetHandler, event_requires_host_pause};
    use asb_interpreter::{CallbackResult, Event, ExecutionResult, Interpreter};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn go_title_reset_triggers_event() {
        let mut interpreter = Interpreter::new(asb_interpreter::InterpreterConfig::default());
        interpreter.register_tag("reset", RuntimeResetHandler);
        interpreter
            .load_script(
                "test",
                r#"
*main
[reset]
"#,
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let saw_go_title = Arc::new(AtomicBool::new(false));
        let saw_go_title_c = Arc::clone(&saw_go_title);
        interpreter.set_callback(move |event| {
            if matches!(event, Event::GoTitle) {
                saw_go_title_c.store(true, Ordering::SeqCst);
            }
            CallbackResult::Continue
        });
        let result = interpreter.run().unwrap();
        assert!(matches!(
            result,
            ExecutionResult::Completed | ExecutionResult::Wait(_)
        ));
        assert!(saw_go_title.load(Ordering::SeqCst));
    }

    #[test]
    fn layer_video_does_not_pause_script_execution() {
        assert!(!event_requires_host_pause(&Event::VideoPlay {
            id: Some("1.0.effect".into()),
            file: ":ani/snow03.ogv".into(),
            skip: true,
            loop_play: true,
        }));
        assert!(event_requires_host_pause(&Event::VideoPlay {
            id: None,
            file: ":mov/op.ogv".into(),
            skip: true,
            loop_play: false,
        }));
    }
}
