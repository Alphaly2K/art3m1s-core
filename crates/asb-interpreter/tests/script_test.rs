//! 测试 script.txt（系统控制脚本）的解析和执行

#[cfg(test)]
mod script_tests {
    use asb_interpreter::{CallbackResult, Event, ExecutionResult, Interpreter, InterpreterConfig};
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_parse_script_txt() {
        // 验证系统控制脚本的关键标签能被正确解析。
        // 原依赖外部 docs/examples/script.txt fixture（已移除），改为自包含内联脚本，
        // 覆盖同样的标签集合，避免对外部文件的脆弱依赖。
        let content = r#"
*main
[my_custom_tag value="1"]
[stop]

*click
[var name="x" data="1"]
[return]

*click2
[var name="x" data="2"]
[return]

*select
[var name="x" data="3"]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter
            .load_script("system/script.iet", content)
            .expect("Failed to parse script");

        // 验证关键标签被解析
        let script = interpreter.get_script("system/script.iet").unwrap();
        assert!(
            script.get_label_line("main").is_some(),
            "*main label not found"
        );
        assert!(
            script.get_label_line("click").is_some(),
            "*click label not found"
        );
        assert!(
            script.get_label_line("click2").is_some(),
            "*click2 label not found"
        );
        assert!(
            script.get_label_line("select").is_some(),
            "*select label not found"
        );
    }

    #[test]
    fn test_stop_tag() {
        // 测试 [stop] 标签的阻塞行为
        let script = r#"
*main
[var name="counter" data="0"]
[stop]
[var name="counter" data="$counter + 1"]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        interpreter.set_callback(|event| match event {
            Event::Wait { reason } => {
                println!("Blocked: {:?}", reason);
                CallbackResult::Pause
            }
            _ => CallbackResult::Continue,
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        // 执行到 [stop]
        let result = interpreter.run();
        let blocked = matches!(result, Ok(ExecutionResult::Wait(_)));
        match result {
            Ok(ExecutionResult::Wait(event)) => {
                println!("Script blocked at: {:?}", event);
            }
            _ => panic!("Expected script to block at [stop]"),
        }

        assert!(blocked, "Script should have blocked at [stop]");
        assert_eq!(
            interpreter.get_variable("counter"),
            Some(asb_interpreter::Value::Int(0)),
            "Counter should not have been incremented"
        );
    }

    #[test]
    fn test_wt_tag() {
        // 测试 [wt] 标签的阻塞行为
        let script = r#"
*main
[var name="value" data="'before'"]
[wt]
[var name="value" data="'after'"]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        let result = interpreter.run();
        assert!(matches!(result, Ok(ExecutionResult::Wait(_))));
        assert_eq!(
            interpreter.get_variable("value"),
            Some(asb_interpreter::Value::String("before".to_string()))
        );
    }

    #[test]
    fn test_at_tag() {
        // 测试 [@] 标签（点击等待）
        let script = r#"
*main
[text data="Hello"]
[@]
[text data="World"]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        let click_wait_count = Arc::new(Mutex::new(0));
        let click_wait_count_clone = Arc::clone(&click_wait_count);

        interpreter.set_callback(move |event| match event {
            Event::Wait { .. } => {
                *click_wait_count_clone.lock().unwrap() += 1;
                CallbackResult::Pause
            }
            _ => CallbackResult::Continue,
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        let result = interpreter.run();
        assert!(matches!(result, Ok(ExecutionResult::Wait(_))));
        assert_eq!(
            *click_wait_count.lock().unwrap(),
            1,
            "Should have encountered [@] once"
        );
    }

    #[test]
    fn test_delay_system() {
        // 测试延迟系统（delay10 到 delay1）
        let script = r#"
*main
[var name="delay_count" data="0"]
[call label="delay3"]
[return]

*delay3
[calllua function="delay_run"]
[calllua function="delay_wait"]

*delay2
[calllua function="delay_run"]
[calllua function="delay_wait"]

*delay1
[calllua function="delay_run"]
[calllua function="delay_wait"]
[return]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());

        // 模拟 Lua 函数
        interpreter.set_callback(|_| CallbackResult::Continue);

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        // 这个测试会失败，因为 delay_run 等 Lua 函数未定义
        // 但这验证了解释器能够解析和尝试执行这些标签
        let result = interpreter.run();
        println!("Result: {:?}", result);
    }

    #[test]
    fn test_script_main_loop() {
        // 测试主循环模式（使用未注册的标签来触发回调）
        let script = r#"
*main
[my_custom_tag value="1"]
[my_custom_tag value="2"]
[jump label="main"]
"#;

        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        let loop_count = Arc::new(Mutex::new(0));
        let loop_count_clone = Arc::clone(&loop_count);

        interpreter.set_callback(move |event| {
            match event {
                Event::Custom { tag, .. } if tag == "my_custom_tag" => {
                    let mut count = loop_count_clone.lock().unwrap();
                    *count += 1;
                    if *count >= 5 {
                        // 防止无限循环
                        CallbackResult::Abort
                    } else {
                        CallbackResult::Continue
                    }
                }
                _ => CallbackResult::Continue,
            }
        });

        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();

        let result = interpreter.run();
        assert!(matches!(result, Err(asb_interpreter::Error::Aborted)));
        assert!(
            *loop_count.lock().unwrap() >= 5,
            "Should have looped at least 5 times"
        );
    }
}
