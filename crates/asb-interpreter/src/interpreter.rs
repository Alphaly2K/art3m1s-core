//! 主解释器（迭代版本）
//!
//! ASB 脚本解释器的核心实现，使用迭代而非递归来避免栈溢出。

use crate::error::{Error, Result};
use crate::event::{CallbackResult, Event, EventCallback, ScriptLoader, default_callback};
use crate::expression::ExpressionEvaluator;
use crate::lua_engine::{DefaultEngineCallbacks, EngineContext, TAG_FILTER_REGISTRY_KEY};
use crate::r#macro::MacroRegistry;
use crate::script::{Instruction, Script};
use crate::tags::{ExecutionContext, TagRegistry, TagResult};
use crate::variable::{Value, VariableStore};
use mlua::Lua;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Artemis 兼容：字符串算术强制转换。
///
/// 大量游戏脚本（如 mulpos）依赖 `"off" * 5 → 0`、`"100" + 20 → 120` 等行为。
/// 标准 Lua 5.1 会在非数字字符串上崩溃，而 Artemis 的定制 Lua 运行时会做隐式
/// tonumber 转换并将无效字符串视为 0。
const ARITHMETIC_COERCION_CODE: &str = r#"
    local function _artemis_coerce(a, b, op)
        local an = tonumber(a)
        local bn = tonumber(b)
        if an and bn then
            return op(an, bn)
        elseif an then
            return op(an, 0)
        elseif bn then
            return op(0, bn)
        else
            return op(0, 0)
        end
    end
    local sm = getmetatable("")
    if sm then
        sm.__mul = function(a, b) return _artemis_coerce(a, b, function(x, y) return x * y end) end
        sm.__add = function(a, b) return _artemis_coerce(a, b, function(x, y) return x + y end) end
        sm.__sub = function(a, b) return _artemis_coerce(a, b, function(x, y) return x - y end) end
        sm.__div = function(a, b) return _artemis_coerce(a, b, function(x, y) return x / y end) end
        sm.__mod = function(a, b)
            local an = tonumber(a); local bn = tonumber(b)
            if an and bn then return an % bn else return 0 end
        end
        sm.__unm = function(a) return -(tonumber(a) or 0) end
        sm.__pow = function(a, b)
            local an = tonumber(a); local bn = tonumber(b)
            if an and bn then return an ^ bn else return 0 end
        end
    end
"#;

/// 调用栈帧
#[derive(Debug, Clone)]
pub struct CallFrame {
    /// 脚本名
    pub script: String,
    /// 返回行号
    pub return_line: usize,
}

#[derive(Debug, Clone)]
pub struct QueuedTagDrain {
    pub wait: Option<Event>,
    pub saw_return: bool,
    /// 排队标签是否执行过 `[call]` 并建立了自己的返回帧。
    pub saw_call: bool,
    /// 排队标签是否执行过 `[jump]` 并把当前返回帧交给跳转目标。
    pub saw_jump: bool,
    pub changed_position: bool,
}

/// 解释器配置
///
/// 包含从 system.ini 中读取的环境变量。
/// 使用方（游戏引擎）负责解析 system.ini，
/// 并将相关配置填入此结构体后传给解释器。
#[derive(Debug, Clone)]
pub struct InterpreterConfig {
    /// 脚本字符编码（默认 SHIFT_JIS，可设为 UTF-8）
    pub encoding: &'static encoding_rs::Encoding,

    /// 舞台宽度（WIDTH）
    pub stage_width: u32,
    /// 舞台高度（HEIGHT）
    pub stage_height: u32,
    /// 帧率（FPS）
    pub fps: u32,

    /// 是否无边框窗口（FRAMELESS）
    pub frameless: bool,
    /// 是否可调整窗口大小（RESIZABLE）
    pub resizable: bool,
    /// 是否固定宽高比（FIXED_ASPECT_RATIO）
    pub fixed_aspect_ratio: bool,
    /// 是否裁剪舞台溢出部分（SIDECUT）
    pub sidecut: bool,
    /// 侧边填充图片路径（SIDE_PICTURE）
    pub side_picture: Option<String>,

    /// 是否启用节能模式（POWER_SAVING）
    pub power_saving: bool,
    /// 是否禁用存档（NO_SAVE）
    pub no_save: bool,

    /// 存档路径（SAVEPATH）
    pub savepath: Option<String>,
    /// 数据文件夹路径（s.datapath）
    pub datapath: Option<String>,
    /// 游戏标题（用于窗口标题等）
    pub title: Option<String>,

    /// 防止多重启动的标识符（PREVENT_MULTIPLE_PROCESS）
    pub process_id: Option<String>,

    /// 其他自定义环境变量（供脚本通过 s.* 访问）
    pub env: HashMap<String, String>,

    /// 目标平台标识（小写：windows / android / ios / wasm 等）。
    /// 决定 `[var system="os"]` 返回值——解释器面向某一目标平台运行，而非宿主机器，
    /// 故不能用编译期 `cfg!(target_os)`。脚本据此选择机种分支。
    pub platform: String,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            encoding: encoding_rs::SHIFT_JIS,
            stage_width: 640,
            stage_height: 480,
            fps: 60,
            frameless: false,
            resizable: false,
            fixed_aspect_ratio: false,
            sidecut: false,
            side_picture: None,
            power_saving: false,
            no_save: false,
            savepath: None,
            datapath: None,
            title: None,
            process_id: None,
            env: HashMap::new(),
            platform: "windows".to_string(),
        }
    }
}

/// 执行结果
#[derive(Debug)]
pub enum ExecutionResult {
    /// 执行完成
    Completed,
    /// 等待用户输入
    Wait(Event),
    /// 调用外部脚本
    CallScript {
        /// 脚本文件
        file: String,
        /// 标签名
        label: String,
    },
    /// 跳转到其他脚本
    JumpScript {
        /// 脚本文件
        file: String,
        /// 标签名
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LuaTagFilterDecision {
    Missing,
    PassThrough,
    Consume,
}

fn lua_filter_consumes(value: &mlua::Value) -> bool {
    match value {
        mlua::Value::Nil => false,
        mlua::Value::Boolean(value) => *value,
        mlua::Value::Integer(value) => *value != 0,
        mlua::Value::Number(value) => *value != 0.0,
        mlua::Value::String(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .is_some_and(|value| value != 0.0),
        _ => true,
    }
}

/// ASB 脚本解释器
pub struct Interpreter {
    /// 配置
    config: InterpreterConfig,
    /// 已加载的脚本
    ///
    /// 用 `Arc<Script>` 持有：同一份脚本同时挂进
    /// [`EngineContext::scripts_view`]，供 `e:getScriptBlock` 按 `{file, index}`
    /// 查询指令块而不复制脚本内容。
    scripts: HashMap<String, Arc<Script>>,
    /// 变量存储
    ///
    /// 用 `Arc<Mutex<_>>` 持有，以便与 [`EngineContext`] 共享同一份变量，使 Lua 中的
    /// `e:var(name)` 能读取解释器写入的变量。
    variables: Arc<Mutex<VariableStore>>,
    /// Lua 上下文
    lua: Lua,
    /// 标签处理器注册表
    tag_registry: TagRegistry,
    /// 当前执行的脚本
    current_script: Option<String>,
    /// 当前执行行号
    current_line: usize,
    /// 调用栈
    call_stack: Vec<CallFrame>,
    /// 脚本加载器（文本）
    script_loader: Option<ScriptLoader>,
    /// 脚本文件加载器（二进制，支持自动检测）
    ///
    /// 用 `Arc` 持有，便于与 [`EngineContext::file_reader`] 共享同一个加载器，
    /// 使 `e:include` 能读取项目文件。
    file_loader: Option<crate::lua_engine::FileReader>,
    /// 事件回调
    callback: EventCallback,
    /// Lua engine 上下文
    engine_ctx: Arc<Mutex<EngineContext>>,
    /// 上一次返回的 `Wait` 是否来自**排队标签**（经 flush_tag_queue 抽干时产生）。
    ///
    /// 排队标签在 `[calllua]` 执行期间由 Lua 经 `e:tag{}` 排入，flush 发生在 step
    /// 循环顶部，此时 `current_line` 已指向**下一条尚未执行**的指令。若宿主在 Wait
    /// 释放后照常调用 `advance_line()`，会越过这条待执行指令——典型表现是 estag 链
    /// 末尾的 `eqwait` 排队 `[wait]` 发出定时等待后，advance 把行号从 `[return]`
    /// 跳到相邻的 `*movie_play [stop]`，导致 brandlogo 卡死。故此处记录来源，
    /// 让 `advance_line()` 对排队来源的 Wait 退化为空操作。
    last_wait_from_queue: bool,
    last_flush_saw_return: bool,
    last_flush_saw_call: bool,
    last_flush_saw_jump: bool,
    last_flush_changed_position: bool,
    /// 当前指令是否经由 `TagResult::Jump` **跳转到达**（而非顺序执行到达）。
    ///
    /// if 链语义需要区分两种到达 [elseif]/[else] 的方式：
    /// - if/elseif 条件为假时 Jump 落到下一个 [elseif] 行 → 正常求值；
    /// - 前面分支已命中、执行完分支体后**顺序 fallthrough** 到达 → 剩余分支
    ///   必须整段跳到匹配的 [/if]（否则 elseif 会被重复求值、两个分支都执行）。
    ///
    /// 每次 step 迭代开头消费并复位；只有 Jump 类结果会重新置位。
    arrived_by_jump: bool,
    /// 已执行过的 `__lua_block`（键：脚本名 + 指令下标）。
    ///
    /// Artemis 语义：[lua] 块在**文件加载时**执行且每块只执行一次（见
    /// docs/tag/system/lua.md）。加载时统一执行后在此登记，行指针随后走到
    /// __lua_block 行时据此跳过，避免重复执行。
    executed_lua_blocks: std::collections::HashSet<(String, usize)>,
    /// 宏注册表（docs/spec/macro.md）。执行未注册标签时先查此表，命中则以
    /// 「call 进合成脚本」的方式展开执行（支持宏体内 if/endif 与 [return]）。
    macros: MacroRegistry,
    /// 宏定义文件 → 该文件注册的宏名列表。[macrodel] 按文件整体反注册。
    macro_files: HashMap<String, Vec<String>>,
}

fn json_to_lua_value(lua: &Lua, value: serde_json::Value) -> mlua::Result<mlua::Value> {
    match value {
        serde_json::Value::Null => Ok(mlua::Value::Nil),
        serde_json::Value::Bool(value) => Ok(mlua::Value::Boolean(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(lua_integer_value(value))
            } else if let Some(value) = value.as_u64() {
                if value <= i64::MAX as u64 {
                    Ok(lua_integer_value(value as i64))
                } else {
                    Ok(mlua::Value::Number(value as f64))
                }
            } else {
                Ok(mlua::Value::Number(value.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::String(value) => Ok(mlua::Value::String(lua.create_string(&value)?)),
        serde_json::Value::Array(values) => {
            let table = lua.create_table()?;
            for (index, value) in values.into_iter().enumerate() {
                table.set((index + 1) as i64, json_to_lua_value(lua, value)?)?;
            }
            Ok(mlua::Value::Table(table))
        }
        serde_json::Value::Object(values) => {
            let table = lua.create_table()?;
            for (key, value) in values {
                let value = json_to_lua_value(lua, value)?;
                if let Some(key) = parse_canonical_integer_key(&key) {
                    table.set(key, value)?;
                } else {
                    table.set(key, value)?;
                }
            }
            Ok(mlua::Value::Table(table))
        }
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

fn lua_value_to_json_value(value: mlua::Value, depth: usize) -> mlua::Result<serde_json::Value> {
    if depth > 128 {
        return Err(mlua::Error::external(
            "JSON encode: Lua table nesting too deep",
        ));
    }

    match value {
        mlua::Value::Nil => Ok(serde_json::Value::Null),
        mlua::Value::Boolean(value) => Ok(serde_json::Value::Bool(value)),
        mlua::Value::Integer(value) => Ok(serde_json::Value::Number(value.into())),
        mlua::Value::Number(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| mlua::Error::external("JSON encode: invalid number")),
        mlua::Value::String(value) => Ok(serde_json::Value::String(value.to_string_lossy())),
        mlua::Value::Table(table) => {
            let mut object = serde_json::Map::new();
            for pair in table.pairs::<mlua::Value, mlua::Value>() {
                let (key, value) = pair?;
                if let Some(key) = lua_key_to_json_key(key) {
                    object.insert(key, lua_value_to_json_value(value, depth + 1)?);
                }
            }
            Ok(serde_json::Value::Object(object))
        }
        _ => Ok(serde_json::Value::Null),
    }
}

fn lua_key_to_json_key(key: mlua::Value) -> Option<String> {
    match key {
        mlua::Value::String(value) => Some(value.to_string_lossy()),
        mlua::Value::Integer(value) => Some(value.to_string()),
        mlua::Value::Number(value) if value.fract() == 0.0 => Some((value as i64).to_string()),
        mlua::Value::Number(value) => Some(value.to_string()),
        mlua::Value::Boolean(value) => Some(if value { "true" } else { "false" }.to_string()),
        _ => None,
    }
}

fn parse_canonical_integer_key(key: &str) -> Option<i64> {
    let value = key.parse::<i64>().ok()?;
    if value.to_string() == key {
        Some(value)
    } else {
        None
    }
}

/// 当前时间（毫秒），与 `e:now()` 同源，供 getScriptWaitReason 的 time 键使用。
fn now_millis_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Interpreter {
    /// 创建新的解释器实例
    pub fn new(config: InterpreterConfig) -> Self {
        let lua = Lua::new();

        // 注入 Pluto 序列化——使用 serde_json 作为持久化后端。
        // Artemis 脚本调用 pluto.persist(refs, tbl) / pluto.unpersist(refs, str)
        // 来序列化 Lua 表。这里用 JSON 替代原版的 Pluto 二进制格式，功能等价。
        let pluto_code = r#"
            pluto = pluto or {}
            function pluto.persist(refs, tbl)
                local ok, json = pcall(function() return __art3m1s_json_encode(tbl) end)
                if ok then return json else return "" end
            end
            function pluto.unpersist(refs, str)
                if type(str) ~= "string" or str == "" then return {} end
                local ok, tbl = pcall(function() return __art3m1s_json_decode(str) end)
                if ok and type(tbl) == "table" then return tbl else return {} end
            end
        "#;

        if let Err(e) = lua.load(pluto_code).exec() {
            eprintln!("警告: 注入 Pluto 桩实现失败: {:?}", e);
        }

        // 注册 JSON 编解码函数
        let encode = lua.create_function(|lua, value: mlua::Value| {
            let value = lua_value_to_json_value(value, 0)?;
            let json = serde_json::to_string(&value)
                .map_err(|e| mlua::Error::external(format!("JSON encode: {e}")))?;
            Ok(mlua::Value::String(lua.create_string(json)?))
        });
        let decode = lua.create_function(|lua, json_str: mlua::String| {
            let s: String = json_str.to_string_lossy();
            if s.is_empty() {
                return Ok(mlua::Value::Table(lua.create_table()?));
            }
            let value: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::external(format!("JSON decode: {e}")))?;
            json_to_lua_value(lua, value)
        });
        if let Err(e) = encode.and_then(|f| lua.globals().set("__art3m1s_json_encode", f)) {
            eprintln!("警告: 注册 JSON encode 失败: {:?}", e);
        }
        if let Err(e) = decode.and_then(|f| lua.globals().set("__art3m1s_json_decode", f)) {
            eprintln!("警告: 注册 JSON decode 失败: {:?}", e);
        }

        // 注入 Artemis 兼容的算术强制转换：字符串自动转数字，非数字字符串视为 0。
        // Artemis 引擎使用定制 Lua，允许 "off" * 5 → 0、"100" + 20 → 120 等。
        // 标准 Lua 5.1 会在这种运算上崩溃，而大量游戏脚本依赖此行为。
        if let Err(e) = lua.load(ARITHMETIC_COERCION_CODE).exec() {
            eprintln!("警告: 注入算术强制转换失败: {:?}", e);
        }

        // 注入探针：注册全局错误追踪器，记录最后发生的 Lua 错误及其调用栈。
        if let Err(e) = lua
            .load(
                r#"
            __artemis_last_error = nil
            function __artemis_traceback(msg)
                __artemis_last_error = msg .. "\n" .. debug.traceback("", 2)
                return msg
            end
        "#,
            )
            .exec()
        {
            eprintln!("警告: 注入错误探针失败: {:?}", e);
        }

        let variables = Arc::new(Mutex::new(VariableStore::new()));
        {
            let mut vars = variables.lock().unwrap();
            // 把目标平台写入变量存储，供 [var system="os"] 读取（见 InterpreterConfig::platform）。
            vars.set_platform(config.platform.clone());
            // 引擎提供的系统常量（s.*）。它们不是存档状态，而是运行环境信息，脚本启动
            // 时直接读取。缺省会让 init.lua 里的数值比较（如 windowsversion 兼容性检查）
            // 对 nil 求值而崩溃，故在此种入合理默认值。
            vars.set(
                "s.engineversion",
                crate::variable::Value::String("4.00".to_string()),
            );
            vars.set(
                "s.windowsversion",
                crate::variable::Value::String("10.0".to_string()),
            );
            // 舞台尺寸供 get_layer_info 等系统查询使用
            vars.set(
                "s.screen_width",
                crate::variable::Value::Int(config.stage_width as i64),
            );
            vars.set(
                "s.screen_height",
                crate::variable::Value::Int(config.stage_height as i64),
            );
            // 存档路径 / 数据路径：脚本用 e:var("s.savepath") 读取。
            // 缺省会让 saveload_init 拼 nil 崩溃。
            let savepath = config
                .savepath
                .clone()
                .unwrap_or_else(|| "save".to_string());
            vars.set("s.savepath", crate::variable::Value::String(savepath));
            if let Some(dp) = &config.datapath {
                vars.set("s.datapath", crate::variable::Value::String(dp.clone()));
            }
        }
        let mut engine_ctx_inner = EngineContext::new(Box::new(DefaultEngineCallbacks));
        // 共享同一份变量存储给 engine 上下文，使 e:var 能读到解释器写入的变量。
        engine_ctx_inner.variables = Some(Arc::clone(&variables));
        let engine_ctx = Arc::new(Mutex::new(engine_ctx_inner));
        #[cfg(feature = "backend-luau")]
        if let Err(e) = crate::luau_polyfill::install(&lua, Arc::clone(&engine_ctx)) {
            eprintln!("警告: 注入 Luau 兼容层失败: {:?}", e);
        }
        let _ = crate::lua_engine::init_lua_engine_api(&lua, Arc::clone(&engine_ctx));

        Self {
            config,
            scripts: HashMap::new(),
            variables,
            lua,
            tag_registry: TagRegistry::new(),
            current_script: None,
            current_line: 0,
            call_stack: Vec::new(),
            script_loader: None,
            file_loader: None,
            callback: Box::new(default_callback),
            engine_ctx,
            last_wait_from_queue: false,
            last_flush_saw_return: false,
            last_flush_saw_call: false,
            last_flush_saw_jump: false,
            last_flush_changed_position: false,
            arrived_by_jump: false,
            executed_lua_blocks: std::collections::HashSet::new(),
            macros: MacroRegistry::new(),
            macro_files: HashMap::new(),
        }
    }

    /// 设置自定义 Lua engine 回调
    ///
    /// 宿主应用可以通过此方法注入自定义回调，
    /// 响应 Lua 脚本中 engine 对象（`e`）的方法调用。
    pub fn set_engine_callbacks(&mut self, callbacks: Box<dyn crate::lua_engine::EngineCallbacks>) {
        self.engine_ctx.lock().unwrap().callbacks = callbacks;
    }

    /// 获取 Lua engine 上下文
    pub fn engine_context(&self) -> &Arc<Mutex<EngineContext>> {
        &self.engine_ctx
    }

    /// 运行 e:setEventFilter 设置的事件过滤器（docs/lua/engine/setEventFilter.txt）。
    ///
    /// 宿主在把一个已命中的事件（lyevent/aevent/输入事件…）派发给其处理器之前调用：
    /// `name` = 事件设置时的标签名（如 "lyevent"），`params` = 该标签参数（含 id/type/
    /// label 等，均为字符串）。返回值：
    /// - `None`：未设置过滤器，或过滤器返回 0/出错 —— 引擎按默认方式派发。
    /// - `Some(1)`：脚本已自行处理，引擎**不**再派发。
    /// - `Some(2)`：过滤器指示派发失败，引擎执行默认行为（与 None 同等对待）。
    ///
    /// 未设置过滤器时零开销（不加载 Lua 值）。
    pub fn run_event_filter(
        &self,
        name: &str,
        params: &std::collections::HashMap<String, String>,
    ) -> Option<i32> {
        let filter: mlua::Function = {
            let ctx = self.engine_ctx.lock().unwrap();
            let key = ctx.event_filter.as_ref()?;
            self.lua.registry_value(key).ok()?
        };

        let params_table = self.lua.create_table().ok()?;
        for (k, v) in params {
            let _ = params_table.set(k.as_str(), v.as_str());
        }
        // 过滤器签名 eventFilter(e, name, param)：e 传空表占位（宿主不便把 EngineApi
        // 自身回传，脚本实际用到的是 name/param）。
        let event_obj = self.lua.create_table().ok()?;
        match filter.call::<i32>((event_obj, name, params_table)) {
            Ok(result) => Some(result),
            Err(err) => {
                self.engine_ctx.lock().unwrap().callbacks.debug(
                    0,
                    &format!("eventFilter error: {err}"),
                    false,
                );
                None
            }
        }
    }

    /// 加载脚本（从文本）
    pub fn load_script(&mut self, name: &str, content: &str) -> Result<()> {
        let script = Script::parse(name, content)?;
        self.insert_script(name.to_string(), script);
        // Artemis 语义（docs/tag/system/lua.md）：[lua] 块在**文件加载时**执行，
        // 与块在文件中的位置无关，且所有文件共享同一 Lua 环境、每块只执行一次。
        // 旧实现是行指针走到 __lua_block 才执行，导致：跳转直达文件中段 label 时
        // 其后 lua 块里定义的函数未注册；[stop]/[return] 之后的块永不执行。
        self.run_lua_blocks_at_load(name)?;
        Ok(())
    }

    /// 加载 tag.ini 文本，供 `&linetag` 行标签解析位置参数顺序使用。
    ///
    /// `&linetag` 把「以半角英数字开头的行」当作不带参数名的行标签，
    /// 位置参数的对应关系（参数顺序）由 tag.ini 定义（见
    /// docs/tag/preprocessor/linetag.md）。宿主/引擎在启动阶段读到 tag.ini
    /// 文本后调用本方法，即把 [`TagIni::parse`] 的结果装进全局预处理器，
    /// 之后 `Script::parse` 就能在预处理阶段展开行标签。
    ///
    /// 注意：这里安装的是**全局** tag.ini（`preprocess::install_tag_ini`），
    /// 因此需在后续 `load_script`/`load_asb` 之前调用才对这些脚本生效。
    pub fn load_tag_ini(&mut self, content: &str) {
        let ini = crate::script::preprocess::TagIni::parse(content);
        crate::script::preprocess::install_tag_ini(Some(ini));
    }

    /// 登记脚本并同步共享视图（供 `e:getScriptBlock` 查询）。
    fn insert_script(&mut self, name: String, script: Script) {
        let script = Arc::new(script);
        self.engine_ctx
            .lock()
            .unwrap()
            .scripts_view
            .insert(name.clone(), Arc::clone(&script));
        self.scripts.insert(name, script);
    }

    /// 在脚本加载时统一执行其中的全部 `__lua_block`，并登记为已执行。
    ///
    /// 同名脚本重新加载视为全新文件：先清除旧的已执行标记再逐块执行。
    fn run_lua_blocks_at_load(&mut self, name: &str) -> Result<()> {
        self.executed_lua_blocks
            .retain(|(script, _)| script != name);

        let blocks: Vec<(usize, String)> = match self.scripts.get(name) {
            Some(script) => script
                .instructions
                .iter()
                .enumerate()
                .filter(|(_, inst)| inst.tag == "__lua_block")
                .map(|(idx, inst)| (idx, inst.get("code").unwrap_or("").to_string()))
                .collect(),
            None => Vec::new(),
        };

        for (idx, code) in blocks {
            self.lua.load(&code).exec().map_err(Error::LuaError)?;
            self.executed_lua_blocks.insert((name.to_string(), idx));
        }
        Ok(())
    }

    /// 加载脚本（从 ASB 二进制数据）
    pub fn load_asb(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let text = asb_decrypt::decode_asb_to_string_with_encoding(data, self.config.encoding)?;
        self.load_script(name, &text)
    }

    /// 智能加载脚本（自动检测文件格式）
    ///
    /// 根据文件魔数自动判断是文本格式（.iet, .ast）还是二进制格式（.asb）：
    /// - 如果以 `ASB\0` 开头，则作为二进制 ASB 文件解密
    /// - 否则作为文本文件直接解析
    ///
    /// 支持的扩展名：
    /// - `.iet` - 文本格式（未加密）
    /// - `.ast` - 文本格式（未加密）
    /// - `.asb` - 二进制格式（加密）
    pub fn load_file(&mut self, name: &str, data: &[u8]) -> Result<()> {
        // 检查是否是 ASB 二进制格式（魔数: ASB\0）
        if data.len() >= 4 && &data[0..4] == b"ASB\x00" {
            self.load_asb(name, data)
        } else {
            // 作为文本处理，按 system.ini 的 CHARSET 解码。
            let (text, _, _) = self.config.encoding.decode(data);
            self.load_script(name, &text)
        }
    }

    /// 设置脚本加载器（文本格式）
    pub fn set_script_loader(&mut self, loader: ScriptLoader) {
        self.script_loader = Some(loader);
    }

    /// 设置脚本文件加载器（支持文本和二进制格式的自动检测）
    ///
    /// 这是推荐的方式，能够自动处理 .iet/.ast（文本）和 .asb（二进制）文件。
    ///
    /// 同一个加载器会被共享到 [`EngineContext`]，使 Lua 中的 `e:include` 能读取
    /// 并执行项目内的 `.lua`/数据文件。
    pub fn set_file_loader(&mut self, loader: crate::event::ScriptFileLoader) {
        // ScriptFileLoader 是 Box；转成 Arc 以便与 engine_ctx 共享同一闭包。
        let shared: crate::lua_engine::FileReader = Arc::from(loader);
        self.engine_ctx.lock().unwrap().file_reader = Some(Arc::clone(&shared));
        self.file_loader = Some(shared);
    }

    /// 加载外部脚本（自动检测格式）
    pub fn load_external_script(&mut self, file: &str) -> Result<()> {
        if self.scripts.contains_key(file) {
            return Ok(());
        }

        // 优先使用文件加载器（支持二进制和文本）
        if let Some(loader) = &self.file_loader {
            let data = loader(file)?;
            return self.load_file(file, &data);
        }

        // 回退到文本加载器
        if let Some(loader) = &self.script_loader {
            let content = loader(file)?;
            return self.load_script(file, &content);
        }

        Err(Error::ScriptNotFound(file.to_string()))
    }

    /// 设置起始标签并开始执行
    pub fn start(&mut self, script: &str, label: &str) -> Result<()> {
        // 确保脚本已加载
        if !self.scripts.contains_key(script) {
            self.load_external_script(script)?;
        }

        // 查找标签
        let script_obj = self
            .scripts
            .get(script)
            .ok_or_else(|| Error::ScriptNotFound(script.to_string()))?;

        let line = script_obj
            .get_label_line(label)
            .ok_or_else(|| Error::LabelNotFound(label.to_string()))?;

        self.current_script = Some(script.to_string());
        self.current_line = line;
        self.call_stack.clear();
        // 显式定位属于「顺序到达」，清掉可能残留的跳转到达标记。
        self.arrived_by_jump = false;

        Ok(())
    }

    /// 执行到下一个等待点（迭代版本，避免栈溢出）
    pub fn step(&mut self) -> Result<ExecutionResult> {
        // 进入执行即视为旧等待已结束：清掉 getScriptWaitReason 的数据源，
        // 返回新的 Wait 时再写入（等待期间宿主不会调 step，信息保持可读）。
        self.engine_ctx.lock().unwrap().wait_reason_info = None;
        let result = self.step_inner();
        if let Ok(ExecutionResult::Wait(event)) = &result {
            self.note_wait_reason(event);
        }
        result
    }

    fn step_inner(&mut self) -> Result<ExecutionResult> {
        loop {
            // 先抽干 Lua 通过 e:tag{} 排队的标签（如图层操作），它们由上一条
            // [calllua]/[lua] 产生，必须走标签管线才能发出对应事件。
            if let Some(result) = self.flush_tag_queue()? {
                return Ok(result);
            }
            // 走到这里说明队列已抽干，接下来执行的是脚本流里的内联指令；它若产生
            // Wait，current_line 指向的就是该 Wait 指令本身，宿主需要 advance_line()
            // 越过它。故在此把来源标记清为「非排队」。
            self.last_wait_from_queue = false;

            // 消费「经由 Jump 到达当前行」标记（见字段注释）：每条指令只用一次，
            // 顺序推进（Continue/Wait 恢复等）不会置位。
            let arrived_by_jump = std::mem::take(&mut self.arrived_by_jump);

            let script_name = match &self.current_script {
                Some(name) => name.clone(),
                None => return Ok(ExecutionResult::Completed),
            };

            let script = match self.scripts.get(&script_name) {
                Some(s) => s,
                None => return Ok(ExecutionResult::Completed),
            };

            if self.current_line >= script.len() {
                return Ok(ExecutionResult::Completed);
            }

            let instruction = script.instructions[self.current_line].clone();

            if std::env::var("ASB_TRACE_STEP").is_ok() {
                eprintln!(
                    "[step] {}:{} tag={} fn={:?}",
                    script_name,
                    self.current_line,
                    instruction.tag,
                    instruction.get("function")
                );
            }

            // if 链 fallthrough 语义：**顺序执行**到达 [elseif]/[else]，说明前面
            // 的分支已经命中并执行完毕，剩余分支必须整段跳过到匹配的 [/if]；
            // 只有经由 if/elseif 条件为假的 Jump 到达时才进入正常求值派发
            // （见 docs/tag/script/if.md 与 tags/condition.rs）。
            if !arrived_by_jump && (instruction.tag == "elseif" || instruction.tag == "else") {
                let endif = crate::tags::find_matching_endif(script, self.current_line)?;
                self.current_line = endif;
                continue;
            }

            // 处理剧情文本
            if instruction.tag == "__text" {
                let text = instruction.get("text").unwrap_or("").to_string();
                let result = (self.callback)(Event::ScenarioText {
                    content: text.clone(),
                    inline: false,
                });
                match result {
                    CallbackResult::Continue => {
                        self.current_line += 1;
                        continue;
                    }
                    CallbackResult::Pause => {
                        return Ok(ExecutionResult::Wait(Event::ScenarioText {
                            content: text,
                            inline: false,
                        }));
                    }
                    CallbackResult::Abort => {
                        return Err(Error::Aborted);
                    }
                }
            }

            // 处理 Lua 代码块：已在脚本加载时统一执行（Artemis 语义，见
            // load_script），行指针走到这里直接跳过。仅当块从未执行过时补跑
            // 一次（向后兼容外部注入等非常规路径），绝不重复执行。
            if instruction.tag == "__lua_block" {
                let key = (script_name.clone(), self.current_line);
                if !self.executed_lua_blocks.contains(&key) {
                    let code = instruction.get("code").unwrap_or("");
                    if let Err(e) = self.lua.load(code).exec() {
                        return Err(Error::LuaError(e));
                    }
                    self.executed_lua_blocks.insert(key);
                }
                self.current_line += 1;
                continue;
            }

            // 执行标签
            let tag_result = self.execute_tag(&instruction, true)?;

            match tag_result {
                TagResult::Continue => {
                    self.current_line += 1;
                    continue;
                }
                TagResult::Jump(line) => {
                    // 标记跳转到达：if/elseif 条件为假落到 [elseif] 行时，
                    // 下一迭代必须走正常求值而非 fallthrough 跳过。
                    self.arrived_by_jump = true;
                    self.current_line = line;
                    continue;
                }
                TagResult::JumpExternal { file, label } => {
                    self.jump_to_external_script(&file, &label)?;
                    continue;
                }
                TagResult::Call {
                    file,
                    label,
                    return_line,
                    return_script,
                } => {
                    // 压入调用栈
                    self.call_stack.push(CallFrame {
                        script: return_script.clone(),
                        return_line,
                    });

                    // 跳转到目标
                    if let Some(target_file) = file {
                        // 跨脚本调用
                        self.load_external_script(&target_file)?;
                        let target_script = self
                            .scripts
                            .get(&target_file)
                            .ok_or_else(|| Error::ScriptNotFound(target_file.clone()))?;
                        let target_line = target_script
                            .get_label_line(&label)
                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;

                        self.current_script = Some(target_file.clone());
                        self.current_line = target_line;
                        continue;
                    } else {
                        // 同脚本调用
                        let script = self.scripts.get(&return_script).unwrap();
                        let line = script
                            .get_label_line(&label)
                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;
                        self.current_line = line;
                        continue;
                    }
                }
                TagResult::Return => {
                    if let Some(frame) = self.call_stack.pop() {
                        self.current_script = Some(frame.script);
                        self.current_line = frame.return_line;
                        continue;
                    } else {
                        return Ok(ExecutionResult::Completed);
                    }
                }
                TagResult::Wait(event) => {
                    let result = (self.callback)(event.clone());
                    match result {
                        CallbackResult::Continue => {
                            self.current_line += 1;
                            continue;
                        }
                        CallbackResult::Pause => {
                            return Ok(ExecutionResult::Wait(event));
                        }
                        CallbackResult::Abort => {
                            return Err(Error::Aborted);
                        }
                    }
                }
                TagResult::Emit(event) => {
                    let result = (self.callback)(event.clone());
                    match result {
                        CallbackResult::Continue => {
                            self.current_line += 1;
                            continue;
                        }
                        CallbackResult::Pause => {
                            return Ok(ExecutionResult::Wait(event));
                        }
                        CallbackResult::Abort => {
                            return Err(Error::Aborted);
                        }
                    }
                }
                TagResult::EmitMany(events) => {
                    for event in events {
                        let result = (self.callback)(event.clone());
                        match result {
                            CallbackResult::Continue => {}
                            CallbackResult::Pause => {
                                return Ok(ExecutionResult::Wait(event));
                            }
                            CallbackResult::Abort => {
                                return Err(Error::Aborted);
                            }
                        }
                    }
                    self.current_line += 1;
                    continue;
                }
                TagResult::Dynamic(inner_instruction) => {
                    // 动态执行另一条指令（用于 tag 标签）
                    let inner_result = self.execute_tag(&inner_instruction, false)?;
                    // 处理内部指令的结果（不增加行号，因为外层会处理）
                    match inner_result {
                        TagResult::Continue => {
                            self.current_line += 1;
                            continue;
                        }
                        TagResult::Jump(line) => {
                            self.arrived_by_jump = true;
                            self.current_line = line;
                            continue;
                        }
                        TagResult::JumpExternal { file, label } => {
                            self.jump_to_external_script(&file, &label)?;
                            continue;
                        }
                        TagResult::Wait(event) => {
                            let result = (self.callback)(event.clone());
                            match result {
                                CallbackResult::Continue => {
                                    self.current_line += 1;
                                    continue;
                                }
                                CallbackResult::Pause => {
                                    return Ok(ExecutionResult::Wait(event));
                                }
                                CallbackResult::Abort => {
                                    return Err(Error::Aborted);
                                }
                            }
                        }
                        TagResult::Emit(event) => {
                            let result = (self.callback)(event.clone());
                            match result {
                                CallbackResult::Continue => {
                                    self.current_line += 1;
                                    continue;
                                }
                                CallbackResult::Pause => {
                                    return Ok(ExecutionResult::Wait(event));
                                }
                                CallbackResult::Abort => {
                                    return Err(Error::Aborted);
                                }
                            }
                        }
                        TagResult::EmitMany(events) => {
                            for event in events {
                                let result = (self.callback)(event.clone());
                                match result {
                                    CallbackResult::Continue => {}
                                    CallbackResult::Pause => {
                                        return Ok(ExecutionResult::Wait(event));
                                    }
                                    CallbackResult::Abort => {
                                        return Err(Error::Aborted);
                                    }
                                }
                            }
                            self.current_line += 1;
                            continue;
                        }
                        TagResult::Dynamic(_) => {
                            // 不支持嵌套动态标签
                            return Err(Error::RuntimeError {
                                line: self.current_line,
                                message: "不支持嵌套的 tag 标签".to_string(),
                            });
                        }
                        other => {
                            // Call/Return 等直接处理
                            self.current_line += 1;
                            // 将结果返回给外层处理
                            match other {
                                TagResult::Call {
                                    file,
                                    label,
                                    return_line,
                                    return_script,
                                } => {
                                    self.call_stack.push(CallFrame {
                                        script: return_script,
                                        return_line,
                                    });
                                    if let Some(target_file) = file {
                                        self.load_external_script(&target_file)?;
                                        let target_script =
                                            self.scripts.get(&target_file).ok_or_else(|| {
                                                Error::ScriptNotFound(target_file.clone())
                                            })?;
                                        let target_line = target_script
                                            .get_label_line(&label)
                                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;
                                        self.current_script = Some(target_file);
                                        self.current_line = target_line;
                                    } else {
                                        let script_name =
                                            self.current_script.clone().unwrap_or_default();
                                        let script = self.scripts.get(&script_name).unwrap();
                                        let line = script
                                            .get_label_line(&label)
                                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;
                                        self.current_line = line;
                                    }
                                    continue;
                                }
                                TagResult::Return => {
                                    if let Some(frame) = self.call_stack.pop() {
                                        self.current_script = Some(frame.script);
                                        self.current_line = frame.return_line;
                                        continue;
                                    } else {
                                        return Ok(ExecutionResult::Completed);
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                }
            }
        }
    }

    /// 抽干 Lua 通过 `e:tag{}` / `e:enqueueTag{}` 排入的标签队列。
    ///
    /// `[calllua]`/`[lua]` 执行时，Lua 脚本会调用 `e:tag{name="lyc",...}` 之类把
    /// 图层等标签推入 [`EngineContext::tag_queue`]。这些标签必须走正常的标签管线
    /// [`execute_tag`](Self::execute_tag) 才能产出对应事件（如 [`Event::Layer`]）。
    /// 此前队列只被写入、从无人消费，导致 Lua 驱动的图层操作全部丢失。
    ///
    /// 每次只取队首一个并执行，这样回调返回 `Pause` 时，剩余标签仍留在队列里，
    /// 下次 `run()` 重入 `step` 可继续处理。返回 `Some(result)` 表示需立即从 `step`
    /// 返回（暂停或中止），`None` 表示队列已清空、可继续正常执行。
    fn flush_tag_queue(&mut self) -> Result<Option<ExecutionResult>> {
        loop {
            // e:setScriptStack 请求的调用栈重写在此落实——Lua 执行期间只能
            // 记录请求，队列抽干点是回到解释器控制流后的第一个安全时机。
            self.apply_pending_stack_override()?;
            let queued = {
                let mut ctx = self.engine_ctx.lock().unwrap();
                if ctx.tag_queue.is_empty() {
                    None
                } else {
                    Some(ctx.tag_queue.remove(0))
                }
            };
            let Some((tag, params)) = queued else {
                return Ok(None);
            };

            let instruction = Instruction {
                tag,
                params,
                line: self.current_line,
            };

            if std::env::var("ASB_TRACE_FLUSH").is_ok() {
                eprintln!(
                    "[flush] tag={} fn={:?} params={:?}",
                    instruction.tag,
                    instruction.get("function"),
                    instruction.params.keys().collect::<Vec<_>>()
                );
            }

            // `tag` 标签自身会返回 Dynamic，需再展开一层拿到真正的指令。
            let mut result = self.execute_tag(&instruction, false)?;
            if let TagResult::Dynamic(inner) = result {
                result = self.execute_tag(&inner, false)?;
            }

            match result {
                TagResult::Continue => continue,
                TagResult::Emit(event) | TagResult::Wait(event) => {
                    match (self.callback)(event.clone()) {
                        CallbackResult::Continue => continue,
                        CallbackResult::Pause => {
                            // 这个 Wait 来自排队标签，current_line 已指向下一条待执行
                            // 指令；记录来源，使 advance_line() 退化为空操作。
                            self.last_wait_from_queue = true;
                            return Ok(Some(ExecutionResult::Wait(event)));
                        }
                        CallbackResult::Abort => return Err(Error::Aborted),
                    }
                }
                TagResult::EmitMany(events) => {
                    for event in events {
                        match (self.callback)(event.clone()) {
                            CallbackResult::Continue => {}
                            CallbackResult::Pause => {
                                self.last_wait_from_queue = true;
                                return Ok(Some(ExecutionResult::Wait(event)));
                            }
                            CallbackResult::Abort => return Err(Error::Aborted),
                        }
                    }
                    continue;
                }
                // Lua 也会通过 eqtag/enqueueTag 排入控制流标签，必须落实其中的
                // jump/call/return 位置变更，否则 boot 无法推进到 game_start。
                // 改动 current_script/current_line 后继续抽干队列；flush 返回后
                // step 主循环会从新位置读取指令。
                TagResult::Jump(line) => {
                    self.last_flush_saw_jump = true;
                    self.last_flush_changed_position = true;
                    self.arrived_by_jump = true;
                    self.current_line = line;
                    // 继续抽干剩余标签而非立即返回——排在 jump 之后的 calllua
                    // 等函数调用仍有效（典型：fn.push 的 jump 和按钮点击 handler
                    // 先后入队，jump 先于 handler 被抽到，若此时 return 则 handler
                    // 被延迟到 jump 后的脚本上下文才执行，导致 dialog 返回值丢失）。
                    continue;
                }
                TagResult::JumpExternal { file, label } => {
                    self.last_flush_saw_jump = true;
                    self.last_flush_changed_position = true;
                    self.jump_to_external_script(&file, &label)?;
                    continue;
                }
                TagResult::Call {
                    file,
                    label,
                    return_line: _,
                    return_script,
                } => {
                    self.last_flush_saw_call = true;
                    self.last_flush_changed_position = true;
                    // 排队 call 与内联 call 的返回语义不同：
                    // 内联 call 时 `self.current_line` 指向 call 指令本身，handler
                    // 用 `current_line + 1` 让 return 落到下一条指令是对的。
                    // 但排队 call（Lua 经 enqueueTag 排入）是在 step 循环顶部抽干
                    // 队列时执行的，此时 `self.current_line` 已经指向**尚未执行**的
                    // 下一条指令。若仍用 handler 的 `current_line + 1`，return 会跳过
                    // 这条待执行指令。因此排队 call 必须返回到 `self.current_line`
                    // 本身，把它执行掉。
                    // 典型案例：system_initialize 在 `[calllua]` 中 enqueueTag 三个
                    // `[call]` 缓存系统脚本，return 必须回到紧随其后的
                    // `[calllua system_starting]`，否则 boot 推进不到 title。
                    let return_line = self.current_line;
                    if std::env::var("ASB_TRACE_FLUSH").is_ok() {
                        eprintln!(
                            "[flush-call] file={:?} label={} return_line={} return_script={}",
                            file, label, return_line, return_script
                        );
                    }
                    self.call_stack.push(CallFrame {
                        script: return_script.clone(),
                        return_line,
                    });
                    if let Some(target_file) = file {
                        self.load_external_script(&target_file)?;
                        let target_line = self
                            .scripts
                            .get(&target_file)
                            .ok_or_else(|| Error::ScriptNotFound(target_file.clone()))?
                            .get_label_line(&label)
                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;
                        self.current_script = Some(target_file);
                        self.current_line = target_line;
                    } else {
                        let line = self
                            .scripts
                            .get(&return_script)
                            .ok_or_else(|| Error::ScriptNotFound(return_script.clone()))?
                            .get_label_line(&label)
                            .ok_or_else(|| Error::LabelNotFound(label.clone()))?;
                        self.current_line = line;
                    }
                    // 继续抽干剩余标签——calllua 等函数调用在跨脚本跳转后仍然有效。
                    continue;
                }
                TagResult::Return => {
                    self.last_flush_saw_return = true;
                    self.last_flush_changed_position = true;
                    if let Some(frame) = self.call_stack.pop() {
                        self.current_script = Some(frame.script);
                        self.current_line = frame.return_line;
                    }
                    continue;
                }
                // Dynamic 已在上面展开一层；理论上不会再出现，安全忽略。
                TagResult::Dynamic(_) => continue,
            }
        }
    }

    /// 把当前调用栈 + 执行位置镜像进 [`EngineContext::script_stack`]。
    ///
    /// 供 `e:getScriptStack` / `e:getScriptBlock` 读取。必须在**每次进入 Lua
    /// 之前**刷新（calllua、tag filter、onEnterFrame/onSave/onLoad 等），因为
    /// Lua 绑定拿不到解释器自身的引用。末项为当前执行位置。
    fn sync_script_state_to_engine(&self) {
        let mut stack: Vec<(String, usize)> = self
            .call_stack
            .iter()
            .map(|frame| (frame.script.clone(), frame.return_line))
            .collect();
        if let Some(current) = &self.current_script {
            stack.push((current.clone(), self.current_line));
        }
        self.engine_ctx.lock().unwrap().script_stack = stack;
    }

    /// 落实 `e:setScriptStack` 的调用栈重写请求（docs/lua/engine/setScriptStack.txt）。
    ///
    /// 数组末帧成为当前执行位置，其余帧成为调用栈——与 getScriptStack 的
    /// 返回形态互逆。典型用法是把栈截到 1 帧，效果等同连续执行多次 [return]。
    fn apply_pending_stack_override(&mut self) -> Result<()> {
        let pending = self.engine_ctx.lock().unwrap().pending_stack_override.take();
        let Some(frames) = pending else {
            return Ok(());
        };
        let Some(((top_file, top_index), rest)) = frames.split_last() else {
            // 空数组：无处可去，忽略（脚本至少要留 1 帧）。
            return Ok(());
        };
        self.call_stack = rest
            .iter()
            .map(|(file, index)| CallFrame {
                script: file.clone(),
                return_line: *index,
            })
            .collect();
        if !self.scripts.contains_key(top_file) {
            self.load_external_script(top_file)?;
        }
        self.current_script = Some(top_file.clone());
        self.current_line = *top_index;
        self.arrived_by_jump = false;
        Ok(())
    }

    /// 把 Wait 事件的等待原因写入 [`EngineContext::wait_reason_info`]，
    /// 作为 `e:getScriptWaitReason` 的数据源（键按文档 getScriptWaitReason.txt）。
    fn note_wait_reason(&self, event: &Event) {
        let Event::Wait { reason } = event else {
            return;
        };
        use crate::event::WaitReason as WR;
        let mut info = HashMap::new();
        match reason {
            // time 键：与 e:now() 兼容的时间戳（等待截止时刻）。
            WR::Timed { milliseconds, .. } => {
                let deadline = now_millis_i64().saturating_add(*milliseconds as i64);
                info.insert("time".to_string(), deadline.to_string());
            }
            // scenario=1 等待文本出现缓动；2 等待消失缓动。
            WR::ScenarioTween { mode } => match mode {
                1 => {
                    info.insert("textTween".to_string(), "1".to_string());
                }
                2 => {
                    info.insert("textClearTween".to_string(), "1".to_string());
                }
                _ => {}
            },
            WR::Se { id, .. } => {
                info.insert("sound".to_string(), id.clone());
            }
            WR::VideoLayer { id } => {
                info.insert("video".to_string(), id.clone());
            }
            // @/stop 等点击类等待没有专属键（脚本以此区分 @ 与 wait 标签）。
            _ => {}
        }
        self.engine_ctx.lock().unwrap().wait_reason_info = Some(info);
    }

    /// 加载宏定义文件并注册其中的全部宏（[macroadd]，docs/tag/script/macroadd.md）。
    ///
    /// 宏文件的每个 `*标签名 … [return]` 块成为一个可当标签调用的宏。
    /// 返回注册的宏数量。
    pub fn load_macro_file(&mut self, file: &str) -> Result<usize> {
        let data: Vec<u8> = if let Some(loader) = &self.file_loader {
            loader(file)?
        } else if let Some(loader) = &self.script_loader {
            loader(file)?.into_bytes()
        } else {
            return Err(Error::ScriptNotFound(file.to_string()));
        };
        let text = if data.len() >= 4 && &data[0..4] == b"ASB\x00" {
            asb_decrypt::decode_asb_to_string_with_encoding(&data, self.config.encoding)?
        } else {
            let (text, _, _) = self.config.encoding.decode(&data);
            text.into_owned()
        };
        let script = Script::parse(file, &text)?;
        let names: Vec<String> = script.labels.keys().cloned().collect();
        let count = self.macros.load_from_script(&script)?;
        self.macro_files.insert(file.to_string(), names);
        Ok(count)
    }

    /// 反注册宏定义文件（[macrodel]，docs/tag/script/macrodel.md）：
    /// 该文件中定义的宏全部不再可用。
    pub fn unload_macro_file(&mut self, file: &str) {
        if let Some(names) = self.macro_files.remove(file) {
            for name in names {
                self.macros.macros.remove(&name);
            }
        }
    }

    /// 宏注册表（只读，供宿主/测试检查）。
    pub fn macro_registry(&self) -> &MacroRegistry {
        &self.macros
    }

    /// 以「call 进合成脚本」的方式展开并执行宏。
    ///
    /// docs/spec/macro.md：宏实参自动展开为变量（宏体内 `$param`、
    /// `var_exist target="param"` 都按变量取用）；同时经
    /// [`MacroRegistry::expand`] 做值参数的文本替换（estimate/cond 表达式
    /// 除外，见 macro.rs）。展开结果拼上收尾 [return] 构成合成脚本，用
    /// `TagResult::Call` 进入，宏体内 if/endif 与显式 [return] 都按普通
    /// 脚本语义工作。
    fn invoke_macro(
        &mut self,
        instruction: &Instruction,
        script_name: &str,
        current_line: usize,
    ) -> Result<TagResult> {
        // 实参先经表达式求值器解析（调用处可能写 `pos="$t.pos"`）。
        let args = {
            let variables = self.variables.lock().unwrap();
            let evaluator = ExpressionEvaluator::new(&variables);
            let mut args = HashMap::new();
            for (key, value) in &instruction.params {
                args.insert(key.clone(), evaluator.resolve_param_str(value)?);
            }
            args
        };
        let expanded = self.macros.expand(&instruction.tag, &args)?;

        // 实参落为同名变量：宏体内的 $param / estimate 求值依赖它们。
        {
            let mut store = self.variables.lock().unwrap();
            for (key, value) in &args {
                store.set(key, Value::String(value.clone()));
            }
        }

        // 合成脚本名带调用深度，避免同名宏递归调用时互相覆写返回帧内容。
        let synth_name = format!("__macro__{}@{}", instruction.tag, self.call_stack.len());
        let mut instructions = expanded;
        instructions.push(Instruction {
            tag: "return".to_string(),
            params: HashMap::new(),
            line: 0,
        });
        let script = Script {
            name: synth_name.clone(),
            labels: HashMap::new(),
            instructions,
        };
        self.insert_script(synth_name.clone(), script);

        // label 空串 = 文件开头（get_label_line 的既有缺省语义）。
        // 内联路径返回到宏调用行的下一行；排队路径由 flush_tag_queue 的
        // Call 分支改写 return_line，两侧语义都正确。
        Ok(TagResult::Call {
            file: Some(synth_name),
            label: String::new(),
            return_line: current_line + 1,
            return_script: script_name.to_string(),
        })
    }

    /// 执行单个标签
    fn execute_tag(
        &mut self,
        instruction: &Instruction,
        apply_tag_filter: bool,
    ) -> Result<TagResult> {
        let script_name = self.current_script.clone().unwrap_or_default();
        let current_line = self.current_line;
        let has_builtin = self.tag_registry.contains(&instruction.tag);

        if apply_tag_filter {
            // 场景脚本中的标签先经过 Artemis tag filter。filter 内通过 e:tag /
            // e:enqueueTag 生成的低层标签属于引擎直接调用，不能再次过滤，否则
            // tags.wait -> enqueueTag{"wait"} 会无限递归。
            // filter 是 Lua 函数，可能调用 e:getScriptStack——先刷新栈镜像。
            self.sync_script_state_to_engine();
            let filter_decision =
                self.dispatch_lua_tag_filter(&instruction.tag, &instruction.params)?;
            if filter_decision == LuaTagFilterDecision::Consume
                || (!has_builtin && filter_decision != LuaTagFilterDecision::Missing)
            {
                return Ok(TagResult::Continue);
            }
        }

        // calllua 会同步执行 Lua 函数，而该函数可能回调 e:var（再次锁 variables）。
        // 若在持有 variables 锁期间执行它会自锁死，故像 __lua_block 一样特判：
        // 不持 variables 锁、直接调用。CallLuaHandler 本身也未使用 ctx.variables。
        if instruction.tag == "calllua" {
            let raw_function_name = instruction.get("function").unwrap_or("");
            if raw_function_name.trim().is_empty() {
                // Lua frequently builds optional callbacks with
                // `e:tag{"calllua", function = maybe_nil}`.  A nil value is
                // omitted while converting the Lua table, so queued engine
                // tags may legitimately have no function at all.  Keep a
                // malformed source `[calllua]` visible as an error.
                if !apply_tag_filter {
                    return Ok(TagResult::Continue);
                }
                return Err(Error::RuntimeError {
                    line: current_line,
                    message: "calllua 缺少 function 参数".to_string(),
                });
            }
            let (function_name, extra_params) = {
                let variables = self.variables.lock().unwrap();
                let evaluator = ExpressionEvaluator::new(&variables);
                let function_name = evaluator.resolve_param_str(raw_function_name)?;
                let mut extra_params = HashMap::new();
                for (key, value) in &instruction.params {
                    if key != "function" {
                        extra_params
                            .insert(key.clone(), evaluator.resolve_param(value)?.as_string());
                    }
                }
                (function_name, extra_params)
            };
            if function_name.is_empty() {
                // Artemis uses dynamic calllua expressions as optional callbacks.
                // An unset `$t.lua` therefore means "nothing to call", not a
                // malformed source tag.
                return Ok(TagResult::Continue);
            }
            // 关键：不持 variables 锁。call_lua_function 同步执行的 Lua 可能回调
            // e:var（经共享句柄再次锁 variables），持锁会在非可重入 Mutex 上自锁死。
            // Lua 内可能调用 e:getScriptStack / e:getScriptBlock，先刷新栈镜像。
            self.sync_script_state_to_engine();
            crate::tags::call_lua_function(&self.lua, &function_name, &extra_params)?;
            return Ok(TagResult::Continue);
        }

        // [macroadd]/[macrodel]：宏定义文件的注册与反注册在解释器内完成
        // （宏表只存在于解释器，宿主没有这份数据）。文件读取失败时仅记日志，
        // 不中断脚本——实机上宏文件缺失常见于可选补丁包。
        if instruction.tag == "macroadd" || instruction.tag == "macrodel" {
            let file = {
                let variables = self.variables.lock().unwrap();
                let evaluator = ExpressionEvaluator::new(&variables);
                evaluator.resolve_param_str(instruction.get("file").unwrap_or(""))?
            };
            if !file.is_empty() {
                if instruction.tag == "macroadd" {
                    if let Err(e) = self.load_macro_file(&file) {
                        let ctx = self.engine_ctx.lock().unwrap();
                        ctx.callbacks
                            .debug(0, &format!("macroadd 加载失败 {file}: {e}"), false);
                    }
                } else {
                    self.unload_macro_file(&file);
                }
            }
            return Ok(TagResult::Continue);
        }

        // `var system=get_layer_info/get_font/fullscreen/minimize`：这些查询
        // 需要宿主回调（图层枚举 / 字体列表 / 窗口状态），在此拦截并按文档
        // 形状写入变量；未命中（如指定 id 的图层不存在）时落回内建 stub。
        if instruction.tag == "var"
            && let Some(system_raw) = instruction.get("system")
        {
            let resolved = {
                let variables = self.variables.lock().unwrap();
                let evaluator = ExpressionEvaluator::new(&variables);
                let system = evaluator.resolve_param_str(system_raw)?;
                if matches!(
                    system.as_str(),
                    "get_layer_info" | "get_font" | "fullscreen" | "minimize"
                ) {
                    let mut resolved = HashMap::new();
                    for (key, value) in &instruction.params {
                        resolved.insert(key.clone(), evaluator.resolve_param_str(value)?);
                    }
                    resolved.insert("system".to_string(), system);
                    Some(resolved)
                } else {
                    None
                }
            };
            if let Some(params) = resolved {
                // 锁序与 e:tag var 路径一致：先 engine_ctx 后 variables。
                let ctx = self.engine_ctx.lock().unwrap();
                let mut store = self.variables.lock().unwrap();
                let handled = crate::lua_engine::apply_system_var_query(
                    ctx.callbacks.as_ref(),
                    &params,
                    &mut store,
                )
                .unwrap_or(false);
                if handled {
                    return Ok(TagResult::Continue);
                }
                // 未命中：释放锁后落回下方的内建 var 处理。
            }
        }

        // [wait] 的 scenario / se / video 参数（docs/tag/script/wait.md）：
        // 注册表里的 WaitHandler 只解析 time/input，这里在解释器层先行拦截，
        // 产出专用等待源。文档语义：
        // - 指定 scenario 或 video 时 time 被忽略；
        // - se 与 time 并用时，time 表示「从该 SE 开始播放的时刻」起算的毫秒数。
        if instruction.tag == "wait"
            && (instruction.has("scenario") || instruction.has("se") || instruction.has("video"))
        {
            let reason = {
                let variables = self.variables.lock().unwrap();
                let evaluator = ExpressionEvaluator::new(&variables);
                let scenario = match instruction.get("scenario") {
                    Some(raw) => evaluator.resolve_param(raw)?.as_int().unwrap_or(0) as i32,
                    None => 0,
                };
                // ID 类参数保留字符串形态（层 ID 可能是 "1.80" 这类带尾零的路径）。
                let video = match instruction.get("video") {
                    Some(raw) => evaluator.resolve_param_str(raw)?,
                    None => String::new(),
                };
                let se = match instruction.get("se") {
                    Some(raw) => evaluator.resolve_param_str(raw)?,
                    None => String::new(),
                };
                if scenario != 0 {
                    crate::event::WaitReason::ScenarioTween { mode: scenario }
                } else if !video.is_empty() {
                    crate::event::WaitReason::VideoLayer { id: video }
                } else if !se.is_empty() {
                    let time = match instruction.get("time") {
                        Some(raw) => {
                            Some(evaluator.resolve_param(raw)?.as_int().unwrap_or(0) as u64)
                        }
                        None => None,
                    };
                    crate::event::WaitReason::Se { id: se, time }
                } else {
                    // scenario=0 且 se/video 为空串：回退为普通计时等待
                    // （与 WaitHandler 行为一致）。
                    let milliseconds = match instruction.get("time") {
                        Some(raw) => evaluator.resolve_param(raw)?.as_int().unwrap_or(0) as u64,
                        None => 0,
                    };
                    let input = match instruction.get("input") {
                        Some(raw) => evaluator.resolve_param(raw)?.as_int().unwrap_or(0) as i32,
                        None => 0,
                    };
                    crate::event::WaitReason::Timed {
                        milliseconds,
                        input,
                    }
                }
            };
            return Ok(TagResult::Wait(Event::Wait { reason }));
        }

        if has_builtin {
            // 创建上下文
            let get_script =
                |name: &str| -> Option<&Script> { self.scripts.get(name).map(|s| s.as_ref()) };

            // 锁定共享变量存储，仅在本次标签执行期间持有。非 Lua 执行类标签不会
            // 重入 e:var，故此处持锁安全（calllua 已在上面特判，不走这里）。
            let mut vars = self.variables.lock().unwrap();
            let mut ctx = ExecutionContext {
                variables: &mut vars,
                lua: &self.lua,
                current_script: &script_name,
                current_line,
                instruction,
                get_script: &get_script,
            };

            // 获取 handler 并执行
            if let Some(handler) = self.tag_registry.get(&instruction.tag) {
                handler.execute(&mut ctx)
            } else {
                unreachable!()
            }
        } else if self.macros.contains(&instruction.tag) {
            // 内建与 Lua filter 都未处理：查宏表（docs/spec/macro.md），
            // 命中则展开为合成脚本并以 call 语义执行。
            self.invoke_macro(instruction, &script_name, current_line)
        } else {
            // Lua filter 也未注册，回退：发出自定义事件。
            Ok(TagResult::Emit(Event::Custom {
                tag: instruction.tag.clone(),
                params: instruction.params.clone(),
            }))
        }
    }

    fn jump_to_external_script(&mut self, file: &str, label: &str) -> Result<()> {
        self.load_external_script(file)?;
        let target_line = self
            .scripts
            .get(file)
            .ok_or_else(|| Error::ScriptNotFound(file.to_string()))?
            .get_label_line(label)
            .ok_or_else(|| Error::LabelNotFound(label.to_string()))?;
        self.current_script = Some(file.to_string());
        self.current_line = target_line;
        Ok(())
    }

    /// 通过 `e:setTagFilter(table)` 注册的表分发标签。
    ///
    /// 游戏自定义标签（如 `msgon`、`delay0`、`btn_click` 等）在
    /// `system/extend/script.lua` 中以 `tags.<tag> = function(e, p) ... end` 注册。
    /// 引擎自身不内置这些标签，而是在此处尝试 Lua 分发。
    ///
    /// 调用签名与 `calllua` 一致：`func(__engine, param_table)`。
    /// 非零返回值表示脚本已经处理，0/nil 表示继续执行引擎默认处理。
    fn dispatch_lua_tag_filter(
        &self,
        tag: &str,
        params: &HashMap<String, String>,
    ) -> Result<LuaTagFilterDecision> {
        // 查找 filter.<tag> 函数，嵌套路径如 tags.msgon 走 Lua 递归解析。
        let func: mlua::Function = {
            let tags: mlua::Table = match self.lua.named_registry_value(TAG_FILTER_REGISTRY_KEY) {
                Ok(tags) => tags,
                Err(_) => return Ok(LuaTagFilterDecision::Missing),
            };
            let parts: Vec<&str> = tag.split('.').collect();
            let mut current: mlua::Value = match tags.get(parts[0]) {
                Ok(value) => value,
                Err(_) => return Ok(LuaTagFilterDecision::Missing),
            };
            for &part in &parts[1..] {
                current = match current {
                    mlua::Value::Table(t) => match t.get(part) {
                        Ok(value) => value,
                        Err(_) => return Ok(LuaTagFilterDecision::Missing),
                    },
                    _ => return Ok(LuaTagFilterDecision::Missing),
                };
            }
            match current {
                mlua::Value::Function(f) => f,
                _ => return Ok(LuaTagFilterDecision::Missing),
            }
        };

        // 构造 param 表
        let param_table = self.lua.create_table()?;
        for (k, v) in params {
            param_table.set(k.as_str(), v.as_str())?;
        }

        // 获取 engine 对象
        let engine: mlua::Value = self.lua.globals().get("__engine")?;

        let result: mlua::Value = match engine {
            mlua::Value::UserData(ud) => func.call((ud, param_table)),
            _ => func.call((param_table,)),
        }?;

        Ok(if lua_filter_consumes(&result) {
            LuaTagFilterDecision::Consume
        } else {
            LuaTagFilterDecision::PassThrough
        })
    }

    /// 持续执行直到完成或等待
    pub fn run(&mut self) -> Result<ExecutionResult> {
        self.step()
    }

    /// 触发注册在 `onEnterFrame` 上的每帧回调（Artemis 约定 `e:setEventHandler{
    /// onEnterFrame="vsync"}`）。宿主应在每帧驱动一次。
    ///
    /// 该回调（如 `vsync`）承载大量周期性逻辑：清除 `flg.imageCacheStart` 加载等待
    /// 标志、键盘 edge 检测、自动模式/快进、lipsync 等。若不驱动，`imageCacheStart`
    /// 永不清除，会导致 `setonpush_calllua` 在入口直接 return，**所有按钮点击全部失效**。
    ///
    /// Lua 函数内部通过 `e:tag{}` 排队的标签留在队列里，由随后的 `run()` 抽干。
    /// 注意：不能在持有 ctx 锁时调用 Lua（会重入再次锁 ctx），故先取出 handler 名
    /// 释放锁，再调用——与 [`crate::tags::call_lua_function`] 的约束一致。
    pub fn fire_enter_frame(&mut self) -> Result<()> {
        let handler = {
            let ctx = self.engine_ctx.lock().unwrap();
            ctx.event_handlers.get("onEnterFrame").cloned()
        };
        if let Some(func) = handler {
            self.sync_script_state_to_engine();
            crate::tags::call_lua_function(self.lua(), &func, &HashMap::new())?;
        }
        Ok(())
    }

    /// 触发存档前的 `onSave` 处理器（脚本中通过 `e:setEventHandler{onSave=...}` 注册）。
    ///
    /// 该回调（如 `store`）负责把 `sys`/`gscr`/`conf` 等 Lua 表经 pluto 序列化进
    /// Artemis 变量（`fsave_pluto` → `e:tag{"var",...}`），从而让存档界面所需的
    /// `sys.saveslot` 元数据随变量一同落盘。**必须在快照变量之前调用**，且调用后
    /// 还需抽干标签队列（`[var]` 标签是排队执行的），快照才能包含这些变量。
    ///
    /// 与 [`Self::fire_enter_frame`] 同样的约束：先取出 handler 名释放锁再调用 Lua。
    pub fn fire_save_handler(&mut self) -> Result<()> {
        let handler = {
            let ctx = self.engine_ctx.lock().unwrap();
            ctx.event_handlers.get("onSave").cloned()
        };
        if let Some(func) = handler {
            self.sync_script_state_to_engine();
            crate::tags::call_lua_function(self.lua(), &func, &HashMap::new())?;
        }
        Ok(())
    }

    /// Runs `onSave` and applies only the tags produced by that handler.
    ///
    /// A numbered save can be emitted while the save dialog still has return,
    /// jump, and reload tags queued.  Draining the shared queue here would run
    /// that UI continuation reentrantly and leave the scenario at the wrong
    /// stop.  Temporarily isolating the queue keeps the snapshot serialization
    /// synchronous without consuming the surrounding script flow.
    pub fn fire_save_handler_and_flush(&mut self) -> Result<()> {
        let pending = {
            let mut ctx = self.engine_ctx.lock().unwrap();
            std::mem::take(&mut ctx.tag_queue)
        };

        let result = self
            .fire_save_handler()
            .and_then(|()| self.flush_tag_queue().map(|_| ()));

        let mut ctx = self.engine_ctx.lock().unwrap();
        let mut generated_leftovers = std::mem::take(&mut ctx.tag_queue);
        generated_leftovers.extend(pending);
        ctx.tag_queue = generated_leftovers;
        result
    }

    /// 触发读档后的 `onLoad` 处理器（脚本中通过 `e:setEventHandler{onLoad=...}` 注册）。
    ///
    /// 该回调（如 `restore`）负责把读档恢复的 Artemis 变量经 pluto 反序列化回
    /// `sys`/`gscr`/`conf`/`scr`/`log` 等 Lua 表（`loadconv` → `fload_pluto`），
    /// 否则即便变量已恢复，承载游戏态与存档槽位的 Lua 表仍是旧的。
    /// **必须在 [`Self::restore_variables`] 之后调用**。
    pub fn fire_load_handler(&mut self) -> Result<()> {
        let handler = {
            let ctx = self.engine_ctx.lock().unwrap();
            ctx.event_handlers.get("onLoad").cloned()
        };
        if let Some(func) = handler {
            self.sync_script_state_to_engine();
            crate::tags::call_lua_function(self.lua(), &func, &HashMap::new())?;
        }
        Ok(())
    }

    /// 抽干当前排队的标签（公开包装 [`Self::flush_tag_queue`]）。
    ///
    /// 供宿主在触发 `onSave`/`onLoad` 处理器后调用：`store`/`restore` 通过
    /// `e:tag{"var",...}` 把序列化结果排入标签队列，这些 `[var]` 标签返回
    /// `Continue`，一次抽干即可将变量真正写入 `VariableStore`，使随后的存档快照
    /// 或读档恢复看到完整变量。
    pub fn flush_pending_tags(&mut self) -> Result<()> {
        self.flush_tag_queue()?;
        Ok(())
    }

    /// 只抽干 Lua/事件排入的标签队列，不在队列清空后继续执行主脚本。
    pub fn drain_queued_tags_only(&mut self) -> Result<QueuedTagDrain> {
        self.last_flush_saw_return = false;
        self.last_flush_saw_call = false;
        self.last_flush_saw_jump = false;
        self.last_flush_changed_position = false;
        let wait = match self.flush_tag_queue()? {
            Some(ExecutionResult::Wait(event)) => Some(event),
            Some(ExecutionResult::Completed) | None => None,
            Some(_) => None,
        };
        if let Some(event) = &wait {
            // 排队标签产生的新等待同样要暴露给 getScriptWaitReason。
            self.note_wait_reason(event);
        }
        Ok(QueuedTagDrain {
            wait,
            saw_return: self.last_flush_saw_return,
            saw_call: self.last_flush_saw_call,
            saw_jump: self.last_flush_saw_jump,
            changed_position: self.last_flush_changed_position,
        })
    }

    /// 当前行号加一。
    ///
    /// 供宿主在 `run()` 返回 `ExecutionResult::Wait` 后调用，越过触发 Wait 的那条指令，
    /// 以便下一次 `run()` 能从下一指令继续执行（例如用户点击推进的 `[wt]` 等待）。
    ///
    /// 但若该 Wait 来自**排队标签**（见 [`Self::last_wait_from_queue`]），则
    /// `current_line` 早已指向下一条待执行指令，此时再加一会越过它，故退化为空操作
    /// （仅复位标记）。
    pub fn advance_line(&mut self) {
        // 宿主 advance = 等待结束：清掉 getScriptWaitReason 的数据源。
        self.engine_ctx.lock().unwrap().wait_reason_info = None;
        if self.last_wait_from_queue {
            self.last_wait_from_queue = false;
            return;
        }
        self.current_line = self.current_line.saturating_add(1);
    }

    /// 获取变量存储的快照（用于存档）
    ///
    /// 变量现由 `Arc<Mutex<_>>` 持有，返回克隆快照以避免暴露锁。
    pub fn variables(&self) -> VariableStore {
        self.variables.lock().unwrap().clone()
    }

    /// 获取共享变量存储句柄（可变访问请锁定后操作）
    pub fn variables_handle(&self) -> Arc<Mutex<VariableStore>> {
        Arc::clone(&self.variables)
    }

    /// 获取解释器配置（包含从 system.ini 读取的环境变量）
    pub fn config(&self) -> &InterpreterConfig {
        &self.config
    }

    /// 便捷入口：加载指定脚本文件并从 `*main`/`*start`/文件开头开始执行
    ///
    /// 使用方解析 system.ini 后，直接将 BOOT 对应的脚本路径传入此方法即可。
    pub fn boot(&mut self, script: &str) -> Result<()> {
        self.load_external_script(script)?;

        // 默认宏定义文件（docs/spec/macro.md：宏写在 macro.iet；其他文件用
        // macroadd 标签追加）。项目没有该文件属正常情况，静默忽略。
        if !self.macro_files.contains_key("macro.iet") {
            let _ = self.load_macro_file("macro.iet");
        }

        let script_obj = self.scripts.get(script).unwrap();
        let start_label = if script_obj.get_label_line("main").is_some() {
            "main"
        } else if script_obj.get_label_line("start").is_some() {
            "start"
        } else if script_obj.get_label_line("_start").is_some() {
            "_start"
        } else {
            // 从文件开头
            self.current_script = Some(script.to_string());
            self.current_line = 0;
            self.call_stack.clear();
            self.arrived_by_jump = false;
            return Ok(());
        };

        self.start(script, start_label)
    }

    /// 恢复变量状态（用于读档）
    pub fn restore_variables(&mut self, store: VariableStore) {
        *self.variables.lock().unwrap() = store;
    }

    /// 加载存档后恢复解释器执行位置。
    /// `jump_target` 为脚本内标签名（如 `"*title"`），解释器会从该标签继续执行。
    pub fn restore_position(
        &mut self,
        script: &str,
        line: usize,
        stack: Vec<CallFrame>,
    ) -> Result<()> {
        self.current_script = Some(script.to_string());
        self.current_line = line;
        self.call_stack = stack;
        self.arrived_by_jump = false;
        // 重新加载目标脚本并定位到当前行
        self.load_external_script(script)?;
        Ok(())
    }

    /// 获取 Lua 上下文
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// 获取 Lua 上下文的可变引用
    pub fn lua_mut(&mut self) -> &mut Lua {
        &mut self.lua
    }

    /// 注册自定义标签处理器
    pub fn register_tag<H: crate::tags::TagHandler + 'static>(&mut self, name: &str, handler: H) {
        self.tag_registry.register(name, handler);
    }

    /// 设置事件回调
    pub fn set_callback<F: FnMut(Event) -> CallbackResult + Send + Sync + 'static>(
        &mut self,
        callback: F,
    ) {
        self.callback = Box::new(callback);
    }

    /// 获取当前脚本
    pub fn current_script(&self) -> Option<&str> {
        self.current_script.as_deref()
    }

    /// 获取当前行号
    pub fn current_line(&self) -> usize {
        self.current_line
    }

    /// 获取调用栈快照（供存档序列化）
    pub fn call_stack(&self) -> Vec<CallFrame> {
        self.call_stack.clone()
    }

    /// 获取脚本
    pub fn get_script(&self, name: &str) -> Option<&Script> {
        self.scripts.get(name).map(|s| s.as_ref())
    }

    /// 设置变量
    pub fn set_variable(&mut self, name: &str, value: Value) {
        self.variables.lock().unwrap().set(name, value);
    }

    /// 获取变量（返回克隆值）
    pub fn get_variable(&self, name: &str) -> Option<Value> {
        self.variables.lock().unwrap().get(name).cloned()
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new(InterpreterConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::Interpreter;
    use crate::event::WaitReason;
    use crate::lua_engine::EngineCallbacks;
    use crate::{CallbackResult, Event, ExecutionResult, InterpreterConfig, Value};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_filter_intercepts_and_reports_verdict() {
        let interpreter = Interpreter::new(InterpreterConfig::default());
        // 未安装过滤器：返回 None（引擎按默认方式派发）。
        assert_eq!(
            interpreter.run_event_filter("lyevent", &HashMap::new()),
            None
        );

        // 安装过滤器：捕获 name/param.id，返回 1（脚本已自行处理）。
        interpreter
            .lua()
            .load(
                r#"
                __engine:setEventFilter(function(e, name, param)
                    seen_name = name
                    seen_id = param.id
                    return 1
                end)
                "#,
            )
            .exec()
            .unwrap();

        let mut params = HashMap::new();
        params.insert("id".to_string(), "10".to_string());
        params.insert("type".to_string(), "click".to_string());
        assert_eq!(interpreter.run_event_filter("lyevent", &params), Some(1));

        let globals = interpreter.lua().globals();
        assert_eq!(globals.get::<String>("seen_name").unwrap(), "lyevent");
        assert_eq!(globals.get::<String>("seen_id").unwrap(), "10");

        // 清除过滤器后重新回到 None。
        interpreter
            .lua()
            .load("__engine:setEventFilter(nil)")
            .exec()
            .unwrap();
        assert_eq!(interpreter.run_event_filter("lyevent", &params), None);
    }

    #[test]
    fn load_text_file_decodes_with_configured_encoding() {
        let mut config = InterpreterConfig::default();
        config.encoding = encoding_rs::SHIFT_JIS;
        let mut interpreter = Interpreter::new(config);
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("*main\nこれはテストです\n");

        interpreter.load_file("main.iet", &bytes).unwrap();

        let script = interpreter.get_script("main.iet").unwrap();
        assert_eq!(script.instructions[0].tag, "__text");
        assert_eq!(script.instructions[0].get("text"), Some("これはテストです"));
    }

    #[test]
    fn load_tag_ini_enables_linetag_positional_params() {
        // load_tag_ini 装入位置参数顺序表后，&linetag 才能把
        // 「以英数字开头的行」展开成带参数名的标签（docs：linetag.md）。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.load_tag_ini("[tags]\nchara=file,x,y\n");

        let content = "*main\n[&linetag allow=\"1\"]\nchara aya01,100,200\n[return]\n";
        interpreter.load_script("main", content).unwrap();

        let script = interpreter.get_script("main").unwrap();
        // 找到展开出来的 chara 标签，校验位置参数按 tag.ini 顺序落到 file/x/y
        let chara = script
            .instructions
            .iter()
            .find(|inst| inst.tag == "chara")
            .expect("chara 行标签应被展开为普通标签");
        assert_eq!(chara.get("file"), Some("aya01"));
        assert_eq!(chara.get("x"), Some("100"));
        assert_eq!(chara.get("y"), Some("200"));

        // 清理全局状态，避免影响其它测试
        crate::script::preprocess::install_tag_ini(None);
    }

    #[test]
    fn load_asb_decodes_string_fields_with_configured_encoding() {
        let mut config = InterpreterConfig::default();
        config.encoding = encoding_rs::SHIFT_JIS;
        let mut interpreter = Interpreter::new(config);
        let (value, _, _) = encoding_rs::SHIFT_JIS.encode("これはテストです");
        let mut asb = Vec::new();

        asb.extend_from_slice(b"ASB\x00");
        asb.push(0);
        asb.extend_from_slice(&2u32.to_le_bytes());
        asb.extend_from_slice(&1u32.to_le_bytes());
        asb.extend_from_slice(&4u32.to_le_bytes());
        asb.extend_from_slice(b"main");
        asb.push(0);
        asb.extend_from_slice(&0u32.to_le_bytes());
        asb.extend_from_slice(&4u32.to_le_bytes());
        asb.extend_from_slice(b"text");
        asb.push(0);
        asb.extend_from_slice(&0u32.to_le_bytes());
        asb.extend_from_slice(&1u32.to_le_bytes());
        asb.extend_from_slice(&4u32.to_le_bytes());
        asb.extend_from_slice(b"body");
        asb.push(0);
        asb.extend_from_slice(&(value.len() as u32).to_le_bytes());
        asb.extend_from_slice(&value);
        asb.push(0);

        interpreter.load_file("main.asb", &asb).unwrap();

        let script = interpreter.get_script("main.asb").unwrap();
        assert_eq!(script.instructions[0].tag, "text");
        assert_eq!(script.instructions[0].get("body"), Some("これはテストです"));
    }

    #[test]
    fn pluto_preserves_mixed_numeric_and_string_table_keys() {
        let interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                local saveslot = {
                    [1] = { file = "save0001" },
                    [4] = { file = "save0004" },
                    last = 4,
                    count = 4,
                    check = { save0001 = true, save0004 = true },
                }
                local encoded = pluto.persist({}, saveslot)
                restored_saveslot = pluto.unpersist({}, encoded)
                "#,
            )
            .exec()
            .unwrap();

        let globals = interpreter.lua().globals();
        let restored: mlua::Table = globals.get("restored_saveslot").unwrap();
        let slot1: mlua::Table = restored.get(1).unwrap();
        let slot4: mlua::Table = restored.get(4).unwrap();
        let check: mlua::Table = restored.get("check").unwrap();

        assert_eq!(slot1.get::<String>("file").unwrap(), "save0001");
        assert_eq!(slot4.get::<String>("file").unwrap(), "save0004");
        assert_eq!(restored.get::<i64>("last").unwrap(), 4);
        assert_eq!(restored.get::<i64>("count").unwrap(), 4);
        assert!(check.get::<bool>("save0001").unwrap());
        assert!(check.get::<bool>("save0004").unwrap());
    }

    #[test]
    fn artemis_string_arithmetic_coercion_is_installed() {
        let interpreter = Interpreter::new(InterpreterConfig::default());
        let value: i64 = interpreter
            .lua()
            .load(
                r#"
                return ("100" + 20)
                    + ("off" * 5)
                    + (5 * "off")
                    + ("20" - 5)
                    + (100 / "4")
                    + ("x" % 3)
                    + (-"8")
                    + ("2" ^ "3")
                "#,
            )
            .eval()
            .unwrap();
        assert_eq!(value, 160);
    }

    fn filtered_interpreter(filter: &str, script: &str) -> Interpreter {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(filter)
            .exec()
            .expect("install tag filter");
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();
        interpreter
    }

    #[test]
    fn calllua_resolves_dynamic_function_and_parameters() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function dialog_return(e, p)
                    dynamic_call_result = p.value
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_variable("t.lua", Value::String("dialog_return".into()));
        interpreter.set_variable("t.value", Value::String("resolved".into()));
        interpreter
            .load_script(
                "test",
                "*main\n[calllua function=\"$t.lua\" value=\"$t.value\"]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Completed
        ));
        assert_eq!(
            interpreter
                .lua()
                .globals()
                .get::<String>("dynamic_call_result")
                .unwrap(),
            "resolved"
        );
    }

    #[test]
    fn calllua_treats_an_unset_dynamic_function_as_optional() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function after_optional_call(e)
                    optional_call_continued = true
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_variable("t.lua", Value::String(String::new()));
        interpreter
            .load_script(
                "test",
                "*main\n[calllua function=\"$t.lua\"]\n[calllua function=\"after_optional_call\"]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Completed
        ));
        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("optional_call_continued")
                .unwrap()
        );
    }

    #[test]
    fn queued_calllua_without_function_is_an_optional_callback() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        {
            let ctx = interpreter.engine_context();
            ctx.lock()
                .unwrap()
                .tag_queue
                .push(("calllua".into(), HashMap::new()));
        }
        interpreter.load_script("test", "*main\n[stop]\n").unwrap();
        interpreter.start("test", "main").unwrap();

        interpreter.run().unwrap();
    }

    #[test]
    fn save_handler_flush_preserves_the_existing_control_flow_queue() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function store_for_test(e)
                    e:tag{"var", name="g.saved", data="yes"}
                end
                "#,
            )
            .exec()
            .unwrap();
        {
            let ctx = interpreter.engine_context();
            let mut ctx = ctx.lock().unwrap();
            ctx.event_handlers
                .insert("onSave".into(), "store_for_test".into());
            ctx.tag_queue.push((
                "jump".into(),
                HashMap::from([("label".into(), "after_save".into())]),
            ));
        }
        interpreter
            .load_script("test", "*main\n[stop]\n*after_save\n[stop]\n")
            .unwrap();
        interpreter.start("test", "main").unwrap();

        interpreter.fire_save_handler_and_flush().unwrap();

        assert_eq!(
            interpreter.variables().get("g.saved"),
            Some(&Value::String("yes".into()))
        );
        let ctx = interpreter.engine_context();
        let queue = &ctx.lock().unwrap().tag_queue;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].0, "jump");
    }

    #[test]
    fn queued_dialog_return_reaches_the_followup_callback() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function open_dialog(e)
                    e:tag{"call", file="ui", label="dialog"}
                end
                function close_dialog(e)
                    local stack = e:getScriptStack()
                    for i = table.maxn(stack), 1, -1 do
                        if stack[i].file == "" then e:tag{"return"} else break end
                    end
                    e:tag{"var", name="t.lua", data="dialog_return"}
                    e:tag{"jump", file="ui", label="return_ui"}
                end
                function dialog_return(e)
                    dialog_closed = true
                end
                function backlog_jump(e)
                    backlog_jump_ran = true
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "main",
                "*main\n[calllua function=\"open_dialog\"]\n[calllua function=\"backlog_jump\"]\n[stop]\n",
            )
            .unwrap();
        interpreter
            .load_script(
                "ui",
                "*dialog\n[calllua function=\"close_dialog\"]\n[stop]\n*return_ui\n[calllua function=\"$t.lua\"]\n[return]\n",
            )
            .unwrap();
        interpreter.start("main", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        let globals = interpreter.lua().globals();
        assert!(globals.get::<bool>("dialog_closed").unwrap());
        assert!(globals.get::<bool>("backlog_jump_ran").unwrap());
    }

    #[test]
    fn tag_filter_nonzero_consumes_builtin_tag() {
        let mut interpreter = filtered_interpreter(
            r#"
                local filter = {}
                function filter.stop(e, p)
                    assert(p["0"] == "exskip")
                    return 1
                end
                __engine:setTagFilter(filter)
            "#,
            "*main\n[stop exskip]\n",
        );

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Completed
        ));
    }

    #[test]
    fn tag_filter_zero_runs_builtin_tag() {
        let mut interpreter = filtered_interpreter(
            r#"
                local filter = {}
                function filter.stop(e, p) return 0 end
                __engine:setTagFilter(filter)
            "#,
            "*main\n[stop exskip]\n",
        );

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop {
                    reason: Some(reason)
                }
            }) if reason == "exskip"
        ));
    }

    #[test]
    fn tag_filter_can_replace_wt0_with_queued_wait() {
        let mut interpreter = filtered_interpreter(
            r#"
                local filter = {}
                function filter.wt0(e, p)
                    e:enqueueTag{"wait", time=0, input=0}
                    return 1
                end
                function filter.wait(e, p)
                    e:enqueueTag{"wait", time=p.time, input=p.input}
                    return 1
                end
                __engine:setTagFilter(filter)
            "#,
            "*main\n[wt0]\n",
        );

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Timed {
                    milliseconds: 0,
                    input: 0
                }
            })
        ));
    }

    #[test]
    fn tag_filter_nil_handles_custom_tag_without_emitting_host_event() {
        let mut interpreter = filtered_interpreter(
            r#"
                local filter = {}
                function filter.custom(e, p)
                    custom_seen = p.value
                end
                __engine:setTagFilter(filter)
            "#,
            "*main\n[custom value=\"ok\"]\n[stop]\n",
        );

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        let seen: String = interpreter
            .lua()
            .globals()
            .get("custom_seen")
            .expect("custom filter ran");
        assert_eq!(seen, "ok");
    }

    /// 构造带标记函数的解释器：`mark_head` 把全局 head_marked 置 true。
    fn interpreter_with_head_marker() -> Interpreter {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function mark_head(e)
                    head_marked = true
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
    }

    #[test]
    fn jump_without_label_goes_to_file_start() {
        // docs/tag/script/jump.md：label 缺省=默认为文件开头。
        // 历史 bug：label 缺省成空串后 get_label_line("") 必然 LabelNotFound。
        let mut interpreter = interpreter_with_head_marker();
        interpreter
            .load_script(
                "test",
                "[calllua function=\"mark_head\"]\n[stop]\n*main\n[jump]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("head_marked")
                .unwrap(),
            "缺省 label 的 [jump] 应跳到文件开头执行第一条指令"
        );
    }

    #[test]
    fn call_without_label_goes_to_file_start_and_returns() {
        // docs/tag/script/call.md：label 参数与 jump 相同（缺省=文件开头），
        // 目标处 [return] 应回到 call 起始点之后。
        let mut interpreter = interpreter_with_head_marker();
        interpreter
            .load_script(
                "test",
                "[calllua function=\"mark_head\"]\n[return]\n*main\n[call]\n[stop]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("head_marked")
                .unwrap(),
            "缺省 label 的 [call] 应跳到文件开头执行子例程"
        );
    }

    #[test]
    fn cross_file_jump_without_label_goes_to_target_file_start() {
        // 跨文件跳转的 label 缺省分支：jump_to_external_script 同样跳到文件开头。
        let mut interpreter = interpreter_with_head_marker();
        interpreter
            .load_script("other", "[calllua function=\"mark_head\"]\n[stop]\n")
            .unwrap();
        interpreter
            .load_script("test", "*main\n[jump file=\"other\"]\n")
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("head_marked")
                .unwrap(),
            "缺省 label 的跨文件 [jump] 应落到目标文件开头"
        );
    }

    /// 执行 `[wait {params}]` 并取回等待原因。
    fn run_wait_reason(params: &str) -> WaitReason {
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
            other => panic!("expected wait, got {other:?}"),
        }
    }

    #[test]
    fn wait_se_produces_se_wait_reason() {
        // docs/tag/script/wait.md：se=STRING 等待该 SE 播放结束；
        // 与 time 并用时表示从该 SE 开始播放的时刻起算的毫秒数。
        assert!(matches!(
            run_wait_reason("se=\"se1\""),
            WaitReason::Se { ref id, time: None } if id == "se1"
        ));
        assert!(matches!(
            run_wait_reason("se=\"se1\" time=\"500\""),
            WaitReason::Se { ref id, time: Some(500) } if id == "se1"
        ));
    }

    #[test]
    fn wait_video_produces_video_layer_wait_reason_and_ignores_time() {
        // video=层 ID：等待该视频层播放结束；指定 video 时 time 被忽略。
        // 层 ID "1.80" 必须保留字符串形态（不得丢尾零）。
        assert!(matches!(
            run_wait_reason("video=\"1.80\" time=\"700\""),
            WaitReason::VideoLayer { ref id } if id == "1.80"
        ));
    }

    #[test]
    fn wait_scenario_produces_scenario_tween_wait_reason() {
        // scenario=1 等待场景文本出现的 Tween；2 等待隐藏的 Tween；
        // 0（显式指定）不等待 → 回退普通计时等待。
        assert!(matches!(
            run_wait_reason("scenario=\"1\""),
            WaitReason::ScenarioTween { mode: 1 }
        ));
        assert!(matches!(
            run_wait_reason("scenario=\"2\""),
            WaitReason::ScenarioTween { mode: 2 }
        ));
        assert!(matches!(
            run_wait_reason("scenario=\"0\" time=\"100\" input=\"1\""),
            WaitReason::Timed {
                milliseconds: 100,
                input: 1
            }
        ));
    }

    #[test]
    fn plain_wait_still_produces_timed_reason() {
        // 回归：无 scenario/se/video 参数的 [wait] 仍走 WaitHandler 的 Timed。
        assert!(matches!(
            run_wait_reason("time=\"250\""),
            WaitReason::Timed {
                milliseconds: 250,
                input: 0
            }
        ));
    }

    #[test]
    fn lua_blocks_execute_at_load_time_regardless_of_position() {
        // docs/tag/system/lua.md：执行时机是文件加载时而非执行到标签处；
        // [stop] 之后的块也必须执行（旧实现里永不执行）。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .load_script(
                "test",
                "*main\n[stop]\n[lua]\nlua_load_count = (lua_load_count or 0) + 1\nfunction defined_late() return 7 end\n[/lua]\n",
            )
            .unwrap();

        // 尚未 start/run，块已在加载时执行。
        let globals = interpreter.lua().globals();
        assert_eq!(globals.get::<i64>("lua_load_count").unwrap(), 1);
        let f: mlua::Function = globals.get("defined_late").unwrap();
        assert_eq!(f.call::<i64>(()).unwrap(), 7);
    }

    #[test]
    fn lua_blocks_do_not_rerun_when_stepped_over() {
        // 向后兼容：加载时已执行过的块，行指针走到时不得重复执行。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                "*main\n[lua]\nstep_count = (step_count or 0) + 1\n[/lua]\n[stop]\n",
            )
            .unwrap();
        assert_eq!(
            interpreter.lua().globals().get::<i64>("step_count").unwrap(),
            1
        );

        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run().unwrap();

        assert_eq!(
            interpreter.lua().globals().get::<i64>("step_count").unwrap(),
            1,
            "行指针越过 __lua_block 时不得重复执行"
        );
    }

    #[test]
    fn functions_from_trailing_lua_block_are_available_to_earlier_lines() {
        // 跳转直达文件中段 label 时，位于其后的 lua 块里定义的函数必须已注册。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                "*main\n[calllua function=\"tail_fn\"]\n[stop]\n[lua]\nfunction tail_fn(e)\n    tail_called = true\nend\n[/lua]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run().unwrap();

        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("tail_called")
                .unwrap(),
            "文件末尾 lua 块中定义的函数应在加载时即可被前面的 [calllua] 调用"
        );
    }

    /// docs/spec/macro.md：macro.iet 中 `*标签名…[return]` 块成为可调用宏；
    /// 参数自动展开为变量，estimate 按变量求值。
    #[test]
    fn macro_from_default_file_runs_like_a_tag() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_file_loader(Box::new(|path: &str| match path {
            "macro.iet" => Ok(concat!(
                "*chara_a\n",
                "[if estimate=\"$pos == 2\"]\n",
                "[lyc id=\"3\" file=\"chara_a\"]\n",
                "[/if]\n",
                "[if estimate=\"$pos == 1\"]\n",
                "[lyc id=\"1\" file=\"chara_a\"]\n",
                "[/if]\n",
                "[return]\n"
            )
            .as_bytes()
            .to_vec()),
            "main.iet" => Ok(b"*main\n[chara_a pos=\"2\"]\n[stop]\n".to_vec()),
            other => Err(crate::Error::ScriptNotFound(other.to_string())),
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        interpreter.set_callback(move |event| {
            sink.lock().unwrap().push(event.clone());
            match event {
                Event::Wait { .. } => CallbackResult::Pause,
                _ => CallbackResult::Continue,
            }
        });
        interpreter.boot("main.iet").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        let events = events.lock().unwrap();
        let created: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::Layer(crate::event::LayerEvent::Create { id, .. }) => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(created, vec!["3".to_string()], "只有 center 分支应命中");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Custom { tag, .. } if tag == "chara_a")),
            "宏命中后不得回退为 Custom 事件"
        );
    }

    /// [macroadd] 注册新宏文件；[macrodel] 后其中的宏恢复为未知标签（Custom）。
    #[test]
    fn macroadd_and_macrodel_register_and_unregister_macro_files() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_file_loader(Box::new(|path: &str| match path {
            "extra.iet" => {
                Ok(b"*hello\n[var name=\"t.count\" data=\"$t.count + 1\"]\n[return]\n".to_vec())
            }
            other => Err(crate::Error::ScriptNotFound(other.to_string())),
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        interpreter.set_callback(move |event| {
            sink.lock().unwrap().push(event.clone());
            match event {
                Event::Wait { .. } => CallbackResult::Pause,
                _ => CallbackResult::Continue,
            }
        });
        interpreter.set_variable("t.count", Value::Int(0));
        interpreter
            .load_script(
                "test",
                "*main\n[macroadd file=\"extra.iet\"]\n[hello]\n[macrodel file=\"extra.iet\"]\n[hello]\n[stop]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        let _ = interpreter.run().unwrap();

        assert_eq!(
            interpreter.variables().get("t.count"),
            Some(&Value::Int(1)),
            "macrodel 前的调用应展开一次"
        );
        let customs = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Custom { tag, .. } if tag == "hello"))
            .count();
        assert_eq!(customs, 1, "macrodel 后 [hello] 应回退为 Custom 事件");
        assert!(!interpreter.macro_registry().contains("hello"));
    }

    /// e:getScriptStack 暴露真实调用栈；e:getScriptBlock 按 {file,index} 查块。
    #[test]
    fn get_script_stack_and_block_reflect_real_call_frames() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function capture_stack(e)
                    local stacks = e:getScriptStack()
                    stack_len = #stacks
                    bottom_file = stacks[1].file
                    top_file = stacks[#stacks].file
                    local block = e:getScriptBlock(stacks[#stacks])
                    top_command = block.command
                    top_line = block.line
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script("main", "*main\n[call file=\"sub\" label=\"s\"]\n[stop]\n")
            .unwrap();
        interpreter
            .load_script("sub", "*s\n[calllua function=\"capture_stack\"]\n[return]\n")
            .unwrap();
        interpreter.start("main", "main").unwrap();

        let _ = interpreter.run().unwrap();

        let globals = interpreter.lua().globals();
        assert_eq!(globals.get::<i64>("stack_len").unwrap(), 2);
        assert_eq!(globals.get::<String>("bottom_file").unwrap(), "main");
        assert_eq!(globals.get::<String>("top_file").unwrap(), "sub");
        assert_eq!(globals.get::<String>("top_command").unwrap(), "calllua");
        assert_eq!(globals.get::<i64>("top_line").unwrap(), 2);
    }

    /// e:setScriptStack 截断到 1 帧 = 连续 return 的效果（setScriptStack.txt）。
    #[test]
    fn set_script_stack_truncation_acts_like_repeated_returns() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .lua()
            .load(
                r#"
                function truncate(e)
                    local stacks = e:getScriptStack()
                    while #stacks ~= 1 do
                        table.remove(stacks, #stacks)
                    end
                    e:setScriptStack(stacks)
                end
                function mark_after(e)
                    after_marked = true
                end
                "#,
            )
            .exec()
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                "*main\n[call label=\"sub\"]\n[calllua function=\"mark_after\"]\n[stop]\n*sub\n[calllua function=\"truncate\"]\n[stop]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. }
            })
        ));
        assert!(
            interpreter
                .lua()
                .globals()
                .get::<bool>("after_marked")
                .unwrap(),
            "重写栈后应回到 call 的下一行继续执行"
        );
        assert!(
            interpreter.call_stack().is_empty(),
            "截断到 1 帧后调用栈应为空"
        );
    }

    /// getScriptWaitReason 按文档返回 table：time / textTween / sound 等键。
    #[test]
    fn get_script_wait_reason_returns_reason_table() {
        // [wait time=...] → time 键为 e:now() 兼容的截止时间戳。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script("test", "*main\n[wait time=\"500\"]\n[stop]\n")
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run().unwrap();

        let time: i64 = interpreter
            .lua()
            .load("return __engine:getScriptWaitReason().time")
            .eval()
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(
            (time - now).abs() < 10_000,
            "time 应是 now+500ms 附近的时间戳: time={time} now={now}"
        );

        // 宿主 advance 后等待结束，表应为空。
        interpreter.advance_line();
        let cleared: bool = interpreter
            .lua()
            .load("return __engine:getScriptWaitReason().time == nil")
            .eval()
            .unwrap();
        assert!(cleared);

        // [wait scenario=1] → textTween=1；[wait se=...] → sound=ID。
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                "*main\n[wait scenario=\"1\"]\n[wait se=\"vo01\"]\n[stop]\n",
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run().unwrap();
        let text_tween: i64 = interpreter
            .lua()
            .load("return __engine:getScriptWaitReason().textTween")
            .eval()
            .unwrap();
        assert_eq!(text_tween, 1);

        interpreter.advance_line();
        let _ = interpreter.run().unwrap();
        let sound: String = interpreter
            .lua()
            .load("return __engine:getScriptWaitReason().sound")
            .eval()
            .unwrap();
        assert_eq!(sound, "vo01");
    }

    /// var system=get_layer_info 省略 id / style=map 的形状（get_layer_info.md），
    /// 走脚本 tag 路径 + 宿主回调。
    #[test]
    fn var_get_layer_info_enumerates_all_layers_via_callbacks() {
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
            fn get_layer_info(&self, id: &str) -> Option<HashMap<String, String>> {
                (id == "foo").then(|| HashMap::from([("left".to_string(), "100".to_string())]))
            }
            fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
                vec![
                    (
                        "bar".to_string(),
                        HashMap::from([("top".to_string(), "100".to_string())]),
                    ),
                    (
                        "foo".to_string(),
                        HashMap::from([("left".to_string(), "100".to_string())]),
                    ),
                ]
            }
            fn get_window_state(&self) -> (bool, bool) {
                (true, false)
            }
        }

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_engine_callbacks(Box::new(LayerProbe));
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter
            .load_script(
                "test",
                concat!(
                    "*main\n",
                    "[var name=\"r\" system=\"get_layer_info\"]\n",
                    "[var name=\"m\" system=\"get_layer_info\" style=\"map\"]\n",
                    "[var name=\"one\" system=\"get_layer_info\" id=\"foo\"]\n",
                    "[var name=\"fs\" system=\"fullscreen\"]\n",
                    "[stop]\n"
                ),
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run().unwrap();

        let vars = interpreter.variables();
        // 伪数组：按 id 升序 + size。id 保持字符串形态。
        assert_eq!(vars.get("r.0.id"), Some(&Value::String("bar".into())));
        assert_eq!(vars.get("r.0.top"), Some(&Value::Float(100.0)));
        assert_eq!(vars.get("r.1.id"), Some(&Value::String("foo".into())));
        assert_eq!(vars.get("r.1.left"), Some(&Value::Float(100.0)));
        assert_eq!(vars.get("r.size"), Some(&Value::Int(2)));
        // map 形态：result.<id>.<prop>。
        assert_eq!(vars.get("m.bar.top"), Some(&Value::Float(100.0)));
        assert_eq!(vars.get("m.foo.left"), Some(&Value::Float(100.0)));
        // 单 id：result.<prop>。
        assert_eq!(vars.get("one.left"), Some(&Value::Float(100.0)));
        // fullscreen 经宿主窗口状态回调。
        assert_eq!(vars.get("fs"), Some(&Value::Int(1)));
    }

    #[cfg(feature = "backend-luau")]
    #[test]
    fn luau_polyfills_cover_maxn_os_and_host_file_write() {
        let writes = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_engine_callbacks(Box::new(TestCallbacks {
            writes: Arc::clone(&writes),
        }));

        interpreter
            .lua()
            .load(
                r#"
                assert(table.maxn({[2] = true, [4] = true, name = "x"}) == 4)
                assert(os.date("!%Y-%m-%d %H:%M:%S", 0) == "1970-01-01 00:00:00")
                assert(os.execute("anything") == -1)
                assert(os.getenv("HOME") == nil)
                assert(math.mod(7, 4) == 3)
                assert(table.getn({1, 2, 3}) == 3)

                local f = assert(io.open("save/test.bin", "wb"))
                assert(f:write("A", 12))
                assert(f:close())
                "#,
            )
            .exec()
            .unwrap();

        let writes = writes.lock().unwrap();
        assert_eq!(
            writes.as_slice(),
            [("save/test.bin".to_string(), b"A12".to_vec())]
        );
    }

    #[cfg(feature = "backend-luau")]
    #[test]
    fn luau_string_gfind_supports_artemis_split_helper() {
        let interpreter = Interpreter::new(InterpreterConfig::default());
        let parts: mlua::Table = interpreter
            .lua()
            .load(
                r##"
                function split(str, delim)
                    if not str then
                        return {}
                    elseif not str:find(delim) then
                        return { str }
                    end

                    local result = {}
                    local pat = "(.-)" .. delim .. "()"
                    local lastPos
                    for part, pos in string.gfind(str, pat) do
                        table.insert(result, part)
                        lastPos = pos
                    end
                    table.insert(result, string.sub(str, lastPos))
                    return result
                end

                return split("s.sp==0", "#")
                "##,
            )
            .eval()
            .unwrap();

        assert_eq!(parts.get::<String>(1).unwrap(), "s.sp==0");

        let parts: mlua::Table = interpreter
            .lua()
            .load(r#"return split("alpha,beta,gamma", ",")"#)
            .eval()
            .unwrap();
        assert_eq!(parts.get::<String>(1).unwrap(), "alpha");
        assert_eq!(parts.get::<String>(2).unwrap(), "beta");
        assert_eq!(parts.get::<String>(3).unwrap(), "gamma");
    }

    #[cfg(feature = "backend-luau")]
    struct TestCallbacks {
        writes: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
    }

    #[cfg(feature = "backend-luau")]
    impl EngineCallbacks for TestCallbacks {
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
        fn file_write(&self, path: &str, data: &[u8]) -> crate::Result<()> {
            self.writes
                .lock()
                .unwrap()
                .push((path.to_string(), data.to_vec()));
            Ok(())
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
}
