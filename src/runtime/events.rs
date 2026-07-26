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
                Event::LoadGame { file, trans_type } => {
                    crate::core_info!(
                        "[runtime] Event::LoadGame file={:?} trans_type={:?}",
                        file,
                        trans_type
                    );
                    if file.is_empty() {
                        crate::core_warn!("[runtime] LoadGame 的 file 为空，跳过");
                    } else if let Err(e) = self.handle_load_game(file, *trans_type) {
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
                Event::FileOperation { command, .. } if command == "clear_cache" => {
                    // [file command=clear_cache]：打包文件（pfs）由宿主资源回调
                    // 持有与缓存，核心转发命令让宿主清缓存。
                    crate::core_info!("[runtime] FileOperation clear_cache → 转发宿主");
                    crate::ffi::emit_ui_command("file_clear_cache", serde_json::json!({}));
                }
                Event::FileOperation {
                    command,
                    url,
                    baseurl,
                    list,
                    ..
                } if command == "wasm_sync" => {
                    // [file command=wasm_sync]：仅 WebAssembly 平台需要同步远端文件到
                    // 本地持久存储（IndexedDB）。桌面/移动宿主收到后可忽略。
                    crate::core_info!("[runtime] FileOperation wasm_sync → 转发宿主");
                    crate::ffi::emit_ui_command(
                        "file_wasm_sync",
                        serde_json::json!({ "url": url, "baseurl": baseurl, "list": list }),
                    );
                }
                Event::FileOperation { command, .. } => {
                    crate::core_warn!("[runtime] 未实现的 FileOperation 命令被忽略: {}", command);
                }
                Event::TakeScreenshot => {
                    self.capture_save_screenshot();
                }
                // stopbyclick/stopbystop 消费见 advance_wait_state；syncse（等 SE
                // 播完再自动前进）依赖 SE 完成时序，暂随事件透传未消费。
                Event::AutoModeConfig {
                    allow,
                    layer,
                    stopbyclick,
                    stopbystop,
                    syncse,
                } => {
                    self.apply_automode_config(
                        *allow,
                        layer.clone(),
                        *stopbyclick,
                        *stopbystop,
                        syncse.clone(),
                    );
                }
                // [alreadyread]：已读/未读判定开关（已读记录由引擎跟踪剧情文本行）。
                Event::AlreadyReadConfig { mode } => self.apply_alreadyread(*mode),
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
                // ── 转发宿主的窗口/系统 UI 命令（payload 风格与 dialog_show 一致）──
                Event::Caption { data } => {
                    // [caption]：设置窗口标题栏字符串，由 Flutter 宿主落实。
                    crate::ffi::emit_ui_command("caption", serde_json::json!({ "data": data }));
                }
                Event::MouseConfig {
                    left,
                    top,
                    hide,
                    autohide,
                } => {
                    // [mouse]：移动/隐藏光标与 autohide 计时都属宿主职责；
                    // 缺省参数按 None 序列化为 null，宿主"保持当前设置"。
                    crate::ffi::emit_ui_command(
                        "mouse",
                        serde_json::json!({
                            "left": left,
                            "top": top,
                            "hide": hide,
                            "autohide": autohide,
                        }),
                    );
                }
                Event::OpenBrowser { url } => {
                    // [openbrowser]：宿主用系统默认浏览器打开（Flutter url_launcher）。
                    crate::ffi::emit_ui_command("openbrowser", serde_json::json!({ "url": url }));
                }
                Event::StatusBar { visible } => {
                    // [statusbar]：iOS/Android 状态栏显隐（SystemChrome）。
                    crate::ffi::emit_ui_command(
                        "statusbar",
                        serde_json::json!({ "visible": visible }),
                    );
                }
                Event::Vibrate { time } => {
                    // [vibrate]：触发设备振动（HapticFeedback / vibration 插件）。
                    crate::ffi::emit_ui_command("vibrate", serde_json::json!({ "time": time }));
                }
                // ── 脚本日志配置与输出 ──
                Event::DebugConfig { mode, level } => {
                    crate::ffi::set_script_debug_config(*mode, *level);
                    crate::core_info!(
                        "[debug] mode={} level={}",
                        crate::ffi::script_debug_mode(),
                        crate::ffi::script_debug_level()
                    );
                }
                Event::DebugPrint { level, data } => {
                    // [debugprint]：按 [debug] 设定的 mode/level 门控后经宿主日志输出。
                    if crate::ffi::script_debug_print_allowed(*level) {
                        crate::core_info!("[debugprint] {}", data);
                    }
                }
                // ── 图层强制拖动 [lydrag] ──
                Event::LayerDrag { id } => {
                    self.force_pointer_drag(id);
                }
                // ── [lytween sync=1]：脚本须等该图层缓动全部结束 ──
                Event::LayerTween { id, sync: true, .. } => {
                    // 解释器对 LayerTween 不暂停（event_requires_host_pause 不含它），
                    // 这里用 wait_reason 挡住后续帧的推进；已有等待时不覆盖——脚本
                    // 本就停着，缓动等待只会更弱。恢复见 dispatch_tween_handlers。
                    if self.wait_reason.is_none() {
                        self.wait_reason = Some(sync_tween_wait_reason(id));
                        self.reset_control_wait_flags();
                    }
                }
                // 视频完成处理器登记很少发生且直接影响脚本能否继续，保持 info 级。
                Event::VideoFinishHandler { .. } | Event::VideoFinishHandlerDel { .. } => {
                    crate::core_info!("[runtime] {}", event_summary(event));
                }
                // ── 模式状态机配置 ──
                Event::HideConfig { allow, window } => {
                    self.apply_hide_config(*allow, window.as_deref());
                }
                Event::RightClickConfig { allow, file } => {
                    self.apply_rclick_config(*allow, file.as_deref());
                }
                Event::AutoSaveConfig { allow } => {
                    crate::core_info!("[runtime] autosave allow={allow}");
                    self.apply_autosave_config(*allow);
                }
                Event::KeyConfig(params) => {
                    self.apply_keyconfig(params);
                }
                // 跟踪活动消息层：hide 模式据此确定要隐藏的消息窗图层；同时
                // 暴露 s.current_message_layer 供脚本查询（system_variables.md）。
                Event::MessageLayerSwitch { id: Some(id), .. } => {
                    self.control.active_message_layer = Some(id.clone());
                    self.interpreter.set_variable(
                        "s.current_message_layer",
                        asb_interpreter::Value::String(id.clone()),
                    );
                }
                Event::MessageLayerPop => {
                    // 回退后活动层不可知（消息层堆栈在文本子系统内部），置空即可：
                    // hide 只会少隐藏一层，不会误藏。s.current_message_layer 同样
                    // 无从得知精确 id，清空为空串（脚本读到空串即"回到未知/默认层"）。
                    self.control.active_message_layer = None;
                    self.interpreter.set_variable(
                        "s.current_message_layer",
                        asb_interpreter::Value::String(String::new()),
                    );
                }
                // ── 宿主转发族 ──
                Event::AvoidConfig { file, windowbutton } => {
                    // [avoid] 仅配置：存下覆盖图与 windowbutton，等 keyconfig
                    // role 15（紧急回避开始）按键触发时才显示覆盖 + 静音。
                    self.apply_avoid_config(file.as_deref(), *windowbutton);
                }
                Event::CallNative {
                    result,
                    module,
                    method,
                    param,
                } => {
                    self.handle_call_native(
                        result.as_deref(),
                        module.as_deref(),
                        method,
                        param.as_deref(),
                    );
                }
                Event::Purchase {
                    purchase,
                    varname,
                    productid,
                    restore,
                    key,
                    sku,
                    consume,
                } => {
                    self.handle_purchase(
                        *purchase,
                        varname.as_deref(),
                        productid.as_deref(),
                        *restore,
                        key.as_deref(),
                        sku.as_deref(),
                        *consume,
                    );
                }
                Event::HttpGet {
                    url,
                    headers,
                    varname_code,
                    varname_data,
                    filename,
                } => {
                    self.handle_http_request(
                        "get",
                        url,
                        headers,
                        &[],
                        &[],
                        varname_code.clone(),
                        varname_data.clone(),
                        filename.clone(),
                    );
                }
                Event::HttpPost {
                    url,
                    headers,
                    data,
                    file_data,
                    varname_code,
                    varname_data,
                    filename,
                } => {
                    self.handle_http_request(
                        "post",
                        url,
                        headers,
                        data,
                        file_data,
                        varname_code.clone(),
                        varname_data.clone(),
                        filename.clone(),
                    );
                }
                Event::DebugReload => {
                    self.handle_debug_reload();
                }
                Event::Reset => {
                    crate::core_info!("[runtime] Event::Reset — 引擎全量重启");
                    if let Err(e) = self.handle_engine_reset() {
                        crate::core_error!("[runtime] 引擎重启失败: {}", e);
                    }
                }
                _ => {}
            }

            self.apply_media_event(event);
            if let Some(pending) = self.apply_text_event(event) {
                self.begin_text_translation(pending);
            }
            // 只有真正送入文本渲染器后才算本帧展示过剧情文本。
            if matches!(event, Event::ScenarioText { .. }) {
                self.scenario_text_shown = true;
            }
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
        // [lytween sync=1] 的恢复条件：目标图层的缓动全部结束（gc 后 tweens
        // 清空）或图层已被删除。只清 wait_reason、不 advance_line——该等待
        // 是派发侧补设的，解释器并未真正停在 lytween 行上。
        let sync_layer = self
            .wait_reason
            .as_ref()
            .and_then(sync_tween_wait_layer)
            .map(str::to_string);
        if let Some(id) = sync_layer {
            if sync_tween_finished(&self.compositor, &id) {
                self.wait_reason = None;
            }
        }

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

    // ── callnative / purchase：经 ui_command 转发宿主，失败兜底 ──────

    /// [callnative]：核心无原生调用能力，经 ui_command 转发 Flutter 宿主。
    /// 宿主完成后经 `art3m1s_runtime_set_string_variable` 把返回字符串写回
    /// result 变量；宿主未注册时按失败分支立即把 result 置空串，脚本不读 nil。
    fn handle_call_native(
        &mut self,
        result: Option<&str>,
        module: Option<&str>,
        method: &str,
        param: Option<&str>,
    ) {
        if let Some(result) = result.filter(|name| !name.is_empty()) {
            // 先落失败缺省值；宿主异步回写成功值时覆盖。
            self.interpreter
                .set_variable(result, asb_interpreter::Value::String(String::new()));
        }
        if !crate::ffi::ui_command_callback_registered() {
            crate::core_warn!("[callnative] 宿主未注册 UI 回调，按失败分支跳过: {method}");
            return;
        }
        crate::ffi::emit_ui_command(
            "callnative",
            serde_json::json!({
                "result": result,
                "module": module,
                "method": method,
                "param": param,
            }),
        );
    }

    /// [purchase]：应用内购买经 ui_command 转发宿主（in_app_purchase 通道）。
    /// 宿主把结果（含 .title/.price 等子键）经 set_string_variable 逐项回注；
    /// 宿主未注册时按失败分支把 varname 置 -1。
    #[allow(clippy::too_many_arguments)]
    fn handle_purchase(
        &mut self,
        purchase: bool,
        varname: Option<&str>,
        productid: Option<&str>,
        restore: bool,
        key: Option<&str>,
        sku: Option<&str>,
        consume: bool,
    ) {
        if !crate::ffi::ui_command_callback_registered() {
            crate::core_warn!("[purchase] 宿主未注册 UI 回调，按失败分支处理");
            if let Some(varname) = varname.filter(|name| !name.is_empty()) {
                // -1 作为"渠道不可用"的结果码，脚本可据此提示购买失败。
                self.interpreter
                    .set_variable(varname, asb_interpreter::Value::Int(-1));
            }
            return;
        }
        crate::ffi::emit_ui_command(
            "purchase",
            serde_json::json!({
                "purchase": purchase,
                "varname": varname,
                "productid": productid,
                "restore": restore,
                "key": key,
                "sku": sku,
                "consume": consume,
            }),
        );
    }

    // ── httpget / httppost：挂起-恢复协议 ─────────────────────────────

    /// 发起 HTTP 请求：经 ui_command 转发宿主并挂起脚本推进；宿主未注册时
    /// 按失败分支立即落结果变量（code=0）不挂起。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_http_request(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        data: &[(String, String)],
        file_data: &[(String, String)],
        varname_code: Option<String>,
        varname_data: Option<String>,
        filename: Option<String>,
    ) {
        if self.control.pending_http.is_some() {
            crate::core_warn!("[http] 上一个请求尚未完成即收到新请求，旧请求按失败结束");
            self.finish_http_request(0, &[]);
        }

        if !crate::ffi::ui_command_callback_registered() {
            crate::core_warn!("[http] 宿主未注册 UI 回调，{method} {url} 按失败分支立即恢复");
            self.control.pending_http = Some(super::control::PendingHttp {
                varname_code,
                varname_data,
                filename,
                serial: self.control.http_serial,
            });
            self.finish_http_request(0, &[]);
            return;
        }

        self.control.http_serial = self.control.http_serial.wrapping_add(1);
        let serial = self.control.http_serial;
        // 清掉上一轮的取消标记，脚本每帧处理里置 s.http.cancel=1 可中断本次请求。
        self.interpreter
            .set_variable("s.http.cancel", asb_interpreter::Value::Int(0));
        self.control.pending_http = Some(super::control::PendingHttp {
            varname_code,
            varname_data,
            filename,
            serial,
        });
        crate::ffi::emit_ui_command(
            "http_request",
            serde_json::json!({
                "serial": serial,
                "method": method,
                "url": url,
                "headers": headers,
                "data": data,
                // 文件型 POST 值：宿主按路径读取内容作为对应 key 的值
                "file_data": file_data,
            }),
        );
    }

    /// 宿主回填 HTTP 结果（FFI 入口）。返回是否有挂起请求被完成。
    pub fn submit_http_result(&mut self, status_code: i32, body: &[u8]) -> bool {
        if self.control.pending_http.is_none() {
            crate::core_warn!("[http] 收到结果时没有挂起的请求");
            return false;
        }
        self.finish_http_request(status_code, body);
        true
    }

    /// 完成挂起的 HTTP 请求：写响应码/响应体变量或落盘文件。
    fn finish_http_request(&mut self, status_code: i32, body: &[u8]) {
        let Some(pending) = self.control.pending_http.take() else {
            return;
        };
        if let Some(varname_code) = pending.varname_code.as_deref() {
            self.interpreter.set_variable(
                varname_code,
                asb_interpreter::Value::Int(i64::from(status_code)),
            );
        }
        if let Some(filename) = pending.filename.as_deref() {
            // 指定 filename 时结果存文件（存档目录），忽略 varname_data。
            match self.save_path_for(filename) {
                Ok(path) => {
                    if let Err(e) = crate::ffi::request_write(&path, body) {
                        crate::core_warn!("[http] 结果写入 {path} 失败: {e}");
                    }
                }
                Err(e) => {
                    crate::core_warn!("[http] 结果文件路径非法 {filename}: {e}");
                }
            }
        } else if let Some(varname_data) = pending.varname_data.as_deref() {
            self.interpreter.set_variable(
                varname_data,
                asb_interpreter::Value::String(String::from_utf8_lossy(body).into_owned()),
            );
        }
    }

    /// 每帧轮询：脚本把 s.http.cancel 置 1 时中断当前 HTTP 请求并立即恢复。
    pub(super) fn poll_pending_http_cancel(&mut self) {
        let Some(pending) = &self.control.pending_http else {
            return;
        };
        let cancelled = self
            .interpreter
            .get_variable("s.http.cancel")
            .and_then(|value| value.as_int())
            .unwrap_or(0)
            == 1;
        if !cancelled {
            return;
        }
        let serial = pending.serial;
        crate::core_info!("[http] s.http.cancel=1，中断请求 serial={serial}");
        crate::ffi::emit_ui_command("http_cancel", serde_json::json!({ "serial": serial }));
        self.interpreter
            .set_variable("s.http.cancel", asb_interpreter::Value::Int(0));
        self.finish_http_request(0, &[]);
    }

    /// 是否有 HTTP 请求挂起（脚本推进须暂停）。
    pub(super) fn http_request_pending(&self) -> bool {
        self.control.pending_http.is_some()
    }

    // ── debugreload / reset ─────────────────────────────────────────

    /// [debugreload]：经宿主重读当前脚本文件并保持执行位置。
    /// 位置无法按编辑内容调整（文档已声明该局限）。
    fn handle_debug_reload(&mut self) {
        let Some(script) = self.interpreter.current_script().map(str::to_string) else {
            crate::core_warn!("[debugreload] 当前无执行中的脚本");
            return;
        };
        let line = self.interpreter.current_line();
        let stack = self.interpreter.call_stack();
        let resolved = super::magic_path::resolve_path(&self.magic_paths, &script);
        let data = match crate::ffi::request_file(&resolved) {
            Ok(data) => data,
            Err(e) => {
                crate::core_warn!("[debugreload] 重读 {script} 失败: {e}");
                return;
            }
        };
        // load_file 覆盖脚本缓存并重跑其 lua 块，再恢复原执行位置。
        if let Err(e) = self.interpreter.load_file(&script, &data) {
            crate::core_error!("[debugreload] 重新解析 {script} 失败: {e:?}");
            return;
        }
        if let Err(e) = self.interpreter.restore_position(&script, line, stack) {
            crate::core_error!("[debugreload] 恢复执行位置失败: {e:?}");
            return;
        }
        crate::core_info!("[debugreload] 已重载 {script}（第 {line} 行继续）");
    }

    /// [reset]：全量重启——重置合成器/音频/视频/控制状态后重新走 boot。
    ///
    /// 解释器侧已在 ResetHandler 里清过 local/temp 变量域；Lua 全局环境无法
    /// 在不重建解释器的情况下清空，boot 脚本重跑时会重新初始化其自有状态。
    fn handle_engine_reset(&mut self) -> Result<(), String> {
        self.stop_all_media();
        self.compositor.reset_for_load();
        self.sync_layer_info_all();
        self.hovered_layers.clear();
        self.pointer_drag = super::PointerDragState::default();
        self.save_screenshot = None;
        self.pending_dialog = None;
        self.clear_pending_text_translation();
        self.active_inline_event_frame = None;
        self.timed_remaining_ms = 0;
        self.wait_reason = None;
        self.last_system_volume = (None, None);
        // 控制状态整体回到初始（keyconfig/hide/rclick/autosave 由 boot 脚本重新配置）
        self.control = super::control::RuntimeControlState::default();
        self.audio.set_skipping(false);
        self.sync_control_status_variables();

        let boot = self
            .boot_script
            .clone()
            .ok_or_else(|| "没有记录 BOOT 脚本，无法重启".to_string())?;
        self.interpreter
            .boot(&boot)
            .map_err(|e| format!("重启 boot 脚本失败: {e:?}"))
    }

    // ── 宿主窗口按钮 / 屏幕方向通知 ────────────────────────────────

    /// 宿主窗口按钮按下（setonwindowbutton，仅 Windows）。
    /// button：0=关闭(×) / 1=最大化 / 2=最小化。
    pub fn notify_window_button(&mut self, button: i32) {
        // 弃用兼容：处理器触发时设 s.status.windowbutton 表示按下的按钮。
        self.interpreter.set_variable(
            "s.status.windowbutton",
            asb_interpreter::Value::Int(i64::from(button)),
        );
        let key = button.to_string();
        // 优先 per-button 处理器；缺省(遗留)注册落在 key="" 上，作为回退。
        let handler = self
            .compositor
            .get_input_handler("windowbutton", &key)
            .or_else(|| self.compositor.get_input_handler("windowbutton", ""))
            .cloned();
        let Some(handler) = handler else {
            crate::core_debug!("[windowbutton] 未注册 button={button} 的处理器");
            return;
        };
        super::input::enqueue_handler_tags(
            &self.interpreter,
            handler.handler.as_deref(),
            handler.file.as_deref(),
            handler.label.as_deref(),
            handler.call,
            &handler.params,
            &[("button", key.as_str()), ("type", "windowbutton")],
        );
    }

    /// 宿主屏幕方向变化（setondirchg，仅 iOS）。
    /// direction：0=纵向 / 1=横向Home右 / 2=倒置纵向 / 3=横向Home左。
    pub fn notify_direction_changed(&mut self, direction: i32) {
        let previous = self
            .interpreter
            .get_variable("s.status.screendirection")
            .and_then(|value| value.as_int())
            .unwrap_or(0);
        self.interpreter.set_variable(
            "s.status.screendirectionprevious",
            asb_interpreter::Value::Int(previous),
        );
        self.interpreter.set_variable(
            "s.status.screendirection",
            asb_interpreter::Value::Int(i64::from(direction)),
        );
        let Some(handler) = self.compositor.get_input_handler("dirchg", "").cloned() else {
            crate::core_debug!("[dirchg] 未注册方向变更处理器");
            return;
        };
        super::input::enqueue_handler_tags(
            &self.interpreter,
            handler.handler.as_deref(),
            handler.file.as_deref(),
            handler.label.as_deref(),
            handler.call,
            &handler.params,
            &[("type", "dirchg")],
        );
    }

    /// 宿主经 FFI 写回解释器变量（callnative/purchase 结果回注通道）。
    pub fn set_string_variable(&mut self, name: &str, value: &str) {
        self.interpreter
            .set_variable(name, asb_interpreter::Value::String(value.to_string()));
    }

    // ── 宿主查询钩子安装（var system=file_*/get_sound_info）──────────

    /// 安装 asb-interpreter 的宿主查询钩子。每个 runtime 只装一次；
    /// 装载时机在 load_project 之后（savepath 已定），钩子捕获 Arc/克隆值。
    pub(super) fn ensure_host_query_hooks(&mut self) {
        if self.control.host_hooks_installed {
            return;
        }
        self.control.host_hooks_installed = true;

        let magic_for_exists = std::sync::Arc::clone(&self.magic_paths);
        let savepath_for_exists = self.savepath.clone();
        let magic_for_crc = std::sync::Arc::clone(&self.magic_paths);
        let savepath_for_time = self.savepath.clone();

        asb_interpreter::tags::var_handler::set_host_query_hooks(
            asb_interpreter::tags::var_handler::HostQueryHooks {
                file_exists: Box::new(move |file, save| {
                    if save {
                        // save=1：目标为存档数据，按存档目录相对路径查询。
                        qualify_hook_save_path(file, &savepath_for_exists)
                            .map(|path| crate::ffi::query_asset_size(&path).is_some())
                            .unwrap_or(false)
                    } else {
                        let resolved =
                            super::magic_path::resolve_path(&magic_for_exists, file);
                        crate::ffi::query_asset_size(&resolved).is_some()
                    }
                }),
                file_crc32: Box::new(move |file| {
                    let resolved = super::magic_path::resolve_path(&magic_for_crc, file);
                    crate::ffi::request_file(&resolved)
                        .ok()
                        .map(|bytes| crc32_ieee(&bytes))
                }),
                file_update_time: Box::new(move |file| {
                    qualify_hook_save_path(file, &savepath_for_time)
                        .and_then(|path| crate::ffi::request_file_mtime(&path))
                }),
                sound_info: Box::new(super::media::sound_info_snapshot),
                // backlog / message-tags 从每帧刷新的进程级快照读取
                // （text_renderer 非 Send+Sync，不能直接借入进程级钩子）。
                backlog_size: Box::new(|| super::text::backlog_snapshot().backlog_size()),
                backlog_tags: Box::new(|page, allfont| {
                    super::text::backlog_snapshot().backlog_tags(page, allfont)
                }),
                message_tags: Box::new(|id, allfont| {
                    super::text::backlog_snapshot().message_tags(id, allfont)
                }),
                message_layer_metrics: Box::new(super::text::text_metrics_snapshot),
            },
        );
    }
}

/// 钩子里可用的存档路径归一（与 save_path_for 相同规则的自由函数版本）。
fn qualify_hook_save_path(file: &str, savepath: &str) -> Option<String> {
    super::save_io::qualify_save_path_for_hooks(file, savepath).ok()
}

/// CRC32（IEEE 802.3，反射多项式 0xEDB88320）——与 Artemis file_crc 一致。
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
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

/// `[lytween sync=1]` 的等待理由：把图层 ID 编码进 Stop 原因，
/// 恢复时据此定位要观察的图层。
fn sync_tween_wait_reason(id: &str) -> WaitReason {
    WaitReason::Stop {
        reason: Some(format!("tween:{id}")),
    }
}

/// 若等待理由来自 `[lytween sync=1]`，返回其目标图层 ID。
fn sync_tween_wait_layer(reason: &WaitReason) -> Option<&str> {
    match reason {
        WaitReason::Stop {
            reason: Some(reason),
        } => reason.strip_prefix("tween:"),
        _ => None,
    }
}

/// 目标图层的缓动是否全部结束（gc 已清空 tweens）或图层已删除。
fn sync_tween_finished(compositor: &crate::compositor::Compositor, id: &str) -> bool {
    compositor
        .scene()
        .get(id)
        .is_none_or(|layer| layer.tweens.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{crc32_ieee, sync_tween_finished, sync_tween_wait_layer, sync_tween_wait_reason};

    #[test]
    fn crc32_matches_ieee_reference_vectors() {
        // 标准校验向量：CRC32("123456789") = 0xCBF43926
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_ieee(b""), 0);
        assert_eq!(crc32_ieee(b"a"), 0xE8B7_BE43);
    }
    use crate::compositor::Compositor;
    use asb_interpreter::Event;
    use asb_interpreter::event::{LayerEvent, WaitReason};

    #[test]
    fn sync_tween_reason_roundtrips_layer_id() {
        let reason = sync_tween_wait_reason("1.05");
        assert_eq!(sync_tween_wait_layer(&reason), Some("1.05"));

        // 其它 Stop 原因（trans/video）与非 Stop 等待都不应被识别为缓动等待。
        assert_eq!(
            sync_tween_wait_layer(&WaitReason::Stop {
                reason: Some("trans".into())
            }),
            None
        );
        assert_eq!(sync_tween_wait_layer(&WaitReason::Generic), None);
    }

    #[test]
    fn sync_tween_wait_resumes_after_layer_tweens_finish() {
        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create {
            id: "1".into(),
            file: "a".into(),
        }));
        c.apply_event(&Event::LayerTween {
            id: "1".into(),
            param: "alpha".into(),
            from: Some("0".into()),
            to: Some("255".into()),
            ease: None,
            time: Some(1000),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: true,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        });

        // 缓动进行中：不可恢复。
        c.advance(500);
        assert!(!sync_tween_finished(&c, "1"));

        // 缓动结束被 gc 回收后：恢复条件成立。
        c.advance(600);
        assert!(sync_tween_finished(&c, "1"));

        // 图层不存在（已删除）时同样视为结束，避免死等。
        assert!(sync_tween_finished(&c, "no-such-layer"));
    }

    // dispatch_events 里每个事件都过 CompositorEvent::from_interpreter 后交给
    // Compositor::apply_event。这里核对阶段1解释器给 lyc 透传的 color/width/height
    // (SetProperties) 与 mask 确实经该路由被合成器消费到纯色/蒙版字段。
    #[test]
    fn lyc_solid_color_and_size_reach_compositor_via_dispatch_route() {
        use crate::compositor::CompositorEvent;
        use std::collections::HashMap;

        let mut c = Compositor::new();
        // lyc 缺省 file → 单色图层模式：先 Create（空 file），再 SetProperties。
        let create = Event::Layer(LayerEvent::Create {
            id: "5".into(),
            file: String::new(),
        });
        c.apply_event(CompositorEvent::from_interpreter(&create).unwrap());

        let props = Event::Layer(LayerEvent::SetProperties {
            id: "5".into(),
            properties: HashMap::from([
                ("color".into(), "80FF0000".into()),
                ("width".into(), "100".into()),
                ("height".into(), "50".into()),
            ]),
        });
        c.apply_event(CompositorEvent::from_interpreter(&props).unwrap());

        let layer = c.scene().get("5").unwrap();
        // color RRGGBB/AARRGGBB → 纯色路径（solid_color = [R,G,B,A]）。
        assert_eq!(layer.solid_color, Some([255, 0, 0, 0x80]));
        assert_eq!(layer.props.width, Some(100.0));
        assert_eq!(layer.props.height, Some(50.0));
    }

    #[test]
    fn lyc_mask_reaches_compositor_via_dispatch_route() {
        use crate::compositor::CompositorEvent;
        use std::collections::HashMap;

        let mut c = Compositor::new();
        let create = Event::Layer(LayerEvent::Create {
            id: "1".into(),
            file: "fg".into(),
        });
        c.apply_event(CompositorEvent::from_interpreter(&create).unwrap());

        let props = Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: HashMap::from([("mask".into(), "fgmask".into())]),
        });
        c.apply_event(CompositorEvent::from_interpreter(&props).unwrap());

        // mask 路径：图层 mask 字段被设置（合成时走 resolve_with_mask）。
        assert_eq!(c.scene().get("1").unwrap().mask.as_deref(), Some("fgmask"));
    }
}
