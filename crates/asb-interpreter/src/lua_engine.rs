//! Lua Engine API
//!
//! 定义注入到 Lua 环境的 engine 对象（`e`）。
//! 每个通过 calllua 调用的 Lua 函数第一个参数都是 engine 对象。
//!
//! 大部分方法的实际行为需要由宿主应用（游戏引擎）通过回调提供。

use mlua::{Lua, Result as LuaResult, UserData, UserDataMethods, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) const TAG_FILTER_REGISTRY_KEY: &str = "__art3m1s_tag_filter";

/// A typed command sent from an Artemis E-Mote layer userdata to the host.
#[derive(Clone, Debug, PartialEq)]
pub enum EmoteLayerCommand {
    SetScale {
        scale: f32,
        origin_x: f32,
        origin_y: f32,
    },
    SetCoord {
        x: f32,
        y: f32,
        z: f32,
        angle: f32,
    },
    SetVariable {
        label: String,
        value: f32,
        frames: f32,
        easing: u32,
    },
    PlayTimeline {
        label: String,
        flags: u32,
    },
    FadeInTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    FadeOutTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    StopTimeline {
        label: String,
    },
    Pass,
    Step,
    Skip,
}

fn unsupported_emote() -> crate::Error {
    crate::Error::RuntimeError {
        line: 0,
        message: "E-Mote host callbacks are not installed".to_string(),
    }
}

/// 引擎事件回调 trait
///
/// 宿主应用实现此 trait 来响应 engine 对象的方法调用。
pub trait EngineCallbacks: Send + Sync {
    /// 输出调试日志
    fn debug(&self, level: i32, data: &str, raw: bool);

    /// 执行标签（返回标签名和参数）
    fn enqueue_tag(&self, tag: String, params: HashMap<String, String>);

    /// 设置事件处理器
    fn set_event_handler(&self, handlers: HashMap<String, String>);

    /// 获取脚本状态（0=执行中, 1=等待点击, 3=停止, 等）
    fn get_script_status(&self) -> u8;

    /// 检查按键是否按下
    fn is_key_down(&self, key_id: u32) -> bool;

    /// 检查按键是否刚按下
    fn is_key_down_edge(&self, key_id: u32) -> bool;

    /// 检查按键是否刚松开
    fn is_key_up_edge(&self, key_id: u32) -> bool;

    /// 检查确认键
    fn is_decide(&self) -> bool;

    /// 获取鼠标位置
    fn get_mouse_point(&self) -> (i32, i32);

    /// 获取触摸点数量
    fn get_touch_count(&self) -> u32;

    /// 获取触摸点位置
    fn get_touch_point(&self, index: u32) -> (i32, i32);

    /// 获取全部触摸点 (id, x, y)，供 getTouchPoint() 无参表形态。
    /// 缺省退化为按序号枚举、以序号充当 id（无真实触摸 id 的后端够用）。
    fn get_touch_points(&self) -> Vec<(u32, i32, i32)> {
        (0..self.get_touch_count())
            .map(|i| {
                let (x, y) = self.get_touch_point(i);
                (i, x, y)
            })
            .collect()
    }

    /// 检查文件是否存在
    fn is_file_exists(&self, path: &str) -> bool;

    /// 写文件。Luau 后端的 `io.open(path, "wb")` polyfill 使用该回调落盘。
    fn file_write(&self, _path: &str, _data: &[u8]) -> crate::Result<()> {
        Err(crate::Error::IoError(std::io::Error::other(
            "file_write callback not implemented",
        )))
    }

    /// 文件操作
    fn file_operation(&self, command: &str, params: HashMap<String, String>);

    /// 加载 Lua 文件
    fn include(&self, path: &str);

    /// 覆盖按键
    fn override_key(&self, from: u32, to: u32);

    /// 覆盖按键（完整语义，docs/lua/engine/overrideKey.txt）：
    /// - `key=None` 表示覆盖所有键；
    /// - `status=None` 表示取消覆盖；
    /// - `status` 为位集合：2=isPush 4=isDown 8=isDownEdge 16=isUpEdge 32=isDecide。
    ///
    /// 默认实现退化转发到旧的 [`override_key`](Self::override_key)。
    fn override_key_status(&self, key: Option<u32>, status: Option<u32>) {
        self.override_key(key.unwrap_or(0), status.unwrap_or(0));
    }

    /// e:isPush 的按键重复语义：按下瞬间 true → 0.5s 内 false → 0.5s 后持续
    /// true。默认实现退化为 isDownEdge（无长按重复）。
    fn is_push(&self, key_id: u32) -> bool {
        self.is_key_down_edge(key_id)
    }

    /// e:isDecide 的按键编号形态。平台相关：Windows 上等价该键的 isDownEdge，
    /// 移动端为 isUpEdge。默认实现忽略键编号退化到 [`is_decide`](Self::is_decide)。
    fn is_decide_key(&self, key_id: u32) -> bool {
        let _ = key_id;
        self.is_decide()
    }

    /// 设置滑动灵敏度
    fn set_flick_sensitivity(&self, sensitivity: f64);

    /// 获取脚本块信息
    fn get_script_block(&self) -> HashMap<String, String>;

    /// 获取脚本栈信息
    fn get_script_stack(&self) -> Vec<HashMap<String, String>>;

    /// 获取脚本等待原因
    fn get_script_wait_reason(&self) -> u8;

    // 查询图层信息，供 `var system="get_layer_info"` 这类同步查询使用。
    fn get_layer_info(&self, _id: &str) -> Option<HashMap<String, String>> {
        None
    }

    /// 枚举全部图层信息（id 升序），供省略 id 的 `var system="get_layer_info"`
    /// 构造伪数组 / style=map 伪关联数组。默认无图层。
    fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
        Vec::new()
    }

    /// 枚举宿主可用字体族（`var system="get_font"`）。参数对应 monospace /
    /// vertical 过滤。默认空列表（非 Windows 行为）。
    fn get_font_list(&self, _monospace: bool, _vertical: bool) -> Vec<String> {
        Vec::new()
    }

    /// 查询宿主窗口状态 `(fullscreen, minimized)`，供 `var system="fullscreen"` /
    /// `"minimize"` 使用。默认 `(false, false)`（非 Windows 行为）。
    fn get_window_state(&self) -> (bool, bool) {
        (false, false)
    }

    // ----------------------------------------------------------------
    // 以下为 boot 流程所需、暂以默认实现（no-op / 合理默认值）提供的回调。
    // 接入真实渲染/资源后端时，宿主可覆盖这些方法。
    // ----------------------------------------------------------------

    /// 设置脚本状态（如 4=停止）。
    fn set_script_status(&self, _status: u8) {}

    /// 注册标签过滤表（脚本侧保留该表引用，宿主据此过滤标签）。
    fn set_tag_filter(&self) {}

    /// 注册事件过滤表。
    fn set_event_filter(&self) {}

    /// 注册日志过滤函数（脚本侧函数存于 registry；宿主据此在日志路径上调用）。
    fn set_log_filter(&self) {}

    /// 注册魔法路径别名（name -> path）。
    fn set_magic_path(&self, _name: &str, _path: &str) {}

    /// 设置多点触控模式。
    fn set_use_multi_touch(&self, _mode: i64) {}

    /// 设置是否启用触摸长按。
    fn set_use_touch_hold(&self, _enabled: bool) {}

    /// 调试跳转到指定脚本索引。
    fn debug_skip(&self, _index: i64) {}

    /// 文本编码转换（docs/lua/engine/convertEncoding.txt）。
    ///
    /// 以字节为单位处理——Lua 字符串是字节串，SJIS/EUC/JIS 源数据不是合法
    /// UTF-8，不能经 `String` 中转。`from` 为空串时自动识别源编码。
    /// 默认实现用 encoding_rs 做真实转换。
    fn convert_encoding(&self, from: &str, to: &str, source: &[u8]) -> Vec<u8> {
        convert_encoding_default(from, to, source)
    }

    /// 执行外部 shell 命令（如打开 URL/文件）。

    // ── Audio volume ──────────────────────────────────────────

    fn set_master_volume(&self, _volume: f32) {}
    fn set_bgm_volume(&self, _volume: f32) {}
    fn set_se_volume(&self, _volume: f32) {}
    fn set_voice_volume(&self, _volume: f32) {}

    /// 执行外部 shell 命令（如打开 URL/文件），返回被执行程序的返回值。
    ///
    /// 文档（callShellExecute.txt）：阻塞方式运行时返回被执行程序的返回值，
    /// 非阻塞方式返回值不确定。默认实现返回 0。
    fn call_shell_execute(&self, _file: &str, _params: HashMap<String, String>) -> i32 {
        0
    }

    /// 将字符串写入系统剪贴板（仅 Windows，docs/lua/engine/writeClipboard.txt）。
    /// 默认无操作。
    fn write_clipboard(&self, _text: &str) {}

    /// 恢复字体缓存。
    fn restore_font_cache(&self, _path: &str) {}

    /// 读取 PNG 文件的注释块（如立绘坐标）。默认无注释返回 None。
    fn load_png_comments(&self, _path: &str) -> Option<HashMap<String, String>> {
        None
    }

    /// 同步绑定 surface（阻塞直到加载完成）。
    fn bind_surface(&self, _key: &str) {}

    /// 异步绑定（预加载）surface。
    fn bind_surface_async(&self, _key: &str) {}

    /// 解绑（释放）surface。
    fn unbind_surface(&self, _key: &str) {}

    /// 清空 surface 加载队列。
    fn clear_surface_load_queue(&self) {}

    /// 是否仍有 surface 在异步加载中。默认 false（无后端即视为加载完成）。
    fn is_loading_surface(&self) -> bool {
        false
    }

    /// 按路径查询 surface 是否在加载/排队中；`None` 表示检查整个加载队列
    /// （docs/lua/engine/isLoadingSurface.txt）。默认退化到无参版本。
    fn is_loading_surface_path(&self, _path: Option<&str>) -> bool {
        self.is_loading_surface()
    }

    /// Creates the pending E-Mote model for a scene layer.
    fn create_emote_layer(
        &self,
        _id: &str,
        _files: &[String],
        _width: u32,
        _height: u32,
    ) -> crate::Result<bool> {
        Err(unsupported_emote())
    }

    /// Resolves the requested E-Mote layer and returns its actual slot selector.
    fn get_emote_layer(&self, _id: &str, _next: bool) -> Option<bool> {
        None
    }

    /// Applies one operation to the current or pending E-Mote layer.
    fn command_emote_layer(
        &self,
        _id: &str,
        _next: bool,
        _command: EmoteLayerCommand,
    ) -> crate::Result<()> {
        Err(unsupported_emote())
    }
}

/// 默认的引擎回调实现（所有方法为空操作）
pub struct DefaultEngineCallbacks;

impl EngineCallbacks for DefaultEngineCallbacks {
    fn debug(&self, _level: i32, _data: &str, _raw: bool) {}
    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
    fn get_script_status(&self) -> u8 {
        0
    }
    fn is_key_down(&self, _key_id: u32) -> bool {
        false
    }
    fn is_key_down_edge(&self, _key_id: u32) -> bool {
        false
    }
    fn is_key_up_edge(&self, _key_id: u32) -> bool {
        false
    }
    fn is_decide(&self) -> bool {
        false
    }
    fn get_mouse_point(&self) -> (i32, i32) {
        (0, 0)
    }
    fn get_touch_count(&self) -> u32 {
        0
    }
    fn get_touch_point(&self, _index: u32) -> (i32, i32) {
        (0, 0)
    }
    fn is_file_exists(&self, _path: &str) -> bool {
        false
    }
    fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
    fn include(&self, _path: &str) {}
    fn override_key(&self, _from: u32, _to: u32) {}
    fn set_flick_sensitivity(&self, _sensitivity: f64) {}
    fn get_script_block(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
        vec![]
    }
    fn get_script_wait_reason(&self) -> u8 {
        0
    }
}

/// 共享的引擎上下文
pub struct EngineContext {
    pub callbacks: Box<dyn EngineCallbacks + Send + Sync>,
    /// 待执行的标签队列
    pub tag_queue: Vec<(String, HashMap<String, String>)>,
    /// 待设置的事件处理器
    pub event_handlers: HashMap<String, String>,
    /// 事件过滤器函数（由 e:setEventFilter 设置）
    /// 当输入事件发生时，引擎调用此过滤器，脚本决定如何处理
    pub event_filter: Option<mlua::RegistryKey>,
    /// 日志过滤器函数（由 e:setLogFilter 设置）。
    /// 日志输出前调用此函数：返回 0 输出原始日志，返回 1 抑制原始日志
    /// （docs/lua/engine/setLogFilter.txt）。core 侧日志路径经另一代理消费。
    pub log_filter: Option<mlua::RegistryKey>,
    /// 项目文件读取器，供 `e:include` 读取 Lua/数据文件。
    ///
    /// `e:include(path)` 的语义是”读取文件并在当前 Lua VM 中执行”，因此 include
    /// 必须能读到项目文件。回调拿不到 Lua VM，故这里直接持有读取器，由 `e:include`
    /// 闭包用 mlua 传入的 `lua` 句柄重入执行。宿主通过
    /// [`Interpreter::set_file_loader`](crate::Interpreter::set_file_loader) 注入。
    pub file_reader: Option<FileReader>,
    /// 共享的变量存储，供 `e:var(name)` 读取解释器写入的变量。
    ///
    /// 与 [`Interpreter`](crate::Interpreter) 持有同一个 `Arc<Mutex<_>>`。
    pub variables: Option<Arc<Mutex<crate::variable::VariableStore>>>,
    /// 解释器同步进来的脚本调用栈镜像（底→顶），末项为**当前执行位置**
    /// `(脚本名, 内部索引)`。每次进入 Lua 前由解释器刷新，供 e:getScriptStack
    /// / e:getScriptBlock 读取真实调用栈。
    pub script_stack: Vec<(String, usize)>,
    /// `e:setScriptStack` 请求的调用栈强制重写（与 `script_stack` 同构），
    /// 解释器在下一轮抽干标签队列前应用并清除。
    pub pending_stack_override: Option<Vec<(String, usize)>>,
    /// 已加载脚本的共享只读视图（脚本名 → Script），供 e:getScriptBlock 按
    /// `{file, index}` 查询指令块。与解释器共享同一份 `Arc<Script>`，无内存翻倍。
    pub scripts_view: HashMap<String, Arc<crate::script::Script>>,
    /// 当前脚本等待原因表（getScriptWaitReason 的数据源）。键按文档：
    /// time / textTween / textClearTween / sound / video；无等待时为 None。
    pub wait_reason_info: Option<HashMap<String, String>>,
}

/// 文件读取器：给定虚拟路径返回原始字节。与
/// [`ScriptFileLoader`](crate::event::ScriptFileLoader) 同型，但用 `Arc` 以便在
/// 解释器与 [`EngineContext`] 之间共享同一个加载器。
pub type FileReader = Arc<dyn Fn(&str) -> crate::error::Result<Vec<u8>> + Send + Sync>;

impl EngineContext {
    pub fn new(callbacks: Box<dyn EngineCallbacks + Send + Sync>) -> Self {
        Self {
            callbacks,
            tag_queue: Vec::new(),
            event_handlers: HashMap::new(),
            event_filter: None,
            log_filter: None,
            file_reader: None,
            variables: None,
            script_stack: Vec::new(),
            pending_stack_override: None,
            scripts_view: HashMap::new(),
            wait_reason_info: None,
        }
    }
}

/// Engine API 对象（注入到 Lua 的第一个参数）
pub struct EngineApi {
    ctx: Arc<Mutex<EngineContext>>,
}

impl EngineApi {
    pub fn new(ctx: Arc<Mutex<EngineContext>>) -> Self {
        Self { ctx }
    }
    // 事件过滤器的实际派发已上移到 Interpreter::run_event_filter（宿主在把命中的
    // 事件交给处理器之前调用），那里同时持有 Lua 句柄与 EngineContext。
}

/// Script-visible E-Mote layer handle.
///
/// The userdata intentionally stores only a logical layer id and current/next
/// selector. Model ownership and rendering stay in the host runtime.
pub struct EmoteLayerApi {
    ctx: Arc<Mutex<EngineContext>>,
    id: String,
    next: bool,
}

impl EmoteLayerApi {
    fn command(&self, command: EmoteLayerCommand) -> LuaResult<()> {
        let ctx = self.ctx.lock().unwrap();
        ctx.callbacks
            .command_emote_layer(&self.id, self.next, command)
            .map_err(mlua::Error::external)
    }
}

impl UserData for EmoteLayerApi {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method(
            "setScale",
            |_lua, this, (scale, origin_x, origin_y): (f32, f32, f32)| {
                this.command(EmoteLayerCommand::SetScale {
                    scale,
                    origin_x,
                    origin_y,
                })
            },
        );
        methods.add_method(
            "setCoord",
            |_lua, this, (x, y, z, angle): (f32, f32, f32, f32)| {
                this.command(EmoteLayerCommand::SetCoord { x, y, z, angle })
            },
        );
        methods.add_method(
            "setVariable",
            |_lua, this, (label, value, frames, easing): (String, f32, f32, u32)| {
                this.command(EmoteLayerCommand::SetVariable {
                    label,
                    value,
                    frames,
                    easing,
                })
            },
        );
        methods.add_method(
            "playTimeline",
            |_lua, this, (label, flags): (String, u32)| {
                this.command(EmoteLayerCommand::PlayTimeline { label, flags })
            },
        );
        methods.add_method(
            "fadeInTimeline",
            |_lua, this, (label, frames, easing): (String, f32, u32)| {
                this.command(EmoteLayerCommand::FadeInTimeline {
                    label,
                    frames,
                    easing,
                })
            },
        );
        methods.add_method(
            "fadeOutTimeline",
            |_lua, this, (label, frames, easing): (String, f32, u32)| {
                this.command(EmoteLayerCommand::FadeOutTimeline {
                    label,
                    frames,
                    easing,
                })
            },
        );
        methods.add_method("stopTimeline", |_lua, this, label: String| {
            this.command(EmoteLayerCommand::StopTimeline { label })
        });
        methods.add_method("pass", |_lua, this, _: ()| {
            this.command(EmoteLayerCommand::Pass)
        });
        methods.add_method("step", |_lua, this, _: ()| {
            this.command(EmoteLayerCommand::Step)
        });
        methods.add_method("skip", |_lua, this, _: ()| {
            this.command(EmoteLayerCommand::Skip)
        });
    }
}

impl UserData for EngineApi {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // e:debug{level=0, data="foo", raw=false} 或 e:debug("foo")
        methods.add_method("debug", |_lua, this, args: mlua::MultiValue| {
            let ctx = this.ctx.lock().unwrap();
            if args.is_empty() {
                return Ok(());
            }
            let first = args.iter().next().unwrap();
            match first {
                Value::String(s) => {
                    let data = s.to_str().map(|b| b.to_string()).unwrap_or_default();
                    ctx.callbacks.debug(0, &data, false);
                }
                Value::Table(t) => {
                    let level: i32 = t.get("level").unwrap_or(0);
                    let data: String = t.get("data").unwrap_or_default();
                    let raw: bool = t.get("raw").unwrap_or(false);
                    ctx.callbacks.debug(level, &data, raw);
                }
                _ => {}
            }
            Ok(())
        });

        // e:tag{"lyc", id="0", file="bg"}
        methods.add_method("tag", |_lua, this, args: mlua::MultiValue| {
            if let Some(Value::Table(t)) = args.into_iter().next() {
                let tag_name: String = t.get(1).unwrap_or_default();
                let mut params = HashMap::new();
                // 用 Value 迭代，再手动转 String。mlua 的 `pairs::<String,String>()`
                // 一旦遇到非字符串值（如 boolean）就会终止迭代，后面的键全部丢失。
                // Lua 脚本传参经常带布尔字段（se=false 等），必须跳过而非中断。
                for pair in t.pairs::<mlua::Value, mlua::Value>() {
                    if let Ok((k, v)) = pair {
                        let key_str = match k {
                            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                            Value::Integer(i) => Some(i.to_string()),
                            _ => None,
                        };
                        let val_str = match v {
                            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                            Value::Integer(i) => Some(i.to_string()),
                            Value::Number(n) => Some(n.to_string()),
                            Value::Boolean(b) => Some(b.to_string()),
                            _ => None,
                        };
                        if let (Some(ks), Some(vs)) = (key_str, val_str) {
                            params.insert(ks, vs);
                        }
                    }
                }
                // 移除数字键 1 (tag name)
                params.remove("1");
                let mut ctx = this.ctx.lock().unwrap();
                ctx.callbacks.enqueue_tag(tag_name.clone(), params.clone());

                // var 标签不产出事件，且脚本常在同一 Lua 函数内紧接着用 e:var 读回
                // 刚设的值。若只入队、等 calllua 返回后才 flush，就会读到 nil。故 var
                // 在此同步落值到共享存储，并且不入队（入队 flush 只会重复落值，对
                // random 等有副作用的变体反而有害）。其余标签照常入队走事件管线。
                if tag_name == "var" {
                    if let Some(vars) = &ctx.variables {
                        let mut store = vars.lock().unwrap();
                        // 需要宿主回调的 system 查询（get_layer_info / get_font /
                        // fullscreen / minimize）在此拦截；未命中则落回通用 var 处理。
                        let handled =
                            apply_system_var_query(ctx.callbacks.as_ref(), &params, &mut store)
                                .unwrap_or(false);

                        if !handled
                            && let Err(e) =
                                crate::tags::var_handler::apply_var_tag(&params, &mut store)
                        {
                            return Err(mlua::Error::external(format!("var 标签执行失败: {e}")));
                        }
                    }
                } else {
                    ctx.tag_queue.push((tag_name, params));
                }
            }
            Ok(())
        });

        // e:enqueueTag{"tagname", param1="val1"}
        methods.add_method("enqueueTag", |lua, this, args: mlua::MultiValue| {
            if let Some(Value::Table(t)) = args.into_iter().next() {
                let tag_name: String = t
                    .get::<Value>(1)
                    .ok()
                    .and_then(|v| match v {
                        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                        Value::Integer(i) => Some(i.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let mut params = HashMap::new();
                let mut real_param_table: Option<mlua::Table> = None;
                for pair in t.pairs::<Value, Value>() {
                    if let Ok((k, v)) = pair {
                        let key_str = match &k {
                            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                            Value::Integer(i) => Some(i.to_string()),
                            _ => None,
                        };
                        if let Some(ks) = key_str {
                            match v {
                                Value::Table(vt) if ks == "params" => {
                                    real_param_table = Some(vt.clone());
                                }
                                Value::Table(_) => {} // 其他表值跳过
                                _ => {
                                    let val_str = match v {
                                        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                                        Value::Integer(i) => Some(i.to_string()),
                                        Value::Number(n) => Some(n.to_string()),
                                        Value::Boolean(b) => Some(b.to_string()),
                                        _ => None,
                                    };
                                    if let Some(vs) = val_str {
                                        params.insert(ks, vs);
                                    }
                                }
                            }
                        }
                    }
                }
                params.remove("1");
                let mut ctx = this.ctx.lock().unwrap();
                ctx.callbacks.enqueue_tag(tag_name.clone(), params.clone());
                // calllua + 含表 params → 表数据无法序列化进 HashMap，同步执行，
                // 仿 e:tag 对 var 标签的特判。
                if tag_name == "calllua" {
                    if let Some(function_name) = params.get("function").cloned() {
                        drop(ctx);
                        if let Some(real_pt) = real_param_table {
                            let param_table =
                                lua.create_table().map_err(|e| mlua::Error::external(e))?;
                            for (k, v) in &params {
                                if k != "function" && k != "params" {
                                    let _ = param_table.set(k.as_str(), v.as_str());
                                }
                            }
                            for pair in real_pt.pairs::<Value, Value>() {
                                if let Ok((k, v)) = pair {
                                    let _ = param_table.set(k, v);
                                }
                            }
                            if let Err(e) = crate::tags::call_lua_function_with_table(
                                lua,
                                &function_name,
                                param_table,
                            ) {
                                return Err(mlua::Error::external(format!(
                                    "calllua 执行失败: {e}"
                                )));
                            }
                            return Ok(());
                        }
                        crate::tags::call_lua_function(lua, &function_name, &params)
                            .map_err(|e| mlua::Error::external(format!("calllua 执行失败: {e}")))?;
                        return Ok(());
                    }
                }
                ctx.tag_queue.push((tag_name, params));
            }
            Ok(())
        });

        // e:setEventHandler{onEnterFrame="func", ...}
        methods.add_method("setEventHandler", |_lua, this, args: mlua::MultiValue| {
            if let Some(Value::Table(t)) = args.into_iter().next() {
                let mut handlers = HashMap::new();
                for pair in t.pairs::<Value, Value>() {
                    if let Ok((k, v)) = pair {
                        let key_str = match k {
                            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                            Value::Integer(i) => Some(i.to_string()),
                            _ => None,
                        };
                        let val_str = match v {
                            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                            Value::Integer(i) => Some(i.to_string()),
                            Value::Number(n) => Some(n.to_string()),
                            Value::Boolean(b) => Some(b.to_string()),
                            _ => None,
                        };
                        if let (Some(ks), Some(vs)) = (key_str, val_str) {
                            handlers.insert(ks, vs);
                        }
                    }
                }
                let mut ctx = this.ctx.lock().unwrap();
                ctx.callbacks.set_event_handler(handlers.clone());
                ctx.event_handlers.extend(handlers);
            }
            Ok(())
        });

        // e:getScriptStatus()
        methods.add_method("getScriptStatus", |_lua, this, _: ()| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.get_script_status())
        });

        // e:random()
        methods.add_method("random", |_lua, _this, _: ()| Ok(rand_i64()));

        // e:now()
        methods.add_method("now", |_lua, _this, _: ()| Ok(now_millis()));

        // e:include("path")
        //
        // 读取项目文件并在**当前** Lua VM 中执行——这是 include 的真正语义
        // （注册函数、建表等副作用必须落在同一个 VM 里）。注意：被 include 的
        // Lua 代码会反过来调用 `e:` 方法（如 e:tag/e:var/e:include 嵌套），这些都会
        // 再次锁 ctx，所以必须先克隆出 file_reader 并释放锁，再读文件、再 exec，
        // 否则会自锁死。
        methods.add_method("include", |lua, this, path: String| {
            let reader = {
                let ctx = this.ctx.lock().unwrap();
                // 仍通知回调（宿主可借此记录/追踪 include），但实际加载在此处完成。
                ctx.callbacks.include(&path);
                ctx.file_reader.clone()
            };

            let Some(reader) = reader else {
                // 未注入读取器：保持旧的“仅通知回调”行为，不视为错误。
                return Ok(());
            };

            let bytes = reader(&path)
                .map_err(|e| mlua::Error::external(format!("include 读取 {path} 失败: {e}")))?;

            // lua.load 接受 &[u8]，文本源码与 luac 字节码均可。
            lua.load(&bytes[..])
                .set_name(path.as_str())
                .exec()
                .map_err(|e| mlua::Error::external(format!("include 执行 {path} 失败: {e}")))?;

            Ok(())
        });

        // e:var("name") -> 读取共享变量存储中的变量值。
        //
        // 按变量原始类型返回对应 Lua 值（整数/浮点/字符串/布尔）。不存在或 Null 的
        // 变量返回字符串 "0"——这是 Artemis 的核心约定（var.txt 标注返回类型为
        // string，从不为 nil；脚本据此写 `if e:var(..) ~= "0"`、`== "0"`，例如
        // system/adv/fileio.lua 的 fload_pluto 靠它判断配置是否首次创建）。返回
        // nil 会让那些判断误判，导致 boot 在"系统数据已初始化"对话框处卡死。
        // 数值用 tn(e:var(..)) 解析时 tn("0")==0，比较与 `if .. then` 也都成立
        // （Lua 中 "0" 为真）。
        methods.add_method("var", |lua, this, name: String| {
            use crate::variable::Value as V;
            let ctx = this.ctx.lock().unwrap();
            let Some(vars) = &ctx.variables else {
                return Ok(mlua::Value::String(lua.create_string("0")?));
            };
            let store = vars.lock().unwrap();
            match store.get(&name) {
                Some(V::Int(n)) => Ok(lua_integer_value(*n)),
                Some(V::Float(f)) => Ok(mlua::Value::Number(*f)),
                // Artemis 变量系统没有独立布尔类型：比较/逻辑表达式（如 `$0==0`）
                // 的结果在脚本里一律当整数 1/0 用，game 侧普遍写成
                // `tn(e:var(...))`（即 tonumber）。若把 Bool 作为 Lua boolean 返回，
                // tonumber(true) 得到 nil，cond() 会把成立的条件误判为 false
                // （典型：brandlogo 的 `cond="s.sp==0"` 被跳过）。故在此折叠成 1/0。
                Some(V::Bool(b)) => Ok(mlua::Value::Integer(if *b { 1 } else { 0 })),
                Some(V::String(s)) => Ok(mlua::Value::String(lua.create_string(s)?)),
                Some(V::Null) | None => Ok(mlua::Value::String(lua.create_string("0")?)),
            }
        });

        // e:isPush(key_id) — 带按键重复语义：按下瞬间 true → 0.5s 内 false →
        // 0.5s 后持续 true（docs/lua/engine/isPush.txt），由宿主回调实现。
        methods.add_method("isPush", |_lua, this, key_id: u32| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_push(key_id))
        });

        // e:isDown(key_id)
        methods.add_method("isDown", |_lua, this, key_id: u32| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_key_down(key_id))
        });

        // e:isDownEdge(key_id)
        methods.add_method("isDownEdge", |_lua, this, key_id: u32| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_key_down_edge(key_id))
        });

        // e:isUpEdge(key_id)
        methods.add_method("isUpEdge", |_lua, this, key_id: u32| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_key_up_edge(key_id))
        });

        // e:isDecide(key_id) — 文档要求按键编号参数（缺省视为鼠标左键 1）。
        methods.add_method("isDecide", |_lua, this, key_id: Option<u32>| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_decide_key(key_id.unwrap_or(1)))
        });

        // e:getMousePoint() -> { x=, y= }
        // 脚本一律按 table 取用（`pos.x` / `pos.y`，见 button.lua slider_clickX、
        // adv.lua 等数十处）。Lua 里 `local pos = e:getMousePoint()` 只接收首个
        // 返回值，若返回 tuple 则 pos 是数字，`pos.x` 会触发 index a number 错误
        // （表现为 config 滑动条点击/拖动报 LuaError、滑条失灵）。
        methods.add_method("getMousePoint", |lua, this, _: ()| {
            let (x, y) = {
                let ctx = this.ctx.lock().unwrap();
                ctx.callbacks.get_mouse_point()
            };
            let t = lua.create_table()?;
            t.set("x", x)?;
            t.set("y", y)?;
            Ok(t)
        });

        // e:getTouchCount()
        methods.add_method("getTouchCount", |_lua, this, _: ()| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.get_touch_count())
        });

        // e:getTouchPoint([index])
        // - 无参（文档形态）：返回按触摸唯一 id 索引的表 {[id]={x=..,y=..}, ..}。
        // - 带序号（兼容旧形态）：返回该序号触摸点的 (x, y) 二元组。
        methods.add_method("getTouchPoint", |lua, this, index: Option<u32>| {
            let ctx = this.ctx.lock().unwrap();
            match index {
                Some(i) => {
                    let (x, y) = ctx.callbacks.get_touch_point(i);
                    Ok(mlua::MultiValue::from_vec(vec![
                        mlua::Value::Integer(x as mlua::Integer),
                        mlua::Value::Integer(y as mlua::Integer),
                    ]))
                }
                None => {
                    let table = lua.create_table()?;
                    for (id, x, y) in ctx.callbacks.get_touch_points() {
                        let point = lua.create_table()?;
                        point.set("x", x)?;
                        point.set("y", y)?;
                        table.set(id, point)?;
                    }
                    Ok(mlua::MultiValue::from_vec(vec![mlua::Value::Table(table)]))
                }
            }
        });

        // e:file("path") -> file bytes as a Lua string
        // e:file{command="copy", src="...", dst="..."} -> file operation
        methods.add_method("file", |lua, this, args: mlua::MultiValue| {
            match args.into_iter().next() {
                Some(Value::String(path)) => {
                    let path = path.to_str()?.to_owned();
                    let reader = {
                        let ctx = this.ctx.lock().unwrap();
                        ctx.file_reader.clone()
                    };
                    let Some(reader) = reader else {
                        return Ok(Value::Nil);
                    };
                    match reader(&path) {
                        Ok(bytes) => Ok(Value::String(lua.create_string(&bytes)?)),
                        Err(_) => Ok(Value::Nil),
                    }
                }
                Some(Value::Table(t)) => {
                    let command: String = t.get("command").unwrap_or_default();
                    let mut params = HashMap::new();
                    for pair in t.pairs::<String, String>() {
                        if let Ok((k, v)) = pair {
                            params.insert(k, v);
                        }
                    }
                    let ctx = this.ctx.lock().unwrap();
                    ctx.callbacks.file_operation(&command, params);
                    Ok(Value::Nil)
                }
                _ => Ok(Value::Nil),
            }
        });

        // e:isFileExists("path")
        methods.add_method("isFileExists", |_lua, this, path: String| {
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_file_exists(&path))
        });

        // e:overrideKey{ key=id, status=位集合 } 或 e:overrideKey(from, to)。
        // 文档语义（overrideKey.txt）：key 省略=覆盖所有键；status 省略=取消
        // 覆盖；status 位集合 2=isPush 4=isDown 8=isDownEdge 16=isUpEdge
        // 32=isDecide。区分「省略」与「显式 0」两种情况，故用 Option 透传。
        methods.add_method("overrideKey", |_lua, this, args: mlua::MultiValue| {
            let mut key: Option<u32> = None;
            let mut status: Option<u32> = None;
            if let Some(first) = args.iter().next() {
                match first {
                    mlua::Value::Table(t) => {
                        key = t.get::<Option<u32>>("key").ok().flatten();
                        status = t.get::<Option<u32>>("status").ok().flatten();
                    }
                    mlua::Value::Integer(n) => {
                        key = Some(*n as u32);
                        if let Some(mlua::Value::Integer(n2)) = args.iter().nth(1) {
                            status = Some(*n2 as u32);
                        }
                    }
                    _ => {}
                }
            }
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.override_key_status(key, status);
            Ok(())
        });

        // e:setFlickSensitivity(sensitivity)
        methods.add_method("setFlickSensitivity", |_lua, this, sensitivity: f64| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_flick_sensitivity(sensitivity);
            Ok(())
        });

        // e:getScriptBlock{file=, index=} -> {command, line, parameter={...}}
        // 数据源是解释器共享进 EngineContext 的脚本视图（宿主没有这份数据，
        // 不走宿主回调）。docs/lua/engine/getScriptBlock.txt。
        methods.add_method("getScriptBlock", |lua, this, args: mlua::MultiValue| {
            let (file, index) = match args.into_iter().next() {
                Some(Value::Table(t)) => (
                    t.get::<Option<String>>("file")
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                    t.get::<Option<i64>>("index").ok().flatten().unwrap_or(-1),
                ),
                _ => (String::new(), -1),
            };
            let ctx = this.ctx.lock().unwrap();
            let instruction = ctx
                .scripts_view
                .get(&file)
                .filter(|_| index >= 0)
                .and_then(|script| script.get_instruction(index as usize));
            match instruction {
                Some(instruction) => {
                    let t = lua.create_table()?;
                    t.set("command", instruction.tag.as_str())?;
                    t.set("line", instruction.line as i64)?;
                    let parameter = lua.create_table()?;
                    for (k, v) in &instruction.params {
                        parameter.set(k.as_str(), v.as_str())?;
                    }
                    t.set("parameter", parameter)?;
                    Ok(mlua::Value::Table(t))
                }
                None => Ok(mlua::Value::Nil),
            }
        });

        // e:getScriptStack() -> { {file, index, reservedCommands={...}}, ... }
        // 读取解释器在进入 Lua 前同步的真实调用栈镜像；排队中的标签作为
        // 顶帧的 reservedCommands 暴露。docs/lua/engine/getScriptStack.txt。
        methods.add_method("getScriptStack", |lua, this, _: ()| {
            let ctx = this.ctx.lock().unwrap();
            let stack_table = lua.create_table()?;
            let frame_count = ctx.script_stack.len();
            for (i, (file, index)) in ctx.script_stack.iter().enumerate() {
                let frame = lua.create_table()?;
                frame.set("file", file.as_str())?;
                frame.set("index", *index as i64)?;
                let reserved = lua.create_table()?;
                if i + 1 == frame_count {
                    for (n, (tag, params)) in ctx.tag_queue.iter().enumerate() {
                        let cmd = lua.create_table()?;
                        cmd.set("command", tag.as_str())?;
                        cmd.set("line", 0)?;
                        let parameter = lua.create_table()?;
                        for (k, v) in params {
                            parameter.set(k.as_str(), v.as_str())?;
                        }
                        cmd.set("parameter", parameter)?;
                        reserved.set((n + 1) as i64, cmd)?;
                    }
                }
                frame.set("reservedCommands", reserved)?;
                stack_table.set((i + 1) as i64, frame)?;
            }
            Ok(stack_table)
        });

        // e:setScriptStack(stack) — 强制重写调用栈（setScriptStack.txt）。
        // 参数与 getScriptStack 返回值同构（[{file, index}, ...]）。这里只记录
        // 重写请求，由解释器在下一轮抽干标签队列前落实（Lua 执行期间不能
        // 直接改动解释器状态）。
        methods.add_method("setScriptStack", |_lua, this, t: mlua::Table| {
            let mut frames: Vec<(i64, String, usize)> = Vec::new();
            for pair in t.pairs::<Value, Value>() {
                let Ok((k, Value::Table(frame))) = pair else {
                    continue;
                };
                // 兼容 0/1 起始的数组下标（文档示例两种形态都出现过）。
                let order = match k {
                    Value::Integer(i) => i as i64,
                    Value::Number(n) => n as i64,
                    _ => continue,
                };
                let file: String = frame
                    .get::<Option<String>>("file")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let index = frame
                    .get::<Option<i64>>("index")
                    .ok()
                    .flatten()
                    .unwrap_or(0)
                    .max(0) as usize;
                frames.push((order, file, index));
            }
            frames.sort_by_key(|(order, _, _)| *order);
            let stack: Vec<(String, usize)> = frames
                .into_iter()
                .map(|(_, file, index)| (file, index))
                .collect();
            let mut ctx = this.ctx.lock().unwrap();
            ctx.pending_stack_override = Some(stack);
            Ok(())
        });

        // e:getScriptWaitReason() -> table（getScriptWaitReason.txt）：
        // 按等待原因挂键 time / textTween / textClearTween / sound / video。
        // 非表返回值会让脚本 `reason.time` 报 index a number，必须返回 table。
        methods.add_method("getScriptWaitReason", |lua, this, _: ()| {
            let ctx = this.ctx.lock().unwrap();
            let t = lua.create_table()?;
            if let Some(info) = &ctx.wait_reason_info {
                for (k, v) in info {
                    match k.as_str() {
                        // 数值键：time 为 e:now() 兼容的时间戳，tween 键为 1。
                        "time" | "textTween" | "textClearTween" => {
                            if let Ok(n) = v.parse::<i64>() {
                                t.set(k.as_str(), n)?;
                            } else {
                                t.set(k.as_str(), v.as_str())?;
                            }
                        }
                        // sound / video 携带目标 ID，保持字符串形态（防丢尾零）。
                        _ => t.set(k.as_str(), v.as_str())?,
                    }
                }
            }
            Ok(t)
        });

        // ------------------------------------------------------------
        // boot 流程所需方法（多数转发到 EngineCallbacks 默认实现）。
        // ------------------------------------------------------------

        // e:setScriptStatus(status)
        methods.add_method("setScriptStatus", |_lua, this, status: i64| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_script_status(status as u8);
            Ok(())
        });

        // e:setTagFilter(tags) — Artemis 会在执行内建标签前查询该表中的同名函数。
        // 保存调用方传入的表本身，不能假定它始终叫全局 `tags`。
        methods.add_method("setTagFilter", |lua, this, filter: mlua::Value| {
            match filter {
                mlua::Value::Table(table) => {
                    lua.set_named_registry_value(TAG_FILTER_REGISTRY_KEY, table)?;
                }
                mlua::Value::Nil => {
                    lua.unset_named_registry_value(TAG_FILTER_REGISTRY_KEY)?;
                }
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "setTagFilter expects a table or nil".to_string(),
                    ));
                }
            }
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_tag_filter();
            Ok(())
        });

        // e:setEventFilter(filter)
        methods.add_method("setEventFilter", |lua, this, filter: mlua::Value| {
            let mut ctx = this.ctx.lock().unwrap();
            // 将过滤器函数存入 registry，以便后续调用
            match filter {
                mlua::Value::Function(f) => {
                    ctx.event_filter = Some(lua.create_registry_value(f)?);
                }
                mlua::Value::Nil => {
                    // 传入 nil 表示清除过滤器
                    if let Some(key) = ctx.event_filter.take() {
                        let _ = lua.remove_registry_value(key);
                    }
                }
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "setEventFilter expects a function or nil".to_string(),
                    ));
                }
            }
            ctx.callbacks.set_event_filter();
            Ok(())
        });

        // e:setLogFilter(fn) — 存日志过滤函数，日志输出前调用（0 输出原始/1 抑制）。
        // 绑定仅负责存函数并通知回调；core 侧日志路径经另一代理消费该函数。
        methods.add_method("setLogFilter", |lua, this, filter: mlua::Value| {
            let mut ctx = this.ctx.lock().unwrap();
            match filter {
                mlua::Value::Function(f) => {
                    ctx.log_filter = Some(lua.create_registry_value(f)?);
                }
                mlua::Value::Nil => {
                    // 传入 nil 表示清除过滤器
                    if let Some(key) = ctx.log_filter.take() {
                        let _ = lua.remove_registry_value(key);
                    }
                }
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "setLogFilter expects a function or nil".to_string(),
                    ));
                }
            }
            ctx.callbacks.set_log_filter();
            Ok(())
        });

        // e:setMagicPath{name, path} — 位置 1=name, 位置 2=path。
        methods.add_method("setMagicPath", |_lua, this, t: mlua::Table| {
            let name: String = t.get(1).unwrap_or_default();
            let path: String = t.get(2).unwrap_or_default();
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_magic_path(&name, &path);
            Ok(())
        });

        // e:setUseMultiTouch(mode)
        methods.add_method("setUseMultiTouch", |_lua, this, mode: i64| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_use_multi_touch(mode);
            Ok(())
        });

        // e:setUseTouchHold(enabled)
        methods.add_method("setUseTouchHold", |_lua, this, enabled: bool| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_use_touch_hold(enabled);
            Ok(())
        });

        // e:setMasterVolume(volume)
        methods.add_method("setMasterVolume", |_lua, this, volume: f32| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_master_volume(volume);
            Ok(())
        });

        // e:setBgmVolume(volume)
        methods.add_method("setBgmVolume", |_lua, this, volume: f32| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_bgm_volume(volume);
            Ok(())
        });

        // e:setSeVolume(volume)
        methods.add_method("setSeVolume", |_lua, this, volume: f32| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_se_volume(volume);
            Ok(())
        });

        // e:setVoiceVolume(volume)
        methods.add_method("setVoiceVolume", |_lua, this, volume: f32| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.set_voice_volume(volume);
            Ok(())
        });

        // e:exit() — request game exit
        methods.add_method("exit", |_lua, this, _: ()| {
            let mut ctx = this.ctx.lock().unwrap();
            ctx.tag_queue
                .push(("exit".to_string(), std::collections::HashMap::new()));
            Ok(())
        });

        // e:debugSkip{index=...}
        methods.add_method("debugSkip", |_lua, this, t: mlua::Table| {
            let index: i64 = t.get("index").unwrap_or(0);
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.debug_skip(index);
            Ok(())
        });

        // e:convertEncoding{from=, to=, source=} -> string
        // source 必须按字节取用：SJIS/EUC/JIS 字节串不是合法 UTF-8，
        // 经 String 中转会直接报错或丢数据。
        methods.add_method("convertEncoding", |lua, this, t: mlua::Table| {
            let from: String = t.get("from").unwrap_or_default();
            let to: String = t.get("to").unwrap_or_default();
            let source: Option<mlua::String> = t.get("source").ok();
            let source_bytes: Vec<u8> = source.map(|s| s.as_bytes().to_vec()).unwrap_or_default();
            let converted = {
                let ctx = this.ctx.lock().unwrap();
                ctx.callbacks.convert_encoding(&from, &to, &source_bytes)
            };
            lua.create_string(&converted)
        });

        // e:callShellExecute{file=..., ...} -> number
        // 文档：以阻塞方式运行时返回被执行程序的返回值，非阻塞时返回值不确定。
        methods.add_method("callShellExecute", |_lua, this, t: mlua::Table| {
            let file: String = t.get("file").unwrap_or_default();
            let mut params = HashMap::new();
            for pair in t.pairs::<String, String>() {
                if let Ok((k, v)) = pair {
                    if k != "file" {
                        params.insert(k, v);
                    }
                }
            }
            let ctx = this.ctx.lock().unwrap();
            let code = ctx.callbacks.call_shell_execute(&file, params);
            Ok(code)
        });

        // e:writeClipboard(text) — 写入系统剪贴板（仅 Windows，无返回值）
        methods.add_method("writeClipboard", |_lua, this, text: String| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.write_clipboard(&text);
            Ok(())
        });

        // e:restoreFontCache(path)
        methods.add_method("restoreFontCache", |_lua, this, path: String| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.restore_font_cache(&path);
            Ok(())
        });

        // e:loadPngComments(path) -> { comment=... } | nil
        methods.add_method("loadPngComments", |lua, this, path: String| {
            let ctx = this.ctx.lock().unwrap();
            match ctx.callbacks.load_png_comments(&path) {
                Some(map) => {
                    let t = lua.create_table()?;
                    for (k, v) in map {
                        t.set(k, v)?;
                    }
                    Ok(mlua::Value::Table(t))
                }
                None => Ok(mlua::Value::Nil),
            }
        });

        // e:bindSurface(key)
        methods.add_method("bindSurface", |_lua, this, key: mlua::Value| {
            let key = lua_value_to_key(&key);
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.bind_surface(&key);
            Ok(())
        });

        // e:bindSurfaceAsync(key)
        methods.add_method("bindSurfaceAsync", |_lua, this, key: mlua::Value| {
            let key = lua_value_to_key(&key);
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.bind_surface_async(&key);
            Ok(())
        });

        // e:unbindSurface(key)
        methods.add_method("unbindSurface", |_lua, this, key: mlua::Value| {
            let key = lua_value_to_key(&key);
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.unbind_surface(&key);
            Ok(())
        });

        // e:clearSurfaceLoadQueue()
        methods.add_method("clearSurfaceLoadQueue", |_lua, this, _: ()| {
            let ctx = this.ctx.lock().unwrap();
            ctx.callbacks.clear_surface_load_queue();
            Ok(())
        });

        // e:isLoadingSurface(path|nil) -> bool
        // 指定路径查该图是否在加载/排队中；nil 检查整个加载队列。
        methods.add_method("isLoadingSurface", |_lua, this, arg: mlua::Value| {
            let path = match &arg {
                mlua::Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                _ => None,
            };
            let ctx = this.ctx.lock().unwrap();
            Ok(ctx.callbacks.is_loading_surface_path(path.as_deref()))
        });

        // e:createEmoteLayer{id=, files={...}, width=, height=} -> EmoteLayer
        methods.add_method("createEmoteLayer", |_lua, this, t: mlua::Table| {
            let id: String = t.get("id")?;
            let width: u32 = t.get("width")?;
            let height: u32 = t.get("height")?;
            let files_table: mlua::Table = t.get("files")?;
            let files = files_table
                .sequence_values::<String>()
                .collect::<LuaResult<Vec<_>>>()?;
            let next = {
                let ctx = this.ctx.lock().unwrap();
                ctx.callbacks
                    .create_emote_layer(&id, &files, width, height)
                    .map_err(mlua::Error::external)?
            };
            Ok(EmoteLayerApi {
                ctx: Arc::clone(&this.ctx),
                id,
                next,
            })
        });

        // e:getEmoteLayer{id=, next=true} -> EmoteLayer | nil
        methods.add_method("getEmoteLayer", |_lua, this, t: mlua::Table| {
            let id: String = t.get("id")?;
            let next: bool = t.get("next").unwrap_or(false);
            let actual_slot = {
                let ctx = this.ctx.lock().unwrap();
                ctx.callbacks.get_emote_layer(&id, next)
            };
            Ok(actual_slot.map(|next| EmoteLayerApi {
                ctx: Arc::clone(&this.ctx),
                id,
                next,
            }))
        });
    }
}

/// 当前时间（毫秒）
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 把 surface 键参数转成字符串。boot 里 bind/unbindSurface 传入的多为路径字符串，
/// 也可能是数值索引；其它类型暂转为空串（stub 阶段不区分具体 surface）。
fn lua_value_to_key(v: &mlua::Value) -> String {
    match v {
        mlua::Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        mlua::Value::Integer(n) => n.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn lua_integer_value(value: i64) -> mlua::Value {
    #[cfg(feature = "backend-luau")]
    {
        if (i32::MIN as i64..=i32::MAX as i64).contains(&value) {
            mlua::Value::Integer(value as mlua::Integer)
        } else {
            mlua::Value::Number(value as f64)
        }
    }
    #[cfg(not(feature = "backend-luau"))]
    {
        mlua::Value::Integer(value as mlua::Integer)
    }
}

/// 编码名（sjis/euc/jis/utf8）到 encoding_rs 编码的映射。
fn encoding_by_name(name: &str) -> Option<&'static encoding_rs::Encoding> {
    match name.to_ascii_lowercase().as_str() {
        "sjis" | "shift_jis" | "shift-jis" => Some(encoding_rs::SHIFT_JIS),
        "euc" | "euc-jp" | "euc_jp" => Some(encoding_rs::EUC_JP),
        "jis" | "iso-2022-jp" => Some(encoding_rs::ISO_2022_JP),
        "utf8" | "utf-8" => Some(encoding_rs::UTF_8),
        _ => None,
    }
}

/// `from` 缺省时的源编码启发式识别（convertEncoding.txt：短串可能识别失败）。
fn detect_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    // ISO-2022-JP 必含 ESC 切换序列；纯 ASCII 不会误伤（会先命中 UTF-8）。
    if bytes.contains(&0x1b) {
        return Some(encoding_rs::ISO_2022_JP);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Some(encoding_rs::UTF_8);
    }
    for encoding in [encoding_rs::SHIFT_JIS, encoding_rs::EUC_JP] {
        let (_, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return Some(encoding);
        }
    }
    None
}

/// e:convertEncoding 的默认实现：encoding_rs 真实转换。
/// `from` 为空串时自动识别；识别失败或目标编码非法时原样返回。
pub fn convert_encoding_default(from: &str, to: &str, source: &[u8]) -> Vec<u8> {
    let from_encoding = if from.is_empty() {
        detect_encoding(source)
    } else {
        encoding_by_name(from)
    };
    let (Some(from_encoding), Some(to_encoding)) = (from_encoding, encoding_by_name(to)) else {
        return source.to_vec();
    };
    if from_encoding == to_encoding {
        return source.to_vec();
    }
    let (text, _, _) = from_encoding.decode(source);
    let (converted, _, _) = to_encoding.encode(&text);
    converted.into_owned()
}

/// 把字符串值写入变量存储：可解析为数字则按 Float，否则按 String。
fn set_var_auto(store: &mut crate::variable::VariableStore, name: &str, value: String) {
    let value = value
        .parse::<f64>()
        .map(crate::variable::Value::Float)
        .unwrap_or(crate::variable::Value::String(value));
    store.set(name, value);
}

/// 处理需要宿主回调的 `var system=...` 查询（get_layer_info / get_font /
/// fullscreen / minimize）。
///
/// 返回值：
/// - `None`：不是本函数负责的 system，调用方应走通用 var 处理；
/// - `Some(true)`：已处理并写入变量；
/// - `Some(false)`：属于本函数负责但未命中（如指定 id 的图层不存在），
///   调用方可回退到内建 stub。
///
/// `params` 需为**已解析**的参数表（脚本 tag 路径先经表达式求值器解析
/// `$var` 引用；Lua e:tag 路径本身就是字面量）。
pub(crate) fn apply_system_var_query(
    callbacks: &(dyn EngineCallbacks + Send + Sync),
    params: &HashMap<String, String>,
    store: &mut crate::variable::VariableStore,
) -> Option<bool> {
    use crate::variable::Value as V;
    let system = params.get("system").map(String::as_str).unwrap_or("");
    let name = params.get("name").map(String::as_str).unwrap_or("");
    match system {
        "get_layer_info" => {
            let id = params.get("id").map(String::as_str).unwrap_or("");
            if !id.is_empty() {
                // 单图层：result.<prop>。
                let Some(info) = callbacks.get_layer_info(id) else {
                    return Some(false);
                };
                for (key, value) in info {
                    set_var_auto(store, &format!("{name}.{key}"), value);
                }
                Some(true)
            } else {
                // 省略 id：全图层枚举（get_layer_info.md），按 id 升序。
                let all = callbacks.get_layer_info_all();
                if params.get("style").map(String::as_str) == Some("map") {
                    // 伪关联数组：result.<id>.<prop>。
                    for (layer_id, info) in &all {
                        for (key, value) in info {
                            set_var_auto(store, &format!("{name}.{layer_id}.{key}"), value.clone());
                        }
                    }
                } else {
                    // 伪数组：result.N.id / result.N.<prop> + result.size。
                    // id 保持字符串形态（"1.80" 之类不得丢尾零）。
                    for (index, (layer_id, info)) in all.iter().enumerate() {
                        store.set(&format!("{name}.{index}.id"), V::String(layer_id.clone()));
                        for (key, value) in info {
                            set_var_auto(store, &format!("{name}.{index}.{key}"), value.clone());
                        }
                    }
                    store.set(&format!("{name}.size"), V::Int(all.len() as i64));
                }
                Some(true)
            }
        }
        "get_font" => {
            // get_font.md：伪数组 name.0..N-1 + name.size；monospace/vertical 过滤。
            let flag = |key: &str| {
                params
                    .get(key)
                    .map(|v| !v.is_empty() && v != "0")
                    .unwrap_or(false)
            };
            let fonts = callbacks.get_font_list(flag("monospace"), flag("vertical"));
            for (index, font) in fonts.iter().enumerate() {
                store.set(&format!("{name}.{index}"), V::String(font.clone()));
            }
            store.set(&format!("{name}.size"), V::Int(fonts.len() as i64));
            Some(true)
        }
        "fullscreen" => {
            let (fullscreen, _) = callbacks.get_window_state();
            store.set(name, V::Int(fullscreen as i64));
            Some(true)
        }
        "minimize" => {
            let (_, minimized) = callbacks.get_window_state();
            store.set(name, V::Int(minimized as i64));
            Some(true)
        }
        _ => None,
    }
}

/// 随机整数（非负 31 位），对应 Artemis 的 `e:random()`。
///
/// 注意：游戏脚本里清一色是 `e:random() % N + 1` 的整数取模用法（见
/// sysvo/macro/user/config 等），因此必须返回**整数**而非 [0,1) 浮点，
/// 否则 `% N` 会得到小数、`tbl[小数]` 取到 nil，触发
/// "attempt to get length of field '?'" 之类崩溃。
fn rand_i64() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    // 用毫秒时间播种一次，之后靠原子计数推进，保证同毫秒内连续调用也不同。
    let prev = STATE.load(Ordering::Relaxed);
    let seed = if prev == 0 { now_millis() as u64 } else { prev };
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    STATE.store(x, Ordering::Relaxed);
    // 取高位做 31 位非负整数
    ((x >> 33) & 0x7fff_ffff) as i64
}

/// 初始化 Lua 环境，注入 engine 对象
pub fn init_lua_engine_api(lua: &Lua, ctx: Arc<Mutex<EngineContext>>) -> LuaResult<()> {
    let engine = EngineApi::new(ctx);
    lua.globals().set("__engine", engine)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct EmoteProbe {
        created: Arc<Mutex<Vec<(String, Vec<String>, u32, u32)>>>,
        commands: Arc<Mutex<Vec<(String, bool, EmoteLayerCommand)>>>,
    }

    impl EngineCallbacks for EmoteProbe {
        fn debug(&self, _level: i32, _data: &str, _raw: bool) {}
        fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
        fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
        fn get_script_status(&self) -> u8 {
            0
        }
        fn is_key_down(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_down_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_up_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_decide(&self) -> bool {
            false
        }
        fn get_mouse_point(&self) -> (i32, i32) {
            (0, 0)
        }
        fn get_touch_count(&self) -> u32 {
            0
        }
        fn get_touch_point(&self, _index: u32) -> (i32, i32) {
            (0, 0)
        }
        fn is_file_exists(&self, _path: &str) -> bool {
            false
        }
        fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
        fn include(&self, _path: &str) {}
        fn override_key(&self, _from: u32, _to: u32) {}
        fn set_flick_sensitivity(&self, _sensitivity: f64) {}
        fn get_script_block(&self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
            Vec::new()
        }
        fn get_script_wait_reason(&self) -> u8 {
            0
        }

        fn create_emote_layer(
            &self,
            id: &str,
            files: &[String],
            width: u32,
            height: u32,
        ) -> crate::Result<bool> {
            self.created
                .lock()
                .unwrap()
                .push((id.to_string(), files.to_vec(), width, height));
            Ok(false)
        }

        fn get_emote_layer(&self, id: &str, _next: bool) -> Option<bool> {
            (id == "1.0").then_some(false)
        }

        fn command_emote_layer(
            &self,
            id: &str,
            next: bool,
            command: EmoteLayerCommand,
        ) -> crate::Result<()> {
            self.commands
                .lock()
                .unwrap()
                .push((id.to_string(), next, command));
            Ok(())
        }
    }

    /// 记录输入族回调调用的探针。
    #[derive(Clone, Default)]
    struct InputProbe {
        overrides: Arc<Mutex<Vec<(Option<u32>, Option<u32>)>>>,
    }

    impl EngineCallbacks for InputProbe {
        fn debug(&self, _level: i32, _data: &str, _raw: bool) {}
        fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
        fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
        fn get_script_status(&self) -> u8 {
            0
        }
        fn is_key_down(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_down_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_up_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_decide(&self) -> bool {
            false
        }
        fn is_decide_key(&self, key_id: u32) -> bool {
            key_id == 13
        }
        fn is_push(&self, key_id: u32) -> bool {
            key_id == 7
        }
        fn override_key_status(&self, key: Option<u32>, status: Option<u32>) {
            self.overrides.lock().unwrap().push((key, status));
        }
        fn get_mouse_point(&self) -> (i32, i32) {
            (0, 0)
        }
        fn get_touch_count(&self) -> u32 {
            0
        }
        fn get_touch_point(&self, _index: u32) -> (i32, i32) {
            (0, 0)
        }
        fn is_file_exists(&self, _path: &str) -> bool {
            false
        }
        fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
        fn include(&self, _path: &str) {}
        fn override_key(&self, _from: u32, _to: u32) {}
        fn set_flick_sensitivity(&self, _sensitivity: f64) {}
        fn get_script_block(&self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
            Vec::new()
        }
        fn get_script_wait_reason(&self) -> u8 {
            0
        }
    }

    /// overrideKey 绑定必须区分「省略」与「显式 0」：key 省略=全键，
    /// status 省略=取消覆盖（docs/lua/engine/overrideKey.txt）。
    #[test]
    fn override_key_binding_distinguishes_omitted_and_zero() {
        let probe = InputProbe::default();
        let observed = probe.clone();
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(probe)))),
        )
        .unwrap();

        lua.load(
            r#"
            __engine:overrideKey{key=1, status=0}
            __engine:overrideKey{status=32}
            __engine:overrideKey{key=5}
            __engine:overrideKey{}
            "#,
        )
        .exec()
        .unwrap();

        assert_eq!(
            observed.overrides.lock().unwrap().as_slice(),
            &[
                (Some(1), Some(0)),
                (None, Some(32)),
                (Some(5), None),
                (None, None),
            ]
        );
    }

    /// isPush / isDecide 绑定按键编号转发到对应回调。
    #[test]
    fn is_push_and_is_decide_bindings_forward_key_ids() {
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(
                InputProbe::default(),
            )))),
        )
        .unwrap();

        let (push7, push3, decide13, decide_default): (bool, bool, bool, bool) = lua
            .load(
                r#"
                return __engine:isPush(7), __engine:isPush(3),
                       __engine:isDecide(13), __engine:isDecide()
                "#,
            )
            .eval()
            .unwrap();
        assert!(push7);
        assert!(!push3);
        assert!(decide13);
        assert!(!decide_default, "缺省键编号应为 1（鼠标左键）");
    }

    /// convertEncoding 默认实现：encoding_rs 真转换 + from 缺省自动识别。
    #[test]
    fn convert_encoding_default_converts_between_utf8_and_sjis() {
        let utf8 = "テスト文字列".as_bytes();
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("テスト文字列");

        // 显式 utf8 → sjis。
        assert_eq!(
            convert_encoding_default("utf8", "sjis", utf8),
            sjis.to_vec()
        );
        // from 缺省：自动识别 sjis → utf8。
        assert_eq!(convert_encoding_default("", "utf8", &sjis), utf8.to_vec());
        // jis（ISO-2022-JP 带 ESC 序列）自动识别。
        let (jis, _, _) = encoding_rs::ISO_2022_JP.encode("テスト");
        assert_eq!(
            convert_encoding_default("", "utf8", &jis),
            "テスト".as_bytes().to_vec()
        );
        // 非法目标编码：原样返回。
        assert_eq!(convert_encoding_default("utf8", "bogus", utf8), utf8);
    }

    /// convertEncoding 绑定按字节处理：SJIS 字节串不是合法 UTF-8，
    /// 必须能安全进出 Lua 字符串。
    #[test]
    fn convert_encoding_binding_handles_non_utf8_bytes() {
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(
                InputProbe::default(),
            )))),
        )
        .unwrap();

        // utf8 → sjis：返回的 Lua 字符串应是 SJIS 字节。
        let out: mlua::String = lua
            .load(r#"return __engine:convertEncoding{from="utf8", to="sjis", source="テスト"}"#)
            .eval()
            .unwrap();
        let (expected, _, _) = encoding_rs::SHIFT_JIS.encode("テスト");
        assert_eq!(out.as_bytes().to_vec(), expected.to_vec());

        // 反向：SJIS 字节从 Lua 传入并转回 utf8。
        let f: mlua::Function = lua
            .load(
                r#"
                return function(bytes)
                    return __engine:convertEncoding{from="sjis", to="utf8", source=bytes}
                end
                "#,
            )
            .eval()
            .unwrap();
        let sjis_str = lua.create_string(&expected).unwrap();
        let back: mlua::String = f.call(sjis_str).unwrap();
        assert_eq!(back.as_bytes().to_vec(), "テスト".as_bytes().to_vec());
    }

    /// e:tag{"var", system="get_layer_info"} 的 Lua 路径：省略 id 全图层枚举。
    #[test]
    fn lua_var_tag_get_layer_info_enumerates_all_layers() {
        struct LayerProbe;
        impl EngineCallbacks for LayerProbe {
            fn debug(&self, _l: i32, _d: &str, _r: bool) {}
            fn enqueue_tag(&self, _t: String, _p: HashMap<String, String>) {}
            fn set_event_handler(&self, _h: HashMap<String, String>) {}
            fn get_script_status(&self) -> u8 {
                0
            }
            fn is_key_down(&self, _k: u32) -> bool {
                false
            }
            fn is_key_down_edge(&self, _k: u32) -> bool {
                false
            }
            fn is_key_up_edge(&self, _k: u32) -> bool {
                false
            }
            fn is_decide(&self) -> bool {
                false
            }
            fn get_mouse_point(&self) -> (i32, i32) {
                (0, 0)
            }
            fn get_touch_count(&self) -> u32 {
                0
            }
            fn get_touch_point(&self, _i: u32) -> (i32, i32) {
                (0, 0)
            }
            fn is_file_exists(&self, _p: &str) -> bool {
                false
            }
            fn file_operation(&self, _c: &str, _p: HashMap<String, String>) {}
            fn include(&self, _p: &str) {}
            fn override_key(&self, _f: u32, _t: u32) {}
            fn set_flick_sensitivity(&self, _s: f64) {}
            fn get_script_block(&self) -> HashMap<String, String> {
                HashMap::new()
            }
            fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
                Vec::new()
            }
            fn get_script_wait_reason(&self) -> u8 {
                0
            }
            fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
                vec![(
                    "1.80".to_string(),
                    HashMap::from([("left".to_string(), "10".to_string())]),
                )]
            }
        }

        let lua = Lua::new();
        let variables = Arc::new(Mutex::new(crate::variable::VariableStore::new()));
        let mut ctx = EngineContext::new(Box::new(LayerProbe));
        ctx.variables = Some(Arc::clone(&variables));
        init_lua_engine_api(&lua, Arc::new(Mutex::new(ctx))).unwrap();

        lua.load(r#"__engine:tag{"var", name="r", system="get_layer_info"}"#)
            .exec()
            .unwrap();

        let store = variables.lock().unwrap();
        // 图层 ID 必须保持字符串形态（"1.80" 不得丢尾零）。
        assert_eq!(
            store.get("r.0.id"),
            Some(&crate::variable::Value::String("1.80".to_string()))
        );
        assert_eq!(
            store.get("r.0.left"),
            Some(&crate::variable::Value::Float(10.0))
        );
        assert_eq!(store.get("r.size"), Some(&crate::variable::Value::Int(1)));
    }

    #[test]
    fn file_string_overload_reads_project_bytes() {
        let lua = Lua::new();
        let mut engine_ctx = EngineContext::new(Box::new(EmoteProbe::default()));
        engine_ctx.file_reader = Some(Arc::new(|path| {
            if path == ":vo/sample.ogg.csv" {
                Ok(b"0.1, 0.5, 1.0\r\n".to_vec())
            } else {
                Err(crate::Error::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    path.to_owned(),
                )))
            }
        }));
        init_lua_engine_api(&lua, Arc::new(Mutex::new(engine_ctx))).unwrap();

        let (first, last): (f64, f64) = lua
            .load(
                r#"
                local data = __engine:file(":vo/sample.ogg.csv")
                local values = {}
                for value in data:gmatch("[^,%s]+") do
                    table.insert(values, tonumber(value))
                end
                return values[1], values[#values]
                "#,
            )
            .eval()
            .unwrap();

        assert_eq!((first, last), (0.1, 1.0));
    }

    #[test]
    fn emote_userdata_forwards_script_calls_to_host() {
        let probe = EmoteProbe::default();
        let observed = probe.clone();
        let lua = Lua::new();
        let ctx = Arc::new(Mutex::new(EngineContext::new(Box::new(probe))));
        init_lua_engine_api(&lua, ctx).unwrap();
        lua.load(
            r#"
            local layer = __engine:createEmoteLayer{
                id="1.0",
                files={"a.psb"},
                width=1600,
                height=1350,
            }
            layer:setScale(0.6, 0, 0)
            layer:playTimeline("笑顔_ボイス再生用", 1)
            local current = __engine:getEmoteLayer{id="1.0", next=true}
            current:setVariable("face_talk", 0.5, 0, 0)
            "#,
        )
        .exec()
        .unwrap();

        assert_eq!(
            observed.created.lock().unwrap().as_slice(),
            &[("1.0".to_string(), vec!["a.psb".to_string()], 1600, 1350)]
        );
        let commands = observed.commands.lock().unwrap();
        assert_eq!(commands.len(), 3);
        assert!(matches!(
            commands[0].2,
            EmoteLayerCommand::SetScale { scale, .. } if (scale - 0.6).abs() < f32::EPSILON
        ));
        assert!(matches!(
            commands[2].2,
            EmoteLayerCommand::SetVariable { ref label, value, .. }
                if label == "face_talk" && (value - 0.5).abs() < f32::EPSILON
        ));
    }

    /// 记录 shell / 剪贴板 / 日志过滤回调调用的探针。
    #[derive(Clone, Default)]
    struct ShellProbe {
        clipboard: Arc<Mutex<Vec<String>>>,
        shell: Arc<Mutex<Vec<(String, HashMap<String, String>)>>>,
        log_filter_set: Arc<Mutex<u32>>,
        /// callShellExecute 回调返回值（模拟被执行程序退出码）
        shell_ret: i32,
    }

    impl EngineCallbacks for ShellProbe {
        fn debug(&self, _level: i32, _data: &str, _raw: bool) {}
        fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
        fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
        fn get_script_status(&self) -> u8 {
            0
        }
        fn is_key_down(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_down_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_key_up_edge(&self, _key_id: u32) -> bool {
            false
        }
        fn is_decide(&self) -> bool {
            false
        }
        fn get_mouse_point(&self) -> (i32, i32) {
            (0, 0)
        }
        fn get_touch_count(&self) -> u32 {
            0
        }
        fn get_touch_point(&self, _index: u32) -> (i32, i32) {
            (0, 0)
        }
        fn is_file_exists(&self, _path: &str) -> bool {
            false
        }
        fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
        fn include(&self, _path: &str) {}
        fn override_key(&self, _from: u32, _to: u32) {}
        fn set_flick_sensitivity(&self, _sensitivity: f64) {}
        fn get_script_block(&self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
            Vec::new()
        }
        fn get_script_wait_reason(&self) -> u8 {
            0
        }
        fn write_clipboard(&self, text: &str) {
            self.clipboard.lock().unwrap().push(text.to_string());
        }
        fn call_shell_execute(&self, file: &str, params: HashMap<String, String>) -> i32 {
            self.shell.lock().unwrap().push((file.to_string(), params));
            self.shell_ret
        }
        fn set_log_filter(&self) {
            *self.log_filter_set.lock().unwrap() += 1;
        }
    }

    /// e:writeClipboard(text) 应把字符串转发给 write_clipboard 回调。
    #[test]
    fn write_clipboard_binding_forwards_text() {
        let probe = ShellProbe::default();
        let observed = probe.clone();
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(probe)))),
        )
        .unwrap();

        lua.load(r#"__engine:writeClipboard("hello 剪贴板")"#)
            .exec()
            .unwrap();

        assert_eq!(
            observed.clipboard.lock().unwrap().as_slice(),
            &["hello 剪贴板".to_string()]
        );
    }

    /// e:callShellExecute{file=...} 应返回回调给出的 number（被执行程序退出码）。
    #[test]
    fn call_shell_execute_binding_returns_number() {
        let probe = ShellProbe {
            shell_ret: 42,
            ..Default::default()
        };
        let observed = probe.clone();
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(probe)))),
        )
        .unwrap();

        let code: i32 = lua
            .load(r#"return __engine:callShellExecute{file="http://x", option="--flag"}"#)
            .eval()
            .unwrap();
        assert_eq!(code, 42, "应返回回调给出的退出码");

        let shell = observed.shell.lock().unwrap();
        assert_eq!(shell.len(), 1);
        assert_eq!(shell[0].0, "http://x");
        assert_eq!(shell[0].1.get("option").map(String::as_str), Some("--flag"));
        assert!(!shell[0].1.contains_key("file"), "file 不应混入 params");
    }

    /// callShellExecute 默认回调返回 0（文档：默认返回 0）。
    #[test]
    fn call_shell_execute_default_returns_zero() {
        let lua = Lua::new();
        init_lua_engine_api(
            &lua,
            Arc::new(Mutex::new(EngineContext::new(Box::new(
                ShellProbe::default(),
            )))),
        )
        .unwrap();
        let code: i32 = lua
            .load(r#"return __engine:callShellExecute{file="x"}"#)
            .eval()
            .unwrap();
        assert_eq!(code, 0);
    }

    /// e:setLogFilter(fn) 应把函数存入 registry 并通知回调；传 nil 清除。
    #[test]
    fn set_log_filter_binding_stores_function_and_notifies() {
        let probe = ShellProbe::default();
        let observed = probe.clone();
        let lua = Lua::new();
        let ctx = Arc::new(Mutex::new(EngineContext::new(Box::new(probe))));
        init_lua_engine_api(&lua, Arc::clone(&ctx)).unwrap();

        lua.load(
            r#"
            __engine:setLogFilter(function(e, log) return 1 end)
            "#,
        )
        .exec()
        .unwrap();

        // 函数已存入 log_filter 字段
        assert!(ctx.lock().unwrap().log_filter.is_some(), "应存过滤函数");
        assert_eq!(
            *observed.log_filter_set.lock().unwrap(),
            1,
            "应通知回调一次"
        );

        // 传 nil 清除
        lua.load("__engine:setLogFilter(nil)").exec().unwrap();
        assert!(
            ctx.lock().unwrap().log_filter.is_none(),
            "nil 应清除过滤函数"
        );
    }
}
