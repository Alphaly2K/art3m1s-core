/// 完整探针：boot → title 画面 → 模拟点击 Start → 进入游戏
///
/// 模拟宿主驱动的完整流程，逐帧推进直到进入主脚本执行（system/script.asb）。
use art3m1s_core::Project;
use asb_interpreter::event::{LayerEvent, WaitReason};
use asb_interpreter::lua_engine::EngineCallbacks;
use asb_interpreter::{CallbackResult, Event};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 用于探针的 EngineCallbacks，提供最精简但功能完整的实现。
struct ProbeCallbacks {
    project_root: PathBuf,
}

impl EngineCallbacks for ProbeCallbacks {
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        eprintln!("[lua-debug] {}", data);
    }
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
    fn is_file_exists(&self, path: &str) -> bool {
        art3m1s_core::resolve_project_path(&self.project_root, path)
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
        vec![]
    }
    fn get_script_wait_reason(&self) -> u8 {
        0
    }
}

#[test]
fn probe_game_entry() {
    let root = std::path::Path::new("/Users/alphaly/lfpm/hamidashi");
    let project = Project::open(root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();

    // 安装 EngineCallbacks，使 e:isFileExists 等能正常工作
    let callbacks = ProbeCallbacks {
        project_root: root.to_path_buf(),
    };
    it.set_engine_callbacks(Box::new(callbacks));

    // ── 阶段 1: boot ──────────────────────────────────────────
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let events_c = Arc::clone(&events);
    let boot_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let boot_done_c = Arc::clone(&boot_done);

    it.set_callback(move |e| {
        events_c.lock().unwrap().push(e.clone());
        match &e {
            Event::Wait {
                reason: WaitReason::Stop { .. },
            } => {
                boot_done_c.store(true, std::sync::atomic::Ordering::SeqCst);
                CallbackResult::Pause
            }
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        }
    });

    project.start_boot(&mut it).unwrap();

    // 走 boot 直到碰到 title 画面的 [stop]（WaitReason::Stop）
    eprintln!("\n=== 阶段 1: 走 boot ===");
    let mut boot_iter = 0;
    loop {
        boot_iter += 1;
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                let stopped = matches!(&reason, WaitReason::Stop { .. });
                eprintln!(
                    "  boot iter={} wait={:?} script={:?}:{}",
                    boot_iter,
                    reason,
                    it.current_script(),
                    it.current_line()
                );

                if stopped || boot_iter > 30 {
                    eprintln!("  => title 画面到着 (boot_iter={})", boot_iter);
                    break;
                }
                // 不是 stop，手动推进（Timed / Generic 等待）
                it.advance_line();
            }
            Ok(other) => {
                eprintln!("  boot iter={} result={:?}", boot_iter, other);
                break;
            }
            Err(e) => {
                eprintln!("  boot iter={} 错误: {:?}", boot_iter, e);
                break;
            }
        }
    }

    // 收集阶段 1 的所有层创建事件
    let phase1 = events.lock().unwrap();
    let layer_count = phase1
        .iter()
        .filter(|e| matches!(e, Event::Layer(LayerEvent::Create { .. })))
        .count();
    let mut ids = Vec::new();
    for e in phase1.iter() {
        if let Event::Layer(LayerEvent::Create { id, .. }) = e {
            ids.push(id.clone());
        }
    }
    eprintln!("  阶段1: {} 层, {} 总事件", layer_count, phase1.len());
    eprintln!("  已创建的层 ID: {:?}", ids);
    drop(phase1);

    // ── 阶段 2: 模拟点击 Start ────────────────────────────────
    eprintln!("\n=== 阶段 2: 模拟点击 Start 并推进 ===");

    // 找到 title_start 函数并调用。在 Artemis 中，Start 按钮点击后会执行：
    //   se_ok() → sysvo(p3) → title_start(p2)
    // 其中 p2 通常是 "gamestart"。我们直接绕过 UI 层命中，调用 title_start。
    {
        let lua = it.lua();
        let globals = lua.globals();

        // 检查 gamestart.ast 是否存在
        let path = "script/gamestart.ast";
        let exists = art3m1s_core::resolve_project_path(root, path)
            .map(|p| p.exists())
            .unwrap_or(false);
        eprintln!("  gamestart.ast 文件存在: {}", exists);

        // 直接调用 title_start("gamestart") 不走 UI 路径是最干净的探针方式
        let result = lua
            .load(
                r#"
            if title_start then
                title_start("gamestart")
            else
                print("[PROBE] title_start 未定义")
            end
        "#,
            )
            .exec();

        match result {
            Ok(_) => eprintln!("  title_start 调用完成"),
            Err(e) => eprintln!("  title_start 调用错误: {:?}", e),
        }

        // title_start 内部用 estag 排入标签；
        // 最后的 title_start2 里会 enqueue {"jump", file="system/script.asb", label="main"}
        // 我们来检查标签队列
        {
            let ctx = it.engine_context();
            let q = ctx.lock().unwrap();
            eprintln!("  tag_queue 长度: {}", q.tag_queue.len());
            for (i, (tag, params)) in q.tag_queue.iter().enumerate() {
                eprintln!("    queue[{}] tag={} params={:?}", i, tag, params);
            }
        }
    }

    // 继续执行：标签队列中的 jump 会自然触发
    let mut game_iter = 0;
    let max_game_iters = 300;
    loop {
        game_iter += 1;
        if game_iter > max_game_iters {
            eprintln!("  => 达到最大迭代次数 {}，停止", max_game_iters);
            break;
        }

        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                eprintln!(
                    "  game iter={} wait={:?} script={:?}:{}",
                    game_iter,
                    reason,
                    it.current_script(),
                    it.current_line()
                );
                it.advance_line();
            }
            Ok(other) => {
                eprintln!(
                    "  game iter={} result={:?} script={:?}:{}",
                    game_iter,
                    other,
                    it.current_script(),
                    it.current_line()
                );
                match other {
                    asb_interpreter::ExecutionResult::Completed => break,
                    _ => {}
                }
            }
            Err(e) => {
                eprintln!("  game iter={} 错误: {:?}", game_iter, e);
                eprintln!(
                    "    script={:?} line={}",
                    it.current_script(),
                    it.current_line()
                );
                break;
            }
        }
    }

    eprintln!("\n=== 最终状态 ===");
    eprintln!("  当前脚本: {:?}", it.current_script());
    eprintln!("  当前行: {}", it.current_line());
}
