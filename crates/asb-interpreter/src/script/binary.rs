use super::{Instruction, Script};
use crate::error::{Error, Result};
use encoding_rs::Encoding;
use std::collections::HashMap;

impl Script {
    /// Load compiled records without a lossy round trip through quoted tag text.
    pub fn parse_asb(name: &str, data: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let mut reader = Reader { data, pos: 0, name };
        if reader.take(4)? != b"ASB\0" {
            return Err(reader.error("invalid ASB magic"));
        }
        reader.take(1)?;
        let count = reader.u32()? as usize;
        if count > reader.remaining() / 9 {
            return Err(reader.error("invalid record count"));
        }
        let mut labels = HashMap::new();
        let mut instructions = Vec::new();
        for _ in 0..count {
            let kind = reader.u32()?;
            let tag = reader.string(encoding)?;
            match kind {
                1 => {
                    labels.insert(tag, instructions.len());
                }
                0 => {
                    let line = reader.u32()? as usize;
                    let count = reader.u32()? as usize;
                    if count > reader.remaining() / 10 {
                        return Err(reader.error("invalid parameter count"));
                    }
                    let mut params = HashMap::new();
                    for _ in 0..count {
                        let key = reader.string(encoding)?;
                        let value = reader.string(encoding)?;
                        params.insert(key, value);
                    }
                    let tag = if tag == "lua" && params.contains_key("script") {
                        let code = params.remove("script").unwrap();
                        params.insert("code".into(), code);
                        "__lua_block".into()
                    } else {
                        tag
                    };
                    instructions.push(Instruction { tag, params, line });
                }
                _ => return Err(reader.error("unknown record type")),
            }
        }
        // The compiler counts instructions, not labels or source lines. Keep
        // that address space intact, including load-time Lua records.
        for inst in &instructions {
            if let Some(index) = inst.get("\u{b}index") {
                if !index
                    .parse::<usize>()
                    .is_ok_and(|i| i <= instructions.len())
                {
                    return Err(reader.error("invalid compiled branch target"));
                }
            }
        }
        Ok(Self {
            name: name.into(),
            labels,
            instructions,
        })
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    name: &'a str,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn error(&self, message: &str) -> Error {
        Error::DecodeError(format!("{} at byte {}: {message}", self.name, self.pos))
    }

    fn take(&mut self, len: usize) -> Result<&[u8]> {
        if len > self.remaining() {
            return Err(self.error("truncated ASB record"));
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.data[start..self.pos])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn string(&mut self, encoding: &'static Encoding) -> Result<String> {
        let len = self.u32()? as usize;
        let value = encoding.decode(self.take(len)?).0.into_owned();
        if self.take(1)? != [0] {
            return Err(self.error("string length does not match NUL terminator"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(entries: &[(&str, Option<&[(&str, &str)]>)]) -> Vec<u8> {
        fn string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        }
        let mut out = b"ASB\0\0".to_vec();
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, (name, params)) in entries.iter().enumerate() {
            out.extend_from_slice(&u32::from(params.is_none()).to_le_bytes());
            string(&mut out, name);
            if let Some(params) = params {
                out.extend_from_slice(&((i + 1) as u32).to_le_bytes());
                out.extend_from_slice(&(params.len() as u32).to_le_bytes());
                for (k, v) in *params {
                    string(&mut out, k);
                    string(&mut out, v);
                }
            }
        }
        out
    }

    #[test]
    fn compiled_strings_and_source_lines_are_lossless() {
        let code = "function probe() return {['[]'] = \"a\\nb\"} end\n-- [/lua]";
        let data = fixture(&[
            ("entry", None),
            ("lua", Some(&[("script", code)])),
            ("caption", Some(&[("data", " [hello] \"world\"\n次の行 ")])),
            ("last", None),
        ]);
        let script = Script::parse_asb("test", &data, encoding_rs::UTF_8).unwrap();
        assert_eq!(script.labels["entry"], 0);
        assert_eq!(script.labels["last"], 2);
        assert_eq!(script.instructions[0].tag, "__lua_block");
        assert_eq!(script.instructions[0].get("code"), Some(code));
        assert_eq!(
            script.instructions[1].get("data"),
            Some(" [hello] \"world\"\n次の行 ")
        );
        assert_eq!(script.instructions[1].line, 3);
    }

    #[test]
    fn truncated_records_return_errors_without_panicking() {
        let data = fixture(&[("lua", Some(&[("script", "local x = 1")]))]);
        for len in 0..data.len() {
            assert!(Script::parse_asb("test", &data[..len], encoding_rs::UTF_8).is_err());
        }
        let mut bad = data.clone();
        bad[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Script::parse_asb("test", &bad, encoding_rs::UTF_8).is_err());
        let mut bad = data;
        *bad.last_mut().unwrap() = b'x';
        assert!(Script::parse_asb("test", &bad, encoding_rs::UTF_8).is_err());
    }

    #[test]
    fn compiled_branches_use_instruction_indices_and_load_lua_once() {
        use crate::{Interpreter, InterpreterConfig};
        let data = fixture(&[
            ("main", None),
            (
                "lua",
                Some(&[(
                    "script",
                    "loads = (loads or 0) + 1; function check() assert(loads == 1) end",
                )]),
            ),
            ("var", Some(&[("name", "result"), ("data", "before")])),
            ("if", Some(&[("estimate", "0"), ("\u{b}index", "5")])),
            ("var", Some(&[("name", "result"), ("data", "then")])),
            ("\u{b}goto", Some(&[("\u{b}index", "9")])),
            ("elseif", Some(&[("estimate", "1"), ("\u{b}index", "8")])),
            ("var", Some(&[("name", "result"), ("data", "elseif")])),
            ("\u{b}goto", Some(&[("\u{b}index", "9")])),
            ("var", Some(&[("name", "result"), ("data", "else")])),
            ("calllua", Some(&[("function", "check")])),
            ("stop", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.load_asb("test", &data).unwrap();
        it.start("test", "main").unwrap();
        it.run().unwrap();
        assert_eq!(it.get_variable("result").unwrap().as_string(), "elseif");
        assert_eq!(it.lua().globals().get::<i32>("loads").unwrap(), 1);
    }

    #[test]
    fn compiled_backward_jump_loops_without_source_endloop() {
        use crate::{Interpreter, InterpreterConfig};
        let data = fixture(&[
            ("main", None),
            ("var", Some(&[("name", "i"), ("data", "0")])),
            ("loop", Some(&[("estimate", "$i < 3"), ("\u{b}index", "4")])),
            ("var", Some(&[("name", "i"), ("data", "$i + 1")])),
            ("\u{b}goto", Some(&[("\u{b}index", "1")])),
            ("stop", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.load_asb("test", &data).unwrap();
        it.start("test", "main").unwrap();
        it.run().unwrap();
        assert_eq!(it.get_variable("i").unwrap().as_int(), Some(3));
    }

    #[test]
    fn compiled_macro_keeps_early_returns_and_file_local_labels() {
        use crate::{Interpreter, InterpreterConfig};
        let data = fixture(&[
            (
                "lua",
                Some(&[("script", "macro_loads = (macro_loads or 0) + 1")]),
            ),
            ("choose", None),
            (
                "if",
                Some(&[("estimate", "$mode == 1"), ("\u{b}index", "4")]),
            ),
            ("var", Some(&[("name", "result"), ("data", "one")])),
            ("return", Some(&[])),
            ("jump", Some(&[("label", "$'helper_' + mode")])),
            ("helper_2", None),
            ("var", Some(&[("name", "result"), ("data", "two")])),
            ("return", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |_| Ok(data.clone())));
        it.load_macro_file("macros.iet").unwrap();
        it.load_macro_file("macros.iet").unwrap();
        it.load_script(
            "main",
            r#"
*main
[choose mode="1"]
[var name="first" data="$result"]
[choose mode="2"]
[stop]
"#,
        )
        .unwrap();
        it.start("main", "main").unwrap();
        it.run().unwrap();
        assert_eq!(it.get_variable("first").unwrap().as_string(), "one");
        assert_eq!(it.get_variable("result").unwrap().as_string(), "two");
        assert_eq!(it.lua().globals().get::<i32>("macro_loads").unwrap(), 1);
        it.unload_macro_file("macros.iet");
        assert!(!it.macro_registry().contains("choose"));
    }

    #[test]
    fn pass_through_filters_run_registered_macros() {
        use crate::{Interpreter, InterpreterConfig};
        for compiled in [false, true] {
            for result in ["0", "nil", "false", "1", "replacement"] {
                let data = if compiled {
                    fixture(&[
                        ("picture", None),
                        ("var", Some(&[("name", "t.picture"), ("data", "$file")])),
                        ("return", Some(&[])),
                    ])
                } else {
                    b"*picture\n[var name=\"t.picture\" data=\"$file\"]\n[return]\n".to_vec()
                };
                let mut it = Interpreter::new(InterpreterConfig::default());
                it.set_file_loader(Box::new(move |_| Ok(data.clone())));
                it.load_macro_file("macros").unwrap();
                let body = if result == "replacement" {
                    "e:enqueueTag{\"picture\", file=p.file}; return 1".into()
                } else {
                    format!("return {result}")
                };
                it.lua()
                    .load(format!(
                        r#"
                    __engine:setTagFilter({{picture = function(e, p)
                        filter_calls = (filter_calls or 0) + 1
                        assert(p.file == "bg/room")
                        {body}
                    end}})
                "#
                    ))
                    .exec()
                    .unwrap();
                it.set_variable("t.file", crate::Value::String("bg/room".into()));
                it.load_script("main", "[picture file=\"$t.file\"]\n[stop]")
                    .unwrap();
                it.start("main", "").unwrap();
                it.run().unwrap();
                assert_eq!(it.lua().globals().get::<i32>("filter_calls").unwrap(), 1);
                if result == "1" {
                    assert!(it.get_variable("t.picture").is_none());
                } else {
                    assert_eq!(it.get_variable("t.picture").unwrap().as_string(), "bg/room");
                }
            }
        }
    }

    #[test]
    fn compiled_macro_arguments_do_not_leak_across_nested_calls() {
        use crate::{Interpreter, InterpreterConfig};
        let data = fixture(&[
            ("outer", None),
            ("inner", Some(&[])),
            ("var", Some(&[("name", "t.outer_id"), ("data", "$id")])),
            ("return", Some(&[])),
            ("inner", None),
            (
                "var",
                Some(&[
                    ("name", "t.child_has_id"),
                    ("system", "var_exist"),
                    ("target", "id"),
                    ("local", "1"),
                ]),
            ),
            (
                "var",
                Some(&[("name", "id"), ("data", "private"), ("writelocal", "1")]),
            ),
            ("var", Some(&[("name", "t.child_id"), ("data", "$id")])),
            ("return", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |_| Ok(data.clone())));
        it.load_macro_file("macros.iet").unwrap();
        it.load_script("main", "[outer id=\"100.6\"]\n[stop]")
            .unwrap();
        it.start("main", "").unwrap();
        it.run().unwrap();
        assert_eq!(it.get_variable("t.child_has_id").unwrap().as_int(), Some(0));
        assert_eq!(
            it.get_variable("t.child_id").unwrap().as_string(),
            "private"
        );
        assert_eq!(it.get_variable("t.outer_id").unwrap().as_string(), "100.6");
        assert!(it.get_variable("id").is_none());
    }

    #[test]
    fn compiled_macro_dynamic_var_destination_is_stored_with_resolved_key() {
        use crate::{Interpreter, InterpreterConfig};
        let data = fixture(&[
            ("slider_h", None),
            (
                "var",
                Some(&[("name", "$'slider.' + id + '.file'"), ("data", "$file")]),
            ),
            (
                "var",
                Some(&[("name", "$'slider.' + id + '.label'"), ("data", "$label")]),
            ),
            ("return", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |_| Ok(data.clone())));
        it.load_macro_file("macros.iet").unwrap();
        it.load_script(
            "main",
            "[slider_h id=\"100.slider.7\" file=\"system/config.iet\" label=\"onslide_sound\"]\n[stop]",
        )
        .unwrap();
        it.start("main", "").unwrap();
        it.run().unwrap();
        assert_eq!(
            it.get_variable("slider.100.slider.7.file")
                .unwrap()
                .as_string(),
            "system/config.iet"
        );
        assert_eq!(
            it.get_variable("slider.100.slider.7.label")
                .unwrap()
                .as_string(),
            "onslide_sound"
        );
    }

    #[test]
    fn compiled_macro_dynamic_callback_survives_macro_return() {
        use crate::{CallbackResult, Event, Interpreter, InterpreterConfig};
        let slider = fixture(&[
            ("slider_h", None),
            (
                "var",
                Some(&[("name", "$'slider.' + id + '.file'"), ("data", "$file")]),
            ),
            (
                "var",
                Some(&[("name", "$'slider.' + id + '.label'"), ("data", "$label")]),
            ),
            ("return", Some(&[])),
        ]);
        let config = b"*onslide_sound\n[var name=\"t.callback\" data=\"ok\"]\n[var name=\"g.bgmvol\" data=\"$t.slider.value\"]\n[return]\n".to_vec();
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |file| match file {
            "slider.iet" => Ok(slider.clone()),
            "system/config.iet" => Ok(config.clone()),
            other => Err(crate::Error::ScriptNotFound(other.to_string())),
        }));
        it.load_macro_file("slider.iet").unwrap();
        it.load_script(
            "main",
            "*main\n[var name=\"t.slider.id\" data=\"100.slider.2\"]\n[var name=\"t.slider.value\" data=\"777\"]\n[slider_h id=\"100.slider.2\" file=\"system/config.iet\" label=\"onslide_sound\"]\n[call file=\"$slider.(t.slider.id).file\" label=\"$slider.(t.slider.id).label\"]\n[stop]\n",
        )
        .unwrap();
        it.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        it.start("main", "main").unwrap();
        it.run().unwrap();
        assert_eq!(it.get_variable("t.callback").unwrap().as_string(), "ok");
        assert_eq!(it.get_variable("g.bgmvol").unwrap().as_int(), Some(777));
    }

    #[test]
    fn removing_event_marker_preserves_compiled_handler_arguments() {
        use crate::{CallFrame, CallbackResult, Event, Interpreter, InterpreterConfig};
        let data = fixture(&[
            ("handler", None),
            (
                "var",
                Some(&[("name", "t.received"), ("data", "$destination")]),
            ),
            ("return", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |_| Ok(data.clone())));
        it.load_macro_file("macros.iet").unwrap();
        it.load_script("main", "[stop]").unwrap();
        it.start("main", "").unwrap();
        it.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        it.run().unwrap();
        it.restore_position(
            "main",
            0,
            vec![CallFrame {
                script: "main".into(),
                return_line: 0,
            }],
        )
        .unwrap();
        it.engine_context().lock().unwrap().tag_queue.push((
            "handler".into(),
            std::collections::HashMap::from([("destination".into(), "settings".into())]),
        ));
        let drain = it.drain_queued_tags_only().unwrap();
        assert!(drain.saw_call);
        it.remove_call_frame(0).unwrap();
        assert_eq!(
            it.get_variable("destination").unwrap().as_string(),
            "settings"
        );
        it.run().unwrap();
        assert_eq!(
            it.get_variable("t.received").unwrap().as_string(),
            "settings"
        );
        assert!(it.get_variable("destination").is_none());
        assert!(it.call_stack().is_empty());
        assert_eq!(it.current_script(), Some("main"));
        assert_eq!(it.current_line(), 0);
    }

    #[test]
    fn text_from_shared_macro_keeps_each_callers_source() {
        use crate::{CallbackResult, Event, Interpreter, InterpreterConfig};
        use std::sync::{Arc, Mutex};
        let data = fixture(&[
            ("dialogue", None),
            ("print", Some(&[("data", "$body")])),
            ("return", Some(&[])),
        ]);
        let mut it = Interpreter::new(InterpreterConfig::default());
        it.set_file_loader(Box::new(move |_| Ok(data.clone())));
        it.load_macro_file("printing.iet").unwrap();
        it.load_script(
            "chapter",
            "[dialogue body=first]\n[dialogue body=second]\n[stop]",
        )
        .unwrap();
        it.start("chapter", "").unwrap();
        let ctx = Arc::clone(it.engine_context());
        let sources = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&sources);
        it.set_callback(move |event| {
            if matches!(event, Event::ScenarioText { .. }) {
                captured
                    .lock()
                    .unwrap()
                    .push(ctx.lock().unwrap().scenario_text_source.clone().unwrap());
            }
            if matches!(event, Event::Wait { .. }) {
                CallbackResult::Pause
            } else {
                CallbackResult::Continue
            }
        });
        it.run().unwrap();
        assert_eq!(
            *sources.lock().unwrap(),
            vec![("chapter".into(), 0), ("chapter".into(), 1)]
        );
    }
}
