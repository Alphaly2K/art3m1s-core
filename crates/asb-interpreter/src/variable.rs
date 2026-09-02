//! 变量存储系统
//!
//! 支持四种变量类型：
//! - 普通变量：局部作用域
//! - 全局变量 (g.)：跨存档持久化
//! - 临时变量 (t.)：不写入存档
//! - 系统变量 (s.)：系统配置相关

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// 变量值
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Value {
    /// 整数值
    Int(i64),
    /// 浮点数值
    Float(f64),
    /// 字符串值
    String(String),
    /// 布尔值
    Bool(bool),
    /// 空值
    Null,
}

impl Value {
    /// 转换为整数
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Float(n) => Some(*n as i64),
            Value::Bool(b) => Some(if *b { 1 } else { 0 }),
            Value::String(s) => s.parse().ok(),
            Value::Null => Some(0),
        }
    }

    /// 转换为浮点数
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Int(n) => Some(*n as f64),
            Value::Float(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => s.parse().ok(),
            Value::Null => Some(0.0),
        }
    }

    /// 转换为布尔值
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
        }
    }

    /// 转换为字符串
    pub fn as_string(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(n) => n.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
        }
    }

    /// 判断是否为数值类型
    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::Bool(b) => write!(f, "{}", if *b { 1 } else { 0 }),
            Value::String(s) => write!(f, "{}", s),
            Value::Null => Ok(()),
        }
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Int(n as i64)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Float(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

/// 变量存储
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariableStore {
    /// 普通变量
    #[serde(default)]
    local: HashMap<String, Value>,
    /// 全局变量 (g.)
    #[serde(default)]
    global: HashMap<String, Value>,
    /// 临时变量 (t.)
    #[serde(skip)]
    temp: HashMap<String, Value>,
    /// 系统变量 (s.)
    #[serde(default)]
    system: HashMap<String, Value>,
    /// 目标平台标识（windows/android/ios/wasm 等），供 `[var system="os"]` 返回。
    /// 不参与存档序列化——它由运行时配置决定，而非游戏进度的一部分。
    #[serde(skip)]
    platform: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    macro_scopes: Vec<MacroScope>,
    #[serde(skip)]
    write_macro_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MacroScope {
    depth: usize,
    values: HashMap<String, Value>,
}

impl VariableStore {
    /// 创建新的变量存储
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置目标平台标识（windows/android/ios/wasm 等）。
    pub fn set_platform(&mut self, platform: impl Into<String>) {
        self.platform = platform.into();
    }

    /// 目标平台标识。空串表示未配置。
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// 获取变量值
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.macro_scopes
            .last()
            .and_then(|scope| scope.values.get(name))
            .or_else(|| self.get_nonlocal(name))
    }

    fn get_nonlocal(&self, name: &str) -> Option<&Value> {
        if let Some(stripped) = name.strip_prefix("g.") {
            self.global.get(stripped)
        } else if let Some(stripped) = name.strip_prefix("t.") {
            self.temp.get(stripped)
        } else if let Some(stripped) = name.strip_prefix("s.") {
            self.system.get(stripped)
        } else {
            self.local.get(name)
        }
    }

    /// 设置变量值
    pub fn set(&mut self, name: &str, value: Value) {
        if self.write_macro_local {
            if let Some(scope) = self.macro_scopes.last_mut() {
                scope.values.insert(name.into(), value);
                return;
            }
        }
        if let Some(stripped) = name.strip_prefix("g.") {
            self.global.insert(stripped.to_string(), value);
        } else if let Some(stripped) = name.strip_prefix("t.") {
            self.temp.insert(stripped.to_string(), value);
        } else if let Some(stripped) = name.strip_prefix("s.") {
            self.system.insert(stripped.to_string(), value);
        } else {
            self.local.insert(name.to_string(), value);
        }
    }

    /// 删除变量
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        if self.write_macro_local {
            if let Some(scope) = self.macro_scopes.last_mut() {
                return scope.values.remove(name);
            }
        }
        if let Some(stripped) = name.strip_prefix("g.") {
            self.global.remove(stripped)
        } else if let Some(stripped) = name.strip_prefix("t.") {
            self.temp.remove(stripped)
        } else if let Some(stripped) = name.strip_prefix("s.") {
            self.system.remove(stripped)
        } else {
            self.local.remove(name)
        }
    }

    /// 检查变量是否存在
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn contains_macro_local(&self, name: &str) -> bool {
        self.macro_scopes.last().map_or_else(
            || self.local.contains_key(name),
            |scope| scope.values.contains_key(name),
        )
    }

    pub(crate) fn push_macro_scope(&mut self, depth: usize, args: &HashMap<String, String>) {
        self.macro_scopes.push(MacroScope {
            depth,
            values: args
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        });
    }

    pub(crate) fn retain_macro_scopes(&mut self, depth: usize) {
        self.macro_scopes.retain(|scope| scope.depth <= depth);
    }

    pub(crate) fn with_local_writes<T>(
        &mut self,
        local: bool,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.write_macro_local, local);
        let result = f(self);
        self.write_macro_local = previous;
        result
    }

    pub(crate) fn iter_writable_macro_local(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.macro_scopes
            .last()
            .filter(|_| self.write_macro_local)
            .into_iter()
            .flat_map(|scope| scope.values.iter())
    }

    /// 清除临时变量
    pub fn clear_temp(&mut self) {
        self.temp.clear();
    }

    /// 清除所有变量（包括全局和系统变量）
    pub fn clear_all(&mut self) {
        self.macro_scopes.clear();
        self.local.clear();
        self.global.clear();
        self.temp.clear();
        self.system.clear();
    }

    /// 清除局部和临时变量（用于 reset）
    pub fn reset(&mut self) {
        self.macro_scopes.clear();
        self.local.clear();
        self.temp.clear();
    }

    /// Numbered-save state, including suspended macro arguments but no global/system data.
    pub fn local_snapshot(&self) -> Self {
        Self {
            local: self.local.clone(),
            macro_scopes: self.macro_scopes.clone(),
            ..Self::default()
        }
    }

    /// Restore numbered-save state without rolling back global/system variables.
    pub fn restore_local_snapshot(&mut self, snapshot: &Self) {
        self.reset();
        self.local.clone_from(&snapshot.local);
        self.macro_scopes.clone_from(&snapshot.macro_scopes);
    }

    /// 序列化（用于存档）
    pub fn save(&self) -> crate::error::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// 反序列化（用于读档）
    pub fn load(data: &[u8]) -> crate::error::Result<Self> {
        Ok(serde_json::from_slice(data)?)
    }

    /// 获取普通变量迭代器
    pub fn iter_local(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.local.iter()
    }

    /// 获取全局变量迭代器
    pub fn iter_global(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.global.iter()
    }

    /// 获取临时变量迭代器
    pub fn iter_temp(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.temp.iter()
    }

    /// 获取系统变量迭代器
    pub fn iter_system(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.system.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_conversion() {
        assert_eq!(Value::Int(42).as_string(), "42");
        assert_eq!(Value::String("123".into()).as_int(), Some(123));
        assert!(Value::Bool(true).as_bool());
        assert!(!Value::Bool(false).as_bool());
        assert_eq!(Value::Null.as_int(), Some(0));
    }

    #[test]
    fn test_variable_store() {
        let mut store = VariableStore::new();

        // 普通变量
        store.set("foo", Value::Int(1));
        assert_eq!(store.get("foo"), Some(&Value::Int(1)));

        // 全局变量
        store.set("g.score", Value::Int(100));
        assert_eq!(store.get("g.score"), Some(&Value::Int(100)));

        // 临时变量
        store.set("t.temp", Value::String("test".into()));
        assert_eq!(store.get("t.temp"), Some(&Value::String("test".into())));

        // 清除临时变量
        store.clear_temp();
        assert_eq!(store.get("t.temp"), None);
        assert_eq!(store.get("g.score"), Some(&Value::Int(100)));
    }

    #[test]
    fn test_serialization() {
        let mut store = VariableStore::new();
        store.set("local_var", Value::Int(42));
        store.set("g.global_var", Value::String("hello".into()));
        store.set("t.temp_var", Value::Bool(true));

        let data = store.save().unwrap();
        let loaded = VariableStore::load(&data).unwrap();

        assert_eq!(loaded.get("local_var"), Some(&Value::Int(42)));
        assert_eq!(
            loaded.get("g.global_var"),
            Some(&Value::String("hello".into()))
        );
        // 临时变量不会被序列化
        assert_eq!(loaded.get("t.temp_var"), None);
    }

    #[test]
    fn macro_scopes_survive_save_and_unwind_to_caller() {
        let mut store = VariableStore::new();
        store.set("id", Value::from("base"));
        store.push_macro_scope(1, &HashMap::from([("id".into(), "outer".into())]));
        store.push_macro_scope(3, &HashMap::from([("id".into(), "inner".into())]));
        store.set("g.index", Value::Int(1));
        let snapshot = VariableStore::load(&store.local_snapshot().save().unwrap()).unwrap();
        assert!(snapshot.get("g.index").is_none());
        let mut loaded = VariableStore::new();
        loaded.set("g.index", Value::Int(2));
        loaded.restore_local_snapshot(&snapshot);
        assert_eq!(loaded.get("g.index"), Some(&Value::Int(2)));
        assert_eq!(loaded.get("id"), Some(&Value::from("inner")));
        loaded.retain_macro_scopes(2);
        assert_eq!(loaded.get("id"), Some(&Value::from("outer")));
        loaded.retain_macro_scopes(0);
        assert_eq!(loaded.get("id"), Some(&Value::from("base")));

        let old = br#"{"local":{"id":"legacy"},"global":{},"system":{}}"#;
        let loaded = VariableStore::load(old).unwrap();
        assert_eq!(loaded.get("id"), Some(&Value::from("legacy")));
        assert!(loaded.macro_scopes.is_empty());
    }

    #[test]
    fn local_host_query_overwrites_local_default_and_delete_clears_children() {
        let mut store = VariableStore::new();
        store.set("t.query.visible", Value::Int(9));
        store.push_macro_scope(1, &HashMap::new());
        store.with_local_writes(true, |store| store.set("t.query.visible", Value::Int(1)));
        let params = HashMap::from([
            ("system".into(), "fullscreen".into()),
            ("name".into(), "t.query.visible".into()),
            ("writelocal".into(), "1".into()),
        ]);
        assert_eq!(
            crate::lua_engine::apply_system_var_query(
                &crate::lua_engine::DefaultEngineCallbacks,
                &params,
                &mut store,
            ),
            Some(true)
        );
        assert_eq!(store.get("t.query.visible"), Some(&Value::Int(0)));
        crate::tags::var_handler::apply_var_tag(
            &HashMap::from([
                ("system".into(), "delete".into()),
                ("name".into(), "t.query".into()),
                ("writelocal".into(), "1".into()),
            ]),
            &mut store,
        )
        .unwrap();
        assert!(!store.contains_macro_local("t.query.visible"));
        assert_eq!(store.get("t.query.visible"), Some(&Value::Int(9)));
        store.set("after", Value::Int(5));
        store.retain_macro_scopes(0);
        assert_eq!(store.get("after"), Some(&Value::Int(5)));
    }
}
