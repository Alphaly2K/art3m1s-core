//! Backend-neutral linear post-processing description.
//!
//! This module deliberately contains no native GPU handles. Backends consume
//! the pass list and map it to their own cached render-pass/pipeline objects.

use crate::backend::{Extent2D, TextureFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderDimensions {
    /// The target where the scene DrawList is rasterized.
    pub render_size: Extent2D,
    /// The target consumed by UI/presentation or the host surface.
    pub output_size: Extent2D,
}

impl RenderDimensions {
    pub const fn new(render_size: Extent2D, output_size: Extent2D) -> Self {
        Self {
            render_size,
            output_size,
        }
    }

    pub const fn same(size: Extent2D) -> Self {
        Self::new(size, size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneTarget {
    pub size: Extent2D,
    pub format: TextureFormat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpscaleMode {
    /// Use the existing linear sampler. This is the production baseline.
    Linear,
    /// Reserved for a future backend-native spatial upscaler.
    Spatial,
}

/// 统一的运行时渲染质量策略。比例只在这里定义，Host/业务层不应自行
/// 计算 render_size。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderQualityPreset {
    Native,
    Quality,
    Balanced,
    Performance,
}

impl RenderQualityPreset {
    pub const fn policy(self) -> (f32, UpscaleMode) {
        match self {
            Self::Native => (1.0, UpscaleMode::Linear),
            Self::Quality => (2.0 / 3.0, UpscaleMode::Spatial),
            Self::Balanced => (0.58, UpscaleMode::Spatial),
            Self::Performance => (0.5, UpscaleMode::Spatial),
        }
    }

    pub const fn from_ffi(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Native),
            1 => Some(Self::Quality),
            2 => Some(Self::Balanced),
            3 => Some(Self::Performance),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpscaleConfig {
    pub mode: UpscaleMode,
    /// A future spatial upscaler may use this value. Linear mode ignores it.
    pub sharpness: f32,
}

impl Default for UpscaleConfig {
    fn default() -> Self {
        Self {
            mode: UpscaleMode::Linear,
            sharpness: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PostProcessPass {
    /// Resample SceneColor into the output target.
    Upscale(UpscaleConfig),
    /// Reserved for a future non-temporal sharpen implementation.
    Sharpen { amount: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PostProcessPipeline {
    pub scene_format: TextureFormat,
    pub output_format: TextureFormat,
    /// Fraction of the logical scene size used for SceneColor. `1.0` keeps
    /// the current native scene target; lower values request a cheaper scene
    /// render followed by the configured upscale pass.
    pub render_scale: f32,
    pub passes: Vec<PostProcessPass>,
}

impl Default for PostProcessPipeline {
    fn default() -> Self {
        Self {
            scene_format: TextureFormat::Rgba8Unorm,
            output_format: TextureFormat::Rgba8Unorm,
            render_scale: 1.0,
            passes: vec![PostProcessPass::Upscale(UpscaleConfig::default())],
        }
    }
}

impl PostProcessPipeline {
    pub fn validate(&self, dimensions: RenderDimensions) -> Result<(), String> {
        if dimensions.render_size.is_empty() || dimensions.output_size.is_empty() {
            return Err("post-process dimensions must be non-zero".into());
        }
        if !self.render_scale.is_finite() || !(0.1..=1.0).contains(&self.render_scale) {
            return Err("render scale must be finite and in [0.1, 1.0]".into());
        }
        for pass in &self.passes {
            match pass {
                PostProcessPass::Upscale(config) => {
                    if !config.sharpness.is_finite() || !(0.0..=1.0).contains(&config.sharpness) {
                        return Err("upscale sharpness must be finite and in [0, 1]".into());
                    }
                }
                PostProcessPass::Sharpen { amount } => {
                    if !amount.is_finite() || !(0.0..=1.0).contains(amount) {
                        return Err("sharpen amount must be finite and in [0, 1]".into());
                    }
                    return Err("sharpen pass is not implemented yet".into());
                }
            }
        }
        Ok(())
    }

    pub fn context(&self, dimensions: RenderDimensions) -> PostProcessContext {
        PostProcessContext {
            dimensions,
            scene: SceneTarget {
                size: dimensions.render_size,
                format: self.scene_format,
            },
            output_format: self.output_format,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PostProcessContext {
    pub dimensions: RenderDimensions,
    pub scene: SceneTarget,
    pub output_format: TextureFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_keeps_render_and_output_sizes_distinct() {
        let dimensions =
            RenderDimensions::new(Extent2D::new(1920, 1080), Extent2D::new(2560, 1440));
        let pipeline = PostProcessPipeline::default();
        let context = pipeline.context(dimensions);
        assert_eq!(context.scene.size, Extent2D::new(1920, 1080));
        assert_eq!(context.output_format, TextureFormat::Rgba8Unorm);
        assert!(pipeline.validate(dimensions).is_ok());
    }

    #[test]
    fn spatial_pass_is_backend_capability_checked() {
        let dimensions = RenderDimensions::same(Extent2D::new(16, 16));
        let mut pipeline = PostProcessPipeline::default();
        pipeline.passes[0] = PostProcessPass::Upscale(UpscaleConfig {
            mode: UpscaleMode::Spatial,
            sharpness: 0.0,
        });
        assert!(pipeline.validate(dimensions).is_ok());
    }
}
