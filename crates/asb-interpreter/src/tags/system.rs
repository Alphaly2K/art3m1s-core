//! 系统操作标签处理器
//!
//! 实现 exec, save, load, debug, debugprint, caption, mouse, file, httpget, httppost 等系统操作标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::Event;

/// [exec] 执行用户操作
pub struct ExecHandler;

impl TagHandler for ExecHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let command = ctx.instruction.get("command").unwrap_or("").to_string();
        let mode = ctx
            .instruction
            .get("mode")
            .and_then(|v| v.parse::<i32>().ok());
        Ok(TagResult::Emit(Event::Exec { command, mode }))
    }
}

/// [save] 存档
pub struct SaveHandler;

impl TagHandler for SaveHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.resolve_param("file")?.as_string();
        Ok(TagResult::Emit(Event::SaveGame { file }))
    }
}

/// [load] 读档
pub struct LoadHandler;

impl TagHandler for LoadHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.resolve_param("file")?.as_string();
        let trans_type = ctx
            .instruction
            .get("type")
            .and_then(|v| v.parse::<i32>().ok());
        Ok(TagResult::Emit(Event::LoadGame { file, trans_type }))
    }
}

/// [debug] 调试设置
pub struct DebugHandler;

impl TagHandler for DebugHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mode = ctx
            .instruction
            .get("mode")
            .and_then(|v| v.parse::<i32>().ok());
        let level = ctx
            .instruction
            .get("level")
            .and_then(|v| v.parse::<i32>().ok());
        Ok(TagResult::Emit(Event::DebugConfig { mode, level }))
    }
}

/// [debugprint] 调试输出
pub struct DebugprintHandler;

impl TagHandler for DebugprintHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let level = ctx
            .instruction
            .get("level")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        let data = ctx.resolve_param("data")?.as_string();
        Ok(TagResult::Emit(Event::DebugPrint { level, data }))
    }
}

/// [debugreload] 调试重载
pub struct DebugreloadHandler;

impl TagHandler for DebugreloadHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::DebugReload))
    }
}

/// [caption] 窗口标题
pub struct CaptionHandler;

impl TagHandler for CaptionHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let data = ctx.resolve_param("data")?.as_string();
        Ok(TagResult::Emit(Event::Caption { data }))
    }
}

/// [mouse] 鼠标设置
pub struct MouseHandler;

impl TagHandler for MouseHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let left = ctx
            .instruction
            .get("left")
            .and_then(|v| v.parse::<i32>().ok());
        let top = ctx
            .instruction
            .get("top")
            .and_then(|v| v.parse::<i32>().ok());
        let hide = ctx
            .instruction
            .get("hide")
            .and_then(|v| v.parse::<i32>().ok());
        let autohide = ctx
            .instruction
            .get("autohide")
            .and_then(|v| v.parse::<u64>().ok());
        Ok(TagResult::Emit(Event::MouseConfig {
            left,
            top,
            hide,
            autohide,
        }))
    }
}

/// [keyconfig] 按键配置
pub struct KeyconfigHandler;

impl TagHandler for KeyconfigHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut config = std::collections::HashMap::new();
        for (key, value) in &ctx.instruction.params {
            config.insert(key.clone(), value.clone());
        }
        Ok(TagResult::Emit(Event::KeyConfig(config)))
    }
}

/// [file] 文件操作
pub struct FileHandler;

impl TagHandler for FileHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let command = ctx.instruction.get("command").unwrap_or("").to_string();
        let src = ctx.instruction.get("src").map(String::from);
        let dst = ctx.instruction.get("dst").map(String::from);
        let target = ctx.instruction.get("target").map(String::from);
        // wasm_sync 专用参数（其余 command 忽略）
        let url = ctx.instruction.get("url").map(String::from);
        let baseurl = ctx.instruction.get("baseurl").map(String::from);
        // list 是 STRING ARRAY：以逗号分隔的文件列表
        let list = ctx.instruction.get("list").map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<String>>()
        });
        Ok(TagResult::Emit(Event::FileOperation {
            command,
            src,
            dst,
            target,
            url,
            baseurl,
            list,
        }))
    }
}

/// 收集 `前缀0/前缀1/...` 形式的编号参数值，按 N 升序返回 `(N, 值)`。
///
/// httpget/httppost 的 header_keyN/header_valueN、keyN/valueN/fileN 都用这种
/// 多组编号约定（文档 httpget.md/httppost.md）。
fn numbered_params(ctx: &ExecutionContext<'_>, prefix: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = ctx
        .instruction
        .params
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix(prefix)
                .and_then(|n| n.parse::<usize>().ok())
                .map(|n| (n, v.clone()))
        })
        .collect();
    out.sort_by_key(|(n, _)| *n);
    out
}

/// 把两组编号参数按相同 N 配对（如 header_keyN + header_valueN）。
fn paired_numbered_params(
    ctx: &ExecutionContext<'_>,
    key_prefix: &str,
    value_prefix: &str,
) -> Vec<(String, String)> {
    let values: std::collections::HashMap<usize, String> =
        numbered_params(ctx, value_prefix).into_iter().collect();
    numbered_params(ctx, key_prefix)
        .into_iter()
        .filter_map(|(n, k)| values.get(&n).map(|v| (k, v.clone())))
        .collect()
}

/// 读取非空可选参数
fn opt_param(ctx: &ExecutionContext<'_>, key: &str) -> Option<String> {
    ctx.instruction
        .get(key)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// [httpget] HTTP GET
pub struct HttpgetHandler;

impl TagHandler for HttpgetHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let url = ctx.resolve_param("url")?.as_string();
        Ok(TagResult::Emit(Event::HttpGet {
            url,
            headers: paired_numbered_params(ctx, "header_key", "header_value"),
            varname_code: opt_param(ctx, "varname_code"),
            varname_data: opt_param(ctx, "varname_data"),
            filename: opt_param(ctx, "filename"),
        }))
    }
}

/// [httppost] HTTP POST
pub struct HttppostHandler;

impl TagHandler for HttppostHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let url = ctx.resolve_param("url")?.as_string();
        // 注意 header_keyN 不以 "key" 开头，不会误配进 POST 数据。
        Ok(TagResult::Emit(Event::HttpPost {
            url,
            headers: paired_numbered_params(ctx, "header_key", "header_value"),
            // keyN 与 valueN 配对为普通 POST 数据；keyN 与 fileN 配对时值为文件路径
            data: paired_numbered_params(ctx, "key", "value"),
            file_data: paired_numbered_params(ctx, "key", "file"),
            varname_code: opt_param(ctx, "varname_code"),
            varname_data: opt_param(ctx, "varname_data"),
            filename: opt_param(ctx, "filename"),
        }))
    }
}

/// [openbrowser] 打开浏览器
pub struct OpenbrowserHandler;

impl TagHandler for OpenbrowserHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let url = ctx.resolve_param("url")?.as_string();
        Ok(TagResult::Emit(Event::OpenBrowser { url }))
    }
}

/// [autosave] 自动存档
pub struct AutosaveHandler;

impl TagHandler for AutosaveHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // allow 保留原值：0=禁用；1=退出/切后台时保存；2=每次输入等待时保存
        let allow = ctx
            .instruction
            .get("allow")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1);
        Ok(TagResult::Emit(Event::AutoSaveConfig { allow }))
    }
}

/// [avoid] 紧急回避
pub struct AvoidHandler;

impl TagHandler for AvoidHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // file=回避图像路径，缺省（None）=禁用紧急回避功能
        let file = ctx
            .instruction
            .get("file")
            .filter(|v| !v.is_empty())
            .map(String::from);
        // windowbutton（仅 Windows）：缺省/0=禁用窗口按钮 / 1=默认操作 / 2=退出回避并执行处理器
        let windowbutton = ctx
            .instruction
            .get("windowbutton")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::AvoidConfig { file, windowbutton }))
    }
}

/// [vibrate] 振动
pub struct VibrateHandler;

impl TagHandler for VibrateHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::Vibrate { time }))
    }
}

/// [statusbar] 状态栏
pub struct StatusbarHandler;

impl TagHandler for StatusbarHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let visible = ctx
            .instruction
            .get("visible")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1)
            != 0;
        Ok(TagResult::Emit(Event::StatusBar { visible }))
    }
}

/// [purchase] 应用内购买（仅 iOS/Android）
pub struct PurchaseHandler;

impl TagHandler for PurchaseHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // 通用：purchase=0 仅获取商品信息 / 缺省或 1 执行购买；varname=结果变量名
        let purchase = ctx
            .instruction
            .get("purchase")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1)
            != 0;
        let varname = ctx
            .instruction
            .get("varname")
            .filter(|v| !v.is_empty())
            .map(String::from);
        // iOS：productid=商品 ID；restore=1 执行恢复流程（purchase 被忽略）
        let productid = ctx
            .instruction
            .get("productid")
            .filter(|v| !v.is_empty())
            .map(String::from);
        let restore = ctx
            .instruction
            .get("restore")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        // Android：key=Google Play 许可密钥；sku=商品 ID；consume=1 执行消耗流程
        let key = ctx
            .instruction
            .get("key")
            .filter(|v| !v.is_empty())
            .map(String::from);
        let sku = ctx
            .instruction
            .get("sku")
            .filter(|v| !v.is_empty())
            .map(String::from);
        let consume = ctx
            .instruction
            .get("consume")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        Ok(TagResult::Emit(Event::Purchase {
            purchase,
            varname,
            productid,
            restore,
            key,
            sku,
            consume,
        }))
    }
}

/// [callnative] 调用原生代码
///
/// module=模块（Windows: DLL 路径；iOS: 类名；Android: JNI 完整类名），
/// method=函数名/选择器名/方法名（WASM 时为直接 eval 的 JS 代码），
/// param=传给函数的字符串，result=存储返回字符串的变量名。
pub struct CallnativeHandler;

impl TagHandler for CallnativeHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let result = ctx
            .instruction
            .get("result")
            .filter(|v| !v.is_empty())
            .map(String::from);
        let module = ctx
            .instruction
            .get("module")
            .filter(|v| !v.is_empty())
            .map(String::from);
        let method = ctx.instruction.get("method").unwrap_or("").to_string();
        let param = ctx.instruction.get("param").map(String::from);
        Ok(TagResult::Emit(Event::CallNative {
            result,
            module,
            method,
            param,
        }))
    }
}

/// 弃用标签的显式空转处理器（slider/uidel 等）
///
/// 文档全文即"保留是为了向后兼容，请不要在将来使用它"，无参数、无定义行为。
/// 注册空转 handler 是为了消除未注册标签走 Event::Custom 回退产生的日志噪音。
pub struct LegacyNoopHandler;

impl TagHandler for LegacyNoopHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Instruction;
    use crate::variable::VariableStore;

    /// 用给定参数执行单个标签处理器，返回 TagResult
    fn exec(handler: &dyn TagHandler, tag: &str, params: &[(&str, &str)]) -> TagResult {
        let lua = mlua::Lua::new();
        let instruction = Instruction {
            tag: tag.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            line: 1,
        };
        let mut variables = VariableStore::new();
        let get_script = |_name: &str| None;
        let mut ctx = ExecutionContext {
            variables: &mut variables,
            lua: &lua,
            current_script: "test",
            current_line: 0,
            instruction: &instruction,
            get_script: &get_script,
        };
        handler.execute(&mut ctx).unwrap()
    }

    #[test]
    fn httpget_passes_headers_and_result_targets() {
        // 文档 httpget.md：header_keyN/header_valueN 可多组；
        // varname_code/varname_data/filename 指定结果去向
        let TagResult::Emit(Event::HttpGet {
            url,
            headers,
            varname_code,
            varname_data,
            filename,
        }) = exec(
            &HttpgetHandler,
            "httpget",
            &[
                ("url", "http://www.ies-net.com/"),
                ("header_key0", "foo"),
                ("header_value0", "bar"),
                ("header_key1", "hoge"),
                ("header_value1", "fuga"),
                ("varname_code", "t.code"),
                ("varname_data", "t.data"),
            ],
        )
        else {
            panic!("httpget 应产出 HttpGet");
        };
        assert_eq!(url, "http://www.ies-net.com/");
        assert_eq!(
            headers,
            vec![
                ("foo".to_string(), "bar".to_string()),
                ("hoge".to_string(), "fuga".to_string())
            ]
        );
        assert_eq!(varname_code.as_deref(), Some("t.code"));
        assert_eq!(varname_data.as_deref(), Some("t.data"));
        assert_eq!(filename, None);
    }

    #[test]
    fn httppost_pairs_data_values_and_files_by_index() {
        // 文档 httppost.md 示例：key0+value0 为普通数据，key1+file1 以文件为值
        let TagResult::Emit(Event::HttpPost {
            url,
            headers,
            data,
            file_data,
            filename,
            ..
        }) = exec(
            &HttppostHandler,
            "httppost",
            &[
                ("url", "http://www.ies-net.com/"),
                ("key0", "foo"),
                ("value0", "bar"),
                ("key1", "hoge"),
                ("file1", "fuga"),
                ("filename", "result.dat"),
            ],
        )
        else {
            panic!("httppost 应产出 HttpPost");
        };
        assert_eq!(url, "http://www.ies-net.com/");
        assert!(headers.is_empty());
        assert_eq!(data, vec![("foo".to_string(), "bar".to_string())]);
        assert_eq!(file_data, vec![("hoge".to_string(), "fuga".to_string())]);
        assert_eq!(filename.as_deref(), Some("result.dat"));
    }

    #[test]
    fn autosave_keeps_raw_allow_value() {
        let TagResult::Emit(Event::AutoSaveConfig { allow }) =
            exec(&AutosaveHandler, "autosave", &[("allow", "2")])
        else {
            panic!("autosave 应产出 AutoSaveConfig");
        };
        assert_eq!(allow, 2, "allow=2（每次输入等待时保存）不得被压成 bool");

        let TagResult::Emit(Event::AutoSaveConfig { allow }) =
            exec(&AutosaveHandler, "autosave", &[("allow", "0")])
        else {
            panic!("autosave 应产出 AutoSaveConfig");
        };
        assert_eq!(allow, 0);
    }

    #[test]
    fn avoid_parses_file_and_windowbutton() {
        let TagResult::Emit(Event::AvoidConfig { file, windowbutton }) = exec(
            &AvoidHandler,
            "avoid",
            &[("file", "sys/boss.png"), ("windowbutton", "2")],
        ) else {
            panic!("avoid 应产出 AvoidConfig");
        };
        assert_eq!(file.as_deref(), Some("sys/boss.png"));
        assert_eq!(windowbutton, 2);

        let TagResult::Emit(Event::AvoidConfig { file, windowbutton }) =
            exec(&AvoidHandler, "avoid", &[])
        else {
            panic!("avoid 应产出 AvoidConfig");
        };
        assert_eq!(file, None, "缺省 file=禁用紧急回避");
        assert_eq!(windowbutton, 0, "缺省禁用窗口按钮");
    }

    #[test]
    fn callnative_parses_documented_params() {
        let TagResult::Emit(Event::CallNative {
            result,
            module,
            method,
            param,
        }) = exec(
            &CallnativeHandler,
            "callnative",
            &[
                ("result", "ret"),
                ("module", "com/ies_net/artemis/Test"),
                ("method", "doWork"),
                ("param", "hello"),
            ],
        )
        else {
            panic!("callnative 应产出 CallNative");
        };
        assert_eq!(result.as_deref(), Some("ret"));
        assert_eq!(module.as_deref(), Some("com/ies_net/artemis/Test"));
        assert_eq!(method, "doWork");
        assert_eq!(param.as_deref(), Some("hello"));
    }

    #[test]
    fn purchase_parses_platform_params() {
        let TagResult::Emit(Event::Purchase {
            purchase,
            varname,
            productid,
            restore,
            key,
            sku,
            consume,
        }) = exec(
            &PurchaseHandler,
            "purchase",
            &[
                ("purchase", "0"),
                ("varname", "result"),
                ("productid", "jp.co.example.item1"),
                ("restore", "1"),
                ("key", "GPKEY"),
                ("sku", "item1"),
                ("consume", "1"),
            ],
        )
        else {
            panic!("purchase 应产出 Purchase");
        };
        assert!(!purchase);
        assert_eq!(varname.as_deref(), Some("result"));
        assert_eq!(productid.as_deref(), Some("jp.co.example.item1"));
        assert!(restore);
        assert_eq!(key.as_deref(), Some("GPKEY"));
        assert_eq!(sku.as_deref(), Some("item1"));
        assert!(consume);

        // 缺省：执行购买、不恢复、不消耗
        let TagResult::Emit(Event::Purchase {
            purchase, restore, consume, ..
        }) = exec(&PurchaseHandler, "purchase", &[])
        else {
            panic!("purchase 应产出 Purchase");
        };
        assert!(purchase);
        assert!(!restore);
        assert!(!consume);
    }

    #[test]
    fn file_parses_wasm_sync_params() {
        let TagResult::Emit(Event::FileOperation {
            command,
            url,
            baseurl,
            list,
            ..
        }) = exec(
            &FileHandler,
            "file",
            &[
                ("command", "wasm_sync"),
                ("url", "https://example.com/list.txt"),
                ("baseurl", "https://example.com/data/"),
                ("list", "a.png, b.png"),
            ],
        )
        else {
            panic!("file 应产出 FileOperation");
        };
        assert_eq!(command, "wasm_sync");
        assert_eq!(url.as_deref(), Some("https://example.com/list.txt"));
        assert_eq!(baseurl.as_deref(), Some("https://example.com/data/"));
        assert_eq!(
            list,
            Some(vec!["a.png".to_string(), "b.png".to_string()])
        );

        let TagResult::Emit(Event::FileOperation {
            command, target, list, ..
        }) = exec(
            &FileHandler,
            "file",
            &[("command", "delete"), ("target", "save/data001.dat")],
        )
        else {
            panic!("file 应产出 FileOperation");
        };
        assert_eq!(command, "delete");
        assert_eq!(target.as_deref(), Some("save/data001.dat"));
        assert_eq!(list, None);
    }

    #[test]
    fn legacy_noop_returns_continue() {
        assert!(matches!(
            exec(&LegacyNoopHandler, "slider", &[]),
            TagResult::Continue
        ));
        assert!(matches!(
            exec(&LegacyNoopHandler, "uidel", &[]),
            TagResult::Continue
        ));
    }
}
