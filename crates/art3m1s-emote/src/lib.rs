//! E-Mote model parsing and playback state.
//!
//! Engines supply model bytes and elapsed time. The runtime evaluates poses
//! into ordered layers for the caller to compose.

mod atlas;
mod error;
mod model;
mod motion;
mod playback;
mod player;
mod psb;
mod render;
mod timeline;

pub use atlas::{AtlasIcon, EmoteAtlas, EmoteTexture, TextureFormat};
pub use error::{EmoteError, Result};
pub use model::{EmoteModel, EmoteModelInfo};
pub use motion::{
    EmoteBezierPath, EmoteEasingCurve, EmoteFrameContent, EmoteFrameCurves, EmoteLayer,
    EmoteLayerFrame, EmoteMesh, EmoteMotion, EmoteMotionLibrary, EmoteMotionParameter,
    EmoteMotionPriority, EmoteMotionRef,
};
pub use playback::EmotePlayback;
pub use player::{EmoteCommand, EmotePlayer, EmoteTransform, TimelineState, VariableState};
pub use psb::{PsbDocument, PsbHeader, PsbResourceData, PsbValue, ResourceRef};
pub use render::{EmoteDrawItem, EmoteEvaluationHistory, EmoteMotionEvaluator, EmoteRenderState};
pub use timeline::{
    EmoteEyeControl, EmoteKeyframe, EmoteSelectorControl, EmoteSelectorOption, EmoteTimeline,
    EmoteTimelineTrack, EmoteVariable,
};
