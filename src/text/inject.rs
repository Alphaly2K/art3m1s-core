//! 文本注入接口。
//!
//! 用于汉化补丁等外部系统的文本替换/修改。注入器在文本渲染前拦截原始内容并返回
//! 替换文本，原文本不会被丢弃（注入器返回 `None` 时仍用原文）。

// ---------------------------------------------------------------------------
// TextInject trait
// ---------------------------------------------------------------------------

/// 文本注入回调。
///
/// 在 `TextRenderer::push_text()` 内部，原始文本先交给所有已注册的注入器。
/// 每个注入器对文本进行一次可能的替换。
///
/// # 汉化/本地化补丁典型用法
///
/// ```rust,ignore
/// struct ChinesePatch {
///     dict: HashMap<String, String>,
/// }
///
/// impl TextInject for ChinesePatch {
///     fn inject(&self, content: &str) -> Option<String> {
///         self.dict.get(content).cloned()
///     }
/// }
/// ```
///
/// 注入器的 `inject` 会被链式调用：前一个注入器的输出作为下一个的输入。
pub trait TextInject: Send + Sync {
    /// 对输入文本进行注入/替换。
    ///
    /// - 返回 `Some(new_text)` 表示替换为新文本
    /// - 返回 `None` 表示不修改，使用原文本（或上一个注入器的输出）
    fn inject(&self, text: &str) -> Option<String>;

    /// 注入器名称，用于日志/调试。
    fn name(&self) -> &str {
        "unnamed"
    }
}

// ---------------------------------------------------------------------------
// InjectionChain
// ---------------------------------------------------------------------------

/// 注入器链：按注册顺序依次对文本执行注入。
#[derive(Default)]
pub struct InjectionChain {
    injectors: Vec<Box<dyn TextInject>>,
}

impl InjectionChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// 在链尾追加一个注入器。
    pub fn push(&mut self, injector: Box<dyn TextInject>) {
        self.injectors.push(injector);
    }

    /// 移除所有注入器。
    pub fn clear(&mut self) {
        self.injectors.clear();
    }

    /// 获取注入器数量。
    pub fn len(&self) -> usize {
        self.injectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.injectors.is_empty()
    }

    /// 对文本运行全部注入器，返回最终文本。
    ///
    /// 若无任何注入器修改文本，返回原始内容。
    pub fn run(&self, text: &str) -> String {
        let mut current = text.to_string();
        for injector in &self.injectors {
            if let Some(replacement) = injector.inject(&current) {
                current = replacement;
            }
        }
        current
    }
}

impl std::fmt::Debug for InjectionChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.injectors.iter().map(|i| i.name()).collect();
        f.debug_list().entries(&names).finish()
    }
}

// ---------------------------------------------------------------------------
// 内置注入器示例
// ---------------------------------------------------------------------------

/// 基于匹配表的简单替换注入器。
///
/// 适用于不需要逻辑、仅做字符串→字符串映射的汉化场景。
///
/// # 示例
/// ```rust,ignore
/// let mut table = HashMap::new();
/// table.insert("こんにちは".to_string(), "你好".to_string());
/// let injector = MapTextInject::new("zh-CN", table);
/// ```
pub struct MapTextInject {
    name: String,
    table: std::collections::HashMap<String, String>,
}

impl MapTextInject {
    pub fn new(name: impl Into<String>, table: std::collections::HashMap<String, String>) -> Self {
        Self {
            name: name.into(),
            table,
        }
    }
}

impl TextInject for MapTextInject {
    fn inject(&self, text: &str) -> Option<String> {
        self.table.get(text).cloned()
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// 条件注入器
// ---------------------------------------------------------------------------

/// 仅在满足条件时应用的注入器包装。
///
/// `condition` 是一个闭包，返回 `true` 时该注入器才会执行。
///
/// # 示例
/// ```rust,ignore
/// let cond_inject = ConditionalInject::new(
///     my_injector,
///     |_text| scr.mw.mode == "adv",
/// );
/// ```
pub struct ConditionalInject<I, F> {
    inner: I,
    name: String,
    condition: F,
}

impl<I: TextInject, F: Fn(&str) -> bool + Send + Sync> ConditionalInject<I, F> {
    pub fn new(inner: I, name: impl Into<String>, condition: F) -> Self {
        Self {
            inner,
            name: name.into(),
            condition,
        }
    }
}

impl<I: TextInject, F: Fn(&str) -> bool + Send + Sync> TextInject for ConditionalInject<I, F> {
    fn inject(&self, text: &str) -> Option<String> {
        if (self.condition)(text) {
            self.inner.inject(text)
        } else {
            None
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}
