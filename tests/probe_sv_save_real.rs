//! 驱动**真实** `sv.save()`（save.asb 的 `calllua sv.save`），定位为何编号存档不落盘。
//!
//! 已有的 probe_numbered_save 直接 `enqueueTag{"save",file=...}`，绕过了 sv.save()
//! 函数体；而日志证明实机只发出 `SaveGame file=""`（syssave），编号存档与 savess
//! 缩略图都没出现 → sv.save() 在到达 line 186/233 前就抛错。这里复现真实调用链。

use art3m1s_core::Project;
use asb_interpreter::event::WaitReason;
use asb_interpreter::lua_engine::EngineCallbacks;
use asb_interpreter::{CallbackResult, Event};
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
fn sv_save_real_path() {
    let root = Path::new("/Users/alphaly/lfpm/loli/root");
    if !root.join("system.ini").exists() {
        eprintln!("跳过：loli 不在 {root:?}");
        return;
    }
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();
    it.set_engine_callbacks(Box::new(ProbeCallbacks {
        root: root.to_path_buf(),
    }));

    // 种 s.savepath，仿 runtime
    it.set_variable(
        "s.savepath",
        asb_interpreter::Value::String("savedata".into()),
    );

    // 捕获所有 SaveGame / savess 事件
    let saves: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let savess: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let saves_c = Arc::clone(&saves);
    let savess_c = Arc::clone(&savess);
    it.set_callback(move |e| {
        match &e {
            Event::SaveGame { file } => saves_c.lock().unwrap().push(file.clone()),
            Event::SaveScreenshot { file, .. } => savess_c.lock().unwrap().push(file.clone()),
            _ => {}
        }
        match &e {
            Event::Wait {
                reason: WaitReason::Stop { .. },
            } => CallbackResult::Pause,
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        }
    });

    project.start_boot(&mut it).unwrap();
    for i in 0..200 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                if matches!(reason, WaitReason::Stop { .. }) {
                    eprintln!("boot 到达 stop (iter={i})");
                    break;
                }
                it.advance_line();
            }
            Ok(_) => break,
            Err(e) => {
                eprintln!("boot iter={i} 错误（继续，基础设施可能已就绪）: {e:?}");
                break;
            }
        }
    }

    // 探查关键全局是否就绪
    let diag = r#"
        __d = {}
        __d.sv = type(sv)
        __d.sys = type(sys)
        __d.saveslot = type(sys and sys.saveslot)
        __d.scr = type(scr)
        __d.scr_ip = type(scr and scr.ip)
        __d.csv = type(csv)
        __d.csv_mw = type(csv and csv.mw)
        __d.savethumb = type(csv and csv.mw and csv.mw.savethumb)
        __d.ast = type(ast)
        __d.init = type(init)
        __d.save_prefix = tostring(init and init.save_prefix)
        __d.save_moveno = tostring(init and init.save_moveno)
        __d.game = type(game)
        __d.savemax = tostring(game and game.savemax)
        __d.getTextBlockText = type(getTextBlockText)
        __diag = ""
        for k,v in pairs(__d) do __diag = __diag .. k .. "=" .. tostring(v) .. "  " end
    "#;
    if let Err(e) = it.lua().load(diag).exec() {
        eprintln!("诊断脚本失败: {e:?}");
    }
    let diag_str: String = it.lua().globals().get("__diag").unwrap_or_default();
    eprintln!("\n=== 全局状态 ===\n{diag_str}\n");

    // 先单独探查 makefile 的返回值——这是关键
    let mkf = r#"
        sv = sv or {}
        scr = scr or {}
        -- 不设 scr.savecom，模拟“可能没设”的情况
        __mf_nosavecom = tostring(sv.makefile(1))
        scr.savecom = "save"
        __mf_savecom = tostring(sv.makefile(1))
        __scr_savecom = tostring(scr.savecom)
    "#;
    let _ = it.lua().load(mkf).exec();
    let mf_no: String = it.lua().globals().get("__mf_nosavecom").unwrap_or_default();
    let mf_yes: String = it.lua().globals().get("__mf_savecom").unwrap_or_default();
    eprintln!("=== makefile 探查 ===");
    eprintln!("makefile(1) 无 scr.savecom = {mf_no:?}");
    eprintln!("makefile(1) scr.savecom=save = {mf_yes:?}");

    // 真实调用：完整复刻 save_init() + sv.saveclick() 设置的状态再调 sv.save()
    let call = r#"
        sv = sv or {}
        flg = flg or {}
        scr = scr or {}
        sys = sys or {}
        sys.saveslot = sys.saveslot or {}
        -- save_init() 设的
        flg.save = { page = (sys.saveslot.page or 1), no = 1, p1 = 1 }
        scr.savecom = "save"
        -- sv.saveclick() 设的
        sv.no = flg.save.no
        sv.fl = nil
        __artemis_last_error = nil
        local ok, err = pcall(function() sv.save() end)
        __sv_ok = ok
        __sv_err = tostring(err)
        __sv_trace = tostring(__artemis_last_error)
        __saveslot1 = type(sys and sys.saveslot and sys.saveslot[1])
        __slot1_file = tostring(sys and sys.saveslot and sys.saveslot[1] and sys.saveslot[1].file)
    "#;
    let _ = it.lua().load(call).exec();
    let ok: bool = it.lua().globals().get("__sv_ok").unwrap_or(false);
    let err: String = it.lua().globals().get("__sv_err").unwrap_or_default();
    let slot1: String = it.lua().globals().get("__saveslot1").unwrap_or_default();
    let slot1_file: String = it.lua().globals().get("__slot1_file").unwrap_or_default();

    eprintln!("\n=== sv.save() 结果 ===");
    eprintln!("ok={ok}");
    eprintln!("err={err}");
    eprintln!("sys.saveslot[1] type after = {slot1}");
    eprintln!("sys.saveslot[1].file = {slot1_file:?}");

    // 抽干队列让任何排入的 save/savess 标签走完
    for _ in 0..20 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(_) => break,
            Err(_) => break,
        }
    }

    eprintln!("\n=== 捕获事件 ===");
    eprintln!("SaveGame files: {:?}", saves.lock().unwrap());
    eprintln!("SaveScreenshot files: {:?}", savess.lock().unwrap());

    // ── 真实生产入口：sv.saveclick()（dialog_exit("save") 调它）──────
    // 它 eqtag{"jump", save.asb, label="save"}；save.asb 再 calllua sv.save。
    // 这是从存档界面点击槽位 → 确认后实际走的链路。
    eprintln!("\n=== 测试 sv.saveclick() → save.asb → sv.save 链路 ===");
    saves.lock().unwrap().clear();
    savess.lock().unwrap().clear();
    let setup = r#"
        flg = flg or {}
        sys = sys or {}
        sys.saveslot = sys.saveslot or {}
        flg.save = { page = 1, no = 3, p1 = 1 }
        scr.savecom = "save"
        __artemis_last_error = nil
        local ok, err = pcall(function() sv.saveclick() end)
        __sc_ok = ok
        __sc_err = tostring(err)
        -- 看队列里有没有 jump save.asb
        __q = ""
    "#;
    let _ = it.lua().load(setup).exec();
    let sc_ok: bool = it.lua().globals().get("__sc_ok").unwrap_or(false);
    let sc_err: String = it.lua().globals().get("__sc_err").unwrap_or_default();
    eprintln!("saveclick ok={sc_ok} err={sc_err}");
    {
        let ctx = it.engine_context();
        let q = ctx.lock().unwrap();
        eprintln!("saveclick 后 tag_queue 长度: {}", q.tag_queue.len());
        for (i, (tag, params)) in q.tag_queue.iter().enumerate() {
            eprintln!("  queue[{i}] tag={tag} params={params:?}");
        }
    }
    // 抽干队列让 jump save.asb → calllua sv.save 走完
    for i in 0..60 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(r) => {
                eprintln!(
                    "  drain iter={i} result={r:?} script={:?}",
                    it.current_script()
                );
                break;
            }
            Err(e) => {
                eprintln!(
                    "  drain iter={i} 错误: {e:?} script={:?}:{}",
                    it.current_script(),
                    it.current_line()
                );
                break;
            }
        }
    }
    eprintln!(
        "最终 script={:?}:{}",
        it.current_script(),
        it.current_line()
    );
    eprintln!("saveclick 链路 SaveGame files: {:?}", saves.lock().unwrap());
    eprintln!(
        "saveclick 链路 SaveScreenshot files: {:?}",
        savess.lock().unwrap()
    );
}
