use super::*;
use std::collections::HashMap;

struct Request {
    identity: RequestIdentity,
    metadata: Value,
    blob: Vec<u8>,
}

fn session(handle: u64, limits: TransportLimits, max_file_bytes: usize) -> ScriptIoSession {
    ScriptIoSession::new(handle, InterpreterConfig::default(), limits, max_file_bytes).unwrap()
}

fn requests(session: &mut ScriptIoSession) -> Vec<Request> {
    let mut bytes = vec![0; session.state().control.bytes];
    let result = session.poll_events(&mut bytes);
    assert_eq!(result.written_bytes, bytes.len());
    decode_requests(&bytes)
}

fn decode_requests(bytes: &[u8]) -> Vec<Request> {
    let mut offset = 0;
    let mut requests = vec![];
    while offset < bytes.len() {
        let u32_at = |index| {
            u32::from_le_bytes(
                bytes[offset + index..offset + index + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let u64_at = |index| {
            u64::from_le_bytes(
                bytes[offset + index..offset + index + 8]
                    .try_into()
                    .unwrap(),
            )
        };
        let record_bytes = u32_at(0) as usize;
        if u64_at(40) != 0 {
            let domain = match u16::from_le_bytes(bytes[offset + 6..offset + 8].try_into().unwrap())
            {
                3 => Domain::Io,
                4 => Domain::Ui,
                domain => panic!("unexpected service domain: {domain}"),
            };
            let metadata_bytes = u32_at(56) as usize;
            let blob_start = offset + 64 + metadata_bytes;
            requests.push(Request {
                identity: RequestIdentity {
                    runtime: u64_at(16),
                    epoch: u64_at(24),
                    request_id: u64_at(40),
                    service: MessageKind {
                        domain,
                        kind: u32_at(8),
                    },
                },
                metadata: serde_json::from_slice(&bytes[offset + 64..blob_start]).unwrap(),
                blob: bytes[blob_start..offset + record_bytes].to_vec(),
            });
        }
        offset += record_bytes;
    }
    requests
}

fn reply(request: &Request, metadata: Value, blob: Vec<u8>) -> HostReply {
    HostReply {
        identity: request.identity,
        body: MessageBody::new(metadata.to_string(), blob).unwrap(),
    }
}

fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    png
}

#[derive(Default)]
struct MemoryHost {
    files: HashMap<(String, String), (u64, Vec<u8>)>,
    reads: Vec<(String, usize)>,
    captions: Vec<String>,
    message_layers: Vec<String>,
    modified_times: HashMap<(String, String), i64>,
}

impl MemoryHost {
    fn asset(&mut self, key: &str, bytes: &[u8]) {
        self.files
            .insert(("asset".into(), key.into()), (7, bytes.to_vec()));
    }
    fn complete(&mut self, request: &Request) -> HostReply {
        let namespace = request.metadata["namespace"].as_str().unwrap().to_string();
        let key = request.metadata["key"].as_str().unwrap().to_string();
        let file_key = (namespace, key.clone());
        match request.identity.service {
            IO_STAT => match self.files.get(&file_key) {
                Some((revision, bytes)) => reply(
                    request,
                    json!({"status":"ok","found":true,"size":bytes.len().to_string(),"revision":revision.to_string(),
                        "modified_utc_ms":self.modified_times.get(&file_key).map(|value| value.to_string())}),
                    vec![],
                ),
                None => reply(request, json!({"status":"ok","found":false}), vec![]),
            },
            IO_READ => {
                let (revision, bytes) = self.files.get(&file_key).unwrap();
                let offset = request.metadata["offset"]
                    .as_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let length = request.metadata["length"]
                    .as_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                assert_eq!(request.metadata["revision"], revision.to_string());
                let end = (offset + length.min(1)).min(bytes.len());
                self.reads.push((key, offset));
                reply(
                    request,
                    json!({"status":"ok","revision":revision.to_string(),"eof":end==bytes.len()}),
                    bytes[offset..end].to_vec(),
                )
            }
            IO_WRITE_ATOMIC => {
                let revision = self.files.get(&file_key).map_or(1, |value| value.0 + 1);
                self.files
                    .insert(file_key, (revision, request.blob.clone()));
                reply(request, json!({"status":"ok"}), vec![])
            }
            IO_DELETE => {
                assert_eq!(file_key.0, "save");
                let status = if self.files.remove(&file_key).is_some() {
                    "ok"
                } else {
                    "not_found"
                };
                reply(request, json!({"status":status}), vec![])
            }
            _ => panic!("unexpected service"),
        }
    }
    fn run(&mut self, session: &mut ScriptIoSession) {
        for _ in 0..512 {
            match session.step().unwrap() {
                ScriptIoProgress::Completed => return,
                ScriptIoProgress::Event => {
                    match session.pending_event() {
                        Some(Event::Caption { data }) => self.captions.push(data.clone()),
                        Some(Event::MessageLayerSwitch { id: Some(id), .. }) => {
                            self.message_layers.push(id.clone())
                        }
                        _ => panic!("unexpected scenario event"),
                    }
                    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
                    session.resume_scenario().unwrap();
                }
                _ => {}
            }
            for request in requests(session) {
                session.submit_replies(&[self.complete(&request)]).unwrap();
            }
        }
        panic!("IO script did not finish");
    }
}

#[test]
fn save_deletion_waits_for_confirmation_and_keeps_assets_separate() {
    let mut session = session(1, TransportLimits::default(), 1024);
    session
        .open_scenario(
            br#"
*main
[var name=t.target data=first]
[file command=delete target=$t.target]
[calllua function=remove]
[caption data=after]
[lua]
function remove(e)
    attempts=(attempts or 0)+1
    local _,message,kind=e:file{command='delete',target='save/SECOND'}
    assert(message=='read only' and kind=='denied')
    local _,message,kind=e:file{command='delete',target='../outside'}
    assert(type(message)=='string' and kind=='denied')
    local _,message,kind=e:file{command='delete',target='save/SECOND'}
    assert(message==nil and kind==nil)
end
[/lua]
"#,
            "delete",
            "main",
        )
        .unwrap();
    let mut host = MemoryHost::default();
    host.asset("first", b"asset");
    for key in ["first", "second"] {
        host.files.insert(("save".into(), key.into()), (1, vec![1]));
    }

    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let first = requests(&mut session).pop().unwrap();
    assert_eq!(first.identity.service, IO_DELETE);
    assert_eq!(first.metadata, json!({"namespace":"save","key":"first"}));
    let globals = session.interpreter().lua().globals();
    assert_eq!(globals.get::<Option<i32>>("attempts").unwrap(), None);
    let malformed = reply(&first, json!({"status":"ok"}), vec![1]);
    assert!(matches!(
        session.submit_replies(&[malformed]),
        Err(ProtocolError::Argument(_))
    ));
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    assert!(requests(&mut session).is_empty());
    let ack = host.complete(&first);
    session.submit_replies(&[ack.clone()]).unwrap();
    assert_eq!(globals.get::<Option<i32>>("attempts").unwrap(), None);
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    assert_eq!(session.submit_replies(&[ack]), Err(ProtocolError::Stale));

    let second = requests(&mut session).pop().unwrap();
    assert_eq!(second.identity.service, IO_DELETE);
    assert_eq!(second.metadata, json!({"namespace":"save","key":"second"}));
    session
        .submit_replies(&[reply(
            &second,
            json!({"status":"denied","message":"read only"}),
            vec![],
        )])
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let retry = requests(&mut session);
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].metadata, second.metadata);
    assert_ne!(retry[0].identity, second.identity);
    session.submit_replies(&[host.complete(&retry[0])]).unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["after"]);
    assert_eq!(globals.get::<i32>("attempts").unwrap(), 1);
    assert_eq!(host.files.len(), 1);
    assert!(host.files.contains_key(&("asset".into(), "first".into())));
}

#[test]
fn numbered_save_runs_only_its_own_queue_before_waiting_for_host_commit() {
    for should_write in [false, true] {
        let mut session = session(1, TransportLimits::default(), 4096);
        session
            .open_scenario(
                br#"
*main
[calllua function=enqueue_save]
[caption data=inline]
[lua]
function enqueue_save(e)
    e:enqueueTag{'save', file='Save/Slot'}
    e:enqueueTag{'calllua', ['function']='outside'}
    e:enqueueTag{'caption', data='queued'}
end
function outside(e) e:tag{'var', name='memsave.bad', data='wrong'} end
function capture(e, p)
    captures=(captures or 0)+1
    assert(p.file=='Save/Slot' and e:file('before')=='')
    e:enqueueTag{'calllua', ['function']='capture_queued'}
end
function capture_queued(e)
    assert(e:file('after')=='')
    e:tag{'var', name='memsave.good', data='right'}
end
__engine:setEventHandler{onSave='capture'}
[/lua]
"#,
                "save-scene",
                "main",
            )
            .unwrap();
        let mut host = MemoryHost::default();
        host.asset("before", b"");
        host.asset("after", b"");
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        let line = session.interpreter().current_line();
        assert!(matches!(
            session.pending_event(),
            Some(Event::SaveGame { .. })
        ));
        session.submit_save().unwrap();
        assert_eq!(session.resume_scenario(), Err(ProtocolError::State));
        for key in ["before", "after"] {
            assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
            let request = requests(&mut session).pop().unwrap();
            assert_eq!(request.identity.service, IO_STAT);
            assert_eq!(request.metadata["key"], key);
            assert_eq!(session.interpreter().current_line(), line);
            assert!(session.interpreter().get_variable("memsave.bad").is_none());
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        let request = requests(&mut session).pop().unwrap();
        assert_eq!(request.identity.service, IO_WRITE_ATOMIC);
        assert_eq!(request.metadata, json!({"namespace":"save","key":"slot"}));
        let save: crate::save::SaveData = serde_json::from_slice(&request.blob).unwrap();
        assert_eq!(save.current_script, "save-scene");
        assert_eq!(save.current_line, line);
        assert_eq!(
            save.variables.get("memsave.good").unwrap().as_string(),
            "right"
        );
        assert!(save.variables.get("memsave.bad").is_none());
        assert_eq!(session.take_save_result(), None);
        let (reply, outcome) = if should_write {
            (host.complete(&request), SaveResult::Written)
        } else {
            (
                reply(
                    &request,
                    json!({"status":"denied","message":"offline"}),
                    vec![],
                ),
                SaveResult::Failed {
                    kind: IoFailure::Denied,
                    message: "offline".into(),
                    files_written: 0,
                },
            )
        };
        session.submit_replies(&[reply]).unwrap();
        assert_eq!(session.take_save_result(), None);
        if should_write {
            for key in ["saveg.dat", "system.dat"] {
                assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
                let request = requests(&mut session).pop().unwrap();
                assert_eq!(request.identity.service, IO_WRITE_ATOMIC);
                assert_eq!(request.metadata["key"], key);
                session.submit_replies(&[host.complete(&request)]).unwrap();
            }
        }
        assert_eq!(session.step().unwrap(), ScriptIoProgress::SaveReady);
        assert_eq!(session.resume_scenario(), Err(ProtocolError::State));
        assert_eq!(session.take_save_result(), Some(outcome));
        assert_eq!(session.submit_save(), Err(ProtocolError::State));
        session.resume_scenario().unwrap();
        host.run(&mut session);
        assert_eq!(host.captions, ["queued", "inline"]);
        assert_eq!(
            session
                .interpreter()
                .lua()
                .globals()
                .get::<i32>("captures")
                .unwrap(),
            1
        );
        assert_eq!(
            host.files.contains_key(&("save".into(), "slot".into())),
            should_write
        );
    }
}

#[test]
fn system_save_preserves_the_existing_format_and_reports_partial_commit() {
    let mut session = session(1, TransportLimits::default(), 4096);
    session
        .open_scenario(
            br#"
*main
[var name=g.slot data=2]
[var name=s.bgmvol data=42]
[var name=s.status.internal data=1]
[save]
[caption data=after]
[lua]
function on_save() error('unnamed save cannot invoke onSave') end
__engine:setEventHandler{onSave='on_save'}
[/lua]
"#,
            "system-save",
            "main",
        )
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    assert!(matches!(session.pending_event(), Some(Event::SaveGame { file }) if file.is_empty()));
    session.submit_save().unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let global = requests(&mut session).pop().unwrap();
    assert_eq!(global.identity.service, IO_WRITE_ATOMIC);
    assert_eq!(
        global.metadata,
        json!({"namespace":"save","key":"saveg.dat"})
    );
    let global_values: HashMap<String, asb_interpreter::Value> =
        serde_json::from_slice(&global.blob).unwrap();
    assert_eq!(global_values["slot"].as_int(), Some(2));
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    assert!(requests(&mut session).is_empty());
    let mut host = MemoryHost::default();
    session.submit_replies(&[host.complete(&global)]).unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let system = requests(&mut session).pop().unwrap();
    assert_eq!(
        system.metadata,
        json!({"namespace":"save","key":"system.dat"})
    );
    let system_values: HashMap<String, asb_interpreter::Value> =
        serde_json::from_slice(&system.blob).unwrap();
    assert_eq!(system_values["bgmvol"].as_int(), Some(42));
    assert!(!system_values.contains_key("savepath"));
    assert!(!system_values.contains_key("status.internal"));
    session
        .submit_replies(&[reply(&system, json!({"status":"io_error"}), vec![])])
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::SaveReady);
    assert!(matches!(
        session.take_save_result(),
        Some(SaveResult::Failed {
            kind: IoFailure::IoError,
            files_written: 1,
            ..
        })
    ));
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["after"]);
    assert_eq!(host.files.len(), 1);
}

#[test]
fn read_history_uses_macro_call_sites_and_retries_until_host_persists_it() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    host.asset("boot", b"*main\nOld page\n[page]\n[save]\n[save]\n[save]\n");
    host.asset("macro.iet", b"*page\nNew page\n[return]\n");
    host.files.insert(
        ("save".into(), "aread.dat".into()),
        (1, br#"{"boot":[0,0]}"#.to_vec()),
    );
    session.open_boot("boot").unwrap();
    for _ in 0..512 {
        if session.step().unwrap() == ScriptIoProgress::Event {
            break;
        }
        let request = requests(&mut session).pop().unwrap();
        session.submit_replies(&[host.complete(&request)]).unwrap();
    }
    for (expected, was_read) in [("Old page", 1), ("New page", 0)] {
        for _ in 0..2 {
            assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
            assert!(
                matches!(session.pending_event(), Some(Event::ScenarioText { content, .. }) if content == expected)
            );
            assert_eq!(
                session
                    .interpreter()
                    .get_variable("s.status.alreadyread")
                    .unwrap()
                    .as_int(),
                Some(was_read)
            );
        }
        session.resume_scenario().unwrap();
    }
    for attempt in 0..3 {
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        session.submit_save().unwrap();
        for key in ["saveg.dat", "system.dat"] {
            assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
            let request = requests(&mut session).pop().unwrap();
            assert_eq!(request.metadata["key"], key);
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
        if attempt < 2 {
            assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
            let request = requests(&mut session).pop().unwrap();
            assert_eq!(request.identity.service, IO_WRITE_ATOMIC);
            assert_eq!(
                request.metadata,
                json!({"namespace":"save","key":"aread.dat"})
            );
            let history: Value = serde_json::from_slice(&request.blob).unwrap();
            assert_eq!(history, json!({"boot":[0,1]}));
            let ack = if attempt == 0 {
                reply(&request, json!({"status":"denied"}), vec![])
            } else {
                host.complete(&request)
            };
            session.submit_replies(&[ack]).unwrap();
        }
        assert_eq!(session.step().unwrap(), ScriptIoProgress::SaveReady);
        assert!(requests(&mut session).is_empty());
        assert_eq!(
            session.take_save_result(),
            Some(if attempt == 0 {
                SaveResult::Failed {
                    kind: IoFailure::Denied,
                    message: "denied".into(),
                    files_written: 2,
                }
            } else {
                SaveResult::Written
            })
        );
        session.resume_scenario().unwrap();
    }
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
}

#[test]
fn failed_save_hook_leaves_the_saved_event_position_and_caller_queue_intact() {
    let mut session = session(1, TransportLimits::default(), 4096);
    session
        .open_scenario(
            br#"
*main
[save file=slot]
[caption data=inline]
[return]
*start
[caption data=unreachable]
[lua]
function capture(e)
    assert(e:file('before')=='')
    e:enqueueTag{'jump', label='start'}
end
__engine:setEventHandler{onSave='capture'}
[/lua]
"#,
            "failed-save",
            "main",
        )
        .unwrap();
    let mut host = MemoryHost::default();
    host.asset("before", b"");
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    let line = session.interpreter().current_line();
    session.submit_save().unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let request = requests(&mut session).pop().unwrap();
    session.submit_replies(&[host.complete(&request)]).unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::SaveReady);
    assert_eq!(session.interpreter().current_line(), line);
    assert!(requests(&mut session).is_empty());
    assert!(matches!(session.take_save_result(),
        Some(SaveResult::Failed {kind: IoFailure::IoError, message, files_written: 0})
            if message.contains("onSave tag requires scenario dispatch")));
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["inline"]);
}

#[test]
fn numbered_load_restores_the_target_before_running_its_resumable_hook() {
    let mut session = session(1, TransportLimits::default(), 8192);
    session
        .open_scenario(
            br#"
*main
[calllua function=menu]
[caption data=old-inline]
[lua]
function menu(e)
    e:enqueueTag{'load', file='slot'}
    e:enqueueTag{'caption', data='old-queue'}
end
[/lua]
"#,
            "menu",
            "main",
        )
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    let mut snapshot = crate::save::SaveData::from_interpreter(session.interpreter());
    snapshot.current_script = "target".into();
    snapshot.current_line = 0;
    snapshot.waiting_for_input = Some(true);
    snapshot.variables.set(
        "memsave.status",
        asb_interpreter::Value::String("saved".into()),
    );

    let mut host = MemoryHost::default();
    host.asset(
        "target",
        br#"
*main
[caption data=restored]
[return]
[lua]
target_init=(target_init or 0)+1
assert(__engine:file('boot-data')=='')
function load_slot(e,p)
    loads=(loads or 0)+1
    local status=e:var('memsave.status')
    assert(p.file=='slot' and status=='saved' and e:file('hook-data')=='')
    e:enqueueTag{'var', name='memsave.hook', data=status..'-loaded'}
end
__engine:setEventHandler{onLoad='load_slot'}
[/lua]
"#,
    );
    host.asset("boot-data", b"");
    host.asset("hook-data", b"");
    host.files.insert(
        ("save".into(), "slot".into()),
        (1, serde_json::to_vec(&snapshot).unwrap()),
    );
    session.submit_numbered_load().unwrap();
    assert_eq!(session.resume_scenario(), Err(ProtocolError::State));
    drive_numbered_load(&mut session, &mut host);
    assert_eq!(session.interpreter().current_script(), Some("target"));
    assert!(session.pending_event().is_none());
    assert_eq!(session.step().unwrap(), ScriptIoProgress::LoadReady);
    let globals = session.interpreter().lua().globals();
    assert_eq!(globals.get::<i32>("target_init").unwrap(), 1);
    assert_eq!(globals.get::<i32>("loads").unwrap(), 1);
    assert_eq!(
        session.take_numbered_load_result(),
        Some(NumberedLoadResult::Restored)
    );
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    assert!(matches!(session.pending_event(), Some(Event::Wait { .. })));
    assert_eq!(
        session
            .interpreter()
            .get_variable("memsave.hook")
            .unwrap()
            .as_string(),
        "saved-loaded"
    );
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["restored"]);
}

#[test]
fn invalid_numbered_load_position_keeps_the_old_save_point_and_queue() {
    let mut session = session(1, TransportLimits::default(), 8192);
    session
        .open_scenario(
            br#"
*main
[calllua function=menu]
[caption data=inline]
[lua]
function menu(e)
    e:enqueueTag{'load', file='slot'}
    e:enqueueTag{'caption', data='queued'}
end
[/lua]
"#,
            "menu",
            "main",
        )
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    let line = session.interpreter().current_line();
    let mut save = crate::save::SaveData::from_interpreter(session.interpreter());
    save.current_script = "target".into();
    save.current_line = 1000;
    let mut host = MemoryHost::default();
    host.asset("target", b"*main\n[lua]\ninvalid_init=true\n[/lua]");
    host.files.insert(
        ("save".into(), "slot".into()),
        (1, serde_json::to_vec(&save).unwrap()),
    );
    session.submit_numbered_load().unwrap();
    drive_numbered_load(&mut session, &mut host);
    assert!(
        matches!(session.take_numbered_load_result(), Some(NumberedLoadResult::Failed { kind: IoFailure::IoError, message }) if message.contains("beyond the script"))
    );
    assert_eq!(session.interpreter().current_script(), Some("menu"));
    assert_eq!(session.interpreter().current_line(), line);
    assert!(session.interpreter().get_script("target").is_none());
    assert!(matches!(
        session.pending_event(),
        Some(Event::LoadGame { .. })
    ));
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["queued", "inline"]);
}

fn drive_numbered_load(session: &mut ScriptIoSession, host: &mut MemoryHost) {
    for _ in 0..2048 {
        match session.step().unwrap() {
            ScriptIoProgress::LoadReady => return,
            ScriptIoProgress::Waiting => {
                let request = requests(session).pop().unwrap();
                session.submit_replies(&[host.complete(&request)]).unwrap();
            }
            other => panic!("unexpected load progress: {other:?}"),
        }
    }
    panic!("load did not finish");
}

#[test]
fn external_calls_macros_and_jumps_resume_without_replaying_filters_or_callers() {
    let mut session = session(1, TransportLimits::default(), 1024);
    let mut host = MemoryHost::default();
    host.asset("defs", b"*hello\n[caption data=macro]\n[return]\n");
    host.asset(
        "child",
        b"*entry\n[caption data=child]\n[return]\n[lua]\nloads=(loads or 0)+1; assert(__engine:file('init')=='ok')\n[/lua]",
    );
    host.asset("finish", b"*entry\n[caption data=done]\n");
    host.asset("init", b"ok");
    host.asset("probe", b"ok");
    session
        .open_scenario(
            br#"
*main
[macroadd file=defs]
[hello]
[calllua function=queue]
[tag data="call,file,child,label,entry"]
[call file=child label=entry]
[jump file=finish label=entry]
[caption data=unreachable]
[lua]
function queue(e)
    queues=(queues or 0)+1
    e:enqueueTag{'call',file='child',label='entry'}
    e:enqueueTag{'caption',data='queued-after'}
end
__engine:setTagFilter{call=function(e,p)
    filters=(filters or 0)+1
    assert(e:file('probe')=='ok')
    return 0
end}
[/lua]
"#,
            "main",
            "main",
        )
        .unwrap();
    host.run(&mut session);
    assert_eq!(
        host.captions,
        ["macro", "child", "queued-after", "child", "child", "done"]
    );
    let globals = session.interpreter().lua().globals();
    assert_eq!(globals.get::<i32>("loads").unwrap(), 1);
    assert_eq!(globals.get::<i32>("queues").unwrap(), 1);
    assert_eq!(globals.get::<i32>("filters").unwrap(), 2);
    assert_eq!(
        host.reads
            .iter()
            .filter(|(key, offset)| key == "child" && *offset == 0)
            .count(),
        1
    );
    assert_eq!(session.state().pending_requests, 0);
}

#[test]
fn optional_macro_misses_continue_but_required_scene_failures_stop_the_session() {
    for cancel in [false, true] {
        let mut session = session(1, TransportLimits::default(), 1024);
        session.open_scenario(
            b"*main\n[macroadd file=optional]\n[caption data=continued]\n[call file=required label=entry]\n[caption data=unreachable]",
            "main", "main",
        ).unwrap();
        let mut host = MemoryHost::default();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        let request = requests(&mut session).pop().unwrap();
        assert_eq!(request.metadata["key"], "optional");
        session.submit_replies(&[host.complete(&request)]).unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        assert!(
            matches!(session.pending_event(), Some(Event::Caption { data }) if data == "continued")
        );
        session.resume_scenario().unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        let request = requests(&mut session).pop().unwrap();
        assert_eq!(request.metadata["key"], "required");
        if cancel {
            session.cancel_pending().unwrap();
            assert!(session.submit_replies(&[host.complete(&request)]).is_err());
        } else {
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
        let error = session.step().unwrap_err().to_string();
        assert!(error.contains(if cancel { "cancelled" } else { "not_found" }));
        assert!(session.step().is_err());
        assert!(session.pending_event().is_none());
    }
}

#[test]
fn boot_reads_its_entry_and_optional_text_or_binary_macros_through_the_host() {
    let mut binary_macro = b"ASB\0\0".to_vec();
    binary_macro.extend_from_slice(&3u32.to_le_bytes());
    for (kind, tag, params) in [
        (1u32, "hello", vec![]),
        (0, "caption", vec![("data", "macro")]),
        (0, "return", vec![]),
    ] {
        binary_macro.extend_from_slice(&kind.to_le_bytes());
        let put_string = |bytes: &mut Vec<u8>, text: &str| {
            bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
            bytes.extend_from_slice(text.as_bytes());
            bytes.push(0);
        };
        put_string(&mut binary_macro, tag);
        if kind == 0 {
            binary_macro.extend_from_slice(&0u32.to_le_bytes());
            binary_macro.extend_from_slice(&(params.len() as u32).to_le_bytes());
            for (key, value) in params {
                put_string(&mut binary_macro, key);
                put_string(&mut binary_macro, value);
            }
        }
    }
    for (source, macros) in [
        (
            "*main\n[hello]\n[caption data=boot]",
            Some(binary_macro.as_slice()),
        ),
        (
            "*start\n[hello]\n[caption data=boot]",
            Some(b"*hello\n[caption data=macro]\n[return]".as_slice()),
        ),
        ("*_start\n[caption data=boot]", None),
        ("[caption data=boot]", None),
    ] {
        let mut session = session(1, TransportLimits::default(), 1024);
        let mut host = MemoryHost::default();
        host.asset("boot", source.as_bytes());
        if let Some(bytes) = macros {
            host.asset("macro.iet", bytes);
        }
        session.open_boot("boot").unwrap();
        let before = session.state();
        assert!(session.open_boot("../invalid").is_err());
        assert_eq!(session.state(), before);
        host.run(&mut session);
        assert_eq!(
            host.captions,
            if macros.is_some() {
                vec!["macro", "boot"]
            } else {
                vec!["boot"]
            }
        );
        assert_eq!(
            host.reads
                .iter()
                .filter(|(key, offset)| key == "boot" && *offset == 0)
                .count(),
            1
        );
    }
}

#[test]
fn boot_initialization_sees_persisted_variables_but_not_old_runtime_settings() {
    let mut session = session(1, TransportLimits::default(), 1024);
    let mut host = MemoryHost::default();
    host.asset(
        "boot",
        br#"
*main
[caption data=boot]
[lua]
local e=__engine
assert(e:var('g.profile')=='restored')
assert(e:var('s.bgmvol')=='42')
assert(e:var('s.savepath')=='save')
[/lua]
"#,
    );
    let globals = HashMap::from([("profile", asb_interpreter::Value::String("restored".into()))]);
    let system = HashMap::from([
        ("bgmvol", asb_interpreter::Value::Int(42)),
        (
            "savepath",
            asb_interpreter::Value::String("old-runtime-path".into()),
        ),
    ]);
    for (file, data) in [
        ("saveg.dat", serde_json::to_vec(&globals).unwrap()),
        ("system.dat", serde_json::to_vec(&system).unwrap()),
    ] {
        host.files.insert(("save".into(), file.into()), (1, data));
    }
    session.open_boot("boot").unwrap();
    assert!(session.interpreter().get_variable("g.profile").is_none());
    host.run(&mut session);
    let order: Vec<_> = host
        .reads
        .iter()
        .filter(|(_, offset)| *offset == 0)
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(order, ["saveg.dat", "system.dat", "boot"]);
    assert_eq!(host.captions, ["boot"]);
}

#[test]
fn project_open_applies_tag_ini_and_starts_at_configured_top_entry() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    host.asset("tag.ini", b"[caption]\n0=data\n");
    host.asset("sub/boot.iet", "*main\n[caption data=wrong-main]\n[return]\n*top\n[&linetag allow=\"1\" prefix=\"#\"]\n[var system=os name=g.detected]\n#caption \u{8d77}\u{52d5}\n".as_bytes());
    let ini = b"[WINDOWS]\nWIDTH=1280\nHEIGHT=720\nBOOT=Sub\\BOOT.IET\nCHARSET=UTF-8\nSAVEPATH=Data\\Slots\n";
    session
        .open_project(ini, "windows", Some("switch"))
        .unwrap();
    assert_eq!(session.interpreter().config().stage_width, 1280);
    assert_eq!(session.interpreter().config().stage_height, 720);
    assert_eq!(
        session
            .interpreter()
            .get_variable("s.savepath")
            .unwrap()
            .as_string(),
        "data/slots"
    );
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let request = requests(&mut session).pop().unwrap();
    assert_eq!(
        request.metadata,
        json!({"namespace":"asset","key":"tag.ini"})
    );
    let before = session.state();
    assert!(matches!(
        session.open_project(ini, "no-section", None),
        Err(ScriptIoError::Project(_))
    ));
    assert_eq!(session.state(), before);
    assert!(matches!(
        session.open_project(
            b"[WINDOWS]\nWIDTH=1280\nHEIGHT=720\nBOOT=../bad\n",
            "windows",
            None
        ),
        Err(ScriptIoError::Protocol(ProtocolError::Argument(_)))
    ));
    assert_eq!(session.state(), before);
    session.submit_replies(&[host.complete(&request)]).unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["\u{8d77}\u{52d5}"]);
    assert_eq!(
        session
            .interpreter()
            .get_variable("g.detected")
            .unwrap()
            .as_string(),
        "switch"
    );
}

#[test]
fn runtime_loading_waits_for_boot_lua_before_queuing_project_ready() {
    use crate::host::runtime::{self, RuntimeError, RuntimePhase};

    let limits = TransportLimits {
        control: QueueBudget {
            records: 1,
            bytes: 4096,
        },
        ..TransportLimits::default()
    };
    let handle = runtime::create(42, limits, 4096).unwrap();
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        b"*top\n[stop]\n[lua]\nassert(__engine:file('init')=='ok')\n[/lua]",
    );
    host.asset("init", b"ok");
    let ini = b"[WINDOWS]\nWIDTH=1280\nHEIGHT=720\nBOOT=boot.iet\n";
    let frame = HostFrame {
        id: 9,
        monotonic_ns: 1_000_000,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let mut bytes = [0; 4096];

    let epoch = runtime::open_project(handle, ini, "windows", None).unwrap();
    let state = runtime::get_state(handle).unwrap();
    assert_eq!(state.phase, RuntimePhase::Loading);
    assert_eq!(state.stage_size, Some([1280, 720]));
    assert_eq!(state.host_frame_id, None);
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    let polled = runtime::poll_events(handle, &mut bytes).unwrap();
    assert_eq!(polled.record_count, 1);
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        SESSION_CHANGED.kind
    );

    let mut waited_for_init = false;
    let mut ready = false;
    for _ in 0..128 {
        let progress = runtime::step(handle, frame).unwrap();
        let state = runtime::get_state(handle).unwrap();
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        assert_eq!(state.host_frame_id, Some(frame.id));
        assert_eq!(state.transport.epoch, epoch);
        if state.phase == RuntimePhase::Running {
            assert_eq!(progress, ScriptIoProgress::Event);
            assert!(state.waiting_for_scenario && !state.waiting_for_io);
            assert_eq!(polled.record_count, 1);
            assert_eq!(
                u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                PROJECT_READY.kind
            );
            ready = true;
            break;
        }
        assert_eq!(state.phase, RuntimePhase::Loading);
        assert_eq!(state.transport.pending_requests, 1);
        assert!(state.waiting_for_io && !state.waiting_for_scenario);
        let requests = decode_requests(&bytes[..polled.written_bytes]);
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        waited_for_init |= request.metadata["key"] == "init";
        runtime::submit_replies(handle, &[host.complete(request)]).unwrap();
        assert_eq!(
            runtime::get_state(handle).unwrap().phase,
            RuntimePhase::Loading
        );
    }
    assert!(waited_for_init && ready);

    assert_eq!(
        runtime::open_project(handle, ini, "windows", None).unwrap(),
        epoch + 1
    );
    runtime::poll_events(handle, &mut bytes).unwrap();
    let mut empty_host = MemoryHost::default();
    for _ in 0..8 {
        match runtime::step(handle, frame) {
            Ok(ScriptIoProgress::Waiting) => {
                let polled = runtime::poll_events(handle, &mut bytes).unwrap();
                let request = decode_requests(&bytes[..polled.written_bytes])
                    .pop()
                    .unwrap();
                runtime::submit_replies(handle, &[empty_host.complete(&request)]).unwrap();
            }
            Err(RuntimeError::Session(ScriptIoError::Script(_))) => break,
            result => panic!("unexpected startup result {result:?}"),
        }
    }
    assert_eq!(
        runtime::get_state(handle).unwrap().phase,
        RuntimePhase::Faulted
    );
    assert_eq!(
        runtime::poll_events(handle, &mut bytes).unwrap().status,
        PollStatus::NoData
    );
    runtime::destroy(handle).unwrap();
}

#[test]
fn runtime_delivers_ui_once_and_resumes_dialog_only_after_valid_reply() {
    use crate::host::runtime::{self, RuntimeError};

    let limits = TransportLimits {
        control: QueueBudget {
            records: 1,
            bytes: 4096,
        },
        ..TransportLimits::default()
    };
    let handle = runtime::create(0, limits, 4096).unwrap();
    let ini = b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\nCHARSET=UTF-8\n";
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[var name=t.player data=original]
[caption data=before]
[dialog title=Name message=Input varname=t.accepted textfield=t.player textfieldsize=2]
[caption data="$t.accepted + ':' + t.player"]
[dialog title=Other varname=t.accepted textfield=t.player]
[caption data="$t.accepted + ':' + t.player"]
[dialog title=Pending varname=t.accepted]
"#,
    );
    runtime::open_project(handle, ini, "windows", None).unwrap();

    let poll = || {
        let mut bytes = vec![0; 4096];
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        bytes.truncate(polled.written_bytes);
        bytes
    };
    let assert_record = |bytes: &[u8], kind: MessageKind, metadata: Value| {
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            kind.domain as u16
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            kind.kind
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes[64..]).unwrap(),
            metadata
        );
    };
    assert_record(&poll(), SESSION_CHANGED, json!({}));
    for _ in 0..512 {
        let progress = runtime::step(handle, frame).unwrap();
        let bytes = poll();
        if progress == ScriptIoProgress::WouldBlock {
            assert_record(&bytes, PROJECT_READY, json!({}));
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        let request = decode_requests(&bytes).pop().unwrap();
        runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
    }
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    assert_record(&poll(), UI_CAPTION, json!({"text":"before"}));
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    let request = decode_requests(&poll()).pop().unwrap();
    assert_eq!(request.identity.service, UI_DIALOG);
    assert_eq!(
        request.metadata,
        json!({"title":"Name","message":"Input","allow_cancel":true,
        "initial_text":"original","max_characters":"2"})
    );
    let state = runtime::get_state(handle).unwrap();
    assert!(state.waiting_for_ui && !state.waiting_for_io);
    assert_eq!(state.transport.pending_requests, 1);
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    assert!(poll().is_empty());

    let invalid = reply(&request, json!({"status":"ok","accepted":1}), vec![]);
    assert!(matches!(
        runtime::submit_replies(handle, &[invalid]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Argument(_)
        )))
    ));
    let ok = reply(
        &request,
        json!({"status":"ok","accepted":true,"text":"\u{306f}\u{306a}X"}),
        vec![],
    );
    runtime::submit_replies(handle, std::slice::from_ref(&ok)).unwrap();
    assert!(poll().is_empty());
    assert!(matches!(
        runtime::submit_replies(handle, &[ok]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Stale
        )))
    ));
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    assert_record(&poll(), UI_CAPTION, json!({"text":"1:\u{306f}\u{306a}"}));

    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    let second = decode_requests(&poll()).pop().unwrap();
    assert_ne!(request.identity, second.identity);
    let unsupported = reply(&second, json!({"status":"unsupported"}), vec![]);
    runtime::submit_replies(handle, &[unsupported]).unwrap();
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    assert_record(&poll(), UI_CAPTION, json!({"text":"0:\u{306f}\u{306a}"}));
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    let pending = decode_requests(&poll()).pop().unwrap();
    runtime::open_project(handle, ini, "windows", None).unwrap();
    assert_record(&poll(), SESSION_CHANGED, json!({}));
    assert!(!runtime::get_state(handle).unwrap().waiting_for_ui);
    let late = reply(&pending, json!({"status":"cancelled"}), vec![]);
    assert!(matches!(
        runtime::submit_replies(handle, &[late]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Stale
        )))
    ));
    runtime::destroy(handle).unwrap();
}

#[test]
fn shutdown_waits_for_confirmations_and_reports_partial_persistence() {
    use crate::host::runtime::{self, RuntimeError, RuntimePhase};

    let ini = b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n";
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    for fail_final_write in [false, true] {
        let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
        let mut host = MemoryHost::default();
        host.asset(
            "boot.iet",
            b"*top\n[var name=g.saved data=hello]\n[exit]\n[caption data=unreachable]\n",
        );
        let epoch = runtime::open_project(handle, ini, "windows", None).unwrap();
        let poll = || {
            let mut bytes = vec![0; 4096];
            let result = runtime::poll_events(handle, &mut bytes).unwrap();
            bytes.truncate(result.written_bytes);
            bytes
        };

        for _ in 0..128 {
            match runtime::step(handle, frame).unwrap() {
                ScriptIoProgress::Waiting => {
                    let request = decode_requests(&poll()).pop().unwrap();
                    runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
                }
                ScriptIoProgress::Event => break,
                progress => panic!("unexpected startup progress {progress:?}"),
            }
        }
        assert!(runtime::get_state(handle).unwrap().exit_requested);
        let bytes = poll();
        let ready_size = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            PROJECT_READY.kind
        );
        let exit = &bytes[ready_size..];
        assert_eq!(exit.len(), HEADER_BYTES + 8 + 2);
        assert_eq!(
            u32::from_le_bytes(exit[8..12].try_into().unwrap()),
            RUNTIME_EXIT_REQUESTED.kind
        );
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::Event
        );
        assert!(poll().is_empty());
        assert!(matches!(
            runtime::submit_shutdown(handle, epoch - 1),
            Err(RuntimeError::Session(ScriptIoError::Protocol(
                ProtocolError::Stale
            )))
        ));
        runtime::submit_shutdown(handle, epoch).unwrap();
        assert_eq!(
            runtime::get_state(handle).unwrap().phase,
            RuntimePhase::Closing
        );
        assert!(poll().is_empty());
        assert!(matches!(
            runtime::open_project(handle, ini, "windows", None),
            Err(RuntimeError::Session(ScriptIoError::Protocol(
                ProtocolError::State
            )))
        ));

        for key in ["saveg.dat", "system.dat"] {
            assert_eq!(
                runtime::step(handle, frame).unwrap(),
                ScriptIoProgress::Waiting
            );
            let request = decode_requests(&poll()).pop().unwrap();
            assert_eq!(request.identity.service, IO_WRITE_ATOMIC);
            assert_eq!(request.metadata, json!({"namespace":"save","key":key}));
            assert!(runtime::get_state(handle).unwrap().waiting_for_io);
            let response = if fail_final_write && key == "system.dat" {
                reply(&request, json!({"status":"denied"}), vec![])
            } else {
                host.complete(&request)
            };
            runtime::submit_replies(handle, &[response]).unwrap();
            assert_eq!(
                runtime::get_state(handle).unwrap().phase,
                RuntimePhase::Closing
            );
        }
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::Completed
        );
        let bytes = poll();
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            RUNTIME_CLOSED.kind
        );
        let saved: Value = serde_json::from_slice(&bytes[64..]).unwrap();
        if fail_final_write {
            assert_eq!(
                saved,
                json!({"save_status":"failed","reason":"denied","files_written":1})
            );
        } else {
            assert_eq!(saved, json!({"save_status":"written"}));
        }
        assert_eq!(
            runtime::get_state(handle).unwrap().phase,
            RuntimePhase::Closed
        );
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::Completed
        );
        assert!(poll().is_empty());
        runtime::destroy(handle).unwrap();
    }
}

#[test]
fn timed_wait_uses_elapsed_monotonic_time_and_starts_in_its_own_frame() {
    use crate::host::runtime;

    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        b"*top\n[wait time=10 input=0]\n[caption data=elapsed]\n[wait time=0 input=0]\n[caption data=next]\n",
    );
    let mut frame = HostFrame {
        id: 1,
        monotonic_ns: 900_500,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let poll = || {
        let mut bytes = vec![0; 4096];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        bytes.truncate(result.written_bytes);
        bytes
    };
    for _ in 0..128 {
        match runtime::step(handle, frame).unwrap() {
            ScriptIoProgress::Waiting => {
                let request = decode_requests(&poll()).pop().unwrap();
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
            ScriptIoProgress::Event => break,
            progress => panic!("unexpected startup progress {progress:?}"),
        }
    }
    let ready = poll();
    assert_eq!(
        u32::from_le_bytes(ready[8..12].try_into().unwrap()),
        PROJECT_READY.kind
    );

    frame.id += 1;
    frame.monotonic_ns += 9_999_999;
    frame.utc_unix_ms += 86_400_000;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    assert!(poll().is_empty());

    frame.id += 1;
    frame.monotonic_ns += 1;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    let caption = poll();
    assert_eq!(
        u32::from_le_bytes(caption[8..12].try_into().unwrap()),
        UI_CAPTION.kind
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&caption[64..]).unwrap(),
        json!({"text":"elapsed"})
    );
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    assert!(poll().is_empty());

    frame.id += 1;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Completed
    );
    let caption = poll();
    assert_eq!(
        serde_json::from_slice::<Value>(&caption[64..]).unwrap(),
        json!({"text":"next"})
    );
    runtime::destroy(handle).unwrap();
}

#[test]
fn compound_events_resume_in_order_without_reexecuting_the_tag() {
    use asb_interpreter::event::LayerEvent;

    let mut session = session(1, TransportLimits::default(), 4096);
    session
        .open_scenario(
            br#"
*main
[lyc id=inline width=10 height=20 color=112233]
[tag data="lyc,id,dynamic,width,10,height,20,color,112233"]
[calllua function=queue]
[caption data=done]
[lua]
function queue()
    __engine:enqueueTag{'lyc', id='queued', width='10', height='20', color='112233'}
end
[/lua]
"#,
            "boot.iet",
            "main",
        )
        .unwrap();
    for expected in ["inline", "dynamic", "queued"] {
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        assert!(matches!(session.pending_event(),
            Some(Event::Layer(LayerEvent::Create { id, .. })) if id == expected));
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        assert!(matches!(session.pending_event(),
            Some(Event::Layer(LayerEvent::Create { id, .. })) if id == expected));
        session.resume_scenario().unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        assert!(matches!(session.pending_event(),
            Some(Event::Layer(LayerEvent::SetProperties { id, properties }))
            if id == expected && properties["width"] == "10"));
        session.resume_scenario().unwrap();
    }
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    assert!(matches!(session.pending_event(), Some(Event::Caption { data }) if data == "done"));
    session.resume_scenario().unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
}

#[test]
fn project_applies_compound_layer_events_before_asb_and_lua_queries() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    host.asset("boot.iet", br#"*top
[lyc id=1.80 width=23 height=45 color=112233]
[lyprop id=1.80 left=15]
[var system=get_layer_info id=1.80 name=t.info]
[lyrename id=1.80 to=1.90]
[calllua function=check_renamed]
[lydel id=1.90]
[calllua function=check_deleted]
[caption data=done]
[lua]
local e=__engine
function check_renamed()
  assert(e:var('t.info.width')=='23' and e:var('t.info.height')=='45' and e:var('t.info.left')=='15')
  e:tag{'var', system='get_layer_info', name='t.layers'}
  assert(e:var('t.layers.1.id')=='1.90' and e:var('t.layers.1.width')=='23')
end
function check_deleted()
  e:tag{'var', system='get_layer_info', name='t.remaining'}
  assert(e:var('t.remaining.size')=='1')
end
[/lua]
"#);
    session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    for _ in 0..1024 {
        if session.step().unwrap() == ScriptIoProgress::Event {
            assert!(
                matches!(session.pending_event(), Some(Event::Caption { data }) if data == "done")
            );
            return;
        }
        for request in requests(&mut session) {
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
    }
    panic!("scenario did not reach its final caption");
}

#[test]
fn host_supplies_texture_dimensions_before_layer_creation_and_lua_queries() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    let png = png_bytes(3, 2, &[255; 3 * 2 * 4]);
    host.asset("tiles/hero", &png);
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=1.80 file=tiles/hero mask=tiles/mask]
[var system=get_layer_info id=1.80 name=t.info]
[lyc id=1.90 file=tiles/hero]
[calllua function=check]
[caption data=ready]
[lua]
function check()
  local e=__engine
  assert(e:var('t.info.width')=='3' and e:var('t.info.height')=='2')
  assert(e:var('t.info.mask')=='tiles/mask')
  e:tag{'var', system='get_layer_info', id='1.90', name='t.second'}
  assert(e:var('t.second.width')=='3' and e:var('t.second.height')=='2')
end
[/lua]
"#,
    );
    session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    let mut candidates = Vec::new();
    for _ in 0..1024 {
        match session.step().unwrap() {
            ScriptIoProgress::Event => {
                assert!(
                    matches!(session.pending_event(), Some(Event::Caption { data }) if data == "ready")
                );
                assert_eq!(
                    candidates,
                    [
                        "tiles/hero.png",
                        "tiles/hero",
                        "tiles/mask.png",
                        "tiles/mask"
                    ]
                );
                return;
            }
            progress => assert_eq!(progress, ScriptIoProgress::Waiting),
        }
        for request in requests(&mut session) {
            let key = request.metadata["key"].as_str().unwrap();
            if key.starts_with("tiles/") && request.identity.service == IO_STAT {
                candidates.push(key.to_owned());
            }
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
    }
    panic!("scenario did not reach its final caption");
}

#[test]
fn basic_frames_keep_revisioned_pixels_and_masks_alive_across_reload() {
    use crate::host::{
        frame::SpriteBlend,
        runtime::{self, RuntimeError},
    };

    let mut host = MemoryHost::default();
    let pixels = [17, 28, 204, 191, 1, 2, 3, 4];
    let mask_pixels = [50, 50, 50, 255, 200, 200, 200, 255];
    host.asset("hero.png", &png_bytes(2, 1, &pixels));
    host.asset("mask.png", &png_bytes(2, 1, &mask_pixels));
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=1.80 file=hero mask=mask]
[lyprop id=1.80 left=15 top=9]
[lyc id=0 width=8 height=3 color=80112233]
[flip]
[wait time=1 input=0]
[lyprop id=1.80 left=24]
[lydel id=0]
[stop]
"#,
    );
    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let start_frame = HostFrame {
        id: 10,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let ini = b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n";
    let epoch = runtime::open_project(handle, ini, "windows", None).unwrap();

    let drive = |host: &mut MemoryHost, frame: HostFrame| {
        for _ in 0..512 {
            let progress = runtime::step(handle, frame).unwrap();
            let mut bytes = [0; 4096];
            let polled = runtime::poll_events(handle, &mut bytes).unwrap();
            if progress == ScriptIoProgress::Event {
                return;
            }
            assert_eq!(progress, ScriptIoProgress::Waiting);
            assert!(runtime::acquire_latest(handle, 0).unwrap().is_none());
            for request in decode_requests(&bytes[..polled.written_bytes]) {
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
        }
        panic!("project never reached its frame boundary");
    };
    drive(&mut host, start_frame);
    let first = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(first.session_epoch, epoch);
    assert_eq!(first.host_frame_id, 10);
    assert_eq!(first.stage_size, [320, 200]);
    assert_eq!(first.sprites.len(), 2);
    let solid = &first.sprites[0];
    assert_eq!(solid.layer_id, "0");
    assert_eq!(solid.quad_size, [8.0, 3.0]);
    let sprite = &first.sprites[1];
    assert_eq!(sprite.layer_id, "1.80");
    assert_eq!(sprite.blend, SpriteBlend::Alpha);
    assert_eq!(sprite.transform, [1.0, 0.0, 0.0, 1.0, 15.0, 9.0]);
    let textures: Vec<_> = first.textures().collect();
    assert_eq!(textures[solid.texture].rgba(), [17, 34, 51, 128]);
    assert_eq!(textures[sprite.texture].rgba(), pixels);
    assert_eq!(textures[sprite.mask.unwrap()].rgba(), mask_pixels);

    let packet = first.packet();
    let u16_at = |i| u16::from_le_bytes(packet[i..i + 2].try_into().unwrap());
    let u32_at = |i| u32::from_le_bytes(packet[i..i + 4].try_into().unwrap());
    let u64_at = |i| u64::from_le_bytes(packet[i..i + 8].try_into().unwrap());
    assert_eq!(&packet[..4], b"ARFP");
    assert_eq!(u16_at(4), crate::host::frame::FRAME_PACKET_VERSION);
    assert_eq!(u16_at(6), 56);
    assert_eq!(u32_at(8) as usize, packet.len());
    assert_eq!(u32_at(12), 3);
    assert_eq!(u32_at(16), 2);
    assert_eq!(
        (u64_at(24), u64_at(32), u64_at(40)),
        (epoch, first.render_frame_id, 10)
    );
    assert_eq!((u32_at(48), u32_at(52)), (320, 200));
    let hero_row = 56 + sprite.texture * 24;
    assert_eq!(u64_at(hero_row), textures[sprite.texture].revision);
    assert_eq!(
        (
            u32_at(hero_row + 8),
            u32_at(hero_row + 12),
            u32_at(hero_row + 16)
        ),
        (2, 1, 8)
    );
    assert_eq!(u16_at(hero_row + 20), 1);
    let solid_record = 56 + 3 * 24;
    assert_eq!(u32_at(solid_record + 12), u32::MAX);
    assert_eq!(u32_at(solid_record), 105);
    let hero_record = solid_record + 105;
    assert_eq!(u16_at(hero_record + 4), 1);
    assert_eq!(u32_at(hero_record + 8) as usize, sprite.texture);
    assert_eq!(u32_at(hero_record + 12) as usize, sprite.mask.unwrap());
    assert_eq!(f32::from_bits(u32_at(hero_record + 40)), 15.0);
    assert_eq!(u32_at(hero_record + 20), 4);
    assert_eq!(hero_record + u32_at(hero_record) as usize, packet.len());
    assert_eq!(&packet[hero_record + 104..], b"1.80");
    assert_eq!(
        runtime::get_state(handle).unwrap().latest_render_frame_id,
        Some(first.render_frame_id)
    );

    runtime::step(handle, start_frame).unwrap();
    assert!(
        runtime::acquire_latest(handle, first.render_frame_id)
            .unwrap()
            .is_none()
    );
    let second_time = HostFrame {
        id: 11,
        monotonic_ns: 1_000_000,
        ..start_frame
    };
    runtime::step(handle, second_time).unwrap();
    let second = runtime::acquire_latest(handle, first.render_frame_id)
        .unwrap()
        .unwrap();
    assert_eq!(second.sprites.len(), 1);
    assert_eq!(second.sprites[0].transform[4], 24.0);
    assert!(std::ptr::eq(
        second.textures().nth(second.sprites[0].texture).unwrap(),
        textures[sprite.texture]
    ));
    assert!(matches!(
        runtime::acquire_latest(handle, 0),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::WouldBlock
        )))
    ));
    assert!(matches!(
        runtime::destroy(handle),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::State
        )))
    ));

    let updated_pixels = [0, 1, 2, 255, 10, 20, 30, 255];
    host.asset("hero.png", &png_bytes(2, 1, &updated_pixels));
    assert_eq!(
        runtime::open_project(handle, ini, "windows", None).unwrap(),
        epoch + 1
    );
    assert!(runtime::acquire_latest(handle, 0).unwrap().is_none());
    drive(&mut host, start_frame);
    let after = second.render_frame_id;
    drop(second);
    let reloaded = runtime::acquire_latest(handle, after).unwrap().unwrap();
    assert_eq!(reloaded.session_epoch, epoch + 1);
    assert_eq!(
        reloaded
            .textures()
            .nth(reloaded.sprites[1].texture)
            .unwrap()
            .rgba(),
        updated_pixels
    );
    assert_eq!(textures[sprite.texture].rgba(), pixels);
    assert_eq!(u64_at(24), epoch);
    drop(reloaded);
    drop(first);
    runtime::destroy(handle).unwrap();
}

#[test]
fn sprite_tween_advances_on_host_frames_after_scenario_completion() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    host.asset("boot.iet", b"*top\n[lyc id=1.80 width=8 height=3 color=112233]\n[lytween id=1.80 param=left from=0 to=20 time=10 ease=linear]\n[return]\n");
    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let first_time = HostFrame {
        id: 1,
        monotonic_ns: 900_500,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    for _ in 0..256 {
        let progress = runtime::step(handle, first_time).unwrap();
        let mut bytes = [0; 4096];
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        if progress == ScriptIoProgress::Completed {
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        for request in decode_requests(&bytes[..polled.written_bytes]) {
            runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        }
    }
    let first = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    let mut after = first.render_frame_id;
    assert_eq!(first.sprites[0].transform[4], 0.0);
    drop(first);
    let mut frame = HostFrame {
        id: 2,
        monotonic_ns: first_time.monotonic_ns + 5_000_000,
        ..first_time
    };
    for expected in [10.0, 20.0] {
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::Completed
        );
        let view = runtime::acquire_latest(handle, after).unwrap().unwrap();
        assert_eq!(view.sprites[0].transform[4], expected);
        after = view.render_frame_id;
        drop(view);
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::Completed
        );
        assert!(runtime::acquire_latest(handle, after).unwrap().is_none());
        frame.id += 1;
        frame.monotonic_ns += 5_000_000;
    }
    runtime::step(handle, frame).unwrap();
    assert!(runtime::acquire_latest(handle, after).unwrap().is_none());
    runtime::destroy(handle).unwrap();
}

#[test]
fn tween_callbacks_resume_host_io_once_and_keep_the_scenario_wait() {
    use crate::host::runtime;

    let ini = b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n";
    for (sync, grouped, queued) in [
        (1, false, false),
        (0, false, false),
        (1, true, false),
        (0, false, true),
    ] {
        let prelude = if grouped {
            "[lyc id=2 width=2 height=2 color=334455]\n[tweenset]\n[lytween id=2 param=left from=0 to=8 time=2 sync=1]\n"
        } else {
            ""
        };
        let time = if grouped { 3 } else { 5 };
        let after_tween = if grouped {
            "[/tweenset]\n"
        } else if queued {
            "[calllua function=queue_wait]\n"
        } else if sync == 0 {
            "[wait time=10 input=0]\n"
        } else {
            ""
        };
        let source = format!(
            r#"*top
[lyc id=1 width=2 height=2 color=112233]
{prelude}[lytween id=1 param=left from=0 to=20 time={time} sync={sync} handler=calllua function=finish]
{after_tween}[caption data="$g.finished"]
[stop]
[lua]
function finish()
    local e=__engine
    assert(e:var('g.finished')~='1')
    e:tag{{'var', name='g.finished', data=1}}
    assert(e:file('effect.dat')=='x')
    assert(e:var('g.finished')=='1')
    e:enqueueTag{{'caption', data='callback'}}
end
function queue_wait()
    local e=__engine
    e:enqueueTag{{'lytween', id='1', param='top', from='0', to='20', time='10', sync='1'}}
    e:enqueueTag{{'caption', data='deferred'}}
end
[/lua]
"#
        );
        let mut host = MemoryHost::default();
        host.asset("boot.iet", source.as_bytes());
        host.asset("effect.dat", b"x");
        let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
        runtime::open_project(handle, ini, "windows", None).unwrap();
        let first = HostFrame {
            id: 1,
            monotonic_ns: 0,
            utc_unix_ms: 0,
            utc_offset_minutes: 0,
        };
        let poll = || {
            let mut bytes = vec![0; 4096];
            let result = runtime::poll_events(handle, &mut bytes).unwrap();
            bytes.truncate(result.written_bytes);
            bytes
        };
        let captions = |bytes: Vec<u8>| {
            let mut remaining = bytes.as_slice();
            let mut out = Vec::new();
            while !remaining.is_empty() {
                let len = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
                let (record, rest) = remaining.split_at(len);
                assert_eq!(
                    u16::from_le_bytes(record[6..8].try_into().unwrap()),
                    Domain::Ui as u16
                );
                assert_eq!(
                    u32::from_le_bytes(record[8..12].try_into().unwrap()),
                    UI_CAPTION.kind
                );
                let metadata: Value = serde_json::from_slice(&record[64..]).unwrap();
                out.push(metadata["text"].as_str().unwrap().to_owned());
                remaining = rest;
            }
            out
        };
        for _ in 0..1024 {
            let progress = runtime::step(handle, first).unwrap();
            let bytes = poll();
            if progress == ScriptIoProgress::Event {
                assert_eq!(
                    u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                    PROJECT_READY.kind
                );
                break;
            }
            assert_eq!(progress, ScriptIoProgress::Waiting);
            for request in decode_requests(&bytes) {
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
        }
        if grouped {
            let intermediate = HostFrame {
                id: 2,
                monotonic_ns: 2_000_000,
                ..first
            };
            assert_eq!(
                runtime::step(handle, intermediate).unwrap(),
                ScriptIoProgress::Event
            );
            assert!(poll().is_empty());
        }
        let finished = HostFrame {
            id: 3,
            monotonic_ns: 5_000_000,
            ..first
        };
        for service in [IO_STAT, IO_READ] {
            assert_eq!(
                runtime::step(handle, finished).unwrap(),
                ScriptIoProgress::Waiting
            );
            let request = decode_requests(&poll()).pop().unwrap();
            assert_eq!(request.identity.service, service);
            assert_eq!(request.metadata["key"], "effect.dat");
            assert_eq!(
                runtime::step(handle, finished).unwrap(),
                ScriptIoProgress::Waiting
            );
            assert!(poll().is_empty());
            runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        }
        assert_eq!(
            runtime::step(handle, finished).unwrap(),
            ScriptIoProgress::Event
        );
        let delivered = captions(poll());
        if sync == 0 {
            assert_eq!(delivered, ["callback"]);
            let deadline = HostFrame {
                id: 4,
                monotonic_ns: 10_000_000,
                ..first
            };
            assert_eq!(
                runtime::step(handle, deadline).unwrap(),
                ScriptIoProgress::Event
            );
            if queued {
                assert_eq!(captions(poll()), ["deferred", "1"]);
            } else {
                assert_eq!(captions(poll()), ["1"]);
            }
        } else {
            assert_eq!(delivered, ["callback", "1"]);
        }
        runtime::destroy(handle).unwrap();
    }
}

#[test]
fn runtime_dispatches_save_and_load_with_confirmation_before_the_script_continues() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[save file=../invalid]
[save file=slot]
[calllua function=load_once]
[load file=missing]
[caption data=done]
[stop]
[lua]
function load_once()
    local e=__engine
    if e:var('g.loaded') ~= '1' then
        e:tag{'var', name='g.loaded', data=1}
        e:enqueueTag{'load', file='slot'}
        e:enqueueTag{'caption', data='abandoned'}
    end
end
[/lua]
"#,
    );
    let limits = TransportLimits {
        control: QueueBudget {
            records: 1,
            bytes: 8192,
        },
        ..TransportLimits::default()
    };
    let handle = runtime::create(0, limits, 8192).unwrap();
    runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let mut writes = Vec::new();
    let mut observed = Vec::new();
    let mut blocked = false;
    for _ in 0..4096 {
        let progress = runtime::step(handle, frame).unwrap();
        blocked |= progress == ScriptIoProgress::WouldBlock;
        let mut bytes = [0; 8192];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        let bytes = &bytes[..result.written_bytes];
        let requests = decode_requests(bytes);
        if !bytes.is_empty() && requests.is_empty() {
            let domain = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
            let kind = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            if (domain, kind) == (Domain::Runtime as u16, RUNTIME_PERSISTENCE_RESULT.kind)
                || domain == Domain::Ui as u16
            {
                observed.push(serde_json::from_slice::<Value>(&bytes[64..]).unwrap());
            }
        }
        for request in requests {
            let key = request.metadata["key"].as_str().unwrap();
            let response = if request.identity.service == IO_WRITE_ATOMIC {
                writes.push(key.to_owned());
                if key == "system.dat" {
                    reply(&request, json!({"status":"io_error"}), vec![])
                } else {
                    host.complete(&request)
                }
            } else {
                host.complete(&request)
            };
            runtime::submit_replies(handle, &[response]).unwrap();
        }
        if progress == ScriptIoProgress::Event {
            break;
        }
    }
    assert!(blocked);
    assert_eq!(writes, ["slot", "saveg.dat", "system.dat"]);
    assert_eq!(
        observed,
        [
            json!({"operation":"save", "status":"failed", "reason":"denied", "files_written":0}),
            json!({"operation":"save", "status":"failed", "reason":"io_error", "files_written":2}),
            json!({"operation":"load", "status":"restored"}),
            json!({"operation":"load", "status":"failed", "reason":"not_found"}),
            json!({"text":"done"}),
        ]
    );
    runtime::destroy(handle).unwrap();
}

#[test]
fn numbered_save_restores_scene_assets_and_tween_clock_in_a_fresh_runtime() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    let pixels = [20, 40, 60, 190];
    let mask = [128, 128, 128, 255];
    host.asset("hero.png", &png_bytes(1, 1, &pixels));
    host.asset("mask.png", &png_bytes(1, 1, &mask));
    host.asset(
        "store.iet",
        br#"*top
[lyc id=1.80 file=hero mask=mask]
[lytween id=1.80 param=left from=0 to=20 time=10 ease=linear]
[flip]
[wait time=4 input=0]
[save file=slot]
[stop]
"#,
    );
    host.asset(
        "load.iet",
        br#"*top
[lyc id=99 width=1 height=1 color=112233]
[flip]
[load file=slot]
[stop]
"#,
    );
    let create = |boot: &str| {
        let handle = runtime::create(0, TransportLimits::default(), 8192).unwrap();
        let ini = format!("[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT={boot}\n");
        runtime::open_project(handle, ini.as_bytes(), "windows", None).unwrap();
        handle
    };
    let drive = |handle, frame, host: &mut MemoryHost, cancel_mask| {
        for _ in 0..8192 {
            let progress = runtime::step(handle, frame).unwrap();
            let mut bytes = [0; 8192];
            let polled = runtime::poll_events(handle, &mut bytes).unwrap();
            if progress == ScriptIoProgress::Event {
                return;
            }
            assert_eq!(progress, ScriptIoProgress::Waiting);
            for request in decode_requests(&bytes[..polled.written_bytes]) {
                let response = if cancel_mask && request.metadata["key"] == "mask.png" {
                    reply(&request, json!({"status":"cancelled"}), vec![])
                } else {
                    host.complete(&request)
                };
                runtime::submit_replies(handle, &[response]).unwrap();
            }
        }
        panic!("runtime did not reach its scene boundary");
    };
    let start = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let saver = create("store.iet");
    drive(saver, start, &mut host, false);
    drive(saver, start, &mut host, false);
    let save_time = HostFrame {
        id: 2,
        monotonic_ns: 4_000_000,
        ..start
    };
    drive(saver, save_time, &mut host, false);
    assert!(host.files.contains_key(&("save".into(), "slot".into())));
    runtime::destroy(saver).unwrap();

    for cancel_mask in [true, false] {
        host.reads.clear();
        let loader = create("load.iet");
        drive(loader, start, &mut host, false);
        let original = runtime::acquire_latest(loader, 0).unwrap().unwrap();
        assert_eq!(original.sprites[0].layer_id, "99");
        drive(loader, start, &mut host, cancel_mask);
        assert!(host.reads.contains(&("hero.png".into(), 0)));
        if cancel_mask {
            assert!(
                runtime::acquire_latest(loader, original.render_frame_id)
                    .unwrap()
                    .is_none()
            );
        } else {
            assert!(host.reads.contains(&("mask.png".into(), 0)));
            let restored = runtime::acquire_latest(loader, original.render_frame_id)
                .unwrap()
                .unwrap();
            let sprite = &restored.sprites[0];
            assert_eq!(sprite.layer_id, "1.80");
            assert_eq!(sprite.transform[4], 8.0);
            assert_eq!(restored.texture(sprite.texture).unwrap().rgba(), pixels);
            assert_eq!(restored.texture(sprite.mask.unwrap()).unwrap().rgba(), mask);
            let after = restored.render_frame_id;
            drop(restored);
            let finished = HostFrame {
                id: 2,
                monotonic_ns: 6_000_000,
                ..start
            };
            drive(loader, finished, &mut host, false);
            let frame = runtime::acquire_latest(loader, after).unwrap().unwrap();
            assert_eq!(frame.sprites[0].transform[4], 20.0);
        }
        assert_eq!(original.sprites[0].layer_id, "99");
        drop(original);
        runtime::destroy(loader).unwrap();
    }
}

#[test]
fn animation_frames_use_preloaded_images_and_masks_at_their_host_times() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    let first_pixels = [200, 0, 0, 255];
    let second_pixels = [0, 200, 0, 255];
    let mask_pixels = [96, 96, 96, 255];
    host.asset("first.png", &png_bytes(1, 1, &first_pixels));
    host.asset("second.png", &png_bytes(1, 1, &second_pixels));
    host.asset("mask.png", &png_bytes(1, 1, &mask_pixels));
    host.asset(
        "boot.iet",
        br#"*top
[anime id=1.80 mode=init file=first time=0 loop=0]
[anime id=1.80 mode=add file=second mask=mask time=6 left=5]
[anime id=1.80 mode=end time=12]
[stop]
"#,
    );
    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let mut frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let mut bytes = [0; 4096];
    let mut assets = Vec::new();
    for _ in 0..512 {
        let progress = runtime::step(handle, frame).unwrap();
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        if progress == ScriptIoProgress::Event {
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        for request in decode_requests(&bytes[..polled.written_bytes]) {
            if request.identity.service == IO_STAT
                && let Some(key) = request.metadata["key"]
                    .as_str()
                    .filter(|key| key.ends_with(".png"))
            {
                assets.push(key.to_owned());
            }
            runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        }
    }
    assert_eq!(assets, ["first.png", "second.png", "mask.png"]);
    let first = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(
        first.texture(first.sprites[0].texture).unwrap().rgba(),
        first_pixels
    );
    assert_eq!(first.sprites[0].mask, None);
    let after = first.render_frame_id;
    drop(first);
    frame.id = 2;
    frame.monotonic_ns = 5_000_000;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    assert!(runtime::acquire_latest(handle, after).unwrap().is_none());
    frame.id = 3;
    frame.monotonic_ns = 6_000_000;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    let second = runtime::acquire_latest(handle, after).unwrap().unwrap();
    let sprite = &second.sprites[0];
    assert_eq!(sprite.transform[4], 5.0);
    assert_eq!(
        second.texture(sprite.texture).unwrap().rgba(),
        second_pixels
    );
    assert_eq!(
        second.texture(sprite.mask.unwrap()).unwrap().rgba(),
        mask_pixels
    );
    frame.id = 4;
    frame.monotonic_ns = 12_000_000;
    assert_eq!(
        runtime::step(handle, frame).unwrap(),
        ScriptIoProgress::Event
    );
    assert!(
        runtime::acquire_latest(handle, second.render_frame_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runtime::poll_events(handle, &mut bytes).unwrap().status,
        PollStatus::NoData
    );
    drop(second);
    runtime::destroy(handle).unwrap();
}

#[test]
fn host_calendar_and_monotonic_clocks_remain_distinct_across_io_suspension() {
    let mut session = session(1, TransportLimits::default(), 1024);
    session
        .set_host_time(HostTime {
            monotonic_ms: 1000,
            utc_unix_ms: -1,
            utc_offset_minutes: 480,
        })
        .unwrap();
    session.open_chunk(br#"
local e=__engine
assert(e:now()==1000 and os.clock()==1 and os.time()==-1)
assert(os.date('%Y-%m-%d %H:%M:%S')=='1970-01-01 07:59:59')
assert(os.date('!%Y-%m-%d %H:%M:%S')=='1969-12-31 23:59:59')
assert(os.time(os.date('*t'))==-1)
e:tag{'var', system='date', name='t.date'}
assert(e:var('t.date.hour')=='7')
local before=e:now()
assert(e:file('probe')=='ok')
assert(e:now()-before==100 and os.clock()==1.1 and os.time()==-3601)
assert(os.date('%c')=='1969-12-31 21:59:59')
e:tag{'var', system='unixtime', name='t.unix'}
assert(e:var('t.unix')=='-3601')
e:tag{'var', system='file_update_time', file='probe', name='t.modified', format='yyyy-MM-dd hh:mm:ss'}
assert(e:var('t.modified')=='1969-12-31 22:59:59')
"#, "clock").unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    session
        .set_host_time(HostTime {
            monotonic_ms: 1100,
            utc_unix_ms: -3_600_001,
            utc_offset_minutes: -60,
        })
        .unwrap();
    assert!(
        session
            .set_host_time(HostTime {
                utc_offset_minutes: 1441,
                ..HostTime::default()
            })
            .is_err()
    );
    let mut host = MemoryHost::default();
    host.asset("probe", b"ok");
    host.modified_times
        .insert(("asset".into(), "probe".into()), -1);
    host.run(&mut session);
}

#[test]
fn seeded_random_streams_are_isolated_between_interleaved_vms_and_fresh_scenarios() {
    let mut a = session(1, TransportLimits::default(), 1024);
    let mut b = session(2, TransportLimits::default(), 1024);
    let script = br#"
local e=__engine
values={e:random(),math.random(1,1000000)}
e:tag{'chgmsg'}
ids={e:var('s.current_message_layer')}
assert(e:file('probe')=='ok')
e:tag{'var',system='random',name='t.random',min=-1000,max=1000}
values[3]=tonumber(e:var('t.random'))
values[4]=e:random()
values[5]=math.random(1,1000000)
e:tag{'chgmsg'}
ids[2]=e:var('s.current_message_layer')
e:tag{'var',system='random',name='t.wide'}
values[6]=tonumber(e:var('t.wide'))
"#;
    for session in [&mut a, &mut b] {
        session.set_random_seed(19).unwrap();
        session.open_chunk(script, "random").unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        assert!(session.set_random_seed(23).is_err());
    }
    let mut host = MemoryHost::default();
    host.asset("probe", b"ok");
    host.run(&mut a);
    host.run(&mut b);
    let values = |session: &ScriptIoSession| {
        session
            .interpreter()
            .lua()
            .globals()
            .get::<Vec<f64>>("values")
            .unwrap()
    };
    assert_eq!(values(&a), values(&b));
    for session in [&a, &b] {
        assert_eq!(
            session
                .interpreter()
                .lua()
                .globals()
                .get::<Vec<String>>("ids")
                .unwrap(),
            ["chgmsg_0", "chgmsg_1"]
        );
    }
    let expected = values(&a);
    let scenario = format!(
        "*main\n[lua]\n{}\n[/lua]",
        std::str::from_utf8(script).unwrap()
    );
    a.open_scenario(scenario.as_bytes(), "fresh", "main")
        .unwrap();
    host.run(&mut a);
    assert_eq!(values(&a), expected);
    assert_eq!(host.message_layers, ["chgmsg_0", "chgmsg_1"]);
}

#[test]
fn loading_blocks_inline_calls_and_queued_calls_resume_in_scenario_order() {
    let mut session = session(1, TransportLimits::default(), 1024);
    let mut host = MemoryHost::default();
    host.asset("init", b"ready");
    host.asset("story", b"ok");
    host.asset("queue", b"queued");
    session
        .open_scenario(
            br#"
*main
[calllua function=outer]
[caption data="end"]
[lua]
load_count = (load_count or 0) + 1
local boot = __engine:file('init')
assert(boot == 'ready')
function outer(e)
    call_count = (call_count or 0) + 1
    local keep = 73
    e:enqueueTag{'calllua', ['function']='after', params={payload={value=91}}}
    assert(e:file('story') == 'ok' and keep == 73)
    e:enqueueTag{'caption', data='first'}
end
function after(e, p)
    queued_count = (queued_count or 0) + 1
    assert(p.payload.value == 91)
    local keep = p.payload
    assert(e:file('queue') == 'queued' and keep.value == 91)
    e:enqueueTag{'caption', data='queued'}
end
[/lua]
"#,
            "scenario",
            "main",
        )
        .unwrap();
    assert!(
        !session
            .interpreter()
            .lua()
            .globals()
            .contains_key("load_count")
            .unwrap()
    );
    host.run(&mut session);
    for name in ["load_count", "call_count", "queued_count"] {
        assert_eq!(
            session
                .interpreter()
                .lua()
                .globals()
                .get::<i32>(name)
                .unwrap(),
            1
        );
    }
    assert_eq!(host.captions, ["first", "queued", "end"]);
    assert_eq!(session.state().pending_requests, 0);
}

#[test]
fn frame_retries_resume_loading_and_handler_io_without_replaying_the_tick() {
    let mut session = session(1, TransportLimits::default(), 1024);
    session
        .open_scenario(
            br#"
*main
[caption data=scene]
[return]
[lua]
local e=__engine
assert(e:file('init')=='')
function tick(e)
    ticks=(ticks or 0)+1
    local n, t = e:getFrameNumber(), e:now()
    e:enqueueTag{'caption', data='tick-'..n}
    assert(e:file('frame-'..n)=='')
    assert(e:getFrameNumber()==n and e:now()==t)
    observed_utc = os.time()
end
e:setEventHandler{onEnterFrame='tick'}
[/lua]
"#,
            "framed",
            "main",
        )
        .unwrap();
    let frame = HostFrame {
        id: 10,
        monotonic_ns: 9_000_001,
        utc_unix_ms: 1000,
        utc_offset_minutes: 0,
    };
    let next = HostFrame {
        id: 11,
        monotonic_ns: 12_000_000,
        utc_unix_ms: -1,
        ..frame
    };
    let globals = session.interpreter().lua().globals();
    let mut host = MemoryHost::default();
    for name in ["init", "frame-1", "frame-2"] {
        host.asset(name, b"");
    }
    assert_eq!(
        session.step_frame(frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    let init = requests(&mut session).pop().unwrap();
    assert_eq!(init.metadata["key"], "init");
    assert_eq!(
        session.step_frame(next).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    assert_eq!(
        session.step_frame(frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    assert!(requests(&mut session).is_empty());
    session.submit_replies(&[host.complete(&init)]).unwrap();
    assert_eq!(globals.get::<Option<i32>>("ticks").unwrap(), None);
    assert_eq!(
        session.step_frame(frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    let request = requests(&mut session).pop().unwrap();
    assert_eq!(request.metadata["key"], "frame-1");
    assert_eq!(globals.get::<i32>("ticks").unwrap(), 1);
    assert!(matches!(
        session.step_frame(HostFrame {
            monotonic_ns: frame.monotonic_ns + 1,
            ..frame
        }),
        Err(ScriptIoError::Protocol(ProtocolError::Argument(_)))
    ));
    assert_eq!(
        session.step_frame(next).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    session.submit_replies(&[host.complete(&request)]).unwrap();
    assert_eq!(globals.get::<Option<i32>>("observed_utc").unwrap(), None);
    assert_eq!(
        session
            .step_frame(HostFrame {
                utc_unix_ms: 3000,
                ..frame
            })
            .unwrap(),
        ScriptIoProgress::Event
    );
    assert_eq!(globals.get::<i32>("observed_utc").unwrap(), 3);
    assert!(matches!(session.pending_event(), Some(Event::Caption { data }) if data == "tick-1"));
    session.resume_scenario().unwrap();
    assert_eq!(session.step_frame(frame).unwrap(), ScriptIoProgress::Event);
    assert!(matches!(session.pending_event(), Some(Event::Caption { data }) if data == "scene"));
    assert_eq!(session.step_frame(next).unwrap(), ScriptIoProgress::Waiting);
    assert_eq!(session.resume_scenario(), Err(ProtocolError::State));
    let request = requests(&mut session).pop().unwrap();
    assert_eq!(request.metadata["key"], "frame-2");
    assert!(matches!(
        session.step_frame(frame),
        Err(ScriptIoError::Protocol(ProtocolError::Argument(_)))
    ));
    session.submit_replies(&[host.complete(&request)]).unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    assert_eq!(globals.get::<i32>("observed_utc").unwrap(), -1);
    assert!(matches!(session.pending_event(), Some(Event::Caption { data }) if data == "scene"));
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["tick-2"]);
    assert_eq!(globals.get::<i32>("ticks").unwrap(), 2);
}

#[test]
fn event_filter_holds_dispatch_until_io_completes_and_rejects_script_errors() {
    let mut session = session(1, TransportLimits::default(), 1024);
    session
        .open_scenario(
            br#"
*main
[stop]
[caption data=after]
[lua]
__engine:setEventFilter(function(e,name,p)
    assert(name=='lyevent' and p.id=='dialog')
    attempts=(attempts or 0)+1
    local before=p.decision
    assert(e:file(p.key)=='')
    if before=='error' then error('rejected') end
    return tonumber(before)
end)
[/lua]
"#,
            "event-filter",
            "main",
        )
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
    let globals = session.interpreter().lua().globals();
    let mut host = MemoryHost::default();
    for (index, (decision, response, expected)) in [
        ("1", true, EventFilterDecision::Handled),
        ("0", true, EventFilterDecision::DispatchDefault),
        ("error", true, EventFilterDecision::Rejected),
        ("0", false, EventFilterDecision::Rejected),
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("request-{index}");
        if response {
            host.asset(&key, b"");
        }
        let mut params = HashMap::from([
            ("id".into(), "dialog".into()),
            ("key".into(), key.clone()),
            ("decision".into(), decision.into()),
        ]);
        session.submit_event_filter("lyevent", &params).unwrap();
        params.insert("key".into(), "changed-after-submit".into());
        assert_eq!(
            globals
                .get::<Option<usize>>("attempts")
                .unwrap()
                .unwrap_or(0),
            index
        );
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        let request = requests(&mut session).pop().unwrap();
        assert_eq!(request.metadata["key"], key);
        assert_eq!(
            session.submit_event_filter("lyevent", &params),
            Err(ProtocolError::WouldBlock)
        );
        assert_eq!(session.resume_scenario(), Err(ProtocolError::State));
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
        session.submit_replies(&[host.complete(&request)]).unwrap();
        assert_eq!(session.take_event_filter_decision(), None);
        assert_eq!(session.step().unwrap(), ScriptIoProgress::EventFilterReady);
        assert_eq!(session.step().unwrap(), ScriptIoProgress::EventFilterReady);
        assert_eq!(session.take_event_filter_decision(), Some(expected));
        assert_eq!(session.take_event_filter_decision(), None);
        assert_eq!(globals.get::<usize>("attempts").unwrap(), index + 1);
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
        assert!(matches!(session.pending_event(), Some(Event::Wait { .. })));
    }
    session.resume_scenario().unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["after"]);
}

#[test]
fn scenario_replacement_drops_a_running_event_filter() {
    let mut session = session(1, TransportLimits::default(), 1024);
    session.open_scenario(b"*main\n[stop]\n[lua]\n__engine:setEventFilter(function(e) e:file('old'); return 1 end)\n[/lua]", "old", "main").unwrap();
    session
        .submit_event_filter("lyevent", &HashMap::new())
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Waiting);
    let old = requests(&mut session).pop().unwrap();
    session
        .open_scenario(b"*main\n[stop]", "new", "main")
        .unwrap();
    assert!(matches!(
        session.submit_replies(&[reply(&old, json!({"status":"ok","found":false}), vec![])]),
        Err(ProtocolError::Stale)
    ));
    session
        .submit_event_filter("lyevent", &HashMap::new())
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::EventFilterReady);
    assert_eq!(
        session.take_event_filter_decision(),
        Some(EventFilterDecision::DispatchDefault)
    );
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Event);
}

#[test]
fn scenario_replacement_is_atomic_and_stale_loading_results_cannot_touch_the_new_vm() {
    let mut session = session(1, TransportLimits::default(), 128);
    let old = b"[lua]\nmarker='old'; __engine:file('old')\n[/lua]\n*main\n";
    let new = b"[lua]\nassert(marker==nil); marker='new'; __engine:file('new')\n[/lua]\n*main\n";
    session.open_scenario(old, "old", "main").unwrap();
    session.step().unwrap();
    let request = requests(&mut session).pop().unwrap();
    let before = session.state();
    assert!(session.open_scenario(new, "new", "absent-label").is_err());
    assert_eq!(session.state(), before);
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<String>("marker")
            .unwrap(),
        "old"
    );
    session.open_scenario(new, "new", "main").unwrap();
    assert_eq!(
        session.submit_replies(&[reply(&request, json!({"status":"not_found"}), vec![])]),
        Err(ProtocolError::Stale)
    );
    session.step().unwrap();
    let current = requests(&mut session).pop().unwrap();
    assert!(current.identity.request_id > request.identity.request_id);
    assert!(current.identity.epoch > request.identity.epoch);
    session
        .submit_replies(&[reply(&current, json!({"status":"not_found"}), vec![])])
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<String>("marker")
            .unwrap(),
        "new"
    );
}

#[test]
fn suspended_filters_preserve_pass_consume_and_filter_command_order() {
    let mut session = session(1, TransportLimits::default(), 128);
    let mut host = MemoryHost::default();
    host.asset("filter", b"yes");
    session
        .open_scenario(
            br#"
*main
[caption data=pass]
[caption data=consume]
[custom data=only]
[lua]
filter_calls = 0
local function filter(e, p)
    filter_calls = filter_calls + 1
    local saved = filter_calls
    assert(e:file('filter') == 'yes' and saved == filter_calls)
    e:tag{'caption', data='emitted:' .. p.data}
    return p.data == 'consume' and 1 or 0
end
__engine:setTagFilter{caption=filter, custom=filter}
[/lua]
"#,
            "filter",
            "main",
        )
        .unwrap();
    host.run(&mut session);
    assert_eq!(
        host.captions,
        ["pass", "emitted:pass", "emitted:consume", "emitted:only"]
    );
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("filter_calls")
            .unwrap(),
        3
    );
}

#[test]
fn dynamic_and_queued_filters_resume_the_expanded_tag_without_replaying_its_wrapper() {
    let mut session = session(1, TransportLimits::default(), 128);
    let mut host = MemoryHost::default();
    host.asset("outer", b"ok");
    host.asset("caption", b"ok");
    session
        .open_scenario(
            br#"
*main
[tag data="caption,data,inline"]
[var name=after data=1]
[lua]
tag_calls, caption_calls = 0, 0
__engine:setTagFilter{
    tag = function(e, p)
        tag_calls = tag_calls + 1
        assert(e:file('outer') == 'ok')
        return 0
    end,
    caption = function(e, p)
        caption_calls = caption_calls + 1
        assert(e:file('caption') == 'ok')
        e:tag{'caption', data='emitted:' .. p.data}
        return 0
    end,
}
__engine:enqueueTag{'tag', data='caption,data,queued'}
[/lua]
"#,
            "expanded-filter",
            "main",
        )
        .unwrap();
    host.run(&mut session);
    assert_eq!(
        host.captions,
        ["queued", "emitted:queued", "inline", "emitted:inline"]
    );
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("tag_calls")
            .unwrap(),
        1
    );
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("caption_calls")
            .unwrap(),
        2
    );
    assert_eq!(
        session
            .interpreter()
            .get_variable("after")
            .and_then(|value| value.as_int()),
        Some(1)
    );
}

#[test]
fn a_suspended_elseif_filter_retains_its_jump_arrival_semantics() {
    let mut session = session(1, TransportLimits::default(), 128);
    let mut host = MemoryHost::default();
    host.asset("condition", b"ok");
    session
        .open_scenario(
            br#"
*main
[if estimate=0]
[caption data=wrong-if]
[elseif estimate=1]
[caption data=right]
[else]
[caption data=wrong-else]
[/if]
[lua]
filter_calls = 0
__engine:setTagFilter{['elseif']=function(e, p)
    filter_calls=filter_calls+1
    assert(e:file('condition') == 'ok')
    return 0
end}
[/lua]
"#,
            "elseif-filter",
            "main",
        )
        .unwrap();
    host.run(&mut session);
    assert_eq!(host.captions, ["right"]);
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("filter_calls")
            .unwrap(),
        1
    );
}

#[test]
fn scenario_and_lua_file_queries_wait_for_host_data_and_share_legacy_formatting() {
    let mut session = session(1, TransportLimits::default(), 128);
    let mut host = MemoryHost::default();
    host.asset("hash", b"123456789");
    host.asset("empty", b"");
    host.modified_times
        .insert(("asset".into(), "hash".into()), -1);
    host.files
        .insert(("save".into(), "slot".into()), (1, vec![]));
    session
        .open_scenario(
            br#"
*main
[var system=file_exists name=t.exists file=hash]
[var system=file_crc name=t.crc file=hash zerox=1 caps=1]
[var system=file_update_time name=t.time file=hash format="yyyy-MM-dd hh:mm:ss"]
[var system=file_update_time name=t.no_time file=empty noexist=unknown]
[calllua function=check]
[lua]
function check(e)
    e:tag{'var', system='file_exists', name='t.saved', file='slot', save=1}
    assert(e:var('t.saved') == '1')
    e:tag{'var', system='file_crc', name='t.empty', file='empty'}
    assert(e:var('t.empty') == '00000000')
    e:tag{'var', system='file_exist', name='t.exe', file='game.exe'}
    assert(e:var('t.exe') == '1')
    e:tag{'var', name='t.expected.exe', data='expected'}
    e:tag{'var', system='file_crc', name='t.expected.exe.check', file='game.exe'}
    assert(e:var('t.expected.exe.check') == 'expected')
    e:tag{'var', name='t.path', data='hash'}
    e:tag{'var', system='file_crc', name='t.dynamic', file='$t.path'}
    assert(e:var('t.dynamic') == 'cbf43926')
    e:tag{'var', system='file_update_time', name='t.lua_time', file='hash', format='yyyy/MM/dd'}
    assert(e:var('t.lua_time') == '1969/12/31')
end
[/lua]
"#,
            "file-queries",
            "main",
        )
        .unwrap();
    session.step().unwrap();
    assert!(session.interpreter().get_variable("t.exists").is_none());
    let request = requests(&mut session).pop().unwrap();
    session.submit_replies(&[host.complete(&request)]).unwrap();
    assert!(session.interpreter().get_variable("t.exists").is_none());
    host.run(&mut session);
    assert_eq!(
        session
            .interpreter()
            .get_variable("t.exists")
            .map(|value| value.as_bool()),
        Some(true)
    );
    for (name, expected) in [
        ("t.crc", "0xCBF43926"),
        ("t.time", "1969-12-31 23:59:59"),
        ("t.no_time", "unknown"),
    ] {
        assert_eq!(
            session
                .interpreter()
                .get_variable(name)
                .unwrap()
                .as_string(),
            expected
        );
    }
    assert!(host.reads.iter().all(|(key, _)| key != "empty"));
}

#[test]
fn nested_dynamic_reads_short_chunks_and_save_read_your_writes_use_only_the_host() {
    let mut limits = TransportLimits::default();
    limits.blob_bytes = 2;
    let mut session = session(1, limits, 128);
    let mut host = MemoryHost::default();
    host.asset("lib.lua", b"function dynamic(e, p) return e:file(p) end");
    host.asset("data1", &[0, 255, 128, 65, 66]);
    host.asset("empty", &[]);
    session
        .open_chunk(
            br#"
        count = (count or 0) + 1
        local keep = 99
        __engine:include('lib.lua')
        local data = dynamic(__engine, 'Data' .. count)
        assert(#data == 5 and string.byte(data, 2) == 255 and keep == 99)
        assert(__engine:isFileExists('empty') and not __engine:isFileExists('absent'))
        assert(__engine:file('empty') == '')
        local f = assert(io.open('SAVE\\Slot', 'w+'))
        f:write('ok')
        assert(f:close())
        f = assert(io.open('save/slot', 'r'))
        assert(f:read('*a') == 'ok' and f:close())
        done = count
    "#,
            "main",
        )
        .unwrap();
    host.run(&mut session);
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("done")
            .unwrap(),
        1
    );
    assert_eq!(session.state().pending_requests, 0);
    assert_eq!(host.files[&("save".into(), "slot".into())].1, b"ok");
    assert_eq!(
        host.reads
            .iter()
            .filter(|(key, _)| key == "data1")
            .map(|(_, offset)| *offset)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    assert!(host.reads.iter().all(|(key, _)| key != "empty"));
}

#[test]
fn writes_wait_for_host_confirmation_and_failures_resume_the_existing_error_branch() {
    let mut session = session(1, TransportLimits::default(), 128);
    session
        .open_chunk(
            br#"
        count = (count or 0) + 1
        local f = assert(io.open('save/slot', 'w'))
        opened = true
        f:write('data')
        local ok, message = pcall(function() f:close() end)
        assert(not ok and string.find(message, 'denied'))
        assert(f:close())
        done = count
    "#,
            "write",
        )
        .unwrap();
    session.step().unwrap();
    assert!(
        !session
            .interpreter()
            .lua()
            .globals()
            .contains_key("opened")
            .unwrap()
    );
    let truncate = requests(&mut session).pop().unwrap();
    assert_eq!(truncate.identity.service, IO_WRITE_ATOMIC);
    assert!(truncate.blob.is_empty());
    session
        .submit_replies(&[reply(&truncate, json!({"status":"ok"}), vec![])])
        .unwrap();
    assert!(
        !session
            .interpreter()
            .lua()
            .globals()
            .contains_key("opened")
            .unwrap()
    );
    session.step().unwrap();
    let write = requests(&mut session).pop().unwrap();
    assert_eq!(write.blob, b"data");
    session
        .submit_replies(&[reply(
            &write,
            json!({"status":"denied","message":"denied"}),
            vec![],
        )])
        .unwrap();
    session.step().unwrap();
    let retry = requests(&mut session).pop().unwrap();
    assert_eq!(retry.blob, write.blob);
    assert!(retry.identity.request_id > write.identity.request_id);
    session
        .submit_replies(&[reply(&retry, json!({"status":"ok"}), vec![])])
        .unwrap();
    assert!(
        !session
            .interpreter()
            .lua()
            .globals()
            .contains_key("done")
            .unwrap()
    );
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
    assert_eq!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<i32>("done")
            .unwrap(),
        1
    );
}

#[test]
fn malformed_reply_batches_preserve_requests_and_enforce_revision_and_eof() {
    let mut session = session(1, TransportLimits::default(), 128);
    session
        .open_chunk(b"assert(__engine:file('file') == 'ab')", "read")
        .unwrap();
    session.step().unwrap();
    let stat = requests(&mut session).pop().unwrap();
    for metadata in [
        json!({"status":"ok","found":true,"size":2,"revision":"7"}),
        json!({"status":"ok","found":true,"size":"18446744073709551616","revision":"7"}),
        json!({"status":"ok","found":true,"size":"2","revision":"7","modified_utc_ms":0}),
        json!({"status":"ok","found":true,"size":"2","revision":"7","modified_utc_ms":"9223372036854775808"}),
    ] {
        let before = session.state();
        assert!(matches!(
            session.submit_replies(&[reply(&stat, metadata, vec![])]),
            Err(ProtocolError::Argument(_))
        ));
        assert_eq!(session.state(), before);
    }
    let good = reply(
        &stat,
        json!({"status":"ok","found":true,"size":"2","revision":"7"}),
        vec![],
    );
    assert_eq!(
        session.submit_replies(&[good.clone(), good.clone()]),
        Err(ProtocolError::Stale)
    );
    session.submit_replies(&[good]).unwrap();
    session.step().unwrap();
    let read = requests(&mut session).pop().unwrap();
    for (metadata, blob) in [
        (
            json!({"status":"ok","revision":"8","eof":true}),
            b"ab".to_vec(),
        ),
        (
            json!({"status":"ok","revision":"7","eof":true}),
            b"a".to_vec(),
        ),
        (json!({"status":"ok","revision":"7","eof":false}), vec![]),
    ] {
        let before = session.state();
        assert!(matches!(
            session.submit_replies(&[reply(&read, metadata, blob)]),
            Err(ProtocolError::Argument(_))
        ));
        assert_eq!(session.state(), before);
    }
    session
        .submit_replies(&[reply(
            &read,
            json!({"status":"ok","revision":"7","eof":true}),
            b"ab".to_vec(),
        )])
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
}

#[test]
fn backpressure_cancel_reload_and_interleaved_sessions_never_replay_a_chunk() {
    let mut limits = TransportLimits::default();
    limits.control.records = 1;
    let mut a = session(1, limits, 128);
    let mut b = session(2, TransportLimits::default(), 128);
    let source = b"count=(count or 0)+1; local data=__engine:file('probe'); cancelled=(data==nil)";
    a.open_chunk(source, "a").unwrap();
    b.open_chunk(source, "b").unwrap();
    assert_eq!(a.step().unwrap(), ScriptIoProgress::WouldBlock);
    assert_eq!(a.step().unwrap(), ScriptIoProgress::WouldBlock);
    assert_eq!(
        a.interpreter().lua().globals().get::<i32>("count").unwrap(),
        1
    );
    assert!(requests(&mut a).is_empty());
    a.step().unwrap();
    b.step().unwrap();
    let old_a = requests(&mut a).pop().unwrap();
    let old_b = requests(&mut b).pop().unwrap();
    assert_eq!(
        a.submit_replies(&[reply(&old_b, json!({"status":"not_found"}), vec![])]),
        Err(ProtocolError::Stale)
    );
    let before = a.state();
    assert!(a.open_chunk(b"local =", "invalid").is_err());
    assert_eq!(a.state(), before);
    a.open_chunk(source, "replacement").unwrap();
    assert_eq!(
        a.submit_replies(&[reply(&old_a, json!({"status":"not_found"}), vec![])]),
        Err(ProtocolError::Stale)
    );
    a.step().unwrap();
    requests(&mut a);
    a.step().unwrap();
    let current = requests(&mut a).pop().unwrap();
    assert!(current.identity.request_id > old_a.identity.request_id);
    a.cancel_pending().unwrap();
    assert_eq!(
        a.submit_replies(&[reply(&current, json!({"status":"not_found"}), vec![])]),
        Err(ProtocolError::Stale)
    );
    assert_eq!(a.step().unwrap(), ScriptIoProgress::Completed);
    assert_eq!(
        a.interpreter().lua().globals().get::<i32>("count").unwrap(),
        2
    );
    assert!(
        a.interpreter()
            .lua()
            .globals()
            .get::<bool>("cancelled")
            .unwrap()
    );
    b.submit_replies(&[reply(&old_b, json!({"status":"not_found"}), vec![])])
        .unwrap();
    drop(a);
    assert_eq!(b.step().unwrap(), ScriptIoProgress::Completed);
    assert_eq!(
        b.interpreter().lua().globals().get::<i32>("count").unwrap(),
        1
    );
    let mut c = session(3, limits, 128);
    c.open_chunk(source, "cancel-before-dispatch").unwrap();
    assert_eq!(c.step().unwrap(), ScriptIoProgress::WouldBlock);
    c.cancel_pending().unwrap();
    assert_eq!(c.step().unwrap(), ScriptIoProgress::Completed);
    assert!(requests(&mut c).is_empty());
}

#[test]
fn resource_limits_and_invalid_keys_complete_without_physical_io_or_false_success() {
    let mut session = session(1, TransportLimits::default(), 2);
    session
        .open_chunk(
            br#"
        assert(__engine:file('../outside') == nil)
        assert(__engine:file('/absolute') == nil)
        __engine:tag{'var',system='file_exists',name='t.x',file='/'}
        assert(__engine:var('t.x') == '0')
        assert(__engine:file('large') == nil)
        local f = io.open('asset', 'w')
        assert(f == nil)
        done = true
    "#,
            "limits",
        )
        .unwrap();
    session.step().unwrap();
    let stat = requests(&mut session).pop().unwrap();
    assert_eq!(stat.metadata["key"], "large");
    session
        .submit_replies(&[reply(
            &stat,
            json!({"status":"ok","found":true,"size":"3","revision":"1"}),
            vec![],
        )])
        .unwrap();
    assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
    assert!(requests(&mut session).is_empty());
    assert!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<bool>("done")
            .unwrap()
    );
}
