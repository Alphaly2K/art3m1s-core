use art3m1s_core::api::{
    ErrorKind, HostFrame, IO_READ, IO_STAT, MEDIA_AUDIO_COMMAND, MessageBody, PlaybackStatus,
    ProjectInfo, Runtime, RuntimeOptions, SESSION_CHANGED, UI_CAPTION, capabilities,
};
use serde_json::{Value, json};

#[test]
fn rust_host_drives_the_queued_session_and_reads_a_frame() {
    const PROJECT: ProjectInfo<'_> = ProjectInfo {
        ini: b"[WINDOWS]\nWIDTH=32\nHEIGHT=24\nBOOT=boot.iet\n",
        platform: "WINDOWS",
        reported_os: None,
    };
    const BOOT: &[u8] = b"*top\n[lyc id=1 width=2 height=1 color=ff112233]\n[caption data=ready]\n[seplay id=fx file=tone.ogg]\n[wait se=fx]\n[caption data=ended]\n[stop]\n";
    let runtime = Runtime::new(RuntimeOptions {
        host_capabilities: capabilities::HOST_CAP_IO | capabilities::HOST_CAP_MEDIA,
        renderer_capabilities: capabilities::CAP_SPRITES,
        ..Default::default()
    })
    .unwrap();
    let other = Runtime::new(Default::default()).unwrap();
    let epoch = runtime.open_project(PROJECT).unwrap();
    let event = runtime.poll_event().unwrap().unwrap();
    assert!(event.is_critical());
    assert_eq!((event.kind, event.epoch), (SESSION_CHANGED, epoch));
    assert!(!event.requires_reply());

    let mut ready = false;
    let mut ended = false;
    let mut playback = None;
    let mut rejected_foreign_reply = false;
    let mut frame = HostFrame {
        id: 1,
        monotonic_ns: 0,
        utc_unix_ms: 0,
        utc_offset_minutes: 0,
    };
    for _ in 0..48 {
        let step = runtime.step(frame).unwrap();
        while let Some(event) = runtime.poll_event().unwrap() {
            let data: Value = serde_json::from_str(event.body.metadata()).unwrap();
            let response = match event.kind {
                IO_STAT if data["key"] == "boot.iet" => Some((
                    json!({"status":"ok", "found":true, "size":BOOT.len().to_string(), "revision":"1"}),
                    vec![],
                )),
                IO_STAT => Some((json!({"status":"ok", "found":false}), vec![])),
                IO_READ => {
                    assert_eq!(data["key"], "boot.iet");
                    Some((
                        json!({"status":"ok", "eof":true, "revision":"1"}),
                        BOOT.to_vec(),
                    ))
                }
                MEDIA_AUDIO_COMMAND => {
                    assert_eq!(data["op"], "play");
                    playback = Some(data["playback_id"].as_str().unwrap().parse().unwrap());
                    None
                }
                UI_CAPTION => {
                    ready |= data["text"] == "ready";
                    ended |= data["text"] == "ended";
                    None
                }
                _ => None,
            };
            assert_eq!(event.requires_reply(), response.is_some());
            if let Some((metadata, blob)) = response {
                let reply = event
                    .reply(MessageBody::new(metadata.to_string(), blob).unwrap())
                    .unwrap();
                if !rejected_foreign_reply {
                    assert_eq!(
                        other
                            .submit_replies(std::slice::from_ref(&reply))
                            .unwrap_err()
                            .kind,
                        ErrorKind::Stale
                    );
                    rejected_foreign_reply = true;
                }
                runtime.submit_replies(&[reply]).unwrap();
            }
        }
        if !step.state.waiting_for_io && !step.backpressured {
            frame.id += 1;
            frame.monotonic_ns += 16_000_000;
        }
        if let Some(id) = playback.take() {
            assert!(ready && !ended);
            runtime
                .submit_playback_status(epoch, id, PlaybackStatus::Started)
                .unwrap();
            runtime
                .submit_playback_status(epoch, id, PlaybackStatus::Ended)
                .unwrap();
        }
        if ended {
            break;
        }
    }
    assert!(ended && rejected_foreign_reply);

    let latest = runtime
        .with_latest_frame(0, |frame| {
            assert!(frame.packet().starts_with(b"ARFP"));
            let texture = frame.texture(0).unwrap();
            assert_eq!([texture.width, texture.height], [1, 1]);
            assert_eq!(texture.rgba(), [0x11, 0x22, 0x33, 0xff]);
            let reloaded = runtime.open_project(PROJECT).unwrap();
            assert!(reloaded > epoch);
            assert_eq!(frame.texture(0).unwrap().rgba(), [0x11, 0x22, 0x33, 0xff]);
            frame.render_frame_id()
        })
        .unwrap()
        .unwrap();
    assert!(runtime.with_latest_frame(latest, |_| ()).unwrap().is_none());
}
