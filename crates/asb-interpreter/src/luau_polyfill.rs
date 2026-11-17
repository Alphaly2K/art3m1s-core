use crate::lua_engine::EngineContext;
use mlua::{Lua, ObjectLike, UserData, UserDataMethods};
use std::sync::{Arc, Mutex};

pub fn install(lua: &Lua, engine_ctx: Arc<Mutex<EngineContext>>) -> mlua::Result<()> {
    install_lua51_compat(lua)?;
    install_table_maxn(lua)?;
    install_os_polyfills(lua)?;
    install_io_polyfills(lua, engine_ctx)?;
    Ok(())
}

fn install_lua51_compat(lua: &Lua) -> mlua::Result<()> {
    lua.load(
        r#"
        string.gfind = string.gfind or string.gmatch
        math.mod = math.mod or math.fmod

        if unpack == nil and table.unpack ~= nil then
            unpack = table.unpack
        end

        if loadstring == nil and load ~= nil then
            loadstring = load
        end

        if table.getn == nil then
            table.getn = function(t)
                if type(t) == "table" and type(t.n) == "number" then
                    return t.n
                end
                return #t
            end
        end

        if table.foreach == nil then
            table.foreach = function(t, f)
                for k, v in pairs(t) do
                    local r = f(k, v)
                    if r ~= nil then return r end
                end
            end
        end

        if table.foreachi == nil then
            table.foreachi = function(t, f)
                for i = 1, #t do
                    local r = f(i, t[i])
                    if r ~= nil then return r end
                end
            end
        end

        if gcinfo == nil and collectgarbage ~= nil then
            gcinfo = function()
                return collectgarbage("count")
            end
        end
        "#,
    )
    .exec()
}

fn install_table_maxn(lua: &Lua) -> mlua::Result<()> {
    let table_lib: mlua::Table = lua.globals().get("table")?;
    let maxn = lua.create_function(|_, table: mlua::Table| {
        let mut max = 0_i64;
        for pair in table.pairs::<mlua::Value, mlua::Value>() {
            let (key, _) = pair?;
            let n = match key {
                mlua::Value::Integer(n) => Some(i64::from(n)),
                mlua::Value::Number(n) if n.fract() == 0.0 => Some(n as i64),
                _ => None,
            };
            if let Some(n) = n
                && n > max
            {
                max = n;
            }
        }
        Ok(max)
    })?;
    table_lib.set("maxn", maxn)
}

fn install_os_polyfills(lua: &Lua) -> mlua::Result<()> {
    let os_lib: mlua::Table = lua.globals().get("os")?;

    os_lib.set(
        "time",
        lua.create_function(|_, value: Option<mlua::Table>| {
            if let Some(table) = value {
                let year: i64 = table.get("year")?;
                let month: i64 = table.get("month").unwrap_or(1);
                let day: i64 = table.get("day").unwrap_or(1);
                let hour: i64 = table.get("hour").unwrap_or(12);
                let min: i64 = table.get("min").unwrap_or(0);
                let sec: i64 = table.get("sec").unwrap_or(0);
                return Ok(unix_from_utc_parts(year, month, day, hour, min, sec));
            }

            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0))
        })?,
    )?;

    os_lib.set(
        "date",
        lua.create_function(|lua, (format, time): (Option<String>, Option<i64>)| {
            let format = format.unwrap_or_else(|| "%c".to_string());
            let use_utc = format.starts_with('!');
            let format = if use_utc { &format[1..] } else { &format };
            let seconds = time.unwrap_or_else(current_unix_seconds);
            let parts = if use_utc {
                utc_datetime_from_unix(seconds)
            } else {
                local_datetime_from_unix(seconds).unwrap_or_else(|| utc_datetime_from_unix(seconds))
            };

            if format == "*t" {
                let table = lua.create_table()?;
                table.set("year", parts.year)?;
                table.set("month", parts.month)?;
                table.set("day", parts.day)?;
                table.set("hour", parts.hour)?;
                table.set("min", parts.minute)?;
                table.set("sec", parts.second)?;
                table.set("isdst", false)?;
                return Ok(mlua::Value::Table(table));
            }

            Ok(mlua::Value::String(
                lua.create_string(format_date(format, &parts))?,
            ))
        })?,
    )?;

    os_lib.set(
        "execute",
        lua.create_function(|_, _: mlua::Value| Ok(-1_i64))?,
    )?;
    os_lib.set("getenv", lua.create_function(|_, _: String| Ok(mlua::Nil))?)?;
    os_lib.set(
        "exit",
        lua.create_function(|_, _: mlua::MultiValue| Ok(()))?,
    )?;
    Ok(())
}

fn install_io_polyfills(lua: &Lua, engine_ctx: Arc<Mutex<EngineContext>>) -> mlua::Result<()> {
    let io_lib = lua.create_table()?;
    lua.globals().set("io", io_lib.clone())?;

    let open_ctx = Arc::clone(&engine_ctx);
    io_lib.set(
        "open",
        lua.create_function(move |lua, (path, mode): (String, Option<String>)| {
            match BufferedFile::open(path, mode.as_deref().unwrap_or("r"), Arc::clone(&open_ctx)) {
                Ok(file) => Ok((mlua::Value::UserData(lua.create_userdata(file)?), None)),
                Err(error) => Ok((mlua::Nil, Some(error))),
            }
        })?,
    )?;
    io_lib.set(
        "close",
        lua.create_function(|_, file: Option<mlua::AnyUserData>| {
            if let Some(file) = file {
                file.call_method::<()>("close", ())?;
            }
            Ok(true)
        })?,
    )?;
    io_lib.set(
        "type",
        lua.create_function(|_, value: mlua::Value| {
            let mlua::Value::UserData(file) = value else {
                return Ok(None);
            };
            if !file.is::<BufferedFile>() {
                return Ok(None);
            }
            Ok(Some(if file.borrow::<BufferedFile>()?.closed {
                "closed file"
            } else {
                "file"
            }))
        })?,
    )?;
    io_lib.set(
        "lines",
        lua.create_function(move |lua, path: String| {
            let mut file = BufferedFile::open(path, "r", Arc::clone(&engine_ctx))
                .map_err(mlua::Error::external)?;
            lua.create_function_mut(move |lua, ()| file.read_line(lua, false))
        })?,
    )?;
    Ok(())
}

struct BufferedFile {
    path: String,
    data: Vec<u8>,
    position: usize,
    readable: bool,
    writable: bool,
    append: bool,
    dirty: bool,
    closed: bool,
    engine_ctx: Arc<Mutex<EngineContext>>,
}

impl BufferedFile {
    fn open(
        path: String,
        mode: &str,
        engine_ctx: Arc<Mutex<EngineContext>>,
    ) -> Result<Self, String> {
        let (readable, writable, append, truncate) = match mode {
            "r" | "rb" => (true, false, false, false),
            "r+" | "r+b" | "rb+" => (true, true, false, false),
            "w" | "wb" => (false, true, false, true),
            "w+" | "w+b" | "wb+" => (true, true, false, true),
            "a" | "ab" => (false, true, true, false),
            "a+" | "a+b" | "ab+" => (true, true, true, false),
            _ => return Err(format!("unsupported io.open mode: {mode}")),
        };
        let data = if truncate {
            engine_ctx
                .lock()
                .unwrap()
                .callbacks
                .file_write(&path, &[])
                .map_err(|e| e.to_string())?;
            Vec::new()
        } else {
            let (reader, exists) = {
                let ctx = engine_ctx.lock().unwrap();
                (ctx.file_reader.clone(), ctx.callbacks.is_file_exists(&path))
            };
            let result = reader
                .ok_or_else(|| "host file reader is not installed".to_string())
                .and_then(|reader| reader(&path).map_err(|e| e.to_string()));
            match result {
                Ok(data) => data,
                Err(error) if !append || exists => return Err(error),
                Err(_) => {
                    engine_ctx
                        .lock()
                        .unwrap()
                        .callbacks
                        .file_write(&path, &[])
                        .map_err(|e| e.to_string())?;
                    Vec::new()
                }
            }
        };
        Ok(Self {
            path,
            position: if append { data.len() } else { 0 },
            data,
            readable,
            writable,
            append,
            dirty: false,
            closed: false,
            engine_ctx,
        })
    }

    fn ensure_open(&self) -> mlua::Result<()> {
        if self.closed {
            Err(mlua::Error::external("attempt to use a closed file"))
        } else {
            Ok(())
        }
    }

    fn flush(&mut self) -> mlua::Result<bool> {
        self.ensure_open()?;
        if self.dirty {
            self.engine_ctx
                .lock()
                .unwrap()
                .callbacks
                .file_write(&self.path, &self.data)
                .map_err(mlua::Error::external)?;
            self.dirty = false;
        }
        Ok(true)
    }

    fn read_line(&mut self, lua: &Lua, keep_newline: bool) -> mlua::Result<mlua::Value> {
        self.ensure_open()?;
        if !self.readable {
            return Err(mlua::Error::external("file is not readable"));
        }
        if self.position >= self.data.len() {
            return Ok(mlua::Nil);
        }
        let start = self.position;
        let newline = self.data[start..].iter().position(|&b| b == b'\n');
        self.position = newline.map_or(self.data.len(), |n| start + n + 1);
        let mut end = self.position;
        if !keep_newline && newline.is_some() {
            end -= 1;
            if end > start && self.data[end - 1] == b'\r' {
                end -= 1;
            }
        }
        Ok(mlua::Value::String(
            lua.create_string(&self.data[start..end])?,
        ))
    }

    fn read(&mut self, lua: &Lua, format: mlua::Value) -> mlua::Result<mlua::Value> {
        self.ensure_open()?;
        if !self.readable {
            return Err(mlua::Error::external("file is not readable"));
        }
        let count = match format {
            mlua::Value::Nil => return self.read_line(lua, false),
            mlua::Value::String(format) => match format.to_str()?.as_ref() {
                "*l" => return self.read_line(lua, false),
                "*L" => return self.read_line(lua, true),
                "*a" | "*all" => {
                    let start = self.position.min(self.data.len());
                    self.position = self.position.max(self.data.len());
                    return Ok(mlua::Value::String(lua.create_string(&self.data[start..])?));
                }
                "*n" => return self.read_number(lua),
                _ => return Err(mlua::Error::external("invalid file read format")),
            },
            mlua::Value::Integer(n) if n >= 0 => n as usize,
            mlua::Value::Number(n)
                if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < usize::MAX as f64 =>
            {
                n as usize
            }
            _ => return Err(mlua::Error::external("invalid file read count")),
        };
        if self.position >= self.data.len() {
            return Ok(mlua::Nil);
        }
        let end = self.position.saturating_add(count).min(self.data.len());
        let value = lua.create_string(&self.data[self.position..end])?;
        self.position = end;
        Ok(mlua::Value::String(value))
    }

    fn read_number(&mut self, lua: &Lua) -> mlua::Result<mlua::Value> {
        if self.position >= self.data.len() {
            return Ok(mlua::Nil);
        }
        while self
            .data
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
        let start = self.position;
        let end = self.data[start..]
            .iter()
            .position(|b| !b.is_ascii_hexdigit() && !b"+-.xXpPeE".contains(b))
            .map_or(self.data.len(), |n| start + n);
        for end in (start + 1..=end).rev() {
            if let Some(number) = lua.coerce_number(mlua::Value::String(
                lua.create_string(&self.data[start..end])?,
            ))? {
                self.position = end;
                return Ok(mlua::Value::Number(number));
            }
        }
        Ok(mlua::Nil)
    }
}

impl UserData for BufferedFile {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_function(
            "write",
            |_, (file, values): (mlua::AnyUserData, mlua::MultiValue)| {
                {
                    let mut this = file.borrow_mut::<Self>()?;
                    this.ensure_open()?;
                    if !this.writable {
                        return Err(mlua::Error::external("file is not writable"));
                    }
                    if this.append {
                        this.position = this.data.len();
                    }
                    for value in values {
                        let bytes = match value {
                            mlua::Value::String(s) => s.as_bytes().to_vec(),
                            mlua::Value::Integer(n) => n.to_string().into_bytes(),
                            mlua::Value::Number(n) => n.to_string().into_bytes(),
                            _ => {
                                return Err(mlua::Error::external(
                                    "file write expects strings or numbers",
                                ));
                            }
                        };
                        let end = this
                            .position
                            .checked_add(bytes.len())
                            .ok_or_else(|| mlua::Error::external("file position overflow"))?;
                        if end > this.data.len() {
                            let extra = end - this.data.len();
                            this.data
                                .try_reserve(extra)
                                .map_err(mlua::Error::external)?;
                            this.data.resize(end, 0);
                        }
                        let start = this.position;
                        this.data[start..end].copy_from_slice(&bytes);
                        this.position = end;
                        this.dirty = true;
                    }
                }
                Ok(file)
            },
        );
        methods.add_method_mut("read", |lua, this, mut formats: mlua::MultiValue| {
            if formats.is_empty() {
                formats.push_back(mlua::Nil);
            }
            let mut values = Vec::new();
            for format in formats {
                let value = this.read(lua, format)?;
                let eof = value.is_nil();
                values.push(value);
                if eof {
                    break;
                }
            }
            Ok(mlua::MultiValue::from_vec(values))
        });
        methods.add_method_mut(
            "seek",
            |_, this, (whence, offset): (Option<String>, Option<i64>)| {
                this.ensure_open()?;
                let base = match whence.as_deref().unwrap_or("cur") {
                    "set" => 0,
                    "cur" => this.position,
                    "end" => this.data.len(),
                    _ => return Err(mlua::Error::external("invalid seek mode")),
                };
                let position = usize::try_from(base as i128 + i128::from(offset.unwrap_or(0)))
                    .map_err(|_| mlua::Error::external("invalid seek position"))?;
                this.position = position;
                Ok(position as f64)
            },
        );
        methods.add_method_mut("flush", |_, this, ()| this.flush());
        methods.add_method_mut("close", |_, this, ()| {
            if !this.closed {
                this.flush()?;
                this.closed = true;
            }
            Ok(true)
        });
        methods.add_function("lines", |lua, file: mlua::AnyUserData| {
            lua.create_function_mut(move |lua, ()| file.borrow_mut::<Self>()?.read_line(lua, false))
        });
    }
}

fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy)]
struct DateTimeParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

#[cfg(unix)]
fn local_datetime_from_unix(seconds: i64) -> Option<DateTimeParts> {
    let raw = seconds as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let ptr = unsafe { libc::localtime_r(&raw, tm.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some(DateTimeParts {
        year: i64::from(tm.tm_year) + 1900,
        month: i64::from(tm.tm_mon) + 1,
        day: i64::from(tm.tm_mday),
        hour: i64::from(tm.tm_hour),
        minute: i64::from(tm.tm_min),
        second: i64::from(tm.tm_sec),
    })
}

#[cfg(not(unix))]
fn local_datetime_from_unix(_seconds: i64) -> Option<DateTimeParts> {
    None
}

fn utc_datetime_from_unix(seconds: i64) -> DateTimeParts {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateTimeParts {
        year,
        month,
        day,
        hour: secs_of_day / 3600,
        minute: secs_of_day % 3600 / 60,
        second: secs_of_day % 60,
    }
}

fn unix_from_utc_parts(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64) -> i64 {
    let days = days_from_civil(year, month, day);
    days * 86_400 + hour * 3600 + min * 60 + sec
}

fn format_date(format: &str, parts: &DateTimeParts) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{:04}", parts.year)),
            Some('y') => out.push_str(&format!("{:02}", parts.year.rem_euclid(100))),
            Some('m') => out.push_str(&format!("{:02}", parts.month)),
            Some('d') => out.push_str(&format!("{:02}", parts.day)),
            Some('H') => out.push_str(&format!("{:02}", parts.hour)),
            Some('M') => out.push_str(&format!("{:02}", parts.minute)),
            Some('S') => out.push_str(&format!("{:02}", parts.second)),
            Some('c') => out.push_str(&format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
            )),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
