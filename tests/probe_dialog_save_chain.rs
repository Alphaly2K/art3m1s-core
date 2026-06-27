//! 复现真实存档 UI 链路：
//! `dialog("save")` 显示确认框并停住，YES 点击后应经
//! `dialog_return -> popfunc01 -> dialog_exit -> sv.saveclick -> save.asb`
//! 产出编号 `SaveGame`。

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
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        eprintln!("[lua] {data}");
    }

    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
    fn get_script_status(&self) -> u8 {
        0
    }
    fn is_key_down(&self, _key: u32) -> bool {
        false
    }
    fn is_key_down_edge(&self, _key: u32) -> bool {
        false
    }
    fn is_key_up_edge(&self, _key: u32) -> bool {
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
    fn is_file_exists(&self, path: &str) -> bool {
        art3m1s_core::resolve_project_path(&self.root, path)
            .map(|p| p.exists())
            .unwrap_or(false)
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

fn run_until_stop(it: &mut asb_interpreter::Interpreter, max_steps: usize) {
    for _ in 0..max_steps {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                if matches!(reason, WaitReason::Stop { .. }) {
                    break;
                }
                it.advance_line();
            }
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => {
                it.advance_line();
            }
            Ok(_) => break,
            Err(e) => {
                eprintln!("run_until_stop 错误: {e:?}");
                break;
            }
        }
    }
}

fn drain(it: &mut asb_interpreter::Interpreter, max_steps: usize) {
    for _ in 0..max_steps {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                if matches!(reason, WaitReason::Stop { .. }) {
                    break;
                }
                it.advance_line();
            }
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => {
                it.advance_line();
            }
            Ok(asb_interpreter::ExecutionResult::Completed) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("drain 错误: {e:?}");
                break;
            }
        }
    }
}

#[test]
fn dialog_yes_runs_numbered_save_chain() {
    let root = Path::new("/Users/alphaly/lfpm/loli/root");
    if !root.join("system.ini").exists() {
        eprintln!("跳过：loli 项目不在 {root:?}");
        return;
    }

    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();
    it.set_engine_callbacks(Box::new(ProbeCallbacks {
        root: root.to_path_buf(),
    }));
    it.set_variable(
        "s.savepath",
        asb_interpreter::Value::String("savedata".into()),
    );

    let saves: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let saves_c = Arc::clone(&saves);
    it.set_callback(move |event| {
        if let Event::SaveGame { file } = &event {
            saves_c.lock().unwrap().push(file.clone());
        }
        match &event {
            Event::Wait {
                reason: WaitReason::Stop { .. },
            } => CallbackResult::Pause,
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        }
    });

    project.start_boot(&mut it).unwrap();
    run_until_stop(&mut it, 300);

    let setup = r#"
        sys = sys or {}
        sys.saveslot = sys.saveslot or {}
        flg = flg or {}
        scr = scr or {}
        btn = btn or {}
        ast = ast or {}
        flg.ui = flg.ui or {}
        flg.save = { page = 1, no = 1, p1 = 1 }
        scr.savecom = "save"
        scr.ip = { block = "__probe_block" }
        ast.__probe_block = { lang = "ja", crc = "probe" }
        btn.cursor = "bt_yes"
        function getTextBlockText() return "probe text" end
        __probe_dialog_exit_param = "not-called"
        __probe_saveclick_called = 0
        __probe_yesno_after_param = "not-called"
        __probe_fn_log = {}
        local orig_fn_set = fn.set
        fn.set = function(p)
            table.insert(__probe_fn_log, "set:"..tostring(p))
            return orig_fn_set(p)
        end
        local orig_fn_push = fn.push
        fn.push = function(name, p)
            table.insert(__probe_fn_log, "push:"..tostring(name))
            return orig_fn_push(name, p)
        end
        local orig_yesno_click = yesno_click
        yesno_click = function(e, p)
            local r = orig_yesno_click(e, p)
            __probe_yesno_after_param = tostring(fn.get())
            return r
        end
        local orig_dialog_exit = dialog_exit
        dialog_exit = function(name)
            __probe_dialog_exit_param = tostring(fn.get())
            return orig_dialog_exit(name)
        end
        local orig_saveclick = sv.saveclick
        sv.saveclick = function(...)
            __probe_saveclick_called = __probe_saveclick_called + 1
            return orig_saveclick(...)
        end
        dialog("save")
    "#;
    it.lua().load(setup).exec().expect("dialog setup 失败");
    run_until_stop(&mut it, 120);
    saves.lock().unwrap().clear();

    it.lua()
        .load(r#"btn.cursor = "bt_yes""#)
        .exec()
        .expect("设置 YES cursor 失败");

    {
        let ctx = it.engine_context();
        let mut queue = ctx.lock().unwrap();
        let mut params = HashMap::new();
        params.insert("function".to_string(), "yesno_click".to_string());
        params.insert("key".to_string(), "1".to_string());
        params.insert("type".to_string(), "click".to_string());
        queue.tag_queue.push(("calllua".to_string(), params));
    }

    drain(&mut it, 300);

    let got = saves.lock().unwrap().clone();
    let exit_param: String = it
        .lua()
        .globals()
        .get("__probe_dialog_exit_param")
        .unwrap_or_default();
    let saveclick_called: i64 = it
        .lua()
        .globals()
        .get("__probe_saveclick_called")
        .unwrap_or_default();
    let yesno_after_param: String = it
        .lua()
        .globals()
        .get("__probe_yesno_after_param")
        .unwrap_or_default();
    let fn_log: String = it
        .lua()
        .load(r#"return table.concat(__probe_fn_log or {}, " | ")"#)
        .eval()
        .unwrap_or_default();
    eprintln!("fn log: {fn_log}");
    eprintln!("yesno_click 后 fn.param: {yesno_after_param}");
    eprintln!("dialog_exit fn.param: {exit_param}");
    eprintln!("sv.saveclick called: {saveclick_called}");
    eprintln!("SaveGame files: {got:?}");
    assert!(
        got.iter().any(|f| f == "save0001.dat"),
        "YES 后应继续到 sv.saveclick/save.asb 并产出编号存档，实际: {got:?}"
    );
}
