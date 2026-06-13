use art3m1s_core::Project;
use asb_interpreter::{CallbackResult, Event};
use asb_interpreter::event::LayerEvent;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn probe_boot_full() {
    let root = Path::new("/Users/alphaly/lfpm/hamidashi");
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();

    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let jumps: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let jumps_clone = Arc::clone(&jumps);

    it.set_callback(move |e| {
        // 跟踪脚本跳转
        match &e {
            Event::ScriptCall { file, label } => {
                jumps_clone.lock().unwrap().push(format!("Call -> {}:{}", file, label));
            }
            Event::Layer(LayerEvent::Create { id, file }) => {
                events_clone.lock().unwrap().push(e.clone());
                println!("[Layer Create] id={} file={}", id, file);
            }
            Event::Trans { .. } => {
                events_clone.lock().unwrap().push(e.clone());
                println!("[Trans] {:?}", e);
            }
            Event::Wait { .. } => return CallbackResult::Pause,
            _ => {
                events_clone.lock().unwrap().push(e.clone());
            }
        }
        CallbackResult::Continue
    });

    project.start_boot(&mut it).unwrap();

    // 反复 run → Wait → 手动跳过一个指令 → 继续 run，直到完成或达到上限。
    let mut iterations = 0;
    loop {
        iterations += 1;
        let r = it.run();
        println!("iter={} script={:?} line={} result={:?}", iterations, it.current_script(), it.current_line(), r);
        match r {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => {
                // run 返回 Wait 时停在 Wait 指令，下一次 run 还会撞同一行；手动越过。
                it.advance_line();
                if iterations > 20 {
                    println!("--- 超过 20 次 Wait，停止 ---");
                    break;
                }
            }
            Ok(_) => break, // 完成或跳脚本
            Err(e) => {
                println!("--- 错误: {:?} ---", e);
                break;
            }
        }
    }

    println!("\n=== 脚本跳转历史 (共 {}) ===", jumps.lock().unwrap().len());
    for (i, j) in jumps.lock().unwrap().iter().enumerate() {
        println!("#{i} {j}");
    }

    println!("\n=== 全部事件 (共 {}) ===", events.lock().unwrap().len());
    // 只显示前 100 个事件和最后 50 个事件
    let all_events = events.lock().unwrap();
    for (i, e) in all_events.iter().enumerate().take(100) {
        println!("#{i} {e:?}");
    }
    if all_events.len() > 150 {
        println!("\n... (中间省略 {} 个事件) ...\n", all_events.len() - 150);
        for (i, e) in all_events.iter().enumerate().skip(all_events.len() - 50) {
            println!("#{i} {e:?}");
        }
    } else if all_events.len() > 100 {
        for (i, e) in all_events.iter().enumerate().skip(100) {
            println!("#{i} {e:?}");
        }
    }
}
