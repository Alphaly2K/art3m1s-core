//! E-Mote model parsing and playback state.
//!
//! The parser and evaluator stay independent from the host runtime, while
//! `art3m1s-core` owns script bindings, GPU resources and scene composition.

mod atlas;
mod error;
mod model;
mod motion;
mod player;
mod psb;
mod render;
mod timeline;

pub use atlas::{AtlasIcon, EmoteAtlas, EmoteTexture, TextureFormat};
pub use error::{EmoteError, Result};
pub use model::{EmoteModel, EmoteModelInfo};
pub use motion::{
    EmoteFrameContent, EmoteLayer, EmoteLayerFrame, EmoteMesh, EmoteMotion, EmoteMotionLibrary,
    EmoteMotionParameter, EmoteMotionRef,
};
pub use player::{EmoteCommand, EmotePlayer, EmoteTransform, TimelineState, VariableState};
pub use psb::{PsbDocument, PsbHeader, PsbValue, ResourceRef};
pub use render::{EmoteDrawItem, EmoteMotionEvaluator, EmoteRenderState};
pub use timeline::{
    EmoteEyeControl, EmoteKeyframe, EmoteSelectorControl, EmoteSelectorOption, EmoteTimeline,
    EmoteTimelineTrack, EmoteVariable,
};
