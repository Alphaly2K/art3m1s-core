//! Hermetic regression tests for stop queue control flow.
//!
//! 回归：脚本因排队 `[stop]` 挂起时，hover/onEnterFrame 这类非控制队列
//! 不能让主脚本继续执行。排入的 call 可以临时进入事件处理器，但 return 后必须
//! 回到原来的 stop；宿主明确释放等待后，主脚本才可继续。

use asb_interpreter::event::WaitReason;
use asb_interpreter::{CallbackResult, Event, ExecutionResult, Interpreter, InterpreterConfig};

fn enqueue(it: &Interpreter, tag: &str, params: &[(&str, &str)]) {
    let ctx = it.engine_context();
    let mut ctx = ctx.lock().unwrap();
    ctx.tag_queue.push((
        tag.to_string(),
        params
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    ));
}

#[test]
fn queued_control_flow_returns_to_the_original_stop() {
    let mut it = Interpreter::new(InterpreterConfig::default());
    it.load_script(
        "test",
        r#"
*main
[call label="open"]
[calllua function="after"]
[return]
*open
[calllua function="queue_stop"]
[return]
*handler
[return]
"#,
    )
    .unwrap();

    it.lua()
        .load(
            r#"
function queue_stop()
    __engine:tag{"stop"}
end
function after()
    after_hit = 1
end
"#,
        )
        .exec()
        .unwrap();

    it.set_callback(|event| match event {
        Event::Wait {
            reason: WaitReason::Stop { .. },
        } => CallbackResult::Pause,
        _ => CallbackResult::Continue,
    });

    it.start("test", "main").unwrap();
    let first = it.run().unwrap();
    assert!(matches!(
        first,
        ExecutionResult::Wait(Event::Wait {
            reason: WaitReason::Stop { .. }
        })
    ));
    let stopped_line = it.current_line();

    enqueue(&it, "debugprint", &[("data", "hover-like non-control tag")]);
    let non_control = it.drain_queued_tags_only().unwrap();
    assert!(!non_control.changed_position);
    assert!(!non_control.saw_return);
    assert!(!non_control.saw_call);
    assert_eq!(it.current_line(), stopped_line);

    enqueue(&it, "call", &[("label", "handler")]);
    let control = it.drain_queued_tags_only().unwrap();
    assert!(control.changed_position);
    assert!(control.saw_call);

    let returned = it.run().unwrap();
    assert!(matches!(
        returned,
        ExecutionResult::Wait(Event::Wait {
            reason: WaitReason::Stop { .. }
        })
    ));
    let after_hit: i64 = it.lua().globals().get("after_hit").unwrap_or_default();
    assert_eq!(after_hit, 0);

    it.advance_line();
    let _ = it.run().unwrap();
    let after_hit: i64 = it.lua().globals().get("after_hit").unwrap_or_default();
    assert_eq!(after_hit, 1);
}
