use super::CoreRuntime;
use crate::backend::{Extent2D, FrameTarget, TextureData, TextureDesc};
use crate::save::AudioSnapshot;
use image::ImageEncoder;
use std::collections::HashMap;

/// `syssave()`（无 file 的 `[save]`）落盘的全局域文件名。
const SAVEG_FILE: &str = "saveg.dat";
/// `syssave()` 落盘的系统域文件名。
const SYSTEM_FILE: &str = "system.dat";
/// [autosave] 自动保存的存档文件名。
const AUTOSAVE_FILE: &str = "__Autosave.dat";
/// 已读记录（alreadyread）持久化文件名。跨会话保留，使"已读跳过"有意义。
const AREAD_FILE: &str = "aread.dat";

/// System variables that are part of the persistent user configuration.
///
/// Artemis exposes a single `s.*` namespace for both user-adjustable settings
/// and values computed from the current runtime.  `iter_system()` therefore
/// cannot be serialized wholesale: values such as `s.status.*`,
/// `s.current_message_layer`, screen dimensions and paths describe the active
/// session and must be re-created by the new runtime. Even writable values
/// such as `s.http.cancel` and `s.backlog.annotation` are operation-scoped,
/// so persistence is an explicit property rather than an inference from
/// writability or from a particular game's script.
///
/// The translated copy of `system_variables` in `example/docs` predates
/// `s.enablemaximizedwindow`; the upstream Japanese specification lists it as
/// writable, and existing projects use it to retain the Windows maximize
/// policy.  Keep it here even though most projects never set it.
fn is_persistent_system_variable(name: &str) -> bool {
    matches!(
        name,
        "bgmvol" | "sevol" | "videovol" | "automodewait" | "enablemaximizedwindow"
    ) || name
        .strip_prefix("segain.")
        .is_some_and(|id| !id.is_empty())
}

fn persistent_system_snapshot(
    store: &asb_interpreter::VariableStore,
) -> HashMap<String, asb_interpreter::Value> {
    store
        .iter_system()
        .filter(|(name, _)| is_persistent_system_variable(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

#[derive(Clone)]
pub(super) struct ScreenshotBuffer {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    scene: crate::compositor::Scene,
    message_text: Option<crate::save::MessageTextSnapshot>,
}

impl CoreRuntime {
    /// 把脚本存档路径归一成宿主 saveDir 内的相对路径。
    ///
    /// 真实 Artemis 的 `[save file="save0001.dat"]` / `[load file=...]`
    /// 默认落在 `SAVEPATH` 下；脚本里 `isSaveFile()` 又会检查
    /// `s.savepath.."/"..file`。所以宿主回调必须看到同一种脚本相对路径：
    /// `savedata/save0001.dat`。若脚本已经传了 `savedata/...`，保持原样。
    pub(super) fn save_path_for(&self, file: &str) -> Result<String, String> {
        qualify_save_path(file, &self.savepath)
    }

    /// 存档目录内的文件复制/移动（fileio 的 copy/move 命令）。
    /// 全部经宿主读写回调完成，core 不直接碰文件系统。
    pub(super) fn copy_save_file(
        &self,
        src: &str,
        dst: &str,
        delete_src: bool,
    ) -> Result<(), String> {
        let src_path = self.save_path_for(src)?;
        let dst_path = self.save_path_for(dst)?;
        let data = crate::ffi::request_file(&src_path)?;
        crate::ffi::request_write(&dst_path, &data)?;
        if delete_src {
            crate::ffi::request_delete(&src_path)?;
        }
        Ok(())
    }

    /// 处理 [save]：触发 onSave 序列化 `sys` 等 Lua 表 → 抽干其 [var] 队列 →
    /// 快照解释器状态 → JSON → 经宿主写回调落盘。
    pub(super) fn handle_save_game(&mut self, file: &str) -> Result<(), String> {
        // onSave（store）把 sys/gscr/conf 经 pluto 序列化进 Artemis 变量，
        // 这些 [var] 标签必须在快照前执行，但保存 UI 当时已有的返回/跳转队列
        // 必须原样保留，否则会在 Event::SaveGame 回调中重入并破坏剧情恢复位置。
        self.interpreter
            .fire_save_handler_and_flush_with_params(&HashMap::from([("file".into(), file.into())]))
            .map_err(|e| format!("onSave 处理器失败: {e:?}"))?;

        let mut data = crate::save::SaveData::from_interpreter(&self.interpreter);
        if let Some(checkpoint) = &self.gameplay_save_checkpoint {
            if checkpoint.is_ancestor_of(&data.current_script, &data.call_stack) {
                crate::core_info!(
                    "[runtime] 编号存档使用点击等待检查点 {}:{} -> {}:{} stack={} -> {}",
                    data.current_script,
                    data.current_line,
                    checkpoint.script,
                    checkpoint.line,
                    data.call_stack.len(),
                    checkpoint.call_stack.len()
                );
                data = data.with_gameplay_checkpoint(checkpoint);
            }
        }
        if let Some(snapshot) = &self.save_screenshot {
            data = data.with_scene(snapshot.scene.clone());
            if let Some(message_text) = snapshot.message_text.clone() {
                data = data.with_message_text(message_text);
            }
        } else {
            data = data.with_scene(self.compositor.scene_snapshot());
            if let Some(message_text) = self.capture_message_text_snapshot() {
                data = data.with_message_text(message_text);
            }
        }
        if data.waiting_for_input.is_none() {
            data.waiting_for_input = Some(super::script::wait_reason_is_input_wait(
                self.wait_reason.as_ref(),
            ));
        }
        data = data.with_audio(AudioSnapshot::from_audio(self.audio.as_ref()));
        let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        let path = self.save_path_for(file)?;
        crate::ffi::request_write(&path, json.as_bytes())?;
        crate::core_info!("[runtime] 已保存存档: {}", path);

        // `store()`/`saveconv(true)` 已把最新 `sys.saveslot` 写入 `g.system`。
        // 编号存档文件本身只负责读档恢复；存档/读档列表重启后的槽位索引来自
        // saveg.dat。若这里只写 `save0001.dat`，本次会话内能读，但重启后列表为空。
        self.syssave()
            .map_err(|e| format!("同步系统存档失败: {e}"))?;
        Ok(())
    }

    /// 处理 `syssave()`——即不带 `file` 的 `[save]`（fileio.lua `eqtag{"save"}`）。
    ///
    /// 与编号存档不同，它只持久化**全局/系统**两个变量域（脚本先经
    /// `fsave_pluto` 把 sys/gscr/conf 序列化进 `g.*` 变量，再触发本保存）。落两份
    /// 固定文件：`saveg.dat`（全局域 `g.`）与 `system.dat`（系统域 `s.`），下次启动
    /// 由 [`Self::sysload`] 读回。
    pub(super) fn syssave(&mut self) -> Result<(), String> {
        // fileio.lua 的 syssave() 已在排入 `[save]` 前同步执行 saveconv()，
        // 将 sys/gscr/conf 写入 g.*。这里不能再触发 onSave/flush_tag_queue：
        // 运行到 YES 对话框时，file="" 的 syssave 事件会早于 UI 关闭队列完成到达；
        // 若此处抽干全局 tag_queue，会把 dialog_return 的 return+jump 提前消费，
        // 破坏后续恢复到 popfunc02 第二个 fn.pop 的控制流。
        let store = self.interpreter.variables();
        let global: HashMap<String, asb_interpreter::Value> = store
            .iter_global()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // `s.*` contains both persistent settings and runtime-derived values.
        // Persist only the documented writable subset; otherwise a later boot
        // inherits stale status/current-layer/path data from the previous run.
        let system = persistent_system_snapshot(&store);

        for (file, map) in [(SAVEG_FILE, &global), (SYSTEM_FILE, &system)] {
            let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
            let path = self.save_path_for(file)?;
            crate::ffi::request_write(&path, json.as_bytes())?;
            crate::core_info!("[runtime] syssave 已保存 {} ({} 项)", path, map.len());
        }
        // 已读记录随 syssave 落盘（仅在有新增时写，减少 IO）。
        self.save_aread()?;
        Ok(())
    }

    /// 把已读记录写入 `aread.dat`（脏时才写）。
    fn save_aread(&mut self) -> Result<(), String> {
        if !self.read_dirty {
            return Ok(());
        }
        let data = self.control.read_lines_export();
        let json = serde_json::to_string(&data).map_err(|e| e.to_string())?;
        let path = self.save_path_for(AREAD_FILE)?;
        crate::ffi::request_write(&path, json.as_bytes())?;
        self.read_dirty = false;
        crate::core_info!("[runtime] 已读记录已保存 {} ({} 脚本)", path, data.len());
        Ok(())
    }

    /// 启动时读回已读记录（缺文件属首次运行，静默跳过）。
    pub(super) fn load_aread(&mut self) {
        let Ok(path) = self.save_path_for(AREAD_FILE) else {
            return;
        };
        let Ok(bytes) = crate::ffi::request_file(&path) else {
            return;
        };
        match serde_json::from_slice::<std::collections::HashMap<String, Vec<usize>>>(&bytes) {
            Ok(data) => {
                let n = data.len();
                self.control.read_lines_import(data);
                crate::core_info!("[runtime] 已读记录已载入 {} ({} 脚本)", path, n);
            }
            Err(e) => {
                crate::core_warn!("[runtime] aread.dat 解析失败: {}", e);
            }
        }
    }

    /// 启动时读回 `syssave()` 落下的全局/系统域。缺文件（首次启动）静默跳过。
    pub(super) fn sysload(&mut self) {
        for (file, is_global) in [(SAVEG_FILE, true), (SYSTEM_FILE, false)] {
            let Ok(path) = self.save_path_for(file) else {
                continue;
            };
            let bytes = match crate::ffi::request_file(&path) {
                Ok(b) => b,
                Err(e) => {
                    // 首次启动尚无文件属正常；记 debug 便于排查"读路径不对"的情况。
                    crate::core_debug!("[runtime] sysload 跳过 {} ({})", path, e);
                    continue;
                }
            };
            let map: HashMap<String, asb_interpreter::Value> = match serde_json::from_slice(&bytes)
            {
                Ok(m) => m,
                Err(e) => {
                    crate::core_warn!("[runtime] sysload 解析 {} 失败: {}", path, e);
                    continue;
                }
            };
            let prefix = if is_global { "g." } else { "s." };
            let total = map.len();
            let mut loaded = 0;
            for (k, v) in map {
                if !is_global && !is_persistent_system_variable(&k) {
                    continue;
                }
                loaded += 1;
                self.interpreter.set_variable(&format!("{prefix}{k}"), v);
            }
            crate::core_info!(
                "[runtime] sysload 已读回 {} ({} 项{})",
                path,
                loaded,
                if !is_global && loaded != total {
                    format!(", 忽略 {} 项运行时变量", total - loaded)
                } else {
                    String::new()
                }
            );
        }
    }

    /// 处理 [load]：经宿主读回调读取 JSON → 恢复变量与执行位置 → 触发 onLoad
    /// 把变量反序列化回 `sys` 等 Lua 表 → 抽干队列 → 清等待状态让脚本续跑。
    ///
    /// `trans_type`：`[load type=0..2]` 指定时在加载完成后执行一次转场
    /// （与 trans 标签 type 同值）；缺省瞬切。
    pub(super) fn capture_gameplay_save_checkpoint(&mut self) {
        let Some(script) = self.interpreter.current_script() else {
            return;
        };
        let call_stack: Vec<crate::save::CallFrameSnapshot> = self
            .interpreter
            .call_stack()
            .iter()
            .map(crate::save::CallFrameSnapshot::from)
            .collect();
        if self
            .gameplay_save_checkpoint
            .as_ref()
            .is_some_and(|existing| existing.is_ancestor_of(script, &call_stack))
        {
            // 菜单/对话框里的等待是从点击等待 call 出去的，不能覆盖原检查点。
            return;
        }
        self.gameplay_save_checkpoint = Some(crate::save::GameplayCheckpoint {
            script: script.to_string(),
            line: self.interpreter.current_line(),
            call_stack,
            variables: self.interpreter.variables().local_snapshot(),
            waiting_for_input: true,
        });
    }

    pub(super) fn handle_load_game(
        &mut self,
        file: &str,
        trans_type: Option<i32>,
    ) -> Result<(), String> {
        let path = self.save_path_for(file)?;
        crate::core_info!("[runtime] 读取存档文件 FFI 开始: {}", path);
        let bytes = crate::ffi::request_file(&path).map_err(|error| {
            crate::core_error!("[runtime] 读取存档文件 FFI 失败: {}: {}", path, error);
            error
        })?;
        crate::core_info!(
            "[runtime] 读取存档文件 FFI 返回: {} ({} bytes)",
            path,
            bytes.len()
        );
        let data: crate::save::SaveData =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        self.gameplay_save_checkpoint = None;
        self.pending_load_resume = None;
        self.pending_message_text = None;
        self.clear_emote_state("save load");
        self.compositor.reset_for_load();
        self.stop_all_media();
        self.reset_control_modes_for_load();
        self.clear_scene_text();
        self.hovered_layers.clear();
        self.wait_reason = None;
        data.restore(&mut self.interpreter)
            .map_err(|e| format!("恢复存档状态失败: {e:?}"))?;

        // onLoad（restore）把恢复的变量经 pluto 反序列化回 sys/gscr/scr/log 等表，
        // 否则承载游戏态与存档槽位的 Lua 表仍是旧的。
        self.interpreter
            .fire_load_handler_with_params(&HashMap::from([("file".into(), file.into())]))
            .map_err(|e| format!("onLoad 处理器失败: {e:?}"))?;
        self.sync_control_status_variables();
        // Immediate e:tag commands cover the outgoing scene. Deferred
        // enqueueTag commands belong on the restored scene.
        let deferred = self.interpreter.take_deferred_queued_tags();
        self.interpreter
            .flush_pending_tags()
            .map_err(|e| format!("抽干读档标签失败: {e:?}"))?;
        let mut profile = crate::profiler::FrameProfile::default();
        self.flush_host_events(&mut profile);
        if let Some(scene) = data.scene.clone() {
            self.compositor.restore_scene(scene);
        }
        self.sync_layer_info_all();
        self.interpreter.append_queued_tags(deferred);
        self.reload_persistent_lua_tables()
            .map_err(|e| format!("恢复当前系统存档索引失败: {e}"))?;
        self.apply_system_audio_volume();
        if let Some(audio) = &data.audio {
            self.restore_audio_snapshot(audio);
        }

        // [load type=..]：加载完成后启动一次转场。下一帧渲染前合成器会捕获
        // FBO 里残留的读档前旧画面作为转场旧帧，再淡入恢复后的场景。
        if let Some(event) = load_transition_event(trans_type) {
            self.compositor.apply_event(&event);
        }

        let waiting = match data.waiting_for_input {
            Some(value) => value,
            None => saved_position_resumes_input_wait(&self.interpreter),
        };
        self.pending_message_text = data.message_text.clone();
        if waiting {
            // Saved click-waits snapshot the PC after the queued `@`/`[wait]`.
            // Hold that wait so onLoad follow-up can run, then stop before the
            // message macro's post-wait cleanup (rp / text_erase) executes.
            self.interpreter.hold_queued_input_wait();
            self.pending_load_resume = Some(super::PendingLoadResume {
                script: data.current_script.clone(),
                line: data.current_line,
                stack_len: data.call_stack.len(),
            });
            self.wait_reason = None;
            self.maybe_restore_loaded_message_text();
        } else {
            self.restore_pending_message_text();
            self.wait_reason = None;
        }
        crate::core_info!(
            "[runtime] 已读取存档: {} -> {}:{} stack={} wait={}",
            path,
            data.current_script,
            data.current_line,
            data.call_stack.len(),
            waiting
        );
        Ok(())
    }

    fn reload_persistent_lua_tables(&mut self) -> Result<(), String> {
        self.interpreter
            .lua()
            .load(
                r#"
                if type(fload_pluto) == "function" and type(init) == "table" then
                    if init.save_system then sys = fload_pluto(init.save_system) or sys or {} end
                    if init.save_global then gscr = fload_pluto(init.save_global) or gscr or {} end
                    if init.save_config then conf = fload_pluto(init.save_config) or conf or {} end
                end
                "#,
            )
            .exec()
            .map_err(|e| e.to_string())
    }

    /// 执行一次自动保存（[autosave] 语义）。存档文件固定为 __Autosave.dat。
    fn autosave_now(&mut self, reason: &str) {
        crate::core_info!("[autosave] 触发（{reason}）→ {AUTOSAVE_FILE}");
        if let Err(e) = self.handle_save_game(AUTOSAVE_FILE) {
            crate::core_warn!("[autosave] 保存失败: {e}");
        }
    }

    /// allow=2：进入用户输入等待时自动保存。由 advance_script 在等待建立时调用。
    pub(super) fn maybe_autosave_on_input_wait(&mut self) {
        if self.control.autosave_allow == 2 {
            self.autosave_now("输入等待");
        }
    }

    /// 宿主生命周期通知（FFI `art3m1s_runtime_notify_lifecycle`）。
    ///
    /// state：0=引擎退出前、1=切到后台（iOS/Android）、2=回到前台。
    /// [autosave allow=1] 时在退出/切后台时自动保存。
    pub fn notify_lifecycle(&mut self, state: i32) {
        match state {
            0 | 1 => {
                if self.control.autosave_allow == 1 {
                    self.autosave_now(if state == 0 {
                        "引擎退出"
                    } else {
                        "切后台"
                    });
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_go_title(&mut self) -> Result<(), String> {
        self.gameplay_save_checkpoint = None;
        self.pending_load_resume = None;
        self.pending_message_text = None;
        self.clear_emote_state("go title");
        self.compositor.reset_for_load();
        self.sync_layer_info_all();
        self.stop_all_media();
        self.clear_scene_text();
        self.hovered_layers.clear();
        self.save_screenshot = None;
        self.timed_remaining_ms = 0;
        self.wait_reason = None;

        // [gotitle] 只表达“回到标题”这一生命周期动作，标题的实际入口
        // 由项目自己的 BOOT 脚本决定。不同模板可能从文件开头跳到
        // system/title.iet，也可能使用 *top/*main 等标签；硬编码
        // `system/first.iet:title` 会在前者没有该标签时直接失败。
        let boot = self
            .boot_script
            .as_deref()
            .ok_or_else(|| "没有记录 BOOT 脚本，无法返回标题".to_string())?;
        self.interpreter
            .boot(boot)
            .map_err(|e| format!("执行 BOOT 脚本 {boot} 失败: {e:?}"))
    }

    pub(super) fn capture_save_screenshot(&mut self) {
        let extent = Extent2D::new(self.stage_w, self.stage_h);
        let rgba = match self.gpu.readback_owned(FrameTarget::Main, extent) {
            Ok(rgba) => rgba,
            Err(error) => {
                crate::core_warn!("[runtime] takess screenshot readback failed: {error}");
                return;
            }
        };
        self.save_screenshot = Some(ScreenshotBuffer {
            width: self.stage_w,
            height: self.stage_h,
            rgba,
            scene: self.compositor.scene_snapshot(),
            message_text: self.capture_message_text_snapshot(),
        });
        crate::core_info!(
            "[runtime] takess 已缓存游戏画面 {}x{}",
            self.stage_w,
            self.stage_h
        );
    }

    pub(super) fn handle_save_screenshot(
        &mut self,
        file: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<(), String> {
        let screenshot = self
            .save_screenshot
            .clone()
            .ok_or_else(|| "savess 前没有 takess 缓存，跳过以避免截到保存界面".to_string())?;
        let target_width = width.unwrap_or(screenshot.width).max(1);
        let target_height = height.unwrap_or(screenshot.height).max(1);
        let rgba = resize_screenshot_rgba(&screenshot, target_width, target_height)?;
        let png = encode_png_rgba(&rgba, target_width, target_height)?;
        let (resource_name, path) = self.screenshot_paths_for(file)?;

        crate::ffi::request_write(&path, &png)?;
        let desc = TextureDesc::sampled_rgba8(target_width, target_height);
        let _ = self
            .gpu
            .create_texture(&resource_name, desc, TextureData::Rgba8(&rgba));
        crate::core_info!(
            "[runtime] 已保存缩略图: {} (resource={}, {}x{})",
            path,
            resource_name,
            target_width,
            target_height
        );
        Ok(())
    }

    fn screenshot_paths_for(&self, file: &str) -> Result<(String, String), String> {
        let name =
            normalize_relative_path(file).ok_or_else(|| format!("非法缩略图路径: {file}"))?;
        let lower = name.to_ascii_lowercase();
        let resource_name = if lower.ends_with(".png") {
            name[..name.len() - 4].to_string()
        } else {
            name.clone()
        };
        let png_name = if lower.ends_with(".png") {
            name
        } else {
            format!("{name}.png")
        };
        Ok((
            self.save_path_for(&resource_name)?,
            self.save_path_for(&png_name)?,
        ))
    }
}

/// 把 `[load type=..]` 的转场类型换成合成器可消费的 `Event::Trans`。
///
/// 文档：type 0..2 与 trans 标签 type 同值（0=瞬切、1=交叉淡化、2=规则图转场，
/// load 无 rule 参数故 2 退化为淡化）；缺省不执行转场。时长/输入用 trans 的
/// 缺省值（time 缺省由合成器决定，input=1 允许输入跳过）。
fn saved_position_resumes_input_wait(interpreter: &asb_interpreter::Interpreter) -> bool {
    let Some(name) = interpreter.current_script() else {
        return false;
    };
    let Some(script) = interpreter.get_script(name) else {
        return false;
    };
    let line = interpreter.current_line();
    if instruction_is_input_wait(script.get_instruction(line)) {
        return true;
    }
    if line == 0 {
        return false;
    }
    instruction_is_input_wait(script.get_instruction(line - 1))
}

fn instruction_is_input_wait(instruction: Option<&asb_interpreter::Instruction>) -> bool {
    let Some(instruction) = instruction else {
        return false;
    };
    match instruction.tag.as_str() {
        "@" | "wt" | "s" | "clickwait" => true,
        "wait" => {
            let timed = instruction.get("time").is_some();
            let input = instruction.get("input");
            if timed && input == Some("0") {
                return false;
            }
            input != Some("0")
        }
        _ => false,
    }
}

fn load_transition_event(trans_type: Option<i32>) -> Option<asb_interpreter::Event> {
    let trans_type = trans_type?;
    if !(0..=2).contains(&trans_type) {
        crate::core_warn!("[load] 非法转场 type={trans_type}，跳过转场");
        return None;
    }
    Some(asb_interpreter::Event::Trans {
        trans_type,
        time: None,
        rule: None,
        vague: None,
        input: 1,
    })
}

/// 规范化 system.ini 的 SAVEPATH 为沙箱内的逻辑相对子目录前缀。
///
/// 原值可能是 Windows 风格（含反斜杠、盘符、CSIDL 特殊文件夹名），桌面/移动端都
/// 不能直接当文件系统路径用。这里：反斜杠转正斜杠、去掉首尾分隔符、剔除
/// `..`/盘符等危险段，得到一个干净的相对前缀；为空则退回 `save`。物理落盘基准由
/// 宿主（Flutter）解析到应用沙箱目录。
pub(super) fn sanitize_savepath(raw: Option<&str>) -> String {
    let raw = raw.map(|s| s.trim()).unwrap_or("");
    if raw.is_empty() {
        return "save".to_string();
    }
    let normalized = raw.replace('\\', "/");
    let cleaned: Vec<&str> = normalized
        .split('/')
        .map(|seg| seg.trim())
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != ".." && !seg.ends_with(':'))
        .collect();
    if cleaned.is_empty() {
        "save".to_string()
    } else {
        cleaned.join("/")
    }
}

fn normalize_relative_path(path: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        let part = part.trim();
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return None;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// 供事件侧钩子（var system=file_exists save=1 / file_update_time）使用的
/// 存档路径归一入口，与 [`CoreRuntime::save_path_for`] 同一套规则。
pub(super) fn qualify_save_path_for_hooks(file: &str, savepath: &str) -> Result<String, String> {
    qualify_save_path(file, savepath)
}

fn qualify_save_path(file: &str, savepath: &str) -> Result<String, String> {
    let f = normalize_relative_path(file).ok_or_else(|| format!("非法存档路径: {file}"))?;
    let prefix = savepath.trim_matches('/');
    if prefix.is_empty() || f == prefix || f.starts_with(&format!("{prefix}/")) {
        Ok(f)
    } else {
        Ok(format!("{prefix}/{f}"))
    }
}

fn resize_screenshot_rgba(
    screenshot: &ScreenshotBuffer,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, String> {
    if screenshot.width == target_width && screenshot.height == target_height {
        return Ok(screenshot.rgba.clone());
    }
    let image =
        image::RgbaImage::from_raw(screenshot.width, screenshot.height, screenshot.rgba.clone())
            .ok_or_else(|| {
                format!(
                    "截图 RGBA 长度不匹配: {}x{} len={}",
                    screenshot.width,
                    screenshot.height,
                    screenshot.rgba.len()
                )
            })?;
    let resized = image::imageops::resize(
        &image,
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
    );
    Ok(resized.into_raw())
}

fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected_len = width as usize * height as usize * 4;
    if rgba.len() != expected_len {
        return Err(format!(
            "PNG RGBA 长度不匹配: {}x{} expected={} actual={}",
            width,
            height,
            expected_len,
            rgba.len()
        ));
    }
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    encoder
        .write_image(rgba, width, height, image::ColorType::Rgba8.into())
        .map_err(|e| e.to_string())?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::{
        ScreenshotBuffer, encode_png_rgba, is_persistent_system_variable, load_transition_event,
        persistent_system_snapshot, qualify_save_path, resize_screenshot_rgba, sanitize_savepath,
        saved_position_resumes_input_wait,
    };
    use asb_interpreter::{Interpreter, InterpreterConfig};

    #[test]
    fn legacy_saves_resume_wait_when_pc_is_after_a_queued_click_wait() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .load_script(
                "macro.iet",
                "*main
[wait input=1]
[@]
[call label=commandskipcheck]
",
            )
            .unwrap();
        interpreter.start("macro.iet", "main").unwrap();
        interpreter
            .restore_position("macro.iet", 2, Vec::new())
            .unwrap();
        assert!(saved_position_resumes_input_wait(&interpreter));
        interpreter
            .restore_position("macro.iet", 1, Vec::new())
            .unwrap();
        assert!(saved_position_resumes_input_wait(&interpreter));
        interpreter
            .restore_position("macro.iet", 0, Vec::new())
            .unwrap();
        assert!(saved_position_resumes_input_wait(&interpreter));
    }

    #[test]
    fn save_paths_are_qualified_with_savepath_once() {
        assert_eq!(
            qualify_save_path("save0001.dat", "savedata").unwrap(),
            "savedata/save0001.dat"
        );
        assert_eq!(
            qualify_save_path("savedata/save0001.dat", "savedata").unwrap(),
            "savedata/save0001.dat"
        );
        assert_eq!(
            qualify_save_path(r"savedata\\saveg.dat", "savedata").unwrap(),
            "savedata/saveg.dat"
        );
    }

    #[test]
    fn save_paths_reject_parent_traversal() {
        assert!(qualify_save_path("../save0001.dat", "savedata").is_err());
        assert!(qualify_save_path("C:/save0001.dat", "savedata").is_err());
    }

    #[test]
    fn savepath_from_ini_is_sanitized() {
        assert_eq!(
            sanitize_savepath(Some(r"Citrus\\hokeloli")),
            "Citrus/hokeloli"
        );
        assert_eq!(sanitize_savepath(Some("../bad/save")), "bad/save");
        assert_eq!(sanitize_savepath(None), "save");
    }

    #[test]
    fn system_snapshot_excludes_runtime_derived_values() {
        let mut store = asb_interpreter::VariableStore::new();
        store.set("s.bgmvol", asb_interpreter::Value::Int(800));
        store.set("s.segain.901", asb_interpreter::Value::Int(800));
        store.set("s.enablemaximizedwindow", asb_interpreter::Value::Int(1));
        store.set("t.transient", asb_interpreter::Value::Int(99));
        store.set(
            "s.current_message_layer",
            asb_interpreter::Value::String("1.80.mw.adv".into()),
        );
        store.set("s.status.alreadyread", asb_interpreter::Value::Int(1));
        store.set("s.screen_width", asb_interpreter::Value::Int(1920));
        store.set(
            "s.savepath",
            asb_interpreter::Value::String("qureate/NekoMiko".into()),
        );

        let snapshot = persistent_system_snapshot(&store);
        assert_eq!(
            snapshot.get("bgmvol"),
            Some(&asb_interpreter::Value::Int(800))
        );
        assert_eq!(
            snapshot.get("segain.901"),
            Some(&asb_interpreter::Value::Int(800))
        );
        assert_eq!(
            snapshot.get("enablemaximizedwindow"),
            Some(&asb_interpreter::Value::Int(1))
        );
        assert!(!snapshot.contains_key("current_message_layer"));
        assert!(!snapshot.contains_key("status.alreadyread"));
        assert!(!snapshot.contains_key("screen_width"));
        assert!(!snapshot.contains_key("savepath"));
        assert!(!snapshot.contains_key("transient"));
    }

    #[test]
    fn persistent_system_variable_schema_contains_only_user_settings() {
        for name in [
            "bgmvol",
            "sevol",
            "videovol",
            "automodewait",
            "enablemaximizedwindow",
            "segain.901",
        ] {
            assert!(is_persistent_system_variable(name), "{name} should persist");
        }
        for name in [
            "current_message_layer",
            "status.controlskip",
            "status.alreadyread",
            "status.fullscreenatexit",
            "engineversion",
            "windowsversion",
            "screen_width",
            "screen_height",
            "datapath",
            "savepath",
            "clickskip",
            "overflowed",
            "backlog.annotation",
            "http.cancel",
            "segain.",
        ] {
            assert!(
                !is_persistent_system_variable(name),
                "{name} is runtime state"
            );
        }
    }

    #[test]
    fn load_type_starts_a_transition_after_restore() {
        // 缺省不转场；越界 type 也不转场。
        assert!(load_transition_event(None).is_none());
        assert!(load_transition_event(Some(3)).is_none());
        assert!(load_transition_event(Some(-1)).is_none());

        // type=1 应产出交叉淡化的 Trans 事件，应用后转场进入进行中状态。
        let event = load_transition_event(Some(1)).expect("type=1 应转场");
        match &event {
            asb_interpreter::Event::Trans {
                trans_type, input, ..
            } => {
                assert_eq!(*trans_type, 1);
                assert_eq!(*input, 1);
            }
            other => panic!("期望 Trans 事件，得到 {other:?}"),
        }
        let mut compositor = crate::compositor::Compositor::new();
        compositor.apply_event(&event);
        assert!(
            crate::render_pipeline::RenderPipeline::new(&compositor).is_transition_in_progress()
        );

        // type=0 = 瞬间切换：应用后不进入转场状态（与 trans type=0 同义）。
        let instant = load_transition_event(Some(0)).expect("type=0 也是合法取值");
        let mut compositor = crate::compositor::Compositor::new();
        compositor.apply_event(&instant);
        assert!(
            !crate::render_pipeline::RenderPipeline::new(&compositor).is_transition_in_progress()
        );
    }

    #[test]
    fn screenshot_resize_and_png_encode_work() {
        let screenshot = ScreenshotBuffer {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            scene: crate::compositor::Scene::new(),
            message_text: None,
        };
        let resized = resize_screenshot_rgba(&screenshot, 1, 1).unwrap();
        assert_eq!(resized.len(), 4);

        let png = encode_png_rgba(&resized, 1, 1).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (1, 1));
    }
}
