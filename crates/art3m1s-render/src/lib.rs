//! Backend-neutral rendering API shared by Art3m1s core and engine adapters.
//!
//! This initial crate contains the DrawList, texture, post-process and shader
//! contracts only. Concrete GL/Metal/Vulkan backends are migrated separately.

pub mod backend;
pub mod draw;
pub mod external;
pub mod hlsl;
pub mod naming;
pub mod post_process;
pub mod shader;
pub mod types;

pub use backend::{AssetSource, FrameCapture, GpuBackend, GpuProfileStats, RenderRegion};
pub use draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawCommandKey, DrawList, DrawMesh,
    LayerCommandKind, LayerDrawSource, LayerShaderGroupKind, NativeEmoteMaterial, ShaderEffect,
    ShaderGroup, ShaderGroupKey, StencilMetadata, TextureId, TextureInfo, TextureOrigin,
    TextureProvider,
};
pub use external::{
    ExternalImage, ExternalImageKind, ExternalTextureHandle, FrameTargetHandle, GpuSyncKind,
    GpuSyncToken, ResourceOwnership, VideoImportCapability, VideoSurfaceHandle,
};
pub use post_process::{
    PostProcessContext, PostProcessPass, PostProcessPipeline, RenderDimensions,
    RenderQualityPreset, SceneTarget, UpscaleConfig, UpscaleMode,
};
pub use shader::{ALPHA_MASK_SHADER, GROUP_COMPOSITE_SHADER, RULE_TRANS_SHADER, SPRITE_SHADER};
pub use types::{
    BackendCapabilities, BackendInfo, BackendKind, BackendStability, Extent2D, FrameTarget,
    NativeSurface, NativeSurfaceKind, PipelineId, RenderTarget, RenderTargetDesc, RenderTargetId,
    TextureData, TextureDesc, TextureFormat, TextureUpdate, TextureUsage,
};
