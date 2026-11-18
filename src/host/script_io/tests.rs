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
                6 => Domain::Render,
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
    snapshot.call_stack = ["outer", "middle"]
        .into_iter()
        .map(|script| crate::save::CallFrameSnapshot {
            script: script.into(),
            return_line: 0,
        })
        .collect();
    snapshot.variables.set(
        "memsave.status",
        asb_interpreter::Value::String("saved".into()),
    );

    let mut host = MemoryHost::default();
    host.asset(
        "outer",
        br#"*main
[caption data=outer-return]
[return]
[lua]
assert(order==nil)
order='outer'
assert(__engine:file('outer-init')=='ok')
[/lua]
"#,
    );
    host.asset(
        "middle",
        br#"*main
[caption data=middle-return]
[return]
[lua]
assert(order=='outer')
order..=';middle'
assert(__engine:file('middle-init')=='ok')
[/lua]
"#,
    );
    host.asset("outer-init", b"ok");
    host.asset("middle-init", b"ok");
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
    assert(order=='outer;middle')
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
    assert_eq!(host.captions, ["restored", "middle-return", "outer-return"]);
    for path in ["outer", "middle", "target", "outer-init", "middle-init"] {
        assert_eq!(
            host.reads
                .iter()
                .filter(|(key, offset)| key == path && *offset == 0)
                .count(),
            1
        );
    }
}

#[test]
fn invalid_numbered_load_call_frame_keeps_the_old_save_point_and_queue() {
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
    save.current_line = 0;
    save.call_stack = [("prepared", 0), ("invalid-caller", 1000)]
        .into_iter()
        .map(|(script, return_line)| crate::save::CallFrameSnapshot {
            script: script.into(),
            return_line,
        })
        .collect();
    let mut host = MemoryHost::default();
    host.asset("prepared", b"*main\n[lua]\nprepared_init=true\n[/lua]");
    host.asset("invalid-caller", b"*main\n[return]\n");
    host.asset("target", b"*main\n[return]\n");
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
    assert!(host.reads.iter().any(|(key, _)| key == "prepared"));
    assert!(session.interpreter().get_script("prepared").is_none());
    assert!(
        session
            .interpreter()
            .lua()
            .globals()
            .get::<Option<bool>>("prepared_init")
            .unwrap()
            .is_none()
    );
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
[exec command=fullscreen]
[exec command=minimize]
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
    for command in ["fullscreen", "minimize"] {
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::WouldBlock
        );
        assert_eq!(
            runtime::step(handle, frame).unwrap(),
            ScriptIoProgress::WouldBlock
        );
        assert_record(&poll(), UI_WINDOW_ACTION, json!({"command": command}));
    }
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
fn queued_input_preserves_edges_across_io_and_releases_only_the_current_wait() {
    use crate::host::{
        input::{HostInput, TouchPhase},
        runtime::{self, RuntimeError},
    };

    let ini = b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n";
    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let epoch = runtime::open_project(handle, ini, "windows", None).unwrap();
    let mut host = MemoryHost::default();
    host.asset("pause", b"");
    host.asset(
        "boot.iet",
        br#"*top
[wt0]
[caption data=click]
[exkey btn=13]
[caption data=key]
[wait time=10000 input=1]
[caption data=input]
[wait time=1 input=0]
[caption data=timer]
[lua]
function tick(e)
  ticks=(ticks or 0)+1
  assert(e:getFrameNumber()==ticks)
  local point=e:getMousePoint()
  if ticks==1 then
    assert(not e:isDown(13) and not e:isDownEdge(13))
  elseif ticks==2 then
    assert(point.x==15 and point.y==-7)
    assert(e:isDown(13) and e:isDecide(13) and e:isPush(13))
    assert(not e:isDecide() and not e:isUpEdge(13))
    assert(e:getTouchCount()==1 and e:getTouchPoint()[9].x==20)
    assert(e:file('pause')=='')
    assert(e:isDownEdge(13) and e:getTouchPoint()[9].x==20)
  elseif ticks==3 then
    assert(e:isDown(13) and e:isPush(13) and not e:isDownEdge(13))
    assert(e:isDownEdge(67) and e:getTouchPoint()[9].x==30)
  elseif ticks==4 then
    assert(e:isDownEdge(13) and e:isUpEdge(13))
    assert(e:getTouchCount()==0 and e:isDown(13))
  elseif ticks==5 then
    assert(not e:isDown(13) and not e:isUpEdge(13) and e:isDownEdge(66))
  end
end
__engine:setEventHandler{onEnterFrame='tick'}
[/lua]
"#,
    );

    let poll = || {
        let mut bytes = vec![0; 4096];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        bytes.truncate(result.written_bytes);
        bytes
    };
    let caption = |expected: &str| {
        let bytes = poll();
        assert_eq!(
            bytes.len(),
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            UI_CAPTION.kind
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes[64..]).unwrap(),
            json!({"text":expected})
        );
    };
    let submit = |events: &[HostInput]| runtime::submit_input(handle, epoch, events);
    let key = |code, down| HostInput::Key { code, down };
    let touch = |phase, x| HostInput::Touch {
        id: 9,
        phase,
        x,
        y: 0,
    };
    let frame = |id, monotonic_ns| HostFrame {
        id,
        monotonic_ns,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let step = |frame| runtime::step(handle, frame).unwrap();

    assert!(matches!(
        submit(&[key(13, true), key(0, true)]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Argument(_)
        )))
    ));
    for _ in 0..2048 {
        match step(frame(1, 0)) {
            ScriptIoProgress::Waiting => {
                let request = decode_requests(&poll()).pop().unwrap();
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
            ScriptIoProgress::Event => break,
            progress => panic!("unexpected startup progress {progress:?}"),
        }
    }
    assert!(runtime::get_state(handle).unwrap().waiting_for_scenario);
    let ready = poll();
    assert_eq!(
        u32::from_le_bytes(ready[8..12].try_into().unwrap()),
        PROJECT_READY.kind
    );

    submit(&[
        HostInput::Pointer { x: 15, y: -7 },
        touch(TouchPhase::Down, 20),
        key(13, true),
    ])
    .unwrap();
    let second = frame(2, 10_000_000);
    let third = frame(3, 510_000_000);
    assert_eq!(step(second), ScriptIoProgress::Waiting);
    let request = decode_requests(&poll()).pop().unwrap();
    assert_eq!(request.metadata["key"], "pause");
    submit(&[touch(TouchPhase::Move, 30), key(67, true)]).unwrap();
    assert_eq!(step(third), ScriptIoProgress::WouldBlock);
    assert_eq!(runtime::get_state(handle).unwrap().host_frame_id, Some(2));
    runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
    assert_eq!(step(second), ScriptIoProgress::Event);
    caption("click");
    assert_eq!(step(second), ScriptIoProgress::Event);
    assert_eq!(step(third), ScriptIoProgress::Event);
    assert!(poll().is_empty());

    submit(&[key(13, false), key(13, true), touch(TouchPhase::Up, 40)]).unwrap();
    assert_eq!(step(frame(4, 510_000_000)), ScriptIoProgress::Event);
    caption("key");
    submit(&[HostInput::FocusLost, key(66, true)]).unwrap();
    assert_eq!(step(frame(5, 510_000_000)), ScriptIoProgress::Event);
    caption("input");
    submit(&[key(1, true)]).unwrap();
    assert_eq!(step(frame(6, 510_999_999)), ScriptIoProgress::Event);
    assert!(poll().is_empty());
    assert_eq!(step(frame(7, 511_000_000)), ScriptIoProgress::Completed);
    caption("timer");

    let next_epoch = runtime::open_project(handle, ini, "windows", None).unwrap();
    assert_ne!(next_epoch, epoch);
    assert!(matches!(
        submit(&[key(13, true)]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Stale
        )))
    ));
    runtime::destroy(handle).unwrap();
}

#[test]
fn skip_respects_timers_unread_text_and_a_rebound_advance_key() {
    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[keyconfig role=0 keys=70]
[exec command=skip mode=1]
[wait time=10 input=0]
A
[wt0]
[calllua function=mark]
[wt0]
[calllua function=mark]
[stop]
[lua]
function mark() marks=(marks or 0)+1 end
[/lua]
"#,
    );
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    let marks = || globals.get::<Option<u32>>("marks").unwrap().unwrap_or(0);
    let mut step = |id, ms, keys: &[(u32, bool)]| {
        submit_playback_keys(&mut session, epoch, keys);
        assert_eq!(
            session.step_queued_frame(playback_frame(id, ms)).unwrap(),
            ScriptIoProgress::Event
        );
    };
    step(2, 9, &[]);
    assert_eq!(marks(), 0);
    step(3, 10, &[]);
    step(4, 11, &[(13, true)]);
    assert_eq!(marks(), 0);
    step(5, 12, &[(17, true)]);
    step(5, 12, &[]);
    assert_eq!(marks(), 1);
    step(6, 13, &[(17, false)]);
    assert_eq!(marks(), 1);
    step(7, 14, &[(70, true)]);
    assert_eq!(marks(), 2);
}

#[test]
fn autoplay_waits_for_text_voice_and_delay_and_consumes_the_exit_click() {
    use crate::host::media::PlaybackStatus;

    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[exec command=automode mode=1]
[chgmsg id=message layered=1]
[scetween type=in delay=100 param=alpha diff=-255]
[voice id=line file=voice.ogg]
AA
[@]
[calllua function=mark]
[wt0]
[calllua function=mark]
[stop]
[lua]
function mark() marks=(marks or 0)+1 end
[/lua]
"#,
    );
    session.set_default_font(epoch, &test_font()).unwrap();
    drive_playback_boot(&mut session, &mut host);
    session
        .interpreter
        .set_variable("s.automodewait", asb_interpreter::Value::Int(10));
    let globals = session.interpreter().lua().globals();
    let marks = || globals.get::<Option<u32>>("marks").unwrap().unwrap_or(0);
    let mut bytes = [0; 4096];
    let polled = session.poll_events(&mut bytes);
    let mut records = &bytes[..polled.written_bytes];
    let voice = loop {
        let len = u32::from_le_bytes(records[0..4].try_into().unwrap()) as usize;
        if u16::from_le_bytes(records[6..8].try_into().unwrap()) == Domain::Media as u16 {
            let body = serde_json::from_slice::<Value>(&records[64..len]).unwrap();
            break body["playback_id"].as_str().unwrap().parse().unwrap();
        }
        records = &records[len..];
    };
    let step = |session: &mut ScriptIoSession, id, ms| {
        assert_eq!(
            session.step_queued_frame(playback_frame(id, ms)).unwrap(),
            ScriptIoProgress::Event
        );
    };
    assert!(!session.scene.as_ref().unwrap().text_revealed());
    step(&mut session, 2, 250);
    assert!(session.scene.as_ref().unwrap().text_revealed());
    assert_eq!(marks(), 0);
    session
        .submit_playback_status(epoch, voice, PlaybackStatus::Ended)
        .unwrap();
    step(&mut session, 3, 251);
    step(&mut session, 4, 260);
    assert_eq!(marks(), 0);
    step(&mut session, 5, 261);
    assert_eq!(marks(), 1);

    submit_playback_keys(&mut session, epoch, &[(1, true)]);
    step(&mut session, 6, 262);
    step(&mut session, 6, 262);
    step(&mut session, 7, 300);
    assert_eq!(marks(), 1);
    submit_playback_keys(&mut session, epoch, &[(1, false), (1, true)]);
    step(&mut session, 8, 301);
    assert_eq!(marks(), 2);
}

fn playback_project(source: &[u8]) -> (ScriptIoSession, u64, MemoryHost) {
    let mut session = session(1, TransportLimits::default(), 4096);
    let epoch = session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    let mut host = MemoryHost::default();
    host.asset("boot.iet", source);
    (session, epoch, host)
}

fn playback_frame(id: u64, ms: u64) -> HostFrame {
    HostFrame {
        id,
        monotonic_ns: ms * 1_000_000,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    }
}

fn submit_playback_keys(session: &mut ScriptIoSession, epoch: u64, keys: &[(u32, bool)]) {
    use crate::host::input::HostInput;
    let events: Vec<_> = keys
        .iter()
        .map(|&(code, down)| HostInput::Key { code, down })
        .collect();
    session.submit_input(epoch, &events).unwrap();
}

fn drive_playback_boot(session: &mut ScriptIoSession, host: &mut MemoryHost) {
    for _ in 0..1024 {
        match session.step_queued_frame(playback_frame(1, 0)).unwrap() {
            ScriptIoProgress::Event => return,
            ScriptIoProgress::Waiting => {
                for request in requests(session) {
                    session.submit_replies(&[host.complete(&request)]).unwrap();
                }
            }
            progress => panic!("unexpected project progress {progress:?}"),
        }
    }
    panic!("project did not reach its first wait");
}

#[test]
fn global_push_claims_its_press_and_dummy_decide_is_not_physical_input() {
    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[setonpush key=13 keyrepeat=1 handler=calllua function=push]
[keyconfig role=0 keys=13,124]
[wt0]
[calllua function=mark]
[wait time=10000 input=1]
[calllua function=mark]
[stop]
[lua]
function mark() marks=(marks or 0)+1 end
function push(e,p)
    assert(p.key=='13' and p.type=='key')
    pushed=(pushed or 0)+1
end
function tick(e)
    if inject then
        assert(not e:isDownEdge(124))
        e:overrideKey{key=124,status=32}
        assert(e:isDecide(124) and not e:isDownEdge(124))
    end
end
__engine:setEventHandler{onEnterFrame='tick'}
__engine:setEventFilter(function(e,name,p)
    assert(name=='setonpush' and p.key=='13' and p.keyrepeat=='1')
    filtered=(filtered or 0)+1
    if filtered==1 then
        assert(e:file('push-gate')=='')
    end
    return reject and 2 or 0
end)
[/lua]
"#,
    );
    host.asset("push-gate", b"");
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    let count = |key| globals.get::<Option<u32>>(key).unwrap().unwrap_or(0);
    let step = |session: &mut ScriptIoSession, id, ms| {
        session.step_queued_frame(playback_frame(id, ms)).unwrap()
    };
    submit_playback_keys(&mut session, epoch, &[(13, true)]);
    assert_eq!(step(&mut session, 2, 1), ScriptIoProgress::Waiting);
    let request = requests(&mut session).pop().unwrap();
    assert_eq!(request.metadata["key"], "push-gate");
    assert_eq!(step(&mut session, 3, 501), ScriptIoProgress::WouldBlock);
    session.submit_replies(&[host.complete(&request)]).unwrap();
    for (id, ms) in [(2, 1), (2, 1), (3, 500), (4, 501), (4, 501)] {
        assert_eq!(step(&mut session, id, ms), ScriptIoProgress::Event);
    }
    assert_eq!(count("filtered"), 2);
    assert_eq!(count("pushed"), 2);
    assert_eq!(count("marks"), 0);

    globals.set("inject", true).unwrap();
    submit_playback_keys(&mut session, epoch, &[(13, false)]);
    assert_eq!(step(&mut session, 5, 502), ScriptIoProgress::Event);
    assert_eq!(step(&mut session, 6, 503), ScriptIoProgress::Event);
    assert_eq!(count("marks"), 1);
    globals.set("inject", false).unwrap();
    globals.set("reject", true).unwrap();
    submit_playback_keys(&mut session, epoch, &[(13, true)]);
    assert_eq!(step(&mut session, 7, 504), ScriptIoProgress::Event);
    assert_eq!(count("filtered"), 3);
    assert_eq!(count("pushed"), 2);
    assert_eq!(count("marks"), 2);
}

#[test]
fn autoplay_mode_callbacks_run_on_transitions_and_restore_the_stop_wait() {
    let (mut session, _, mut host) = playback_project(
        br#"*top
[setonautomodein handler=calllua function=record]
[setonautomodeout handler=calllua function=record]
[exec command=automode mode=1]
[exec command=automode mode=1]
[stop]
[lua]
function record(e,p)
    trace=(trace or '')..p.type..';'
end
__engine:setEventFilter(function(e,name,p)
    assert(name=='setonautomodein' or name=='setonautomodeout')
    if name=='setonautomodeout' then assert(e:file('mode-gate')=='') end
    return 0
end)
[/lua]
"#,
    );
    host.asset("mode-gate", b"");
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "automodein;automodeout;"
    );
    assert!(matches!(
        session.pending_event(),
        Some(Event::Wait {
            reason: asb_interpreter::WaitReason::Stop { .. }
        })
    ));
    assert_eq!(
        session.step_queued_frame(playback_frame(1, 0)).unwrap(),
        ScriptIoProgress::Event
    );
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "automodein;automodeout;"
    );
}

#[test]
fn forced_drag_moves_the_knob_and_releases_with_the_physical_button_masked() {
    use crate::host::input::HostInput;

    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[lyc id=track width=5 height=5 color=ffffff]
[lyprop id=track draggable=1]
[lyevent id=track type=dragin label=grab call=1]
[lyc id=knob width=5 height=5 color=ffffff]
[lyprop id=knob left=10 top=10 dragarea="0,0,25,35"]
[lyevent id=knob type=drag handler=calllua function=drag]
[lyevent id=knob type=dragout handler=calllua function=release]
[wt0]
[calllua function=advance]
[stop]
*grab
[lydrag id=knob]
[calllua function=grab]
[return]
[lua]
function grab() trace='grab'; captured=true end
function drag(e,p) assert(p.id=='knob' and p.drag=='1'); trace..=';move' end
function release(e,p)
    assert(p.id=='knob' and p.drag=='0' and not e:isUpEdge(1))
    trace..=';release'
    captured=false
end
function tick(e)
    if captured then e:overrideKey{key=1,status=0} end
end
function advance() trace..=';advance' end
__engine:setEventHandler{onEnterFrame='tick'}
[/lua]
"#,
    );
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    let key = |down| HostInput::Key { code: 1, down };
    let point = |x, y| HostInput::Pointer { x, y };
    let step = |session: &mut ScriptIoSession, id, inputs: &[HostInput]| {
        session.submit_input(epoch, inputs).unwrap();
        assert_eq!(
            session.step_queued_frame(playback_frame(id, id)).unwrap(),
            ScriptIoProgress::Event
        );
    };
    step(&mut session, 2, &[point(2, 2), key(true)]);
    assert_eq!(globals.get::<String>("trace").unwrap(), "grab");
    step(&mut session, 3, &[point(50, 50)]);
    let scene = session.scene.as_ref().unwrap().save_snapshot();
    assert_eq!(scene.get("track").unwrap().props.offset(), (0.0, 0.0));
    assert_eq!(scene.get("knob").unwrap().props.offset(), (25.0, 35.0));
    step(&mut session, 4, &[key(false)]);
    step(&mut session, 4, &[]);
    assert_eq!(globals.get::<String>("trace").unwrap(), "grab;move;release");
    step(&mut session, 5, &[key(true)]);
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "grab;move;release;advance"
    );
}

#[test]
fn text_link_clicks_follow_the_message_layer_transform_and_visibility() {
    use crate::host::input::HostInput;

    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[chgmsg id=1.message layered=1]
[lyprop id=1 left=80 top=60 visible=0]
[font left=10 top=10 size=20]
[link label=target type=1 color=ff0000]
AA
[/link]
[wt0]
[calllua function=show]
[lyprop id=1 visible=1]
[wt0]
[calllua function=wrong]
[stop]
*target
[calllua function=jumped]
[stop]
[lua]
function show() trace='show' end
function wrong() trace..=';wrong' end
function jumped() trace..=';jumped' end
[/lua]
"#,
    );
    session.set_default_font(epoch, &test_font()).unwrap();
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    let key = |down| HostInput::Key { code: 1, down };
    let step = |session: &mut ScriptIoSession, id, inputs: &[HostInput]| {
        session.submit_input(epoch, inputs).unwrap();
        assert_eq!(
            session.step_queued_frame(playback_frame(id, id)).unwrap(),
            ScriptIoProgress::Event
        );
    };
    step(
        &mut session,
        2,
        &[HostInput::Pointer { x: 92, y: 72 }, key(true)],
    );
    assert_eq!(globals.get::<String>("trace").unwrap(), "show");
    step(&mut session, 3, &[key(false), key(true)]);
    assert_eq!(globals.get::<String>("trace").unwrap(), "show;jumped");
}

#[test]
fn hidden_message_window_consumes_the_restore_click_and_preserves_visibility() {
    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[lyc id=window width=10 height=10 color=ffffff]
[lyc id=off width=10 height=10 color=ffffff]
[lyprop id=off visible=0]
[chgmsg id=message layered=1]
[hide allow=1 window="window,off"]
[setonhidein handler=calllua function=mark]
[setonhideout handler=calllua function=mark]
[setonbacklogin handler=calllua function=mark]
[wt0]
[calllua function=advance]
[stop]
[lua]
function mark(e,p) trace=(trace or '')..p.type..';' end
function advance() trace..='advance;' end
[/lua]
"#,
    );
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    let mut step = |id, keys: &[(u32, bool)]| {
        submit_playback_keys(&mut session, epoch, keys);
        assert_eq!(
            session.step_queued_frame(playback_frame(id, id)).unwrap(),
            ScriptIoProgress::Event
        );
        let scene = session.scene.as_ref().unwrap();
        (scene.save_snapshot(), session.capture_scene_snapshot(scene))
    };
    let (hidden, saved) = step(2, &[(32, true)]);
    assert!(!hidden.is_effectively_visible("window"));
    assert!(!hidden.is_effectively_visible("message"));
    assert!(saved.is_effectively_visible("window"));
    assert!(saved.is_effectively_visible("message"));
    assert!(!saved.is_effectively_visible("off"));
    let (shown, _) = step(3, &[(1, true)]);
    assert!(shown.is_effectively_visible("window"));
    assert!(shown.is_effectively_visible("message"));
    assert!(!shown.is_effectively_visible("off"));
    step(4, &[(38, true)]);
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "hidein;hideout;backlogin;"
    );
    step(5, &[(1, false), (1, true)]);
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "hidein;hideout;backlogin;advance;"
    );
}

#[test]
fn right_click_menu_loads_through_host_io_and_returns_to_the_story_wait() {
    let (mut session, epoch, mut host) = playback_project(
        br#"*top
[rclick allow=1 file=menu.iet]
[setonpush key=2 handler=calllua function=wrong]
[wt0]
[calllua function=advance]
[stop]
[lua]
function wrong() error('right click fell through to push') end
function advance() trace..=';advance' end
[/lua]
"#,
    );
    host.asset(
        "menu.iet",
        br#"*top
[calllua function=open]
[stop]
*leave
[calllua function=close]
[return]
[lua]
function open() trace='open' end
function close() trace..=';close' end
[/lua]
"#,
    );
    drive_playback_boot(&mut session, &mut host);
    let globals = session.interpreter().lua().globals();
    submit_playback_keys(&mut session, epoch, &[(2, true)]);
    let open_frame = playback_frame(2, 1);
    assert_eq!(
        session.step_queued_frame(open_frame).unwrap(),
        ScriptIoProgress::Waiting
    );
    for _ in 0..1024 {
        for request in requests(&mut session) {
            assert_eq!(request.metadata["key"], "menu.iet");
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
        if session.step_queued_frame(open_frame).unwrap() == ScriptIoProgress::Event {
            break;
        }
    }
    assert_eq!(session.interpreter().current_script(), Some("menu.iet"));
    assert_eq!(globals.get::<String>("trace").unwrap(), "open");
    submit_playback_keys(&mut session, epoch, &[(2, false), (27, true)]);
    assert_eq!(
        session.step_queued_frame(playback_frame(3, 2)).unwrap(),
        ScriptIoProgress::Event
    );
    assert_eq!(session.interpreter().current_script(), Some("boot.iet"));
    assert_eq!(globals.get::<String>("trace").unwrap(), "open;close");
    submit_playback_keys(&mut session, epoch, &[(13, true)]);
    assert_eq!(
        session.step_queued_frame(playback_frame(4, 3)).unwrap(),
        ScriptIoProgress::Event
    );
    assert_eq!(
        globals.get::<String>("trace").unwrap(),
        "open;close;advance"
    );
}

#[test]
fn numbered_save_from_a_menu_reloads_the_story_wait_with_the_save_hook_values() {
    let (mut saver, epoch, mut host) = playback_project(
        br#"*top
[var name=story_state data=7]
[rclick allow=1 file=menu.iet]
[wt0]
[calllua function=continued]
[stop]
[lua]
function continued() advanced=true end
function capture(e,p)
    assert(p.file=='slot')
    e:enqueueTag{'var',name='memsave.marker',data='saved'}
end
__engine:setEventHandler{onSave='capture'}
[/lua]
"#,
    );
    host.asset(
        "menu.iet",
        b"*top\n[var name=story_state data=99]\n[wt0]\n[save file=slot]\n[stop]\n",
    );
    host.asset("load.iet", b"*top\n[load file=slot]\n[stop]\n");
    drive_playback_boot(&mut saver, &mut host);
    let drive = |session: &mut ScriptIoSession, host: &mut MemoryHost, frame| {
        for _ in 0..2048 {
            if session.step_queued_frame(frame).unwrap() == ScriptIoProgress::Event {
                return;
            }
            for request in requests(session) {
                session.submit_replies(&[host.complete(&request)]).unwrap();
            }
        }
        panic!("project did not reach its next wait");
    };
    submit_playback_keys(&mut saver, epoch, &[(2, true)]);
    drive(&mut saver, &mut host, playback_frame(2, 1));
    assert_eq!(saver.interpreter().current_script(), Some("menu.iet"));
    submit_playback_keys(&mut saver, epoch, &[(13, true)]);
    drive(&mut saver, &mut host, playback_frame(3, 2));
    let save: crate::save::SaveData =
        serde_json::from_slice(&host.files[&("save".into(), "slot".into())].1).unwrap();
    assert_eq!(save.current_script, "boot.iet");
    assert_eq!(save.waiting_for_input, Some(true));
    assert!(save.call_stack.is_empty());
    assert_eq!(
        save.variables.get("story_state"),
        Some(&asb_interpreter::Value::Int(7))
    );
    assert_eq!(
        save.variables.get("memsave.marker"),
        Some(&asb_interpreter::Value::String("saved".into()))
    );

    for waiting_for_input in [Some(true), None] {
        let mut fixture = save.clone();
        fixture.waiting_for_input = waiting_for_input;
        host.files.insert(
            ("save".into(), "slot".into()),
            (1, serde_json::to_vec(&fixture).unwrap()),
        );
        let mut loader = session(2, TransportLimits::default(), 4096);
        let epoch = loader
            .open_queued_project(
                b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=load.iet\n",
                "windows",
                None,
            )
            .unwrap();
        drive(&mut loader, &mut host, playback_frame(1, 0));
        let globals = loader.interpreter().lua().globals();
        assert_eq!(loader.interpreter().current_script(), Some("boot.iet"));
        assert!(!globals.get::<bool>("advanced").unwrap());
        submit_playback_keys(&mut loader, epoch, &[(13, true)]);
        drive(&mut loader, &mut host, playback_frame(2, 1));
        assert!(globals.get::<bool>("advanced").unwrap());
    }
}

#[test]
fn numbered_load_blends_the_previous_frame_without_blocking_the_restored_script() {
    let (mut producer, _, mut host) = playback_project(
        b"*top\n[lyc id=restored width=2 height=1 color=ffffff]\n[save file=slot]\n[calllua function=mark]\n[stop]\n[lua]\nfunction mark() restored=true end\n[/lua]\n",
    );
    drive_playback_boot(&mut producer, &mut host);
    host.asset("load.iet", b"*top\n[lyc id=previous width=1 height=1 color=ffffff]\n[flip]\n[load file=slot type=2]\n[stop]\n");
    let mut loader = session(2, TransportLimits::default(), 4096);
    loader
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=load.iet\n",
            "windows",
            None,
        )
        .unwrap();
    drive_playback_boot(&mut loader, &mut host);
    loader.publish_frame(ScriptIoProgress::Event).unwrap();
    let previous = loader.frames.latest_snapshot().unwrap();
    assert_eq!(previous.sprites[0].layer_id, "previous");

    let start = playback_frame(2, 10);
    for _ in 0..2048 {
        let progress = loader.step_queued_frame(start).unwrap();
        if progress == ScriptIoProgress::Event {
            loader.publish_frame(progress).unwrap();
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        for request in requests(&mut loader) {
            loader.submit_replies(&[host.complete(&request)]).unwrap();
        }
    }
    assert!(
        loader
            .interpreter()
            .lua()
            .globals()
            .get::<bool>("restored")
            .unwrap()
    );
    let new = loader.frames.latest_snapshot().unwrap();
    assert_eq!(new.sprites[0].layer_id, "restored");
    let blend = new.transition.as_ref().unwrap();
    assert_eq!(blend.source[0].layer_id, "previous");
    assert_eq!(blend.progress, 0.0);
    assert!(blend.rule.is_none());

    let progress = loader.step_queued_frame(playback_frame(3, 510)).unwrap();
    loader.publish_frame(progress).unwrap();
    assert_eq!(
        loader
            .frames
            .latest_snapshot()
            .unwrap()
            .transition
            .as_ref()
            .unwrap()
            .progress,
        0.5
    );
    let progress = loader.step_queued_frame(playback_frame(4, 1010)).unwrap();
    loader.publish_frame(progress).unwrap();
    assert!(
        loader
            .frames
            .latest_snapshot()
            .unwrap()
            .transition
            .is_none()
    );
    assert!(previous.transition.is_none());
}

#[test]
fn avoid_state_survives_backpressure_and_window_buttons_restore_it_before_callbacks() {
    use crate::host::input::HostInput;

    let mut limits = TransportLimits::default();
    limits.control.records = 1;
    let mut session = session(1, limits, 4096);
    let epoch = session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[avoid file=cover.png windowbutton=2]
[keyconfig role=15 keys=70]
[setonwindowbutton button=0 handler=calllua function=pressed]
[wt0]
[stop]
[lua]
function pressed(e,p)
    assert(p.type=='windowbutton' and p.button=='0')
    called=(called or 0)+1
end
[/lua]
"#,
    );
    let start = playback_frame(1, 0);
    for _ in 0..1024 {
        let progress = session.step_queued_frame(start).unwrap();
        for request in requests(&mut session) {
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
        if progress == ScriptIoProgress::Event {
            break;
        }
    }
    let poll = |session: &mut ScriptIoSession, kind: MessageKind| {
        let mut bytes = [0; 4096];
        let result = session.poll_events(&mut bytes);
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize,
            result.written_bytes
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            kind.kind
        );
        serde_json::from_slice::<Value>(&bytes[64..result.written_bytes]).unwrap()
    };
    let occupy_queue = |session: &mut ScriptIoSession| {
        session
            .transport
            .notify(
                UI_CAPTION,
                MessageBody::new(r#"{"text":"busy"}"#.into(), vec![]).unwrap(),
            )
            .unwrap()
    };
    let step = |session: &mut ScriptIoSession, id| {
        session.step_queued_frame(playback_frame(id, id)).unwrap()
    };
    occupy_queue(&mut session);
    submit_playback_keys(&mut session, epoch, &[(70, true)]);
    assert_eq!(step(&mut session, 2), ScriptIoProgress::WouldBlock);
    assert_eq!(step(&mut session, 3), ScriptIoProgress::WouldBlock);
    assert_eq!(poll(&mut session, UI_CAPTION), json!({"text":"busy"}));
    assert_eq!(step(&mut session, 2), ScriptIoProgress::Event);
    assert_eq!(
        poll(&mut session, UI_AVOID),
        json!({"visible":true,"mute":true,"asset":"cover.png","windowbutton":2})
    );

    let globals = session.interpreter().lua().globals();
    occupy_queue(&mut session);
    session
        .submit_input(epoch, &[HostInput::WindowButton { button: 0 }])
        .unwrap();
    assert_eq!(step(&mut session, 3), ScriptIoProgress::WouldBlock);
    assert!(globals.get::<Option<i32>>("called").unwrap().is_none());
    poll(&mut session, UI_CAPTION);
    assert_eq!(step(&mut session, 3), ScriptIoProgress::Event);
    assert_eq!(
        poll(&mut session, UI_AVOID),
        json!({"visible":false,"mute":false})
    );
    assert_eq!(globals.get::<i32>("called").unwrap(), 1);
    assert_eq!(step(&mut session, 3), ScriptIoProgress::Event);
    assert_eq!(globals.get::<i32>("called").unwrap(), 1);
}

#[test]
fn audio_feedback_uses_playback_identity_and_resumes_its_wait_after_callback_io() {
    use crate::host::{
        media::PlaybackStatus,
        runtime::{self, RuntimeError},
    };

    let mut limits = TransportLimits::default();
    limits.control.records = 1;
    let handle = runtime::create(0, limits, 4096).unwrap();
    let epoch = runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let mut host = MemoryHost::default();
    host.asset("empty", b"");
    host.asset(
        "boot.iet",
        br#"*top
[splay file=:bgm/theme_a.ogg loop=1 gain=450]
[setonsoundfinish id=fx label=finished call=1]
[seplay id=fx file=old.ogg]
[seplay id=fx file=new.ogg]
[wait se=fx time=50]
[caption data=timed]
[wait se=fx]
[caption data=ended]
[stop]
*finished
[calllua function=verify]
[caption data=callback]
[return]
[lua]
__engine:setMagicPath{'bgm', 'sound'}
function verify(e)
  assert(not finished)
  finished=true
  assert(e:file('empty')=='')
end
[/lua]
"#,
    );
    let poll = || {
        let mut bytes = vec![0; 4096];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        bytes.truncate(result.written_bytes);
        bytes
    };
    let record = |kind: MessageKind| {
        let bytes = poll();
        assert_eq!(
            bytes.len(),
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize
        );
        assert_eq!(
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            kind.domain as u16
        );
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            kind.kind
        );
        serde_json::from_slice::<Value>(&bytes[64..]).unwrap()
    };
    let id = |body: &Value| {
        body["playback_id"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let step = |id, monotonic_ns| {
        runtime::step(
            handle,
            HostFrame {
                id,
                monotonic_ns,
                utc_unix_ms: 0,
                utc_offset_minutes: 0,
            },
        )
        .unwrap()
    };
    let feedback = |id, status| runtime::submit_playback_status(handle, epoch, id, status);
    let is_stale = |error| {
        matches!(
            error,
            Err(RuntimeError::Session(ScriptIoError::Protocol(
                ProtocolError::Stale
            )))
        )
    };

    assert_eq!(record(SESSION_CHANGED), json!({}));
    for _ in 0..1024 {
        match step(1, 0) {
            ScriptIoProgress::Waiting => {
                let request = decode_requests(&poll()).pop().unwrap();
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
            ScriptIoProgress::WouldBlock => break,
            progress => panic!("unexpected boot progress {progress:?}"),
        }
    }
    assert_eq!(record(PROJECT_READY), json!({}));
    assert_eq!(step(1, 0), ScriptIoProgress::WouldBlock);
    let bgm = record(MEDIA_AUDIO_COMMAND);
    assert_eq!(bgm["key"], "sound/theme_a.ogg");
    assert_eq!(bgm["loop_key"], "sound/theme_b.ogg");
    assert_eq!(bgm["gain"], 450);
    assert_eq!(step(1, 0), ScriptIoProgress::WouldBlock);
    let old = record(MEDIA_AUDIO_COMMAND);
    assert_eq!(old["key"], "old.ogg");
    assert_eq!(step(1, 0), ScriptIoProgress::Event);
    let current = record(MEDIA_AUDIO_COMMAND);
    assert_eq!(current["key"], "new.ogg");
    assert_eq!(current["replaces"], old["playback_id"]);
    assert_ne!(id(&old), id(&current));
    assert!(is_stale(feedback(id(&old), PlaybackStatus::Ended)));

    feedback(id(&current), PlaybackStatus::Started).unwrap();
    assert!(is_stale(feedback(id(&current), PlaybackStatus::Started)));
    assert_eq!(step(1, 0), ScriptIoProgress::Event);
    assert_eq!(step(2, 20_000_000), ScriptIoProgress::Event);
    assert_eq!(step(3, 69_999_999), ScriptIoProgress::Event);
    assert!(poll().is_empty());
    assert_eq!(step(4, 70_000_000), ScriptIoProgress::Event);
    assert_eq!(record(UI_CAPTION), json!({"text":"timed"}));

    feedback(id(&current), PlaybackStatus::Ended).unwrap();
    assert!(is_stale(feedback(id(&current), PlaybackStatus::Ended)));
    assert_eq!(step(4, 70_000_000), ScriptIoProgress::Event);
    assert!(poll().is_empty());
    assert_eq!(step(5, 71_000_000), ScriptIoProgress::Waiting);
    let request = decode_requests(&poll()).pop().unwrap();
    assert_eq!(request.metadata["key"], "empty");
    runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
    assert_eq!(step(5, 71_000_000), ScriptIoProgress::WouldBlock);
    assert_eq!(record(UI_CAPTION), json!({"text":"callback"}));
    assert_eq!(step(5, 71_000_000), ScriptIoProgress::Event);
    assert_eq!(record(UI_CAPTION), json!({"text":"ended"}));
    assert!(is_stale(feedback(id(&current), PlaybackStatus::Cancelled)));
    runtime::destroy(handle).unwrap();
}

#[test]
fn host_video_layers_publish_immutable_frames_and_resume_media_waits() {
    use crate::host::{
        media::{PlaybackStatus, VideoFrame},
        runtime::{self, RuntimeError},
    };

    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let epoch = runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=640\nHEIGHT=480\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=1.80]
[lyprop id=1.80 left=12 top=4]
[setonvideofinish id=1.80 label=finish call=1]
[video id=1.80 file=overlay.ogv]
[wait video=1.80]
[caption data=layer_done]
[video file=ending.mp4 skip=2]
[caption data=full_done]
[stop]
*finish
[caption data=callback]
[return]
"#,
    );
    let step = |id| {
        runtime::step(
            handle,
            HostFrame {
                id,
                monotonic_ns: id * 1_000_000,
                utc_unix_ms: 0,
                utc_offset_minutes: 0,
            },
        )
        .unwrap()
    };
    let poll = || {
        let mut bytes = vec![0; 4096];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        bytes.truncate(result.written_bytes);
        bytes
    };
    let drain = |mut bytes: &[u8]| {
        let mut commands = Vec::new();
        while !bytes.is_empty() {
            let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
            let domain = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
            let kind = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            if domain == Domain::Media as u16 || domain == Domain::Ui as u16 {
                commands.push((
                    kind,
                    serde_json::from_slice::<Value>(&bytes[64..len]).unwrap(),
                ));
            }
            bytes = &bytes[len..];
        }
        commands
    };
    let mut commands = Vec::new();
    for _ in 0..1024 {
        let progress = step(1);
        let bytes = poll();
        commands.extend(drain(&bytes));
        if progress == ScriptIoProgress::Event {
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        let request = decode_requests(&bytes).pop().unwrap();
        runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
    }
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].0, MEDIA_VIDEO_COMMAND.kind);
    assert_eq!(commands[0].1["key"], "overlay.ogv");
    assert_eq!(commands[0].1["layer"], "1.80");
    let playback = |body: &Value| {
        body["playback_id"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let layer = playback(&commands[0].1);
    let submit = |frame_id, rgba: &[u8]| {
        runtime::submit_video_frame(
            handle,
            epoch,
            VideoFrame {
                playback_id: layer,
                frame_id,
                width: 1,
                height: 1,
                rgba,
            },
        )
    };
    let stale = |value| {
        matches!(
            value,
            Err(RuntimeError::Session(ScriptIoError::Protocol(
                ProtocolError::Stale
            )))
        )
    };
    let first_pixel = [1, 2, 3, 255];
    let next_pixel = [4, 5, 6, 180];
    assert!(matches!(
        submit(1, &[0; 3]),
        Err(RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Argument(_)
        )))
    ));
    submit(1, &first_pixel).unwrap();
    assert_eq!(step(1), ScriptIoProgress::Event);
    let blank = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert!(blank.sprites.is_empty());
    drop(blank);
    assert_eq!(step(2), ScriptIoProgress::Event);
    let first = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(first.sprites[0].layer_id, "1.80");
    assert_eq!(first.sprites[0].transform[4..], [12.0, 4.0]);
    assert!(stale(submit(1, &next_pixel)));
    submit(2, &first_pixel).unwrap();
    submit(3, &next_pixel).unwrap();
    assert_eq!(step(3), ScriptIoProgress::Event);
    let next = runtime::acquire_latest(handle, first.render_frame_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        next.texture(next.sprites[0].texture).unwrap().rgba(),
        next_pixel
    );
    assert_eq!(
        first.texture(first.sprites[0].texture).unwrap().rgba(),
        first_pixel
    );
    let previous = next.render_frame_id;
    drop(next);
    assert!(poll().is_empty());

    runtime::submit_playback_status(handle, epoch, layer, PlaybackStatus::Ended).unwrap();
    assert!(stale(submit(4, &next_pixel)));
    assert_eq!(step(4), ScriptIoProgress::Event);
    let records = drain(&poll());
    assert_eq!(records.len(), 3);
    assert_eq!(records[0], (UI_CAPTION.kind, json!({"text":"callback"})));
    assert_eq!(records[1], (UI_CAPTION.kind, json!({"text":"layer_done"})));
    assert_eq!(records[2].0, MEDIA_VIDEO_COMMAND.kind);
    assert_eq!(records[2].1["layer"], Value::Null);
    assert_eq!(records[2].1["skip"], 2);
    let blank = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    assert!(blank.sprites.is_empty());
    drop(blank);
    drop(first);
    let fullscreen = playback(&records[2].1);
    runtime::submit_playback_status(handle, epoch, fullscreen, PlaybackStatus::Cancelled).unwrap();
    assert_eq!(step(4), ScriptIoProgress::Event);
    assert!(poll().is_empty());
    assert_eq!(step(5), ScriptIoProgress::Event);
    assert_eq!(
        drain(&poll()),
        [(UI_CAPTION.kind, json!({"text":"full_done"}))]
    );
    runtime::destroy(handle).unwrap();
}

#[test]
fn host_font_draws_text_on_click_wait_and_keeps_previous_atlas_alive() {
    use crate::host::{input::HostInput, runtime};

    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let epoch = runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    runtime::set_default_font(handle, epoch, &test_font()).unwrap();
    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[chgmsg id=message layered=1]
[lyprop id=~ left=10 top=5]
[scetween type=in delay=100 param=alpha diff=-255]
AA
[@]
[rp]
B
[wt0]
[stop]
"#,
    );
    let step = |id| {
        runtime::step(
            handle,
            HostFrame {
                id,
                monotonic_ns: id * 1_000_000,
                utc_unix_ms: 0,
                utc_offset_minutes: 0,
            },
        )
        .unwrap()
    };
    for _ in 0..1024 {
        match step(1) {
            ScriptIoProgress::Waiting => {
                let mut bytes = [0; 4096];
                let polled = runtime::poll_events(handle, &mut bytes).unwrap();
                let request = decode_requests(&bytes[..polled.written_bytes])
                    .pop()
                    .unwrap();
                runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
            }
            ScriptIoProgress::Event => break,
            progress => panic!("unexpected text progress {progress:?}"),
        }
    }
    let key = |down| HostInput::Key { code: 13, down };
    runtime::submit_input(handle, epoch, &[key(true)]).unwrap();
    assert_eq!(step(2), ScriptIoProgress::Event);
    let first = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(first.sprites.len(), 2);
    assert_eq!(first.sprites[0].layer_id, "message");
    assert!(first.sprites[0].transform[4] >= 10.0);
    assert!(first.sprites[0].transform[5] >= 5.0);
    let atlas = first.texture(first.sprites[0].texture).unwrap();
    let coverage = |pixels: &[u8]| pixels.iter().skip(3).step_by(4).filter(|&&a| a > 0).count();
    let old_coverage = coverage(atlas.rgba());
    assert!(old_coverage > 0);
    assert_eq!(step(2), ScriptIoProgress::Event);
    assert!(
        runtime::acquire_latest(handle, first.render_frame_id)
            .unwrap()
            .is_none()
    );

    runtime::submit_input(handle, epoch, &[key(false), key(true)]).unwrap();
    assert_eq!(step(3), ScriptIoProgress::Event);
    assert_eq!(step(4), ScriptIoProgress::Event);
    let next = runtime::acquire_latest(handle, first.render_frame_id)
        .unwrap()
        .unwrap();
    assert_eq!(next.sprites.len(), 1);
    let next_atlas = next.texture(next.sprites[0].texture).unwrap();
    assert_ne!(atlas.revision, next_atlas.revision);
    assert!(coverage(next_atlas.rgba()) > old_coverage);
    assert_eq!(coverage(atlas.rgba()), old_coverage);
    drop(next);
    drop(first);
    runtime::destroy(handle).unwrap();
}

// Two synthetic rectangle glyphs so this test never consults a system font.
fn test_font() -> Vec<u8> {
    fn put16(data: &mut [u8], at: usize, value: u16) {
        data[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }
    let mut head = vec![0; 54];
    head[0..4].copy_from_slice(&[0, 1, 0, 0]);
    head[12..16].copy_from_slice(&[0x5f, 0x0f, 0x3c, 0xf5]);
    put16(&mut head, 18, 1000);
    let mut hhea = vec![0; 36];
    hhea[0..4].copy_from_slice(&[0, 1, 0, 0]);
    put16(&mut hhea, 4, 800);
    put16(&mut hhea, 6, (-200i16) as u16);
    put16(&mut hhea, 34, 3);
    let mut cmap = vec![0; 12 + 262];
    put16(&mut cmap, 2, 1);
    put16(&mut cmap, 6, 3);
    cmap[11] = 12;
    put16(&mut cmap, 14, 262);
    cmap[18 + b'A' as usize] = 1;
    cmap[18 + b'B' as usize] = 2;
    let glyph = [
        0, 1, 0, 0, 0, 0, 1, 244, 2, 188, 0, 3, 0, 0, 0x31, 0x21, 0x11, 0x21, 1, 244, 254, 12, 2,
        188,
    ];
    let tables = [
        (*b"cmap", cmap),
        (*b"glyf", glyph.repeat(2)),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", [2, 188, 0, 0].repeat(3)),
        (*b"loca", vec![0, 0, 0, 0, 0, 12, 0, 24]),
        (*b"maxp", vec![0, 0, 0x50, 0, 0, 3]),
    ];
    let mut font = vec![0; 12 + 16 * tables.len()];
    font[0..4].copy_from_slice(&[0, 1, 0, 0]);
    put16(&mut font, 4, tables.len() as u16);
    for (index, (name, table)) in tables.into_iter().enumerate() {
        let at = 12 + 16 * index;
        let offset = (font.len() as u32).to_be_bytes();
        font[at..at + 4].copy_from_slice(&name);
        font[at + 8..at + 12].copy_from_slice(&offset);
        font[at + 12..at + 16].copy_from_slice(&(table.len() as u32).to_be_bytes());
        font.extend_from_slice(&table);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    font
}

#[test]
fn layer_edits_compose_preloaded_pixels_and_do_not_mutate_leased_frames() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    let base = [0, 50, 200, 255, 71, 24, 8, 255];
    let overlay = [8, 30, 60, 255];
    host.asset("base.png", &png_bytes(2, 1, &base));
    host.asset("overlay.png", &png_bytes(1, 1, &overlay));
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=layer file=base]
[lyedit id=layer mode=negative]
[flip]
[lyedit id=layer mode=blend file=overlay left=1]
[flip]
[lyc id=layer file=base]
[flip]
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
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let drive = |host: &mut MemoryHost| {
        for _ in 0..1024 {
            let mut bytes = [0; 4096];
            let progress = runtime::step(handle, frame).unwrap();
            let polled = runtime::poll_events(handle, &mut bytes).unwrap();
            match progress {
                ScriptIoProgress::Event => return,
                ScriptIoProgress::Waiting => {
                    let request = decode_requests(&bytes[..polled.written_bytes])
                        .pop()
                        .unwrap();
                    runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
                }
                other => panic!("unexpected layer edit progress {other:?}"),
            }
        }
        panic!("layer edit fixture did not settle");
    };
    drive(&mut host);
    let negative = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    let first_texture = negative.texture(negative.sprites[0].texture).unwrap();
    let expected = [255, 205, 55, 255, 184, 231, 247, 255];
    assert_eq!(first_texture.rgba(), expected);
    drive(&mut host);
    let blended = runtime::acquire_latest(handle, negative.render_frame_id)
        .unwrap()
        .unwrap();
    let blended_texture = blended.texture(blended.sprites[0].texture).unwrap();
    assert_eq!(blended_texture.rgba(), &[255, 205, 55, 255, 8, 30, 60, 255]);
    assert_ne!(blended_texture.revision, first_texture.revision);
    assert!(host.reads.contains(&("overlay.png".into(), 0)));
    let previous = blended.render_frame_id;
    drop(blended);
    drive(&mut host);
    let rebuilt = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    assert_eq!(
        rebuilt.texture(rebuilt.sprites[0].texture).unwrap().rgba(),
        base
    );
    assert_eq!(first_texture.rgba(), expected);
    drop(rebuilt);
    drop(negative);
    runtime::destroy(handle).unwrap();
}

#[test]
fn transitions_keep_both_scenes_and_obey_time_and_input_policy() {
    use crate::host::{input::HostInput, runtime};

    let mut host = MemoryHost::default();
    let rule_pixels = [64, 64, 64, 255];
    host.asset("rule.png", &png_bytes(1, 1, &rule_pixels));
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=1 width=2 height=1 color=ff112233]
[flip]
[lyc id=1 width=2 height=1 color=ff445566]
[trans type=1 time=10 input=0]
[flip]
[lyc id=1 width=2 height=1 color=ff778899]
[trans type=2 rule=rule vague=40 time=100 input=1]
[stop]
"#,
    );
    let handle = runtime::create(0, TransportLimits::default(), 4096).unwrap();
    let epoch = runtime::open_project(
        handle,
        b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
        "windows",
        None,
    )
    .unwrap();
    let step = |id, millis: u64| {
        runtime::step(
            handle,
            HostFrame {
                id,
                monotonic_ns: millis * 1_000_000,
                utc_unix_ms: 0,
                utc_offset_minutes: 0,
            },
        )
        .unwrap()
    };
    for _ in 0..1024 {
        let progress = step(1, 0);
        let mut bytes = [0; 4096];
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        if progress == ScriptIoProgress::Event {
            break;
        }
        assert_eq!(progress, ScriptIoProgress::Waiting);
        for request in decode_requests(&bytes[..polled.written_bytes]) {
            runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        }
    }
    let old = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    let old_pixel = [0x11, 0x22, 0x33, 0xff];
    assert_eq!(
        old.texture(old.sprites[0].texture).unwrap().rgba(),
        old_pixel
    );
    assert_eq!(step(2, 15), ScriptIoProgress::Event);
    let start = runtime::acquire_latest(handle, old.render_frame_id)
        .unwrap()
        .unwrap();
    let transition = start.transition.as_ref().unwrap();
    assert_eq!(transition.progress, 0.0);
    assert_eq!(transition.rule, None);
    assert_eq!(
        start.texture(transition.source[0].texture).unwrap().rgba(),
        old_pixel
    );
    assert_eq!(
        start.texture(start.sprites[0].texture).unwrap().rgba(),
        &[0x44, 0x55, 0x66, 0xff]
    );
    let packet = start.packet();
    let op = 56 + start.textures().len() * 24;
    assert_eq!(
        u32::from_le_bytes(packet[op..op + 4].try_into().unwrap()),
        24
    );
    assert_eq!(
        u16::from_le_bytes(packet[op + 4..op + 6].try_into().unwrap()),
        2
    );
    assert_eq!(
        u32::from_le_bytes(packet[op + 8..op + 12].try_into().unwrap()),
        1
    );
    let previous = start.render_frame_id;
    drop(start);
    assert_eq!(step(2, 15), ScriptIoProgress::Event);
    assert!(runtime::acquire_latest(handle, previous).unwrap().is_none());

    let key = |down| HostInput::Key { code: 13, down };
    runtime::submit_input(handle, epoch, &[key(true)]).unwrap();
    assert_eq!(step(3, 20), ScriptIoProgress::Event);
    let mid = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    assert_eq!(mid.transition.as_ref().unwrap().progress, 0.5);
    let previous = mid.render_frame_id;
    drop(mid);

    assert_eq!(step(4, 25), ScriptIoProgress::Event);
    let finished = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    assert!(finished.transition.is_none());
    let previous = finished.render_frame_id;
    drop(finished);
    assert_eq!(step(4, 25), ScriptIoProgress::Waiting);
    assert_eq!(step(5, 26), ScriptIoProgress::WouldBlock);
    for _ in 0..1024 {
        let mut bytes = [0; 4096];
        let polled = runtime::poll_events(handle, &mut bytes).unwrap();
        let request = decode_requests(&bytes[..polled.written_bytes])
            .pop()
            .unwrap();
        assert_eq!(request.metadata["key"], "rule.png");
        runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        if step(4, 25) == ScriptIoProgress::Event {
            break;
        }
    }
    let next = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    let plan = next.transition.as_ref().unwrap();
    assert_eq!(plan.progress, 0.0);
    assert_eq!(plan.vague, 40.0 / 255.0);
    assert_eq!(
        next.texture(plan.rule.unwrap()).unwrap().rgba(),
        rule_pixels
    );
    let op = 56 + next.textures().len() * 24;
    let packet = next.packet();
    assert_eq!(
        u32::from_le_bytes(packet[op + 16..op + 20].try_into().unwrap()),
        plan.rule.unwrap() as u32
    );
    let previous = next.render_frame_id;
    drop(next);
    runtime::submit_input(handle, epoch, &[key(false), key(true)]).unwrap();
    assert_eq!(step(5, 26), ScriptIoProgress::Event);
    let skipped = runtime::acquire_latest(handle, previous).unwrap().unwrap();
    assert!(skipped.transition.is_none());
    assert_eq!(
        skipped.texture(skipped.sprites[0].texture).unwrap().rgba(),
        &[0x77, 0x88, 0x99, 0xff]
    );
    assert_eq!(
        old.texture(old.sprites[0].texture).unwrap().rgba(),
        old_pixel
    );
    drop(skipped);
    drop(old);
    runtime::destroy(handle).unwrap();
}

#[test]
fn hlsl_source_replacement_keeps_old_frames_and_prefetches_shader_bindings() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    let first_source = b"first shader";
    let second_source = b"second shader";
    let mask = [255, 128, 0, 64];
    let user = [0, 128, 255, 255];
    host.asset("first.hlsl", first_source);
    host.asset("second.hlsl", second_source);
    host.asset("mask.png", &png_bytes(1, 1, &mask));
    host.asset("user.png", &png_bytes(1, 1, &user));
    host.asset("boot.iet", br#"*top
[lyshader id=test file=first.hlsl]
[lyc id=tile width=4 height=4 color=ffffff]
[lyprop id=tile shader=test shaderconstant=tint shadertexture=textureUser tint="0.25,0.5,1" mask=mask textureUser=user]
[flip]
[lyshader id=test file=second.hlsl]
[trans type=1 time=50]
[stop]
"#);
    session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    let mut advance = |session: &mut ScriptIoSession, id| {
        let frame = HostFrame {
            id,
            monotonic_ns: id * 1_000_000,
            utc_unix_ms: 0,
            utc_offset_minutes: 0,
        };
        for _ in 0..1536 {
            let progress = session.step_queued_frame(frame).unwrap();
            session.publish_frame(progress).unwrap();
            match progress {
                ScriptIoProgress::Event => return,
                ScriptIoProgress::Waiting => {}
                progress => panic!("unexpected shader progress: {progress:?}"),
            }
            for request in requests(session) {
                session.submit_replies(&[host.complete(&request)]).unwrap();
            }
        }
        panic!("shader scenario did not advance");
    };
    advance(&mut session, 1);
    let old = session.frames.latest_snapshot().unwrap();
    let effect = old.sprites[0].shader.as_ref().unwrap();
    assert_eq!(&*effect.shader.source, first_source);
    assert_eq!(effect.uniforms["tint"], [0.25, 0.5, 1.0]);
    assert_eq!(old.texture(effect.mask.unwrap()).unwrap().rgba(), mask);
    assert_eq!(old.texture(effect.user.unwrap()).unwrap().rgba(), user);
    advance(&mut session, 2);
    let frame = session
        .frames
        .acquire(
            crate::host::runtime::RuntimeHandle::from_raw(1),
            old.render_frame_id,
        )
        .unwrap()
        .unwrap();
    let new_effect = frame.sprites[0].shader.as_ref().unwrap();
    assert_eq!(&*new_effect.shader.source, second_source);
    let old_effect = frame.transition.as_ref().unwrap().source[0]
        .shader
        .as_ref()
        .unwrap();
    assert_eq!(old_effect.shader.revision, effect.shader.revision);
    assert_ne!(new_effect.shader.revision, effect.shader.revision);
    assert_eq!(
        frame.texture(old_effect.user.unwrap()).unwrap().rgba(),
        user
    );
    let packet = frame.packet();
    let mut records = &packet[56 + frame.textures().len() * 24..];
    let mut programs = Vec::new();
    let mut bindings = Vec::new();
    while !records.is_empty() {
        let length = u32::from_le_bytes(records[..4].try_into().unwrap()) as usize;
        match u16::from_le_bytes(records[4..6].try_into().unwrap()) {
            6 => programs.push(&records[16..length]),
            7 => bindings.push(records[6] & 16 != 0),
            _ => {}
        }
        records = &records[length..];
    }
    assert_eq!(programs, [&first_source[..], &second_source[..]]);
    assert_eq!(bindings, [true, false]);
}

#[test]
fn emote_models_load_through_host_io_and_publish_the_cpu_atlas() {
    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    host.asset("models/bad.psb", b"not a PSB");
    host.asset("models/red.psb", include_bytes!("fixtures/emote-red.psb"));
    host.asset(
        "boot.iet",
        br#"*top
[calllua function=load_model]
[stop]
[lua]
function load_model(e)
    attempts = (attempts or 0) + 1
    e:setMagicPath{'model', 'models'}
    local ok = pcall(function()
        e:createEmoteLayer{id='model', files={':model/bad.psb'}, width=4, height=4}
    end)
    assert(not ok and e:getEmoteLayer{id='model'} == nil)
    local layer = e:createEmoteLayer{id='model', files={':model/red.psb'}, width=4, height=4}
    layer:setCoord(-2, -2, 0, 0)
    finished = true
end
[/lua]
"#,
    );
    session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n",
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
    let mut model_stats = 0;
    for _ in 0..4096 {
        let progress = session.step_queued_frame(frame).unwrap();
        session.publish_frame(progress).unwrap();
        match progress {
            ScriptIoProgress::Event => break,
            ScriptIoProgress::Waiting => {}
            progress => panic!("unexpected model load progress: {progress:?}"),
        }
        for request in requests(&mut session) {
            if request.identity.service == IO_STAT
                && request.metadata["key"]
                    .as_str()
                    .unwrap()
                    .starts_with("models/")
            {
                model_stats += 1;
            }
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
    }
    let globals = session.interpreter().lua().globals();
    assert!(globals.get::<bool>("finished").unwrap());
    assert_eq!(globals.get::<u32>("attempts").unwrap(), 1);
    assert_eq!(model_stats, 2);
    let frame = session.frames.latest_snapshot().unwrap();
    let sprite = &frame.sprites[0];
    assert_eq!(frame.sprites.len(), 1);
    assert_eq!(sprite.layer_id, "model");
    assert_eq!(&sprite.transform[4..], &[0.0, 0.0]);
    assert!(sprite.emote_material.is_some());
    assert!(
        frame
            .texture(sprite.texture)
            .unwrap()
            .rgba()
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255])
    );
}

#[test]
fn transition_preserves_the_old_scenes_nested_composite_groups() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    let mask_pixels = [255, 255, 255, 128];
    host.asset("mask.png", &png_bytes(1, 1, &mask_pixels));
    host.asset(
        "boot.iet",
        br#"*top
[lyprop id=group intermediate_render=1 intermediate_render_mask=mask alpha=128]
[lyprop id=group.inner intermediate_render=2 alpha=64]
[lyc id=group.inner.image width=2 height=1 color=ffffff]
[lyc id=group.image width=1 height=1 color=ffffff]
[flip]
[lydel id=group]
[lyc id=other width=2 height=1 color=ff0000]
[trans type=1 time=50]
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
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    for _ in 0..1024 {
        if runtime::step(handle, frame).unwrap() == ScriptIoProgress::Event {
            break;
        }
        let mut bytes = [0; 4096];
        let poll = runtime::poll_events(handle, &mut bytes).unwrap();
        for request in decode_requests(&bytes[..poll.written_bytes]) {
            runtime::submit_replies(handle, &[host.complete(&request)]).unwrap();
        }
    }
    let old = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(old.groups.len(), 2);
    assert_eq!(old.groups[0].sprites.len(), 1);
    assert!(old.groups[0].opaque);
    assert_eq!(old.groups[0].opacity, 64.0 / 255.0);
    assert_eq!(old.groups[1].sprites, 0..2);
    assert_eq!(old.groups[1].opacity, 128.0 / 255.0);
    assert_eq!(
        old.texture(old.groups[1].mask.unwrap()).unwrap().rgba(),
        mask_pixels
    );

    assert_eq!(
        runtime::step(handle, HostFrame { id: 2, ..frame }).unwrap(),
        ScriptIoProgress::Event
    );
    let frame = runtime::acquire_latest(handle, old.render_frame_id)
        .unwrap()
        .unwrap();
    assert!(frame.groups.is_empty());
    let transition = frame.transition.as_ref().unwrap();
    assert_eq!(transition.source_groups[1].sprites, 0..2);
    let mask = transition.source_groups[1].mask.unwrap();
    assert_eq!(frame.texture(mask).unwrap().rgba(), mask_pixels);
    let packet = frame.packet();
    let group = &packet[packet.len() - 56..];
    assert_eq!(u16::from_le_bytes(group[4..6].try_into().unwrap()), 3);
    assert_ne!(u16::from_le_bytes(group[6..8].try_into().unwrap()) & 16, 0);
    assert_eq!(
        u32::from_le_bytes(group[16..20].try_into().unwrap()),
        mask as u32
    );
    drop(frame);
    drop(old);
    runtime::destroy(handle).unwrap();
}

#[test]
fn screenshot_pins_the_gameplay_frame_and_uses_it_after_a_menu_opens() {
    use crate::host::runtime;

    let mut host = MemoryHost::default();
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=game width=2 height=1 color=ff112233]
[takess]
[lydel id=game]
[lyc id=menu width=2 height=1 color=ff445566]
[savess file=thumb width=1 height=1]
[save file=slot]
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
    let frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let step = || runtime::step(handle, frame).unwrap();
    let poll = || {
        let mut bytes = [0; 4096];
        let result = runtime::poll_events(handle, &mut bytes).unwrap();
        decode_requests(&bytes[..result.written_bytes])
    };
    let submit = |reply| runtime::submit_replies(handle, &[reply]);
    let capture = (0..1024)
        .find_map(|_| {
            assert_eq!(step(), ScriptIoProgress::Waiting);
            let request = poll().pop().unwrap();
            if request.identity.service == RENDER_CAPTURE {
                Some(request)
            } else {
                submit(host.complete(&request)).unwrap();
                None
            }
        })
        .expect("capture request");
    assert!(runtime::get_state(handle).unwrap().waiting_for_render);
    let old = runtime::acquire_latest(handle, 0).unwrap().unwrap();
    assert_eq!(
        capture.metadata["render_frame_id"],
        old.render_frame_id.to_string()
    );
    assert_eq!(old.sprites[0].layer_id, "game");
    assert_eq!(step(), ScriptIoProgress::Waiting);
    assert!(poll().is_empty());
    assert_eq!(
        runtime::step(handle, HostFrame { id: 2, ..frame }).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    assert!(matches!(
        submit(reply(&capture, json!({"status":"invalid"}), vec![])),
        Err(runtime::RuntimeError::Session(ScriptIoError::Protocol(
            ProtocolError::Argument(_)
        )))
    ));
    submit(reply(&capture, json!({"status":"ok"}), vec![])).unwrap();

    assert_eq!(step(), ScriptIoProgress::Waiting);
    let write = poll().pop().unwrap();
    assert_eq!(write.identity.service, RENDER_SAVE_CAPTURE);
    assert_eq!(
        write.metadata,
        json!({
            "capture_id": capture.identity.request_id.to_string(), "namespace": "save",
            "key": "thumb.png", "width": 1, "height": 1,
        })
    );
    submit(reply(&write, json!({"status":"ok"}), vec![])).unwrap();
    loop {
        match step() {
            ScriptIoProgress::Event => break,
            ScriptIoProgress::Waiting => {
                let request = poll().pop().unwrap();
                submit(host.complete(&request)).unwrap();
            }
            progress => panic!("unexpected save progress: {progress:?}"),
        }
    }
    let (_, bytes) = &host.files[&("save".into(), "slot".into())];
    let save: crate::save::SaveData = serde_json::from_slice(bytes).unwrap();
    let scene = save.scene.unwrap();
    assert!(scene.get("game").is_some());
    assert!(scene.get("menu").is_none());
    let menu = runtime::acquire_latest(handle, old.render_frame_id)
        .unwrap()
        .unwrap();
    assert_eq!(menu.sprites[0].layer_id, "menu");
    drop(menu);
    drop(old);
    runtime::destroy(handle).unwrap();
}

#[test]
fn pointer_filters_resume_once_and_only_unclaimed_clicks_advance_the_story() {
    use crate::host::input::HostInput;
    use asb_interpreter::WaitReason;

    let mut session = session(1, TransportLimits::default(), 4096);
    let mut host = MemoryHost::default();
    host.asset("gate", b"");
    host.asset("button.png", &png_bytes(2, 1, &[0, 0, 0, 0, 0, 0, 0, 255]));
    host.asset(
        "boot.iet",
        br#"*top
[lyc id=1.10 width=2 height=1 color=ffffff]
[lyc id=1.20 file=button]
[lyprop id=1.20 clickablethreshold=128]
[lyevent id=1.10 type=click handler=calllua function=bottom]
[lyevent id=1.20 type=click penetration=1 handler=calllua function=top]
[lyevent id=1.20 type=rollover handler=calllua function=enter]
[lyevent id=1.20 type=rollout handler=calllua function=leave]
[wt0]
[calllua function=advanced]
[stop]
[lua]
local function emit(s) trace=(trace or '')..s end
function bottom() emit('b') end
function top() emit('t') end
function enter() emit('+') end
function leave() emit('-') end
function advanced() emit('a') end
__engine:setEventFilter(function(e,name,p)
    assert(name=='lyevent' and p.click==nil)
    if p.type=='click' and p.id=='1.20' then
        attempts=(attempts or 0)+1
        assert(e:file('gate')=='')
    elseif p.type=='click' and reject_bottom then
        return 2
    end
    return 0
end)
[/lua]
"#,
    );
    let epoch = session
        .open_queued_project(
            b"[WINDOWS]\nWIDTH=320\nHEIGHT=200\nBOOT=boot.iet\n",
            "windows",
            None,
        )
        .unwrap();
    let frame = |id| HostFrame {
        id,
        monotonic_ns: id * 1_000_000,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    let waiting_for_click = |session: &ScriptIoSession| {
        matches!(
            session.pending_event(),
            Some(Event::Wait {
                reason: WaitReason::Generic0
            })
        )
    };
    for _ in 0..1024 {
        if session.step_queued_frame(frame(1)).unwrap() == ScriptIoProgress::Event {
            break;
        }
        for request in requests(&mut session) {
            session.submit_replies(&[host.complete(&request)]).unwrap();
        }
    }
    assert!(waiting_for_click(&session));
    let globals = session.interpreter().lua().globals();
    let key = |down| HostInput::Key { code: 1, down };
    session
        .submit_input(epoch, &[HostInput::Pointer { x: 1, y: 0 }, key(true)])
        .unwrap();
    assert_eq!(
        session.step_queued_frame(frame(2)).unwrap(),
        ScriptIoProgress::Waiting
    );
    let request = requests(&mut session).pop().unwrap();
    assert_eq!(request.metadata["key"], "gate");
    assert_eq!(
        session.step_queued_frame(frame(3)).unwrap(),
        ScriptIoProgress::WouldBlock
    );
    session.submit_replies(&[host.complete(&request)]).unwrap();
    for _ in 0..2 {
        assert_eq!(
            session.step_queued_frame(frame(2)).unwrap(),
            ScriptIoProgress::Event
        );
        assert!(waiting_for_click(&session));
    }
    assert_eq!(globals.get::<String>("trace").unwrap(), "+bt");
    assert_eq!(globals.get::<u32>("attempts").unwrap(), 1);

    globals.set("reject_bottom", true).unwrap();
    session
        .submit_input(
            epoch,
            &[HostInput::Pointer { x: 0, y: 0 }, key(false), key(true)],
        )
        .unwrap();
    assert_eq!(
        session.step_queued_frame(frame(3)).unwrap(),
        ScriptIoProgress::Event
    );
    assert_eq!(globals.get::<String>("trace").unwrap(), "+bt-a");
    assert_eq!(globals.get::<u32>("attempts").unwrap(), 1);
    assert!(matches!(
        session.pending_event(),
        Some(Event::Wait {
            reason: WaitReason::Stop { .. }
        })
    ));
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
fn numbered_save_restores_scene_text_and_audio_in_a_fresh_runtime() {
    use crate::host::{
        media::PlaybackStatus,
        runtime::{self, RuntimeError},
    };

    let mut host = MemoryHost::default();
    let pixels = [20, 40, 60, 190];
    let mask = [128, 128, 128, 255];
    host.asset("hero.png", &png_bytes(1, 1, &pixels));
    host.asset("mask.png", &png_bytes(1, 1, &mask));
    host.asset("font/fixture-regular.ttf", &test_font());
    host.asset(
        "store.iet",
        br#"*top
[splay file=theme_a.ogg loop=1 gain=420]
[sfade gain=480 time=10]
[span pan=-40]
[voice id=line file=voice.ogg]
[lyc id=1.80 file=hero mask=mask]
[lytween id=1.80 param=left from=0 to=20 time=10 ease=linear]
[chgmsg id=dialog layered=1]
[font face=font/fixture-medium.ttf]
AB
[flip]
[wait time=4 input=0]
[save file=slot]
[stop]
"#,
    );
    host.asset(
        "load.iet",
        br#"*top
[splay file=old.ogg]
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
    let drive = |handle, frame, host: &mut MemoryHost, cancel_key: Option<&str>| {
        let mut audio = Vec::new();
        for _ in 0..8192 {
            let progress = runtime::step(handle, frame).unwrap();
            let mut bytes = [0; 8192];
            let polled = runtime::poll_events(handle, &mut bytes).unwrap();
            let mut records = &bytes[..polled.written_bytes];
            while !records.is_empty() {
                let len = u32::from_le_bytes(records[0..4].try_into().unwrap()) as usize;
                let domain = u16::from_le_bytes(records[6..8].try_into().unwrap());
                if domain == Domain::Media as u16 {
                    assert_eq!(
                        u32::from_le_bytes(records[8..12].try_into().unwrap()),
                        MEDIA_AUDIO_COMMAND.kind
                    );
                    audio.push(serde_json::from_slice::<Value>(&records[64..len]).unwrap());
                }
                records = &records[len..];
            }
            if progress == ScriptIoProgress::Event {
                return audio;
            }
            assert_eq!(progress, ScriptIoProgress::Waiting);
            for request in decode_requests(&bytes[..polled.written_bytes]) {
                let response = if cancel_key.is_some_and(|key| request.metadata["key"] == key) {
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
    drive(saver, start, &mut host, None);
    let page = runtime::acquire_latest(saver, 0).unwrap().unwrap();
    assert_eq!(
        page.sprites
            .iter()
            .filter(|sprite| sprite.layer_id == "dialog")
            .count(),
        2
    );
    drop(page);
    drive(saver, start, &mut host, None);
    let save_time = HostFrame {
        id: 2,
        monotonic_ns: 4_000_000,
        ..start
    };
    drive(saver, save_time, &mut host, None);
    let saved = &host.files[&("save".into(), "slot".into())].1;
    let saved: crate::save::SaveData = serde_json::from_slice(saved).unwrap();
    let snapshot = saved.audio.unwrap();
    assert_eq!(snapshot.bgm.as_ref().unwrap().gain, 480);
    assert_eq!(snapshot.bgm.as_ref().unwrap().pan, -40);
    assert_eq!(snapshot.voice[0].id, "line");
    let text = saved.message_text.unwrap();
    assert_eq!(text.active_layer.as_deref(), Some("dialog"));
    assert!(
        text.layers[0]
            .tags
            .contains(&crate::text::BacklogTag::Text("AB".into()))
    );
    runtime::destroy(saver).unwrap();

    for cancel_key in [Some("mask.png"), Some("font/fixture-regular.ttf"), None] {
        host.reads.clear();
        let loader = create("load.iet");
        let initial = drive(loader, start, &mut host, None);
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0]["key"], "old.ogg");
        let playback = |value: &Value| {
            value["playback_id"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        let old_id = playback(&initial[0]);
        let epoch = runtime::get_state(loader).unwrap().transport.epoch;
        let original = runtime::acquire_latest(loader, 0).unwrap().unwrap();
        assert_eq!(original.sprites[0].layer_id, "99");
        let audio = drive(loader, start, &mut host, cancel_key);
        assert!(host.reads.contains(&("hero.png".into(), 0)));
        if cancel_key.is_some() {
            assert!(audio.is_empty());
            runtime::submit_playback_status(loader, epoch, old_id, PlaybackStatus::Started)
                .unwrap();
            assert!(
                runtime::acquire_latest(loader, original.render_frame_id)
                    .unwrap()
                    .is_none()
            );
        } else {
            assert_eq!(audio.len(), 1);
            assert_eq!(audio[0]["op"], "restore");
            let plays = audio[0]["plays"].as_array().unwrap();
            assert_eq!(plays.len(), 2);
            assert_eq!(plays[0]["key"], "theme_a.ogg");
            assert_eq!(plays[0]["loop_key"], "theme_b.ogg");
            assert_eq!(plays[0]["gain"], 480);
            assert_eq!(plays[0]["pan"], -40);
            assert_eq!(plays[1]["key"], "voice.ogg");
            assert_ne!(playback(&plays[0]), old_id);
            assert!(matches!(
                runtime::submit_playback_status(loader, epoch, old_id, PlaybackStatus::Ended),
                Err(RuntimeError::Session(ScriptIoError::Protocol(
                    ProtocolError::Stale
                )))
            ));
            runtime::submit_playback_status(
                loader,
                epoch,
                playback(&plays[0]),
                PlaybackStatus::Started,
            )
            .unwrap();
            assert!(host.reads.contains(&("mask.png".into(), 0)));
            let restored = runtime::acquire_latest(loader, original.render_frame_id)
                .unwrap()
                .unwrap();
            assert!(host.reads.contains(&("font/fixture-regular.ttf".into(), 0)));
            let restored_text: Vec<_> = restored
                .sprites
                .iter()
                .filter(|sprite| sprite.layer_id == "dialog")
                .collect();
            assert_eq!(restored_text.len(), 2);
            assert!(
                restored
                    .texture(restored_text[0].texture)
                    .unwrap()
                    .rgba()
                    .iter()
                    .skip(3)
                    .step_by(4)
                    .any(|&alpha| alpha != 0)
            );
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
            drive(loader, finished, &mut host, None);
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
