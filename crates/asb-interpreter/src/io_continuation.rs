//! Opt-in Luau IO tasks. Lua yields after each native request builder returns,
//! so no borrowed userdata or Rust callback frame survives suspension.

use crate::luau_polyfill::BufferedFile;
use crate::{Interpreter, Result};
use mlua::{AnyUserData, Lua, MultiValue, Thread, ThreadStatus, UserData, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoRequest {
    Read {
        path: String,
    },
    Exists {
        path: String,
    },
    ModifiedTime {
        path: String,
    },
    Write {
        path: String,
        data: Vec<u8>,
    },
    DeleteSave {
        path: String,
    },
    TransferSave {
        source: String,
        destination: String,
        remove_source: bool,
    },
}

impl UserData for IoRequest {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoFailure {
    NotFound,
    Denied,
    IoError,
    Cancelled,
    Unsupported,
    Limit,
}

impl IoFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Denied => "denied",
            Self::IoError => "io_error",
            Self::Cancelled => "cancelled",
            Self::Unsupported => "unsupported",
            Self::Limit => "limit",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IoResponse {
    Read(Vec<u8>),
    Exists(bool),
    ModifiedTime(Option<i64>),
    Written,
    Deleted,
    Transferred,
    Failed { kind: IoFailure, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoProgress {
    Waiting,
    Completed,
}

pub struct IoTask {
    lua: Lua,
    thread: Thread,
    started: bool,
    finished: bool,
    failed: bool,
    pending: Option<IoRequest>,
    initial_args: MultiValue,
    result: Option<MultiValue>,
}

impl IoTask {
    /// Compile without executing. IO wrappers must be installed before loading
    /// script code that might retain references to the engine or IO functions.
    pub fn compile(lua: &Lua, source: &[u8], name: &str) -> Result<Self> {
        let function = lua.load(source).set_name(name).into_function()?;
        Self::from_function(lua, function, MultiValue::new())
    }

    pub fn from_function(
        lua: &Lua,
        function: mlua::Function,
        initial_args: MultiValue,
    ) -> Result<Self> {
        Ok(Self {
            lua: lua.clone(),
            thread: lua.create_thread(function)?,
            started: false,
            finished: false,
            failed: false,
            pending: None,
            initial_args,
            result: None,
        })
    }

    pub fn call(
        lua: &Lua,
        name: &str,
        params: &std::collections::HashMap<String, String>,
    ) -> Result<Option<Self>> {
        let mut value = Value::Table(lua.globals());
        for part in name.split('.') {
            value = match value {
                Value::Table(table) => table.get(part)?,
                _ => return Ok(None),
            };
        }
        let Value::Function(function) = value else {
            return Ok(None);
        };
        let table = if let Some(key) = params.get("__art3m1s_params_key")
            && let Ok(mailbox) = lua.globals().get::<mlua::Table>("__art3m1s_enqueue_params")
            && let Ok(table) = mailbox.get::<mlua::Table>(key.as_str())
        {
            mailbox.set(key.as_str(), Value::Nil)?;
            table
        } else {
            lua.create_table()?
        };
        for (key, value) in params {
            if key == "__art3m1s_params_key" {
                continue;
            }
            table.set(key.as_str(), value.as_str())?;
        }
        let engine: Value = lua.globals().get("__engine")?;
        let args = MultiValue::from_vec(vec![engine, Value::Table(table)]);
        Ok(Some(Self::from_function(lua, function, args)?))
    }

    pub fn pending_request(&self) -> Option<&IoRequest> {
        self.pending.as_ref()
    }

    pub fn take_result(&mut self) -> Option<MultiValue> {
        self.result.take()
    }

    /// A missing response while waiting leaves the coroutine untouched.
    pub fn step(&mut self, response: Option<IoResponse>) -> Result<IoProgress> {
        if self.finished || self.failed {
            return Err(mlua::Error::external("IO task is no longer runnable").into());
        }
        let args = match (self.pending.as_ref(), response) {
            (Some(_), None) => return Ok(IoProgress::Waiting),
            (None, None) if !self.started => std::mem::take(&mut self.initial_args),
            (Some(request), Some(response)) => {
                let value = match (request, response) {
                    (_, IoResponse::Failed { kind, message }) => MultiValue::from_vec(vec![
                        Value::Boolean(false),
                        Value::String(self.lua.create_string(message)?),
                        Value::String(self.lua.create_string(kind.as_str())?),
                    ]),
                    (IoRequest::Read { .. }, IoResponse::Read(bytes)) => {
                        MultiValue::from_vec(vec![
                            Value::Boolean(true),
                            Value::String(self.lua.create_string(bytes)?),
                        ])
                    }
                    (IoRequest::Exists { .. }, IoResponse::Exists(exists)) => {
                        MultiValue::from_vec(vec![Value::Boolean(true), Value::Boolean(exists)])
                    }
                    (IoRequest::ModifiedTime { .. }, IoResponse::ModifiedTime(time)) => {
                        MultiValue::from_vec(vec![
                            Value::Boolean(true),
                            match time {
                                Some(time) => {
                                    Value::String(self.lua.create_string(time.to_string())?)
                                }
                                None => Value::Nil,
                            },
                        ])
                    }
                    (IoRequest::Write { .. }, IoResponse::Written)
                    | (IoRequest::DeleteSave { .. }, IoResponse::Deleted)
                    | (IoRequest::TransferSave { .. }, IoResponse::Transferred) => {
                        MultiValue::from_vec(vec![Value::Boolean(true), Value::Boolean(true)])
                    }
                    _ => {
                        return Err(
                            mlua::Error::external("IO response does not match request").into()
                        );
                    }
                };
                self.pending = None;
                value
            }
            _ => return Err(mlua::Error::external("IO task has no pending request").into()),
        };
        self.started = true;
        let result = self.resume(args);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn resume(&mut self, args: MultiValue) -> Result<IoProgress> {
        let values = self.thread.resume::<MultiValue>(args)?;
        if self.thread.status() == ThreadStatus::Finished {
            self.finished = true;
            self.result = Some(values);
            return Ok(IoProgress::Completed);
        }
        if values.len() == 1
            && let Some(Value::UserData(value)) = values.front()
            && let Ok(request) = value.borrow::<IoRequest>()
        {
            self.pending = Some(request.clone());
            return Ok(IoProgress::Waiting);
        }
        Err(mlua::Error::external("unexpected coroutine yield outside Host IO").into())
    }
}

/// Install on a fresh interpreter. Normal synchronous entry points remain for
/// other interpreters; this interpreter's IO must run through [`IoTask`].
pub fn install(interpreter: &Interpreter) -> Result<()> {
    let lua = interpreter.lua();
    if lua
        .named_registry_value::<bool>("__art3m1s_io_continuation")
        .unwrap_or(false)
    {
        return Ok(());
    }
    let native: AnyUserData = lua.globals().get("__engine")?;
    let methods: mlua::Table = native.metatable()?.get("__index")?;
    let helpers = lua.create_table()?;
    helpers.set("tag", methods.get::<mlua::Function>("tag")?)?;
    helpers.set(
        "mtime",
        lua.create_function(|_, path: String| Ok(IoRequest::ModifiedTime { path }))?,
    )?;
    let query_variables = interpreter.variables_handle();
    let native_call_variables = query_variables.clone();
    helpers.set(
        "unsupported_native_call",
        lua.create_function(move |_, params: mlua::Table| {
            if let Some(raw) = query_param(&params, "result")? {
                let mut store = native_call_variables.lock().unwrap();
                let name = crate::ExpressionEvaluator::new(&store)
                    .resolve_param_str(&raw)
                    .map_err(mlua::Error::external)?;
                if !name.is_empty() {
                    store.set(&name, crate::Value::String(String::new()));
                }
            }
            Ok(())
        })?,
    )?;
    let path_variables = query_variables.clone();
    helpers.set(
        "query_path",
        lua.create_function(move |_, (params, resolved): (mlua::Table, Option<bool>)| {
            let path = query_param(&params, "file")?.unwrap_or_default();
            if resolved == Some(true) {
                return Ok(path);
            }
            crate::ExpressionEvaluator::new(&path_variables.lock().unwrap())
                .resolve_param(&path)
                .map(|value| value.as_string())
                .map_err(mlua::Error::external)
        })?,
    )?;
    helpers.set(
        "complete_query",
        lua.create_function(
            move |_, (params, value, virtual_exe): (mlua::Table, Value, Option<bool>)| {
                let system = query_param(&params, "system")?.unwrap_or_default();
                let name = query_param(&params, "name")?.unwrap_or_default();
                let local_write = query_param(&params, "writelocal")?;
                let result = match system.as_str() {
                    "file_exist" | "file_exists" => {
                        crate::Value::Bool(matches!(value, Value::Boolean(true)))
                    }
                    "file_crc" => {
                        let text = if virtual_exe == Some(true) {
                            query_variables
                                .lock()
                                .unwrap()
                                .get(name.strip_suffix(".check").unwrap_or(&name))
                                .map(|value| value.as_string())
                                .unwrap_or_default()
                        } else if let Value::String(bytes) = value {
                            crate::tags::var_handler::format_crc32(
                                crc32(&bytes.as_bytes()),
                                query_param(&params, "zerox")?.as_deref() == Some("1"),
                                query_param(&params, "caps")?.as_deref() == Some("1"),
                            )
                        } else {
                            String::new()
                        };
                        crate::Value::String(text)
                    }
                    "file_update_time" => {
                        let time = match value {
                            Value::String(value) => value.to_str()?.parse::<i64>().ok(),
                            _ => None,
                        };
                        let text = match time {
                            Some(time) => crate::tags::var_handler::format_update_time(
                                &crate::host_time::utc_components_from_seconds(
                                    query_variables
                                        .lock()
                                        .unwrap()
                                        .host_time()
                                        .map_or(time.div_euclid(1000), |host| {
                                            host.local_unix_seconds(time)
                                        }),
                                ),
                                query_param(&params, "format")?
                                    .as_deref()
                                    .filter(|format| !format.is_empty())
                                    .unwrap_or("yyyy/MM/dd hh:mm:ss"),
                            ),
                            None => query_param(&params, "noexist")?.unwrap_or_default(),
                        };
                        crate::Value::String(text)
                    }
                    _ => return Err(mlua::Error::external("unsupported resumed file query")),
                };
                query_variables.lock().unwrap().with_local_writes(
                    crate::tags::var_handler::param_is_on(local_write.as_ref()),
                    |store| store.set(&name, result),
                );
                Ok(())
            },
        )?,
    )?;
    helpers.set(
        "read",
        lua.create_function(|_, path: String| Ok(IoRequest::Read { path }))?,
    )?;
    helpers.set(
        "exists",
        lua.create_function(|_, path: String| Ok(IoRequest::Exists { path }))?,
    )?;
    helpers.set(
        "delete_save",
        lua.create_function(|_, path: String| Ok(IoRequest::DeleteSave { path }))?,
    )?;
    helpers.set(
        "transfer_save",
        lua.create_function(
            |_, (source, destination, remove_source): (String, String, bool)| {
                Ok(IoRequest::TransferSave {
                    source,
                    destination,
                    remove_source,
                })
            },
        )?,
    )?;
    let delete_context = interpreter.engine_context().clone();
    helpers.set(
        "warn_delete",
        lua.create_function(move |_, (path, error): (String, String)| {
            delete_context.lock().unwrap().callbacks.debug(
                0,
                &format!("save delete failed for {path}: {error}"),
                false,
            );
            Ok(())
        })?,
    )?;
    helpers.set(
        "write",
        lua.create_function(|_, (path, data): (String, mlua::String)| {
            Ok(IoRequest::Write {
                path,
                data: data.as_bytes().to_vec(),
            })
        })?,
    )?;
    helpers.set(
        "compile",
        lua.create_function(|lua, (source, name): (mlua::String, String)| {
            lua.load(source.as_bytes().as_ref())
                .set_name(name)
                .into_function()
        })?,
    )?;
    let ctx = interpreter.engine_context().clone();
    let emote_support = ctx.clone();
    helpers.set(
        "emote_model_args",
        lua.create_function(move |_, params: mlua::Table| {
            let id: String = params.get("id")?;
            let width: u32 = params.get("width")?;
            let height: u32 = params.get("height")?;
            let files: mlua::Table = params.get("files")?;
            if files.raw_len() != 1 {
                return Err(mlua::Error::external(
                    "E-Mote requires one embedded-texture PSB",
                ));
            }
            let file = files.raw_get::<String>(1)?;
            let path = emote_support
                .lock()
                .unwrap()
                .callbacks
                .resolve_emote_model_path(&file)
                .map_err(mlua::Error::external)?;
            Ok((id, path, width, height))
        })?,
    )?;
    let emote_context = ctx.clone();
    helpers.set(
        "install_emote_model",
        lua.create_function(move |lua, (id, path, width, height, bytes): (String, String, u32, u32, mlua::String)| {
            let next = emote_context.lock().unwrap().callbacks.create_emote_layer_from_bytes(
                &id, &path, bytes.as_bytes().to_vec(), width, height,
            ).map_err(mlua::Error::external)?;
            lua.create_userdata(crate::lua_engine::EmoteLayerApi::new(emote_context.clone(), id, next))
        })?,
    )?;
    let probe = lua.create_userdata(
        BufferedFile::from_bytes(String::new(), "r", ctx.clone(), vec![])
            .map_err(mlua::Error::external)?,
    )?;
    let file_methods: mlua::Table = probe.metatable()?.get("__index")?;
    helpers.set(
        "file",
        lua.create_function(
            move |lua, (path, mode, data): (String, String, mlua::String)| {
                let file =
                    BufferedFile::from_bytes(path, &mode, ctx.clone(), data.as_bytes().to_vec())
                        .map_err(mlua::Error::external)?;
                lua.create_userdata(file)
            },
        )?,
    )?;
    helpers.set(
        "snapshot",
        lua.create_function(|lua, file: AnyUserData| {
            match file.borrow::<BufferedFile>()?.write_snapshot()? {
                Some((path, data)) => Ok((Some(path), Some(lua.create_string(data)?))),
                None => Ok((None, None)),
            }
        })?,
    )?;
    helpers.set(
        "ack",
        lua.create_function(|_, file: AnyUserData| {
            file.borrow_mut::<BufferedFile>()?.acknowledge_write()
        })?,
    )?;
    helpers.set(
        "close",
        lua.create_function(|_, file: AnyUserData| {
            file.borrow_mut::<BufferedFile>()?.mark_closed();
            Ok(())
        })?,
    )?;
    helpers.set(
        "closed",
        lua.create_function(|_, file: AnyUserData| Ok(file.borrow::<BufferedFile>()?.is_closed()))?,
    )?;
    let functions: mlua::Table = lua
        .load(include_str!("io_continuation.luau"))
        .call(helpers)?;
    methods.set("file", functions.get::<mlua::Function>("file")?)?;
    methods.set("include", functions.get::<mlua::Function>("include")?)?;
    methods.set(
        "createEmoteLayer",
        functions.get::<mlua::Function>("create_emote_layer")?,
    )?;
    methods.set("isFileExists", functions.get::<mlua::Function>("exists")?)?;
    methods.set("tag", functions.get::<mlua::Function>("tag")?)?;
    lua.set_named_registry_value(
        "__art3m1s_io_file_query",
        functions.get::<mlua::Function>("file_query")?,
    )?;
    lua.set_named_registry_value(
        "__art3m1s_io_delete_save",
        functions.get::<mlua::Function>("delete_save")?,
    )?;
    let flush: mlua::Function = functions.get("flush")?;
    let close: mlua::Function = functions.get("close")?;
    file_methods.set("flush", flush)?;
    file_methods.set("close", close)?;
    lua.globals()
        .set("io", functions.get::<mlua::Table>("io")?)?;
    lua.set_named_registry_value("__art3m1s_io_continuation", true)?;
    Ok(())
}

pub(crate) fn delete_save_task(lua: &Lua, path: &str) -> Result<IoTask> {
    let function = lua.named_registry_value("__art3m1s_io_delete_save")?;
    let args = MultiValue::from_vec(vec![
        Value::String(lua.create_string(path)?),
        Value::Boolean(true),
    ]);
    IoTask::from_function(lua, function, args)
}

pub fn file_query_task(
    lua: &Lua,
    params: &std::collections::HashMap<String, String>,
) -> Result<IoTask> {
    let function = lua.named_registry_value("__art3m1s_io_file_query")?;
    let table = lua.create_table()?;
    for (key, value) in params {
        table.set(key.as_str(), value.as_str())?;
    }
    let engine: Value = lua.globals().get("__engine")?;
    IoTask::from_function(
        lua,
        function,
        MultiValue::from_vec(vec![engine, Value::Table(table), Value::Boolean(true)]),
    )
}

fn query_param(table: &mlua::Table, key: &str) -> mlua::Result<Option<String>> {
    Ok(match table.get::<Value>(key)? {
        Value::String(value) => Some(value.to_str()?.to_string()),
        Value::Integer(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Boolean(value) => Some(value.to_string()),
        _ => None,
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod scenario_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn interpreter() -> Interpreter {
        let interpreter = Interpreter::default();
        install(&interpreter).unwrap();
        install(&interpreter).unwrap();
        interpreter
    }

    #[test]
    fn nested_include_and_dynamic_read_resume_without_replaying_locals_or_side_effects() {
        let i = interpreter();
        let mut task = IoTask::compile(
            i.lua(),
            br#"
            count = (count or 0) + 1
            local saved = { value = 73 }
            __engine:include('lib.lua')
            result = dynamic(__engine, 'data' .. count)
            assert(saved.value == 73 and count == 1)
        "#,
            "main",
        )
        .unwrap();
        assert_eq!(task.step(None).unwrap(), IoProgress::Waiting);
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Read {
                path: "lib.lua".into()
            })
        );
        assert_eq!(task.step(None).unwrap(), IoProgress::Waiting);
        assert_eq!(
            task.step(Some(IoResponse::Read(
                b"function dynamic(e, path) return e:file(path) end".to_vec()
            )))
            .unwrap(),
            IoProgress::Waiting
        );
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Read {
                path: "data1".into()
            })
        );
        assert!(task.step(Some(IoResponse::Exists(false))).is_err());
        assert_eq!(
            task.step(Some(IoResponse::Read(vec![0, 255, 128])))
                .unwrap(),
            IoProgress::Completed
        );
        assert_eq!(i.lua().globals().get::<i32>("count").unwrap(), 1);
        assert_eq!(
            i.lua()
                .globals()
                .get::<mlua::String>("result")
                .unwrap()
                .as_bytes()
                .as_ref(),
            &[0, 255, 128]
        );
        assert!(task.step(None).is_err());
    }

    #[test]
    fn close_waits_for_write_confirmation_and_failed_flush_keeps_the_file_retryable() {
        let i = interpreter();
        let mut task = IoTask::compile(
            i.lua(),
            br#"
            count = (count or 0) + 1
            local f = assert(io.open('save/test', 'w+'))
            assert(f:write('value') == f)
            local ok, message = pcall(function() return io.close(f) end)
            assert(not ok and string.find(message, 'denied'))
            assert(io.type(f) == 'file')
            assert(f:flush())
            assert(f:seek('set', 0) == 0 and f:read('*a') == 'value')
            assert(f:close() and io.type(f) == 'closed file' and count == 1)
        "#,
            "write",
        )
        .unwrap();
        assert_eq!(task.step(None).unwrap(), IoProgress::Waiting);
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Write {
                path: "save/test".into(),
                data: vec![]
            })
        );
        task.step(Some(IoResponse::Written)).unwrap();
        let write = IoRequest::Write {
            path: "save/test".into(),
            data: b"value".to_vec(),
        };
        assert_eq!(task.pending_request(), Some(&write));
        task.step(Some(IoResponse::Failed {
            kind: IoFailure::Denied,
            message: "denied".into(),
        }))
        .unwrap();
        assert_eq!(task.pending_request(), Some(&write));
        assert_eq!(
            task.step(Some(IoResponse::Written)).unwrap(),
            IoProgress::Completed
        );
    }

    #[test]
    fn append_only_creates_missing_files_and_preserves_other_failures() {
        let i = interpreter();
        let mut task = IoTask::compile(
            i.lua(),
            br#"
            local f, message = io.open('save/denied', 'a')
            assert(f == nil and message == 'denied')
            f = assert(io.open('save/new', 'a'))
            f:write('x')
            assert(f:close())
            local n = 0
            for line in io.lines('empty') do n = n + 1 end
            assert(n == 0)
        "#,
            "append",
        )
        .unwrap();
        task.step(None).unwrap();
        task.step(Some(IoResponse::Failed {
            kind: IoFailure::Denied,
            message: "denied".into(),
        }))
        .unwrap();
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Read {
                path: "save/new".into()
            })
        );
        task.step(Some(IoResponse::Failed {
            kind: IoFailure::NotFound,
            message: "missing".into(),
        }))
        .unwrap();
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Write {
                path: "save/new".into(),
                data: vec![]
            })
        );
        task.step(Some(IoResponse::Written)).unwrap();
        task.step(Some(IoResponse::Written)).unwrap();
        assert_eq!(
            task.pending_request(),
            Some(&IoRequest::Read {
                path: "empty".into()
            })
        );
        assert_eq!(
            task.step(Some(IoResponse::Read(vec![]))).unwrap(),
            IoProgress::Completed
        );
    }
}
