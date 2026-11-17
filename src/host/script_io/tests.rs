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
            assert_eq!(
                u16::from_le_bytes(bytes[offset + 6..offset + 8].try_into().unwrap()),
                Domain::Io as u16
            );
            let metadata_bytes = u32_at(56) as usize;
            let blob_start = offset + 64 + metadata_bytes;
            requests.push(Request {
                identity: RequestIdentity {
                    runtime: u64_at(16),
                    epoch: u64_at(24),
                    request_id: u64_at(40),
                    service: MessageKind {
                        domain: Domain::Io,
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
