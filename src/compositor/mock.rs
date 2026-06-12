//! 用于测试的假后端。
//!
//! [`MockProvider`] 给每个不同的资源名分配一个稳定的 [`TextureId`]，并返回固定
//! 尺寸，便于在不接 GPU 的情况下断言"哪张纹理被画了、按什么顺序、什么变换"。
//! [`MockRenderer`] 把每帧收到的 [`DrawList`] 记录下来供断言。

use crate::compositor::renderer::{DrawList, Renderer, TextureId, TextureInfo, TextureProvider};
use std::collections::HashMap;

/// mock 纹理统一使用的边长（像素）。
pub const TEXTURE_SIZE: u32 = 256;

/// 把资源名映射到稳定句柄的假纹理提供者。
#[derive(Debug, Default)]
pub struct MockProvider {
    by_name: HashMap<String, TextureId>,
    by_id: HashMap<u64, String>,
    next: u64,
    /// 这些名字会被当作"资源缺失"，`resolve` 返回 `None`。
    missing: Vec<String>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// 标记某个资源名为缺失，用于测试解析失败时跳过图层的行为。
    pub fn mark_missing(&mut self, name: &str) {
        self.missing.push(name.to_string());
    }

    /// 反查句柄对应的资源名（断言绘制顺序时用）。
    pub fn name_of(&self, id: TextureId) -> &str {
        self.by_id
            .get(&id.0)
            .map(String::as_str)
            .unwrap_or("<unknown>")
    }
}

impl TextureProvider for MockProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        if self.missing.iter().any(|m| m == name) {
            return None;
        }
        let id = if let Some(id) = self.by_name.get(name) {
            *id
        } else {
            let id = TextureId(self.next);
            self.next += 1;
            self.by_name.insert(name.to_string(), id);
            self.by_id.insert(id.0, name.to_string());
            id
        };
        Some((
            id,
            TextureInfo {
                width: TEXTURE_SIZE,
                height: TEXTURE_SIZE,
            },
        ))
    }
}

/// 记录每帧绘制列表的假渲染器。
#[derive(Debug, Default)]
pub struct MockRenderer {
    pub frames: Vec<DrawList>,
}

impl MockRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 最近一帧。
    pub fn last(&self) -> Option<&DrawList> {
        self.frames.last()
    }
}

impl Renderer for MockRenderer {
    fn render(&mut self, frame: &DrawList) {
        self.frames.push(frame.clone());
    }
}
