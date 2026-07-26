use super::CoreRuntime;
use crate::compositor::CompositorEvent;
use asb_interpreter::Event;
use asb_interpreter::event::{LayerEvent, WaitReason};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

impl CoreRuntime {
    pub(super) fn drain_events(&mut self) -> Vec<Event> {
        let mut events = self.events.lock().unwrap();
        events.drain(..).collect()
    }

    pub(super) fn dispatch_events(&mut self, events: &[Event]) {
        for event in events {
            if matches!(event, Event::Exit) {
                crate::core_info!("[runtime] Event::Exit received");
                self.exit_requested.store(true, Ordering::SeqCst);
            }

            // 存档 / 读档 / 文件删除——通过宿主回调真正落盘（方案 B + A1）
            match event {
                Event::SaveGame { file } => {
                    crate::core_info!("[runtime] Event::SaveGame file={:?}", file);
                    if file.is_empty() {
                        // 不带 file 的 [save] 即 syssave()：持久化全局/系统域到
                        // saveg.dat / system.dat（fileio.lua eqtag{"save"}）。
                        if let Err(e) = self.syssave() {
                            crate::core_error!("[runtime] syssave 失败: {}", e);
                        }
                    } else if let Err(e) = self.handle_save_game(file) {
                        crate::core_error!("[runtime] 保存存档失败 {}: {}", file, e);
                    }
                }
                Event::LoadGame { file, .. } => {
                    crate::core_info!("[runtime] Event::LoadGame file={:?}", file);
                    if file.is_empty() {
                        crate::core_warn!("[runtime] LoadGame 的 file 为空，跳过");
                    } else if let Err(e) = self.handle_load_game(file) {
                        crate::core_error!("[runtime] 读取存档失败 {}: {}", file, e);
                    }
                }
                Event::GoTitle => {
                    crate::core_info!("[runtime] Event::GoTitle");
                    if let Err(e) = self.handle_go_title() {
                        crate::core_error!("[runtime] 返回标题失败: {}", e);
                    }
                }
                Event::FileOperation {
                    command, target, ..
                } if command == "delete" => {
                    crate::core_info!("[runtime] Event::FileOperation delete target={:?}", target);
                    if let Some(t) = target {
                        match self.save_path_for(t) {
                            Ok(path) => match crate::ffi::request_delete(&path) {
                                Ok(()) => {
                                    crate::core_info!("[runtime] 已删除 {}", path);
                                }
                                Err(e) => {
                                    crate::core_warn!("[runtime] 删除文件失败 {}: {}", path, e);
                                }
                            },
                            Err(e) => {
                                crate::core_warn!("[runtime] 删除文件路径非法 {}: {}", t, e);
                            }
                        }
                    }
                }
                Event::FileOperation {
                    command, src, dst, ..
                } if command == "copy" || command == "move" => {
                    crate::core_info!(
                        "[runtime] Event::FileOperation {} src={:?} dst={:?}",
                        command,
                        src,
                        dst
                    );
                    if let (Some(src), Some(dst)) = (src, dst) {
                        if let Err(e) = self.copy_save_file(src, dst, command == "move") {
                            crate::core_warn!(
                                "[runtime] 文件{}失败 {} -> {}: {}",
                                if command == "move" { "移动" } else { "复制" },
                                src,
                                dst,
                                e
                            );
                        }
                    }
                }
                Event::FileOperation { command, .. } => {
                    crate::core_warn!("[runtime] 未实现的 FileOperation 命令被忽略: {}", command);
                }
                Event::TakeScreenshot => {
                    self.capture_save_screenshot();
                }
                Event::AutoModeConfig { allow, layer } => {
                    self.apply_automode_config(*allow, layer.clone());
                }
                Event::SkipConfig { allow, skip_unread } => {
                    self.apply_skip_config(*allow, *skip_unread);
                }
                Event::AutoSkipDisable => {
                    self.disable_auto_skip();
                }
                Event::Exec { command, mode } => {
                    self.apply_exec_command(command, *mode);
                }
                Event::ShowDialog {
                    title,
                    message,
                    varname,
                    textfield,
                    textfield_size,
                } => {
                    self.request_dialog(
                        title,
                        message,
                        varname.as_deref(),
                        textfield.as_deref(),
                        *textfield_size,
                    );
                }
                Event::SaveScreenshot {
                    file,
                    width,
                    height,
                } => {
                    crate::core_info!(
                        "[runtime] Event::SaveScreenshot file={:?} width={:?} height={:?}",
                        file,
                        width,
                        height
                    );
                    if let Err(e) = self.handle_save_screenshot(file, *width, *height) {
                        crate::core_error!("[runtime] 保存缩略图失败 {}: {}", file, e);
                    }
                }
                Event::Custom { tag, params } if tag == "lyshader" => {
                    self.handle_shader_load(params);
                }
                Event::Custom { tag, .. } => {
                    crate::core_debug!("[runtime] 未处理的自定义标签: {tag}");
                }
                Event::ShaderLoad { id, file } => {
                    self.load_shader(id, file);
                }
                // 视频完成处理器登记很少发生且直接影响脚本能否继续，保持 info 级。
                Event::VideoFinishHandler { .. } | Event::VideoFinishHandlerDel => {
                    crate::core_info!("[runtime] {}", event_summary(event));
                }
                _ => {}
            }

            self.apply_media_event(event);
            self.apply_text_event(event);
            if let Some(event) = CompositorEvent::from_interpreter(event) {
                self.compositor.apply_event(event);
                self.sync_layer_info_all();
            }
            crate::core_debug!("[event] {}", event_summary(event));
        }
    }

    fn handle_shader_load(&mut self, params: &HashMap<String, String>) {
        let Some(id) = params.get("id").filter(|id| !id.is_empty()) else {
            crate::core_warn!("[shader] lyshader 缺少 id");
            return;
        };
        let Some(file) = params.get("file").filter(|file| !file.is_empty()) else {
            crate::core_warn!("[shader] lyshader id={} 缺少 file", id);
            return;
        };

        self.load_shader(id, file);
    }

    fn load_shader(&mut self, id: &str, file: &str) {
        let source = match crate::ffi::request_file(file) {
            Ok(source) => source,
            Err(error) => {
                crate::core_warn!("[shader] 读取失败 id={} file={}: {}", id, file, error);
                return;
            }
        };
        match self.renderer.register_hlsl_shader(id, &source) {
            Ok(()) => {
                crate::core_info!("[shader] 已加载 id={} file={}", id, file);
            }
            Err(error) => {
                crate::core_error!("[shader] 编译失败 id={} file={}: {}", id, file, error);
            }
        }
    }

    /// 缓动完成回调派发：把合成器攒下的 [`TweenHandler`] 转成排队标签，
    /// 下一次 `advance_script` 时由解释器执行（enqueue-and-run 模型）。
    pub(super) fn dispatch_tween_handlers(&mut self) {
        for h in self.compositor.poll_tween_events() {
            if h.handler.is_none() && h.file.is_none() && h.label.is_none() {
                // 纯 sync/delete 标记，无回调可派发。
                continue;
            }
            super::input::enqueue_handler_tags(
                &self.interpreter,
                h.handler.as_deref(),
                h.file.as_deref(),
                h.label.as_deref(),
                h.call,
                &HashMap::new(),
                &[],
            );
        }
    }

    pub(super) fn sync_layer_info_all(&self) {
        let mut out = HashMap::new();
        for layer in self.compositor.scene().all_layers() {
            out.insert(layer.id.clone(), layer_info_entry(layer));
        }
        *self.layer_info.lock().unwrap() = out;
    }

    pub(super) fn sync_layer_info(&self, id: &str) {
        let mut table = self.layer_info.lock().unwrap();
        let Some(layer) = self.compositor.scene().get(id) else {
            table.remove(id);
            return;
        };
        table.insert(id.to_string(), layer_info_entry(layer));
    }
}

/// 单个图层暴露给脚本层的几何信息（left/top/width/height 字符串表）。
fn layer_info_entry(layer: &crate::compositor::Layer) -> HashMap<String, String> {
    let (left, top) = layer.props.offset();
    let (width, height) =
        if let (Some(width), Some(height)) = (layer.props.width, layer.props.height) {
            (width, height)
        } else if let Some([_, _, width, height]) = layer.props.clip_rect() {
            (width, height)
        } else {
            (0.0, 0.0)
        };
    HashMap::from([
        ("left".to_string(), trim_layer_float(left)),
        ("top".to_string(), trim_layer_float(top)),
        ("width".to_string(), trim_layer_float(width)),
        ("height".to_string(), trim_layer_float(height)),
    ])
}

fn trim_layer_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        (value as i32).to_string()
    } else {
        value.to_string()
    }
}

/// `[event]` 调试日志的单行摘要：`名称 key=value ...`，长度有上限。
///
/// 高频事件（图层/文本/缓动）用手写的紧凑格式；其余一律回退到截断的
/// Debug 输出——保证每种事件都能看到字段内容，而不是一个裸变体名。
fn event_summary(e: &Event) -> String {
    /// 单行摘要长度上限（字符）。ScenarioText/FontSettings 等可能很长。
    const MAX_CHARS: usize = 200;

    let s = match e {
        Event::Layer(layer_event) => match layer_event {
            LayerEvent::Create { id, file } => format!("LayerCreate id={id} file={file}"),
            LayerEvent::Create2 { id, file, alpha } => {
                format!("LayerCreate2 id={id} file={file} alpha={alpha:?}")
            }
            LayerEvent::Delete { id } => format!("LayerDelete id={id}"),
            LayerEvent::SetProperty {
                id,
                property,
                value,
            } => {
                format!("LayerSetProp id={id} {property}={value}")
            }
            LayerEvent::SetProperties { id, properties } => {
                let mut kv: Vec<String> =
                    properties.iter().map(|(k, v)| format!("{k}={v}")).collect();
                kv.sort();
                format!("LayerSetProps id={id} {}", kv.join(" "))
            }
        },
        Event::LayerTween {
            id, param, to, time, ..
        } => {
            format!("LayerTween id={id} {param}->{to:?} time={time:?}")
        }
        Event::LayerRename { id, to } => format!("LayerRename id={id} -> {to}"),
        Event::Trans {
            trans_type,
            time,
            rule,
            ..
        } => {
            format!("Trans type={trans_type} time={time:?} rule={rule:?}")
        }
        Event::BgmPlay {
            file,
            loop_play,
            gain,
            ..
        } => {
            format!("BgmPlay file={file} loop={loop_play} gain={gain:?}")
        }
        Event::SePlay { id, file, .. } => format!("SePlay id={id} file={file}"),
        Event::VoicePlay { file, gain, .. } => format!("VoicePlay file={file} gain={gain:?}"),
        Event::VideoPlay { id, file, .. } => format!("VideoPlay id={id:?} file={file}"),
        Event::Text { content } => format!("Text {content:?}"),
        Event::ScenarioText { content, inline } => {
            format!("ScenarioText inline={inline} {content:?}")
        }
        Event::FontSettings(s) => format!("FontSettings {}", format_map(s)),
        Event::FontDefault(s) => format!("FontDefault {}", format_map(s)),
        Event::Wait { reason } => match reason {
            WaitReason::Generic => "Wait(Generic)".to_string(),
            WaitReason::Stop { reason } => match reason.as_deref() {
                Some(r) => format!("Wait(Stop:{r})"),
                None => "Wait(Stop)".to_string(),
            },
            WaitReason::Timed {
                milliseconds,
                input,
            } => {
                format!("Wait(Timed time={milliseconds} input={input})")
            }
            reason => format!("Wait({reason:?})"),
        },
        Event::Custom { tag, params } => format!("Custom [{tag}] {}", format_map(params)),
        // 其余低频事件：Debug 输出已含全部字段，直接用（超长由下方统一截断）。
        e => format!("{e:?}"),
    };

    match s.char_indices().nth(MAX_CHARS) {
        Some((byte_idx, _)) => format!("{}…", &s[..byte_idx]),
        None => s,
    }
}

/// 把参数表格式化为稳定有序的 `k=v k=v`（HashMap 迭代序随机，排序保证日志可对比）。
fn format_map(map: &HashMap<String, String>) -> String {
    let mut kv: Vec<String> = map.iter().map(|(k, v)| format!("{k}={v}")).collect();
    kv.sort();
    kv.join(" ")
}
