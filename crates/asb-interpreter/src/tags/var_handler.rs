//! var 标签的 system= 变体处理器
//!
//! 实现 [var system="xxx"] 的各种系统功能。

use crate::error::Result;
use crate::expression::ExpressionEvaluator;
use crate::variable::{Value, VariableStore};
use std::collections::HashMap;
use std::sync::RwLock;

// ---------------------------------------------------------------------------
// 宿主查询钩子（由 art3m1s-core 等宿主在启动时安装）
// ---------------------------------------------------------------------------

/// 单个声道的播放信息（get_sound_info 用）。gain/pan 为 Artemis 原始刻度
/// （gain 0..1000，pan -1000..1000）。
#[derive(Debug, Clone)]
pub struct SoundChannelInfo {
    pub id: String,
    pub playing: bool,
    pub gain: i64,
    pub pan: i64,
}

/// 声音子系统快照：BGM + 全部 SE 声道（SE 按 ID 升序，保证伪数组下标稳定）。
#[derive(Debug, Clone, Default)]
pub struct SoundInfoSnapshot {
    pub bgm: Option<SoundChannelInfo>,
    pub se: Vec<SoundChannelInfo>,
}

/// 宿主查询钩子集合。
///
/// `var system=file_exists/file_crc/file_update_time/get_sound_info` 的真实
/// 数据来自宿主（包文件系统/存档目录/音频状态）。标签路径的
/// [`ExecutionContext`](super::ExecutionContext) 不携带 EngineCallbacks，
/// 因此这里提供进程级钩子注册点；未安装钩子时各命令保持原有的保守回退。
pub struct HostQueryHooks {
    /// (文件路径, save 参数是否为 1) -> 是否存在。宿主负责 magic path/存档路径解析。
    pub file_exists: Box<dyn Fn(&str, bool) -> bool + Send + Sync>,
    /// 文件路径 -> CRC32（IEEE），读不到文件时 None。
    pub file_crc32: Box<dyn Fn(&str) -> Option<u32> + Send + Sync>,
    /// 存档文件路径 -> 本地时间分量 [年,月,日,时,分,秒]，文件不存在时 None。
    pub file_update_time: Box<dyn Fn(&str) -> Option<[i64; 6]> + Send + Sync>,
    /// 当前声音播放状态快照。
    pub sound_info: Box<dyn Fn() -> SoundInfoSnapshot + Send + Sync>,
    /// 历史（backlog）总页数。文档 var/get_backlog_size.md。
    pub backlog_size: Box<dyn Fn() -> usize + Send + Sync>,
    /// (页号 0 起, allfont) -> 该页再现所需标签序列；页号越界返回 None。
    pub backlog_tags: Box<dyn Fn(usize, bool) -> Option<Vec<String>> + Send + Sync>,
    /// (消息层 id, allfont) -> 当前页已执行的文本相关标签序列；层不存在返回 None。
    pub message_tags: Box<dyn Fn(&str, bool) -> Option<Vec<String>> + Send + Sync>,
    /// 当前消息层文本度量 `(整体宽度, 总高度, 最后一行宽度)`。
    /// 供 get_message_layer_width/height/line_width。
    pub message_layer_metrics: Box<dyn Fn() -> (f32, f32, f32) + Send + Sync>,
}

static HOST_QUERY_HOOKS: RwLock<Option<HostQueryHooks>> = RwLock::new(None);

/// 安装（或替换）宿主查询钩子。宿主重建 runtime 后重新安装即可覆盖旧钩子。
pub fn set_host_query_hooks(hooks: HostQueryHooks) {
    *HOST_QUERY_HOOKS.write().unwrap() = Some(hooks);
}

/// 在钩子（若已安装）上执行查询。
fn with_host_hooks<R>(f: impl FnOnce(&HostQueryHooks) -> R) -> Option<R> {
    HOST_QUERY_HOOKS.read().unwrap().as_ref().map(f)
}

/// 标签参数是否为"开"（`1`/`true`/`on`/`yes`，缺省 false）。
fn param_is_on(value: Option<&String>) -> bool {
    value
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

/// 把字符串序列落成伪数组：`name.0..N-1` = 各元素，`name.size` = 个数。
/// 序列为 None（查询未命中/无钩子）时只落 `name.size = 0`。
fn set_pseudo_array(name: &str, items: Option<&[String]>, variables: &mut VariableStore) {
    let items = items.unwrap_or(&[]);
    for (index, item) in items.iter().enumerate() {
        variables.set(&format!("{name}.{index}"), Value::String(item.clone()));
    }
    variables.set(&format!("{name}.size"), Value::Int(items.len() as i64));
}

/// 应用一个 `var` 标签到变量存储（同步落值）。
///
/// 这是 `[var ...]` 标签语义的纯逻辑实现，VarHandler 与 Lua 的 `e:tag{"var",...}`
/// 都走这里。关键在于 var 标签不产出任何事件（始终 Continue），因此 Lua 在同一函数
/// 内 `e:tag{"var", name="t.w", system=...}` 之后立刻 `e:var("t.w")` 时，必须已经落值，
/// 否则读到 nil。把落值动作放在入队/执行的同一处，即可消除该时序窗口。
///
/// 表达式参数（system 变体的 source/string/... 以及简单形式的 data）在此就地解析。
pub fn apply_var_tag(
    params: &HashMap<String, String>,
    variables: &mut VariableStore,
) -> Result<()> {
    if let Some(system) = params.get("system") {
        let system = system.clone();

        // 先解析所有可能含表达式的参数（解析期间只读 variables）。
        let mut resolved: HashMap<String, Value> = HashMap::new();
        {
            let evaluator = ExpressionEvaluator::new(variables);
            for key in &[
                "source", "string", "min", "max", "position", "length", "file", "target",
                "message", "key",
            ] {
                if let Some(val) = params.get(*key) {
                    resolved.insert(key.to_string(), evaluator.resolve_param(val)?);
                }
            }
        }

        execute_var_system(&system, params, &resolved, variables)?;
        return Ok(());
    }

    let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
    let data = params.get("data").map(|s| s.as_str()).unwrap_or("");
    let value = {
        let evaluator = ExpressionEvaluator::new(variables);
        evaluator.resolve_param(data)?
    };
    variables.set(name, value);
    Ok(())
}

/// 执行 var system= 变体
pub fn execute_var_system(
    system: &str,
    params: &HashMap<String, String>,
    resolved: &HashMap<String, Value>,
    variables: &mut VariableStore,
) -> Result<()> {
    // 辅助函数：获取已解析的参数值
    let get_resolved = |key: &str| -> Option<&Value> { resolved.get(key) };

    match system {
        "var_exist" => {
            let target = params.get("target").map(|s| s.as_str()).unwrap_or("");
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let local = params.get("local").map(|s| s.as_str()) == Some("1");

            let exists = if local {
                variables.get(target).is_some()
                    && !target.starts_with("g.")
                    && !target.starts_with("t.")
                    && !target.starts_with("s.")
            } else {
                variables.contains(target)
            };

            variables.set(name, Value::Bool(exists));
        }

        "delete" => {
            if let Some(name) = params.get("name") {
                if name.is_empty() {
                    variables.clear_all();
                } else {
                    delete_variable_tree(variables, name);
                }
            } else {
                variables.clear_all();
            }
        }

        "random" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let min = get_resolved("min").and_then(|v| v.as_int()).unwrap_or(0);
            let max = get_resolved("max")
                .and_then(|v| v.as_int())
                .unwrap_or(i64::MAX);

            let result = if max > min {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                let range = (max - min + 1) as u64;
                let rand_val =
                    ((seed.wrapping_mul(6364136223846793005).wrapping_add(1)) as u64) % range;
                min + rand_val as i64
            } else {
                min
            };
            variables.set(name, Value::Int(result));
        }

        "length" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let mode = params.get("mode").map(|s| s.as_str()).unwrap_or("0");

            let len = if mode == "1" {
                source.chars().count() as i64
            } else {
                source.len() as i64
            };
            variables.set(name, Value::Int(len));
        }

        "find" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let string = get_resolved("string")
                .map(|v| v.as_string())
                .unwrap_or_default();

            let pos = source.find(&string).map(|p| p as i64).unwrap_or(-1);
            variables.set(name, Value::Int(pos));
        }

        "substr" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let position = get_resolved("position")
                .and_then(|v| v.as_int())
                .unwrap_or(0)
                .max(0) as usize;
            let length = get_resolved("length")
                .and_then(|v| v.as_int())
                .unwrap_or(source.len() as i64)
                .max(0) as usize;
            let mode = params.get("mode").map(|s| s.as_str()).unwrap_or("0");

            let result = if mode == "1" {
                source
                    .chars()
                    .skip(position)
                    .take(length)
                    .collect::<String>()
            } else {
                // 字节模式：position/length 按字节计。越界时截断到串尾而不是返回空串；
                // 落在多字节字符中间的边界向合法字符边界收拢（起点前移、终点后退），
                // 避免 UTF-8 引擎下产生非法切片。
                let len = source.len();
                let mut start = position.min(len);
                while !source.is_char_boundary(start) {
                    start += 1;
                }
                let mut end = position.saturating_add(length).min(len);
                while !source.is_char_boundary(end) {
                    end -= 1;
                }
                if start <= end {
                    source[start..end].to_string()
                } else {
                    String::new()
                }
            };
            variables.set(name, Value::String(result));
        }

        "explode" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let delimiter = params.get("delimiter").map(|s| s.as_str()).unwrap_or(",");
            // 转义序列缺省为反斜杠：被转义的分隔符视为字面量并去掉转义符
            let escape = params.get("escape").map(|s| s.as_str()).unwrap_or("\\");

            let parts = split_with_escape(&source, delimiter, escape);
            for (i, part) in parts.iter().enumerate() {
                variables.set(&format!("{}.{}", name, i), Value::String(part.clone()));
            }
            variables.set(&format!("{}.size", name), Value::Int(parts.len() as i64));
        }

        "date" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let now = local_datetime();
            variables.set(&format!("{}.year", name), Value::Int(now.year));
            variables.set(&format!("{}.month", name), Value::Int(now.month));
            variables.set(&format!("{}.day", name), Value::Int(now.day));
            variables.set(&format!("{}.hour", name), Value::Int(now.hour));
            variables.set(&format!("{}.minute", name), Value::Int(now.minute));
            variables.set(&format!("{}.second", name), Value::Int(now.second));
        }

        "unixtime" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            variables.set(name, Value::Int(ts));
        }

        "os" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            // 优先返回项目配置的目标平台（解释器面向某目标平台运行，而非宿主机器）。
            // 脚本据此选择机种分支（windows/android/ios/wasm 等），其表里并无 macos/linux。
            // 仅当未配置平台时，才退回宿主 OS 作为兜底。
            let os = if !variables.platform().is_empty() {
                variables.platform().to_string()
            } else {
                #[cfg(target_os = "windows")]
                let host = "windows";
                #[cfg(target_os = "macos")]
                let host = "macos";
                #[cfg(target_os = "linux")]
                let host = "linux";
                #[cfg(target_os = "ios")]
                let host = "iphone";
                #[cfg(target_os = "android")]
                let host = "android";
                #[cfg(not(any(
                    target_os = "windows",
                    target_os = "macos",
                    target_os = "linux",
                    target_os = "ios",
                    target_os = "android"
                )))]
                let host = "unknown";
                host.to_string()
            };
            variables.set(name, Value::String(os));
        }

        // fullscreen：当前是否全屏（全屏=1 窗口=0）；minimize：窗口是否最小化（是=1 否=0）。
        // 两者原版仅 Windows 有效。宿主（Flutter）窗口状态查询暂不可得，
        // 按非 Windows 行为保守返回 0；后续可经宿主回调（lua_engine Callbacks/FFI 桥）接入真实状态。
        "fullscreen" | "minimize" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            variables.set(name, Value::Int(0));
        }

        "file_exist" | "file_exists" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let file = get_resolved("file")
                .map(|v| v.as_string())
                .unwrap_or_default();
            // save=1 时目标为存档数据（宿主解析到存档目录）
            let save = params.get("save").map(|s| s.as_str()) == Some("1");
            // .exe 文件直接假装存在（引擎不会真正读取 exe，只是游戏的启动检查）
            let exists = if file.to_ascii_lowercase().ends_with(".exe") {
                true
            } else if let Some(exists) = with_host_hooks(|h| (h.file_exists)(&file, save)) {
                // 宿主钩子可查包文件/magic path/存档目录，优先于本地文件系统
                exists
            } else {
                std::path::Path::new(&file).exists()
            };
            variables.set(name, Value::Bool(exists));
        }

        "base64_encode" => {
            // 对 source 的 UTF-8 字节做标准 RFC 4648 BASE64 编码（带 = 填充）
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            variables.set(name, Value::String(base64_encode_bytes(source.as_bytes())));
        }

        "url_encode" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            // 按 UTF-8 字节逐字节百分号编码：多字节字符（如 'あ'）编成
            // %E3%81%82，而不是截断码点。未保留字符集为字母数字与 -_.~
            let mut encoded = String::with_capacity(source.len() * 3);
            for &b in source.as_bytes() {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    encoded.push(b as char);
                } else {
                    encoded.push_str(&format!("%{:02X}", b));
                }
            }
            variables.set(name, Value::String(encoded));
        }

        "url_decode" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            // 先把 %XX 解成原始字节收集起来，最后统一按 UTF-8 还原字符串，
            // 保证多字节序列（%E3%81%82 → あ）不被拆成乱码
            let mut bytes: Vec<u8> = Vec::with_capacity(source.len());
            let mut chars = source.chars();
            while let Some(c) = chars.next() {
                if c == '%' {
                    let hex: String = chars.by_ref().take(2).collect();
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        bytes.push(byte);
                    }
                } else if c == '+' {
                    bytes.push(b' ');
                } else {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
            let decoded = String::from_utf8_lossy(&bytes).into_owned();
            variables.set(name, Value::String(decoded));
        }

        "screen_width" | "screen_height" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let value = if system == "screen_width" {
                match variables.get("s.screen_width") {
                    Some(Value::Int(n)) if *n > 0 => *n,
                    _ => 640,
                }
            } else {
                match variables.get("s.screen_height") {
                    Some(Value::Int(n)) if *n > 0 => *n,
                    _ => 480,
                }
            };
            variables.set(name, Value::Int(value));
        }

        // 解析 exe 启动参数（Artemis.exe /a foo /b bar → name.a=foo、name.b=bar），仅 Windows。
        // 宿主是 Flutter，没有传统命令行参数且宿主查询回调暂不可得，
        // 这里实现"正确形状的保守结果"：不落任何子变量（等价于无启动参数），
        // 而不是错误地把 name 本身写成 0。后续可经宿主启动参数回调接入真实值。
        "get_exe_parameter" => {}

        // 取声音播放状态（文档 var/get_sound_info.md）：
        // 无 id：name.playing/gain/pan 为 BGM 状态 + name.N.* 为各 SE + name.size；
        // 有 id：该 SE 的 playing/gain/pan；id 不存在时仅 name.playing=0。
        "get_sound_info" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let Some(snapshot) = with_host_hooks(|h| (h.sound_info)()) else {
                // 无宿主钩子：保持旧的保守回退（脚本读到"什么都没在放"）
                variables.set(name, Value::Int(0));
                return Ok(());
            };
            apply_sound_info(
                name,
                params.get("id").map(|s| s.as_str()),
                &snapshot,
                variables,
            );
        }

        // 历史总页数（文档 var/get_backlog_size.md）：name = 页数。
        "get_backlog_size" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let size = with_host_hooks(|h| (h.backlog_size)()).unwrap_or(0);
            variables.set(name, Value::Int(size as i64));
        }

        // 某历史页的再现标签序列（文档 var/get_backlog_tags.md）：
        // name.0..N-1 为各标签字符串，name.size 为个数。
        // page 指定页号（0 起），allfont=1 时页首附页首字体标签。
        "get_backlog_tags" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let page = params
                .get("page")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
                .max(0) as usize;
            let allfont = param_is_on(params.get("allfont"));
            let tags = with_host_hooks(|h| (h.backlog_tags)(page, allfont)).flatten();
            set_pseudo_array(name, tags.as_deref(), variables);
        }

        // 当前消息层本页已执行的文本相关标签序列（文档 var/get_message_tags.md）。
        // 用法同 get_backlog_tags，但取"当前正在写的这一页"，按 id 区分消息层。
        "get_message_tags" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let id = params.get("id").map(|s| s.as_str()).unwrap_or("");
            let allfont = param_is_on(params.get("allfont"));
            let tags = with_host_hooks(|h| (h.message_tags)(id, allfont)).flatten();
            set_pseudo_array(name, tags.as_deref(), variables);
        }

        // 当前消息层文本度量（文档 var/get_message_layer_*.md）：整体宽度 /
        // 总高度 / 最后一行宽度。经宿主钩子从 text_renderer 每帧快照读取。
        "get_message_layer_width" | "get_message_layer_height" | "get_message_layer_line_width" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let (width, height, line_width) =
                with_host_hooks(|h| (h.message_layer_metrics)()).unwrap_or((0.0, 0.0, 0.0));
            let value = match system {
                "get_message_layer_width" => width,
                "get_message_layer_height" => height,
                _ => line_width,
            };
            variables.set(name, Value::Float(value as f64));
        }

        "get_layer_info" | "get_font" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            // get_layer_info 需要设置子字段（left/top/width/height），
            // 否则 Lua 端 e:var("t.ly.width") 会读到 nil。
            if system == "get_layer_info" {
                let sw = match variables.get("s.screen_width") {
                    Some(Value::Int(n)) => *n as f64,
                    _ => 1280.0,
                };
                let sh = match variables.get("s.screen_height") {
                    Some(Value::Int(n)) => *n as f64,
                    _ => 720.0,
                };
                variables.set(&format!("{}.left", name), Value::Int(0));
                variables.set(&format!("{}.top", name), Value::Int(0));
                variables.set(&format!("{}.width", name), Value::Float(sw));
                variables.set(&format!("{}.height", name), Value::Float(sh));
            } else {
                variables.set(name, Value::Int(0));
            }
        }

        "file_crc" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let file = get_resolved("file")
                .map(|v| v.as_string())
                .unwrap_or_default();
            // .exe 文件直接返回"期望 CRC"（从同名变量读取，去掉 .check 后缀）
            // 例如：name="t.crc.exe.check" → 查找 "t.crc.exe" 的值
            if file.to_ascii_lowercase().ends_with(".exe") {
                let expected_name = name.strip_suffix(".check").unwrap_or(name);
                let expected_crc = variables
                    .get(expected_name)
                    .map(|v| v.as_string())
                    .unwrap_or_default();
                variables.set(name, Value::String(expected_crc));
            } else {
                // zerox=1 添加 0x 前缀；caps=1 十六进制大写（缺省小写）
                let zerox = params.get("zerox").map(|s| s.as_str()) == Some("1");
                let caps = params.get("caps").map(|s| s.as_str()) == Some("1");
                let crc = with_host_hooks(|h| (h.file_crc32)(&file))
                    .flatten()
                    .map(|crc| format_crc32(crc, zerox, caps))
                    // 无钩子或读不到文件：保持旧行为返回空串
                    .unwrap_or_default();
                variables.set(name, Value::String(crc));
            }
        }

        "implode" => {
            // 把伪数组 source.0 .. source.(size-1) 用分隔符连接成一个字符串存入 name。
            // source 是伪数组变量的基名（如 "foo"），个数由 source.size 决定；
            // size 缺失时退化为从 0 起连续探测索引。
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let delimiter = params.get("delimiter").map(|s| s.as_str()).unwrap_or(",");

            let size = variables
                .get(&format!("{}.size", source))
                .and_then(|v| v.as_int());
            let mut items: Vec<String> = Vec::new();
            match size {
                Some(n) => {
                    for i in 0..n.max(0) {
                        let item = variables
                            .get(&format!("{}.{}", source, i))
                            .map(|v| v.as_string())
                            .unwrap_or_default();
                        items.push(item);
                    }
                }
                None => {
                    // 无 size 时按连续索引探测，遇到第一个缺失的下标即停
                    let mut i = 0i64;
                    while let Some(v) = variables.get(&format!("{}.{}", source, i)) {
                        items.push(v.as_string());
                        i += 1;
                    }
                }
            }
            variables.set(name, Value::String(items.join(delimiter)));
        }

        "hmac_sha1_base64" => {
            // 用 key 对 message 计算 HMAC-SHA1，结果做 BASE64 编码后存 name
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let message = get_resolved("message")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let key = get_resolved("key")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let mac = hmac_sha1(key.as_bytes(), message.as_bytes());
            variables.set(name, Value::String(base64_encode_bytes(&mac)));
        }

        "character_reference_to_utf8" => {
            // 把含数值字符引用（&#nnnn; / &#xhhhh;）的字符串还原为 UTF-8 字符串，
            // 其余文本原样保留，非法引用不做替换
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            variables.set(name, Value::String(decode_character_references(&source)));
        }

        "convert_encoding" => {
            // 转换字符串编码。内部字符串是 UTF-8，非 UTF-8 编码的字节串以
            // 「逐字节映射为 char（Latin-1 方式）」的形式承载在 Value::String 中，
            // 因此 to=sjis 的输出可以再作为 from=sjis 的输入无损往返。
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let from = params.get("from").map(|s| s.as_str()).unwrap_or("");
            let to = params.get("to").map(|s| s.as_str()).unwrap_or("utf8");
            variables.set(
                name,
                Value::String(convert_encoding_impl(&source, from, to)),
            );
        }

        // 文档标注「为向后兼容保留，勿再使用」。内部字符串本就是 UTF-8，
        // 这里把 source 原样落到 name（合理退化），避免静默 no-op 导致脚本读到 nil。
        "to_sjis" | "to_utf8" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let source = get_resolved("source")
                .map(|v| v.as_string())
                .unwrap_or_default();
            variables.set(name, Value::String(source));
        }

        // 取存档文件的更新日期时间字符串（文档 var/file_update_time.md）：
        // format 支持 yyyy/yy（年）、MM dd hh mm ss（补零）、M d h m s（不补零），
        // 缺省 yyyy/MM/dd hh:mm:ss；文件不存在时返回 noexist 参数值。
        "file_update_time" => {
            let name = params.get("name").map(|s| s.as_str()).unwrap_or("");
            let file = get_resolved("file")
                .map(|v| v.as_string())
                .unwrap_or_default();
            let format = params
                .get("format")
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("yyyy/MM/dd hh:mm:ss");
            let noexist = params.get("noexist").map(|s| s.as_str()).unwrap_or("");
            let value = with_host_hooks(|h| (h.file_update_time)(&file))
                .flatten()
                .map(|components| format_update_time(&components, format))
                .unwrap_or_else(|| noexist.to_string());
            variables.set(name, Value::String(value));
        }

        _ => {}
    }

    Ok(())
}

/// 把声音快照落成 get_sound_info 约定的变量（伪数组/子字段形状见文档示例）。
fn apply_sound_info(
    name: &str,
    id: Option<&str>,
    snapshot: &SoundInfoSnapshot,
    variables: &mut VariableStore,
) {
    // 有 id：只查该 SE；不存在时仅设 name.playing=0（文档明确）。
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        match snapshot.se.iter().find(|ch| ch.id == id) {
            Some(ch) => {
                variables.set(
                    &format!("{name}.playing"),
                    Value::Int(i64::from(ch.playing)),
                );
                variables.set(&format!("{name}.gain"), Value::Int(ch.gain));
                variables.set(&format!("{name}.pan"), Value::Int(ch.pan));
            }
            None => {
                variables.set(&format!("{name}.playing"), Value::Int(0));
            }
        }
        return;
    }

    // 无 id：BGM 状态 + 各 SE 伪数组 + size。BGM 未播放时 playing=0，
    // gain/pan 用未设置时的缺省刻度（1000/0）。
    let (playing, gain, pan) = snapshot
        .bgm
        .as_ref()
        .map(|ch| (i64::from(ch.playing), ch.gain, ch.pan))
        .unwrap_or((0, 1000, 0));
    variables.set(&format!("{name}.playing"), Value::Int(playing));
    variables.set(&format!("{name}.gain"), Value::Int(gain));
    variables.set(&format!("{name}.pan"), Value::Int(pan));
    for (index, ch) in snapshot.se.iter().enumerate() {
        variables.set(&format!("{name}.{index}.id"), Value::String(ch.id.clone()));
        variables.set(
            &format!("{name}.{index}.playing"),
            Value::Int(i64::from(ch.playing)),
        );
        variables.set(&format!("{name}.{index}.gain"), Value::Int(ch.gain));
        variables.set(&format!("{name}.{index}.pan"), Value::Int(ch.pan));
    }
    variables.set(
        &format!("{name}.size"),
        Value::Int(snapshot.se.len() as i64),
    );
}

/// 格式化 CRC32：8 位十六进制，zerox 加 0x 前缀，caps 大写。
fn format_crc32(crc: u32, zerox: bool, caps: bool) -> String {
    let hex = if caps {
        format!("{crc:08X}")
    } else {
        format!("{crc:08x}")
    };
    if zerox { format!("0x{hex}") } else { hex }
}

/// 按 Artemis 的日期格式串渲染本地时间分量 [年,月,日,时,分,秒]。
///
/// 模式：yyyy=四位年、yy=年末两位、MM/dd/hh/mm/ss=补零两位、M/d/h/m/s=不补零；
/// 其余字符原样输出。较长模式优先匹配（yyyy 先于 yy，MM 先于 M）。
fn format_update_time(c: &[i64; 6], format: &str) -> String {
    let [year, month, day, hour, minute, second] = *c;
    let mut out = String::with_capacity(format.len() + 8);
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 统计当前字符的连续重复长度
        let ch = chars[i];
        let run = chars[i..].iter().take_while(|&&c| c == ch).count();
        let (text, consumed) = match ch {
            'y' if run >= 4 => (format!("{year:04}"), 4),
            'y' if run >= 2 => (format!("{:02}", year.rem_euclid(100)), 2),
            'M' if run >= 2 => (format!("{month:02}"), 2),
            'M' => (month.to_string(), 1),
            'd' if run >= 2 => (format!("{day:02}"), 2),
            'd' => (day.to_string(), 1),
            'h' if run >= 2 => (format!("{hour:02}"), 2),
            'h' => (hour.to_string(), 1),
            'm' if run >= 2 => (format!("{minute:02}"), 2),
            'm' => (minute.to_string(), 1),
            's' if run >= 2 => (format!("{second:02}"), 2),
            's' => (second.to_string(), 1),
            other => (other.to_string(), 1),
        };
        out.push_str(&text);
        i += consumed;
    }
    out
}

/// 按分隔符切分字符串，支持转义序列（explode 用）。
///
/// 被转义的分隔符视为字面量并去掉转义符（`a\,b,c` 按 `,` 切成 `a,b` 和 `c`）；
/// 转义符自身也可被转义（`\\` → `\`）。escape 为空串时不做转义处理。
fn split_with_escape(source: &str, delimiter: &str, escape: &str) -> Vec<String> {
    if delimiter.is_empty() {
        return vec![source.to_string()];
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while i < source.len() {
        let rest = &source[i..];
        if !escape.is_empty() && rest.starts_with(escape) {
            let after = &rest[escape.len()..];
            if after.starts_with(delimiter) {
                // 被转义的分隔符：作为字面量并去掉转义符
                current.push_str(delimiter);
                i += escape.len() + delimiter.len();
                continue;
            }
            if after.starts_with(escape) {
                // 转义符转义自身
                current.push_str(escape);
                i += escape.len() * 2;
                continue;
            }
        }
        if rest.starts_with(delimiter) {
            parts.push(std::mem::take(&mut current));
            i += delimiter.len();
            continue;
        }
        let ch = rest.chars().next().unwrap();
        current.push(ch);
        i += ch.len_utf8();
    }
    parts.push(current);
    parts
}

/// RFC 4648 标准 BASE64 编码（带 `=` 填充）
fn base64_encode_bytes(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// SHA-1 摘要（RFC 3174），返回 20 字节
fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    // 填充：0x80 + 若干 0x00，直到长度 ≡ 56 (mod 64)，末尾附 64 位大端比特长度
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// HMAC-SHA1（RFC 2104）：块大小 64 字节，超长 key 先做一次 SHA-1
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..20].copy_from_slice(&sha1_digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(message);
    let inner_hash = sha1_digest(&inner);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5C).collect();
    outer.extend_from_slice(&inner_hash);
    sha1_digest(&outer)
}

/// 把含数值字符引用（`&#nnnn;` / `&#xhhhh;`）的字符串还原为普通字符串。
/// 非法引用（无分号、非法数字、超出 Unicode 范围）原样保留。
fn decode_character_references(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < source.len() {
        let rest = &source[i..];
        if rest.starts_with("&#") {
            if let Some(end) = rest.find(';') {
                let body = &rest[2..end];
                let code =
                    if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
                        body.parse::<u32>().ok()
                    } else {
                        None
                    };
                if let Some(ch) = code.and_then(char::from_u32) {
                    out.push(ch);
                    i += end + 1;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 把「逐字节映射为 char」（Latin-1 方式）承载的字节串还原为原始字节；
/// 若字符串含超出 U+00FF 的字符（即不是承载形式），退回其 UTF-8 字节序列。
fn carried_string_to_bytes(s: &str) -> Vec<u8> {
    if s.chars().all(|c| (c as u32) <= 0xFF) {
        s.chars().map(|c| c as u8).collect()
    } else {
        s.as_bytes().to_vec()
    }
}

/// 把原始字节以「逐字节映射为 char」（Latin-1 方式）承载进 String，
/// 与 [`carried_string_to_bytes`] 互逆，用于在 UTF-8 内部表示中携带 sjis/euc/jis 字节
fn bytes_to_carried_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// convert_encoding 的核心实现。
///
/// 内部字符串是 UTF-8：`from`/`to` 为 sjis/euc/jis 时，对应字节串以
/// Latin-1 逐字节映射的形式承载于 String，to=sjis 的结果可作为 from=sjis 的输入无损往返。
/// `from` 缺省时自动识别（优先 UTF-8，含 ESC 判为 JIS，再依次试 Shift_JIS / EUC-JP；短串可能识别失败）。
fn convert_encoding_impl(source: &str, from: &str, to: &str) -> String {
    use encoding_rs::{EUC_JP, ISO_2022_JP, SHIFT_JIS};

    // 第一步：按 from 把 source 解码为 UTF-8 文本
    let text: String = match from {
        "utf8" => source.to_string(),
        "sjis" => {
            let bytes = carried_string_to_bytes(source);
            SHIFT_JIS.decode(&bytes).0.into_owned()
        }
        "euc" => {
            let bytes = carried_string_to_bytes(source);
            EUC_JP.decode(&bytes).0.into_owned()
        }
        "jis" => {
            let bytes = carried_string_to_bytes(source);
            ISO_2022_JP.decode(&bytes).0.into_owned()
        }
        _ => {
            // 自动识别：先看字节是否本就是合法 UTF-8，再按特征依次尝试
            let bytes = carried_string_to_bytes(source);
            if let Ok(s) = std::str::from_utf8(&bytes) {
                s.to_string()
            } else if bytes.contains(&0x1B) {
                // ISO-2022-JP 以 ESC 序列切换字符集
                ISO_2022_JP.decode(&bytes).0.into_owned()
            } else {
                let (decoded, _, had_errors) = SHIFT_JIS.decode(&bytes);
                if !had_errors {
                    decoded.into_owned()
                } else {
                    let (decoded, _, had_errors) = EUC_JP.decode(&bytes);
                    if !had_errors {
                        decoded.into_owned()
                    } else {
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                }
            }
        }
    };

    // 第二步：按 to 编码输出
    match to {
        "sjis" => bytes_to_carried_string(&SHIFT_JIS.encode(&text).0),
        "euc" => bytes_to_carried_string(&EUC_JP.encode(&text).0),
        "jis" => bytes_to_carried_string(&ISO_2022_JP.encode(&text).0),
        // utf8 或未指定：直接输出 UTF-8 文本
        _ => text,
    }
}

#[derive(Debug, Clone, Copy)]
struct DateTimeParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

fn local_datetime() -> DateTimeParts {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    local_datetime_from_unix(seconds).unwrap_or_else(|| utc_datetime_from_unix(seconds))
}

#[cfg(unix)]
fn local_datetime_from_unix(seconds: i64) -> Option<DateTimeParts> {
    let raw = seconds as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let ptr = unsafe { libc::localtime_r(&raw, tm.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some(DateTimeParts {
        year: i64::from(tm.tm_year) + 1900,
        month: i64::from(tm.tm_mon) + 1,
        day: i64::from(tm.tm_mday),
        hour: i64::from(tm.tm_hour),
        minute: i64::from(tm.tm_min),
        second: i64::from(tm.tm_sec),
    })
}

#[cfg(not(unix))]
fn local_datetime_from_unix(_seconds: i64) -> Option<DateTimeParts> {
    None
}

fn utc_datetime_from_unix(seconds: i64) -> DateTimeParts {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateTimeParts {
        year,
        month,
        day,
        hour: secs_of_day / 3600,
        minute: secs_of_day % 3600 / 60,
        second: secs_of_day % 60,
    }
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// 删除变量树（包括子变量）
fn delete_variable_tree(variables: &mut VariableStore, name: &str) {
    variables.remove(name);

    let prefix = format!("{}.", name);
    let to_remove: Vec<String> = variables
        .iter_local()
        .chain(variables.iter_global())
        .chain(variables.iter_temp())
        .chain(variables.iter_system())
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(k, _)| k.clone())
        .collect();

    for key in to_remove {
        variables.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pseudo_array_lays_out_indices_and_size() {
        let mut vars = VariableStore::new();
        set_pseudo_array(
            "t.bl",
            Some(&["[print data=\"你好\"]".to_string(), "[rt]".to_string()]),
            &mut vars,
        );
        assert_eq!(
            vars.get("t.bl.0").map(Value::as_string).as_deref(),
            Some("[print data=\"你好\"]")
        );
        assert_eq!(
            vars.get("t.bl.1").map(Value::as_string).as_deref(),
            Some("[rt]")
        );
        assert_eq!(vars.get("t.bl.size").and_then(Value::as_int), Some(2));
        // None（查询未命中）：只落 size=0，不留脏索引。
        set_pseudo_array("t.empty", None, &mut vars);
        assert_eq!(vars.get("t.empty.size").and_then(Value::as_int), Some(0));
        assert!(vars.get("t.empty.0").is_none());
    }

    #[test]
    fn backlog_var_systems_read_installed_hooks() {
        // 装一个返回固定历史的钩子，验证三个 backlog var system 落值。
        set_host_query_hooks(HostQueryHooks {
            file_exists: Box::new(|_, _| false),
            file_crc32: Box::new(|_| None),
            file_update_time: Box::new(|_| None),
            sound_info: Box::new(SoundInfoSnapshot::default),
            backlog_size: Box::new(|| 3),
            backlog_tags: Box::new(|page, allfont| {
                (page < 3).then(|| {
                    let mut tags = Vec::new();
                    if allfont {
                        tags.push("[font size=\"40\"]".to_string());
                    }
                    tags.push(format!("[print data=\"page{page}\"]"));
                    tags
                })
            }),
            message_tags: Box::new(|id, _| {
                (id == "adv01").then(|| vec!["[print data=\"cur\"]".to_string()])
            }),
            message_layer_metrics: Box::new(|| (320.0, 48.0, 120.0)),
        });

        let run = |params: &[(&str, &str)]| {
            let mut vars = VariableStore::new();
            let map: HashMap<String, String> = params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            apply_var_tag(&map, &mut vars).unwrap();
            vars
        };

        let size = run(&[("system", "get_backlog_size"), ("name", "t.n")]);
        assert_eq!(size.get("t.n").and_then(Value::as_int), Some(3));

        let tags = run(&[
            ("system", "get_backlog_tags"),
            ("name", "t.bl"),
            ("page", "1"),
            ("allfont", "1"),
        ]);
        assert_eq!(tags.get("t.bl.size").and_then(Value::as_int), Some(2));
        assert_eq!(
            tags.get("t.bl.0").map(Value::as_string).as_deref(),
            Some("[font size=\"40\"]")
        );
        assert_eq!(
            tags.get("t.bl.1").map(Value::as_string).as_deref(),
            Some("[print data=\"page1\"]")
        );

        // 越界页 → None → size=0。
        let oob = run(&[
            ("system", "get_backlog_tags"),
            ("name", "t.o"),
            ("page", "9"),
        ]);
        assert_eq!(oob.get("t.o.size").and_then(Value::as_int), Some(0));

        let msg = run(&[
            ("system", "get_message_tags"),
            ("name", "t.m"),
            ("id", "adv01"),
        ]);
        assert_eq!(msg.get("t.m.size").and_then(Value::as_int), Some(1));
        assert_eq!(
            msg.get("t.m.0").map(Value::as_string).as_deref(),
            Some("[print data=\"cur\"]")
        );

        // 文本度量：钩子返回 (320,48,120) → width/height/line_width 分别落值。
        let w = run(&[("system", "get_message_layer_width"), ("name", "t.w")]);
        assert_eq!(w.get("t.w").and_then(Value::as_float), Some(320.0));
        let h = run(&[("system", "get_message_layer_height"), ("name", "t.h")]);
        assert_eq!(h.get("t.h").and_then(Value::as_float), Some(48.0));
        let lw = run(&[("system", "get_message_layer_line_width"), ("name", "t.lw")]);
        assert_eq!(lw.get("t.lw").and_then(Value::as_float), Some(120.0));

        // 清理进程级钩子，避免污染其它测试。
        *HOST_QUERY_HOOKS.write().unwrap() = None;
    }

    #[test]
    fn crc_formatting_honors_zerox_and_caps() {
        // file_crc.md：zerox=1 加 0x 前缀；caps=1 大写；缺省小写无前缀
        assert_eq!(format_crc32(0x89AB_CDEF, false, false), "89abcdef");
        assert_eq!(format_crc32(0x89AB_CDEF, true, false), "0x89abcdef");
        assert_eq!(format_crc32(0x89AB_CDEF, false, true), "89ABCDEF");
        assert_eq!(format_crc32(0x89AB_CDEF, true, true), "0x89ABCDEF");
        // 高位为 0 时补足 8 位
        assert_eq!(format_crc32(0x1F, false, false), "0000001f");
    }

    #[test]
    fn update_time_format_patterns_match_doc() {
        // file_update_time.md：yyyy/yy 年、MM dd hh mm ss 补零、M d h m s 不补零
        let c = [2026, 7, 3, 9, 5, 8];
        assert_eq!(
            format_update_time(&c, "yyyy/MM/dd hh:mm:ss"),
            "2026/07/03 09:05:08"
        );
        assert_eq!(format_update_time(&c, "yy-M-d h:m:s"), "26-7-3 9:5:8");
        // 非模式字符原样输出
        assert_eq!(format_update_time(&c, "yyyy年M月d日"), "2026年7月3日");
    }

    #[test]
    fn sound_info_pseudo_array_matches_doc_example() {
        // get_sound_info.md 的示例形状：BGM 顶层字段 + SE 伪数组 + size
        let snapshot = SoundInfoSnapshot {
            bgm: Some(SoundChannelInfo {
                id: "bgm".into(),
                playing: true,
                gain: 500,
                pan: 0,
            }),
            se: vec![
                SoundChannelInfo {
                    id: "bar".into(),
                    playing: true,
                    gain: 1000,
                    pan: -500,
                },
                SoundChannelInfo {
                    id: "hoge".into(),
                    playing: true,
                    gain: 1000,
                    pan: 500,
                },
            ],
        };

        let mut vars = VariableStore::new();
        apply_sound_info("result", None, &snapshot, &mut vars);
        assert_eq!(vars.get("result.playing"), Some(&Value::Int(1)));
        assert_eq!(vars.get("result.gain"), Some(&Value::Int(500)));
        assert_eq!(vars.get("result.pan"), Some(&Value::Int(0)));
        assert_eq!(
            vars.get("result.0.id"),
            Some(&Value::String("bar".to_string()))
        );
        assert_eq!(vars.get("result.0.pan"), Some(&Value::Int(-500)));
        assert_eq!(
            vars.get("result.1.id"),
            Some(&Value::String("hoge".to_string()))
        );
        assert_eq!(vars.get("result.1.pan"), Some(&Value::Int(500)));
        assert_eq!(vars.get("result.size"), Some(&Value::Int(2)));

        // 有 id：只落该 SE 的三个字段
        let mut vars = VariableStore::new();
        apply_sound_info("result", Some("bar"), &snapshot, &mut vars);
        assert_eq!(vars.get("result.playing"), Some(&Value::Int(1)));
        assert_eq!(vars.get("result.gain"), Some(&Value::Int(1000)));
        assert_eq!(vars.get("result.pan"), Some(&Value::Int(-500)));

        // id 不存在：仅 result.playing=0（文档明确）
        let mut vars = VariableStore::new();
        apply_sound_info("result", Some("nothing"), &snapshot, &mut vars);
        assert_eq!(vars.get("result.playing"), Some(&Value::Int(0)));
        assert_eq!(vars.get("result.gain"), None);
    }

    #[test]
    fn test_var_system_var_exist() {
        let mut vars = VariableStore::new();
        vars.set("foo", Value::Int(42));

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "result".to_string());
            m.insert("target".to_string(), "foo".to_string());
            m
        };
        let resolved = HashMap::new();

        execute_var_system("var_exist", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("result"), Some(&Value::Bool(true)));

        let params2 = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "result".to_string());
            m.insert("target".to_string(), "bar".to_string());
            m
        };
        execute_var_system("var_exist", &params2, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("result"), Some(&Value::Bool(false)));
    }

    #[test]
    fn test_var_system_explode() {
        let mut vars = VariableStore::new();

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "arr".to_string());
            m
        };
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("a,b,c".to_string()));

        execute_var_system("explode", &params, &resolved, &mut vars).unwrap();

        assert_eq!(vars.get("arr.0"), Some(&Value::String("a".to_string())));
        assert_eq!(vars.get("arr.1"), Some(&Value::String("b".to_string())));
        assert_eq!(vars.get("arr.2"), Some(&Value::String("c".to_string())));
        assert_eq!(vars.get("arr.size"), Some(&Value::Int(3)));
    }

    #[test]
    fn test_var_system_length() {
        let mut vars = VariableStore::new();

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "len".to_string());
            m.insert("mode".to_string(), "1".to_string());
            m
        };
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("hello".to_string()));

        execute_var_system("length", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("len"), Some(&Value::Int(5)));
    }

    #[test]
    fn test_var_system_find() {
        let mut vars = VariableStore::new();

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "pos".to_string());
            m
        };
        let mut resolved = HashMap::new();
        resolved.insert(
            "source".to_string(),
            Value::String("hello world".to_string()),
        );
        resolved.insert("string".to_string(), Value::String("world".to_string()));

        execute_var_system("find", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("pos"), Some(&Value::Int(6)));
    }

    #[test]
    fn test_var_system_substr() {
        let mut vars = VariableStore::new();

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "sub".to_string());
            m.insert("mode".to_string(), "1".to_string());
            m
        };
        let mut resolved = HashMap::new();
        resolved.insert(
            "source".to_string(),
            Value::String("hello world".to_string()),
        );
        resolved.insert("position".to_string(), Value::Int(6));
        resolved.insert("length".to_string(), Value::Int(5));

        execute_var_system("substr", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("sub"), Some(&Value::String("world".to_string())));
    }

    #[test]
    fn test_var_system_delete() {
        let mut vars = VariableStore::new();
        vars.set("foo.bar", Value::Int(1));
        vars.set("foo.baz", Value::Int(2));
        vars.set("foo", Value::Int(3));

        let params = {
            let mut m = HashMap::new();
            m.insert("name".to_string(), "foo".to_string());
            m
        };
        let resolved = HashMap::new();

        execute_var_system("delete", &params, &resolved, &mut vars).unwrap();

        assert_eq!(vars.get("foo"), None);
        assert_eq!(vars.get("foo.bar"), None);
        assert_eq!(vars.get("foo.baz"), None);
    }

    #[test]
    fn test_var_system_date_uses_current_clock() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "now".to_string())]);
        let resolved = HashMap::new();

        execute_var_system("date", &params, &resolved, &mut vars).unwrap();

        let year = vars.get("now.year").and_then(Value::as_int).unwrap_or(0);
        let month = vars.get("now.month").and_then(Value::as_int).unwrap_or(0);
        let day = vars.get("now.day").and_then(Value::as_int).unwrap_or(0);
        assert!(year >= 2026, "date year should come from current clock");
        assert!((1..=12).contains(&month));
        assert!((1..=31).contains(&day));
    }

    #[test]
    fn utc_datetime_from_unix_splits_epoch() {
        let epoch = utc_datetime_from_unix(0);
        assert_eq!(epoch.year, 1970);
        assert_eq!(epoch.month, 1);
        assert_eq!(epoch.day, 1);
        assert_eq!(epoch.hour, 0);
        assert_eq!(epoch.minute, 0);
        assert_eq!(epoch.second, 0);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    // ---- substr 字节模式 ----

    #[test]
    fn substr_byte_mode_default_length_truncates_to_end() {
        // position>0 且 length 缺省时应截到串尾，而不是越界返回空串
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "sub".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert(
            "source".to_string(),
            Value::String("hello world".to_string()),
        );
        resolved.insert("position".to_string(), Value::Int(6));

        execute_var_system("substr", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("sub"), Some(&Value::String("world".to_string())));
    }

    #[test]
    fn substr_byte_mode_snaps_to_char_boundary() {
        // 起点/终点落在多字节字符中间时收拢到合法边界，而不是返回空串
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "sub".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("あいう".to_string()));
        resolved.insert("position".to_string(), Value::Int(1));
        resolved.insert("length".to_string(), Value::Int(8));

        execute_var_system("substr", &params, &resolved, &mut vars).unwrap();
        // 起点 1 → 3（い 的开头），终点 9 恰是串尾
        assert_eq!(vars.get("sub"), Some(&Value::String("いう".to_string())));
    }

    #[test]
    fn substr_byte_mode_out_of_range_returns_empty() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "sub".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("abc".to_string()));
        resolved.insert("position".to_string(), Value::Int(100));

        execute_var_system("substr", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("sub"), Some(&Value::String(String::new())));
    }

    // ---- explode 转义 ----

    #[test]
    fn explode_escaped_delimiter_is_literal() {
        // "a\,b,c" 应切成 2 段：a,b 和 c，且去掉转义符
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "arr".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("a\\,b,c".to_string()));

        execute_var_system("explode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("arr.0"), Some(&Value::String("a,b".to_string())));
        assert_eq!(vars.get("arr.1"), Some(&Value::String("c".to_string())));
        assert_eq!(vars.get("arr.size"), Some(&Value::Int(2)));
    }

    #[test]
    fn split_with_escape_handles_escaped_escape() {
        // "a\\,b"（字面 a、反斜杠、反斜杠、逗号、b）：\\ → 字面反斜杠，逗号照常分隔
        let parts = split_with_escape("a\\\\,b", ",", "\\");
        assert_eq!(parts, vec!["a\\".to_string(), "b".to_string()]);
        // 转义序列为空串时不做转义
        let parts = split_with_escape("a\\,b", ",", "");
        assert_eq!(parts, vec!["a\\".to_string(), "b".to_string()]);
    }

    // ---- implode ----

    #[test]
    fn implode_joins_pseudo_array() {
        let mut vars = VariableStore::new();
        vars.set("foo.0", Value::String("bar".to_string()));
        vars.set("foo.1", Value::String("hoge".to_string()));
        vars.set("foo.2", Value::String("fuga".to_string()));
        vars.set("foo.size", Value::Int(3));

        let params = HashMap::from([("name".to_string(), "result".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("foo".to_string()));

        execute_var_system("implode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(
            vars.get("result"),
            Some(&Value::String("bar,hoge,fuga".to_string()))
        );
    }

    #[test]
    fn implode_custom_delimiter_and_missing_size() {
        let mut vars = VariableStore::new();
        vars.set("bar.0", Value::String("x".to_string()));
        vars.set("bar.1", Value::String("y".to_string()));
        // 无 bar.size：按连续下标探测

        let params = HashMap::from([
            ("name".to_string(), "result".to_string()),
            ("delimiter".to_string(), "-".to_string()),
        ]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("bar".to_string()));

        execute_var_system("implode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("result"), Some(&Value::String("x-y".to_string())));
    }

    // ---- url_encode / url_decode ----

    #[test]
    fn url_encode_multibyte_utf8_bytes() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "enc".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("aあ-".to_string()));

        execute_var_system("url_encode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(
            vars.get("enc"),
            Some(&Value::String("a%E3%81%82-".to_string()))
        );
    }

    #[test]
    fn url_decode_multibyte_utf8_bytes() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "dec".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert(
            "source".to_string(),
            Value::String("a%E3%81%82+b".to_string()),
        );

        execute_var_system("url_decode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("dec"), Some(&Value::String("aあ b".to_string())));
    }

    #[test]
    fn url_encode_decode_roundtrip_japanese() {
        // あ → %E3%81%82 → あ 往返
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "enc".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("あ".to_string()));
        execute_var_system("url_encode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(
            vars.get("enc"),
            Some(&Value::String("%E3%81%82".to_string()))
        );

        let params2 = HashMap::from([("name".to_string(), "dec".to_string())]);
        let mut resolved2 = HashMap::new();
        resolved2.insert(
            "source".to_string(),
            Value::String(vars.get("enc").unwrap().as_string()),
        );
        execute_var_system("url_decode", &params2, &resolved2, &mut vars).unwrap();
        assert_eq!(vars.get("dec"), Some(&Value::String("あ".to_string())));
    }

    // ---- base64_encode ----

    #[test]
    fn base64_rfc4648_test_vectors() {
        assert_eq!(base64_encode_bytes(b""), "");
        assert_eq!(base64_encode_bytes(b"f"), "Zg==");
        assert_eq!(base64_encode_bytes(b"fo"), "Zm8=");
        assert_eq!(base64_encode_bytes(b"foo"), "Zm9v");
        assert_eq!(base64_encode_bytes(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode_bytes(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode_bytes(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_tag_writes_real_encoding() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "b64".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("Hello".to_string()));

        execute_var_system("base64_encode", &params, &resolved, &mut vars).unwrap();
        assert_eq!(
            vars.get("b64"),
            Some(&Value::String("SGVsbG8=".to_string()))
        );
    }

    // ---- SHA-1 / HMAC-SHA1 ----

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(
            hex(&sha1_digest(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex(&sha1_digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn hmac_sha1_rfc2202_vector() {
        // RFC 2202 test case 2
        let mac = hmac_sha1(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex(&mac), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn hmac_sha1_base64_via_apply_var_tag() {
        // 经 apply_var_tag 验证 message/key 已进入表达式预解析列表并正确落值
        let mut vars = VariableStore::new();
        let params = HashMap::from([
            ("system".to_string(), "hmac_sha1_base64".to_string()),
            ("name".to_string(), "mac".to_string()),
            (
                "message".to_string(),
                "The quick brown fox jumps over the lazy dog".to_string(),
            ),
            ("key".to_string(), "key".to_string()),
        ]);
        apply_var_tag(&params, &mut vars).unwrap();
        assert_eq!(
            vars.get("mac"),
            Some(&Value::String("3nybhbi3iqa8ino29wqQcBydtNk=".to_string()))
        );
    }

    // ---- character_reference_to_utf8 ----

    #[test]
    fn character_reference_decimal_and_hex() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "out".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert(
            "source".to_string(),
            Value::String("&#12354;&#x3044;u!".to_string()),
        );

        execute_var_system("character_reference_to_utf8", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("out"), Some(&Value::String("あいu!".to_string())));
    }

    #[test]
    fn character_reference_invalid_kept_as_is() {
        assert_eq!(
            decode_character_references("&#zz; &#x; &#12354"),
            "&#zz; &#x; &#12354"
        );
    }

    // ---- convert_encoding ----

    #[test]
    fn convert_encoding_utf8_to_sjis_carried_bytes() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([
            ("name".to_string(), "out".to_string()),
            ("from".to_string(), "utf8".to_string()),
            ("to".to_string(), "sjis".to_string()),
        ]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("あ".to_string()));

        execute_var_system("convert_encoding", &params, &resolved, &mut vars).unwrap();
        // 'あ' 的 Shift_JIS 字节是 0x82 0xA0，按 Latin-1 逐字节承载
        assert_eq!(
            vars.get("out"),
            Some(&Value::String("\u{82}\u{A0}".to_string()))
        );
    }

    #[test]
    fn convert_encoding_roundtrip_and_autodetect() {
        // utf8 → sjis → utf8 往返（第二步 from 缺省走自动识别）
        let sjis = convert_encoding_impl("こんにちは", "utf8", "sjis");
        assert_eq!(convert_encoding_impl(&sjis, "sjis", "utf8"), "こんにちは");
        assert_eq!(convert_encoding_impl(&sjis, "", "utf8"), "こんにちは");
        // 纯 UTF-8 输入自动识别为 UTF-8，原样输出
        assert_eq!(convert_encoding_impl("日本語", "", "utf8"), "日本語");
    }

    // ---- to_sjis / to_utf8 ----

    #[test]
    fn to_sjis_to_utf8_pass_source_through() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "out".to_string())]);
        let mut resolved = HashMap::new();
        resolved.insert("source".to_string(), Value::String("text".to_string()));

        execute_var_system("to_sjis", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("out"), Some(&Value::String("text".to_string())));

        vars.remove("out");
        execute_var_system("to_utf8", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("out"), Some(&Value::String("text".to_string())));
    }

    // ---- get_exe_parameter / fullscreen / minimize ----

    #[test]
    fn get_exe_parameter_sets_no_variables() {
        // 保守正确形状：无启动参数 → 不落任何变量（尤其不能把 name 本身写成 0）
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "test".to_string())]);
        let resolved = HashMap::new();

        execute_var_system("get_exe_parameter", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("test"), None);
        assert_eq!(vars.get("test.a"), None);
    }

    #[test]
    fn fullscreen_and_minimize_return_zero() {
        let mut vars = VariableStore::new();
        let params = HashMap::from([("name".to_string(), "flag".to_string())]);
        let resolved = HashMap::new();

        execute_var_system("fullscreen", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("flag"), Some(&Value::Int(0)));

        execute_var_system("minimize", &params, &resolved, &mut vars).unwrap();
        assert_eq!(vars.get("flag"), Some(&Value::Int(0)));
    }
}
