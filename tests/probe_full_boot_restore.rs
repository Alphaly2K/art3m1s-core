//! 复刻**生产加载顺序**：在 start_boot 之前把真实 saveg.dat 的 g.config/g.system/
//! g.script 种进变量域（正如 runtime.load_project→sysload 所做），跑完整 boot，再读
//! 回 conf/sys/gscr，验证 system_dataloading() 是否真正恢复了它们。
//!
//! probe_syssave_roundtrip 在 boot **之后**才种 g.config 再手动 load_config，绕过了
//! 真实时序；本探针修复这一点，直接命中"重启后 config 没恢复"的报告路径。

use art3m1s_core::Project;
use asb_interpreter::event::WaitReason;
use asb_interpreter::lua_engine::EngineCallbacks;
use asb_interpreter::{CallbackResult, Event, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct ProbeCallbacks {
    root: PathBuf,
}
impl EngineCallbacks for ProbeCallbacks {
    fn debug(&self, _l: i32, data: &str, _r: bool) {
        eprintln!("[lua-debug] {data}");
    }
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
    fn is_file_exists(&self, path: &str) -> bool {
        art3m1s_core::resolve_project_path(&self.root, path)
            .map(|p| p.exists())
            .unwrap_or(false)
    }
    fn file_operation(&self, _c: &str, _p: HashMap<String, String>) {}
    fn include(&self, _p: &str) {}
    fn override_key(&self, _f: u32, _t: u32) {}
    fn set_flick_sensitivity(&self, _s: f64) {}
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

#[test]
fn syssave_restores_through_full_boot() {
    let root = Path::new("/Users/alphaly/lfpm/loli/root");
    if !root.join("system.ini").exists() {
        eprintln!("跳过：loli 不在 {root:?}");
        return;
    }

    // 真实 saveg.dat（runtime sysload 会从沙箱读它）。这里直接拿一份代表性内容，
    // 模拟"上次运行把 bgm 调成 33、language 设成 ja"后落盘的全局/系统域。
    // 形如 runtime.syssave 落的：{ "config": <json>, "system": <json>, "script": <json> }
    // 注意：键是去掉 g. 前缀后的名字（config/system/script），对应 g.config/g.system/g.script。
    let config_json =
        r#"{"bgm":33,"se":77,"voice":100,"language":"ja","mspeed":80,"window":0,"aspeed":50}"#;
    let system_json = r#"{"adv":{"dummy":0},"saveslot":{"lock":{}},"dlg":{"dummy":0}}"#;
    let script_json = r#"{"playtime":12345}"#;

    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();
    it.set_engine_callbacks(Box::new(ProbeCallbacks {
        root: root.to_path_buf(),
    }));

    // ── 复刻 runtime.load_project 的顺序 ──
    // 1) 种 s.savepath
    it.set_variable("s.savepath", Value::String("savedata".into()));
    // 2) sysload：把 saveg.dat 的 config/system/script 种进 g.*（runtime 用键名直接拼 g.<k>）
    it.set_variable("g.config", Value::String(config_json.into()));
    it.set_variable("g.system", Value::String(system_json.into()));
    it.set_variable("g.script", Value::String(script_json.into()));

    // 确认种值生效
    eprintln!(
        "种值后 g.config = {:?}",
        it.get_variable("g.config").map(|v| v.as_string())
    );

    it.set_callback(move |e| match &e {
        Event::Wait {
            reason: WaitReason::Stop { .. },
        } => CallbackResult::Pause,
        Event::Wait { .. } => CallbackResult::Pause,
        _ => CallbackResult::Continue,
    });

    // 3) start_boot —— boot.lua 内的 system_dataloading() 应当 conf=fload_pluto("g.config")
    project.start_boot(&mut it).unwrap();

    // 在 boot 之前给 fload_pluto / config_default 打桩日志，看谁在动 conf。
    let _ = it
        .lua()
        .load(
            r#"
        if not __patched then
            __patched = true
            __flog = {}
            local orig_fload = fload_pluto
            fload_pluto = function(name)
                local r = orig_fload(name)
                if name == "g.config" or name == init.save_config then
                    local bgm = (type(r)=="table") and tostring(r.bgm) or "nontable"
                    table.insert(__flog, "fload_pluto("..tostring(name)..")->bgm="..bgm)
                end
                return r
            end
            if config_default then
                local orig_cd = config_default
                config_default = function(...)
                    table.insert(__flog, "config_default() CALLED conf.bgm_before="..tostring(conf and conf.bgm))
                    return orig_cd(...)
                end
            end
        end
    "#,
        )
        .exec();

    // 跑 boot 直到 title 的 stop
    for i in 0..400 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                if matches!(reason, WaitReason::Stop { .. }) {
                    eprintln!("boot 到达 stop (iter={i}) script={:?}", it.current_script());
                    break;
                }
                it.advance_line();
            }
            Ok(other) => {
                eprintln!("boot iter={i} result={other:?}");
                break;
            }
            Err(e) => {
                eprintln!(
                    "boot iter={i} 错误: {e:?} @ {:?}:{}",
                    it.current_script(),
                    it.current_line()
                );
                break;
            }
        }
    }

    // ── 读回 boot 之后的 conf / sys / gscr ──
    let probe = r#"
        __r = {}
        __r.conf_type = type(conf)
        __r.conf_bgm  = tostring(conf and conf.bgm)
        __r.conf_se   = tostring(conf and conf.se)
        __r.conf_lang = tostring(conf and conf.language)
        __r.sys_type  = type(sys)
        __r.gscr_type = type(gscr)
        __r.gscr_playtime = tostring(gscr and gscr.playtime)
        __r.gconfig_var = tostring(__engine and "n/a")
        __out = ""
        for k,v in pairs(__r) do __out = __out .. k .. "=" .. tostring(v) .. "  " end
    "#;
    if let Err(e) = it.lua().load(probe).exec() {
        eprintln!("读回探针失败: {e:?}");
    }
    let out: String = it.lua().globals().get("__out").unwrap_or_default();
    eprintln!("\n=== boot 后状态 ===\n{out}\n");

    // 也直接看变量域里 g.config 是否还在（boot 可能覆盖）
    eprintln!(
        "boot 后 g.config = {:?}",
        it.get_variable("g.config").map(|v| v.as_string())
    );

    // 单独导出 conf.bgm 到一个普通全局字符串，避免直接引用 mlua 类型。
    let _ = it
        .lua()
        .load(r#"__conf_bgm = tostring(conf and conf.bgm)"#)
        .exec();
    let bgm: String = it.lua().globals().get("__conf_bgm").unwrap_or_default();
    eprintln!("\n>>> 关键断言：conf.bgm 应为 33（上次保存值），实际 = {bgm}");
    assert_eq!(bgm, "33", "conf.bgm 应从 saveg.dat 恢复为 33，实际 {bgm}");
}
