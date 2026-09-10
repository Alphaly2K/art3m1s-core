//! Native Metal backend for macOS and iOS.
//!
//! Metal objects, command buffers, pipeline states and synchronization stay
//! private to this module. The runtime continues to exchange only DrawList and
//! backend-neutral resource identifiers.

use crate::backend::{
    AssetSource, BackendCapabilities, BackendInfo, BackendKind, BackendStability, Extent2D,
    ExternalImage, ExternalImageKind, ExternalTextureHandle, FrameCapture, FrameTarget, GpuBackend,
    GpuProfileStats, GpuSyncKind, GpuSyncToken, NativeSurface, NativeSurfaceKind, RenderRegion,
    RenderTarget, RenderTargetDesc, RenderTargetId, ResourceOwnership, ShaderCompileError,
    ShaderId, TextureData, TextureDesc, TextureFormat, TextureOrigin, TextureUpdate, TextureUsage,
    VideoImportCapability, VideoSurfaceHandle,
};
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, ShaderEffect, ShaderGroup, TextureId,
    TextureInfo, TextureProvider,
};
use crate::render_pipeline::hlsl::{
    CompiledShader, ShaderCompiler, ShaderRegistry, ShaderResourceKind, ShaderRuntimeParameters,
    ShaderTexture,
};
use crate::render_pipeline::post_process::{
    PostProcessPass, PostProcessPipeline, RenderDimensions, UpscaleMode,
};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_core_foundation::CGSize;
use objc2_foundation::NSString;
use objc2_io_surface::IOSurfaceRef;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer,
    MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue, MTLCreateSystemDefaultDevice,
    MTLDevice, MTLFunction, MTLIndexType, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResourceOptions, MTLSamplerAddressMode,
    MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerState, MTLScissorRect, MTLSize,
    MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage, MTLViewport,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
mod core_video;
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ptr::NonNull;
use std::time::Instant;

type Device = Retained<ProtocolObject<dyn MTLDevice>>;
type CommandQueue = Retained<ProtocolObject<dyn MTLCommandQueue>>;
type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;
type RenderCommandEncoder = Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>;
type Texture = Retained<ProtocolObject<dyn MTLTexture>>;
type Buffer = Retained<ProtocolObject<dyn objc2_metal::MTLBuffer>>;
type PipelineState = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;
type Function = Retained<ProtocolObject<dyn MTLFunction>>;

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VertexUniforms {
    transform: [f32; 16],
    size: [f32; 2],
    uv_offset: [f32; 2],
    uv_scale: [f32; 2],
    padding: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpriteUniforms {
    opacity_flags: [f32; 4],
    multiply: [f32; 4],
    emote_uv_rect: [f32; 4],
    emote_color_tl: [f32; 4],
    emote_color_tr: [f32; 4],
    emote_color_bl: [f32; 4],
    emote_color_br: [f32; 4],
    emote_blend_mode: [f32; 4],
    emote_clip_rect: [f32; 4],
    emote_wipe: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EffectUniforms {
    alpha_progress_vague_opaque: [f32; 4],
    color_multiply_grayscale: [f32; 4],
    negative_padding: [f32; 4],
}

struct ImportedNative {
    handle: ExternalTextureHandle,
    #[allow(dead_code)]
    ownership: ResourceOwnership,
    #[allow(dead_code)]
    kind: ExternalImageKind,
    /// Native retains; dropped after GPU retirement.
    retains: Vec<core_video::CfPtr>,
}

impl Drop for ImportedNative {
    fn drop(&mut self) {
        self.retains.clear();
    }
}

struct MetalTexture {
    raw: Texture,
    desc: TextureDesc,
    info: TextureInfo,
    cpu_pixels: PixelStorage,
    opaque: bool,
    external: Option<ImportedNative>,
}

struct MetalRenderTarget {
    id: RenderTargetId,
    color: TextureId,
    desc: RenderTargetDesc,
}

struct PrivateRenderTarget {
    texture: Texture,
    extent: Extent2D,
    format: MTLPixelFormat,
}

/// 薄 MetalFX 绑定。MetalFX 没有稳定的 Rust crate，使用 Objective-C runtime
/// 动态查找类，从而在 macOS 12/旧设备上安全地回退而不触发链接或启动崩溃。
struct MetalFxSpatialScaler {
    raw: Retained<AnyObject>,
    input: Extent2D,
    output: Extent2D,
}

impl MetalFxSpatialScaler {
    fn create(
        device: &ProtocolObject<dyn MTLDevice>,
        input: Extent2D,
        output: Extent2D,
    ) -> Option<Self> {
        if input.is_empty()
            || output.is_empty()
            || output.width < input.width
            || output.height < input.height
            || input == output
        {
            return None;
        }
        let class = AnyClass::get(c"MTLFXSpatialScalerDescriptor")?;
        let supported: bool = unsafe { msg_send![class, supportsDevice: device] };
        if !supported {
            return None;
        }
        let descriptor: Retained<AnyObject> = unsafe { msg_send![class, new] };
        unsafe {
            let _: () = msg_send![&*descriptor, setColorTextureFormat: MTLPixelFormat::RGBA8Unorm];
            let _: () = msg_send![&*descriptor, setOutputTextureFormat: MTLPixelFormat::RGBA8Unorm];
            let _: () = msg_send![&*descriptor, setInputWidth: input.width as usize];
            let _: () = msg_send![&*descriptor, setInputHeight: input.height as usize];
            let _: () = msg_send![&*descriptor, setOutputWidth: output.width as usize];
            let _: () = msg_send![&*descriptor, setOutputHeight: output.height as usize];
            // Linear is correct for the current RGBA8 SceneColor contract.
            let _: () = msg_send![&*descriptor, setColorProcessingMode: 1isize];
        }
        let raw: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*descriptor, newSpatialScalerWithDevice: device] };
        raw.map(|raw| Self { raw, input, output })
    }

    fn encode(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        source: &ProtocolObject<dyn MTLTexture>,
        destination: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe {
            let _: () = msg_send![&*self.raw, setColorTexture: source];
            let _: () = msg_send![&*self.raw, setOutputTexture: destination];
            let _: () = msg_send![&*self.raw, setInputContentWidth: self.input.width as usize];
            let _: () = msg_send![&*self.raw, setInputContentHeight: self.input.height as usize];
            let _: () = msg_send![&*self.raw, encodeToCommandBuffer: command_buffer];
        }
    }
}

struct MetalPipeline {
    raw: PipelineState,
}

struct MetalShader {
    _library: Retained<ProtocolObject<dyn MTLLibrary>>,
    function: Function,
    compiled: CompiledShader,
}

struct MetalBuffer {
    raw: Buffer,
    length: usize,
}

impl MetalBuffer {
    fn from_bytes(device: &ProtocolObject<dyn MTLDevice>, bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() {
            return Err("Metal buffer cannot be empty".into());
        }
        let pointer = NonNull::new(bytes.as_ptr().cast_mut().cast())
            .ok_or_else(|| "Metal buffer source pointer is null".to_string())?;
        let raw = unsafe {
            device.newBufferWithBytes_length_options(
                pointer,
                bytes.len(),
                MTLResourceOptions::StorageModeShared,
            )
        }
        .ok_or_else(|| "failed to create Metal buffer".to_string())?;
        Ok(Self {
            raw,
            length: bytes.len(),
        })
    }
}

enum PixelStorage {
    None,
    Opaque,
    Alpha(Vec<u8>),
    Rgba(Vec<u8>),
}

impl PixelStorage {
    fn alpha_only(rgba: &[u8]) -> Self {
        let alpha = rgba
            .chunks_exact(4)
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>();
        if alpha.iter().all(|&value| value == 255) {
            Self::Opaque
        } else {
            Self::Alpha(alpha)
        }
    }

    fn allocated_bytes(&self) -> usize {
        match self {
            Self::None | Self::Opaque => 0,
            Self::Alpha(alpha) => alpha.len(),
            Self::Rgba(rgba) => rgba.len(),
        }
    }
}

struct ActiveFrame {
    command_buffer: CommandBuffer,
    target: PrivateRenderTarget,
}

struct SubmittedFrame {
    serial: u64,
    command_buffer: CommandBuffer,
}

enum RetiredResource {
    Texture(MetalTexture),
    Buffer(MetalBuffer),
    Pipeline(MetalPipeline),
}

struct PendingRetirement {
    after_serial: u64,
    resource: RetiredResource,
}

enum MetalSurface {
    /// Host-shared IOSurface or imported MTLTexture. Flutter may sample this
    /// object at any time, so presents must not clear it and must finish GPU
    /// work before `frameAvailable`.
    Shared { texture: Texture, extent: Extent2D },
    Layer {
        layer: Retained<CAMetalLayer>,
        extent: Extent2D,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ShaderKind {
    Sprite,
    AlphaMask,
    GroupComposite,
    RuleTransition,
    Custom(ShaderId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PipelineBlend {
    Replace,
    Draw(BlendMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StencilMode {
    Disabled,
    MaskComposite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    shader: ShaderKind,
    blend: PipelineBlend,
    stencil: StencilMode,
    color_format: MTLPixelFormat,
}

/// Native Apple GPU backend. It creates no EGL or OpenGL context.
pub struct MetalBackend {
    device: Device,
    queue: CommandQueue,
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    quad_vertex: MetalBuffer,
    quad_index: MetalBuffer,
    white_texture: Texture,
    transparent_texture: Texture,
    main_target: PrivateRenderTarget,
    scene_size: Extent2D,
    output_size: Extent2D,
    post_process: PostProcessPipeline,
    metalfx_supported: bool,
    spatial_scaler: Option<MetalFxSpatialScaler>,
    upscaled_scene: Option<PrivateRenderTarget>,
    active_frame: Option<ActiveFrame>,
    textures: HashMap<TextureId, MetalTexture>,
    names: HashMap<String, TextureId>,
    render_targets: HashMap<RenderTargetId, MetalRenderTarget>,
    group_targets: Vec<PrivateRenderTarget>,
    mask_targets: Vec<PrivateRenderTarget>,
    pipelines: HashMap<PipelineKey, MetalPipeline>,
    runtime_shaders: ShaderRegistry<MetalShader>,
    source: Option<Box<AssetSource>>,
    surface: Option<MetalSurface>,
    next_texture_id: u64,
    next_target_id: u64,
    content_revision: u64,
    texture_revisions: HashMap<TextureId, u64>,
    last_damage_overlay: Option<RenderRegion>,
    damage_flash_index: usize,
    submitted_serial: u64,
    completed_serial: u64,
    inflight: VecDeque<SubmittedFrame>,
    retired: VecDeque<PendingRetirement>,
    profiling_enabled: Cell<bool>,
    profile_texture_upload_ns: Cell<u64>,
    profile_uploaded_bytes: Cell<u64>,
    profile_draw_calls: Cell<u64>,
    profile_vertices: Cell<u64>,
    profile_texture_binds: Cell<u64>,
    profile_dynamic_mesh_bytes: Cell<u64>,
    profile_upscale_enabled: Cell<bool>,
    profile_upscale_cpu_encode_ns: Cell<u64>,
    shader_clock: Instant,
    shader_frame_index: u64,
    cv_texture_cache: Option<core_video::CvMetalTextureCache>,
    yuv_pipeline: Option<PipelineState>,
    video_leases: HashMap<u64, TextureId>,
    next_external_id: u64,
    profile_video_upload_ns: Cell<u64>,
    profile_video_uploaded_bytes: Cell<u64>,
    profile_video_uploaded_frames: Cell<u64>,
}

impl MetalBackend {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("Metal stage extent must be non-zero".into());
        }
        let device = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| "Metal system device is unavailable".to_string())?;
        let queue = device
            .newCommandQueue()
            .ok_or_else(|| "failed to create Metal command queue".to_string())?;
        let metalfx_supported = AnyClass::get(c"MTLFXSpatialScalerDescriptor")
            .map(|class| unsafe { msg_send![class, supportsDevice: &*device] })
            .unwrap_or(false);
        crate::core_info!(
            "[MetalFX] device_support={} logical_scene={}x{}",
            metalfx_supported,
            width,
            height
        );
        let source = NSString::from_str(include_str!("shaders.metal"));
        let library = device
            .newLibraryWithSource_options_error(&source, None)
            .map_err(|error| format!("failed to compile built-in MSL: {error}"))?;

        let sampler_desc = MTLSamplerDescriptor::new();
        sampler_desc.setMinFilter(MTLSamplerMinMagFilter::Linear);
        sampler_desc.setMagFilter(MTLSamplerMinMagFilter::Linear);
        sampler_desc.setSAddressMode(MTLSamplerAddressMode::ClampToEdge);
        sampler_desc.setTAddressMode(MTLSamplerAddressMode::ClampToEdge);
        let sampler = device
            .newSamplerStateWithDescriptor(&sampler_desc)
            .ok_or_else(|| "failed to create Metal sampler".to_string())?;

        let quad = [
            Vertex {
                position: [0.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [1.0, 0.0],
                uv: [1.0, 0.0],
            },
            Vertex {
                position: [1.0, 1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [0.0, 1.0],
                uv: [0.0, 1.0],
            },
        ];
        let indices = [0u16, 1, 2, 0, 2, 3];
        let quad_vertex = MetalBuffer::from_bytes(&device, as_bytes(&quad))?;
        let quad_index = MetalBuffer::from_bytes(&device, as_bytes(&indices))?;
        let main_target = create_private_target(
            &device,
            Extent2D::new(width, height),
            MTLPixelFormat::RGBA8Unorm,
        )?;
        let white_texture = create_solid_texture(&device, [255, 255, 255, 255])?;
        let transparent_texture = create_solid_texture(&device, [0, 0, 0, 0])?;

        Ok(Self {
            device,
            queue,
            library,
            sampler,
            quad_vertex,
            quad_index,
            white_texture,
            transparent_texture,
            main_target,
            scene_size: Extent2D::new(width, height),
            output_size: Extent2D::new(width, height),
            post_process: PostProcessPipeline::default(),
            metalfx_supported,
            spatial_scaler: None,
            upscaled_scene: None,
            active_frame: None,
            textures: HashMap::new(),
            names: HashMap::new(),
            render_targets: HashMap::new(),
            group_targets: Vec::new(),
            mask_targets: Vec::new(),
            pipelines: HashMap::new(),
            runtime_shaders: ShaderRegistry::default(),
            source: None,
            surface: None,
            next_texture_id: 1,
            next_target_id: 1,
            content_revision: 0,
            texture_revisions: HashMap::new(),
            last_damage_overlay: None,
            damage_flash_index: 0,
            submitted_serial: 0,
            completed_serial: 0,
            inflight: VecDeque::new(),
            retired: VecDeque::new(),
            profiling_enabled: Cell::new(false),
            profile_texture_upload_ns: Cell::new(0),
            profile_uploaded_bytes: Cell::new(0),
            profile_draw_calls: Cell::new(0),
            profile_vertices: Cell::new(0),
            profile_texture_binds: Cell::new(0),
            profile_dynamic_mesh_bytes: Cell::new(0),
            profile_upscale_enabled: Cell::new(false),
            profile_upscale_cpu_encode_ns: Cell::new(0),
            shader_clock: Instant::now(),
            shader_frame_index: 0,
            cv_texture_cache: None,
            yuv_pipeline: None,
            video_leases: HashMap::new(),
            next_external_id: 1,
            profile_video_upload_ns: Cell::new(0),
            profile_video_uploaded_bytes: Cell::new(0),
            profile_video_uploaded_frames: Cell::new(0),
        })
    }

    fn allocate_texture_id(&mut self) -> TextureId {
        let id = TextureId(self.next_texture_id);
        self.next_texture_id = self.next_texture_id.wrapping_add(1).max(1);
        id
    }

    fn allocate_target_id(&mut self) -> RenderTargetId {
        let id = RenderTargetId::from_opaque(self.next_target_id);
        self.next_target_id = self.next_target_id.wrapping_add(1).max(1);
        id
    }

    fn mark_texture_changed(&mut self, texture: TextureId) {
        self.content_revision = self.content_revision.wrapping_add(1);
        self.texture_revisions
            .insert(texture, self.content_revision);
    }

    fn retire_texture(&mut self, texture: MetalTexture) {
        self.retired.push_back(PendingRetirement {
            after_serial: self.submitted_serial,
            resource: RetiredResource::Texture(texture),
        });
    }

    fn retire_private_target(&mut self, target: PrivateRenderTarget) {
        self.retire_texture(MetalTexture {
            raw: target.texture,
            desc: TextureDesc {
                extent: target.extent,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsage::RENDER_TARGET | TextureUsage::SAMPLED,
            },
            info: TextureInfo {
                width: target.extent.width,
                height: target.extent.height,
            },
            cpu_pixels: PixelStorage::None,
            opaque: false,
            external: None,
        });
    }

    fn replace_scene_target(&mut self, extent: Extent2D) -> Result<(), String> {
        let replacement = create_private_target(&self.device, extent, MTLPixelFormat::RGBA8Unorm)?;
        let old = std::mem::replace(&mut self.main_target, replacement);
        self.retire_private_target(old);
        for target in std::mem::take(&mut self.group_targets) {
            self.retire_private_target(target);
        }
        for target in std::mem::take(&mut self.mask_targets) {
            self.retire_private_target(target);
        }
        self.spatial_scaler = None;
        if let Some(target) = self.upscaled_scene.take() {
            self.retire_private_target(target);
        }
        self.last_damage_overlay = None;
        Ok(())
    }

    fn ensure_spatial_resources(&mut self) -> Result<bool, String> {
        let input = self.main_target.extent;
        let output = self.output_size;
        if !self.metalfx_supported
            || output.width < input.width
            || output.height < input.height
            || input == output
        {
            self.spatial_scaler = None;
            return Ok(false);
        }
        let needs_rebuild = self.spatial_scaler.as_ref().is_none_or(|scaler| {
            scaler.input != self.main_target.extent || scaler.output != self.output_size
        });
        if needs_rebuild {
            let Some(scaler) = MetalFxSpatialScaler::create(
                &self.device,
                self.main_target.extent,
                self.output_size,
            ) else {
                self.spatial_scaler = None;
                return Ok(false);
            };
            crate::core_info!(
                "[MetalFX] spatial scaler created input={}x{} output={}x{}",
                self.main_target.extent.width,
                self.main_target.extent.height,
                self.output_size.width,
                self.output_size.height
            );
            self.spatial_scaler = Some(scaler);
            let replacement =
                create_private_target(&self.device, self.output_size, MTLPixelFormat::RGBA8Unorm)?;
            if let Some(old) = self.upscaled_scene.replace(replacement) {
                self.retire_private_target(old);
            }
        }
        Ok(self.spatial_scaler.is_some() && self.upscaled_scene.is_some())
    }

    fn invalidate_shader_pipelines(&mut self, shader: ShaderId) {
        let keys = self
            .pipelines
            .keys()
            .copied()
            .filter(|key| key.shader == ShaderKind::Custom(shader))
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(pipeline) = self.pipelines.remove(&key) {
                self.retired.push_back(PendingRetirement {
                    after_serial: self.submitted_serial,
                    resource: RetiredResource::Pipeline(pipeline),
                });
            }
        }
    }

    fn remove_texture_id(&mut self, texture: TextureId) -> bool {
        let names = self
            .names
            .iter()
            .filter_map(|(name, &id)| (id == texture).then_some(name.clone()))
            .collect::<Vec<_>>();
        for name in names {
            self.names.remove(&name);
        }
        self.video_leases.retain(|_, id| *id != texture);
        self.texture_revisions.remove(&texture);
        if let Some(texture) = self.textures.remove(&texture) {
            self.retire_texture(texture);
            self.content_revision = self.content_revision.wrapping_add(1);
            true
        } else {
            false
        }
    }

    fn insert_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
        cpu_readable: bool,
    ) -> Result<TextureId, String> {
        validate_texture_data(name, desc, &data)?;
        if let Some(old) = self.names.get(name).copied() {
            self.remove_texture_id(old);
        }
        let started = self.profiling_enabled.get().then(Instant::now);
        let raw = create_texture(&self.device, desc)?;
        let bytes = texture_data_bytes(&data);
        if let Some(bytes) = bytes {
            upload_texture(&raw, desc, [0, 0], desc.extent, bytes)?;
        }
        let cpu_pixels = match data {
            TextureData::Rgba8(rgba) if cpu_readable => PixelStorage::Rgba(rgba.to_vec()),
            TextureData::Rgba8(rgba) => PixelStorage::alpha_only(rgba),
            TextureData::Uninitialized => PixelStorage::None,
            TextureData::Bc3(_) | TextureData::Astc4x4(_) => PixelStorage::None,
        };
        let opaque = match data {
            TextureData::Rgba8(rgba) => rgba_is_opaque(rgba),
            _ => false,
        };
        let id = self.allocate_texture_id();
        self.names.insert(name.to_owned(), id);
        self.textures.insert(
            id,
            MetalTexture {
                raw,
                desc,
                info: TextureInfo {
                    width: desc.extent.width,
                    height: desc.extent.height,
                },
                cpu_pixels,
                opaque,
                external: None,
            },
        );
        self.mark_texture_changed(id);
        if let Some(started) = started {
            self.profile_texture_upload_ns.set(
                self.profile_texture_upload_ns
                    .get()
                    .saturating_add(elapsed_ns(started)),
            );
            self.profile_uploaded_bytes.set(
                self.profile_uploaded_bytes
                    .get()
                    .saturating_add(bytes.map_or(0, |bytes| bytes.len() as u64)),
            );
        }
        Ok(id)
    }

    fn target_for(&self, target: FrameTarget) -> Result<PrivateRenderTarget, String> {
        match target {
            FrameTarget::Main => Ok(self.main_target.clone()),
            FrameTarget::Offscreen(id) => {
                let target = self
                    .render_targets
                    .get(&id)
                    .ok_or_else(|| "unknown Metal render target".to_string())?;
                let texture = self
                    .textures
                    .get(&target.color)
                    .ok_or_else(|| "Metal render target texture is missing".to_string())?;
                Ok(PrivateRenderTarget {
                    texture: texture.raw.clone(),
                    extent: target.desc.extent,
                    format: pixel_format(target.desc.color_format),
                })
            }
        }
    }

    fn allocate_external_handle(&mut self) -> u64 {
        let id = self.next_external_id;
        self.next_external_id = self.next_external_id.wrapping_add(1).max(1);
        id
    }

    fn record_video_frame(&self, bytes: usize) {
        self.profile_video_uploaded_frames
            .set(self.profile_video_uploaded_frames.get().saturating_add(1));
        self.profile_video_uploaded_bytes.set(
            self.profile_video_uploaded_bytes
                .get()
                .saturating_add(bytes as u64),
        );
    }

    fn bind_imported_texture(
        &mut self,
        name: &str,
        raw: Texture,
        desc: TextureDesc,
        external: ImportedNative,
    ) -> Result<ExternalTextureHandle, String> {
        let handle = external.handle;
        let prepared = MetalTexture {
            raw,
            desc,
            info: TextureInfo {
                width: desc.extent.width,
                height: desc.extent.height,
            },
            cpu_pixels: PixelStorage::None,
            opaque: true,
            external: Some(external),
        };
        if let Some(old) = self.names.get(name).copied() {
            self.remove_texture_id(old);
        }
        let id = self.allocate_texture_id();
        self.names.insert(name.to_owned(), id);
        self.video_leases.insert(handle.opaque(), id);
        self.textures.insert(id, prepared);
        self.mark_texture_changed(id);
        Ok(handle)
    }

    fn ensure_cv_texture_cache(&mut self) -> Result<(), String> {
        if self.cv_texture_cache.is_none() {
            self.cv_texture_cache = Some(core_video::CvMetalTextureCache::new(&self.device)?);
        }
        Ok(())
    }

    fn encode_import_wait(&self, wait: GpuSyncToken) -> Result<(), String> {
        match wait.kind {
            GpuSyncKind::None => Ok(()),
            GpuSyncKind::MetalSharedEvent => {
                if wait.handle.is_null() {
                    return Err("MTLSharedEvent pointer is null".into());
                }
                let command_buffer = self.queue.commandBuffer().ok_or_else(|| {
                    "failed to create Metal command buffer for import wait".to_string()
                })?;
                unsafe {
                    let _: () = msg_send![
                        &*command_buffer,
                        encodeWaitForEvent: wait.handle,
                        value: wait.value
                    ];
                }
                command_buffer.commit();
                Ok(())
            }
            GpuSyncKind::VulkanSemaphore => {
                Err("MetalBackend cannot wait on a Vulkan semaphore".into())
            }
        }
    }

    fn ensure_yuv_pipeline(&mut self) -> Result<PipelineState, String> {
        if let Some(pipeline) = &self.yuv_pipeline {
            return Ok(pipeline.clone());
        }
        let vertex = self
            .library
            .newFunctionWithName(&NSString::from_str("fullscreen_vertex"))
            .ok_or_else(|| "Metal fullscreen_vertex is missing".to_string())?;
        let fragment = self
            .library
            .newFunctionWithName(&NSString::from_str("yuv_convert_fragment"))
            .ok_or_else(|| "Metal yuv_convert_fragment is missing".to_string())?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        attachment.setBlendingEnabled(false);
        let raw = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| format!("failed to create YUV convert pipeline: {error}"))?;
        self.yuv_pipeline = Some(raw.clone());
        Ok(raw)
    }

    fn convert_nv12(
        &mut self,
        luma: &Texture,
        chroma: &Texture,
        extent: Extent2D,
        video_range: bool,
    ) -> Result<Texture, String> {
        let destination = create_texture(
            &self.device,
            TextureDesc {
                extent,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsage::SAMPLED
                    | TextureUsage::RENDER_TARGET
                    | TextureUsage::TRANSFER_SRC,
            },
        )?;
        let pipeline = self.ensure_yuv_pipeline()?;
        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or_else(|| "failed to create Metal YUV convert command buffer".to_string())?;
        let target = PrivateRenderTarget {
            texture: destination.clone(),
            extent,
            format: MTLPixelFormat::RGBA8Unorm,
        };
        let pass = render_pass(&target, MTLLoadAction::DontCare, [0.0, 0.0, 0.0, 1.0]);
        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or_else(|| "failed to create Metal YUV convert encoder".to_string())?;
        encoder.setRenderPipelineState(&pipeline);
        let params = [if video_range { 1.0f32 } else { 0.0 }, 0.0, 0.0, 0.0];
        unsafe {
            encoder.setFragmentTexture_atIndex(Some(luma), 0);
            encoder.setFragmentTexture_atIndex(Some(chroma), 1);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
            encoder.setFragmentBytes_length_atIndex(
                value_bytes(&params),
                std::mem::size_of_val(&params),
                0,
            );
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        }
        encoder.endEncoding();
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        if command_buffer.status() == MTLCommandBufferStatus::Error {
            return Err(format_command_error(&command_buffer));
        }
        Ok(destination)
    }

    fn import_cv_pixel_buffer(
        &mut self,
        name: &str,
        image: &ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        self.ensure_cv_texture_cache()?;
        let info = core_video::inspect_pixel_buffer(image.handle)?;
        if info.extent != image.extent {
            crate::core_warn!(
                "[MetalBackend] CVPixelBuffer size {}x{} differs from host {}x{}; using buffer size",
                info.extent.width,
                info.extent.height,
                image.extent.width,
                image.extent.height
            );
        }
        if info.color.is_yuv() && info.plane_count < 2 {
            return Err("biplanar CVPixelBuffer is missing the chroma plane".into());
        }
        let mut retains = Vec::new();
        match image.ownership {
            ResourceOwnership::Borrowed => {}
            ResourceOwnership::Imported => {
                retains.push(unsafe { core_video::CfPtr::retain(image.handle) }?)
            }
            ResourceOwnership::Owned => {
                retains.push(unsafe { core_video::CfPtr::from_created(image.handle) }?)
            }
        }
        let (raw, desc) = if info.color.is_yuv() {
            let (luma, chroma) = {
                let cache = self
                    .cv_texture_cache
                    .as_ref()
                    .ok_or_else(|| "CVMetalTextureCache is missing".to_string())?;
                let luma = core_video::create_plane_texture(
                    cache,
                    image.handle,
                    MTLPixelFormat::R8Unorm,
                    info.extent.width as usize,
                    info.extent.height as usize,
                    0,
                )?;
                let chroma = core_video::create_plane_texture(
                    cache,
                    image.handle,
                    MTLPixelFormat::RG8Unorm,
                    (info.extent.width as usize).div_ceil(2),
                    (info.extent.height as usize).div_ceil(2),
                    1,
                )?;
                cache.flush();
                (luma, chroma)
            };
            let converted = self.convert_nv12(
                &luma.metal,
                &chroma.metal,
                info.extent,
                matches!(info.color, core_video::CvColor::Nv12Video),
            );
            retains.push(luma.cv_texture);
            retains.push(chroma.cv_texture);
            (
                converted?,
                TextureDesc {
                    extent: info.extent,
                    format: TextureFormat::Rgba8Unorm,
                    usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_SRC,
                },
            )
        } else {
            let plane = {
                let cache = self
                    .cv_texture_cache
                    .as_ref()
                    .ok_or_else(|| "CVMetalTextureCache is missing".to_string())?;
                let plane = core_video::create_plane_texture(
                    cache,
                    image.handle,
                    match info.color {
                        core_video::CvColor::Bgra => MTLPixelFormat::BGRA8Unorm,
                        _ => MTLPixelFormat::RGBA8Unorm,
                    },
                    info.extent.width as usize,
                    info.extent.height as usize,
                    0,
                )?;
                cache.flush();
                plane
            };
            let metal = plane.metal;
            retains.push(plane.cv_texture);
            (
                metal,
                TextureDesc {
                    extent: info.extent,
                    format: info.color.metal_format(),
                    usage: TextureUsage::SAMPLED,
                },
            )
        };
        let handle = ExternalTextureHandle::from_opaque(self.allocate_external_handle());
        self.bind_imported_texture(
            name,
            raw,
            desc,
            ImportedNative {
                handle,
                ownership: image.ownership,
                kind: image.kind,
                retains,
            },
        )
    }

    fn import_metal_texture(
        &mut self,
        name: &str,
        image: &ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        let raw = match image.ownership {
            ResourceOwnership::Owned => unsafe {
                objc2::rc::Retained::from_raw(image.handle.cast::<ProtocolObject<dyn MTLTexture>>())
            },
            _ => unsafe {
                objc2::rc::Retained::retain(image.handle.cast::<ProtocolObject<dyn MTLTexture>>())
            },
        }
        .ok_or_else(|| "invalid MTLTexture pointer".to_string())?;
        if raw.width() != image.extent.width as usize
            || raw.height() != image.extent.height as usize
        {
            return Err(format!(
                "MTLTexture size {}x{} does not match {}x{}",
                raw.width(),
                raw.height(),
                image.extent.width,
                image.extent.height
            ));
        }
        let format = match raw.pixelFormat() {
            MTLPixelFormat::BGRA8Unorm => TextureFormat::Bgra8Unorm,
            MTLPixelFormat::RGBA8Unorm => TextureFormat::Rgba8Unorm,
            other => {
                return Err(format!(
                    "unsupported imported MTLPixelFormat {other:?}; use RGBA8 or BGRA8"
                ));
            }
        };
        let handle = ExternalTextureHandle::from_opaque(self.allocate_external_handle());
        self.bind_imported_texture(
            name,
            raw,
            TextureDesc {
                extent: image.extent,
                format,
                usage: TextureUsage::SAMPLED,
            },
            ImportedNative {
                handle,
                ownership: image.ownership,
                kind: image.kind,
                retains: Vec::new(),
            },
        )
    }

    fn import_io_surface(
        &mut self,
        name: &str,
        image: &ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                image.extent.width as usize,
                image.extent.height as usize,
                false,
            )
        };
        descriptor.setTextureType(MTLTextureType::Type2D);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let io_surface = unsafe { &*image.handle.cast::<IOSurfaceRef>() };
        let raw = self
            .device
            .newTextureWithDescriptor_iosurface_plane(&descriptor, io_surface, 0)
            .ok_or_else(|| "failed to create Metal texture from IOSurface".to_string())?;
        let mut retains = Vec::new();
        match image.ownership {
            ResourceOwnership::Borrowed => {}
            ResourceOwnership::Imported => {
                retains.push(unsafe { core_video::CfPtr::retain(image.handle) }?)
            }
            ResourceOwnership::Owned => {
                retains.push(unsafe { core_video::CfPtr::from_created(image.handle) }?)
            }
        }
        let handle = ExternalTextureHandle::from_opaque(self.allocate_external_handle());
        self.bind_imported_texture(
            name,
            raw,
            TextureDesc {
                extent: image.extent,
                format: TextureFormat::Bgra8Unorm,
                usage: TextureUsage::SAMPLED,
            },
            ImportedNative {
                handle,
                ownership: image.ownership,
                kind: image.kind,
                retains,
            },
        )
    }
}

impl Clone for PrivateRenderTarget {
    fn clone(&self) -> Self {
        Self {
            texture: self.texture.clone(),
            extent: self.extent,
            format: self.format,
        }
    }
}

fn as_bytes<T>(values: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

fn value_bytes<T>(value: &T) -> NonNull<std::ffi::c_void> {
    NonNull::from(value).cast()
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn pixel_format(format: TextureFormat) -> MTLPixelFormat {
    match format {
        TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        TextureFormat::Bc3RgbaUnorm => MTLPixelFormat::BC3_RGBA,
        TextureFormat::Astc4x4RgbaUnorm => MTLPixelFormat::ASTC_4x4_LDR,
    }
}

fn texture_usage(usage: TextureUsage) -> MTLTextureUsage {
    let mut result = MTLTextureUsage::Unknown;
    if usage.contains(TextureUsage::SAMPLED) {
        result |= MTLTextureUsage::ShaderRead;
    }
    if usage.contains(TextureUsage::RENDER_TARGET) {
        result |= MTLTextureUsage::RenderTarget;
    }
    result
}

fn create_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    desc: TextureDesc,
) -> Result<Texture, String> {
    if desc.extent.is_empty() {
        return Err("Metal texture extent must be non-zero".into());
    }
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            pixel_format(desc.format),
            desc.extent.width as usize,
            desc.extent.height as usize,
            false,
        )
    };
    descriptor.setTextureType(MTLTextureType::Type2D);
    descriptor.setStorageMode(MTLStorageMode::Shared);
    descriptor.setUsage(texture_usage(desc.usage));
    device
        .newTextureWithDescriptor(&descriptor)
        .ok_or_else(|| format!("failed to allocate Metal texture {desc:?}"))
}

fn create_private_target(
    device: &ProtocolObject<dyn MTLDevice>,
    extent: Extent2D,
    format: MTLPixelFormat,
) -> Result<PrivateRenderTarget, String> {
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            format,
            extent.width as usize,
            extent.height as usize,
            false,
        )
    };
    descriptor.setTextureType(MTLTextureType::Type2D);
    descriptor.setStorageMode(MTLStorageMode::Private);
    descriptor.setUsage(
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite,
    );
    let texture = device
        .newTextureWithDescriptor(&descriptor)
        .ok_or_else(|| "failed to allocate Metal render target".to_string())?;
    Ok(PrivateRenderTarget {
        texture,
        extent,
        format,
    })
}

fn create_solid_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    rgba: [u8; 4],
) -> Result<Texture, String> {
    let desc = TextureDesc::sampled_rgba8(1, 1);
    let texture = create_texture(device, desc)?;
    upload_texture(&texture, desc, [0, 0], desc.extent, &rgba)?;
    Ok(texture)
}

fn texture_data_bytes<'a>(data: &'a TextureData<'a>) -> Option<&'a [u8]> {
    match data {
        TextureData::Uninitialized => None,
        TextureData::Rgba8(bytes) | TextureData::Bc3(bytes) | TextureData::Astc4x4(bytes) => {
            Some(bytes)
        }
    }
}

fn validate_texture_data(
    name: &str,
    desc: TextureDesc,
    data: &TextureData<'_>,
) -> Result<(), String> {
    if desc.extent.is_empty() {
        return Err(format!("texture {name} has an empty extent"));
    }
    let expected = match (desc.format, data) {
        (TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm, TextureData::Rgba8(_)) => {
            desc.extent.rgba8_len()
        }
        (TextureFormat::Bc3RgbaUnorm, TextureData::Bc3(_))
        | (TextureFormat::Astc4x4RgbaUnorm, TextureData::Astc4x4(_)) => desc.extent.block_4x4_len(),
        (_, TextureData::Uninitialized) => return Ok(()),
        _ => return Err(format!("texture {name} data does not match its format")),
    }
    .ok_or_else(|| format!("texture {name} byte size overflow"))?;
    let actual = texture_data_bytes(data).map_or(0, <[u8]>::len);
    if actual != expected {
        return Err(format!(
            "texture {name} has {actual} bytes, expected {expected}"
        ));
    }
    Ok(())
}

fn upload_texture(
    texture: &ProtocolObject<dyn MTLTexture>,
    desc: TextureDesc,
    origin: [u32; 2],
    extent: Extent2D,
    bytes: &[u8],
) -> Result<(), String> {
    let bytes_per_row = match desc.format {
        TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm => extent.width as usize * 4,
        TextureFormat::Bc3RgbaUnorm | TextureFormat::Astc4x4RgbaUnorm => {
            extent.width.div_ceil(4) as usize * 16
        }
    };
    let region = objc2_metal::MTLRegion {
        origin: MTLOrigin {
            x: origin[0] as usize,
            y: origin[1] as usize,
            z: 0,
        },
        size: MTLSize {
            width: extent.width as usize,
            height: extent.height as usize,
            depth: 1,
        },
    };
    let pointer = NonNull::new(bytes.as_ptr().cast_mut().cast())
        .ok_or_else(|| "Metal texture upload pointer is null".to_string())?;
    unsafe {
        texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, pointer, bytes_per_row)
    };
    Ok(())
}

fn rgba_is_opaque(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|pixel| pixel[3] == 255)
}

impl TextureProvider for MetalBackend {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        if let Some(&id) = self.names.get(name) {
            return self.textures.get(&id).map(|texture| (id, texture.info));
        }
        if crate::video::is_video_layer_texture_name(name) {
            return None;
        }

        if let Some(source) = &self.source
            && let Some(bytes) = source(name)
            && let Some((width, height, rgba)) = decode_rgba(&bytes)
        {
            let desc = TextureDesc::sampled_rgba8(width, height);
            return self
                .insert_texture(name, desc, TextureData::Rgba8(&rgba), false)
                .ok()
                .and_then(|id| self.textures.get(&id).map(|texture| (id, texture.info)));
        }

        let size = 256u32;
        let mut rgba = vec![0; size as usize * size as usize * 4];
        for y in 0..size {
            for x in 0..size {
                let offset = ((y * size + x) * 4) as usize;
                let color = if ((x / 32) + (y / 32)) % 2 == 0 {
                    [255, 0, 255, 255]
                } else {
                    [24, 24, 24, 255]
                };
                rgba[offset..offset + 4].copy_from_slice(&color);
            }
        }
        let desc = TextureDesc::sampled_rgba8(size, size);
        self.insert_texture(name, desc, TextureData::Rgba8(&rgba), false)
            .ok()
            .and_then(|id| self.textures.get(&id).map(|texture| (id, texture.info)))
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let desc = TextureDesc::sampled_rgba8(width, height);
        self.insert_texture(name, desc, TextureData::Rgba8(data), true)
            .ok()
            .map(|id| (id, TextureInfo { width, height }))
    }

    fn upload_rgba_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let desc = TextureDesc::sampled_rgba8(width, height);
        self.insert_texture(name, desc, TextureData::Rgba8(data), false)
            .ok()
            .map(|id| (id, TextureInfo { width, height }))
    }

    fn upload_dxt5_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        if cfg!(target_os = "ios") {
            return None;
        }
        let desc = TextureDesc {
            extent: Extent2D::new(width, height),
            format: TextureFormat::Bc3RgbaUnorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        self.insert_texture(name, desc, TextureData::Bc3(data), false)
            .ok()
            .map(|id| (id, TextureInfo { width, height }))
    }

    fn supports_astc_4x4(&self) -> bool {
        cfg!(target_os = "ios")
    }

    fn upload_astc_4x4_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        if !self.supports_astc_4x4() {
            return None;
        }
        let desc = TextureDesc {
            extent: Extent2D::new(width, height),
            format: TextureFormat::Astc4x4RgbaUnorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        self.insert_texture(name, desc, TextureData::Astc4x4(data), false)
            .ok()
            .map(|id| (id, TextureInfo { width, height }))
    }

    fn pixel_alpha(&self, texture: TextureId, x: u32, y: u32) -> Option<u8> {
        let texture = self.textures.get(&texture)?;
        if x >= texture.info.width || y >= texture.info.height {
            return None;
        }
        let pixel = y as usize * texture.info.width as usize + x as usize;
        match &texture.cpu_pixels {
            PixelStorage::None => None,
            PixelStorage::Opaque => Some(255),
            PixelStorage::Alpha(alpha) => alpha.get(pixel).copied(),
            PixelStorage::Rgba(rgba) => rgba.get(pixel * 4 + 3).copied(),
        }
    }

    fn texture_is_opaque(&self, texture: TextureId) -> bool {
        self.textures
            .get(&texture)
            .is_some_and(|texture| texture.opaque)
    }

    fn retain(&mut self, names: &HashSet<String>) {
        let stale = self
            .names
            .iter()
            .filter_map(|(name, &id)| (!names.contains(name)).then_some(id))
            .collect::<HashSet<_>>();
        for id in stale {
            self.remove_texture_id(id);
        }
    }

    fn solid_texture(&mut self, rgba: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        let name = crate::render_pipeline::draw::solid_texture_name(rgba);
        if let Some(&id) = self.names.get(&name) {
            return Some((
                id,
                TextureInfo {
                    width: 1,
                    height: 1,
                },
            ));
        }
        self.upload_rgba(&name, 1, 1, &rgba)
    }

    fn resolve_with_mask(&mut self, file: &str, mask: &str) -> Option<(TextureId, TextureInfo)> {
        let name = crate::render_pipeline::draw::masked_texture_name(file, mask);
        if let Some(&id) = self.names.get(&name) {
            return self.textures.get(&id).map(|texture| (id, texture.info));
        }
        let (width, height, mut foreground) = self.pixels_of(file)?;
        let (mask_width, mask_height, mask_pixels) = self.pixels_of(mask)?;
        if width != mask_width || height != mask_height {
            return self.resolve(file);
        }
        for (pixel, mask) in foreground
            .chunks_exact_mut(4)
            .zip(mask_pixels.chunks_exact(4))
        {
            let gray = ((mask[0] as u16 + mask[1] as u16 + mask[2] as u16) / 3) as u8;
            pixel[3] = ((pixel[3] as u16 * gray as u16) / 255) as u8;
        }
        self.upload_rgba(&name, width, height, &foreground)
    }

    fn pixels_of(&mut self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        let id = self
            .names
            .get(name)
            .copied()
            .or_else(|| self.resolve(name).map(|x| x.0))?;
        let texture = self.textures.get(&id)?;
        let PixelStorage::Rgba(rgba) = &texture.cpu_pixels else {
            return None;
        };
        Some((texture.info.width, texture.info.height, rgba.clone()))
    }
}

impl GpuBackend for MetalBackend {
    fn backend_info(&self) -> BackendInfo {
        BackendInfo {
            kind: BackendKind::Metal,
            name: "Metal",
            stability: BackendStability::Production,
            capabilities: BackendCapabilities {
                runtime_shader: true,
                hlsl_shader: true,
                offscreen_render_target: true,
                readback: true,
                compressed_astc: self.supports_astc_4x4(),
                compressed_bc: cfg!(target_os = "macos"),
                stencil: true,
                custom_shader: true,
                dynamic_mesh: true,
                spatial_upscaling: self.metalfx_supported,
                external_texture: true,
                zero_copy_video: true,
                ..BackendCapabilities::default()
            },
        }
    }

    fn begin_access(&mut self) {}

    fn end_access(&mut self) {}

    fn create_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
    ) -> Result<TextureId, String> {
        let cpu_readable = desc.usage.contains(TextureUsage::CPU_READABLE);
        self.insert_texture(name, desc, data, cpu_readable)
    }

    fn update_texture(
        &mut self,
        texture: TextureId,
        update: TextureUpdate<'_>,
    ) -> Result<(), String> {
        let stored = self
            .textures
            .get_mut(&texture)
            .ok_or_else(|| "unknown Metal texture".to_string())?;
        if !stored.desc.usage.contains(TextureUsage::TRANSFER_DST) {
            return Err("texture was not created with TRANSFER_DST usage".into());
        }
        let end_x = update.origin[0]
            .checked_add(update.extent.width)
            .ok_or("texture update x overflow")?;
        let end_y = update.origin[1]
            .checked_add(update.extent.height)
            .ok_or("texture update y overflow")?;
        if update.extent.is_empty()
            || end_x > stored.desc.extent.width
            || end_y > stored.desc.extent.height
        {
            return Err("texture update is outside the texture extent".into());
        }
        let bytes = texture_data_bytes(&update.data)
            .ok_or_else(|| "texture update has no payload".to_string())?;
        let update_desc = TextureDesc {
            extent: update.extent,
            format: stored.desc.format,
            usage: stored.desc.usage,
        };
        validate_texture_data("update", update_desc, &update.data)?;
        upload_texture(
            &stored.raw,
            stored.desc,
            update.origin,
            update.extent,
            bytes,
        )?;
        update_cpu_pixels(stored, update.origin, update.extent, &update.data);
        stored.opaque = if update.origin == [0, 0] && update.extent == stored.desc.extent {
            matches!(update.data, TextureData::Rgba8(rgba) if rgba_is_opaque(rgba))
        } else {
            stored.opaque && matches!(update.data, TextureData::Rgba8(rgba) if rgba_is_opaque(rgba))
        };
        self.mark_texture_changed(texture);
        Ok(())
    }

    fn destroy_texture(&mut self, texture: TextureId) {
        let target_ids = self
            .render_targets
            .iter()
            .filter_map(|(&id, target)| (target.color == texture).then_some(id))
            .collect::<Vec<_>>();
        for id in target_ids {
            self.render_targets.remove(&id);
        }
        self.remove_texture_id(texture);
    }

    fn create_render_target(
        &mut self,
        name: &str,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, String> {
        if desc.extent.is_empty() {
            return Err("Metal render target extent must be non-zero".into());
        }
        if !matches!(
            desc.color_format,
            TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm
        ) {
            return Err("Metal render targets currently require RGBA8 or BGRA8".into());
        }
        let mut usage = TextureUsage::RENDER_TARGET | TextureUsage::TRANSFER_SRC;
        if desc.sampled {
            usage |= TextureUsage::SAMPLED;
        }
        let texture_desc = TextureDesc {
            extent: desc.extent,
            format: desc.color_format,
            usage,
        };
        let color = self.insert_texture(name, texture_desc, TextureData::Uninitialized, false)?;
        let id = self.allocate_target_id();
        self.render_targets
            .insert(id, MetalRenderTarget { id, color, desc });
        Ok(RenderTarget { id, color, desc })
    }

    fn destroy_render_target(&mut self, target: RenderTargetId) {
        if let Some(render_target) = self.render_targets.remove(&target) {
            debug_assert_eq!(render_target.id, target);
            self.remove_texture_id(render_target.color);
        }
    }

    fn resize(&mut self, extent: Extent2D) -> Result<(), String> {
        let had_surface = self.surface.is_some();
        self.scene_size = extent;
        if !had_surface {
            self.output_size = extent;
        }
        let target_extent = self
            .post_process
            .resolve_render_size(self.scene_size, self.output_size);
        self.replace_scene_target(target_extent)?;
        Ok(())
    }

    fn render_dimensions(&self) -> Option<RenderDimensions> {
        Some(RenderDimensions::new(
            self.main_target.extent,
            self.output_size,
        ))
    }

    fn configure_post_process(&mut self, pipeline: PostProcessPipeline) -> Result<(), String> {
        let dimensions = RenderDimensions::new(self.main_target.extent, self.output_size);
        pipeline.validate(dimensions)?;
        let spatial_requested = pipeline.passes.iter().any(|pass| {
            matches!(pass, PostProcessPass::Upscale(config) if config.mode == UpscaleMode::Spatial)
        });
        if spatial_requested && !self.metalfx_supported {
            self.set_render_scale(1.0)?;
            self.post_process = PostProcessPipeline::default();
            crate::core_info!(
                "[MetalFX] spatial requested but unavailable; native render selected"
            );
            return Ok(());
        }
        self.set_render_scale(pipeline.render_scale)?;
        let mode = if spatial_requested {
            "spatial"
        } else {
            "linear"
        };
        crate::core_info!(
            "[MetalFX] configure mode={} scale={:.3} logical={}x{} render={}x{} output={}x{} active={}",
            mode,
            pipeline.render_scale,
            self.scene_size.width,
            self.scene_size.height,
            self.main_target.extent.width,
            self.main_target.extent.height,
            self.output_size.width,
            self.output_size.height,
            spatial_requested
                && self.metalfx_supported
                && self.output_size.width >= self.main_target.extent.width
                && self.output_size.height >= self.main_target.extent.height
                && self.output_size != self.main_target.extent
        );
        self.post_process = pipeline;
        Ok(())
    }

    fn set_render_scale(&mut self, scale: f32) -> Result<(), String> {
        if !scale.is_finite() || !(0.1..=1.0).contains(&scale) {
            return Err("render scale must be finite and in [0.1, 1.0]".into());
        }
        let mut pipeline = self.post_process.clone();
        pipeline.render_scale = scale;
        let extent = pipeline.resolve_render_size(self.scene_size, self.output_size);
        if extent != self.main_target.extent {
            self.replace_scene_target(extent)?;
        }
        Ok(())
    }

    fn begin_frame(&mut self, target: FrameTarget) -> Result<(), String> {
        if self.active_frame.is_some() {
            return Err("a Metal frame is already active".into());
        }
        let target = self.target_for(target)?;
        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or_else(|| "failed to create Metal command buffer".to_string())?;
        self.active_frame = Some(ActiveFrame {
            command_buffer,
            target,
        });
        self.shader_frame_index = self.shader_frame_index.wrapping_add(1);
        Ok(())
    }

    fn clear(&mut self, color: [f32; 4]) {
        let Some(frame) = self.active_frame.as_ref() else {
            crate::core_warn!("[MetalBackend] clear called without an active frame");
            return;
        };
        if let Err(error) = encode_clear(&frame.command_buffer, &frame.target, color) {
            crate::core_warn!("[MetalBackend] clear failed: {error}");
        }
    }

    fn end_frame(&mut self) {
        let Some(frame) = self.active_frame.take() else {
            return;
        };
        frame.command_buffer.commit();
        self.submitted_serial = self.submitted_serial.wrapping_add(1).max(1);
        self.inflight.push_back(SubmittedFrame {
            serial: self.submitted_serial,
            command_buffer: frame.command_buffer,
        });
    }

    fn capture_frame(&mut self, name: &str, extent: Extent2D) -> FrameCapture {
        let Some(frame) = self.active_frame.as_ref() else {
            return FrameCapture::Pixels(Vec::new());
        };
        let source = frame.target.texture.clone();
        let desc = TextureDesc {
            extent,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        let Ok(id) = self.insert_texture(name, desc, TextureData::Uninitialized, false) else {
            return FrameCapture::Pixels(Vec::new());
        };
        let Some(destination) = self.textures.get(&id).map(|texture| texture.raw.clone()) else {
            return FrameCapture::Pixels(Vec::new());
        };
        let Some(frame) = self.active_frame.as_ref() else {
            return FrameCapture::Pixels(Vec::new());
        };
        if encode_texture_copy(&frame.command_buffer, &source, &destination, extent).is_err() {
            self.remove_texture_id(id);
            return FrameCapture::Pixels(Vec::new());
        }
        FrameCapture::Texture(
            id,
            TextureInfo {
                width: extent.width,
                height: extent.height,
            },
            TextureOrigin::TopLeft,
        )
    }

    fn readback(
        &mut self,
        target: FrameTarget,
        extent: Extent2D,
        out: &mut [u8],
    ) -> Result<usize, String> {
        if self.active_frame.is_some() {
            return Err("cannot read back an active Metal frame".into());
        }
        let expected = extent.rgba8_len().ok_or("readback size overflow")?;
        if out.len() < expected {
            return Err(format!(
                "readback buffer has {} bytes, expected {expected}",
                out.len()
            ));
        }
        let target = self.target_for(target)?;
        if extent.width > target.extent.width || extent.height > target.extent.height {
            return Err("readback extent exceeds the Metal target".into());
        }
        let staging = self
            .device
            .newBufferWithLength_options(expected, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| "failed to create Metal readback buffer".to_string())?;
        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or_else(|| "failed to create Metal readback command buffer".to_string())?;
        let blit = command_buffer
            .blitCommandEncoder()
            .ok_or_else(|| "failed to create Metal blit encoder".to_string())?;
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &target.texture,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: extent.width as usize, height: extent.height as usize, depth: 1 },
                &staging,
                0,
                extent.width as usize * 4,
                expected,
            );
        }
        blit.endEncoding();
        command_buffer.commit();
        command_buffer.waitUntilCompleted();
        if command_buffer.status() == MTLCommandBufferStatus::Error {
            return Err(format_command_error(&command_buffer));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                staging.contents().as_ptr().cast::<u8>(),
                out.as_mut_ptr(),
                expected,
            );
        }
        Ok(expected)
    }

    fn render(&mut self, frame: &DrawList) -> RenderRegion {
        self.render_internal(frame, None, false)
    }

    fn render_damage(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion {
        self.render_internal(frame, Some(damage), false)
    }

    fn render_damage_visualized(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion {
        self.render_internal(frame, Some(damage), true)
    }

    fn render_visualized(&mut self, frame: &DrawList) -> RenderRegion {
        self.render_internal(frame, None, true)
    }

    fn clear_damage_overlay(&mut self, frame: &DrawList) -> Option<RenderRegion> {
        let region = self.last_damage_overlay?;
        Some(self.render_internal(frame, region.damage(), false))
    }

    fn replace_asset_source(&mut self, source: Box<AssetSource>) {
        let ids = self.textures.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.remove_texture_id(id);
        }
        self.names.clear();
        self.render_targets.clear();
        self.source = Some(source);
    }

    fn cached_texture_info(&self, name: &str) -> Option<TextureInfo> {
        self.names
            .get(name)
            .and_then(|id| self.textures.get(id))
            .map(|texture| texture.info)
    }

    fn texture_content_revision(&self) -> u64 {
        self.content_revision
    }

    fn changed_texture_ids_since(&self, revision: u64) -> HashSet<TextureId> {
        self.texture_revisions
            .iter()
            .filter_map(|(&id, &changed)| (changed > revision).then_some(id))
            .collect()
    }

    fn evict_texture_prefix(&mut self, prefix: &str) -> usize {
        let ids = self
            .names
            .iter()
            .filter_map(|(name, &id)| name.starts_with(prefix).then_some(id))
            .collect::<HashSet<_>>();
        for id in &ids {
            self.remove_texture_id(*id);
        }
        ids.len()
    }

    fn upload_video_rgba(&mut self, name: &str, width: u32, height: u32, rgba: &[u8]) -> bool {
        let Some(expected) = Extent2D::new(width, height).rgba8_len() else {
            return false;
        };
        if width == 0 || height == 0 || rgba.len() < expected {
            return false;
        }
        let rgba = &rgba[..expected];
        let started = self.profiling_enabled.get().then(std::time::Instant::now);
        if let Some(&id) = self.names.get(name)
            && let Some(stored) = self.textures.get(&id)
            && stored.external.is_none()
            && stored.info.width == width
            && stored.info.height == height
        {
            let raw = stored.raw.clone();
            let desc = stored.desc;
            if upload_texture(&raw, desc, [0, 0], desc.extent, rgba).is_err() {
                return false;
            }
            if let Some(stored) = self.textures.get_mut(&id) {
                stored.opaque = true;
                stored.cpu_pixels = PixelStorage::None;
            }
            self.mark_texture_changed(id);
            self.record_video_frame(rgba.len());
            if let Some(started) = started {
                self.profile_video_upload_ns.set(
                    self.profile_video_upload_ns
                        .get()
                        .saturating_add(elapsed_ns(started)),
                );
            }
            return true;
        }
        let desc = TextureDesc::sampled_rgba8(width, height);
        let Ok(id) = self.insert_texture(name, desc, TextureData::Rgba8(rgba), false) else {
            return false;
        };
        if let Some(stored) = self.textures.get_mut(&id) {
            stored.opaque = true;
            stored.cpu_pixels = PixelStorage::None;
        }
        self.record_video_frame(rgba.len());
        if let Some(started) = started {
            self.profile_video_upload_ns.set(
                self.profile_video_upload_ns
                    .get()
                    .saturating_add(elapsed_ns(started)),
            );
        }
        true
    }

    fn video_import_capability(&self) -> VideoImportCapability {
        VideoImportCapability {
            preferred: ExternalImageKind::CvPixelBuffer,
            cpu_rgba: true,
            cv_pixel_buffer: true,
            metal_texture: true,
            io_surface: true,
            ahardware_buffer: false,
            opengl_framebuffer: false,
        }
    }

    fn import_external_texture(
        &mut self,
        name: &str,
        image: ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        self.encode_import_wait(image.wait)?;
        let started = self.profiling_enabled.get().then(std::time::Instant::now);
        let handle = match image.kind {
            ExternalImageKind::CpuRgba => {
                let rgba = image
                    .rgba
                    .ok_or_else(|| "CPU RGBA import requires pixel bytes".to_string())?;
                if !self.upload_video_rgba(name, image.extent.width, image.extent.height, rgba) {
                    return Err("CPU RGBA video upload failed".into());
                }
                let id = *self
                    .names
                    .get(name)
                    .ok_or_else(|| "CPU RGBA video texture was not registered".to_string())?;
                let handle = ExternalTextureHandle::from_opaque(self.allocate_external_handle());
                self.video_leases.insert(handle.opaque(), id);
                if let Some(stored) = self.textures.get_mut(&id) {
                    stored.external = Some(ImportedNative {
                        handle,
                        ownership: image.ownership,
                        kind: image.kind,
                        retains: Vec::new(),
                    });
                }
                handle
            }
            ExternalImageKind::CvPixelBuffer => self.import_cv_pixel_buffer(name, &image)?,
            ExternalImageKind::MetalTexture => self.import_metal_texture(name, &image)?,
            ExternalImageKind::IoSurface => self.import_io_surface(name, &image)?,
            ExternalImageKind::AHardwareBuffer => {
                return Err("AHardwareBuffer import is a Vulkan extension point".into());
            }
            ExternalImageKind::OpenGlFramebuffer => {
                return Err("OpenGL framebuffer import is not available on MetalBackend".into());
            }
        };
        self.record_video_frame(image.extent.rgba8_len().unwrap_or(0));
        if let Some(started) = started {
            self.profile_video_upload_ns.set(
                self.profile_video_upload_ns
                    .get()
                    .saturating_add(elapsed_ns(started)),
            );
        }
        Ok(handle)
    }

    fn release_external_texture(&mut self, handle: ExternalTextureHandle) -> bool {
        let Some(id) = self.video_leases.remove(&handle.opaque()) else {
            return false;
        };
        self.remove_texture_id(id)
    }

    fn acquire_video_surface(
        &mut self,
        name: &str,
        extent: Extent2D,
    ) -> Result<VideoSurfaceHandle, String> {
        if extent.is_empty() {
            return Err("video surface extent must be non-zero".into());
        }
        if let Some(&id) = self.names.get(name)
            && let Some(stored) = self.textures.get(&id)
            && stored.info.width == extent.width
            && stored.info.height == extent.height
        {
            if let Some(external) = stored.external.as_ref() {
                return Ok(VideoSurfaceHandle::from_opaque(external.handle.opaque()));
            }
        }
        let desc = TextureDesc {
            extent,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsage::SAMPLED
                | TextureUsage::RENDER_TARGET
                | TextureUsage::TRANSFER_SRC
                | TextureUsage::TRANSFER_DST,
        };
        let raw = create_texture(&self.device, desc)?;
        let handle = ExternalTextureHandle::from_opaque(self.allocate_external_handle());
        self.bind_imported_texture(
            name,
            raw,
            desc,
            ImportedNative {
                handle,
                ownership: ResourceOwnership::Owned,
                kind: ExternalImageKind::MetalTexture,
                retains: Vec::new(),
            },
        )?;
        Ok(VideoSurfaceHandle::from_opaque(handle.opaque()))
    }

    fn commit_video_surface(&mut self, handle: VideoSurfaceHandle) -> bool {
        let Some(&id) = self.video_leases.get(&handle.opaque()) else {
            return false;
        };
        if let Some(stored) = self.textures.get_mut(&id) {
            stored.opaque = true;
            stored.cpu_pixels = PixelStorage::None;
        }
        self.mark_texture_changed(id);
        true
    }

    fn video_surface_consumed(&mut self, handle: VideoSurfaceHandle) -> bool {
        self.collect_retired_resources();
        if self.video_leases.contains_key(&handle.opaque()) {
            return false;
        }
        !self.retired.iter().any(|pending| match &pending.resource {
            RetiredResource::Texture(texture) => texture
                .external
                .as_ref()
                .is_some_and(|external| external.handle.opaque() == handle.opaque()),
            _ => false,
        })
    }

    fn capture_screenshot(&mut self, extent: Extent2D, out: &mut [u8]) -> Result<usize, String> {
        if self.active_frame.is_some() {
            return Err("cannot capture screenshot during an active Metal frame".into());
        }
        self.readback(FrameTarget::Main, extent, out)
    }

    fn set_native_surface(&mut self, surface: NativeSurface) -> Result<(), String> {
        self.surface = None;
        let result = match surface.kind {
            NativeSurfaceKind::AppleIoSurface => {
                let descriptor = unsafe {
                    MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                        MTLPixelFormat::BGRA8Unorm,
                        surface.extent.width as usize,
                        surface.extent.height as usize,
                        false,
                    )
                };
                descriptor.setTextureType(MTLTextureType::Type2D);
                descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
                let io_surface = unsafe { &*surface.handle.cast::<IOSurfaceRef>() };
                let texture = self
                    .device
                    .newTextureWithDescriptor_iosurface_plane(&descriptor, io_surface, 0)
                    .ok_or_else(|| "failed to create Metal texture from IOSurface".to_string())?;
                self.surface = Some(MetalSurface::Shared {
                    texture,
                    extent: surface.extent,
                });
                Ok(())
            }
            NativeSurfaceKind::AppleMetalLayer => {
                let layer = unsafe { Retained::retain(surface.handle.cast::<CAMetalLayer>()) }
                    .ok_or_else(|| "invalid CAMetalLayer pointer".to_string())?;
                layer.setDevice(Some(&self.device));
                layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
                layer.setFramebufferOnly(true);
                layer.setDrawableSize(CGSize {
                    width: surface.extent.width as f64,
                    height: surface.extent.height as f64,
                });
                self.surface = Some(MetalSurface::Layer {
                    layer,
                    extent: surface.extent,
                });
                Ok(())
            }
            NativeSurfaceKind::AppleMetalTexture => {
                let texture = unsafe {
                    Retained::retain(surface.handle.cast::<ProtocolObject<dyn MTLTexture>>())
                }
                .ok_or_else(|| "invalid MTLTexture pointer".to_string())?;
                self.surface = Some(MetalSurface::Shared {
                    texture,
                    extent: surface.extent,
                });
                Ok(())
            }
            NativeSurfaceKind::AndroidNativeWindow => {
                Err("ANativeWindow is unsupported by MetalBackend".into())
            }
        };
        if result.is_ok() {
            self.output_size = surface.extent;
            let extent = self
                .post_process
                .resolve_render_size(self.scene_size, self.output_size);
            if extent != self.main_target.extent {
                self.replace_scene_target(extent)?;
            }
            crate::core_info!(
                "[MetalFX] output surface={}x{} logical={}x{} render={}x{}",
                self.output_size.width,
                self.output_size.height,
                self.scene_size.width,
                self.scene_size.height,
                self.main_target.extent.width,
                self.main_target.extent.height
            );
        }
        result
    }

    fn clear_native_surface(&mut self) {
        self.surface = None;
        self.output_size = self.scene_size;
        let extent = self
            .post_process
            .resolve_render_size(self.scene_size, self.output_size);
        if extent != self.main_target.extent
            && let Err(error) = self.replace_scene_target(extent)
        {
            crate::core_warn!("[MetalFX] failed to restore scene target: {error}");
        }
    }

    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String> {
        let dimensions = RenderDimensions::new(self.main_target.extent, self.output_size);
        self.post_process.validate(dimensions)?;
        let source = self.main_target.texture.clone();
        match self.surface.as_ref() {
            Some(MetalSurface::Shared { texture, extent }) => {
                let target = PrivateRenderTarget {
                    texture: texture.clone(),
                    extent: *extent,
                    format: texture.pixelFormat(),
                };
                // Shared textures are displayed by Flutter from the same object.
                // Wait so `frameAvailable` cannot observe a half-written surface.
                self.execute_post_process(&source, &target, None, damage, true)
            }
            Some(MetalSurface::Layer { layer, extent }) => {
                let drawable = layer
                    .nextDrawable()
                    .ok_or_else(|| "CAMetalLayer has no drawable available".to_string())?;
                let target = PrivateRenderTarget {
                    texture: drawable.texture(),
                    extent: *extent,
                    format: layer.pixelFormat(),
                };
                self.execute_post_process(&source, &target, Some(&drawable), None, false)
            }
            None => Err("native Metal surface is not configured".into()),
        }
    }

    fn register_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, ShaderCompileError> {
        let compiled = ShaderCompiler::compile_hlsl(name, source)?;
        let source = NSString::from_str(&compiled.msl);
        let library = self
            .device
            .newLibraryWithSource_options_error(&source, None)
            .map_err(|error| {
                ShaderCompileError::new(name, format!("Metal MSL compilation failed: {error}"))
            })?;
        let entry = NSString::from_str(&compiled.msl_entry_point);
        let function = library.newFunctionWithName(&entry).ok_or_else(|| {
            ShaderCompileError::new(
                name,
                format!(
                    "Metal fragment function {} is missing",
                    compiled.msl_entry_point
                ),
            )
        })?;
        let shader = MetalShader {
            _library: library,
            function,
            compiled,
        };
        let (id, _old) = self.runtime_shaders.insert_or_replace(name, shader);
        self.invalidate_shader_pipelines(id);
        Ok(id)
    }

    fn unregister_shader(&mut self, name: &str) -> bool {
        let Some((id, _shader)) = self.runtime_shaders.remove(name) else {
            return false;
        };
        self.invalidate_shader_pipelines(id);
        true
    }

    fn collect_retired_resources(&mut self) {
        while let Some(frame) = self.inflight.front() {
            let status = frame.command_buffer.status();
            if status != MTLCommandBufferStatus::Completed
                && status != MTLCommandBufferStatus::Error
            {
                break;
            }
            self.completed_serial = frame.serial;
            if status == MTLCommandBufferStatus::Error {
                crate::core_warn!(
                    "[MetalBackend] {}",
                    format_command_error(&frame.command_buffer)
                );
            }
            self.inflight.pop_front();
        }
        while self
            .retired
            .front()
            .is_some_and(|pending| pending.after_serial <= self.completed_serial)
        {
            if let Some(pending) = self.retired.pop_front() {
                match pending.resource {
                    RetiredResource::Texture(texture) => drop(texture),
                    RetiredResource::Buffer(buffer) => drop(buffer),
                    RetiredResource::Pipeline(pipeline) => drop(pipeline),
                }
            }
        }
    }

    fn set_profile_enabled(&self, enabled: bool) {
        self.profiling_enabled.set(enabled);
        if !enabled {
            let _ = self.take_profile_stats();
        }
    }

    fn take_profile_stats(&self) -> GpuProfileStats {
        let texture_gpu_bytes = self.textures.values().fold(0u64, |sum, texture| {
            sum.saturating_add(texture.desc.extent.rgba8_len().unwrap_or(0) as u64)
        });
        let texture_cpu_bytes = self.textures.values().fold(0u64, |sum, texture| {
            sum.saturating_add(texture.cpu_pixels.allocated_bytes() as u64)
        });
        GpuProfileStats {
            texture_upload_ns: self.profile_texture_upload_ns.replace(0),
            uploaded_bytes: self.profile_uploaded_bytes.replace(0),
            video_upload_ns: self.profile_video_upload_ns.replace(0),
            video_uploaded_bytes: self.profile_video_uploaded_bytes.replace(0),
            video_uploaded_frames: self.profile_video_uploaded_frames.replace(0),
            draw_calls: self.profile_draw_calls.replace(0),
            vertices: self.profile_vertices.replace(0),
            texture_binds: self.profile_texture_binds.replace(0),
            dynamic_mesh_uploaded_bytes: self.profile_dynamic_mesh_bytes.replace(0),
            texture_count: self.textures.len() as u64,
            texture_gpu_bytes,
            texture_cpu_bytes,
            upscale_enabled: self.profile_upscale_enabled.replace(false),
            upscale_cpu_encode_ns: self.profile_upscale_cpu_encode_ns.replace(0),
            ..GpuProfileStats::default()
        }
    }
}

impl MetalBackend {
    fn render_internal(
        &mut self,
        frame: &DrawList,
        damage: Option<[f32; 4]>,
        visualize_damage: bool,
    ) -> RenderRegion {
        let current_region = RenderRegion::from_damage(damage);
        let repaint = self
            .last_damage_overlay
            .map(|previous| previous.union(current_region))
            .unwrap_or(current_region);
        let Some(active) = self.active_frame.as_ref() else {
            crate::core_warn!("[MetalBackend] render called without an active frame");
            return repaint;
        };
        let command_buffer = active.command_buffer.clone();
        let target = active.target.clone();
        let result = (|| {
            match repaint.damage() {
                None => encode_clear(&command_buffer, &target, [0.0, 0.0, 0.0, 1.0])?,
                Some(rect) => self.encode_clear_rect(&command_buffer, &target, rect)?,
            }
            self.encode_range(
                &command_buffer,
                &target,
                frame,
                0,
                frame.commands.len(),
                frame.shader_groups.len(),
                0,
                repaint.damage(),
            )?;
            if visualize_damage {
                self.encode_damage_overlay(&command_buffer, &target, current_region)?;
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            crate::core_warn!("[MetalBackend] render encoding failed: {error}");
        }
        self.last_damage_overlay = visualize_damage.then_some(current_region);
        repaint
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_range(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
        frame: &DrawList,
        start: usize,
        end: usize,
        group_limit: usize,
        depth: usize,
        damage: Option<[f32; 4]>,
    ) -> Result<(), String> {
        let mut index = start;
        while index < end {
            let Some((group_index, group)) = next_shader_group(frame, index, end, group_limit)
            else {
                let batch_end = ((index + 1)..end)
                    .find(|&candidate| {
                        next_shader_group(frame, candidate, end, group_limit).is_some()
                    })
                    .unwrap_or(end);
                self.encode_draw_batch(
                    command_buffer,
                    target,
                    &frame.commands[index..batch_end],
                    damage,
                )?;
                index = batch_end;
                continue;
            };

            let group_target = self.ensure_group_target(depth)?;
            encode_clear(command_buffer, &group_target, [0.0, 0.0, 0.0, 0.0])?;
            self.encode_range(
                command_buffer,
                &group_target,
                frame,
                group.start,
                group.end,
                group_index,
                depth + 1,
                None,
            )?;

            let mask_texture = if let Some([mask_start, mask_end]) = group.mask_range {
                let mask_target = self.ensure_mask_target(depth)?;
                encode_clear(command_buffer, &mask_target, [0.0, 0.0, 0.0, 0.0])?;
                self.encode_draw_batch(
                    command_buffer,
                    &mask_target,
                    &frame.mask_commands[mask_start..mask_end],
                    None,
                )?;
                Some(mask_target.texture)
            } else {
                None
            };

            let mut effect = group.effect.clone();
            effect.mask_texture = None;
            let composite = DrawCommand {
                texture: TextureId(0),
                size: TextureInfo {
                    width: group_target.extent.width,
                    height: group_target.extent.height,
                },
                transform: glam::Affine2::IDENTITY,
                opacity: 1.0,
                blend: shader_group_blend(&group),
                color: ColorFilter::default(),
                clip: ClipRect {
                    uv_offset: [0.0, 0.0],
                    uv_scale: [1.0, 1.0],
                    quad_size: [self.scene_size.width as f32, self.scene_size.height as f32],
                },
                clip_bounds: group.clip_bounds,
                shader: Some(effect),
                mesh: None,
                stencil: None,
                native_emote: None,
            };
            self.encode_draw(
                command_buffer,
                target,
                &composite,
                damage,
                Some(&group_target.texture),
                mask_texture.as_ref(),
                None,
                None,
            )?;
            index = group.end;
        }
        Ok(())
    }

    fn ensure_group_target(&mut self, depth: usize) -> Result<PrivateRenderTarget, String> {
        ensure_target(
            &self.device,
            &mut self.group_targets,
            depth,
            self.main_target.extent,
        )
    }

    fn ensure_mask_target(&mut self, depth: usize) -> Result<PrivateRenderTarget, String> {
        ensure_target(
            &self.device,
            &mut self.mask_targets,
            depth,
            self.main_target.extent,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_draw(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
        command: &DrawCommand,
        damage: Option<[f32; 4]>,
        source_override: Option<&Texture>,
        mask_override: Option<&Texture>,
        blend_override: Option<PipelineBlend>,
        stage_override: Option<Extent2D>,
    ) -> Result<(), String> {
        let encoder = self.draw_encoder(command_buffer, target)?;
        let result = self.encode_draw_command(
            &encoder,
            target,
            command,
            damage,
            source_override,
            mask_override,
            blend_override,
            stage_override,
        );
        encoder.endEncoding();
        result
    }

    fn encode_draw_batch(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
        commands: &[DrawCommand],
        damage: Option<[f32; 4]>,
    ) -> Result<(), String> {
        if commands.is_empty() {
            return Ok(());
        }
        let encoder = self.draw_encoder(command_buffer, target)?;
        let result = commands.iter().try_for_each(|command| {
            self.encode_draw_command(&encoder, target, command, damage, None, None, None, None)
        });
        encoder.endEncoding();
        result
    }

    fn draw_encoder(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
    ) -> Result<RenderCommandEncoder, String> {
        let pass = render_pass(target, MTLLoadAction::Load, [0.0; 4]);
        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or_else(|| "failed to create Metal render command encoder".to_string())?;
        encoder.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: target.extent.width as f64,
            height: target.extent.height as f64,
            znear: 0.0,
            zfar: 1.0,
        });
        unsafe {
            encoder.setFragmentTexture_atIndex(Some(&self.transparent_texture), 2);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
        }
        Ok(encoder)
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_draw_command(
        &mut self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        target: &PrivateRenderTarget,
        command: &DrawCommand,
        damage: Option<[f32; 4]>,
        source_override: Option<&Texture>,
        mask_override: Option<&Texture>,
        blend_override: Option<PipelineBlend>,
        stage_override: Option<Extent2D>,
    ) -> Result<(), String> {
        let stage = stage_override.unwrap_or(self.scene_size);
        let Some(scissor) = metal_scissor(
            intersect_optional(command.clip_bounds, damage),
            stage,
            target.extent,
        ) else {
            return Ok(());
        };
        let shader = self.shader_kind(command.shader.as_ref());
        let pipeline_key = PipelineKey {
            shader,
            blend: blend_override.unwrap_or(PipelineBlend::Draw(command.blend)),
            stencil: if shader == ShaderKind::AlphaMask {
                StencilMode::MaskComposite
            } else {
                StencilMode::Disabled
            },
            color_format: target.format,
        };
        let pipeline = self.pipeline(pipeline_key)?;
        let source = source_override
            .cloned()
            .or_else(|| {
                self.textures
                    .get(&command.texture)
                    .map(|texture| texture.raw.clone())
            })
            .ok_or_else(|| format!("unknown Metal texture {:?}", command.texture))?;
        let mask = mask_override
            .cloned()
            .or_else(|| {
                command
                    .shader
                    .as_ref()
                    .and_then(|effect| effect.mask_texture)
                    .and_then(|id| self.textures.get(&id))
                    .map(|texture| texture.raw.clone())
            })
            .unwrap_or_else(|| self.white_texture.clone());
        let user = command
            .shader
            .as_ref()
            .and_then(|effect| effect.user_texture)
            .and_then(|id| self.textures.get(&id))
            .map(|texture| texture.raw.clone())
            .unwrap_or_else(|| self.transparent_texture.clone());

        encoder.setRenderPipelineState(&pipeline);
        encoder.setScissorRect(scissor);

        let vertex_uniforms = vertex_uniforms(command, stage);
        unsafe {
            encoder.setVertexBytes_length_atIndex(
                value_bytes(&vertex_uniforms),
                std::mem::size_of::<VertexUniforms>(),
                1,
            );
        }

        if let ShaderKind::Custom(shader_id) = shader {
            let shader = self
                .runtime_shaders
                .get(shader_id)
                .ok_or_else(|| format!("unknown Metal runtime shader {:?}", shader_id))?;
            let effect = command
                .shader
                .as_ref()
                .ok_or_else(|| "custom Metal pipeline has no shader effect".to_string())?;
            let parameters = ShaderRuntimeParameters::from_draw(
                command,
                [stage.width as f32, stage.height as f32],
                vertex_uniforms.transform,
                self.shader_clock.elapsed().as_secs_f32(),
                self.shader_frame_index,
                &effect.uniforms,
            );
            let uniforms = shader.compiled.reflection.encode_uniforms(&parameters);
            unsafe {
                if let Some(pointer) = NonNull::new(uniforms.as_ptr().cast_mut().cast()) {
                    if let Some(resource) = shader
                        .compiled
                        .reflection
                        .resources
                        .iter()
                        .find(|resource| resource.kind == ShaderResourceKind::UniformBuffer)
                    {
                        encoder.setFragmentBytes_length_atIndex(
                            pointer,
                            uniforms.len(),
                            resource.binding as usize,
                        );
                    }
                }
                for resource in &shader.compiled.reflection.resources {
                    match resource.kind {
                        ShaderResourceKind::Texture(ShaderTexture::Foreground) => encoder
                            .setFragmentTexture_atIndex(Some(&source), resource.binding as usize),
                        ShaderResourceKind::Texture(ShaderTexture::Mask) => encoder
                            .setFragmentTexture_atIndex(Some(&mask), resource.binding as usize),
                        ShaderResourceKind::Texture(ShaderTexture::User) => encoder
                            .setFragmentTexture_atIndex(Some(&user), resource.binding as usize),
                        ShaderResourceKind::Texture(ShaderTexture::Background) => encoder
                            .setFragmentTexture_atIndex(
                                Some(&self.transparent_texture),
                                resource.binding as usize,
                            ),
                        ShaderResourceKind::Sampler => encoder.setFragmentSamplerState_atIndex(
                            Some(&self.sampler),
                            resource.binding as usize,
                        ),
                        ShaderResourceKind::UniformBuffer => {}
                    }
                }
            }
        } else {
            unsafe {
                encoder.setFragmentTexture_atIndex(Some(&source), 0);
                encoder.setFragmentTexture_atIndex(Some(&mask), 1);
                encoder.setFragmentTexture_atIndex(Some(&user), 3);
            }
            let sprite_uniforms = sprite_uniforms(command);
            let effect_uniforms = effect_uniforms(command);
            unsafe {
                encoder.setFragmentBytes_length_atIndex(
                    value_bytes(&sprite_uniforms),
                    std::mem::size_of::<SpriteUniforms>(),
                    0,
                );
                encoder.setFragmentBytes_length_atIndex(
                    value_bytes(&effect_uniforms),
                    std::mem::size_of::<EffectUniforms>(),
                    1,
                );
            }
        }

        let vertices = if let Some(mesh) = command
            .mesh
            .as_ref()
            .filter(|mesh| !mesh.vertices.is_empty())
        {
            let vertices = mesh
                .vertices
                .iter()
                .map(|vertex| Vertex {
                    position: [vertex[0], vertex[1]],
                    uv: [vertex[2], vertex[3]],
                })
                .collect::<Vec<_>>();
            let buffer = MetalBuffer::from_bytes(&self.device, as_bytes(&vertices))?;
            unsafe {
                encoder.setVertexBuffer_offset_atIndex(Some(&buffer.raw), 0, 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    vertices.len(),
                );
            }
            let count = vertices.len() as u64;
            let bytes = buffer.length as u64;
            self.retired.push_back(PendingRetirement {
                after_serial: self.submitted_serial.wrapping_add(1),
                resource: RetiredResource::Buffer(buffer),
            });
            (count, bytes)
        } else {
            unsafe {
                encoder.setVertexBuffer_offset_atIndex(Some(&self.quad_vertex.raw), 0, 0);
                encoder.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                    MTLPrimitiveType::Triangle,
                    6,
                    MTLIndexType::UInt16,
                    &self.quad_index.raw,
                    0,
                );
            }
            (6, 0)
        };
        if self.profiling_enabled.get() {
            self.profile_draw_calls
                .set(self.profile_draw_calls.get().saturating_add(1));
            self.profile_vertices
                .set(self.profile_vertices.get().saturating_add(vertices.0));
            self.profile_texture_binds
                .set(self.profile_texture_binds.get().saturating_add(4));
            self.profile_dynamic_mesh_bytes.set(
                self.profile_dynamic_mesh_bytes
                    .get()
                    .saturating_add(vertices.1),
            );
        }
        Ok(())
    }

    fn encode_clear_rect(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
        rect: [f32; 4],
    ) -> Result<(), String> {
        let command = solid_command(self.scene_size, [0.0, 0.0, 0.0], 1.0, Some(rect));
        let white = self.white_texture.clone();
        self.encode_draw(
            command_buffer,
            target,
            &command,
            None,
            Some(&white),
            None,
            Some(PipelineBlend::Replace),
            None,
        )
    }

    fn encode_damage_overlay(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        target: &PrivateRenderTarget,
        region: RenderRegion,
    ) -> Result<(), String> {
        const COLORS: [[f32; 3]; 4] = [
            [1.0, 0.12, 0.08],
            [0.05, 0.72, 1.0],
            [0.18, 1.0, 0.28],
            [1.0, 0.82, 0.05],
        ];
        let color = COLORS[self.damage_flash_index % COLORS.len()];
        self.damage_flash_index = self.damage_flash_index.wrapping_add(1);
        let command = solid_command(self.scene_size, color, 0.24, region.damage());
        let white = self.white_texture.clone();
        self.encode_draw(
            command_buffer,
            target,
            &command,
            None,
            Some(&white),
            None,
            None,
            None,
        )
    }

    fn pipeline(&mut self, key: PipelineKey) -> Result<PipelineState, String> {
        if let Some(pipeline) = self.pipelines.get(&key) {
            return Ok(pipeline.raw.clone());
        }
        let vertex_name = NSString::from_str("sprite_vertex");
        let fragment_name = NSString::from_str(match key.shader {
            ShaderKind::Sprite => "sprite_fragment",
            ShaderKind::AlphaMask => "alpha_mask_fragment",
            ShaderKind::GroupComposite => "group_composite_fragment",
            ShaderKind::RuleTransition => "rule_transition_fragment",
            ShaderKind::Custom(_) => "",
        });
        let vertex = self
            .library
            .newFunctionWithName(&vertex_name)
            .ok_or_else(|| "Metal vertex function is missing".to_string())?;
        let fragment = match key.shader {
            ShaderKind::Custom(id) => self
                .runtime_shaders
                .get(id)
                .map(|shader| shader.function.clone())
                .ok_or_else(|| format!("Metal runtime shader {:?} is missing", id))?,
            _ => self
                .library
                .newFunctionWithName(&fragment_name)
                .ok_or_else(|| format!("Metal fragment function {fragment_name} is missing"))?,
        };
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setPixelFormat(key.color_format);
        apply_blend(&attachment, key.blend);
        let raw = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| format!("failed to create Metal pipeline {key:?}: {error}"))?;
        self.pipelines
            .insert(key, MetalPipeline { raw: raw.clone() });
        Ok(raw)
    }

    fn shader_kind(&self, effect: Option<&ShaderEffect>) -> ShaderKind {
        match effect.map(|effect| effect.name.as_str()) {
            Some(crate::render_pipeline::shader::ALPHA_MASK_SHADER) => ShaderKind::AlphaMask,
            Some(crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER) => {
                ShaderKind::GroupComposite
            }
            Some(crate::render_pipeline::shader::RULE_TRANS_SHADER) => ShaderKind::RuleTransition,
            Some(name) => self
                .runtime_shaders
                .id(name)
                .map(ShaderKind::Custom)
                .unwrap_or(ShaderKind::Sprite),
            None => ShaderKind::Sprite,
        }
    }

    fn execute_post_process(
        &mut self,
        source: &Texture,
        target: &PrivateRenderTarget,
        drawable: Option<&ProtocolObject<dyn CAMetalDrawable>>,
        damage: Option<[f32; 4]>,
        wait: bool,
    ) -> Result<(), String> {
        for pass in &self.post_process.passes {
            match pass {
                PostProcessPass::Upscale(config) if config.mode == UpscaleMode::Linear => {}
                PostProcessPass::Upscale(config) if config.mode == UpscaleMode::Spatial => {}
                _ => return Err("unsupported Metal post-process pass".into()),
            }
        }
        let spatial = self.post_process.passes.iter().any(|pass| {
            matches!(pass, PostProcessPass::Upscale(config) if config.mode == UpscaleMode::Spatial)
        });
        let spatial_has_work = self.metalfx_supported
            && self.output_size.width >= self.main_target.extent.width
            && self.output_size.height >= self.main_target.extent.height
            && self.output_size != self.main_target.extent;
        if spatial && spatial_has_work {
            if self.ensure_spatial_resources()? {
                let command_buffer = self
                    .queue
                    .commandBuffer()
                    .ok_or_else(|| "failed to create MetalFX command buffer".to_string())?;
                let encode_started = Instant::now();
                let upscaled = self
                    .upscaled_scene
                    .as_ref()
                    .ok_or_else(|| "MetalFX output texture is unavailable".to_string())?;
                let upscaled_texture = upscaled.texture.clone();
                self.spatial_scaler
                    .as_ref()
                    .ok_or_else(|| "MetalFX spatial scaler is unavailable".to_string())?
                    .encode(&command_buffer, &**source, &*upscaled.texture);
                self.profile_upscale_enabled.set(true);
                self.profile_upscale_cpu_encode_ns.set(
                    self.profile_upscale_cpu_encode_ns
                        .get()
                        .saturating_add(elapsed_ns(encode_started)),
                );
                return self.encode_present_command(
                    &command_buffer,
                    &upscaled_texture,
                    target,
                    drawable,
                    // MetalFX produces a complete output texture. Present it as
                    // one complete frame so a transition cannot expose stale,
                    // pre-upscale pixels outside a logical damage rectangle.
                    None,
                    wait,
                );
            }
            crate::core_warn!("[MetalFX] scaler creation failed; using linear present");
        }
        self.encode_present(source, target, drawable, damage, wait)
    }

    fn encode_present(
        &mut self,
        source: &Texture,
        target: &PrivateRenderTarget,
        drawable: Option<&ProtocolObject<dyn CAMetalDrawable>>,
        damage: Option<[f32; 4]>,
        wait: bool,
    ) -> Result<(), String> {
        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or_else(|| "failed to create Metal present command buffer".to_string())?;
        self.encode_present_command(&command_buffer, source, target, drawable, damage, wait)
    }

    fn encode_present_command(
        &mut self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        source: &Texture,
        target: &PrivateRenderTarget,
        drawable: Option<&ProtocolObject<dyn CAMetalDrawable>>,
        damage: Option<[f32; 4]>,
        wait: bool,
    ) -> Result<(), String> {
        let command = DrawCommand {
            texture: TextureId(0),
            size: TextureInfo {
                width: source.width() as u32,
                height: source.height() as u32,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [self.scene_size.width as f32, self.scene_size.height as f32],
            },
            clip_bounds: damage,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        };
        self.encode_draw(
            command_buffer,
            target,
            &command,
            None,
            Some(source),
            None,
            Some(PipelineBlend::Replace),
            Some(self.scene_size),
        )?;
        if let Some(drawable) = drawable {
            command_buffer.presentDrawable(drawable.as_ref());
        }
        command_buffer.commit();
        self.submitted_serial = self.submitted_serial.wrapping_add(1).max(1);
        if wait {
            command_buffer.waitUntilCompleted();
            if command_buffer.status() == MTLCommandBufferStatus::Error {
                return Err(format_command_error(&command_buffer));
            }
            self.completed_serial = self.submitted_serial;
        } else {
            self.inflight.push_back(SubmittedFrame {
                serial: self.submitted_serial,
                command_buffer: command_buffer.to_owned().into(),
            });
        }
        Ok(())
    }
}

fn ensure_target(
    device: &ProtocolObject<dyn MTLDevice>,
    targets: &mut Vec<PrivateRenderTarget>,
    depth: usize,
    extent: Extent2D,
) -> Result<PrivateRenderTarget, String> {
    while targets.len() <= depth {
        targets.push(create_private_target(
            device,
            extent,
            MTLPixelFormat::RGBA8Unorm,
        )?);
    }
    if targets[depth].extent != extent {
        targets[depth] = create_private_target(device, extent, MTLPixelFormat::RGBA8Unorm)?;
    }
    Ok(targets[depth].clone())
}

fn render_pass(
    target: &PrivateRenderTarget,
    load: MTLLoadAction,
    clear: [f32; 4],
) -> Retained<MTLRenderPassDescriptor> {
    let pass = MTLRenderPassDescriptor::new();
    let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
    attachment.setTexture(Some(&target.texture));
    attachment.setLoadAction(load);
    attachment.setStoreAction(MTLStoreAction::Store);
    attachment.setClearColor(objc2_metal::MTLClearColor {
        red: clear[0] as f64,
        green: clear[1] as f64,
        blue: clear[2] as f64,
        alpha: clear[3] as f64,
    });
    pass
}

fn encode_clear(
    command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    target: &PrivateRenderTarget,
    color: [f32; 4],
) -> Result<(), String> {
    let pass = render_pass(target, MTLLoadAction::Clear, color);
    let encoder = command_buffer
        .renderCommandEncoderWithDescriptor(&pass)
        .ok_or_else(|| "failed to create Metal clear encoder".to_string())?;
    encoder.endEncoding();
    Ok(())
}

fn encode_texture_copy(
    command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    source: &ProtocolObject<dyn MTLTexture>,
    destination: &ProtocolObject<dyn MTLTexture>,
    extent: Extent2D,
) -> Result<(), String> {
    let blit = command_buffer
        .blitCommandEncoder()
        .ok_or_else(|| "failed to create Metal capture blit encoder".to_string())?;
    unsafe {
        blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
            source,
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
            MTLSize { width: extent.width as usize, height: extent.height as usize, depth: 1 },
            destination,
            0,
            0,
            MTLOrigin { x: 0, y: 0, z: 0 },
        );
    }
    blit.endEncoding();
    Ok(())
}

fn apply_blend(
    attachment: &objc2_metal::MTLRenderPipelineColorAttachmentDescriptor,
    blend: PipelineBlend,
) {
    if blend == PipelineBlend::Replace {
        attachment.setBlendingEnabled(false);
        return;
    }
    attachment.setBlendingEnabled(true);
    attachment.setRgbBlendOperation(MTLBlendOperation::Add);
    attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
    match blend {
        PipelineBlend::Replace => unreachable!(),
        PipelineBlend::Draw(BlendMode::Alpha) => {
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        }
        PipelineBlend::Draw(BlendMode::PremultipliedAlpha) => {
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        }
        PipelineBlend::Draw(BlendMode::PremultipliedAdd) => {
            set_factors(attachment, MTLBlendFactor::One, MTLBlendFactor::One);
        }
        PipelineBlend::Draw(BlendMode::Add) => {
            set_factors(attachment, MTLBlendFactor::SourceAlpha, MTLBlendFactor::One);
        }
        PipelineBlend::Draw(BlendMode::Screen) => {
            set_factors(
                attachment,
                MTLBlendFactor::One,
                MTLBlendFactor::OneMinusSourceColor,
            );
        }
        PipelineBlend::Draw(BlendMode::Multiply) => {
            set_factors(
                attachment,
                MTLBlendFactor::DestinationColor,
                MTLBlendFactor::OneMinusSourceAlpha,
            );
        }
        PipelineBlend::Draw(BlendMode::NativeAdd) => {
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::One);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::Zero);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);
        }
        PipelineBlend::Draw(BlendMode::NativeReverseSubtract) => {
            attachment.setRgbBlendOperation(MTLBlendOperation::ReverseSubtract);
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::One);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::Zero);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);
        }
        PipelineBlend::Draw(BlendMode::NativeMultiply) => {
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::DestinationColor);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::Zero);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);
        }
        PipelineBlend::Draw(BlendMode::NativeScreen) => {
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::OneMinusDestinationColor);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::One);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::Zero);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);
        }
    }
}

fn set_factors(
    attachment: &objc2_metal::MTLRenderPipelineColorAttachmentDescriptor,
    source: MTLBlendFactor,
    destination: MTLBlendFactor,
) {
    attachment.setSourceRGBBlendFactor(source);
    attachment.setDestinationRGBBlendFactor(destination);
    attachment.setSourceAlphaBlendFactor(source);
    attachment.setDestinationAlphaBlendFactor(destination);
}

fn shader_group_blend(group: &ShaderGroup) -> BlendMode {
    if group.effect.name != crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER {
        return BlendMode::PremultipliedAlpha;
    }
    match group
        .effect
        .uniforms
        .get("blendMode")
        .and_then(|values| values.first())
        .copied()
        .unwrap_or(0.0) as i32
    {
        1 => BlendMode::PremultipliedAdd,
        2 => BlendMode::Screen,
        3 => BlendMode::Multiply,
        _ => BlendMode::PremultipliedAlpha,
    }
}

fn next_shader_group(
    frame: &DrawList,
    start: usize,
    end: usize,
    group_limit: usize,
) -> Option<(usize, ShaderGroup)> {
    frame
        .shader_groups
        .iter()
        .enumerate()
        .take(group_limit)
        .filter(|(_, group)| group.start == start && group.end > start && group.end <= end)
        .max_by_key(|(index, group)| (group.end, *index))
        .map(|(index, group)| (index, group.clone()))
}

fn vertex_uniforms(command: &DrawCommand, stage: Extent2D) -> VertexUniforms {
    let matrix = command.transform.matrix2;
    let translation = command.transform.translation;
    let transform = glam::Mat4::from_cols(
        glam::Vec4::new(matrix.x_axis.x, matrix.x_axis.y, 0.0, 0.0),
        glam::Vec4::new(matrix.y_axis.x, matrix.y_axis.y, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
        glam::Vec4::new(translation.x, translation.y, 0.0, 1.0),
    );
    let projection = glam::Mat4::from_cols(
        glam::Vec4::new(2.0 / stage.width as f32, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, -2.0 / stage.height as f32, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
        glam::Vec4::new(-1.0, 1.0, 0.0, 1.0),
    );
    VertexUniforms {
        transform: (projection * transform).to_cols_array(),
        size: if command.mesh.is_some() {
            [1.0, 1.0]
        } else {
            command.clip.quad_size
        },
        uv_offset: command.clip.uv_offset,
        uv_scale: command.clip.uv_scale,
        padding: [0.0; 2],
    }
}

fn sprite_uniforms(command: &DrawCommand) -> SpriteUniforms {
    let material = command.native_emote;
    let default_colors = [[1.0; 4]; 4];
    let colors = material
        .map(|material| material.corner_colors)
        .unwrap_or(default_colors);
    SpriteUniforms {
        opacity_flags: [
            command.opacity,
            command.color.grayscale as u8 as f32,
            command.color.negative as u8 as f32,
            material.is_some() as u8 as f32,
        ],
        multiply: [
            command.color.multiply[0],
            command.color.multiply[1],
            command.color.multiply[2],
            0.0,
        ],
        emote_uv_rect: material.map(|value| value.uv_rect).unwrap_or([0.0; 4]),
        emote_color_tl: colors[0],
        emote_color_tr: colors[1],
        emote_color_bl: colors[2],
        emote_color_br: colors[3],
        emote_blend_mode: [
            material.map(|value| value.blend_mode as f32).unwrap_or(0.0),
            0.0,
            0.0,
            0.0,
        ],
        emote_clip_rect: material
            .map(|value| value.clip_rect)
            .unwrap_or([0.0, 0.0, 1.0, 1.0]),
        emote_wipe: material
            .map(|value| [value.wipe[0], value.wipe[1], value.wipe[2], 0.0])
            .unwrap_or([0.0; 4]),
    }
}

fn effect_uniforms(command: &DrawCommand) -> EffectUniforms {
    let uniform = |name: &str, index: usize, default: f32| {
        command
            .shader
            .as_ref()
            .and_then(|effect| effect.uniforms.get(name))
            .and_then(|values| values.get(index))
            .copied()
            .unwrap_or(default)
    };
    EffectUniforms {
        alpha_progress_vague_opaque: [
            uniform("alpha", 0, command.opacity),
            uniform("progress", 0, 0.0),
            uniform("vague", 0, 0.0),
            uniform("opaque", 0, 0.0),
        ],
        color_multiply_grayscale: [
            uniform("colorMultiply", 0, command.color.multiply[0]),
            uniform("colorMultiply", 1, command.color.multiply[1]),
            uniform("colorMultiply", 2, command.color.multiply[2]),
            uniform("grayscale", 0, command.color.grayscale as u8 as f32),
        ],
        negative_padding: [
            uniform("negative", 0, command.color.negative as u8 as f32),
            0.0,
            0.0,
            0.0,
        ],
    }
}

fn intersect_optional(left: Option<[f32; 4]>, right: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match (left, right) {
        (Some(left), Some(right)) => intersect_rect(left, right),
        (Some(rect), None) | (None, Some(rect)) => Some(rect),
        (None, None) => None,
    }
}

fn intersect_rect(left: [f32; 4], right: [f32; 4]) -> Option<[f32; 4]> {
    let x0 = left[0].max(right[0]);
    let y0 = left[1].max(right[1]);
    let x1 = (left[0] + left[2]).min(right[0] + right[2]);
    let y1 = (left[1] + left[3]).min(right[1] + right[3]);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

fn metal_scissor(
    rect: Option<[f32; 4]>,
    stage: Extent2D,
    target: Extent2D,
) -> Option<MTLScissorRect> {
    let rect = rect.unwrap_or([0.0, 0.0, stage.width as f32, stage.height as f32]);
    let rect = intersect_rect(rect, [0.0, 0.0, stage.width as f32, stage.height as f32])?;
    let scale_x = target.width as f32 / stage.width as f32;
    let scale_y = target.height as f32 / stage.height as f32;
    let left = (rect[0] * scale_x).floor().max(0.0) as usize;
    let top = (rect[1] * scale_y).floor().max(0.0) as usize;
    let right = ((rect[0] + rect[2]) * scale_x)
        .ceil()
        .min(target.width as f32) as usize;
    let bottom = ((rect[1] + rect[3]) * scale_y)
        .ceil()
        .min(target.height as f32) as usize;
    (right > left && bottom > top).then_some(MTLScissorRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn solid_command(
    extent: Extent2D,
    multiply: [f32; 3],
    opacity: f32,
    clip_bounds: Option<[f32; 4]>,
) -> DrawCommand {
    DrawCommand {
        texture: TextureId(0),
        size: TextureInfo {
            width: 1,
            height: 1,
        },
        transform: glam::Affine2::IDENTITY,
        opacity,
        blend: BlendMode::Alpha,
        color: ColorFilter {
            multiply,
            grayscale: false,
            negative: false,
        },
        clip: ClipRect {
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
            quad_size: [extent.width as f32, extent.height as f32],
        },
        clip_bounds,
        shader: None,
        mesh: None,
        stencil: None,
        native_emote: None,
    }
}

fn update_cpu_pixels(
    texture: &mut MetalTexture,
    origin: [u32; 2],
    extent: Extent2D,
    data: &TextureData<'_>,
) {
    let TextureData::Rgba8(rgba) = data else {
        texture.cpu_pixels = PixelStorage::None;
        return;
    };
    let update_alpha = |alpha: &mut [u8]| {
        for row in 0..extent.height as usize {
            for column in 0..extent.width as usize {
                let source = (row * extent.width as usize + column) * 4 + 3;
                let destination = (origin[1] as usize + row) * texture.info.width as usize
                    + origin[0] as usize
                    + column;
                alpha[destination] = rgba[source];
            }
        }
    };
    match &mut texture.cpu_pixels {
        PixelStorage::None => {}
        PixelStorage::Opaque if rgba_is_opaque(rgba) => {}
        PixelStorage::Opaque => {
            let mut alpha = vec![255; texture.info.width as usize * texture.info.height as usize];
            update_alpha(&mut alpha);
            texture.cpu_pixels = PixelStorage::Alpha(alpha);
        }
        PixelStorage::Alpha(alpha) => update_alpha(alpha),
        PixelStorage::Rgba(pixels) => {
            for row in 0..extent.height as usize {
                let source = row * extent.width as usize * 4;
                let destination = ((origin[1] as usize + row) * texture.info.width as usize
                    + origin[0] as usize)
                    * 4;
                let len = extent.width as usize * 4;
                pixels[destination..destination + len].copy_from_slice(&rgba[source..source + len]);
            }
        }
    }
}

fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some((width, height, image.into_raw()))
}

fn format_command_error(command_buffer: &ProtocolObject<dyn MTLCommandBuffer>) -> String {
    command_buffer
        .error()
        .map(|error| format!("Metal command buffer failed: {error}"))
        .unwrap_or_else(|| "Metal command buffer failed without NSError".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_key_separates_blend_shader_and_format() {
        let alpha = PipelineKey {
            shader: ShaderKind::Sprite,
            blend: PipelineBlend::Draw(BlendMode::Alpha),
            stencil: StencilMode::Disabled,
            color_format: MTLPixelFormat::RGBA8Unorm,
        };
        assert_ne!(
            alpha,
            PipelineKey {
                blend: PipelineBlend::Draw(BlendMode::Add),
                ..alpha
            }
        );
        assert_ne!(
            alpha,
            PipelineKey {
                shader: ShaderKind::AlphaMask,
                stencil: StencilMode::MaskComposite,
                ..alpha
            }
        );
        assert_ne!(
            alpha,
            PipelineKey {
                color_format: MTLPixelFormat::BGRA8Unorm,
                ..alpha
            }
        );
    }

    #[test]
    fn scissor_uses_top_left_stage_coordinates() {
        assert_eq!(
            metal_scissor(
                Some([10.0, 5.0, 20.0, 10.0]),
                Extent2D::new(100, 50),
                Extent2D::new(200, 100),
            ),
            Some(MTLScissorRect {
                x: 20,
                y: 10,
                width: 40,
                height: 20,
            })
        );
    }

    #[test]
    fn native_metal_renders_sprite_mesh_and_offscreen_target() {
        let Ok(mut backend) = MetalBackend::new(4, 4) else {
            return;
        };
        let red = backend
            .create_texture(
                "red",
                TextureDesc::sampled_rgba8(1, 1),
                TextureData::Rgba8(&[255, 0, 0, 255]),
            )
            .expect("red texture");
        let command = DrawCommand {
            texture: red,
            size: TextureInfo {
                width: 1,
                height: 1,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [4.0, 4.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        };
        let mut frame = DrawList::new();
        frame.push(command.clone());
        frame.push(command.clone());
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();

        let mut pixels = [0; 64];
        backend
            .readback(FrameTarget::Main, Extent2D::new(4, 4), &mut pixels)
            .unwrap();
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 0, 0, 255])
        );
        let cached_pipelines = backend.pipelines.len();

        let target = backend
            .create_render_target("offscreen", RenderTargetDesc::sampled_rgba8(4, 4))
            .unwrap();
        let mut mesh_command = command;
        mesh_command.color.multiply = [0.0, 1.0, 0.0];
        mesh_command.mesh = Some(crate::render_pipeline::draw::DrawMesh {
            vertices: vec![
                [0.0, 0.0, 0.0, 0.0],
                [4.0, 0.0, 1.0, 0.0],
                [4.0, 4.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
                [4.0, 4.0, 1.0, 1.0],
                [0.0, 4.0, 0.0, 1.0],
            ]
            .into(),
        });
        let mut mesh_frame = DrawList::new();
        mesh_frame.push(mesh_command);
        backend
            .begin_frame(FrameTarget::Offscreen(target.id))
            .unwrap();
        backend.render(&mesh_frame);
        backend.end_frame();
        backend
            .readback(
                FrameTarget::Offscreen(target.id),
                Extent2D::new(4, 4),
                &mut pixels,
            )
            .unwrap();
        assert!(pixels.chunks_exact(4).all(|pixel| pixel == [0, 0, 0, 255]));
        assert_eq!(backend.pipelines.len(), cached_pipelines);
        backend.destroy_render_target(target.id);
        backend.collect_retired_resources();
        assert!(backend.retired.is_empty());
    }

    #[test]
    fn native_metal_executes_stencil_mask_group() {
        let Ok(mut backend) = MetalBackend::new(4, 4) else {
            return;
        };
        let red = backend
            .upload_rgba("red", 1, 1, &[255, 0, 0, 255])
            .unwrap()
            .0;
        let white = backend
            .upload_rgba("mask", 1, 1, &[255, 255, 255, 255])
            .unwrap()
            .0;
        let quad = |texture| DrawCommand {
            texture,
            size: TextureInfo {
                width: 1,
                height: 1,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [4.0, 4.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        };
        let mut frame = DrawList::new();
        frame.push(quad(red));
        frame.mask_commands.push(quad(white));
        frame.shader_groups.push(ShaderGroup {
            key: None,
            start: 0,
            end: 1,
            effect: ShaderEffect {
                name: crate::render_pipeline::shader::ALPHA_MASK_SHADER.to_owned(),
                uniforms: Default::default(),
                mask_texture: None,
                user_texture: None,
            },
            clip_bounds: None,
            mask_range: Some([0, 1]),
        });
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();
        let pixels = backend
            .readback_owned(FrameTarget::Main, Extent2D::new(4, 4))
            .unwrap();
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 0, 0, 255])
        );
        assert!(backend.pipelines.keys().any(|key| {
            key.shader == ShaderKind::AlphaMask && key.stencil == StencilMode::MaskComposite
        }));
    }

    #[test]
    fn native_metal_group_composite_applies_group_alpha() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        let red = backend
            .upload_rgba("red", 1, 1, &[255, 0, 0, 255])
            .unwrap()
            .0;
        let command = DrawCommand {
            texture: red,
            size: TextureInfo {
                width: 1,
                height: 1,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [2.0, 2.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        };
        let mut frame = DrawList::new();
        frame.push(command);
        frame.shader_groups.push(ShaderGroup {
            key: None,
            start: 0,
            end: 1,
            effect: ShaderEffect {
                name: crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER.to_owned(),
                uniforms: [
                    ("alpha".to_owned(), vec![0.5]),
                    ("colorMultiply".to_owned(), vec![1.0, 1.0, 1.0]),
                    ("opaque".to_owned(), vec![0.0]),
                ]
                .into(),
                mask_texture: None,
                user_texture: None,
            },
            clip_bounds: None,
            mask_range: None,
        });
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();
        let pixels = backend
            .readback_owned(FrameTarget::Main, Extent2D::new(2, 2))
            .unwrap();
        assert!(pixels.chunks_exact(4).all(|pixel| {
            (127..=128).contains(&pixel[0]) && pixel[1] == 0 && pixel[2] == 0 && pixel[3] == 255
        }));
    }

    #[test]
    fn shared_present_keeps_undamaged_pixels() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        let surface = backend
            .create_render_target("present-surface", RenderTargetDesc::sampled_rgba8(2, 2))
            .unwrap();
        backend
            .begin_frame(FrameTarget::Offscreen(surface.id))
            .unwrap();
        backend.clear([0.0, 1.0, 0.0, 1.0]);
        backend.end_frame();

        let red = backend
            .create_texture(
                "red",
                TextureDesc::sampled_rgba8(1, 1),
                TextureData::Rgba8(&[255, 0, 0, 255]),
            )
            .expect("red texture");
        let mut frame = DrawList::new();
        frame.push(DrawCommand {
            texture: red,
            size: TextureInfo {
                width: 1,
                height: 1,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [2.0, 2.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();

        let texture = backend
            .textures
            .get(&surface.color)
            .expect("present surface texture")
            .raw
            .clone();
        let handle = Retained::as_ptr(&texture) as *mut std::ffi::c_void;
        backend
            .set_native_surface(NativeSurface {
                kind: NativeSurfaceKind::AppleMetalTexture,
                handle,
                extent: Extent2D::new(2, 2),
            })
            .unwrap();
        backend.present(Some([0.0, 0.0, 1.0, 1.0])).unwrap();

        let pixels = backend
            .readback_owned(FrameTarget::Offscreen(surface.id), Extent2D::new(2, 2))
            .unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
        assert_eq!(&pixels[8..12], &[0, 255, 0, 255]);
        assert_eq!(&pixels[12..16], &[0, 255, 0, 255]);
    }

    #[test]
    fn metalfx_present_writes_one_complete_upscaled_frame() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        if !backend.metalfx_supported {
            return;
        }

        let surface = backend
            .create_render_target("metalfx-surface", RenderTargetDesc::sampled_rgba8(4, 4))
            .unwrap();
        let texture = backend
            .textures
            .get(&surface.color)
            .expect("present surface texture")
            .raw
            .clone();
        backend
            .set_native_surface(NativeSurface {
                kind: NativeSurfaceKind::AppleMetalTexture,
                handle: Retained::as_ptr(&texture) as *mut std::ffi::c_void,
                extent: Extent2D::new(4, 4),
            })
            .unwrap();

        let mut pipeline = PostProcessPipeline::default();
        pipeline.render_scale = 0.5;
        pipeline.passes[0] = PostProcessPass::Upscale(crate::render_pipeline::UpscaleConfig {
            mode: UpscaleMode::Spatial,
            sharpness: 0.0,
        });
        backend.configure_post_process(pipeline).unwrap();
        assert_eq!(
            backend.render_dimensions(),
            Some(RenderDimensions::new(
                Extent2D::new(2, 2),
                Extent2D::new(4, 4)
            ))
        );

        let red = backend
            .upload_rgba("metalfx-red", 1, 1, &[255, 0, 0, 255])
            .unwrap()
            .0;
        let mut frame = DrawList::new();
        frame.push(DrawCommand {
            texture: red,
            size: TextureInfo {
                width: 1,
                height: 1,
            },
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [2.0, 2.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();

        // A logical partial-damage hint must not expose untouched pixels after
        // MetalFX, because the scaler has produced a complete output image.
        backend.present(Some([0.0, 0.0, 1.0, 1.0])).unwrap();
        let pixels = backend
            .readback_owned(FrameTarget::Offscreen(surface.id), Extent2D::new(4, 4))
            .unwrap();
        assert!(
            pixels.chunks_exact(4).all(|pixel| {
                pixel[0] >= 250 && pixel[1] <= 5 && pixel[2] <= 5 && pixel[3] >= 250
            })
        );
    }
    #[test]
    fn metal_uploads_video_rgba_without_placeholder() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        let name = crate::video::video_layer_texture_name("movie");
        assert!(backend.resolve(&name).is_none());
        assert!(backend.upload_video_rgba(&name, 1, 1, &[0, 255, 0, 255]));
        let (id, info) = backend.resolve(&name).expect("video texture");
        assert_eq!(
            info,
            TextureInfo {
                width: 1,
                height: 1
            }
        );
        let red = backend
            .create_texture(
                "blit-red",
                TextureDesc::sampled_rgba8(1, 1),
                TextureData::Rgba8(&[255, 0, 0, 255]),
            )
            .unwrap();
        let mut frame = DrawList::new();
        frame.push(DrawCommand {
            texture: id,
            size: info,
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [2.0, 2.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        let _ = red;
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.render(&frame);
        backend.end_frame();
        let pixels = backend
            .readback_owned(FrameTarget::Main, Extent2D::new(2, 2))
            .unwrap();
        assert_eq!(&pixels[0..4], &[0, 255, 0, 255]);
        assert_eq!(
            backend.video_import_capability().preferred,
            ExternalImageKind::CvPixelBuffer
        );
        assert!(backend.backend_info().capabilities.zero_copy_video);
        assert!(backend.backend_info().capabilities.external_texture);
    }

    #[test]
    fn metal_imports_mtltexture_zero_copy_and_tracks_consumption() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        let source = backend
            .create_texture(
                "import-src",
                TextureDesc::sampled_rgba8(2, 2),
                TextureData::Rgba8(&[
                    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
                ]),
            )
            .unwrap();
        let raw = backend.textures.get(&source).unwrap().raw.clone();
        let name = crate::video::video_layer_texture_name("layer");
        let handle = backend
            .import_external_texture(
                &name,
                ExternalImage {
                    kind: ExternalImageKind::MetalTexture,
                    handle: objc2::rc::Retained::as_ptr(&raw) as *mut _,
                    extent: Extent2D::new(2, 2),
                    format: TextureFormat::Rgba8Unorm,
                    ownership: ResourceOwnership::Borrowed,
                    wait: GpuSyncToken::NONE,
                    rgba: None,
                },
            )
            .expect("import MTLTexture");
        let (id, info) = backend.resolve(&name).expect("imported video");
        assert_eq!(info.width, 2);
        assert_eq!(id, *backend.video_leases.get(&handle.opaque()).unwrap());
        assert!(!backend.video_surface_consumed(VideoSurfaceHandle::from_opaque(handle.opaque())));

        backend.begin_frame(FrameTarget::Main).unwrap();
        let mut frame = DrawList::new();
        frame.push(DrawCommand {
            texture: id,
            size: info,
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect {
                uv_offset: [0.0, 0.0],
                uv_scale: [1.0, 1.0],
                quad_size: [2.0, 2.0],
            },
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        backend.render(&frame);
        backend.end_frame();
        let pixels = backend
            .readback_owned(FrameTarget::Main, Extent2D::new(2, 2))
            .unwrap();
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 0, 0, 255])
        );

        assert!(backend.release_external_texture(handle));
        backend.collect_retired_resources();
        assert!(backend.video_surface_consumed(VideoSurfaceHandle::from_opaque(handle.opaque())));
        assert!(backend.resolve(&name).is_none());
    }

    #[test]
    fn metal_screenshot_does_not_use_video_surface() {
        let Ok(mut backend) = MetalBackend::new(2, 2) else {
            return;
        };
        backend.begin_frame(FrameTarget::Main).unwrap();
        backend.clear([0.0, 0.0, 1.0, 1.0]);
        backend.end_frame();
        let mut pixels = [0u8; 16];
        let written = backend
            .capture_screenshot(Extent2D::new(2, 2), &mut pixels)
            .unwrap();
        assert_eq!(written, 16);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 0, 255, 255])
        );
        assert!(
            backend
                .video_surface_gl_framebuffer(VideoSurfaceHandle::from_opaque(1))
                .is_none()
        );
    }
}
