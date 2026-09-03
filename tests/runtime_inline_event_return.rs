//! Hermetic regression tests for inline event return frames.

use asb_interpreter::{
    CallFrame, CallbackResult, Event, ExecutionResult, Interpreter, InterpreterConfig,
};
use std::collections::HashMap;

#[test]
fn inline_event_helper_return_preserves_waiting_script_position() {
    let mut interpreter = Interpreter::new(InterpreterConfig::default());
    interpreter
        .load_script(
            "scenario",
            r#"
*main
[wait]
*inline_helper
[return]
"#,
        )
        .unwrap();
    interpreter.set_callback(|event| match event {
        Event::Wait { .. } => CallbackResult::Pause,
        _ => CallbackResult::Continue,
    });
    interpreter.start("scenario", "main").unwrap();
    assert!(matches!(
        interpreter.run().unwrap(),
        ExecutionResult::Wait(_)
    ));

    let script = interpreter.current_script().unwrap().to_string();
    let line = interpreter.current_line();
    let mut stack = interpreter.call_stack();
    stack.push(CallFrame {
        script: script.clone(),
        return_line: line,
    });
    interpreter.restore_position(&script, line, stack).unwrap();

    let context = interpreter.engine_context();
    context.lock().unwrap().tag_queue.push((
        "jump".into(),
        HashMap::from([("label".into(), "inline_helper".into())]),
    ));

    assert!(matches!(
        interpreter.run().unwrap(),
        ExecutionResult::Wait(_)
    ));
    assert_eq!(interpreter.current_script(), Some("scenario"));
    assert_eq!(interpreter.current_line(), line);
    assert!(interpreter.call_stack().is_empty());
}
