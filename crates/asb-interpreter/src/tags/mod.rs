//! 标签处理器模块
//!
//! 提供各种标签的执行逻辑。

mod condition;
mod control;
pub mod event_handler;
mod layer;
pub mod lua;
mod save;
mod scenario;
mod sound;
pub mod system;
mod ui;
pub mod var_handler;

pub use condition::*;
pub use event_handler::*;
pub use layer::*;
pub use lua::*;
pub use save::*;
pub use scenario::*;
pub use sound::*;
pub use system::*;
pub use ui::*;

use crate::r#macro::MacroAddHandler;

use crate::error::Result;
use crate::event::Event;
use crate::expression::ExpressionEvaluator;
use crate::script::{Instruction, Script};
use crate::variable::{Value, VariableStore};
use mlua::Lua;
use std::collections::HashMap;

/// 标签执行结果
#[derive(Debug)]
pub enum TagResult {
    /// 继续下一条指令
    Continue,
    /// 跳转到指定行
    Jump(usize),
    /// 跨脚本跳转，不压入返回地址
    JumpExternal { file: String, label: String },
    /// 调用标签（压入返回地址）
    Call {
        /// 目标脚本
        file: Option<String>,
        /// 目标标签
        label: String,
        /// 返回行号
        return_line: usize,
        /// 返回脚本名
        return_script: String,
    },
    /// 返回到调用点
    Return,
    /// 等待事件
    Wait(Event),
    /// 发出事件
    Emit(Event),
    /// 发出多个事件
    EmitMany(Vec<Event>),
    /// 动态执行另一条指令（用于 tag 标签）
    Dynamic(Instruction),
}

/// 执行上下文
pub struct ExecutionContext<'a> {
    pub variables: &'a mut VariableStore,
    pub lua: &'a Lua,
    pub current_script: &'a str,
    pub current_line: usize,
    pub instruction: &'a Instruction,
    /// 获取脚本的函数
    pub get_script: &'a dyn Fn(&str) -> Option<&'a Script>,
}

impl<'a> ExecutionContext<'a> {
    /// 创建表达式求值器
    pub fn evaluator(&self) -> ExpressionEvaluator<'_> {
        ExpressionEvaluator::new(self.variables)
    }

    /// 解析参数值（处理表达式）
    pub fn resolve_param(&self, key: &str) -> Result<Value> {
        let value = self.instruction.get(key).unwrap_or("");
        self.evaluator().resolve_param(value)
    }

    /// 解析 ID 类参数，保留字符串形态（不做数值强转）。
    ///
    /// 图层/音轨 ID 是带点号的层级路径（`1.80`、`1.0`），用 [`resolve_param`]
    /// 会被误判为浮点而丢尾零。见 [`ExpressionEvaluator::resolve_param_str`]。
    ///
    /// [`resolve_param`]: Self::resolve_param
    /// [`ExpressionEvaluator::resolve_param_str`]: crate::expression::ExpressionEvaluator::resolve_param_str
    pub fn resolve_param_str(&self, key: &str) -> Result<String> {
        let value = self.instruction.get(key).unwrap_or("");
        self.evaluator().resolve_param_str(value)
    }

    /// 获取原始参数值
    pub fn get_param(&self, key: &str) -> Option<&str> {
        self.instruction.get(key)
    }

    /// 获取脚本
    pub fn get_script(&self, name: &str) -> Option<&Script> {
        (self.get_script)(name)
    }

    /// 查找 else/elseif/endif 的位置（用于 if 条件为假时）
    pub fn find_else_elseif_or_endif(&self) -> Result<usize> {
        let script = self
            .get_script(self.current_script)
            .ok_or_else(|| crate::error::Error::ScriptNotFound(self.current_script.to_string()))?;

        let mut depth = 1;
        let mut line = self.current_line + 1;

        while line < script.len() {
            if let Some(inst) = script.get_instruction(line) {
                match inst.tag.as_str() {
                    "if" => depth += 1,
                    "/if" => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(line);
                        }
                    }
                    "else" | "elseif" if depth == 1 => {
                        return Ok(line);
                    }
                    _ => {}
                }
            }
            line += 1;
        }

        Err(crate::error::Error::RuntimeError {
            line: self.current_line,
            message: "未找到匹配的 endif".to_string(),
        })
    }

    /// 查找 elseif/else/endif 的位置（用于 elseif 条件为假时）
    pub fn find_elseif_else_or_endif(&self) -> Result<usize> {
        // 与 find_else_elseif_or_endif 逻辑相同
        self.find_else_elseif_or_endif()
    }

    /// 查找 endif 的位置（用于 else 执行后）
    pub fn find_endif(&self) -> Result<usize> {
        let script = self
            .get_script(self.current_script)
            .ok_or_else(|| crate::error::Error::ScriptNotFound(self.current_script.to_string()))?;

        let mut depth = 1;
        let mut line = self.current_line + 1;

        while line < script.len() {
            if let Some(inst) = script.get_instruction(line) {
                match inst.tag.as_str() {
                    "if" => depth += 1,
                    "/if" => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(line);
                        }
                    }
                    _ => {}
                }
            }
            line += 1;
        }

        Err(crate::error::Error::RuntimeError {
            line: self.current_line,
            message: "未找到匹配的 endif".to_string(),
        })
    }

    /// 查找 /loop 的位置（用于 loop 条件为假时跳过循环体）
    pub fn find_endloop(&self) -> Result<usize> {
        let script = self
            .get_script(self.current_script)
            .ok_or_else(|| crate::error::Error::ScriptNotFound(self.current_script.to_string()))?;

        let mut depth = 1;
        let mut line = self.current_line + 1;

        while line < script.len() {
            if let Some(inst) = script.get_instruction(line) {
                match inst.tag.as_str() {
                    "loop" => depth += 1,
                    "/loop" => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(line);
                        }
                    }
                    _ => {}
                }
            }
            line += 1;
        }

        Err(crate::error::Error::RuntimeError {
            line: self.current_line,
            message: "未找到匹配的 /loop".to_string(),
        })
    }

    /// 查找对应 loop 的开始位置（用于 /loop 跳回循环开头）
    pub fn find_loop_start(&self) -> Result<usize> {
        let script = self
            .get_script(self.current_script)
            .ok_or_else(|| crate::error::Error::ScriptNotFound(self.current_script.to_string()))?;

        let mut depth = 1;
        let mut line = self.current_line;
        if line == 0 {
            return Err(crate::error::Error::RuntimeError {
                line: self.current_line,
                message: "/loop 没有匹配的 loop".to_string(),
            });
        }
        line -= 1;

        loop {
            if let Some(inst) = script.get_instruction(line) {
                match inst.tag.as_str() {
                    "/loop" => depth += 1,
                    "loop" => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(line);
                        }
                    }
                    _ => {}
                }
            }
            if line == 0 {
                break;
            }
            line -= 1;
        }

        Err(crate::error::Error::RuntimeError {
            line: self.current_line,
            message: "/loop 没有匹配的 loop".to_string(),
        })
    }
}

/// 标签处理器 trait
pub trait TagHandler: Send + Sync {
    /// 执行标签
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult>;
}

/// 标签处理器注册表
pub struct TagRegistry {
    handlers: HashMap<String, Box<dyn TagHandler>>,
}

impl Default for TagRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TagRegistry {
    /// 创建新的注册表（包含内置处理器）
    pub fn new() -> Self {
        let mut registry = Self {
            handlers: HashMap::new(),
        };

        // 注册内置标签处理器
        registry.register("jump", JumpHandler);
        registry.register("call", CallHandler);
        registry.register("return", ReturnHandler);
        registry.register("stop", StopHandler);
        registry.register("wt", WtHandler);
        registry.register("wt0", Wt0Handler);
        registry.register("wait", WaitHandler);
        registry.register("exkey", ExkeyHandler);
        registry.register("var", VarHandler);
        registry.register("calllua", CallLuaHandler);
        registry.register("yesno", YesNoHandler);
        registry.register("dialog", DialogHandler);
        registry.register("exit", ExitHandler);
        registry.register("gotitle", GoTitleHandler);
        registry.register("uitrans", UiTransHandler);
        registry.register("loading", LoadingHandler);
        registry.register("saving", SavingHandler);
        registry.register("sysshow", SysShowHandler);
        registry.register("syshide", SysHideHandler);
        registry.register("syssave", SysSaveHandler);
        registry.register("lyc", LycHandler);
        registry.register("lyc2", Lyc2Handler);
        registry.register("lydel", LydelHandler);
        registry.register("lyprop", LypropHandler);
        registry.register("lyshader", LyshaderHandler);
        registry.register("se_saveok", SeSaveOkHandler);
        registry.register("se_loadok", SeLoadOkHandler);
        registry.register("se_exitok", SeExitOkHandler);
        registry.register("allsoundstop", AllSoundStopHandler);
        registry.register("alldelete", AllDeleteHandler);
        registry.register("loadmask", LoadMaskHandler);
        registry.register("repeatedly", RepeatedlyHandler);
        registry.register("autoskip_disable", AutoSkipDisableHandler);
        registry.register("reset", ResetHandler);
        registry.register("@", AtHandler);
        registry.register("tag", TagTagHandler);

        // 条件标签
        registry.register("if", IfHandler);
        registry.register("elseif", ElseifHandler);
        registry.register("else", ElseHandler);
        registry.register("/if", EndifHandler);
        registry.register("loop", LoopHandler);
        registry.register("/loop", EndloopHandler);

        // 宏标签
        registry.register("macroadd", MacroAddHandler);

        // 剧情标签
        registry.register("print", PrintHandler);
        registry.register("rt", RtHandler);
        registry.register("rp", RpHandler);
        registry.register("font", FontHandler);
        registry.register("font_close", FontCloseHandler);
        registry.register("/font", FontCloseHandler);
        registry.register("fontdefault", FontDefaultHandler);
        registry.register("fontinit", FontInitHandler);
        registry.register("ruby", RubyHandler);
        registry.register("/ruby", RubyEndHandler);
        registry.register("link", LinkHandler);
        registry.register("/link", LinkEndHandler);
        registry.register("linkdisable", LinkDisableHandler);
        registry.register("linkenable", LinkEnableHandler);
        registry.register("glyph", GlyphHandler);
        registry.register("chgmsg", ChgmsgHandler);
        registry.register("chgmsg_close", ChgmsgCloseHandler);
        registry.register("/chgmsg", ChgmsgCloseHandler);
        registry.register("scetween", ScetweenHandler);
        registry.register("scein", SceinHandler);
        registry.register("sceout", SceoutHandler);
        registry.register("automode", AutomodeHandler);
        registry.register("skip", SkipHandler);
        registry.register("backlog", BacklogHandler);
        registry.register("hide", HideHandler);
        registry.register("alreadyread", AlreadyreadHandler);
        registry.register("writebacklog", WritebacklogHandler);
        registry.register("indent", IndentHandler);
        registry.register("prohibit", ProhibitHandler);
        registry.register("wordparts", WordpartsHandler);

        // 音频标签
        registry.register("splay", SplayHandler);
        registry.register("sstop", SstopHandler);
        registry.register("sfade", SfadeHandler);
        registry.register("span", SpanHandler);
        registry.register("sxfade", SxfadeHandler);
        registry.register("seplay", SeplayHandler);
        registry.register("sestop", SestopHandler);
        registry.register("sefade", SefadeHandler);
        registry.register("sepan", SepanHandler);
        registry.register("voice", VoiceHandler);
        registry.register("/voice", VoiceEndHandler);
        registry.register("setonsoundfinish", SetOnSoundFinishHandler);
        registry.register("delonsoundfinish", DelOnSoundFinishHandler);
        registry.register("sefadein", SefadeinHandler);
        registry.register("sefadeout", SefadeoutHandler);
        registry.register("sfadein", SfadeinHandler);
        registry.register("sfadeout", SfadeoutHandler);

        // 系统操作标签
        registry.register("exec", ExecHandler);
        registry.register("save", SaveHandler);
        registry.register("load", LoadHandler);
        registry.register("debug", DebugHandler);
        registry.register("debugprint", DebugprintHandler);
        registry.register("debugreload", DebugreloadHandler);
        registry.register("caption", CaptionHandler);
        registry.register("mouse", MouseHandler);
        registry.register("keyconfig", KeyconfigHandler);
        registry.register("file", FileHandler);
        registry.register("httpget", HttpgetHandler);
        registry.register("httppost", HttppostHandler);
        registry.register("openbrowser", OpenbrowserHandler);
        registry.register("autosave", AutosaveHandler);
        registry.register("avoid", AvoidHandler);
        registry.register("vibrate", VibrateHandler);
        registry.register("statusbar", StatusbarHandler);
        registry.register("purchase", PurchaseHandler);
        registry.register("callnative", CallnativeHandler);
        // 弃用标签：显式空转，消除 Event::Custom 回退噪音
        registry.register("slider", LegacyNoopHandler);
        registry.register("uidel", LegacyNoopHandler);

        // 事件处理器标签
        registry.register("setonpush", SetOnPushHandler);
        registry.register("setonautomodein", SetOnAutomodeInHandler);
        registry.register("setonautomodeout", SetOnAutomodeOutHandler);
        registry.register("setonbacklogin", SetOnBacklogInHandler);
        registry.register("setonbacklogout", SetOnBacklogOutHandler);
        registry.register("setoncommandskipin", SetOnCommandSkipInHandler);
        registry.register("setoncommandskipout", SetOnCommandSkipOutHandler);
        registry.register("setoncontrolskipin", SetOnControlSkipInHandler);
        registry.register("setoncontrolskipout", SetOnControlSkipOutHandler);
        registry.register("setondirchg", SetOnDirchgHandler);
        registry.register("setonhidein", SetOnHideInHandler);
        registry.register("setonhideout", SetOnHideOutHandler);
        registry.register("setonwindowbutton", SetOnWindowButtonHandler);
        registry.register("delonpush", DelOnPushHandler);
        registry.register("delonautomodein", DelOnAutomodeInHandler);
        registry.register("delonautomodeout", DelOnAutomodeOutHandler);
        registry.register("delonbacklogin", DelOnBacklogInHandler);
        registry.register("delonbacklogout", DelOnBacklogOutHandler);
        registry.register("deloncommandskipin", DelOnCommandSkipInHandler);
        registry.register("deloncommandskipout", DelOnCommandSkipOutHandler);
        registry.register("deloncontrolskipin", DelOnControlSkipInHandler);
        registry.register("deloncontrolskipout", DelOnControlSkipOutHandler);
        registry.register("delondirchg", DelOnDirchgHandler);
        registry.register("delonhidein", DelOnHideInHandler);
        registry.register("delonhideout", DelOnHideOutHandler);
        registry.register("delonwindowbutton", DelOnWindowButtonHandler);

        // legacy seton*/delon* 别名（向后兼容）：转发到 lyevent 机制
        registry.register("setonclick", SetOnClickHandler);
        registry.register("setondrag", SetOnDragHandler);
        registry.register("setondragin", SetOnDragInHandler);
        registry.register("setondragout", SetOnDragOutHandler);
        registry.register("setonrollout", SetOnRolloutHandler);
        registry.register("setonrollover", SetOnRolloverHandler);
        registry.register("delonclick", DelOnClickHandler);
        registry.register("delondrag", DelOnDragHandler);
        registry.register("delondragin", DelOnDragInHandler);
        registry.register("delondragout", DelOnDragOutHandler);
        registry.register("delonrollout", DelOnRolloutHandler);
        registry.register("delonrollover", DelOnRolloverHandler);

        // 图层高级标签
        registry.register("lytween", LytweenHandler);
        registry.register("lytweendel", LytweendelHandler);
        registry.register("tweenset", TweensetHandler);
        registry.register("/tweenset", TweensetEndHandler);
        registry.register("lyevent", LyeventHandler);
        registry.register("lyrename", LyrenameHandler);
        registry.register("lyedit", LyeditHandler);
        registry.register("lydrag", LydragHandler);
        registry.register("anime", AnimeHandler);
        registry.register("video", VideoHandler);
        registry.register("setonvideofinish", SetOnVideofinishHandler);
        registry.register("delonvideofinish", DelOnVideofinishHandler);
        registry.register("trans", TransHandler);
        registry.register("flip", FlipHandler);
        registry.register("takess", TakessHandler);
        registry.register("savess", SavessHandler);
        registry.register("rclick", RclickHandler);
        registry.register("macrodel", MacrodelHandler);

        registry
    }

    /// 注册标签处理器
    pub fn register<H: TagHandler + 'static>(&mut self, name: &str, handler: H) {
        self.handlers.insert(name.to_string(), Box::new(handler));
    }

    /// 获取标签处理器
    pub fn get(&self, name: &str) -> Option<&dyn TagHandler> {
        self.handlers.get(name).map(|h| h.as_ref())
    }

    /// 检查是否已注册
    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }
}

// ── 控制流标签 ──────────────────────────────────────────────────

/// [jump] 跳转标签
struct JumpHandler;

impl TagHandler for JumpHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let label = ctx.instruction.get("label").unwrap_or("");

        // 检查条件跳转
        if let Some(cond) = ctx.instruction.get("cond") {
            let value = ctx.evaluator().resolve_param(&format!("${}", cond))?;
            if !value.as_bool() {
                return Ok(TagResult::Continue);
            }
        }

        // 跨脚本跳转
        if let Some(file) = ctx.instruction.get("file") {
            return Ok(TagResult::JumpExternal {
                file: file.to_string(),
                label: label.to_string(),
            });
        }

        // 查找标签行号
        if let Some(script) = ctx.get_script(ctx.current_script) {
            if let Some(line) = script.get_label_line(label) {
                return Ok(TagResult::Jump(line));
            }
        }

        Err(crate::error::Error::LabelNotFound(label.to_string()))
    }
}

/// [call] 调用标签
struct CallHandler;

impl TagHandler for CallHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.instruction.get("file").map(String::from);
        let label = ctx.instruction.get("label").unwrap_or("").to_string();

        Ok(TagResult::Call {
            file,
            label,
            return_line: ctx.current_line + 1,
            return_script: ctx.current_script.to_string(),
        })
    }
}

/// [return] 返回标签
struct ReturnHandler;

impl TagHandler for ReturnHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Return)
    }
}

/// [stop] 停止标签
struct StopHandler;

impl TagHandler for StopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let reason = ctx.instruction.get("0").map(String::from);
        if std::env::var("ASB_TRACE_STOP").is_ok() {
            eprintln!("[STOP] reason={:?} line={}", reason, ctx.instruction.line);
        }
        Ok(TagResult::Wait(Event::Wait {
            reason: crate::event::WaitReason::Stop { reason },
        }))
    }
}

/// [wt] 等待标签
struct WtHandler;

impl TagHandler for WtHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // [wt] 是脚本宏 tags.wt → syswait → [wait input=1 time=0]（见游戏的
        // system/adv/func.lua）。它是"让出一帧后按计时推进"的等待，缺省 time=0
        // 表示下一帧立即推进（加载序列里用作 yield 点），而非等待点击。故与
        // [wait] 一样产出 Timed，绝不能截成会阻塞点击的 Generic。
        let time = ctx.resolve_param("time")?.as_int().unwrap_or(0) as u64;
        let input = ctx.resolve_param("input")?.as_int().unwrap_or(1) as i32;
        Ok(TagResult::Wait(Event::Wait {
            reason: crate::event::WaitReason::Timed {
                milliseconds: time,
                input,
            },
        }))
    }
}

/// [wt0] 等待标签（变体）
struct Wt0Handler;

impl TagHandler for Wt0Handler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Wait(Event::Wait {
            reason: crate::event::WaitReason::Generic0,
        }))
    }
}

/// [wait] 时间等待标签
struct WaitHandler;

impl TagHandler for WaitHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx.resolve_param("time")?.as_int().unwrap_or(0) as u64;
        let input = ctx.resolve_param("input")?.as_int().unwrap_or(0) as i32;
        // 文档 wait.md：指定 scenario 或 video 参数时 time 被忽略；
        // se 与 time 并用时表示从该 SE 开始播放的时刻起算的毫秒数。
        let reason = if let Some(video) = ctx
            .instruction
            .get("video")
            .filter(|v| !v.is_empty())
        {
            crate::event::WaitReason::VideoLayer {
                id: video.to_string(),
            }
        } else if let Some(mode) = ctx
            .instruction
            .get("scenario")
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|m| *m == 1 || *m == 2)
        {
            crate::event::WaitReason::ScenarioTween { mode }
        } else if let Some(se) = ctx.instruction.get("se").filter(|v| !v.is_empty()) {
            crate::event::WaitReason::Se {
                id: se.to_string(),
                time: ctx.instruction.get("time").is_some().then_some(time),
            }
        } else {
            crate::event::WaitReason::Timed {
                milliseconds: time,
                input,
            }
        };
        Ok(TagResult::Wait(Event::Wait { reason }))
    }
}

/// [exkey] 按键等待标签
struct ExkeyHandler;

impl TagHandler for ExkeyHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut buttons = Vec::new();

        // 收集所有按钮参数
        if let Some(btn) = ctx.instruction.get("btn") {
            buttons.push(btn.to_string());
        }

        // 收集数字键参数
        for i in 0..10 {
            if let Some(btn) = ctx.instruction.get(&i.to_string()) {
                buttons.push(btn.to_string());
            }
        }

        Ok(TagResult::Wait(Event::Wait {
            reason: crate::event::WaitReason::KeyWait { buttons },
        }))
    }
}

/// [var] 变量设置标签
struct VarHandler;

impl TagHandler for VarHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        var_handler::apply_var_tag(&ctx.instruction.params, ctx.variables)?;
        Ok(TagResult::Continue)
    }
}

/// [yesno] 是/否选择标签
struct YesNoHandler;

impl TagHandler for YesNoHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.instruction.get("file").unwrap_or("").to_string();
        let se = ctx.instruction.get("se").map(String::from);

        Ok(TagResult::Wait(Event::YesNo { file, se }))
    }
}

/// [dialog] 对话框标签
struct DialogHandler;

impl TagHandler for DialogHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let title = ctx.resolve_param("title")?.as_string();
        let message = ctx.resolve_param("message")?.as_string();
        let varname = ctx
            .instruction
            .get("varname")
            .filter(|value| !value.is_empty())
            .map(String::from);
        let textfield = ctx
            .instruction
            .get("textfield")
            .filter(|value| !value.is_empty())
            .map(String::from);
        let textfield_size = ctx
            .resolve_param("textfieldsize")?
            .as_string()
            .parse::<usize>()
            .ok()
            .filter(|size| *size > 0);

        Ok(TagResult::Wait(Event::ShowDialog {
            title,
            message,
            varname,
            textfield,
            textfield_size,
        }))
    }
}

/// [exit] 退出标签
struct ExitHandler;

impl TagHandler for ExitHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        println!("Exit called");
        Ok(TagResult::Emit(Event::Exit))
    }
}

/// [gotitle] 返回标题标签
struct GoTitleHandler;

impl TagHandler for GoTitleHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::GoTitle))
    }
}

/// [reset] 重启引擎标签
///
/// 文档语义是整个引擎回到初始启动状态并从 boot 脚本重跑。解释器侧先清
/// local/temp 变量域，再发 [`Event::Reset`] 交由宿主重置合成器/音频/控制
/// 状态并重新走 boot 管线。
struct ResetHandler;

impl TagHandler for ResetHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        ctx.variables.reset();
        Ok(TagResult::Emit(Event::Reset))
    }
}

/// [@] 特殊标签
struct AtHandler;

impl TagHandler for AtHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // [@] 通常用于暂停等待点击
        Ok(TagResult::Wait(Event::Wait {
            reason: crate::event::WaitReason::Generic,
        }))
    }
}

/// [tag] 执行任意标签
///
/// 格式：`[tag data="tagname,key1,val1,key2,val2,..."]`
/// 逗号分隔，第一个是标签名，后续是 key,value 对。
/// 转义字符为反斜杠。
struct TagTagHandler;

impl TagHandler for TagTagHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let data =
            ctx.instruction
                .get("data")
                .ok_or_else(|| crate::error::Error::RuntimeError {
                    line: ctx.current_line,
                    message: "tag 标签缺少 data 参数".to_string(),
                })?;

        // 解析逗号分隔的参数，支持反斜杠转义
        let parts = split_tag_data(data);
        if parts.is_empty() {
            return Err(crate::error::Error::RuntimeError {
                line: ctx.current_line,
                message: "tag 标签 data 为空".to_string(),
            });
        }

        let tag_name = parts[0].clone();
        let mut params = HashMap::new();

        // 后续元素是 key,value,key,value,...
        let mut i = 1;
        while i + 1 < parts.len() {
            params.insert(parts[i].clone(), parts[i + 1].clone());
            i += 2;
        }

        let instruction = Instruction {
            tag: tag_name,
            params,
            line: ctx.current_line,
        };

        Ok(TagResult::Dynamic(instruction))
    }
}

/// 解析 tag 标签的 data 参数（逗号分隔，支持反斜杠转义）
fn split_tag_data(data: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = data.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // 转义：下一个字符原样添加
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' => {
                parts.push(current.clone());
                current.clear();
            }
            _ => {
                current.push(c);
            }
        }
    }
    parts.push(current);
    parts
}

/// [repeatedly] 重复执行标签
struct RepeatedlyHandler;

impl TagHandler for RepeatedlyHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::Repeatedly))
    }
}

/// [autoskip_disable] 禁用自动跳过标签
struct AutoSkipDisableHandler;

impl TagHandler for AutoSkipDisableHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::AutoSkipDisable))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        CallbackResult, Event, ExecutionResult, Interpreter, InterpreterConfig, WaitReason,
    };

    fn run_wait(params: &str) -> WaitReason {
        let script = format!("*main\n[wait {params}]\n[return]\n");
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter.load_script("test", &script).unwrap();
        interpreter.start("test", "main").unwrap();

        match interpreter.run().unwrap() {
            ExecutionResult::Wait(Event::Wait { reason }) => reason,
            result => panic!("expected wait, got {result:?}"),
        }
    }

    #[test]
    fn wait_preserves_input_policy() {
        assert!(matches!(
            run_wait("time=\"2500\" input=\"1\""),
            WaitReason::Timed {
                milliseconds: 2500,
                input: 1,
            }
        ));
        assert!(matches!(
            run_wait("time=\"2500\""),
            WaitReason::Timed {
                milliseconds: 2500,
                input: 0,
            }
        ));
    }

    #[test]
    fn wait_parses_se_video_and_scenario_sources() {
        // wait.md：se=SE 的 ID，等待其播放结束；与 time 并用时从 SE 开播起算
        match run_wait("se=\"bar\"") {
            WaitReason::Se { id, time } => {
                assert_eq!(id, "bar");
                assert_eq!(time, None);
            }
            other => panic!("expected Se, got {other:?}"),
        }
        match run_wait("se=\"bar\" time=\"1500\"") {
            WaitReason::Se { id, time } => {
                assert_eq!(id, "bar");
                assert_eq!(time, Some(1500));
            }
            other => panic!("expected Se, got {other:?}"),
        }

        // video=层 ID，等待视频层播放结束；指定时 time 被忽略
        match run_wait("video=\"mv\" time=\"999\"") {
            WaitReason::VideoLayer { id } => assert_eq!(id, "mv"),
            other => panic!("expected VideoLayer, got {other:?}"),
        }

        // scenario=1/2 等待场景文本 Tween；指定时 time 被忽略
        assert!(matches!(
            run_wait("scenario=\"1\" time=\"999\""),
            WaitReason::ScenarioTween { mode: 1 }
        ));
        assert!(matches!(
            run_wait("scenario=\"2\""),
            WaitReason::ScenarioTween { mode: 2 }
        ));
        // scenario=0（缺省语义）不构成 Tween 等待，退回 Timed
        assert!(matches!(
            run_wait("scenario=\"0\" time=\"100\""),
            WaitReason::Timed {
                milliseconds: 100,
                ..
            }
        ));
    }

    #[test]
    fn dialog_preserves_host_response_contract() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::ShowDialog { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                r#"
*main
[dialog title="Name" message="Input" varname="accepted" textfield="player" textfieldsize="10"]
"#,
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        match interpreter.run().unwrap() {
            ExecutionResult::Wait(Event::ShowDialog {
                title,
                message,
                varname,
                textfield,
                textfield_size,
            }) => {
                assert_eq!(title, "Name");
                assert_eq!(message, "Input");
                assert_eq!(varname.as_deref(), Some("accepted"));
                assert_eq!(textfield.as_deref(), Some("player"));
                assert_eq!(textfield_size, Some(10));
            }
            result => panic!("expected dialog, got {result:?}"),
        }
    }
}
