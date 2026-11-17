//! Draft CPU frame view for the Artemis Host renderer.

use super::protocol::ProtocolError;
use std::ops::Deref;
use std::sync::Arc;

mod packet;
pub use packet::FRAME_PACKET_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteBlend {
    Alpha,
    Add,
    Screen,
    Multiply,
}

#[derive(Debug, PartialEq)]
pub struct FrameSprite {
    pub layer_id: String,
    pub texture: usize,
    /// If present, set each source texel's alpha to `alpha * mask_red / 255` before sampling.
    pub mask: Option<usize>,
    /// Two affine basis vectors (columns), followed by the translation.
    pub transform: [f32; 6],
    pub quad_size: [f32; 2],
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    /// Stage coordinates (left, top, width, height).
    pub clip_bounds: Option<[f32; 4]>,
    pub opacity: f32,
    pub color_multiply: [f32; 3],
    pub grayscale: bool,
    pub negative: bool,
    pub blend: SpriteBlend,
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
            && self
                .textures
                .iter()
                .map(|texture| texture.revision)
                .eq(other.textures.iter().map(|texture| texture.revision))
    }
}

#[derive(Debug)]
pub struct FrameLease {
    frame: Arc<PublishedFrame>,
    _lease: Arc<()>,
}

impl FrameLease {
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
    snapshot: FrameSnapshot,
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
            snapshot: frame,
            packet,
        }));
        Ok(())
    }

    pub fn acquire(&self, after: u64) -> Result<Option<FrameLease>, ProtocolError> {
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
            frame: frame.clone(),
            _lease: self.leases.clone(),
        }))
    }
}
