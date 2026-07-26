//! ASB 脚本解释器库
//!
//! 这是一个用于解释执行 ASB (Artemis Engine) 脚本的库。
//! ASB 是一种二进制脚本格式，常用于视觉小说游戏引擎。
//!
//! # 特性
//!
//! - 支持 ASB 二进制脚本解码（通过 `asb-decrypt` 库）
//! - 标签系统和参数解析
//! - 变量系统（普通变量、全局变量、临时变量、系统变量）
//! - 表达式求值（算术、逻辑、比较运算）
//! - Lua 脚本集成（通过 `mlua`）
//! - 可扩展的标签处理器系统
//! - 事件回调机制
//! - 存档/读档支持
//!
//! # 快速开始
//!
//! ```rust,no_run
//! use asb_interpreter::{Interpreter, InterpreterConfig, Event, CallbackResult, ExecutionResult};
//!
//! // 创建解释器
//! let mut interpreter = Interpreter::new(InterpreterConfig::default());
//!
//! // 加载脚本
//! let script = r#"
//! *main
//! [var name="message" data="'Hello, World!'"]
//! [return]
//! "#;
//! interpreter.load_script("test", script).unwrap();
//!
//! // 设置事件回调
//! interpreter.set_callback(|event| {
//!     match event {
//!         Event::Text { content } => {
//!             println!("文本: {}", content);
//!             CallbackResult::Continue
//!         }
//!         _ => CallbackResult::Continue,
//!     }
//! });
//!
//! // 执行脚本
//! interpreter.start("test", "main").unwrap();
//! interpreter.run().unwrap();
//! ```

pub mod error;
pub mod event;
pub mod expression;
pub mod interpreter;
pub mod lua_engine;
#[cfg(feature = "backend-luau")]
pub mod luau_polyfill;
pub mod r#macro;
pub mod script;
pub mod tags;
pub mod variable;

#[cfg(all(feature = "backend-lua51", feature = "backend-luau"))]
compile_error!("features `backend-lua51` and `backend-luau` are mutually exclusive");

#[cfg(not(any(feature = "backend-lua51", feature = "backend-luau")))]
compile_error!("select one Lua backend feature: `backend-lua51` or `backend-luau`");

// 重新导出主要类型
pub use error::{Error, Result};
pub use event::{
    CallbackResult, Event, EventCallback, LayerProperties, LoadMaskAction, SaveAction,
    ScriptFileLoader, ScriptLoader, SystemUiAction, TransitionEvent, WaitReason,
};
pub use expression::ExpressionEvaluator;
pub use interpreter::{CallFrame, ExecutionResult, Interpreter, InterpreterConfig};
pub use lua_engine::{
    DefaultEngineCallbacks, EmoteLayerApi, EmoteLayerCommand, EngineApi, EngineCallbacks,
    EngineContext,
};
pub use r#macro::{Macro, MacroRegistry};
pub use script::{Instruction, Script};
pub use tags::{ExecutionContext, TagHandler, TagRegistry, TagResult};
pub use variable::{Value, VariableStore};

/// 库版本
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
