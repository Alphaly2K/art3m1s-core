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
                    json!({"status":"ok","found":true,"size":bytes.len().to_string(),"revision":revision.to_string()}),
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
            if session.step().unwrap() == ScriptIoProgress::Completed {
                return;
            }
            for request in requests(session) {
                session.submit_replies(&[self.complete(&request)]).unwrap();
            }
        }
        panic!("IO script did not finish");
    }
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
        local ok = pcall(function() __engine:tag{'var',system='file_exists',name='t.x',file='/'} end)
        assert(not ok)
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
