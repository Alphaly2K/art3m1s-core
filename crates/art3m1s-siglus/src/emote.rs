use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use art3m1s_render::TextureId;
use siglus_scene_vm::assets::RgbaImage;
use siglus_scene_vm::emote::EmoteRenderPacket;
use siglus_scene_vm::render::EmoteTextureRenderer;

use crate::{ColorEffects, SiglusFrameSnapshot, SiglusFrameTexture, apply_color_effects};

/// A deferred stage texture. A Host can render this with the Siglus object compositor.
pub struct SiglusEmoteTexture {
    pub id: TextureId,
    pub packet: Arc<EmoteRenderPacket>,
    color: ColorEffects,
}

impl SiglusEmoteTexture {
    pub(crate) fn new(id: TextureId, packet: Arc<EmoteRenderPacket>, color: ColorEffects) -> Self {
        Self { id, packet, color }
    }

    /// Apply the stage object's color overrides to its composited texture.
    pub fn stage_image(&self, image: Arc<RgbaImage>) -> Arc<RgbaImage> {
        if self.color.is_identity() {
            return image;
        }
        Arc::new(RgbaImage {
            width: image.width,
            height: image.height,
            center_x: 0,
            center_y: 0,
            rgba: apply_color_effects(&image.rgba, self.color),
        })
    }
}

struct ObjectSurface {
    version: u64,
    alpha_readback: bool,
    image: Arc<RgbaImage>,
}

/// The Host prepass for Siglus object textures. Keep one per presenting session.
#[derive(Default)]
pub struct SiglusEmoteRenderer {
    gpu: Option<EmoteTextureRenderer>,
    surfaces: HashMap<u64, ObjectSurface>,
}

impl SiglusEmoteRenderer {
    /// Materialize the stage textures for upload or a frame lease. Present successive
    /// frames in order so feedback can sample the last rendered object version.
    pub fn resolve_textures(
        &mut self,
        frame: &SiglusFrameSnapshot,
    ) -> Result<Vec<SiglusFrameTexture>> {
        let pending = frame.emote_textures();
        let mut textures = frame.textures().to_vec();
        let live: HashSet<_> = pending.iter().map(|entry| entry.packet.render_id).collect();
        self.surfaces.retain(|id, _| live.contains(id));
        if let Some(gpu) = &mut self.gpu {
            gpu.retain_objects(&live);
        }
        if pending.is_empty() {
            return Ok(textures);
        }
        if self.gpu.is_none() {
            self.gpu = Some(EmoteTextureRenderer::new()?);
        }
        let gpu = self.gpu.as_mut().unwrap();
        for entry in pending {
            let packet = &entry.packet;
            if self.surfaces.get(&packet.render_id).is_none_or(|surface| {
                surface.version != packet.version
                    || (packet.alpha_readback && !surface.alpha_readback)
            }) {
                let rgba = gpu.draw(packet)?;
                self.surfaces.insert(
                    packet.render_id,
                    ObjectSurface {
                        version: packet.version,
                        alpha_readback: packet.alpha_readback,
                        image: Arc::new(RgbaImage {
                            width: packet.width,
                            height: packet.height,
                            center_x: 0,
                            center_y: 0,
                            rgba,
                        }),
                    },
                );
            }
            textures.push(SiglusFrameTexture {
                id: entry.id,
                image: entry.stage_image(self.surfaces[&packet.render_id].image.clone()),
            });
        }
        Ok(textures)
    }
}
