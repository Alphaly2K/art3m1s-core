use art3m1s_emote::{EmoteCommand, EmoteDrawItem, EmoteModel, EmotePlayback, Result};
use art3m1s_render::{
    DrawCommand, TextureId, TextureInfo,
    emote::{EmoteDrawPlacement, draw_command},
};
use asb_interpreter::EmoteLayerCommand;
use glam::{Affine2, Vec2};

pub(crate) struct ArtemisEmoteLayer {
    player: EmotePlayback,
    generation: u64,
    size: Vec2,
}

impl ArtemisEmoteLayer {
    pub(crate) fn new(model: EmoteModel, generation: u64, width: u32, height: u32) -> Self {
        Self {
            player: EmotePlayback::new(model, generation as u32),
            generation,
            size: Vec2::new(width as f32, height as f32),
        }
    }

    #[cfg(feature = "legacy")]
    pub(crate) fn model(&self) -> &EmoteModel {
        self.player.model()
    }

    pub(crate) fn command(&mut self, command: EmoteLayerCommand) {
        self.player.command(match command {
            EmoteLayerCommand::SetScale {
                scale,
                origin_x,
                origin_y,
            } => EmoteCommand::SetScale([scale, origin_x, origin_y]),
            EmoteLayerCommand::SetCoord { x, y, z, angle } => {
                EmoteCommand::SetCoord([x, y, z, angle])
            }
            EmoteLayerCommand::SetVariable {
                label,
                value,
                frames,
                easing,
            } => EmoteCommand::SetVariable {
                label,
                value,
                frames,
                easing,
            },
            EmoteLayerCommand::PlayTimeline { label, flags } => {
                EmoteCommand::PlayTimeline { label, flags }
            }
            EmoteLayerCommand::FadeInTimeline {
                label,
                frames,
                easing,
            } => EmoteCommand::FadeInTimeline {
                label,
                frames,
                easing,
            },
            EmoteLayerCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            } => EmoteCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            },
            EmoteLayerCommand::StopTimeline { label } => EmoteCommand::StopTimeline { label },
            EmoteLayerCommand::Pass => EmoteCommand::Pass,
            EmoteLayerCommand::Step => EmoteCommand::Step,
            EmoteLayerCommand::Skip => EmoteCommand::Skip,
        });
    }

    pub(crate) fn advance(&mut self, frames: f32) -> bool {
        self.player.advance(frames)
    }

    pub(crate) fn evaluate(&mut self) -> Result<Vec<EmoteDrawItem>> {
        self.player.evaluate()
    }

    pub(crate) fn draw_command(
        &self,
        item: EmoteDrawItem,
        texture_id: TextureId,
        texture_info: TextureInfo,
    ) -> Option<DrawCommand> {
        let transform = self.player.transform();
        let [scale, origin_x, origin_y] = transform.scale;
        let [x, y, _, angle] = transform.coord;
        let placement = EmoteDrawPlacement {
            transform: Affine2::from_translation(self.size * 0.5 + Vec2::new(x, y))
                * Affine2::from_angle(angle.to_radians())
                * Affine2::from_scale(Vec2::splat(scale))
                * Affine2::from_translation(Vec2::new(-origin_x, -origin_y)),
            clip_bounds: Some([0.0, 0.0, self.size.x, self.size.y]),
            stencil_namespace: self.generation,
        };
        draw_command(item, texture_id, texture_info, placement)
    }
}
