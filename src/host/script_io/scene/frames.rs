use super::{SceneProjection, ScriptIoProgress, ScriptIoSession};
use crate::compositor::{BlendMode, TextureId, TextureInfo, TextureProvider, build};
use crate::host::frame::{FrameSnapshot, FrameSprite, FrameTexture, SpriteBlend};
use crate::host::protocol::ProtocolError;
use crate::render_pipeline::draw::LayerCommandKind;
use crate::text::{TextRenderer, message_layer_scene_id};
use std::collections::HashMap;
use std::sync::Arc;

impl ScriptIoSession {
    pub(in crate::host) fn publish_frame(
        &mut self,
        progress: ScriptIoProgress,
    ) -> Result<(), ProtocolError> {
        if self.project_loading
            || self.shutdown.is_closing_or_closed()
            || self.screenshot.pending_request().is_some()
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
        if !scene.dirty && self.transition.is_none() && !self.frames.latest_is_transition() {
            return Ok(());
        }
        scene.dirty = false;
        let config = self.interpreter.config();
        let stage = [config.stage_width, config.stage_height];
        match scene.build_frame(self.transport.state().epoch, frame.id, stage) {
            Some(mut snapshot) => {
                if let Some(transition) = &self.transition {
                    snapshot.blend_from(
                        &transition.source,
                        transition.progress(frame),
                        transition.rule.as_ref(),
                        transition.vague,
                    );
                }
                self.frames.publish(snapshot)
            }
            None => {
                self.frames.clear();
                Ok(())
            }
        }
    }
}

impl SceneProjection {
    pub(super) fn hit_layers(&mut self, x: i32, y: i32) -> Vec<String> {
        let mut textures = CpuTextureView {
            assets: &self.textures,
            video: &self.video_frames,
            edited: &self.edited,
            atlases: &mut self.text.atlases,
            atlas_revision: &mut self.text.atlas_revision,
            revisions: HashMap::new(),
            table: Vec::new(),
        };
        self.compositor
            .hit_test_all(x as f32, y as f32, &mut textures)
    }

    fn build_frame(
        &mut self,
        session_epoch: u64,
        host_frame_id: u64,
        stage_size: [u32; 2],
    ) -> Option<FrameSnapshot> {
        let mut textures = CpuTextureView {
            assets: &self.textures,
            video: &self.video_frames,
            edited: &self.edited,
            atlases: &mut self.text.atlases,
            atlas_revision: &mut self.text.atlas_revision,
            revisions: HashMap::new(),
            table: Vec::new(),
        };
        let scene = self.compositor.scene();
        let renderer = &mut self.text.renderer;
        let mut text_commands: HashMap<_, _> = renderer
            .build_text_commands(&mut textures)
            .into_iter()
            .filter_map(|(id, commands)| {
                if !self.compositor.is_message_layer_visible(&id) {
                    return None;
                }
                let layer = renderer.font_state().layers.get(&id)?;
                Some((message_layer_scene_id(&id, layer.layered), commands))
            })
            .collect();
        let mut text_for = |id: &str| text_commands.remove(id).unwrap_or_default();
        let files: HashMap<_, _> = self
            .edited
            .files(scene)
            .chain(
                self.video_frames
                    .keys()
                    .map(|id| (id.clone(), crate::video::video_layer_texture_name(id))),
            )
            .collect();
        let frame = build::build_frame_with_content(
            scene,
            self.compositor.clock_ms(),
            &mut textures,
            None,
            Some(&mut text_for),
            Some(&files),
        );
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
            let edited = files
                .get(&key.layer_id)
                .is_some_and(|file| super::edit::LayerEdits::is_edit_key(file));
            let mask = (key.kind != LayerCommandKind::Text && !edited)
                .then(|| {
                    scene
                        .get(&key.layer_id)
                        .filter(|layer| {
                            self.video_frames.contains_key(&key.layer_id)
                                || layer.file.as_deref().is_some_and(|file| !file.is_empty())
                        })
                        .and_then(|layer| layer.mask.as_deref())
                        .and_then(|mask| textures.assets.get(mask)?.clone())
                        .filter(|mask| {
                            mask.width == command.size.width && mask.height == command.size.height
                        })
                        .map(|mask| textures.record(mask).0.0 as usize)
                })
                .flatten();
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
            transition: None,
            textures: textures.table.into_boxed_slice(),
        })
    }
}

struct CpuTextureView<'a> {
    assets: &'a HashMap<String, Option<Arc<FrameTexture>>>,
    video: &'a HashMap<String, Arc<FrameTexture>>,
    edited: &'a super::edit::LayerEdits,
    atlases: &'a mut HashMap<String, Arc<FrameTexture>>,
    atlas_revision: &'a mut u64,
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
        if let Some(texture) = self.edited.resolve(name) {
            return Some(self.record(texture));
        }
        if let Some(texture) = self.atlases.get(name) {
            return Some(self.record(texture.clone()));
        }
        if let Some(id) = name.strip_prefix(crate::video::VIDEO_LAYER_TEXTURE_PREFIX)
            && let Some(texture) = self.video.get(id)
        {
            return Some(self.record(texture.clone()));
        }
        Some(self.record(self.assets.get(name)?.clone()?))
    }

    fn pixel_alpha(&self, id: TextureId, x: u32, y: u32) -> Option<u8> {
        let texture = self.table.get(id.0 as usize)?;
        if x >= texture.width || y >= texture.height {
            return None;
        }
        let pixel = (y as usize * texture.width as usize + x as usize) * 4;
        texture.rgba().get(pixel + 3).copied()
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let revision = self
            .atlas_revision
            .checked_add(1)
            .filter(|id| *id < 1 << 61)?;
        let texture = Arc::new(FrameTexture {
            revision: (1 << 61) | revision,
            width,
            height,
            rgba: rgba.into(),
        });
        *self.atlas_revision = revision;
        self.atlases.insert(name.to_owned(), texture.clone());
        Some(self.record(texture))
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
