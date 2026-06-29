use super::CoreRuntime;
use crate::backend::gl::platform;
use crate::save::AudioSnapshot;
use glow::HasContext;
use image::ImageEncoder;
use std::collections::HashMap;

/// `syssave()`（无 file 的 `[save]`）落盘的全局域文件名。
const SAVEG_FILE: &str = "saveg.dat";
/// `syssave()` 落盘的系统域文件名。
const SYSTEM_FILE: &str = "system.dat";

#[derive(Clone)]
pub(super) struct ScreenshotBuffer {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    scene: crate::compositor::Scene,
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

    /// 处理 [save]：触发 onSave 序列化 `sys` 等 Lua 表 → 抽干 [var] 队列 →
    /// 快照解释器状态 → JSON → 经宿主写回调落盘。
    pub(super) fn handle_save_game(&mut self, file: &str) -> Result<(), String> {
        // onSave（store）把 sys/gscr/conf 经 pluto 序列化进 Artemis 变量，
        // 这些 [var] 标签排入队列后必须抽干，快照才能包含它们。
        self.interpreter
            .fire_save_handler()
            .map_err(|e| format!("onSave 处理器失败: {e:?}"))?;
        self.interpreter
            .flush_pending_tags()
            .map_err(|e| format!("抽干存档标签失败: {e:?}"))?;

        let mut data = crate::save::SaveData::from_interpreter(&self.interpreter);
        if let Some(snapshot) = &self.save_screenshot {
            data = data.with_scene(snapshot.scene.clone());
        } else {
            data = data.with_scene(self.compositor.scene_snapshot());
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
        let system: HashMap<String, asb_interpreter::Value> = store
            .iter_system()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (file, map) in [(SAVEG_FILE, &global), (SYSTEM_FILE, &system)] {
            let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
            let path = self.save_path_for(file)?;
            crate::ffi::request_write(&path, json.as_bytes())?;
            crate::core_info!("[runtime] syssave 已保存 {} ({} 项)", path, map.len());
        }
        Ok(())
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
            let n = map.len();
            for (k, v) in map {
                self.interpreter.set_variable(&format!("{prefix}{k}"), v);
            }
            crate::core_info!("[runtime] sysload 已读回 {} ({} 项)", path, n);
        }
    }

    /// 处理 [load]：经宿主读回调读取 JSON → 恢复变量与执行位置 → 触发 onLoad
    /// 把变量反序列化回 `sys` 等 Lua 表 → 抽干队列 → 清等待状态让脚本续跑。
    pub(super) fn handle_load_game(&mut self, file: &str) -> Result<(), String> {
        let path = self.save_path_for(file)?;
        let bytes = crate::ffi::request_file(&path)?;
        let data: crate::save::SaveData =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        self.compositor.reset_for_load();
        self.stop_all_media();
        self.hovered_layers.clear();
        if let Some(scene) = data.scene.clone() {
            self.compositor.restore_scene(scene);
        }
        self.sync_layer_info_all();
        data.restore(&mut self.interpreter)
            .map_err(|e| format!("恢复存档状态失败: {e:?}"))?;

        // onLoad（restore）把恢复的变量经 pluto 反序列化回 sys/gscr/scr/log 等表，
        // 否则承载游戏态与存档槽位的 Lua 表仍是旧的。
        self.interpreter
            .fire_load_handler()
            .map_err(|e| format!("onLoad 处理器失败: {e:?}"))?;
        self.interpreter
            .flush_pending_tags()
            .map_err(|e| format!("抽干读档标签失败: {e:?}"))?;
        self.apply_system_audio_volume();
        if let Some(audio) = &data.audio {
            self.restore_audio_snapshot(audio);
        }

        // 清除等待状态，使下一帧 run() 从恢复后的位置继续执行。
        self.wait_reason = None;
        crate::core_info!("[runtime] 已读取存档: {}", path);
        Ok(())
    }

    pub(super) fn handle_go_title(&mut self) -> Result<(), String> {
        self.compositor.reset_for_load();
        self.sync_layer_info_all();
        self.stop_all_media();
        self.hovered_layers.clear();
        self.save_screenshot = None;
        self.timed_remaining_ms = 0;
        self.wait_reason = None;
        self.interpreter
            .start("system/first.iet", "title")
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    pub(super) fn capture_save_screenshot(&mut self) {
        // 确保 FBO 已绑定并渲染完成
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl.finish();
        }
        // 从 FBO 读取像素（使用 glReadPixels，对所有后端都可靠）
        let rgba =
            unsafe { platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32) };
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.save_screenshot = Some(ScreenshotBuffer {
            width: self.stage_w,
            height: self.stage_h,
            rgba,
            scene: self.compositor.scene_snapshot(),
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
        self.texture_provider
            .upload_rgba(&resource_name, target_width, target_height, &rgba);
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
        ScreenshotBuffer, encode_png_rgba, qualify_save_path, resize_screenshot_rgba,
        sanitize_savepath,
    };

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
    fn screenshot_resize_and_png_encode_work() {
        let screenshot = ScreenshotBuffer {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            scene: crate::compositor::Scene::new(),
        };
        let resized = resize_screenshot_rgba(&screenshot, 1, 1).unwrap();
        assert_eq!(resized.len(), 4);

        let png = encode_png_rgba(&resized, 1, 1).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (1, 1));
    }
}
