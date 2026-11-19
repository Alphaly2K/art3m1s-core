//! Draft CPU frame view for the Artemis Host renderer.

use super::protocol::ProtocolError;
use super::runtime::RuntimeHandle;
use std::ops::Deref;
use std::sync::Arc;

mod packet;
pub use art3m1s_render::artemis::{
    FRAME_PACKET_VERSION, FrameGroup, FramePacket, FramePacketError, FrameShader,
    FrameShaderEffect, FrameShaderLanguage, FrameSprite, FrameTextureDescriptor, FrameTransition,
    SpriteBlend,
};

fn remap_effect(effect: &mut FrameShaderEffect, textures: &[usize]) {
    effect.mask = effect.mask.map(|index| textures[index]);
    effect.user = effect.user.map(|index| textures[index]);
}

#[derive(Debug)]
pub struct FrameTexture {
    /// Unique within the runtime and session epoch. Pixels never change for this revision.
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub(crate) rgba: Box<[u8]>,
}

impl FrameTexture {
    /// Tightly packed RGBA8, straight alpha, with the first row at the top.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

#[derive(Debug)]
pub struct FrameSnapshot {
    pub session_epoch: u64,
    pub render_frame_id: u64,
    pub host_frame_id: u64,
    pub stage_size: [u32; 2],
    pub sprites: Box<[FrameSprite]>,
    pub groups: Box<[FrameGroup]>,
    pub masks: Box<[FrameSprite]>,
    pub transition: Option<FrameTransition>,
    pub(crate) textures: Box<[Arc<FrameTexture>]>,
}

impl FrameSnapshot {
    pub fn texture(&self, index: usize) -> Option<&FrameTexture> {
        self.textures.get(index).map(Arc::as_ref)
    }

    /// Texture indices in this frame refer to this table.
    pub fn textures(&self) -> impl ExactSizeIterator<Item = &FrameTexture> {
        self.textures.iter().map(Arc::as_ref)
    }

    pub(crate) fn same_content(&self, other: &Self) -> bool {
        self.session_epoch == other.session_epoch
            && self.stage_size == other.stage_size
            && self.sprites == other.sprites
            && self.groups == other.groups
            && self.masks == other.masks
            && self.transition == other.transition
            && self
                .textures
                .iter()
                .map(|texture| texture.revision)
                .eq(other.textures.iter().map(|texture| texture.revision))
    }

    pub(crate) fn blend_from(
        &mut self,
        source: &Self,
        progress: f32,
        rule: Option<&Arc<FrameTexture>>,
        vague: f32,
    ) {
        use std::collections::HashMap;
        let mut textures = std::mem::take(&mut self.textures).into_vec();
        let mut by_revision: HashMap<_, _> = textures
            .iter()
            .enumerate()
            .map(|(index, image)| (image.revision, index))
            .collect();
        let remap: Vec<_> = source
            .textures
            .iter()
            .map(|image| {
                *by_revision.entry(image.revision).or_insert_with(|| {
                    let index = textures.len();
                    textures.push(image.clone());
                    index
                })
            })
            .collect();
        let remap_sprite = |mut sprite: FrameSprite| {
            sprite.texture = remap[sprite.texture];
            sprite.mask = sprite.mask.map(|index| remap[index]);
            if let Some(effect) = &mut sprite.shader {
                remap_effect(effect, &remap);
            }
            sprite
        };
        let sprites = source.sprites.iter().cloned().map(remap_sprite).collect();
        let masks = source.masks.iter().cloned().map(remap_sprite).collect();
        let groups = source
            .groups
            .iter()
            .cloned()
            .map(|mut group| {
                group.mask = group.mask.map(|index| remap[index]);
                if let Some(effect) = &mut group.shader {
                    remap_effect(effect, &remap);
                }
                group
            })
            .collect();
        let rule = rule.map(|image| {
            *by_revision.entry(image.revision).or_insert_with(|| {
                let index = textures.len();
                textures.push(image.clone());
                index
            })
        });
        self.textures = textures.into_boxed_slice();
        self.transition = Some(FrameTransition {
            source: sprites,
            source_groups: groups,
            source_masks: masks,
            progress,
            rule,
            vague,
        });
    }
}

#[derive(Debug)]
pub struct FrameLease {
    runtime: RuntimeHandle,
    frame: Arc<PublishedFrame>,
    _lease: Arc<()>,
}

impl FrameLease {
    pub fn runtime(&self) -> RuntimeHandle {
        self.runtime
    }

    /// Versioned drawing commands and texture descriptors; pixel bytes are borrowed separately.
    pub fn packet(&self) -> &[u8] {
        &self.frame.packet
    }
}

impl Deref for FrameLease {
    type Target = FrameSnapshot;

    fn deref(&self) -> &FrameSnapshot {
        &self.frame.snapshot
    }
}

#[derive(Debug)]
struct PublishedFrame {
    snapshot: Arc<FrameSnapshot>,
    packet: Box<[u8]>,
}

#[derive(Default)]
pub(crate) struct FramePublisher {
    serial: u64,
    latest: Option<Arc<PublishedFrame>>,
    leases: Arc<()>,
}

impl FramePublisher {
    pub fn clear(&mut self) {
        self.latest = None;
    }

    pub fn has_leases(&self) -> bool {
        Arc::strong_count(&self.leases) > 1
    }

    pub fn latest_id(&self) -> Option<u64> {
        self.latest
            .as_ref()
            .map(|frame| frame.snapshot.render_frame_id)
    }

    pub fn latest_snapshot(&self) -> Option<Arc<FrameSnapshot>> {
        self.latest.as_ref().map(|frame| frame.snapshot.clone())
    }

    pub fn latest_is_transition(&self) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|frame| frame.snapshot.transition.is_some())
    }

    pub fn publish(&mut self, mut frame: FrameSnapshot) -> Result<(), ProtocolError> {
        if self
            .latest
            .as_ref()
            .is_some_and(|last| last.snapshot.same_content(&frame))
        {
            return Ok(());
        }
        frame.render_frame_id = self.serial.checked_add(1).ok_or(ProtocolError::Limit)?;
        let packet = match packet::encode(&frame) {
            Ok(packet) => packet,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };
        self.serial = frame.render_frame_id;
        self.latest = Some(Arc::new(PublishedFrame {
            snapshot: Arc::new(frame),
            packet,
        }));
        Ok(())
    }

    pub fn acquire(
        &self,
        runtime: RuntimeHandle,
        after: u64,
    ) -> Result<Option<FrameLease>, ProtocolError> {
        let Some(frame) = self
            .latest
            .as_ref()
            .filter(|frame| frame.snapshot.render_frame_id > after)
        else {
            return Ok(None);
        };
        if Arc::strong_count(&self.leases) > 2 {
            return Err(ProtocolError::WouldBlock);
        }
        Ok(Some(FrameLease {
            runtime,
            frame: frame.clone(),
            _lease: self.leases.clone(),
        }))
    }
}
