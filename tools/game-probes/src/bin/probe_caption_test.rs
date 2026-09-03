//! 诊断：对真实游戏跑 caption 探测，打印 boot 发出的所有事件，看 caption 是否被发出。
//! 用法：cargo run --features game-probes --bin probe_caption_test -- <project-root>

use art3m1s_core::Project;
use art3m1s_core::script::{CallbackResult, Event};
use std::sync::{Arc, Mutex};

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: probe_caption_test <project-root>");
        std::process::exit(2);
    });
    eprintln!("== 探测游戏: {root}");

    let project = match Project::open(&root, "WINDOWS") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("open project 失败: {e:?}");
            return;
        }
    };
    eprintln!("boot 脚本 = {}", project.config().boot_script);

    let mut interp = project.create_interpreter();
    let caption: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cb = Arc::clone(&caption);
    let count = Arc::new(Mutex::new(0usize));
    let count_cb = Arc::clone(&count);
    interp.set_callback(move |event| {
        let mut n = count_cb.lock().unwrap();
        *n += 1;
        if *n <= 80 {
            eprintln!("  [{:>3}] {:?}", *n, event);
        }
        if let Event::Caption { data } = event {
            eprintln!(">>> 命中 CAPTION: {data:?}");
            *cb.lock().unwrap() = Some(data.clone());
            return CallbackResult::Pause;
        }
        CallbackResult::Continue
    });

    match project.start_boot(&mut interp) {
        Ok(()) => eprintln!("start_boot OK"),
        Err(e) => {
            eprintln!("start_boot 失败: {e:?}");
            return;
        }
    }
    let result = interp.run();
    eprintln!("run() 返回: {result:?}");
    eprintln!("总事件数: {}", *count.lock().unwrap());
    eprintln!("== 最终 CAPTION: {:?}", caption.lock().unwrap());
}
