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
        // 输出视频相关事件日志（始终输出）
        for event in events {
            match event {
                Event::VideoPlay { id, file, .. } => {
                    crate::core_debug!("[runtime] VideoPlay: file={}, id={:?}", file, id);
                }
                Event::VideoFinishHandler {
                    file,
                    label,
                    call,
                    handler,
                } => {
                    crate::core_info!(
                        "[runtime] VideoFinishHandler: file={:?}, label={:?}, call={}, handler={:?}",
                        file,
                        label,
                        call,
                        handler
                    );
                }
                Event::VideoFinishHandlerDel => {
                    crate::core_info!("[runtime] VideoFinishHandlerDel");
                }
                _ => {}
            }
        }

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
                _ => {}
            }

            self.apply_media_event(event);
            self.apply_text_event(event);
            if let Some(event) = CompositorEvent::from_interpreter(event) {
                self.compositor.apply_event(event);
                self.sync_layer_info_all();
            }
            crate::core_debug!("[event] {}", event_name(event));
        }
    }

    pub(super) fn sync_layer_info_all(&self) {
        let mut out = HashMap::new();
        for layer in self.compositor.scene().all_layers() {
            let (left, top) = layer.props.offset();
            let (width, height) =
                if let (Some(width), Some(height)) = (layer.props.width, layer.props.height) {
                    (width, height)
                } else if let Some([_, _, width, height]) = layer.props.clip_rect() {
                    (width, height)
                } else {
                    (0.0, 0.0)
                };
            out.insert(
                layer.id.clone(),
                HashMap::from([
                    ("left".to_string(), trim_layer_float(left)),
                    ("top".to_string(), trim_layer_float(top)),
                    ("width".to_string(), trim_layer_float(width)),
                    ("height".to_string(), trim_layer_float(height)),
                ]),
            );
        }
        *self.layer_info.lock().unwrap() = out;
    }

    pub(super) fn sync_layer_info(&self, id: &str) {
        let mut table = self.layer_info.lock().unwrap();
        let Some(layer) = self.compositor.scene().get(id) else {
            table.remove(id);
            return;
        };
        let (left, top) = layer.props.offset();
        let (width, height) =
            if let (Some(width), Some(height)) = (layer.props.width, layer.props.height) {
                (width, height)
            } else if let Some([_, _, width, height]) = layer.props.clip_rect() {
                (width, height)
            } else {
                (0.0, 0.0)
            };
        table.insert(
            id.to_string(),
            HashMap::from([
                ("left".to_string(), trim_layer_float(left)),
                ("top".to_string(), trim_layer_float(top)),
                ("width".to_string(), trim_layer_float(width)),
                ("height".to_string(), trim_layer_float(height)),
            ]),
        );
    }
}

fn trim_layer_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        (value as i32).to_string()
    } else {
        value.to_string()
    }
}

/// 事件摘要：名称 + 关键参数，供 `[event]` 调试日志使用。
///
/// 返回拥有所有权的 `String`（不再用 `Box::leak`——旧实现对每个 `Wait(Stop)`
/// 事件泄漏一段堆内存）。仅展开常用事件的关键字段，其余只给变体名。
fn event_name(e: &Event) -> String {
    match e {
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
                format!(
                    "LayerSetProps id={id} keys={:?}",
                    properties.keys().collect::<Vec<_>>()
                )
            }
        },
        Event::LayerTween { id, param, .. } => format!("LayerTween id={id} param={param}"),
        Event::LayerTweenDelete { .. } => "LayerTweenDel".to_string(),
        Event::LayerRename { id, to } => format!("LayerRename id={id} -> {to}"),
        Event::LayerEventHandler { .. } => "LayerEvtHandler".to_string(),
        Event::UiTransition(_) => "UiTrans".to_string(),
        Event::Trans {
            trans_type,
            time,
            rule,
            ..
        } => {
            format!("Trans type={trans_type} time={time:?} rule={rule:?}")
        }
        Event::Flip => "Flip".to_string(),
        Event::BgmPlay {
            file,
            loop_play,
            gain,
            ..
        } => {
            format!("BgmPlay file={file} loop={loop_play} gain={gain:?}")
        }
        Event::BgmStop { .. } => "BgmStop".to_string(),
        Event::BgmFade { .. } => "BgmFade".to_string(),
        Event::BgmCrossFade { .. } => "BgmCrossFade".to_string(),
        Event::SePlay { id, file, .. } => format!("SePlay id={id} file={file}"),
        Event::SeStop { .. } => "SeStop".to_string(),
        Event::SeFade { .. } => "SeFade".to_string(),
        Event::VoicePlay { file, .. } => format!("VoicePlay file={file}"),
        Event::StopAllSounds { .. } => "StopAllSounds".to_string(),
        Event::SoundFinishHandler { .. } => "SoundFinishHandler".to_string(),
        Event::SoundFinishHandlerDel { .. } => "SoundFinishHandlerDel".to_string(),
        Event::VideoPlay { id, file, .. } => format!("VideoPlay id={id:?} file={file}"),
        Event::VideoFinishHandler { .. } => "VideoFinishHandler".to_string(),
        Event::VideoFinishHandlerDel => "VideoFinishHandlerDel".to_string(),
        Event::Text { content } => format!("Text {content:?}"),
        Event::ScenarioText { content, inline } => {
            format!("ScenarioText inline={inline} {content:?}")
        }
        Event::LineBreak => "LineBreak".to_string(),
        Event::PageBreak { .. } => "PageBreak".to_string(),
        Event::FontSettings(_) => "FontSettings".to_string(),
        Event::FontClose => "FontClose".to_string(),
        Event::FontDefault(_) => "FontDefault".to_string(),
        Event::FontInit => "FontInit".to_string(),
        Event::MessageLayerSwitch { .. } => "MsgLayerSwitch".to_string(),
        Event::MessageLayerPop => "MsgLayerPop".to_string(),
        Event::Wait { reason } => match reason {
            WaitReason::Generic => "Wait(Generic)".to_string(),
            WaitReason::Stop { reason } => match reason.as_deref() {
                Some(r) => format!("Wait(Stop:{r})"),
                None => "Wait(Stop)".to_string(),
            },
            WaitReason::Timed { .. } => "Wait(Timed)".to_string(),
            WaitReason::KeyWait { .. } => "Wait(KeyWait)".to_string(),
            _ => "Wait".to_string(),
        },
        Event::SaveGame { file } => format!("Save file={file:?}"),
        Event::LoadGame { file, trans_type } => {
            format!("Load file={file:?} type={trans_type:?}")
        }
        Event::FileOperation {
            command,
            src,
            dst,
            target,
        } => {
            format!("FileOp {command} src={src:?} dst={dst:?} target={target:?}")
        }
        Event::SaveScreenshot {
            file,
            width,
            height,
        } => {
            format!("SaveScreenshot file={file:?} {width:?}x{height:?}")
        }
        Event::Exit => "Exit".to_string(),
        Event::GoTitle => "GoTitle".to_string(),
        Event::ShowDialog { .. } => "ShowDialog".to_string(),
        Event::YesNo { .. } => "YesNo".to_string(),
        Event::SceneIn => "SceneIn".to_string(),
        Event::SceneOut => "SceneOut".to_string(),
        Event::AutoModeConfig { allow, layer } => {
            format!("AutoMode allow={allow} layer={layer:?}")
        }
        Event::SkipConfig { allow, skip_unread } => {
            format!("Skip allow={allow} unread={skip_unread}")
        }
        Event::AutoSkipDisable => "AutoSkipDisable".to_string(),
        Event::Exec { command, mode } => format!("Exec command={command} mode={mode:?}"),
        e => {
            crate::core_debug!("[event] {:?}", e);
            "Not implemented event".to_string()
        }
    }
}
