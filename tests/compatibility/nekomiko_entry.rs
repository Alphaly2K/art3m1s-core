use art3m1s_core::Project;
use art3m1s_core::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};
use art3m1s_core::text::{GlyphTextRenderer, TextRenderer};
use asb_interpreter::event::{LayerEvent, WaitReason};
use asb_interpreter::{CallbackResult, Event, ExecutionResult, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct ProbeTextureProvider;

impl TextureProvider for ProbeTextureProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        (name == ":text/atlas").then_some((
            TextureId(1),
            TextureInfo {
                width: 1024,
                height: 1024,
            },
        ))
    }

    fn upload_rgba(
        &mut self,
        _name: &str,
        width: u32,
        height: u32,
        _data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        Some((TextureId(1), TextureInfo { width, height }))
    }
}

fn run_until_stop(interpreter: &mut asb_interpreter::Interpreter, max_steps: usize) {
    for _ in 0..max_steps {
        match interpreter.run().expect("NekoMiko probe execution failed") {
            ExecutionResult::Wait(Event::Wait {
                reason:
                    WaitReason::Stop {
                        reason: Some(reason),
                    },
            }) if reason == "exskip" => interpreter.advance_line(),
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { .. },
            }) => return,
            ExecutionResult::Wait(_) => interpreter.advance_line(),
            ExecutionResult::Completed => return,
            _ => {}
        }
    }
    panic!("NekoMiko probe did not reach stop within {max_steps} waits");
}

#[test]
#[ignore = "requires the external nekomiko fixture"]
fn nekomiko_language_selection_and_name_dialog_chain() {
    let root = crate::common::project_fixture("nekomiko");

    let project = Project::open(&root, "WINDOWS").expect("open NekoMiko");
    let mut interpreter = project.create_interpreter();
    interpreter.set_engine_callbacks(Box::new(crate::common::ProjectCallbacks::new(root.clone())));

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_callback = Arc::clone(&events);
    interpreter.set_callback(move |event| {
        events_for_callback.lock().unwrap().push(event.clone());
        match event {
            Event::Wait { .. } | Event::ShowDialog { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        }
    });

    project
        .start_boot(&mut interpreter)
        .expect("start NekoMiko");
    run_until_stop(&mut interpreter, 300);

    let boot_events = events.lock().unwrap().clone();
    eprintln!(
        "language stop at {:?}:{} with {} events",
        interpreter.current_script(),
        interpreter.current_line(),
        boot_events.len()
    );
    for event in &boot_events {
        match event {
            Event::ScenarioText { content, .. } => eprintln!("text: {content:?}"),
            Event::Layer(LayerEvent::Create { id, file }) if id.contains(".120") => {
                eprintln!("select layer: {id} file={file}")
            }
            Event::LayerEventHandler {
                id,
                event_type,
                handler,
                ..
            } if id.contains(".120") => {
                eprintln!("select handler: {id} {event_type} {handler:?}")
            }
            Event::Wait { reason } => eprintln!("wait: {reason:?}"),
            _ => {}
        }
    }
    assert!(boot_events.iter().any(|event| {
        matches!(
            event,
            Event::Layer(LayerEvent::Create { file, .. })
                if file == ":cg/selectlanguage"
        )
    }));
    assert!(boot_events.iter().any(|event| {
        matches!(
            event,
            Event::FontSettings(settings)
                if settings.get("face").map(String::as_str)
                    == Some("font/GenJyuuGothic-Bold.ttf")
        )
    }));
    let font_bytes =
        std::fs::read(root.join("font/GenJyuuGothic-Bold.ttf")).expect("read NekoMiko font");
    let mut text_renderer = GlyphTextRenderer::new();
    text_renderer
        .set_font(&font_bytes)
        .expect("parse NekoMiko font");
    text_renderer.switch_message_layer(Some("choice"), true);
    text_renderer.apply_font_settings(&HashMap::from([
        ("size".to_string(), "52".to_string()),
        ("width".to_string(), "977".to_string()),
        ("height".to_string(), "116".to_string()),
    ]));
    text_renderer.push_text("简体中文", false);
    text_renderer.reveal_all();
    let commands = text_renderer.build_text_commands(&mut ProbeTextureProvider);
    assert!(
        commands
            .get("choice")
            .is_some_and(|commands| commands.len() >= 4),
        "NekoMiko project font should rasterize the language labels"
    );
    for expected in ["简体中文", "英语"] {
        assert!(boot_events.iter().any(|event| {
            matches!(
                event,
                Event::ScenarioText { content, .. } if content == expected
            )
        }));
    }
    let select_click_handlers = boot_events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::LayerEventHandler {
                    event_type,
                    handler,
                    ..
                } if event_type == "click" && handler.as_deref() == Some("calllua")
            )
        })
        .count();
    assert!(
        select_click_handlers >= 2,
        "language selection should create two clickable choices"
    );

    events.lock().unwrap().clear();
    interpreter
        .lua()
        .load(
            r#"
                conf.language = "ja"
                game.path.ui = init.system.ui_path .. init.lang.ja
                title_start("gamestart")
            "#,
        )
        .exec()
        .expect("enqueue NekoMiko game start");

    let dialog = loop {
        match interpreter.run().expect("run NekoMiko game start") {
            ExecutionResult::Wait(event @ Event::ShowDialog { .. }) => break event,
            ExecutionResult::Wait(_) => interpreter.advance_line(),
            ExecutionResult::Completed => panic!("game completed before name dialog"),
            _ => {}
        }
    };

    match dialog {
        Event::ShowDialog {
            textfield,
            textfield_size,
            ..
        } => {
            assert_eq!(textfield.as_deref(), Some("myname"));
            assert_eq!(textfield_size, Some(10));
        }
        _ => unreachable!(),
    }

    interpreter.set_variable("myname", Value::String("测试名字".into()));
    interpreter.advance_line();
    for _ in 0..100 {
        match interpreter.run().expect("resume after name dialog") {
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Generic0,
            }) => panic!("name confirmation fell into Generic0"),
            ExecutionResult::Wait(Event::Wait {
                reason:
                    WaitReason::Stop {
                        reason: Some(reason),
                    },
            }) if reason == "exskip" => {
                panic!("inactive exskip stop should be consumed by the Lua tag filter")
            }
            ExecutionResult::Wait(Event::Wait {
                reason: WaitReason::Stop { reason: None },
            }) => break,
            ExecutionResult::Wait(_) => interpreter.advance_line(),
            ExecutionResult::Completed => panic!("game completed before name confirmation"),
            _ => {}
        }
    }

    let name: String = interpreter
        .lua()
        .load("return scr and scr.myname or ''")
        .eval()
        .unwrap_or_default();
    assert_eq!(name, "测试名字", "nameset should receive dialog text");

    let confirmation_events = events.lock().unwrap().clone();
    for expected in ["，确定吗？", "确定", "再想想"] {
        assert!(confirmation_events.iter().any(|event| {
            matches!(
                event,
                Event::ScenarioText { content, .. } if content == expected
            )
        }));
    }
    let confirmation_click_handlers = confirmation_events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::LayerEventHandler {
                    event_type,
                    handler,
                    ..
                } if event_type == "click" && handler.as_deref() == Some("calllua")
            )
        })
        .count();
    assert!(
        confirmation_click_handlers >= 2,
        "name confirmation should create two clickable choices"
    );
}
