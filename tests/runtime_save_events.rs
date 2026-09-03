//! Hermetic coverage for save events emitted through Lua's queued-tag path.

use asb_interpreter::{CallbackResult, Event, Interpreter, InterpreterConfig};
use std::sync::{Arc, Mutex};

#[test]
fn queued_numbered_save_carries_its_filename() {
    let script = r#"
*main
[lua]
function queue_save(e)
    e:enqueueTag{"save", file="save0001.dat"}
end
[/lua]
[calllua function="queue_save"]
[stop]
"#;

    let mut interpreter = Interpreter::new(InterpreterConfig::default());
    let saves = Arc::new(Mutex::new(Vec::new()));
    let saves_for_callback = Arc::clone(&saves);
    interpreter.set_callback(move |event| {
        if let Event::SaveGame { file } = event {
            saves_for_callback.lock().unwrap().push(file);
        }
        CallbackResult::Continue
    });

    interpreter.load_script("save-test", script).unwrap();
    interpreter.start("save-test", "main").unwrap();
    interpreter.run().unwrap();

    assert_eq!(saves.lock().unwrap().as_slice(), ["save0001.dat"]);
}
