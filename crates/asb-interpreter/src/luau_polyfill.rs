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
    let globals = lua.globals();
    let io_lib = match globals.get::<mlua::Table>("io") {
        Ok(table) => table,
        Err(_) => {
            let table = lua.create_table()?;
            globals.set("io", table.clone())?;
            table
        }
    };

    let open_ctx = Arc::clone(&engine_ctx);
    io_lib.set(
        "open",
        lua.create_function(move |lua, (path, mode): (String, Option<String>)| {
            let mode = mode.unwrap_or_else(|| "r".to_string());
            if !mode.contains('w') && !mode.contains('a') {
                return Ok((mlua::Nil, Some(format!("unsupported io.open mode: {mode}"))));
            }
            let append = mode.contains('a');
            let file = lua.create_userdata(BufferedWriter {
                path,
                data: Vec::new(),
                closed: false,
                append,
                engine_ctx: Arc::clone(&open_ctx),
            })?;
            Ok((mlua::Value::UserData(file), None::<String>))
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

    Ok(())
}

struct BufferedWriter {
    path: String,
    data: Vec<u8>,
    closed: bool,
    append: bool,
    engine_ctx: Arc<Mutex<EngineContext>>,
}

impl UserData for BufferedWriter {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("write", |_, this, values: mlua::MultiValue| {
            if this.closed {
                return Err(mlua::Error::RuntimeError(
                    "attempt to write a closed file".to_string(),
                ));
            }
            for value in values {
                match value {
                    mlua::Value::String(s) => this.data.extend_from_slice(s.as_bytes().as_ref()),
                    mlua::Value::Integer(n) => {
                        this.data.extend_from_slice(n.to_string().as_bytes())
                    }
                    mlua::Value::Number(n) => this.data.extend_from_slice(n.to_string().as_bytes()),
                    mlua::Value::Boolean(b) => {
                        this.data
                            .extend_from_slice(if b { b"true" } else { b"false" })
                    }
                    mlua::Value::Nil => {}
                    other => this.data.extend_from_slice(format!("{other:?}").as_bytes()),
                }
            }
            Ok(true)
        });

        methods.add_method_mut("close", |_, this, ()| {
            if this.closed {
                return Ok(true);
            }
            if this.append {
                return Err(mlua::Error::external(
                    "append mode is not supported by host file writer",
                ));
            }
            this.engine_ctx
                .lock()
                .unwrap()
                .callbacks
                .file_write(&this.path, &this.data)
                .map_err(mlua::Error::external)?;
            this.closed = true;
            Ok(true)
        });

        methods.add_method("flush", |_, _, ()| Ok(true));
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
