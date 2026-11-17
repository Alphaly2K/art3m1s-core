use super::*;

const READ: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 2,
};
const DIALOG: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 1,
};

fn body(blob_bytes: usize) -> MessageBody {
    MessageBody::new("{}".into(), vec![0xA5; blob_bytes]).unwrap()
}

fn transport(runtime: u64, limits: TransportLimits) -> SessionTransport {
    let mut transport = SessionTransport::new(runtime, limits).unwrap();
    transport.begin_session().unwrap();
    transport.poll_events(&mut [0; 128]);
    transport
}

fn drain(transport: &mut SessionTransport) -> Vec<u8> {
    let state = transport.state();
    let mut output = vec![0; state.control.bytes + state.diagnostics.bytes];
    let result = transport.poll_events(&mut output);
    assert_eq!(result.written_bytes, output.len());
    assert_eq!(result.required_next_bytes, 0);
    output
}

fn reply(identity: RequestIdentity) -> HostReply {
    HostReply {
        identity,
        body: body(0),
    }
}

#[test]
fn records_encode_the_portable_envelope_and_raw_blob() {
    let runtime = 0x8877_6655_4433_2211;
    let mut t = transport(runtime, TransportLimits::default());
    let metadata = r#"{"offset":"18446744073709551615","key":"画像.png"}"#;
    let identity = t
        .request(
            READ,
            MessageBody::new(metadata.into(), vec![0, 255, 128]).unwrap(),
        )
        .unwrap();
    let bytes = drain(&mut t);
    let size = HEADER_BYTES + 8 + metadata.len() + 3;
    let mut expected = Vec::new();
    expected.extend_from_slice(&(size as u32).to_le_bytes());
    expected.extend_from_slice(&[2, 0, 3, 0]);
    expected.extend_from_slice(&2u32.to_le_bytes());
    expected.extend_from_slice(&0x103u32.to_le_bytes());
    expected.extend_from_slice(&runtime.to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.extend_from_slice(&2u64.to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.extend_from_slice(&((size - 56) as u32).to_le_bytes());
    expected.extend_from_slice(&[0; 4]);
    expected.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    expected.extend_from_slice(&3u32.to_le_bytes());
    expected.extend_from_slice(metadata.as_bytes());
    expected.extend_from_slice(&[0, 255, 128]);
    assert_eq!(bytes, expected);
    assert_eq!(
        identity,
        RequestIdentity {
            runtime,
            epoch: 1,
            request_id: 1,
            service: READ
        }
    );
}

#[test]
fn poll_reports_required_capacity_without_losing_records() {
    let mut t = SessionTransport::new(1, TransportLimits::default()).unwrap();
    t.begin_session().unwrap();
    t.request(READ, body(5)).unwrap();
    let before = t.state();
    let mut short = [0xFF; 65];
    assert_eq!(
        t.poll_events(&mut short),
        PollResult {
            status: PollStatus::Buffer,
            record_count: 0,
            written_bytes: 0,
            required_next_bytes: 66,
        }
    );
    assert_eq!(short, [0xFF; 65]);
    assert_eq!(t.state(), before);
    let mut partial = [0xFF; 100];
    assert_eq!(
        t.poll_events(&mut partial),
        PollResult {
            status: PollStatus::Ok,
            record_count: 1,
            written_bytes: 66,
            required_next_bytes: 71,
        }
    );
    assert!(partial[66..].iter().all(|byte| *byte == 0xFF));
    assert_eq!(t.poll_events(&mut []).status, PollStatus::Buffer);
    assert_eq!(drain(&mut t).len(), 71);
    assert_eq!(
        t.poll_events(&mut []),
        PollResult {
            status: PollStatus::NoData,
            record_count: 0,
            written_bytes: 0,
            required_next_bytes: 0,
        }
    );
}

#[test]
fn control_and_diagnostics_have_independent_record_and_byte_budgets() {
    let limits = TransportLimits {
        control: QueueBudget {
            records: 2,
            bytes: 150,
        },
        diagnostics: QueueBudget {
            records: 1,
            bytes: 80,
        },
        ..TransportLimits::default()
    };
    let mut t = transport(1, limits);
    assert_eq!(t.diagnostic(1, body(0)), Ok(DiagnosticOutcome::Queued));
    assert_eq!(t.diagnostic(1, body(0)), Ok(DiagnosticOutcome::Dropped));
    let first = t.request(READ, body(0)).unwrap();
    let second = t.request(DIALOG, body(0)).unwrap();
    let full = t.state();
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::WouldBlock));
    assert_eq!(t.state(), full);
    let mut control = [0; 132];
    let poll = t.poll_events(&mut control);
    assert_eq!((poll.record_count, poll.required_next_bytes), (2, 66));
    assert_eq!(
        u16::from_le_bytes(control[6..8].try_into().unwrap()),
        Domain::Io as u16
    );
    let third = t.request(READ, body(30)).unwrap();
    assert_eq!(third.request_id, second.request_id + 1);
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::WouldBlock));
    assert_eq!(t.request(READ, body(100)), Err(ProtocolError::Limit));
    assert_eq!(t.state().control.bytes, 96);
    assert_eq!(t.state().dropped_diagnostics, 1);
    drain(&mut t);
    assert_eq!(t.diagnostic(1, body(20)), Ok(DiagnosticOutcome::Dropped));
    assert_eq!(t.state().dropped_diagnostics, 2);
    t.submit_replies(&[reply(first), reply(second), reply(third)])
        .unwrap();
}

#[test]
fn reply_batches_reject_foreign_wrong_kind_duplicate_and_undelivered_requests_atomically() {
    let mut a = transport(1, TransportLimits::default());
    let mut b = transport(2, TransportLimits::default());
    let one = a.request(READ, body(0)).unwrap();
    let two = a.request(DIALOG, body(0)).unwrap();
    let other = b.request(READ, body(0)).unwrap();
    assert_eq!(a.submit_replies(&[reply(one)]), Err(ProtocolError::State));
    drain(&mut a);
    drain(&mut b);
    for invalid in [
        other,
        RequestIdentity {
            service: DIALOG,
            ..one
        },
        RequestIdentity {
            request_id: 0,
            ..one
        },
    ] {
        let before = a.state();
        assert_eq!(
            a.submit_replies(&[reply(two), reply(invalid)]),
            Err(ProtocolError::Stale)
        );
        assert_eq!(a.state(), before);
    }
    let before = a.state();
    assert_eq!(
        a.submit_replies(&[reply(one), reply(one)]),
        Err(ProtocolError::Stale)
    );
    assert_eq!(a.state(), before);
    let mut submitted = vec![reply(two), reply(one)];
    a.submit_replies(&submitted).unwrap();
    submitted[0].body = body(7);
    assert_eq!(a.state().pending_requests, 0);
    assert_eq!(a.submit_replies(&[reply(one)]), Err(ProtocolError::Stale));
    assert_eq!(a.pop_reply(), Some(reply(two)));
    assert_eq!(a.pop_reply(), Some(reply(one)));
    assert_eq!(a.pop_reply(), None);
    b.submit_replies(&[reply(other)]).unwrap();
}

#[test]
fn reply_capacity_retries_preserve_pending_requests_and_batch_order() {
    let limits = TransportLimits {
        replies: QueueBudget {
            records: 2,
            bytes: 140,
        },
        batch_messages: 2,
        pending_requests: 3,
        ..TransportLimits::default()
    };
    let mut t = transport(1, limits);
    let ids: Vec<_> = (0..3).map(|_| t.request(READ, body(0)).unwrap()).collect();
    drain(&mut t);
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::WouldBlock));
    let original = t.state();
    assert_eq!(
        t.submit_replies(&ids.iter().copied().map(reply).collect::<Vec<_>>()),
        Err(ProtocolError::Limit)
    );
    let too_large = HostReply {
        identity: ids[0],
        body: body(100),
    };
    assert_eq!(t.submit_replies(&[too_large]), Err(ProtocolError::Limit));
    assert_eq!(t.state(), original);
    let first = reply(ids[0]);
    t.submit_replies(&[first.clone()]).unwrap();
    let before = t.state();
    let byte_limited = HostReply {
        identity: ids[1],
        body: body(10),
    };
    assert_eq!(
        t.submit_replies(&[byte_limited]),
        Err(ProtocolError::WouldBlock)
    );
    assert_eq!(t.state(), before);
    assert_eq!(
        t.submit_replies(&[reply(ids[1]), reply(ids[2])]),
        Err(ProtocolError::WouldBlock)
    );
    assert_eq!(t.state(), before);
    assert_eq!(t.pop_reply(), Some(first));
    t.submit_replies(&[reply(ids[2]), reply(ids[1])]).unwrap();
    assert_eq!(t.pop_reply().unwrap().identity, ids[2]);
    assert_eq!(t.pop_reply().unwrap().identity, ids[1]);
    assert_eq!(t.state().replies.bytes, 0);
}

#[test]
fn reload_invalidates_old_work_and_starts_with_a_new_session_record() {
    let mut limits = TransportLimits::default();
    limits.control.records = 2;
    limits.diagnostics.records = 1;
    let mut t = transport(1, limits);
    let accepted = t.request(READ, body(0)).unwrap();
    let delivered = t.request(DIALOG, body(0)).unwrap();
    drain(&mut t);
    t.submit_replies(&[reply(accepted)]).unwrap();
    let queued = t.request(READ, body(0)).unwrap();
    t.notify(
        MessageKind {
            domain: Domain::Media,
            kind: 7,
        },
        body(0),
    )
    .unwrap();
    t.diagnostic(1, body(0)).unwrap();
    t.diagnostic(1, body(0)).unwrap();
    let last_sequence = t.last_sequence;
    assert_eq!(t.begin_session(), Ok(2));
    assert_eq!(t.state().pending_requests, 0);
    assert_eq!(t.state().diagnostics.records, 0);
    assert_eq!(t.state().dropped_diagnostics, 1);
    assert_eq!(t.pop_reply(), None);
    let new = t.request(READ, body(0)).unwrap();
    assert!(new.request_id > queued.request_id);
    assert_eq!(new.epoch, 2);
    for old in [accepted, delivered, queued] {
        assert_eq!(t.submit_replies(&[reply(old)]), Err(ProtocolError::Stale));
    }
    let records = drain(&mut t);
    assert_eq!(
        u16::from_le_bytes(records[6..8].try_into().unwrap()),
        Domain::Runtime as u16
    );
    assert_eq!(
        u32::from_le_bytes(records[8..12].try_into().unwrap()),
        SESSION_CHANGED.kind
    );
    assert_eq!(u64::from_le_bytes(records[24..32].try_into().unwrap()), 2);
    assert_eq!(
        u64::from_le_bytes(records[32..40].try_into().unwrap()),
        last_sequence + 1
    );
    t.submit_replies(&[reply(new)]).unwrap();
}

#[test]
fn cancellation_removes_undelivered_requests_and_rejects_late_replies() {
    let mut t = transport(1, TransportLimits::default());
    let before_poll = t.request(READ, body(0)).unwrap();
    let after_poll = t.request(READ, body(0)).unwrap();
    t.notify(
        MessageKind {
            domain: Domain::Media,
            kind: 7,
        },
        body(0),
    )
    .unwrap();
    t.cancel(before_poll).unwrap();
    assert_eq!(
        t.state().control,
        QueueBudget {
            records: 2,
            bytes: 132
        }
    );
    let records = drain(&mut t);
    assert_eq!(
        u64::from_le_bytes(records[40..48].try_into().unwrap()),
        after_poll.request_id
    );
    assert_eq!(u64::from_le_bytes(records[106..114].try_into().unwrap()), 0);
    t.cancel(after_poll).unwrap();
    assert_eq!(t.state().pending_requests, 0);
    for id in [before_poll, after_poll] {
        assert_eq!(t.submit_replies(&[reply(id)]), Err(ProtocolError::Stale));
        assert_eq!(t.cancel(id), Err(ProtocolError::Stale));
    }
    assert!(t.request(READ, body(0)).unwrap().request_id > after_poll.request_id);
}

#[test]
fn exhausted_counters_never_wrap_or_partially_reset_a_session() {
    let mut t = transport(1, TransportLimits::default());
    t.last_request_id = u64::MAX;
    let before = t.state();
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::Limit));
    assert_eq!(t.state(), before);
    t.last_request_id = 0;
    t.last_sequence = u64::MAX;
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::Limit));
    assert_eq!(t.begin_session(), Err(ProtocolError::Limit));
    assert_eq!(t.state(), before);
    t.last_sequence = 1;
    t.epoch = u64::MAX;
    let pending = t.request(READ, body(0)).unwrap();
    let before = t.state();
    assert_eq!(t.begin_session(), Err(ProtocolError::Limit));
    assert_eq!(t.state(), before);
    t.cancel(pending).unwrap();
}

#[test]
fn malformed_bodies_limits_and_reserved_kinds_are_rejected() {
    for metadata in ["", "null", "[]", "true", "{", "{\"v\":NaN}"] {
        assert!(MessageBody::new(metadata.into(), Vec::new()).is_err());
    }
    assert!(SessionTransport::new(0, TransportLimits::default()).is_err());
    let mut limits = TransportLimits::default();
    limits.control.bytes = 65;
    assert!(SessionTransport::new(1, limits).is_err());
    limits = TransportLimits::default();
    limits.pending_requests = 0;
    assert!(SessionTransport::new(1, limits).is_err());
    let mut t = SessionTransport::new(1, TransportLimits::default()).unwrap();
    assert_eq!(t.request(READ, body(0)), Err(ProtocolError::State));
    t.begin_session().unwrap();
    let before = t.state();
    assert!(matches!(
        t.notify(SESSION_CHANGED, body(0)),
        Err(ProtocolError::Argument(_))
    ));
    assert!(matches!(
        t.diagnostic(0, body(0)),
        Err(ProtocolError::Argument(_))
    ));
    assert!(matches!(
        t.request(SESSION_CHANGED, body(0)),
        Err(ProtocolError::Argument(_))
    ));
    assert_eq!(t.state(), before);
    let mut limits = TransportLimits::default();
    limits.blob_bytes = 1;
    limits.metadata_bytes = 2;
    let mut t = transport(1, limits);
    assert_eq!(t.request(READ, body(2)), Err(ProtocolError::Limit));
    assert_eq!(
        t.notify(
            READ,
            MessageBody::new("{\"x\":0}".into(), Vec::new()).unwrap()
        ),
        Err(ProtocolError::Limit)
    );
    assert_eq!(t.state().control.records, 0);
}
