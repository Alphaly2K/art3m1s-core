//! 错误类型定义

use thiserror::Error;

/// 解释器错误
#[derive(Debug, Error)]
pub enum Error {
    /// 脚本解析错误
    #[error("解析错误 (行 {line}): {message}")]
    ParseError { line: usize, message: String },

    /// 运行时错误
    #[error("运行时错误 (行 {line}): {message}")]
    RuntimeError { line: usize, message: String },

    /// 表达式求值错误
    #[error("表达式错误: {0}")]
    ExpressionError(String),

    /// 变量未定义
    #[error("未定义的变量: {0}")]
    UndefinedVariable(String),

    /// 标签未找到
    #[error("未找到标签: {0}")]
    LabelNotFound(String),

    /// 脚本未找到
    #[error("未找到脚本: {0}")]
    ScriptNotFound(String),

    /// Lua 错误
    #[error("Lua 错误: {0}")]
    LuaError(#[from] mlua::Error),

    /// IO 错误
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),

    /// 序列化错误
    #[error("序列化错误: {0}")]
    SerializeError(String),

    /// ASB 解码错误
    #[error("ASB 解码错误: {0}")]
    DecodeError(String),

    /// 回调中止
    #[error("执行被中止")]
    Aborted,
}

impl From<asb_decrypt::DecodeError> for Error {
    fn from(e: asb_decrypt::DecodeError) -> Self {
        Error::DecodeError(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::SerializeError(e.to_string())
    }
}

/// 结果类型别名
pub type Result<T> = std::result::Result<T, Error>;
