use super::*;
use crate::{CallbackResult, Event, ExecutionResult};

#[test]
fn restored_variables_keep_current_host_services_and_monotonic_wait_deadlines() {
    let mut i = Interpreter::default();
    i.enable_host_services(7).unwrap();
    i.set_callback(|_| CallbackResult::Pause);
    let snapshot = serde_json::to_vec(&i.variables()).unwrap();
    i.set_host_time(crate::HostTime {
        monotonic_ms: 250,
        utc_unix_ms: -1000,
        utc_offset_minutes: 60,
    })
    .unwrap();
    i.restore_variables(serde_json::from_slice(&snapshot).unwrap());
    i.load_script(
        "wait",
        "*main\n[var system=unixtime name=t.unix]\n[wait time=50]",
    )
    .unwrap();
    i.start("wait", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Wait(_)));
    assert_eq!(i.get_variable("t.unix").and_then(|v| v.as_int()), Some(-1));
    i.lua().load("assert(__engine:now()==250 and __engine:getScriptWaitReason().time==300); assert(os.date('%c')=='1970-01-01 00:59:59')").exec().unwrap();
}

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

#[test]
fn a_pending_external_read_keeps_its_call_frame_and_rejects_wrong_completion_types() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.load_script(
        "parent",
        "*main\n[call file=child label=entry]\n[var name=after data=1]",
    )
    .unwrap();
    i.start("parent", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    let pc = i.current_line();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(i.current_line(), pc);
    assert!(i.resume_host_io(IoResponse::Written).is_err());
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "child".into()
        })
    );
    assert!(i.load_script("child", "*entry\n[return]").is_err());
    i.resume_host_io(IoResponse::Read(b"*entry\n[return]".to_vec()))
        .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
    assert_eq!(i.get_variable("after").and_then(|v| v.as_int()), Some(1));
}

#[test]
fn extensionless_scene_targets_reuse_the_resolved_script() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.load_script(
        "caller.iet",
        "*main\n[call file=episode label=entry]\n[jump file=episode label=again]\n",
    )
    .unwrap();
    i.start("caller.iet", "main").unwrap();
    let missing = || IoResponse::Failed {
        kind: IoFailure::NotFound,
        message: "file not found".into(),
    };
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "episode".into()
        })
    );
    assert_eq!(i.resume_host_io(missing()).unwrap(), IoProgress::Waiting);
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "episode.iet".into()
        })
    );
    i.resume_host_io(IoResponse::Read(
        b"*entry\n[var name=visited data=1]\n[return]\n*again\n[var name=returned data=1]\n[lua]\nload_count=(load_count or 0)+1\n[/lua]\n".to_vec(),
    ))
    .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(i.get_variable("visited").and_then(|v| v.as_int()), Some(1));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "episode".into()
        })
    );
    assert_eq!(i.resume_host_io(missing()).unwrap(), IoProgress::Completed);
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
    assert_eq!(i.current_script(), Some("episode.iet"));
    assert_eq!(i.get_variable("returned").and_then(|v| v.as_int()), Some(1));
    assert_eq!(i.lua().globals().get::<i32>("load_count").unwrap(), 1);
}

#[test]
fn stack_overrides_and_position_restore_load_missing_scripts_without_running_old_queues() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.load_script("parent", "*main\n[calllua function=replace]\n[var name=old data=1]\n[lua]\nfunction replace(e) e:setScriptStack{{file='child',index=0}} end\n[/lua]")
        .unwrap();
    i.start("parent", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "child".into()
        })
    );
    i.restore_position(
        "new",
        0,
        vec![crate::CallFrame {
            script: "restored-parent".into(),
            return_line: 0,
        }],
    )
    .unwrap();
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read { path: "new".into() })
    );
    i.resume_host_io(IoResponse::Read(
        b"[var name=new data=1]\n[return]".to_vec(),
    ))
    .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    assert_eq!(
        i.pending_host_io(),
        Some(&IoRequest::Read {
            path: "restored-parent".into()
        })
    );
    i.resume_host_io(IoResponse::Read(b"[var name=restored data=1]".to_vec()))
        .unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
    assert!(i.get_variable("old").is_none());
    assert_eq!(i.get_variable("new").and_then(|v| v.as_int()), Some(1));
    assert_eq!(i.get_variable("restored").and_then(|v| v.as_int()), Some(1));
}

#[test]
fn a_queued_filter_finishes_its_continuation_before_restoring_an_older_stop() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.set_callback(|_| CallbackResult::Pause);
    i.load_script(
        "story",
        r#"
*main
[calllua function=hold]
[var name=after data=1]
[lua]
function hold(e) e:enqueueTag{'stop'} end
__engine:setTagFilter{custom=function(e,p)
    assert(e:file('probe') == 'ok')
    e:tag{'caption', data='resumed'}
    return 1
end}
[/lua]
"#,
    )
    .unwrap();
    i.start("story", "main").unwrap();
    assert!(matches!(
        i.run().unwrap(),
        ExecutionResult::Wait(Event::Wait { .. })
    ));
    i.lua()
        .load("__engine:enqueueTag{'tag',data='custom'}")
        .exec()
        .unwrap();
    i.drain_queued_tags_only().unwrap();
    assert!(i.pending_host_io().is_some());
    i.resume_host_io(IoResponse::Read(b"ok".to_vec())).unwrap();
    assert!(
        matches!(i.run().unwrap(), ExecutionResult::Wait(Event::Caption { data }) if data == "resumed")
    );
    i.advance_line();
    assert!(matches!(
        i.run().unwrap(),
        ExecutionResult::Wait(Event::Wait { .. })
    ));
    assert!(i.get_variable("after").is_none());
}

#[test]
fn restoring_a_position_discards_the_suspended_filter_and_its_old_side_effects() {
    let mut i = Interpreter::default();
    i.enable_resumable_io().unwrap();
    i.set_callback(|_| CallbackResult::Pause);
    i.load_script(
        "story",
        r#"
*main
[caption data=original]
[lua]
filter_calls = 0
__engine:setTagFilter{caption=function(e,p)
    filter_calls=filter_calls+1
    local invocation=filter_calls
    assert(e:file('probe') == 'ok')
    e:tag{'caption', data=tostring(invocation)}
    return 1
end}
[/lua]
"#,
    )
    .unwrap();
    i.start("story", "main").unwrap();
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    i.restore_position("story", i.current_line(), vec![])
        .unwrap();
    assert!(i.pending_host_io().is_none());
    assert!(i.resume_host_io(IoResponse::Read(b"old".to_vec())).is_err());
    assert!(matches!(i.run().unwrap(), ExecutionResult::HostIoPending));
    i.resume_host_io(IoResponse::Read(b"ok".to_vec())).unwrap();
    assert!(
        matches!(i.run().unwrap(), ExecutionResult::Wait(Event::Caption { data }) if data == "2")
    );
    i.advance_line();
    assert!(matches!(i.run().unwrap(), ExecutionResult::Completed));
}
