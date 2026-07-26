//! 用于测试的假后端。
//!
//! [`MockProvider`] 给每个不同的资源名分配一个稳定的 [`TextureId`]，并返回固定
//! 尺寸，便于在不接 GPU 的情况下断言"哪张纹理被画了、按什么顺序、什么变换"。

use crate::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};
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
    /// 资源名 → CPU 侧 RGBA 像素（lyedit 测试用；upload_rgba 也会记录）。
    pixels: HashMap<String, (u32, u32, Vec<u8>)>,
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

    /// 预置某资源名的 CPU 像素（`pixels_of` 读取，lyedit 测试用）。
    pub fn put_pixels(&mut self, name: &str, width: u32, height: u32, data: Vec<u8>) {
        self.pixels.insert(name.to_string(), (width, height, data));
    }

    /// 读取（上传或预置的）CPU 像素副本，断言 lyedit 结果时用。
    pub fn pixels_named(&self, name: &str) -> Option<&(u32, u32, Vec<u8>)> {
        self.pixels.get(name)
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

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let id = if let Some(id) = self.by_name.get(name) {
            *id
        } else {
            let id = TextureId(self.next);
            self.next += 1;
            self.by_name.insert(name.to_string(), id);
            self.by_id.insert(id.0, name.to_string());
            id
        };
        self.pixels
            .insert(name.to_string(), (width, height, data.to_vec()));
        Some((id, TextureInfo { width, height }))
    }

    /// file+mask 合成在 mock 里退化为解析组合名，便于断言蒙版路径被走到。
    fn resolve_with_mask(&mut self, file: &str, mask: &str) -> Option<(TextureId, TextureInfo)> {
        let name = crate::render_pipeline::draw::masked_texture_name(file, mask);
        self.resolve(&name)
    }

    fn pixels_of(&mut self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        self.pixels.get(name).cloned()
    }
}
