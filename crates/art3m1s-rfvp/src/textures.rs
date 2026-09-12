use std::collections::HashMap;

use art3m1s_render::{TextureId, TextureInfo};

use crate::protocol::TextureHandle;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureBinding {
    pub texture: TextureId,
    pub info: TextureInfo,
}

#[derive(Debug, Clone, Default)]
pub struct TextureBindings {
    entries: HashMap<u32, TextureBinding>,
    solid: Option<TextureBinding>,
}

impl TextureBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        handle: TextureHandle,
        texture: TextureId,
        info: TextureInfo,
    ) -> Option<TextureBinding> {
        self.entries
            .insert(handle.0, TextureBinding { texture, info })
    }

    pub fn remove(&mut self, handle: TextureHandle) -> Option<TextureBinding> {
        self.entries.remove(&handle.0)
    }

    pub fn get(&self, handle: TextureHandle) -> Option<TextureBinding> {
        self.entries.get(&handle.0).copied()
    }

    pub fn set_solid(&mut self, texture: TextureId, info: TextureInfo) {
        self.solid = Some(TextureBinding { texture, info });
    }

    pub fn solid(&self) -> Option<TextureBinding> {
        self.solid
    }
}
