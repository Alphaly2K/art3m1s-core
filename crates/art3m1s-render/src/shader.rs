//! Backend-independent shader identities used by render-pass declarations.
//!
//! Concrete shader sources and compiler profiles belong to each GPU backend.
//! This module only names the semantic programs referenced by a
//! [`crate::draw::DrawList`].

use crate::draw::ShaderEffect;

pub const SPRITE_SHADER: &str = "sprite";
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) const PRESENT_ANTIALIAS_SHADER: &str = "__art3m1s_present_antialias";
/// Sprite draw that must sample with a nearest filter.
///
/// RFVP uses nearest sampling for its text atlases and gaiji glyph textures;
/// keeping this as a named sprite variant lets adapters preserve that state
/// without changing the `DrawCommand` layout or the C ABI.
pub const SPRITE_NEAREST_SHADER: &str = "sprite-nearest";
/// Sprite sampling constrained to its source rectangle, with filtering across
/// the projected pixel footprint when the sprite is reduced.
pub const SPRITE_RESAMPLE_SHADER: &str = "sprite-resample";

#[cfg(any(feature = "gl", feature = "metal", feature = "vulkan"))]
pub(crate) fn sprite_sample_rect(command: &crate::draw::DrawCommand) -> [f32; 4] {
    if !command
        .shader
        .as_ref()
        .is_some_and(|shader| shader.name == SPRITE_RESAMPLE_SHADER)
    {
        return [0.0; 4];
    }
    let [left, top] = command.clip.uv_offset;
    let [width, height] = command.clip.uv_scale;
    let right = left + width;
    let bottom = top + height;
    [
        left.min(right),
        top.min(bottom),
        left.max(right),
        top.max(bottom),
    ]
    .map(|edge| edge.clamp(0., 1.))
}

pub const ALPHA_MASK_SHADER: &str = "alpha-mask";
pub const GROUP_COMPOSITE_SHADER: &str = "group-composite";
/// `[trans type=2]` rule-image transition shader identity.
pub const RULE_TRANS_SHADER: &str = "rule-trans";

/// Returns whether a sprite draw must ignore its texture's linear sampler.
///
/// This is intentionally a shader identity instead of a `DrawCommand` field so
/// engine adapters can preserve nearest sampling without changing the stable
/// draw-list or C ABI layouts.
pub fn uses_nearest_sampler(effect: Option<&ShaderEffect>) -> bool {
    effect.is_some_and(|effect| effect.name == SPRITE_NEAREST_SHADER)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn nearest_sprite_identity_selects_nearest_sampling() {
        let effect = ShaderEffect {
            name: SPRITE_NEAREST_SHADER.to_owned(),
            uniforms: BTreeMap::new(),
            mask_texture: None,
            user_texture: None,
        };

        assert!(uses_nearest_sampler(Some(&effect)));
        assert!(!uses_nearest_sampler(None));
    }
}
