use super::{SceneProjection, ScriptIoProgress, ScriptIoSession};
use crate::compositor::{BlendMode, TextureId, TextureInfo, TextureProvider, build};
use crate::host::frame::{FrameSnapshot, FrameSprite, FrameTexture, SpriteBlend};
use crate::host::protocol::ProtocolError;
use std::collections::HashMap;
use std::sync::Arc;

impl ScriptIoSession {
    pub(in crate::host) fn publish_frame(
        &mut self,
        progress: ScriptIoProgress,
    ) -> Result<(), ProtocolError> {
        if self.project_loading
            || self.shutdown.is_closing_or_closed()
            || self.operation.is_some()
            || !matches!(
                progress,
                ScriptIoProgress::Event | ScriptIoProgress::Completed | ScriptIoProgress::Waiting
            )
        {
            return Ok(());
        }
        let (Some(scene), Some(frame)) = (&self.scene, self.frame) else {
            return Ok(());
        };
        let mut scene = scene.scene.lock().unwrap();
        if !scene.dirty {
            return Ok(());
        }
        scene.dirty = false;
        let config = self.interpreter.config();
        let stage = [config.stage_width, config.stage_height];
        match scene.build_frame(self.transport.state().epoch, frame.id, stage) {
            Some(snapshot) => self.frames.publish(snapshot),
            None => {
                self.frames.clear();
                Ok(())
            }
        }
    }
}

impl SceneProjection {
    fn build_frame(
        &self,
        session_epoch: u64,
        host_frame_id: u64,
        stage_size: [u32; 2],
    ) -> Option<FrameSnapshot> {
        let mut textures = CpuTextureView {
            assets: &self.textures,
            revisions: HashMap::new(),
            table: Vec::new(),
        };
        let scene = self.compositor.scene();
        let frame = build::build_frame(scene, self.compositor.clock_ms(), &mut textures, None);
        if !frame.shader_groups.is_empty() || !frame.mask_commands.is_empty() {
            return None;
        }
        let mut sprites = Vec::with_capacity(frame.commands.len());
        for (index, command) in frame.commands.iter().enumerate() {
            if command.shader.is_some()
                || command.mesh.is_some()
                || command.stencil.is_some()
                || command.native_emote.is_some()
            {
                return None;
            }
            let key = frame.command_key(index)?;
            let mask = scene
                .get(&key.layer_id)
                .filter(|layer| layer.file.as_deref().is_some_and(|file| !file.is_empty()))
                .and_then(|layer| layer.mask.as_deref())
                .and_then(|mask| textures.assets.get(mask)?.clone())
                .filter(|mask| {
                    mask.width == command.size.width && mask.height == command.size.height
                })
                .map(|mask| textures.record(mask).0.0 as usize);
            let transform = command.transform.to_cols_array();
            if !transform
                .into_iter()
                .chain(command.clip.quad_size)
                .chain(command.clip.uv_offset)
                .chain(command.clip.uv_scale)
                .chain(command.clip_bounds.into_iter().flatten())
                .chain(command.color.multiply)
                .chain([command.opacity])
                .all(f32::is_finite)
            {
                return None;
            }
            sprites.push(FrameSprite {
                layer_id: key.layer_id.clone(),
                texture: command.texture.0 as usize,
                mask,
                transform,
                quad_size: command.clip.quad_size,
                uv_offset: command.clip.uv_offset,
                uv_scale: command.clip.uv_scale,
                clip_bounds: command.clip_bounds,
                opacity: command.opacity,
                color_multiply: command.color.multiply,
                grayscale: command.color.grayscale,
                negative: command.color.negative,
                blend: match command.blend {
                    BlendMode::Alpha => SpriteBlend::Alpha,
                    BlendMode::Add => SpriteBlend::Add,
                    BlendMode::Screen => SpriteBlend::Screen,
                    BlendMode::Multiply => SpriteBlend::Multiply,
                    _ => return None,
                },
            });
        }
        Some(FrameSnapshot {
            session_epoch,
            render_frame_id: 0,
            host_frame_id,
            stage_size,
            sprites: sprites.into_boxed_slice(),
            textures: textures.table.into_boxed_slice(),
        })
    }
}

struct CpuTextureView<'a> {
    assets: &'a HashMap<String, Option<Arc<FrameTexture>>>,
    revisions: HashMap<u64, TextureId>,
    table: Vec<Arc<FrameTexture>>,
}

impl CpuTextureView<'_> {
    fn record(&mut self, texture: Arc<FrameTexture>) -> (TextureId, TextureInfo) {
        let info = TextureInfo {
            width: texture.width,
            height: texture.height,
        };
        let id = self.revisions.entry(texture.revision).or_insert_with(|| {
            let id = TextureId(self.table.len() as u64);
            self.table.push(texture);
            id
        });
        (*id, info)
    }
}

impl TextureProvider for CpuTextureView<'_> {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        Some(self.record(self.assets.get(name)?.clone()?))
    }

    fn upload_rgba(
        &mut self,
        _: &str,
        _: u32,
        _: u32,
        _: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        None
    }

    fn solid_texture(&mut self, rgba: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        let revision = (1 << 63) | u64::from(u32::from_le_bytes(rgba));
        let info = TextureInfo {
            width: 1,
            height: 1,
        };
        if let Some(&id) = self.revisions.get(&revision) {
            return Some((id, info));
        }
        Some(self.record(Arc::new(FrameTexture {
            revision,
            width: 1,
            height: 1,
            rgba: Box::new(rgba),
        })))
    }
}
