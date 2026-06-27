//! 验证编号存档：Lua `e:enqueueTag{"save", file="save0001.dat"}` 是否最终
//! 产出携带正确 file 的 `Event::SaveGame`。复现「编号存档没建文件」。

use art3m1s_core::Project;
use asb_interpreter::{CallbackResult, Event};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn numbered_save_carries_filename() {
    let root = Path::new("/Users/alphaly/lfpm/loli/root");
    if !root.join("system.ini").exists() {
        eprintln!("跳过：loli 项目不在 {root:?}");
        return;
    }
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();

    let saves: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let saves_c = Arc::clone(&saves);
    it.set_callback(move |e| {
        if let Event::SaveGame { file } = &e {
            saves_c.lock().unwrap().push(file.clone());
        }
        match &e {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        }
    });

    project.start_boot(&mut it).unwrap();
    for _ in 0..40 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(_) => break,
            Err(_) => break,
        }
    }

    // 直接模拟 sv.save 末尾那条： tag{"save", file=(file..".dat"), eq=1}
    it.lua()
        .load(r#"e:enqueueTag{"save", file="save0001.dat"}"#)
        .exec()
        .expect("enqueueTag save 失败");

    // 抽干队列让 save 标签走完管线
    for _ in 0..10 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(_) => break,
            Err(e) => {
                eprintln!("flush 出错: {e:?}");
                break;
            }
        }
    }

    let got = saves.lock().unwrap().clone();
    eprintln!("捕获的 SaveGame 事件: {got:?}");
    assert!(
        got.iter().any(|f| f == "save0001.dat"),
        "应捕获到 file=save0001.dat 的 SaveGame 事件，实际: {got:?}"
    );
}
