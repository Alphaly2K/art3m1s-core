//! Backend-independent shader identities used by render-pass declarations.
//!
//! Concrete shader sources and compiler profiles belong to each GPU backend.
//! This module only names the semantic programs referenced by a
//! [`crate::render_pipeline::draw::DrawList`].

pub const SPRITE_SHADER: &str = "sprite";
pub const ALPHA_MASK_SHADER: &str = "alpha-mask";
pub const GROUP_COMPOSITE_SHADER: &str = "group-composite";
/// `[trans type=2]` rule-image transition shader identity.
pub const RULE_TRANS_SHADER: &str = "rule-trans";
