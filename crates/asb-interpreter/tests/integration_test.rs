//! 集成测试：使用 docs/examples/project 中的真实项目验证解释器

#[cfg(test)]
mod integration_tests {
    use asb_interpreter::event::LayerEvent;
    use asb_interpreter::{CallbackResult, Event, Interpreter, InterpreterConfig};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn get_project_path() -> PathBuf {
        // 真实示例项目位于同级 crate art3m1s-core 下（docs/examples/project 已移除）。
        // 依赖在旁边 checkout 了 art3m1s-core；缺失时相关测试会因 fixture 不存在而失败。
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../art3m1s-core/example/project")
    }

    #[test]
    fn test_event_callback() {
        // 测试事件回调是否能正确接收事件
        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        let script = r#"
*main
[debugprint data="Hello from ASB!"]
[lyc id="test" file="test.png"]
[trans type=1 time=500]
[seplay id="bgm" file="test.ogg" loop=1]
[stop]
"#;

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        interpreter.set_callback(move |event: Event| {
            events_clone.lock().unwrap().push(format!("{:?}", event));
            CallbackResult::Continue
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        // 执行到 stop
        let _ = interpreter.run();

        // 验证收到了事件
        let events = events.lock().unwrap();
        assert!(events.len() > 0, "Should have received events");
        println!("Received {} events:", events.len());
        for event in events.iter() {
            println!("  - {}", event);
        }
    }

    #[test]
    fn test_complex_script_parsing() {
        // 测试复杂脚本的解析（包含各种标签）
        let script = r#"
*main
; 控制流
[if estimate="$flag == 1"]
    [jump label="branch1"]
[elseif estimate="$flag == 2"]
    [jump label="branch2"]
[else]
    [jump label="default"]
[/if]

*branch1
[var name="result" data="'branch1'"]
[return]

*branch2
[var name="result" data="'branch2'"]
[return]

*default
; 循环
[var name="i" data="0"]
[loop estimate="$i < 3"]
    [var name="sum" data="$sum + $i"]
    [var name="i" data="$i + 1"]
[/loop]

; Lua 块
[lua]
function test_func(e)
    e:debug{level=0, data="Hello from Lua"}
end
[/lua]
[calllua function="test_func"]

; 图层操作
[lyc id="bg" file="bg.png"]
[lyprop id="bg" left=0 top=0 alpha=255]
[lytween id="bg" param="alpha" from=255 to=0 time=1000]

; 音频
[splay file="bgm.ogg" loop=1]
[seplay id="se1" file="click.ogg"]

; 等待和转场
[wt]
[trans type=1 time=500]

[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .load_script("complex", script)
            .expect("Failed to parse complex script");

        // 验证所有标签都被正确解析
        let script_obj = interpreter.get_script("complex").unwrap();

        // 检查指令数量（大约 30+ 条指令）
        assert!(
            script_obj.instructions.len() > 20,
            "Expected > 20 instructions, got {}",
            script_obj.instructions.len()
        );

        println!(
            "Successfully parsed {} instructions",
            script_obj.instructions.len()
        );
    }

    #[test]
    fn test_macro_system() {
        // 测试宏系统
        let macro_script = r#"
*show_chara
[lyc id="$layer" file="$file"]
[lyprop id="$layer" left=$left top=$top]
[trans type=1 time=300]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.load_script("macro", macro_script).unwrap();

        // 验证宏被解析
        let script = interpreter.get_script("macro").unwrap();
        assert!(script.get_label_line("show_chara").is_some());

        println!("Macro system test passed");
    }

    #[test]
    fn test_load_ast_file() {
        // 测试加载 .ast 文件（文本格式）
        let project_path = get_project_path();
        let ast_path = project_path.join("script/共通-03.ast");

        if !ast_path.exists() {
            println!("Skipping .ast test: file not found");
            return;
        }

        let data = std::fs::read(&ast_path).expect("Failed to read .ast file");

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        // 使用 load_file 方法（自动检测格式）
        interpreter
            .load_file("script/共通-03.ast", &data)
            .expect("Failed to load .ast file");

        // 验证脚本被加载
        assert!(interpreter.get_script("script/共通-03.ast").is_some());

        println!("Text file (.ast) loading test passed");
    }

    #[test]
    fn test_lua_enqueued_tags_produce_events() {
        // 回归测试：Lua 通过 e:tag{} 排入的标签必须走标签管线并产出事件。
        // 此前 tag_queue 只被写入、从无人消费，导致 Lua 驱动的图层操作全部丢失。
        let script = r#"
*main
[lua]
function show_bg(e)
    e:tag{"lyc", id="bg", file="bg.png"}
    e:tag{"lyprop", id="bg", left="0", top="0"}
end
[/lua]
[calllua function="show_bg"]
[stop]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        interpreter.set_callback(move |event: Event| {
            events_clone.lock().unwrap().push(event);
            CallbackResult::Continue
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run();

        let events = events.lock().unwrap();

        let created = events.iter().any(|e| {
            matches!(
                e,
                Event::Layer(LayerEvent::Create { id, file })
                    if id == "bg" && file == "bg.png"
            )
        });
        assert!(
            created,
            "Lua e:tag{{lyc}} 应产出 LayerEvent::Create, 实际事件: {events:?}"
        );

        let propped = events.iter().any(|e| {
            matches!(
                e,
                Event::Layer(LayerEvent::SetProperties { id, .. }) if id == "bg"
            )
        });
        assert!(
            propped,
            "Lua e:tag{{lyprop}} 应产出 LayerEvent::SetProperties, 实际事件: {events:?}"
        );
    }

    #[test]
    fn test_lua_enqueued_jump_transfers_control() {
        // 回归测试：Lua 通过 eqtag/enqueueTag 排入的控制流标签（如 jump）必须生效。
        // 真实 boot 里 system_starting 用 eqtag{"jump", label="game_start"} 推进流程，
        // 若 flush_tag_queue 忽略 Jump/Call/Return，boot 永远到不了 game_start。
        let script = r#"
*main
[lua]
function go(e)
    e:enqueueTag{"jump", label="target"}
end
[/lua]
[calllua function="go"]
[lyc id="never" file="never.png"]
[stop]
*target
[lyc id="arrived" file="arrived.png"]
[stop]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        interpreter.set_callback(move |event: Event| {
            events_clone.lock().unwrap().push(event);
            CallbackResult::Continue
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run();

        let events = events.lock().unwrap();

        let arrived = events
            .iter()
            .any(|e| matches!(e, Event::Layer(LayerEvent::Create { id, .. }) if id == "arrived"));
        let fell_through = events
            .iter()
            .any(|e| matches!(e, Event::Layer(LayerEvent::Create { id, .. }) if id == "never"));
        assert!(
            arrived,
            "排队的 jump 应跳到 *target 并执行其图层标签, 实际事件: {events:?}"
        );
        assert!(
            !fell_through,
            "jump 后不应落到 *main 的后续指令, 实际事件: {events:?}"
        );
    }

    #[test]
    fn test_lua_include_loads_and_executes() {
        // 回归测试：e:include 必须读取文件并在当前 VM 执行（注册函数等副作用）。
        // 此前 include 只通知一个 no-op 回调，导致 init.lua 等从未真正加载，
        // 后续 [calllua] 调用其中定义的函数全部静默落空。
        let script = r#"
*main
[lua]
function bootstrap(e)
    e:include("lib/widgets.lua")
end
[/lua]
[calllua function="bootstrap"]
[calllua function="draw_widget"]
[stop]
"#;

        // 被 include 的文件：定义一个会产出图层事件的全局函数。
        let widgets = br#"
function draw_widget(e)
    e:tag{"lyc", id="widget", file="widget.png"}
end
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        // 文件加载器只认识这一个虚拟路径。
        let widgets_bytes = widgets.to_vec();
        interpreter.set_file_loader(Box::new(move |name: &str| {
            if name == "lib/widgets.lua" {
                Ok(widgets_bytes.clone())
            } else {
                Err(asb_interpreter::Error::ScriptNotFound(name.to_string()))
            }
        }));

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        interpreter.set_callback(move |event: Event| {
            events_clone.lock().unwrap().push(event);
            CallbackResult::Continue
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run();

        let events = events.lock().unwrap();
        let drew = events
            .iter()
            .any(|e| matches!(e, Event::Layer(LayerEvent::Create { id, .. }) if id == "widget"));
        assert!(
            drew,
            "include 进来的 draw_widget 应被定义并可调用、产出图层事件, 实际事件: {events:?}"
        );
    }

    #[test]
    fn test_pluto_unpersist_restores_numeric_table_keys() {
        // JSON 对象 key 只能是字符串；旧实现用 LuaSerdeExt 反序列化时会把
        // sys.saveslot["8"] 保成字符串 key，游戏脚本读 sys.saveslot[8] 就为空。
        let interpreter = Interpreter::new(InterpreterConfig::default());

        interpreter
            .lua()
            .load(
                r#"
local sys = pluto.unpersist({}, '{"saveslot":{"last":8,"8":{"file":"save0004"},"lock":{"8":true}}}')
assert(type(sys.saveslot) == "table")
assert(sys.saveslot[8] ~= nil, "numeric slot key should be restored")
assert(sys.saveslot[8].file == "save0004")
assert(sys.saveslot.last == 8)
assert(sys.saveslot.lock[8] == true)
assert(sys.saveslot["8"] == nil, "legacy string slot key should not be required")
"#,
            )
            .exec()
            .unwrap();
    }

    #[test]
    fn test_savess_preserves_requested_size() {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        interpreter.set_callback(move |event: Event| {
            events_clone.lock().unwrap().push(event);
            CallbackResult::Continue
        });

        interpreter
            .load_script(
                "test",
                r#"
*main
[savess file="save0001" width=166 height=93]
[stop]
"#,
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let _ = interpreter.run();

        let events = events.lock().unwrap();
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::SaveScreenshot {
                    file,
                    width: Some(166),
                    height: Some(93),
                } if file == "save0001"
            )),
            "savess 应带上脚本请求的输出尺寸，实际事件: {events:?}"
        );
    }
}
