use super::*;
use crate::{CallbackResult, Event, ExecutionResult};

#[test]
fn loading_and_inline_calls_preserve_the_pc_queue_and_host_frame() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.set_callback(|_| CallbackResult::Pause);
    i.load_script(
        "story",
        r#"
*main
[calllua function=read]
[var name=after_call data=1]
[lua]
load_count = (load_count or 0) + 1
assert(__engine:file('load') == 'ready')
function read(e)
    call_count = (call_count or 0) + 1
    local frame = e:getFrameNumber()
    e:enqueueTag{'caption', data='before-read'}
    assert(e:file('dynamic') == 'ok' and frame == e:getFrameNumber())
    assert(frame == 1)
end
[/lua]
"#,
    )
    .unwrap();
    i.start("story", "main").unwrap();
    i.begin_frame();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    let loading_pc = i.current_line();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(i.current_line(), loading_pc);
    assert!(i.load_script("other", "*main\n[return]").is_err());
    assert!(i.get_script("other").is_none());
    assert!(i.start("story", "main").is_err());
    i.resume_host_io(IoResponse::Read(b"ready".to_vec()))
        .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    let call_pc = i.current_line();
    assert!(i.get_variable("after_call").is_none());
    assert_eq!(i.engine_context().lock().unwrap().tag_queue.len(), 1);
    assert!(i.drain_queued_tags_only().unwrap().wait.is_none());
    assert_eq!(i.engine_context().lock().unwrap().tag_queue.len(), 1);
    i.resume_host_io(IoResponse::Read(b"ok".to_vec())).unwrap();
    assert!(
        matches!(i.run().unwrap(), ExecutionResult::Wait(Event::Caption { data }) if data == "before-read")
    );
    i.advance_line();
    assert_eq!(i.current_line(), call_pc);
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
    assert_eq!(
        i.get_variable("after_call")
            .and_then(|value| value.as_int()),
        Some(1)
    );
    for name in ["load_count", "call_count"] {
        assert_eq!(i.lua().globals().get::<i32>(name).unwrap(), 1);
    }
}

#[test]
fn replacing_unstarted_loads_and_resetting_suspended_loads_discard_old_continuations() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.load_script(
        "story",
        "*main\n[lua]\ncount=(count or 0)+1; source='old'\n[/lua]",
    )
    .unwrap();
    i.load_script(
        "story",
        "*main\n[lua]\ncount=(count or 0)+1; source='new'; __engine:file('pending')\n[/lua]",
    )
    .unwrap();
    i.start("story", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(i.lua().globals().get::<i32>("count").unwrap(), 1);
    assert_eq!(i.lua().globals().get::<String>("source").unwrap(), "new");
    i.reset_execution_state();
    assert!(i.pending_host_io().is_none());
    assert!(i.resume_host_io(IoResponse::Read(vec![])).is_err());
    i.start("story", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(i.lua().globals().get::<i32>("count").unwrap(), 2);
    i.resume_host_io(IoResponse::Read(vec![])).unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
}

#[test]
fn external_call_finishes_loading_lua_before_running_the_target_label() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.set_file_loader(Box::new(|name| {
        assert_eq!(name, "child");
        Ok(br#"
*entry
[calllua function=child_function]
[return]
[lua]
child_load_count=(child_load_count or 0)+1
assert(__engine:file('child-load') == 'ready')
function child_function(e)
    child_count=(child_count or 0)+1
    assert(e:file('child-data') == 'ok')
end
[/lua]
"#
        .to_vec())
    }));
    i.load_script(
        "parent",
        "*main\n[call file=child label=entry]\n[var name=after data=1]",
    )
    .unwrap();
    i.start("parent", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "child-load".into()
        })
    );
    assert!(!i.lua().globals().contains_key("child_function").unwrap());
    i.resume_host_io(IoResponse::Read(b"ready".to_vec()))
        .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "child-data".into()
        })
    );
    i.resume_host_io(IoResponse::Read(b"ok".to_vec())).unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
    assert_eq!(
        i.get_variable("after").and_then(|value| value.as_int()),
        Some(1)
    );
    for name in ["child_load_count", "child_count"] {
        assert_eq!(i.lua().globals().get::<i32>(name).unwrap(), 1);
    }
}
