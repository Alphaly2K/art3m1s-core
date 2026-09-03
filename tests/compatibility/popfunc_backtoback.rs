//! 验证 popfunc02 的两次 `calllua fn.pop` 是否**背靠背**执行（中间无 stop/wait）。
//!
//! dialog() push{ {dialog_main,name},{dialog_exit,name} } → jump popfunc02。
//! popfunc02 = [calllua fn.pop][calllua fn.pop][return]。
//! 真机靠"dialog_main 显示对话框后挂起、用户点击再恢复到第二个 fn.pop"才能让
//! dialog_exit 拿到点击结果（fn.param=1）。若我们的引擎把两个 calllua 背靠背同步
//! 执行，dialog_exit 会在用户点击**之前**就拿 fn.param=nil 跑掉 → sw["save"]()
//! 永不触发 → 编号存档/配置应用都失效。本探针隔离验证这一点。

use art3m1s_core::Project;
use asb_interpreter::event::WaitReason;
use asb_interpreter::{CallbackResult, Event};

#[test]
#[ignore = "requires the external loli fixture"]
fn popfunc_runs_both_pops_back_to_back() {
    let root = crate::common::project_fixture("loli");
    let project = Project::open(&root, "WINDOWS").unwrap();
    let mut it = project.create_interpreter();
    it.set_engine_callbacks(Box::new(crate::common::ProjectCallbacks::new(root)));
    it.set_callback(move |e| match &e {
        Event::Wait {
            reason: WaitReason::Stop { .. },
        } => CallbackResult::Pause,
        Event::Wait { .. } => CallbackResult::Pause,
        _ => CallbackResult::Continue,
    });

    project.start_boot(&mut it).unwrap();
    // 推进到 boot 完成（infra 就绪：fn/script.asb 已加载）
    for _ in 0..200 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                if matches!(reason, WaitReason::Stop { .. }) {
                    break;
                }
                it.advance_line();
            }
            Ok(_) => break,
            Err(_) => break,
        }
    }

    // 安装两个记录函数，模拟 {dialog_main, dialog_exit}。
    // __rec 记录调用顺序与每次调用时刻 fn.param 的值。
    // 关键：__main 返回 nil（仿 dialog_main 显示对话框、等待点击的情形），
    //       __exit 读 fn.get() —— 真机应在用户点击后才跑、读到 1；
    //       若背靠背跑，它读到的是 __main 的返回值 nil。
    let setup = r#"
        __rec = {}
        function __main(name)
            table.insert(__rec, "main:param="..tostring(fn.get()))
            -- 模拟显示对话框：注册等待，但 Lua 函数本身同步返回 nil
            return nil
        end
        function __exit(name)
            table.insert(__rec, "exit:param="..tostring(fn.get()))
        end
        -- 仿 dialog(): push 两帧后 jump popfunc02
        fn.push("dlg", { { __main, "save" }, { __exit, "save" } })
    "#;
    it.lua().load(setup).exec().expect("setup 失败");

    // 驱动 run()：抽干队列 + 执行 popfunc02 的两条 calllua fn.pop
    for _ in 0..60 {
        match it.run() {
            Ok(asb_interpreter::ExecutionResult::Wait(_)) => it.advance_line(),
            Ok(_) => break,
            Err(e) => {
                eprintln!("run 错误: {e:?}");
                break;
            }
        }
    }

    let rec: String = it
        .lua()
        .load(r#"return table.concat(__rec, " | ")"#)
        .eval()
        .unwrap_or_default();
    eprintln!("\n=== popfunc 执行记录 ===\n{rec}\n");

    // 真机正确行为：main 跑后应"挂起"，exit 在用户点击设 fn.param=1 后才跑。
    // 若 exit 紧跟 main 背靠背执行且 param=nil，即复现 bug。
    let both_ran = rec.contains("main:") && rec.contains("exit:");
    let exit_saw_nil = rec.contains("exit:param=nil");
    eprintln!("两个 pop 都跑了: {both_ran}");
    eprintln!("exit 读到 fn.param=nil（背靠背，未等点击）: {exit_saw_nil}");
}
