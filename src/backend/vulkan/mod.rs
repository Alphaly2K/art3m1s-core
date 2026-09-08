//! Native Vulkan backend. All Vulkan handles and synchronization stay private.

use crate::backend::*;
use crate::render_pipeline::draw::*;
use ash::{Device, Entry, Instance, vk};
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::io::Cursor;
use std::time::Instant;

const FRAMES_IN_FLIGHT: usize = 2;
const MAX_DRAW_SETS: u32 = 8192;
const SCRATCH_INITIAL: usize = 2 * 1024 * 1024;

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
#[repr(C)]
#[derive(Clone, Copy)]
struct UniformBlock {
    vertex: VertexUniforms,
    sprite: SpriteUniforms,
    effect: EffectUniforms,
}

struct Buffer {
    raw: vk::Buffer,
    memory: vk::DeviceMemory,
    size: vk::DeviceSize,
}
struct ScratchBuffer {
    buffer: Buffer,
    mapped: *mut u8,
    cursor: usize,
}
impl ScratchBuffer {
    fn alloc(&mut self, size: usize, align: usize) -> Option<u64> {
        let align = align.max(1);
        let start = self.cursor.div_ceil(align) * align;
        let end = start.checked_add(size)?;
        if end > self.buffer.size as usize {
            return None;
        }
        self.cursor = end;
        Some(start as u64)
    }
    fn write(&mut self, offset: u64, data: &[u8]) {
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.mapped.add(offset as usize),
                data.len(),
            );
        }
    }
}
struct Image {
    raw: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    desc: TextureDesc,
    layout: vk::ImageLayout,
}
enum PixelStorage {
    None,
    Opaque,
    Alpha(Vec<u8>),
    Rgba(Vec<u8>),
}
struct VulkanTexture {
    image: Image,
    info: TextureInfo,
    cpu: PixelStorage,
    opaque: bool,
}
struct VulkanRenderTarget {
    id: RenderTargetId,
    color: TextureId,
    desc: RenderTargetDesc,
    framebuffer: vk::Framebuffer,
    pass: vk::RenderPass,
}
struct PrivateTarget {
    image: Image,
    framebuffer: vk::Framebuffer,
    pass: vk::RenderPass,
}

#[derive(Clone, Copy)]
struct TargetView {
    image: vk::Image,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    extent: Extent2D,
    format: vk::Format,
    pass: vk::RenderPass,
}

struct ActiveFrame {
    target: FrameTarget,
    view: TargetView,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    descriptors: vk::DescriptorPool,
    scratch: ScratchBuffer,
    pass_open: bool,
    pass_framebuffer: vk::Framebuffer,
    resources: Vec<Retired>,
}
struct Submission {
    serial: u64,
    fence: vk::Fence,
    pool: vk::CommandPool,
    descriptors: Option<vk::DescriptorPool>,
    scratch: Option<ScratchBuffer>,
    resources: Vec<Retired>,
}
enum Retired {
    Texture(VulkanTexture),
    Buffer(Buffer),
    Framebuffer(vk::Framebuffer),
}
struct PendingRetirement {
    after: u64,
    resource: Retired,
}

struct PresentFrame {
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    available: vk::Semaphore,
    finished: vk::Semaphore,
    fence: vk::Fence,
    serial: u64,
}
struct Swapchain {
    surface: vk::SurfaceKHR,
    raw: vk::SwapchainKHR,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    layouts: Vec<vk::ImageLayout>,
    frames: Vec<PresentFrame>,
    next_frame: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ShaderKind {
    Sprite,
    AlphaMask,
    GroupComposite,
    RuleTransition,
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
enum VertexLayout {
    IndexedQuad,
    Mesh,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    shader: ShaderKind,
    format: vk::Format,
    blend: PipelineBlend,
    stencil: StencilMode,
    vertex: VertexLayout,
}
struct ShaderModules {
    vertex: vk::ShaderModule,
    sprite: vk::ShaderModule,
    alpha_mask: vk::ShaderModule,
    group: vk::ShaderModule,
    rule: vk::ShaderModule,
}

pub struct VulkanBackend {
    _entry: Entry,
    instance: Instance,
    physical: vk::PhysicalDevice,
    device: Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    queue_family: u32,
    queue: vk::Queue,
    surface_loader: ash::khr::surface::Instance,
    swapchain_loader: ash::khr::swapchain::Device,
    descriptor_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    sampler: vk::Sampler,
    shaders: ShaderModules,
    render_passes: HashMap<vk::Format, vk::RenderPass>,
    pipelines: HashMap<PipelineKey, vk::Pipeline>,
    quad_vertex: Buffer,
    quad_index: Buffer,
    white: Image,
    transparent: Image,
    main: PrivateTarget,
    active: Option<ActiveFrame>,
    textures: HashMap<TextureId, VulkanTexture>,
    names: HashMap<String, TextureId>,
    render_targets: HashMap<RenderTargetId, VulkanRenderTarget>,
    groups: Vec<PrivateTarget>,
    masks: Vec<PrivateTarget>,
    swapchain: Option<Swapchain>,
    source: Option<Box<AssetSource>>,
    next_texture: u64,
    next_target: u64,
    revision: u64,
    texture_revisions: HashMap<TextureId, u64>,
    submitted: u64,
    completed: u64,
    submissions: VecDeque<Submission>,
    retired: VecDeque<PendingRetirement>,
    fatal_error: Option<String>,
    last_overlay: Option<RenderRegion>,
    flash: usize,
    profiling: Cell<bool>,
    upload_ns: Cell<u64>,
    uploaded: Cell<u64>,
    draws: Cell<u64>,
    vertices: Cell<u64>,
    binds: Cell<u64>,
    mesh_bytes: Cell<u64>,
    uniform_align: usize,
    scratch_capacity: usize,
    scratch_free: Vec<ScratchBuffer>,
}

impl VulkanBackend {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("Vulkan stage extent must be non-zero".into());
        }
        let entry = unsafe { Entry::load() }.map_err(|e| format!("load Vulkan loader: {e}"))?;
        let app = CString::new("art3m1s-core").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app)
            .application_version(1)
            .engine_name(&app)
            .engine_version(1)
            .api_version(vk::API_VERSION_1_1);
        let mut extensions = vec![ash::khr::surface::NAME.as_ptr()];
        #[cfg(target_os = "android")]
        extensions.push(ash::khr::android_surface::NAME.as_ptr());
        let create = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&extensions);
        let instance = unsafe { entry.create_instance(&create, None) }
            .map_err(|e| format!("create Vulkan instance: {e}"))?;
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let (physical, queue_family) = select_physical_device(&instance)?;
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let uniform_align = unsafe { instance.get_physical_device_properties(physical) }
            .limits
            .min_uniform_buffer_offset_alignment
            .max(1) as usize;
        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions);
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|e| format!("create Vulkan device: {e}"))?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);

        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_count(1)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
            sampled_binding(1),
            sampled_binding(2),
            sampled_binding(3),
            vk::DescriptorSetLayoutBinding::default()
                .binding(4)
                .descriptor_count(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        let descriptor_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )
        }
        .map_err(|e| format!("create Vulkan descriptor layout: {e}"))?;
        let set_layouts = [descriptor_layout];
        let pipeline_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts),
                None,
            )
        }
        .map_err(|e| format!("create Vulkan pipeline layout: {e}"))?;
        let sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
        }
        .map_err(|e| format!("create Vulkan sampler: {e}"))?;
        let shaders = create_shaders(&device)?;
        let render_pass = create_render_pass(&device, vk::Format::R8G8B8A8_UNORM)?;
        let mut render_passes = HashMap::new();
        render_passes.insert(vk::Format::R8G8B8A8_UNORM, render_pass);
        let quad = [
            Vertex {
                position: [0., 0.],
                uv: [0., 0.],
            },
            Vertex {
                position: [1., 0.],
                uv: [1., 0.],
            },
            Vertex {
                position: [1., 1.],
                uv: [1., 1.],
            },
            Vertex {
                position: [0., 1.],
                uv: [0., 1.],
            },
        ];
        let indices = [0u16, 1, 2, 0, 2, 3];
        let quad_vertex = upload_device_buffer(
            &device,
            &memory,
            queue,
            queue_family,
            bytes(&quad),
            vk::BufferUsageFlags::VERTEX_BUFFER,
        )?;
        let quad_index = upload_device_buffer(
            &device,
            &memory,
            queue,
            queue_family,
            bytes(&indices),
            vk::BufferUsageFlags::INDEX_BUFFER,
        )?;
        let white = create_uploaded_image(
            &device,
            &memory,
            queue,
            queue_family,
            TextureDesc::sampled_rgba8(1, 1),
            Some(&[255, 255, 255, 255]),
        )?;
        let transparent = create_uploaded_image(
            &device,
            &memory,
            queue,
            queue_family,
            TextureDesc::sampled_rgba8(1, 1),
            Some(&[0, 0, 0, 0]),
        )?;
        let main = create_private_target(
            &device,
            &memory,
            render_pass,
            Extent2D::new(width, height),
            TextureFormat::Rgba8Unorm,
        )?;
        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            memory,
            queue_family,
            queue,
            surface_loader,
            swapchain_loader,
            descriptor_layout,
            pipeline_layout,
            sampler,
            shaders,
            render_passes,
            pipelines: HashMap::new(),
            quad_vertex,
            quad_index,
            white,
            transparent,
            main,
            active: None,
            textures: HashMap::new(),
            names: HashMap::new(),
            render_targets: HashMap::new(),
            groups: vec![],
            masks: vec![],
            swapchain: None,
            source: None,
            next_texture: 1,
            next_target: 1,
            revision: 0,
            texture_revisions: HashMap::new(),
            submitted: 0,
            completed: 0,
            submissions: VecDeque::new(),
            retired: VecDeque::new(),
            fatal_error: None,
            last_overlay: None,
            flash: 0,
            profiling: Cell::new(false),
            upload_ns: Cell::new(0),
            uploaded: Cell::new(0),
            draws: Cell::new(0),
            vertices: Cell::new(0),
            binds: Cell::new(0),
            mesh_bytes: Cell::new(0),
            uniform_align,
            scratch_capacity: SCRATCH_INITIAL,
            scratch_free: Vec::new(),
        })
    }

    fn render_pass(&mut self, format: vk::Format) -> Result<vk::RenderPass, String> {
        if let Some(&pass) = self.render_passes.get(&format) {
            return Ok(pass);
        }
        let pass = create_render_pass(&self.device, format)?;
        self.render_passes.insert(format, pass);
        Ok(pass)
    }
    fn target_view(&self, target: FrameTarget) -> Result<TargetView, String> {
        match target {
            FrameTarget::Main => Ok(private_view(&self.main)),
            FrameTarget::Offscreen(id) => {
                let rt = self
                    .render_targets
                    .get(&id)
                    .ok_or("unknown Vulkan render target")?;
                let tex = self
                    .textures
                    .get(&rt.color)
                    .ok_or("Vulkan target texture missing")?;
                Ok(TargetView {
                    image: tex.image.raw,
                    view: tex.image.view,
                    framebuffer: rt.framebuffer,
                    extent: rt.desc.extent,
                    format: vk_format(rt.desc.color_format),
                    pass: rt.pass,
                })
            }
        }
    }
    fn target_layout(&self, target: FrameTarget) -> Result<vk::ImageLayout, String> {
        match target {
            FrameTarget::Main => Ok(self.main.image.layout),
            FrameTarget::Offscreen(id) => {
                let rt = self
                    .render_targets
                    .get(&id)
                    .ok_or("unknown Vulkan render target")?;
                Ok(self
                    .textures
                    .get(&rt.color)
                    .ok_or("target texture missing")?
                    .image
                    .layout)
            }
        }
    }
    fn set_target_layout(&mut self, target: FrameTarget, layout: vk::ImageLayout) {
        match target {
            FrameTarget::Main => self.main.image.layout = layout,
            FrameTarget::Offscreen(id) => {
                if let Some(rt) = self.render_targets.get(&id)
                    && let Some(t) = self.textures.get_mut(&rt.color)
                {
                    t.image.layout = layout;
                }
            }
        }
    }
    fn retirement_serial(&self) -> u64 {
        self.submitted + u64::from(self.active.is_some())
    }
    fn check_operational(&self) -> Result<(), String> {
        self.fatal_error
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    fn fail(&mut self, context: &str, error: impl std::fmt::Display) {
        let error = format!("Vulkan backend is no longer operational ({context}): {error}");
        crate::core_error!("[VulkanBackend] {error}");
        self.fatal_error.get_or_insert(error);
    }
    fn retire(&mut self, resource: Retired) {
        self.retired.push_back(PendingRetirement {
            after: self.retirement_serial(),
            resource,
        });
    }
    fn allocate_texture_id(&mut self) -> TextureId {
        let id = TextureId(self.next_texture);
        self.next_texture = self.next_texture.wrapping_add(1).max(1);
        id
    }
    fn mark_changed(&mut self, id: TextureId) {
        self.revision = self.revision.wrapping_add(1);
        self.texture_revisions.insert(id, self.revision);
    }
}

fn sampled_binding(binding: u32) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(binding)
        .descriptor_count(1)
        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
}

fn select_physical_device(instance: &Instance) -> Result<(vk::PhysicalDevice, u32), String> {
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|e| format!("enumerate Vulkan devices: {e}"))?;
    let mut choices = Vec::new();
    for physical in devices {
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|e| format!("enumerate Vulkan device extensions: {e}"))?;
        if !extensions.iter().any(|extension| {
            (unsafe { CStr::from_ptr(extension.extension_name.as_ptr()) })
                == ash::khr::swapchain::NAME
        }) {
            continue;
        }
        let props = unsafe { instance.get_physical_device_properties(physical) };
        for (index, q) in unsafe { instance.get_physical_device_queue_family_properties(physical) }
            .iter()
            .enumerate()
        {
            if q.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                let score = match props.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 3,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
                    _ => 1,
                };
                choices.push((score, physical, index as u32));
                break;
            }
        }
    }
    choices
        .into_iter()
        .max_by_key(|x| x.0)
        .map(|(_, p, q)| (p, q))
        .ok_or_else(|| "no Vulkan graphics device".into())
}

fn create_shaders(device: &Device) -> Result<ShaderModules, String> {
    fn module(device: &Device, data: &[u8]) -> Result<vk::ShaderModule, String> {
        if !data.len().is_multiple_of(4) {
            return Err("invalid SPIR-V byte length".into());
        }
        let words =
            ash::util::read_spv(&mut Cursor::new(data)).map_err(|e| format!("read SPIR-V: {e}"))?;
        unsafe {
            device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|e| format!("create Vulkan shader module: {e}"))
    }
    Ok(ShaderModules {
        vertex: module(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/vulkan_sprite.vert.spv")),
        )?,
        sprite: module(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/vulkan_sprite.frag.spv")),
        )?,
        alpha_mask: module(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/vulkan_alpha_mask.frag.spv")),
        )?,
        group: module(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/vulkan_group.frag.spv")),
        )?,
        rule: module(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/vulkan_rule.frag.spv")),
        )?,
    })
}

fn create_render_pass(device: &Device, format: vk::Format) -> Result<vk::RenderPass, String> {
    let attachments = [vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::LOAD)
        .store_op(vk::AttachmentStoreOp::STORE)
        .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let color = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let subpasses = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color)];
    unsafe {
        device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(&attachments)
                .subpasses(&subpasses),
            None,
        )
    }
    .map_err(|e| format!("create Vulkan render pass: {e}"))
}

fn vk_format(f: TextureFormat) -> vk::Format {
    match f {
        TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        TextureFormat::Bc3RgbaUnorm => vk::Format::BC3_UNORM_BLOCK,
        TextureFormat::Astc4x4RgbaUnorm => vk::Format::ASTC_4X4_UNORM_BLOCK,
    }
}
fn image_usage(u: TextureUsage) -> vk::ImageUsageFlags {
    let mut r = vk::ImageUsageFlags::empty();
    if u.contains(TextureUsage::SAMPLED) {
        r |= vk::ImageUsageFlags::SAMPLED
    }
    if u.contains(TextureUsage::RENDER_TARGET) {
        r |= vk::ImageUsageFlags::COLOR_ATTACHMENT
    }
    if u.contains(TextureUsage::TRANSFER_SRC) {
        r |= vk::ImageUsageFlags::TRANSFER_SRC
    }
    if u.contains(TextureUsage::TRANSFER_DST) {
        r |= vk::ImageUsageFlags::TRANSFER_DST
    }
    r
}
fn memory_type(
    memory: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    flags: vk::MemoryPropertyFlags,
) -> Result<u32, String> {
    (0..memory.memory_type_count)
        .find(|&i| {
            bits & (1 << i) != 0
                && memory.memory_types[i as usize]
                    .property_flags
                    .contains(flags)
        })
        .ok_or_else(|| format!("no Vulkan memory type for {flags:?}"))
}
fn create_buffer(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    size: usize,
    usage: vk::BufferUsageFlags,
    flags: vk::MemoryPropertyFlags,
) -> Result<Buffer, String> {
    let raw = unsafe {
        device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size as u64)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )
    }
    .map_err(|e| format!("create Vulkan buffer: {e}"))?;
    let req = unsafe { device.get_buffer_memory_requirements(raw) };
    let ty = memory_type(memory, req.memory_type_bits, flags)?;
    let allocation = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(ty),
            None,
        )
    }
    .map_err(|e| format!("allocate Vulkan buffer: {e}"))?;
    unsafe { device.bind_buffer_memory(raw, allocation, 0) }
        .map_err(|e| format!("bind Vulkan buffer: {e}"))?;
    Ok(Buffer {
        raw,
        memory: allocation,
        size: size as u64,
    })
}
fn write_buffer(device: &Device, buffer: &Buffer, data: &[u8]) -> Result<(), String> {
    let p = unsafe {
        device.map_memory(
            buffer.memory,
            0,
            data.len() as u64,
            vk::MemoryMapFlags::empty(),
        )
    }
    .map_err(|e| format!("map Vulkan buffer: {e}"))?;
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast(), data.len());
        device.unmap_memory(buffer.memory)
    };
    Ok(())
}
fn destroy_buffer(device: &Device, b: Buffer) {
    unsafe {
        device.destroy_buffer(b.raw, None);
        device.free_memory(b.memory, None)
    }
}
fn bytes<T>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}
fn value_bytes<T>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((v as *const T).cast(), std::mem::size_of::<T>()) }
}

fn immediate<F: FnOnce(vk::CommandBuffer)>(
    device: &Device,
    queue: vk::Queue,
    family: u32,
    record: F,
) -> Result<(), String> {
    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(family)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT),
            None,
        )
    }
    .map_err(|e| format!("create upload pool: {e}"))?;
    let cmd = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
    }
    .map_err(|e| format!("allocate upload command: {e}"))?[0];
    unsafe {
        device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
    }
    .map_err(|e| format!("begin upload command: {e}"))?;
    record(cmd);
    unsafe { device.end_command_buffer(cmd) }.map_err(|e| format!("end upload command: {e}"))?;
    let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
        .map_err(|e| format!("create upload fence: {e}"))?;
    let cmds = [cmd];
    let submits = [vk::SubmitInfo::default().command_buffers(&cmds)];
    unsafe { device.queue_submit(queue, &submits, fence) }
        .map_err(|e| format!("submit upload: {e}"))?;
    unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
        .map_err(|e| format!("wait upload: {e}"))?;
    unsafe {
        device.destroy_fence(fence, None);
        device.destroy_command_pool(pool, None)
    };
    Ok(())
}

fn upload_device_buffer(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    family: u32,
    data: &[u8],
    usage: vk::BufferUsageFlags,
) -> Result<Buffer, String> {
    let staging = create_buffer(
        device,
        memory,
        data.len(),
        vk::BufferUsageFlags::TRANSFER_SRC,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    )?;
    write_buffer(device, &staging, data)?;
    let result = create_buffer(
        device,
        memory,
        data.len(),
        usage | vk::BufferUsageFlags::TRANSFER_DST,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )?;
    immediate(device, queue, family, |cmd| unsafe {
        device.cmd_copy_buffer(
            cmd,
            staging.raw,
            result.raw,
            &[vk::BufferCopy::default().size(data.len() as u64)],
        )
    })?;
    destroy_buffer(device, staging);
    Ok(result)
}

fn create_image(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    desc: TextureDesc,
) -> Result<Image, String> {
    let raw = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk_format(desc.format))
                .extent(vk::Extent3D {
                    width: desc.extent.width,
                    height: desc.extent.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(image_usage(desc.usage))
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )
    }
    .map_err(|e| format!("create Vulkan image: {e}"))?;
    let req = unsafe { device.get_image_memory_requirements(raw) };
    let ty = memory_type(
        memory,
        req.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )?;
    let allocation = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(ty),
            None,
        )
    }
    .map_err(|e| format!("allocate Vulkan image: {e}"))?;
    unsafe { device.bind_image_memory(raw, allocation, 0) }
        .map_err(|e| format!("bind Vulkan image: {e}"))?;
    let view = unsafe {
        device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(raw)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk_format(desc.format))
                .subresource_range(color_range()),
            None,
        )
    }
    .map_err(|e| format!("create Vulkan image view: {e}"))?;
    Ok(Image {
        raw,
        memory: allocation,
        view,
        desc,
        layout: vk::ImageLayout::UNDEFINED,
    })
}
fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}
fn barrier(
    device: &Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) {
    let (src, dst, sa, da) = barrier_masks(old, new);
    let barriers = [vk::ImageMemoryBarrier::default()
        .old_layout(old)
        .new_layout(new)
        .image(image)
        .subresource_range(color_range())
        .src_access_mask(sa)
        .dst_access_mask(da)];
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            src,
            dst,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barriers,
        )
    }
}
fn barrier_masks(
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) -> (
    vk::PipelineStageFlags,
    vk::PipelineStageFlags,
    vk::AccessFlags,
    vk::AccessFlags,
) {
    let src = match old {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        ),
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => (
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_WRITE,
        ),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => (
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_READ,
        ),
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::AccessFlags::SHADER_READ,
        ),
        vk::ImageLayout::PRESENT_SRC_KHR => (
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::AccessFlags::empty(),
        ),
        _ => (
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
        ),
    };
    let dst = match new {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        ),
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => (
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_WRITE,
        ),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => (
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::TRANSFER_READ,
        ),
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::AccessFlags::SHADER_READ,
        ),
        vk::ImageLayout::PRESENT_SRC_KHR => (
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::AccessFlags::empty(),
        ),
        _ => (
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::AccessFlags::empty(),
        ),
    };
    (src.0, dst.0, src.1, dst.1)
}
fn create_uploaded_image(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    family: u32,
    desc: TextureDesc,
    data: Option<&[u8]>,
) -> Result<Image, String> {
    let mut image = create_image(device, memory, desc)?;
    if let Some(data) = data {
        let staging = create_buffer(
            device,
            memory,
            data.len(),
            vk::BufferUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        write_buffer(device, &staging, data)?;
        immediate(device, queue, family, |cmd| {
            barrier(
                device,
                cmd,
                image.raw,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            let regions = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: desc.extent.width,
                    height: desc.extent.height,
                    depth: 1,
                })];
            unsafe {
                device.cmd_copy_buffer_to_image(
                    cmd,
                    staging.raw,
                    image.raw,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &regions,
                )
            };
            barrier(
                device,
                cmd,
                image.raw,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            )
        })?;
        destroy_buffer(device, staging);
        image.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    }
    Ok(image)
}
fn create_private_target(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    pass: vk::RenderPass,
    extent: Extent2D,
    format: TextureFormat,
) -> Result<PrivateTarget, String> {
    let desc = TextureDesc {
        extent,
        format,
        usage: TextureUsage::RENDER_TARGET
            | TextureUsage::SAMPLED
            | TextureUsage::TRANSFER_SRC
            | TextureUsage::TRANSFER_DST,
    };
    let image = create_image(device, memory, desc)?;
    let attachments = [image.view];
    let framebuffer = unsafe {
        device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1),
            None,
        )
    }
    .map_err(|e| format!("create Vulkan framebuffer: {e}"))?;
    Ok(PrivateTarget {
        image,
        framebuffer,
        pass,
    })
}
fn private_view(t: &PrivateTarget) -> TargetView {
    TargetView {
        image: t.image.raw,
        view: t.image.view,
        framebuffer: t.framebuffer,
        extent: t.image.desc.extent,
        format: vk_format(t.image.desc.format),
        pass: t.pass,
    }
}

impl PixelStorage {
    fn alpha(rgba: &[u8]) -> Self {
        let a = rgba.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>();
        if a.iter().all(|&x| x == 255) {
            Self::Opaque
        } else {
            Self::Alpha(a)
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::Alpha(x) => x.len(),
            Self::Rgba(x) => x.len(),
            _ => 0,
        }
    }
}
fn rgba_opaque(x: &[u8]) -> bool {
    x.chunks_exact(4).all(|p| p[3] == 255)
}
fn data_bytes<'a>(x: &'a TextureData<'a>) -> Option<&'a [u8]> {
    match x {
        TextureData::Uninitialized => None,
        TextureData::Rgba8(x) | TextureData::Bc3(x) | TextureData::Astc4x4(x) => Some(x),
    }
}
fn validate_data(name: &str, desc: TextureDesc, data: &TextureData<'_>) -> Result<(), String> {
    if desc.extent.is_empty() {
        return Err(format!("texture {name} has empty extent"));
    }
    let expected = match (desc.format, data) {
        (TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm, TextureData::Rgba8(_)) => {
            desc.extent.rgba8_len()
        }
        (TextureFormat::Bc3RgbaUnorm, TextureData::Bc3(_))
        | (TextureFormat::Astc4x4RgbaUnorm, TextureData::Astc4x4(_)) => desc.extent.block_4x4_len(),
        (_, TextureData::Uninitialized) => return Ok(()),
        _ => return Err(format!("texture {name} payload format mismatch")),
    }
    .ok_or("texture size overflow")?;
    if data_bytes(data).map_or(0, |x| x.len()) != expected {
        return Err(format!("texture {name} payload size mismatch"));
    }
    Ok(())
}
fn decode_rgba(x: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let i = image::load_from_memory(x).ok()?.to_rgba8();
    let (w, h) = i.dimensions();
    Some((w, h, i.into_raw()))
}

impl VulkanBackend {
    fn insert_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
        cpu_readable: bool,
    ) -> Result<TextureId, String> {
        validate_data(name, desc, &data)?;
        if let Some(old) = self.names.get(name).copied() {
            self.remove_texture(old);
        }
        let started = self.profiling.get().then(Instant::now);
        let payload = data_bytes(&data);
        let image = create_uploaded_image(
            &self.device,
            &self.memory,
            self.queue,
            self.queue_family,
            desc,
            payload,
        )?;
        let cpu = match data {
            TextureData::Rgba8(x) if cpu_readable => PixelStorage::Rgba(x.to_vec()),
            TextureData::Rgba8(x) => PixelStorage::alpha(x),
            _ => PixelStorage::None,
        };
        let opaque = matches!(data,TextureData::Rgba8(x)if rgba_opaque(x));
        let id = self.allocate_texture_id();
        self.names.insert(name.into(), id);
        self.textures.insert(
            id,
            VulkanTexture {
                image,
                info: TextureInfo {
                    width: desc.extent.width,
                    height: desc.extent.height,
                },
                cpu,
                opaque,
            },
        );
        self.mark_changed(id);
        if let Some(s) = started {
            self.upload_ns.set(
                self.upload_ns
                    .get()
                    .saturating_add(s.elapsed().as_nanos().min(u64::MAX as u128) as u64),
            );
            self.uploaded.set(
                self.uploaded
                    .get()
                    .saturating_add(payload.map_or(0, |x| x.len() as u64)),
            )
        }
        Ok(id)
    }
    fn remove_texture(&mut self, id: TextureId) -> bool {
        self.names.retain(|_, v| *v != id);
        self.texture_revisions.remove(&id);
        if let Some(t) = self.textures.remove(&id) {
            self.retire(Retired::Texture(t));
            self.revision = self.revision.wrapping_add(1);
            true
        } else {
            false
        }
    }
    fn wait_draw_submissions(&mut self) -> Result<(), String> {
        let fences = self.submissions.iter().map(|x| x.fence).collect::<Vec<_>>();
        if !fences.is_empty() {
            unsafe { self.device.wait_for_fences(&fences, true, u64::MAX) }
                .map_err(|e| format!("wait Vulkan draw submissions: {e}"))?;
        }
        self.collect_retired_resources();
        Ok(())
    }
    fn wait_submissions(&mut self) -> Result<(), String> {
        self.wait_draw_submissions()?;
        let Some((fences, serials)) = self.swapchain.as_ref().map(|s| {
            (
                s.frames
                    .iter()
                    .filter(|frame| frame.serial != 0)
                    .map(|frame| frame.fence)
                    .collect::<Vec<_>>(),
                s.frames
                    .iter()
                    .map(|frame| frame.serial)
                    .collect::<Vec<_>>(),
            )
        }) else {
            return Ok(());
        };
        if !fences.is_empty() {
            unsafe { self.device.wait_for_fences(&fences, true, u64::MAX) }
                .map_err(|e| format!("wait Vulkan submissions: {e}"))?;
        }
        for serial in serials {
            self.completed = self.completed.max(serial);
        }
        self.collect_retired_resources();
        Ok(())
    }
    fn update_image(&mut self, id: TextureId, update: TextureUpdate<'_>) -> Result<(), String> {
        self.wait_draw_submissions()?;
        let t = self.textures.get(&id).ok_or("unknown Vulkan texture")?;
        let end = [
            update.origin[0]
                .checked_add(update.extent.width)
                .ok_or("update overflow")?,
            update.origin[1]
                .checked_add(update.extent.height)
                .ok_or("update overflow")?,
        ];
        if update.extent.is_empty()
            || end[0] > t.image.desc.extent.width
            || end[1] > t.image.desc.extent.height
        {
            return Err("texture update outside extent".into());
        }
        if matches!(
            t.image.desc.format,
            TextureFormat::Bc3RgbaUnorm | TextureFormat::Astc4x4RgbaUnorm
        ) && (update.origin[0] % 4 != 0
            || update.origin[1] % 4 != 0
            || (update.extent.width % 4 != 0 && end[0] != t.image.desc.extent.width)
            || (update.extent.height % 4 != 0 && end[1] != t.image.desc.extent.height))
        {
            return Err("compressed Vulkan texture update is not block aligned".into());
        }
        validate_data(
            "update",
            TextureDesc {
                extent: update.extent,
                format: t.image.desc.format,
                usage: t.image.desc.usage,
            },
            &update.data,
        )?;
        let payload = data_bytes(&update.data).ok_or("texture update missing payload")?;
        let staging = create_buffer(
            &self.device,
            &self.memory,
            payload.len(),
            vk::BufferUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        write_buffer(&self.device, &staging, payload)?;
        let image = t.image.raw;
        let old = t.image.layout;
        let device = &self.device;
        immediate(device, self.queue, self.queue_family, |cmd| {
            barrier(
                device,
                cmd,
                image,
                old,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D {
                    x: update.origin[0] as i32,
                    y: update.origin[1] as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: update.extent.width,
                    height: update.extent.height,
                    depth: 1,
                })];
            unsafe {
                device.cmd_copy_buffer_to_image(
                    cmd,
                    staging.raw,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &region,
                )
            };
            barrier(
                device,
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            )
        })?;
        destroy_buffer(&self.device, staging);
        let t = self.textures.get_mut(&id).unwrap();
        t.image.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        update_cpu(t, update.origin, update.extent, &update.data);
        t.opaque = if update.origin == [0, 0] && update.extent == t.image.desc.extent {
            matches!(update.data,TextureData::Rgba8(x)if rgba_opaque(x))
        } else {
            t.opaque && matches!(update.data,TextureData::Rgba8(x)if rgba_opaque(x))
        };
        self.mark_changed(id);
        Ok(())
    }
}

impl TextureProvider for VulkanBackend {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        if let Some(&id) = self.names.get(name) {
            return self.textures.get(&id).map(|t| (id, t.info));
        }
        if let Some(src) = &self.source
            && let Some(x) = src(name)
            && let Some((w, h, p)) = decode_rgba(&x)
        {
            return self.upload_rgba_render_only(name, w, h, &p);
        }
        let n = 256;
        let mut p = vec![0; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                let o = (y * n + x) * 4;
                p[o..o + 4].copy_from_slice(if ((x / 32) + (y / 32)) % 2 == 0 {
                    &[255, 0, 255, 255]
                } else {
                    &[24, 24, 24, 255]
                })
            }
        }
        self.upload_rgba_render_only(name, n as u32, n as u32, &p)
    }
    fn upload_rgba(
        &mut self,
        n: &str,
        w: u32,
        h: u32,
        x: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.insert_texture(
            n,
            TextureDesc::sampled_rgba8(w, h),
            TextureData::Rgba8(x),
            true,
        )
        .ok()
        .map(|id| {
            (
                id,
                TextureInfo {
                    width: w,
                    height: h,
                },
            )
        })
    }
    fn upload_rgba_render_only(
        &mut self,
        n: &str,
        w: u32,
        h: u32,
        x: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.insert_texture(
            n,
            TextureDesc::sampled_rgba8(w, h),
            TextureData::Rgba8(x),
            false,
        )
        .ok()
        .map(|id| {
            (
                id,
                TextureInfo {
                    width: w,
                    height: h,
                },
            )
        })
    }
    fn upload_dxt5_render_only(
        &mut self,
        n: &str,
        w: u32,
        h: u32,
        x: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let d = TextureDesc {
            extent: Extent2D::new(w, h),
            format: TextureFormat::Bc3RgbaUnorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        self.insert_texture(n, d, TextureData::Bc3(x), false)
            .ok()
            .map(|id| {
                (
                    id,
                    TextureInfo {
                        width: w,
                        height: h,
                    },
                )
            })
    }
    fn supports_astc_4x4(&self) -> bool {
        let p = unsafe {
            self.instance.get_physical_device_format_properties(
                self.physical,
                vk::Format::ASTC_4X4_UNORM_BLOCK,
            )
        };
        p.optimal_tiling_features
            .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
    }
    fn upload_astc_4x4_render_only(
        &mut self,
        n: &str,
        w: u32,
        h: u32,
        x: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        if !self.supports_astc_4x4() {
            return None;
        }
        let d = TextureDesc {
            extent: Extent2D::new(w, h),
            format: TextureFormat::Astc4x4RgbaUnorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        self.insert_texture(n, d, TextureData::Astc4x4(x), false)
            .ok()
            .map(|id| {
                (
                    id,
                    TextureInfo {
                        width: w,
                        height: h,
                    },
                )
            })
    }
    fn pixel_alpha(&self, id: TextureId, x: u32, y: u32) -> Option<u8> {
        let t = self.textures.get(&id)?;
        if x >= t.info.width || y >= t.info.height {
            return None;
        }
        let i = (y * t.info.width + x) as usize;
        match &t.cpu {
            PixelStorage::Opaque => Some(255),
            PixelStorage::Alpha(a) => a.get(i).copied(),
            PixelStorage::Rgba(p) => p.get(i * 4 + 3).copied(),
            _ => None,
        }
    }
    fn texture_is_opaque(&self, id: TextureId) -> bool {
        self.textures.get(&id).is_some_and(|t| t.opaque)
    }
    fn retain(&mut self, names: &HashSet<String>) {
        let stale = self
            .names
            .iter()
            .filter_map(|(n, &id)| (!names.contains(n)).then_some(id))
            .collect::<HashSet<_>>();
        for id in stale {
            self.remove_texture(id);
        }
    }
    fn solid_texture(&mut self, c: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        let n = solid_texture_name(c);
        if let Some(&id) = self.names.get(&n) {
            return Some((
                id,
                TextureInfo {
                    width: 1,
                    height: 1,
                },
            ));
        }
        self.upload_rgba(&n, 1, 1, &c)
    }
    fn resolve_with_mask(&mut self, file: &str, mask: &str) -> Option<(TextureId, TextureInfo)> {
        let n = masked_texture_name(file, mask);
        if let Some(&id) = self.names.get(&n) {
            return self.textures.get(&id).map(|t| (id, t.info));
        }
        let (w, h, mut f) = self.pixels_of(file)?;
        let (mw, mh, m) = self.pixels_of(mask)?;
        if (w, h) != (mw, mh) {
            return self.resolve(file);
        }
        for (p, a) in f.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
            let g = ((a[0] as u16 + a[1] as u16 + a[2] as u16) / 3) as u8;
            p[3] = ((p[3] as u16 * g as u16) / 255) as u8
        }
        self.upload_rgba(&n, w, h, &f)
    }
    fn pixels_of(&mut self, n: &str) -> Option<(u32, u32, Vec<u8>)> {
        let id = self
            .names
            .get(n)
            .copied()
            .or_else(|| self.resolve(n).map(|x| x.0))?;
        let t = self.textures.get(&id)?;
        if let PixelStorage::Rgba(x) = &t.cpu {
            Some((t.info.width, t.info.height, x.clone()))
        } else {
            None
        }
    }
}

fn update_cpu(t: &mut VulkanTexture, origin: [u32; 2], extent: Extent2D, data: &TextureData<'_>) {
    let TextureData::Rgba8(x) = data else {
        t.cpu = PixelStorage::None;
        return;
    };
    let w = t.info.width as usize;
    match &mut t.cpu {
        PixelStorage::Opaque if rgba_opaque(x) => {}
        PixelStorage::Opaque => {
            let mut a = vec![255; w * t.info.height as usize];
            for y in 0..extent.height as usize {
                for z in 0..extent.width as usize {
                    a[(origin[1] as usize + y) * w + origin[0] as usize + z] =
                        x[(y * extent.width as usize + z) * 4 + 3]
                }
            }
            t.cpu = PixelStorage::Alpha(a)
        }
        PixelStorage::Alpha(a) => {
            for y in 0..extent.height as usize {
                for z in 0..extent.width as usize {
                    a[(origin[1] as usize + y) * w + origin[0] as usize + z] =
                        x[(y * extent.width as usize + z) * 4 + 3]
                }
            }
        }
        PixelStorage::Rgba(p) => {
            for y in 0..extent.height as usize {
                let s = y * extent.width as usize * 4;
                let d = ((origin[1] as usize + y) * w + origin[0] as usize) * 4;
                let n = extent.width as usize * 4;
                p[d..d + n].copy_from_slice(&x[s..s + n])
            }
        }
        _ => {}
    }
}

impl GpuBackend for VulkanBackend {
    fn backend_info(&self) -> BackendInfo {
        let bc = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, vk::Format::BC3_UNORM_BLOCK)
        };
        BackendInfo {
            kind: BackendKind::Vulkan,
            name: "Vulkan",
            stability: BackendStability::Experimental,
            capabilities: BackendCapabilities {
                offscreen_render_target: true,
                readback: true,
                compressed_astc: self.supports_astc_4x4(),
                compressed_bc: bc
                    .optimal_tiling_features
                    .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE),
                stencil: true,
                dynamic_mesh: true,
                ..BackendCapabilities::default()
            },
        }
    }

    fn begin_access(&mut self) {}
    fn end_access(&mut self) {}
    fn create_texture(
        &mut self,
        n: &str,
        d: TextureDesc,
        x: TextureData<'_>,
    ) -> Result<TextureId, String> {
        self.insert_texture(n, d, x, d.usage.contains(TextureUsage::CPU_READABLE))
    }
    fn update_texture(&mut self, id: TextureId, u: TextureUpdate<'_>) -> Result<(), String> {
        self.update_image(id, u)
    }
    fn destroy_texture(&mut self, id: TextureId) {
        let targets = self
            .render_targets
            .iter()
            .filter_map(|(&k, v)| (v.color == id).then_some(k))
            .collect::<Vec<_>>();
        for k in targets {
            if let Some(t) = self.render_targets.remove(&k) {
                self.retire(Retired::Framebuffer(t.framebuffer));
            }
        }
        self.remove_texture(id);
    }
    fn create_render_target(
        &mut self,
        n: &str,
        d: RenderTargetDesc,
    ) -> Result<RenderTarget, String> {
        if d.extent.is_empty()
            || !matches!(
                d.color_format,
                TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm
            )
        {
            return Err("Vulkan render target requires RGBA8/BGRA8".into());
        }
        let mut usage =
            TextureUsage::RENDER_TARGET | TextureUsage::TRANSFER_SRC | TextureUsage::TRANSFER_DST;
        if d.sampled {
            usage |= TextureUsage::SAMPLED
        }
        let td = TextureDesc {
            extent: d.extent,
            format: d.color_format,
            usage,
        };
        let color = self.insert_texture(n, td, TextureData::Uninitialized, false)?;
        let pass = self.render_pass(vk_format(d.color_format))?;
        let view = self.textures[&color].image.view;
        let a = [view];
        let fb = unsafe {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(pass)
                    .attachments(&a)
                    .width(d.extent.width)
                    .height(d.extent.height)
                    .layers(1),
                None,
            )
        }
        .map_err(|e| format!("create framebuffer: {e}"))?;
        let id = RenderTargetId::from_opaque(self.next_target);
        self.next_target = self.next_target.wrapping_add(1).max(1);
        self.render_targets.insert(
            id,
            VulkanRenderTarget {
                id,
                color,
                desc: d,
                framebuffer: fb,
                pass,
            },
        );
        Ok(RenderTarget { id, color, desc: d })
    }
    fn destroy_render_target(&mut self, id: RenderTargetId) {
        if let Some(t) = self.render_targets.remove(&id) {
            debug_assert_eq!(t.id, id);
            self.retire(Retired::Framebuffer(t.framebuffer));
            self.remove_texture(t.color);
        }
    }
    fn resize(&mut self, e: Extent2D) -> Result<(), String> {
        if e.is_empty() {
            return Err("empty Vulkan extent".into());
        }
        self.wait_submissions()?;
        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM)?;
        let new = create_private_target(
            &self.device,
            &self.memory,
            pass,
            e,
            TextureFormat::Rgba8Unorm,
        )?;
        let old = std::mem::replace(&mut self.main, new);
        self.destroy_private(old);
        self.clear_private_caches();
        self.last_overlay = None;
        if self.swapchain.is_some() {
            self.recreate_swapchain(e)?
        }
        Ok(())
    }
    fn begin_frame(&mut self, target: FrameTarget) -> Result<(), String> {
        self.check_operational()?;
        if self.active.is_some() {
            return Err("Vulkan frame already active".into());
        }
        self.collect_retired_resources();
        let view = self.target_view(target)?;
        let old = self.target_layout(target)?;
        let pool = unsafe {
            self.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(self.queue_family)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| format!("create frame pool: {e}"))?;
        let command = unsafe {
            self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| format!("allocate frame command: {e}"))?[0];
        unsafe {
            self.device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| format!("begin frame command: {e}"))?;
        barrier(
            &self.device,
            command,
            view.image,
            old,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        );
        self.set_target_layout(target, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: MAX_DRAW_SETS,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: MAX_DRAW_SETS * 3,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: MAX_DRAW_SETS,
            },
        ];
        let descriptors = unsafe {
            self.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(MAX_DRAW_SETS)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .map_err(|e| format!("create descriptor pool: {e}"))?;
        let scratch = match self.take_scratch() {
            Ok(scratch) => scratch,
            Err(error) => {
                unsafe {
                    self.device.destroy_descriptor_pool(descriptors, None);
                    self.device.destroy_command_pool(pool, None);
                }
                return Err(error);
            }
        };
        self.active = Some(ActiveFrame {
            target,
            view,
            pool,
            command,
            descriptors,
            scratch,
            pass_open: false,
            pass_framebuffer: vk::Framebuffer::null(),
            resources: vec![],
        });
        Ok(())
    }
    fn clear(&mut self, color: [f32; 4]) {
        let Some(target) = self.active.as_ref().map(|a| a.view) else {
            return;
        };
        self.clear_current(target, color, None);
    }
    fn end_frame(&mut self) {
        self.end_open_pass();
        let Some(mut a) = self.active.take() else {
            return;
        };
        self.scratch_capacity = self
            .scratch_capacity
            .max(a.scratch.cursor.next_power_of_two());
        barrier(
            &self.device,
            a.command,
            a.view.image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        self.set_target_layout(a.target, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        if let Err(error) = unsafe { self.device.end_command_buffer(a.command) } {
            self.fail("ending frame command buffer", error);
            self.discard_active(a);
            return;
        }
        let fence = match unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        } {
            Ok(x) => x,
            Err(error) => {
                self.fail("creating frame fence", error);
                self.discard_active(a);
                return;
            }
        };
        let cmds = [a.command];
        let submits = [vk::SubmitInfo::default().command_buffers(&cmds)];
        if let Err(error) = unsafe { self.device.queue_submit(self.queue, &submits, fence) } {
            self.fail("submitting frame", error);
            unsafe { self.device.destroy_fence(fence, None) };
            self.discard_active(a);
            return;
        }
        self.submitted = self.submitted.wrapping_add(1).max(1);
        self.submissions.push_back(Submission {
            serial: self.submitted,
            fence,
            pool: a.pool,
            descriptors: Some(a.descriptors),
            scratch: Some(a.scratch),
            resources: std::mem::take(&mut a.resources),
        })
    }
    fn capture_frame(&mut self, n: &str, e: Extent2D) -> FrameCapture {
        match self.capture_texture(n, e) {
            Ok(id) => FrameCapture::Texture(
                id,
                TextureInfo {
                    width: e.width,
                    height: e.height,
                },
                TextureOrigin::TopLeft,
            ),
            Err(_) => FrameCapture::Pixels(vec![]),
        }
    }
    fn readback(
        &mut self,
        target: FrameTarget,
        e: Extent2D,
        out: &mut [u8],
    ) -> Result<usize, String> {
        self.readback_impl(target, e, out)
    }
    fn render(&mut self, f: &DrawList) -> RenderRegion {
        self.render_impl(f, None, false)
    }
    fn render_damage(&mut self, f: &DrawList, d: [f32; 4]) -> RenderRegion {
        self.render_impl(f, Some(d), false)
    }
    fn render_damage_visualized(&mut self, f: &DrawList, d: [f32; 4]) -> RenderRegion {
        self.render_impl(f, Some(d), true)
    }
    fn render_visualized(&mut self, f: &DrawList) -> RenderRegion {
        self.render_impl(f, None, true)
    }
    fn clear_damage_overlay(&mut self, f: &DrawList) -> Option<RenderRegion> {
        let r = self.last_overlay?;
        Some(self.render_impl(f, r.damage(), false))
    }
    fn replace_asset_source(&mut self, s: Box<AssetSource>) {
        let framebuffers = self
            .render_targets
            .drain()
            .map(|(_, target)| target.framebuffer)
            .collect::<Vec<_>>();
        for framebuffer in framebuffers {
            self.retire(Retired::Framebuffer(framebuffer));
        }
        let ids = self.textures.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.remove_texture(id);
        }
        self.source = Some(s)
    }
    fn cached_texture_info(&self, n: &str) -> Option<TextureInfo> {
        self.names
            .get(n)
            .and_then(|id| self.textures.get(id))
            .map(|t| t.info)
    }
    fn texture_content_revision(&self) -> u64 {
        self.revision
    }
    fn changed_texture_ids_since(&self, r: u64) -> HashSet<TextureId> {
        self.texture_revisions
            .iter()
            .filter_map(|(&id, &x)| (x > r).then_some(id))
            .collect()
    }
    fn evict_texture_prefix(&mut self, p: &str) -> usize {
        let ids = self
            .names
            .iter()
            .filter_map(|(n, &id)| n.starts_with(p).then_some(id))
            .collect::<HashSet<_>>();
        for id in &ids {
            self.remove_texture(*id);
        }
        ids.len()
    }
    fn upload_video_rgba(&mut self, _: &str, _: u32, _: u32, _: &[u8]) -> bool {
        false
    }
    fn set_native_surface(&mut self, s: NativeSurface) -> Result<(), String> {
        self.attach_surface(s)
    }
    fn clear_native_surface(&mut self) {
        self.destroy_swapchain()
    }
    fn present(&mut self, _: Option<[f32; 4]>) -> Result<(), String> {
        self.present_impl()
    }
    fn register_hlsl_shader(&mut self, _: &str, _: &[u8]) -> Result<ShaderId, String> {
        Err("runtime HLSL is not implemented by VulkanBackend".into())
    }
    fn collect_retired_resources(&mut self) {
        self.collect()
    }
    fn set_profile_enabled(&self, x: bool) {
        self.profiling.set(x);
        if !x {
            let _ = self.take_profile_stats();
        }
    }
    fn take_profile_stats(&self) -> GpuProfileStats {
        GpuProfileStats {
            texture_upload_ns: self.upload_ns.replace(0),
            uploaded_bytes: self.uploaded.replace(0),
            draw_calls: self.draws.replace(0),
            vertices: self.vertices.replace(0),
            texture_binds: self.binds.replace(0),
            dynamic_mesh_uploaded_bytes: self.mesh_bytes.replace(0),
            texture_count: self.textures.len() as u64,
            texture_gpu_bytes: self
                .textures
                .values()
                .map(|t| t.image.desc.extent.rgba8_len().unwrap_or(0) as u64)
                .sum(),
            texture_cpu_bytes: self.textures.values().map(|t| t.cpu.bytes() as u64).sum(),
            ..Default::default()
        }
    }
}

impl VulkanBackend {
    fn destroy_private(&self, t: PrivateTarget) {
        unsafe { self.device.destroy_framebuffer(t.framebuffer, None) };
        destroy_image(&self.device, t.image)
    }
    fn clear_private_caches(&mut self) {
        let groups = std::mem::take(&mut self.groups);
        let masks = std::mem::take(&mut self.masks);
        for t in groups.into_iter().chain(masks) {
            self.destroy_private(t)
        }
    }
    fn collect(&mut self) {
        loop {
            let Some(s) = self.submissions.front() else {
                break;
            };
            match unsafe { self.device.get_fence_status(s.fence) } {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    self.fail("polling submission fence", error);
                    break;
                }
            }
            let s = self.submissions.pop_front().unwrap();
            self.completed = self.completed.max(s.serial);
            if let Some(scratch) = s.scratch {
                self.recycle_scratch(scratch);
            }
            unsafe {
                self.device.destroy_fence(s.fence, None);
                if let Some(p) = s.descriptors {
                    self.device.destroy_descriptor_pool(p, None)
                }
                self.device.destroy_command_pool(s.pool, None)
            }
            for r in s.resources {
                self.destroy_retired(r)
            }
        }
        while self
            .retired
            .front()
            .is_some_and(|x| x.after <= self.completed)
        {
            let r = self.retired.pop_front().unwrap().resource;
            self.destroy_retired(r)
        }
    }
    fn destroy_retired(&self, r: Retired) {
        match r {
            Retired::Texture(t) => destroy_image(&self.device, t.image),
            Retired::Buffer(b) => destroy_buffer(&self.device, b),
            Retired::Framebuffer(f) => unsafe { self.device.destroy_framebuffer(f, None) },
        }
    }
    fn capture_texture(&mut self, n: &str, e: Extent2D) -> Result<TextureId, String> {
        self.end_open_pass();
        let (a_cmd, a_view, a_target) = self
            .active
            .as_ref()
            .map(|a| (a.command, a.view, a.target))
            .ok_or("capture without active Vulkan frame")?;
        if e.width > a_view.extent.width || e.height > a_view.extent.height {
            return Err("capture extent exceeds frame".into());
        }
        let d = TextureDesc {
            extent: e,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST,
        };
        let id = self.insert_texture(n, d, TextureData::Uninitialized, false)?;
        let dst = self.textures[&id].image.raw;
        barrier(
            &self.device,
            a_cmd,
            a_view.image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        );
        barrier(
            &self.device,
            a_cmd,
            dst,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        let region = [vk::ImageCopy::default()
            .src_subresource(layers())
            .dst_subresource(layers())
            .extent(vk::Extent3D {
                width: e.width,
                height: e.height,
                depth: 1,
            })];
        unsafe {
            self.device.cmd_copy_image(
                a_cmd,
                a_view.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &region,
            )
        };
        barrier(
            &self.device,
            a_cmd,
            dst,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        barrier(
            &self.device,
            a_cmd,
            a_view.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        );
        self.textures.get_mut(&id).unwrap().image.layout =
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        self.set_target_layout(a_target, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        Ok(id)
    }
    fn readback_impl(
        &mut self,
        target: FrameTarget,
        e: Extent2D,
        out: &mut [u8],
    ) -> Result<usize, String> {
        self.check_operational()?;
        if self.active.is_some() {
            return Err("cannot read active Vulkan frame".into());
        }
        let n = e.rgba8_len().ok_or("readback overflow")?;
        if out.len() < n {
            return Err("readback buffer too small".into());
        }
        self.wait_submissions()?;
        let view = self.target_view(target)?;
        if e.width > view.extent.width || e.height > view.extent.height {
            return Err("readback extent exceeds target".into());
        }
        let old = self.target_layout(target)?;
        let staging = create_buffer(
            &self.device,
            &self.memory,
            n,
            vk::BufferUsageFlags::TRANSFER_DST,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let device = &self.device;
        immediate(device, self.queue, self.queue_family, |cmd| {
            barrier(
                device,
                cmd,
                view.image,
                old,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            let r = [vk::BufferImageCopy::default()
                .image_subresource(layers())
                .image_extent(vk::Extent3D {
                    width: e.width,
                    height: e.height,
                    depth: 1,
                })];
            unsafe {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    view.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    staging.raw,
                    &r,
                )
            };
            barrier(
                device,
                cmd,
                view.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                old,
            )
        })?;
        let p = unsafe {
            self.device
                .map_memory(staging.memory, 0, n as u64, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| format!("map readback: {e}"))?;
        unsafe {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), out.as_mut_ptr(), n);
            self.device.unmap_memory(staging.memory)
        };
        destroy_buffer(&self.device, staging);
        if view.format == vk::Format::B8G8R8A8_UNORM {
            for p in out[..n].chunks_exact_mut(4) {
                p.swap(0, 2)
            }
        }
        Ok(n)
    }
    fn render_impl(
        &mut self,
        frame: &DrawList,
        damage: Option<[f32; 4]>,
        visualize: bool,
    ) -> RenderRegion {
        let current = RenderRegion::from_damage(damage);
        let repaint = self
            .last_overlay
            .map(|x| x.union(current))
            .unwrap_or(current);
        let Some(target) = self.active.as_ref().map(|a| a.view) else {
            return repaint;
        };
        self.clear_current(target, [0., 0., 0., 1.], repaint.damage());
        if let Err(e) = self.encode_range(
            target,
            frame,
            0,
            frame.commands.len(),
            frame.shader_groups.len(),
            0,
            repaint.damage(),
        ) {
            self.fail("encoding render commands", e);
        }
        if visualize {
            let colors = [
                [1., 0.12, 0.08],
                [0.05, 0.72, 1.],
                [0.18, 1., 0.28],
                [1., 0.82, 0.05],
            ];
            let c = colors[self.flash % 4];
            self.flash = self.flash.wrapping_add(1);
            let solid = solid_command(self.main.image.desc.extent, c, 0.24, current.damage());
            let _ = self.encode_draw(target, &solid, None, Some(self.white.view), None, None);
        }
        self.last_overlay = visualize.then_some(current);
        repaint
    }
    #[allow(clippy::too_many_arguments)]
    fn encode_range(
        &mut self,
        target: TargetView,
        frame: &DrawList,
        start: usize,
        end: usize,
        limit: usize,
        depth: usize,
        damage: Option<[f32; 4]>,
    ) -> Result<(), String> {
        let mut i = start;
        while i < end {
            let Some((gi, g)) = next_group(frame, i, end, limit) else {
                self.encode_draw(target, &frame.commands[i], damage, None, None, None)?;
                i += 1;
                continue;
            };
            let gt = self.ensure_target(false, depth)?;
            self.clear_current(gt, [0.; 4], None);
            self.encode_range(gt, frame, g.start, g.end, gi, depth + 1, None)?;
            self.transition_private(false, depth, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            let mask = if let Some([s, e]) = g.mask_range {
                let mt = self.ensure_target(true, depth)?;
                self.clear_current(mt, [0.; 4], None);
                for c in &frame.mask_commands[s..e] {
                    self.encode_draw(mt, c, None, None, None, None)?
                }
                self.transition_private(true, depth, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                Some(mt.view)
            } else {
                None
            };
            let mut effect = g.effect.clone();
            effect.mask_texture = None;
            let composite = DrawCommand {
                texture: TextureId(0),
                size: TextureInfo {
                    width: gt.extent.width,
                    height: gt.extent.height,
                },
                transform: glam::Affine2::IDENTITY,
                opacity: 1.,
                blend: group_blend(&g),
                color: ColorFilter::default(),
                clip: ClipRect {
                    uv_offset: [0., 0.],
                    uv_scale: [1., 1.],
                    quad_size: [
                        self.main.image.desc.extent.width as f32,
                        self.main.image.desc.extent.height as f32,
                    ],
                },
                clip_bounds: g.clip_bounds,
                shader: Some(effect),
                mesh: None,
                stencil: None,
                native_emote: None,
            };
            self.encode_draw(target, &composite, damage, Some(gt.view), mask, None)?;
            i = g.end;
        }
        Ok(())
    }
    fn ensure_target(&mut self, mask: bool, depth: usize) -> Result<TargetView, String> {
        let extent = self.main.image.desc.extent;
        let pass = self.render_pass(vk::Format::R8G8B8A8_UNORM)?;
        {
            let list = if mask {
                &mut self.masks
            } else {
                &mut self.groups
            };
            while list.len() <= depth {
                list.push(create_private_target(
                    &self.device,
                    &self.memory,
                    pass,
                    extent,
                    TextureFormat::Rgba8Unorm,
                )?)
            }
            if list[depth].image.desc.extent != extent {
                let old = std::mem::replace(
                    &mut list[depth],
                    create_private_target(
                        &self.device,
                        &self.memory,
                        pass,
                        extent,
                        TextureFormat::Rgba8Unorm,
                    )?,
                );
                unsafe { self.device.destroy_framebuffer(old.framebuffer, None) };
                destroy_image(&self.device, old.image)
            }
        }
        let (image, old_layout) = {
            let target = if mask {
                &self.masks[depth]
            } else {
                &self.groups[depth]
            };
            (target.image.raw, target.image.layout)
        };
        if old_layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
            self.image_barrier(image, old_layout, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            if mask {
                self.masks[depth].image.layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
            } else {
                self.groups[depth].image.layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
            }
        }
        Ok(private_view(if mask {
            &self.masks[depth]
        } else {
            &self.groups[depth]
        }))
    }
    fn transition_private(&mut self, mask: bool, depth: usize, new: vk::ImageLayout) {
        let (image, old) = {
            let target = if mask {
                &self.masks[depth]
            } else {
                &self.groups[depth]
            };
            (target.image.raw, target.image.layout)
        };
        self.image_barrier(image, old, new);
        if mask {
            self.masks[depth].image.layout = new;
        } else {
            self.groups[depth].image.layout = new;
        }
    }
    fn end_open_pass(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if !active.pass_open {
            return;
        }
        unsafe { self.device.cmd_end_render_pass(active.command) };
        active.pass_open = false;
        active.pass_framebuffer = vk::Framebuffer::null();
    }
    fn ensure_pass(&mut self, target: TargetView) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.pass_open && active.pass_framebuffer == target.framebuffer)
        {
            return;
        }
        self.end_open_pass();
        let command = self.active.as_ref().unwrap().command;
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: target.extent.width,
                height: target.extent.height,
            },
        };
        unsafe {
            self.device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(target.pass)
                    .framebuffer(target.framebuffer)
                    .render_area(area),
                vk::SubpassContents::INLINE,
            );
        }
        let active = self.active.as_mut().unwrap();
        active.pass_open = true;
        active.pass_framebuffer = target.framebuffer;
    }
    fn image_barrier(&mut self, image: vk::Image, old: vk::ImageLayout, new: vk::ImageLayout) {
        self.end_open_pass();
        let command = self.active.as_ref().unwrap().command;
        barrier(&self.device, command, image, old, new);
    }
    fn clear_current(&mut self, target: TargetView, color: [f32; 4], rect: Option<[f32; 4]>) {
        self.ensure_pass(target);
        let command = self.active.as_ref().unwrap().command;
        let r = scissor(rect, target.extent, target.extent).unwrap_or(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: target.extent.width,
                height: target.extent.height,
            },
        });
        unsafe {
            self.device.cmd_clear_attachments(
                command,
                &[vk::ClearAttachment {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    color_attachment: 0,
                    clear_value: vk::ClearValue {
                        color: vk::ClearColorValue { float32: color },
                    },
                }],
                &[vk::ClearRect {
                    rect: r,
                    base_array_layer: 0,
                    layer_count: 1,
                }],
            );
        }
    }
    fn take_scratch(&mut self) -> Result<ScratchBuffer, String> {
        let size = self.scratch_capacity.max(SCRATCH_INITIAL);
        if let Some(index) = self
            .scratch_free
            .iter()
            .position(|scratch| scratch.buffer.size as usize >= size)
        {
            let mut scratch = self.scratch_free.swap_remove(index);
            scratch.cursor = 0;
            return Ok(scratch);
        }
        create_scratch_buffer(&self.device, &self.memory, size)
    }
    fn recycle_scratch(&mut self, mut scratch: ScratchBuffer) {
        if (scratch.buffer.size as usize) < self.scratch_capacity
            || self.scratch_free.len() >= FRAMES_IN_FLIGHT
        {
            destroy_scratch(&self.device, scratch);
            return;
        }
        scratch.cursor = 0;
        self.scratch_free.push(scratch);
    }
    fn alloc_scratch(
        &mut self,
        size: usize,
        align: usize,
        usage: vk::BufferUsageFlags,
        data: &[u8],
    ) -> Result<(vk::Buffer, u64, Option<Buffer>), String> {
        let grow_to = {
            let active = self.active.as_mut().ok_or("draw without active frame")?;
            if let Some(offset) = active.scratch.alloc(size, align) {
                active.scratch.write(offset, data);
                return Ok((active.scratch.buffer.raw, offset, None));
            }
            active
                .scratch
                .cursor
                .saturating_add(size)
                .max(self.scratch_capacity.saturating_mul(2))
                .next_power_of_two()
        };
        let buffer = create_buffer(
            &self.device,
            &self.memory,
            size,
            usage,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        write_buffer(&self.device, &buffer, data)?;
        self.scratch_capacity = self.scratch_capacity.max(grow_to);
        Ok((buffer.raw, 0, Some(buffer)))
    }
    fn discard_active(&mut self, active: ActiveFrame) {
        destroy_scratch(&self.device, active.scratch);
        for resource in active.resources {
            self.destroy_retired(resource);
        }
        unsafe {
            self.device
                .destroy_descriptor_pool(active.descriptors, None);
            self.device.destroy_command_pool(active.pool, None);
        }
    }
    fn encode_draw(
        &mut self,
        target: TargetView,
        c: &DrawCommand,
        damage: Option<[f32; 4]>,
        source_override: Option<vk::ImageView>,
        mask_override: Option<vk::ImageView>,
        blend: Option<PipelineBlend>,
    ) -> Result<(), String> {
        let Some(sc) = scissor(
            intersect_optional(c.clip_bounds, damage),
            self.main.image.desc.extent,
            target.extent,
        ) else {
            return Ok(());
        };
        let vertex = if c.mesh.as_ref().is_some_and(|x| !x.vertices.is_empty()) {
            VertexLayout::Mesh
        } else {
            VertexLayout::IndexedQuad
        };
        let shader = shader_kind(c.shader.as_ref());
        let key = PipelineKey {
            shader,
            format: target.format,
            blend: blend.unwrap_or(PipelineBlend::Draw(c.blend)),
            stencil: if shader == ShaderKind::AlphaMask {
                StencilMode::MaskComposite
            } else {
                StencilMode::Disabled
            },
            vertex,
        };
        let pipeline = self.pipeline(key, target.pass)?;
        let source = source_override
            .or_else(|| self.textures.get(&c.texture).map(|t| t.image.view))
            .ok_or("unknown Vulkan texture")?;
        let mask = mask_override
            .or_else(|| {
                c.shader
                    .as_ref()?
                    .mask_texture
                    .and_then(|id| self.textures.get(&id).map(|t| t.image.view))
            })
            .unwrap_or(self.white.view);
        let user = c
            .shader
            .as_ref()
            .and_then(|e| e.user_texture)
            .and_then(|id| self.textures.get(&id))
            .map(|t| t.image.view)
            .unwrap_or(self.transparent.view);
        let uniform = UniformBlock {
            vertex: vertex_uniforms(c, self.main.image.desc.extent),
            sprite: sprite_uniforms(c),
            effect: effect_uniforms(c),
        };
        let uniform_bytes = value_bytes(&uniform);
        let (uniform_buffer, uniform_offset, extra_uniform) = self.alloc_scratch(
            uniform_bytes.len(),
            self.uniform_align,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            uniform_bytes,
        )?;
        let pool = self
            .active
            .as_ref()
            .map(|a| a.descriptors)
            .ok_or("draw without active frame")?;
        let layouts = [self.descriptor_layout];
        let set = unsafe {
            self.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&layouts),
            )
        }
        .map_err(|e| format!("allocate draw descriptor: {e}"))?[0];
        let bi = [vk::DescriptorBufferInfo::default()
            .buffer(uniform_buffer)
            .offset(uniform_offset)
            .range(std::mem::size_of::<UniformBlock>() as u64)];
        let src = [vk::DescriptorImageInfo::default()
            .image_view(source)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let msk = [vk::DescriptorImageInfo::default()
            .image_view(mask)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let usr = [vk::DescriptorImageInfo::default()
            .image_view(user)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let sam = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&bi),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&src),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&msk),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&usr),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&sam),
        ];
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        let mesh = if vertex == VertexLayout::Mesh {
            let v = c
                .mesh
                .as_ref()
                .unwrap()
                .vertices
                .iter()
                .map(|x| Vertex {
                    position: [x[0], x[1]],
                    uv: [x[2], x[3]],
                })
                .collect::<Vec<_>>();
            let bytes = bytes(&v);
            let (buffer, offset, extra) =
                self.alloc_scratch(bytes.len(), 16, vk::BufferUsageFlags::VERTEX_BUFFER, bytes)?;
            Some((buffer, offset, extra, v.len() as u32, bytes.len() as u64))
        } else {
            None
        };
        self.ensure_pass(target);
        let active_cmd = self.active.as_ref().unwrap().command;
        unsafe {
            self.device
                .cmd_bind_pipeline(active_cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
            self.device.cmd_set_viewport(
                active_cmd,
                0,
                &[vk::Viewport {
                    x: 0.,
                    y: 0.,
                    width: target.extent.width as f32,
                    height: target.extent.height as f32,
                    min_depth: 0.,
                    max_depth: 1.,
                }],
            );
            self.device.cmd_set_scissor(active_cmd, 0, &[sc]);
            self.device.cmd_bind_descriptor_sets(
                active_cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[set],
                &[],
            );
            if let Some((buffer, offset, _, n, _)) = &mesh {
                self.device
                    .cmd_bind_vertex_buffers(active_cmd, 0, &[*buffer], &[*offset]);
                self.device.cmd_draw(active_cmd, *n, 1, 0, 0)
            } else {
                self.device
                    .cmd_bind_vertex_buffers(active_cmd, 0, &[self.quad_vertex.raw], &[0]);
                self.device.cmd_bind_index_buffer(
                    active_cmd,
                    self.quad_index.raw,
                    0,
                    vk::IndexType::UINT16,
                );
                self.device.cmd_draw_indexed(active_cmd, 6, 1, 0, 0, 0)
            }
        };
        let count = mesh.as_ref().map_or(6, |x| x.3 as u64);
        let mb = mesh.as_ref().map_or(0, |x| x.4);
        let a = self.active.as_mut().unwrap();
        if let Some(buffer) = extra_uniform {
            a.resources.push(Retired::Buffer(buffer));
        }
        if let Some((_, _, Some(buffer), _, _)) = mesh {
            a.resources.push(Retired::Buffer(buffer));
        }
        if self.profiling.get() {
            self.draws.set(self.draws.get() + 1);
            self.vertices.set(self.vertices.get() + count);
            self.binds.set(self.binds.get() + 3);
            self.mesh_bytes.set(self.mesh_bytes.get() + mb)
        }
        Ok(())
    }
    fn pipeline(&mut self, key: PipelineKey, pass: vk::RenderPass) -> Result<vk::Pipeline, String> {
        if let Some(&p) = self.pipelines.get(&key) {
            return Ok(p);
        }
        let (fragment, name) = match key.shader {
            ShaderKind::Sprite => (self.shaders.sprite, c"sprite_fragment"),
            ShaderKind::AlphaMask => (self.shaders.alpha_mask, c"alpha_mask_fragment"),
            ShaderKind::GroupComposite => (self.shaders.group, c"group_composite_fragment"),
            ShaderKind::RuleTransition => (self.shaders.rule, c"rule_transition_fragment"),
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.shaders.vertex)
                .name(c"sprite_vertex"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment)
                .name(name),
        ];
        let binding = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<Vertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let attrs = [
            vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 8,
            },
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&binding)
            .vertex_attribute_descriptions(&attrs);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachment = blend_state(key.blend);
        let attachments = [attachment];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dyns = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dy = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyns);
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .dynamic_state(&dy)
            .layout(self.pipeline_layout)
            .render_pass(pass)
            .subpass(0)];
        let p = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &info, None)
        }
        .map_err(|(_, e)| format!("create Vulkan pipeline {key:?}: {e}"))?[0];
        self.pipelines.insert(key, p);
        Ok(p)
    }
    fn attach_surface(&mut self, s: NativeSurface) -> Result<(), String> {
        self.check_operational()?;
        if s.kind != NativeSurfaceKind::AndroidNativeWindow {
            return Err("VulkanBackend currently accepts ANativeWindow only".into());
        }
        self.destroy_swapchain();
        #[cfg(target_os = "android")]
        {
            let loader = ash::khr::android_surface::Instance::new(&self._entry, &self.instance);
            let info = vk::AndroidSurfaceCreateInfoKHR::default()
                .window(s.handle.cast::<vk::ANativeWindow>());
            let surface = unsafe { loader.create_android_surface(&info, None) }
                .map_err(|e| format!("create Android Vulkan surface: {e}"))?;
            if !unsafe {
                self.surface_loader.get_physical_device_surface_support(
                    self.physical,
                    self.queue_family,
                    surface,
                )
            }
            .map_err(|e| format!("query Vulkan presentation support: {e}"))?
            {
                unsafe { self.surface_loader.destroy_surface(surface, None) };
                return Err(
                    "selected Vulkan graphics queue cannot present to ANativeWindow".into(),
                );
            }
            match self.build_swapchain(surface, s.extent) {
                Ok(swapchain) => {
                    self.swapchain = Some(swapchain);
                    Ok(())
                }
                Err(error) => {
                    unsafe { self.surface_loader.destroy_surface(surface, None) };
                    Err(error)
                }
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = s;
            Err("ANativeWindow surfaces are available only on Android".into())
        }
    }
    fn build_swapchain(
        &self,
        surface: vk::SurfaceKHR,
        desired: Extent2D,
    ) -> Result<Swapchain, String> {
        let caps = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical, surface)
        }
        .map_err(|e| format!("query surface capabilities: {e}"))?;
        let formats = unsafe {
            self.surface_loader
                .get_physical_device_surface_formats(self.physical, surface)
        }
        .map_err(|e| format!("query surface formats: {e}"))?;
        let selected = formats
            .iter()
            .find(|x| x.format == vk::Format::B8G8R8A8_UNORM)
            .copied()
            .or_else(|| formats.first().copied())
            .ok_or("Vulkan surface has no formats")?;
        let source_features = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, vk::Format::R8G8B8A8_UNORM)
        };
        let target_features = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, selected.format)
        };
        if !source_features
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::BLIT_SRC)
            || !target_features
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::BLIT_DST)
        {
            return Err("Vulkan surface formats do not support presentation blit".into());
        }
        if !caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_DST)
        {
            return Err("Vulkan swapchain does not support transfer destination".into());
        }
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: desired
                    .width
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: desired
                    .height
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let modes = unsafe {
            self.surface_loader
                .get_physical_device_surface_present_modes(self.physical, surface)
        }
        .map_err(|e| format!("query surface present modes: {e}"))?;
        let present_mode = choose_present_mode(&modes);
        let count = preferred_swapchain_image_count(caps.min_image_count, caps.max_image_count);
        let alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|x| caps.supported_composite_alpha.contains(*x))
        .ok_or("no Vulkan composite alpha mode")?;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(count)
            .image_format(selected.format)
            .image_color_space(selected.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(present_mode)
            .clipped(true);
        let raw = unsafe { self.swapchain_loader.create_swapchain(&info, None) }
            .map_err(|e| format!("create Vulkan swapchain: {e}"))?;
        let images = unsafe { self.swapchain_loader.get_swapchain_images(raw) }
            .map_err(|e| format!("get swapchain images: {e}"))?;
        let mut views = Vec::with_capacity(images.len());
        for &image in &images {
            views.push(
                unsafe {
                    self.device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(selected.format)
                            .subresource_range(color_range()),
                        None,
                    )
                }
                .map_err(|e| format!("create swapchain view: {e}"))?,
            )
        }
        let mut frames = Vec::new();
        for _ in 0..FRAMES_IN_FLIGHT {
            let pool = unsafe {
                self.device.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(self.queue_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
            }
            .map_err(|e| format!("create present pool: {e}"))?;
            let command = unsafe {
                self.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            }
            .map_err(|e| format!("allocate present command: {e}"))?[0];
            let available = unsafe { self.device.create_semaphore(&Default::default(), None) }
                .map_err(|e| format!("create acquire semaphore: {e}"))?;
            let finished = unsafe { self.device.create_semaphore(&Default::default(), None) }
                .map_err(|e| format!("create render semaphore: {e}"))?;
            let fence = unsafe {
                self.device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
            }
            .map_err(|e| format!("create present fence: {e}"))?;
            frames.push(PresentFrame {
                pool,
                command,
                available,
                finished,
                fence,
                serial: 0,
            })
        }
        let layouts = vec![vk::ImageLayout::UNDEFINED; images.len()];
        Ok(Swapchain {
            surface,
            raw,
            extent,
            images,
            views,
            layouts,
            frames,
            next_frame: 0,
        })
    }
    fn recreate_swapchain(&mut self, e: Extent2D) -> Result<(), String> {
        let surface = self.swapchain.as_ref().ok_or("no Vulkan surface")?.surface;
        unsafe { self.device.device_wait_idle() }
            .map_err(|e| format!("wait before swapchain recreation: {e}"))?;
        let old = self.swapchain.take().unwrap();
        self.destroy_swapchain_parts(old, false);
        match self.build_swapchain(surface, e) {
            Ok(swapchain) => {
                self.swapchain = Some(swapchain);
                Ok(())
            }
            Err(error) => {
                unsafe { self.surface_loader.destroy_surface(surface, None) };
                Err(error)
            }
        }
    }
    fn destroy_swapchain(&mut self) {
        if let Some(s) = self.swapchain.take() {
            let _ = unsafe { self.device.device_wait_idle() };
            self.destroy_swapchain_parts(s, true)
        }
    }
    fn destroy_swapchain_parts(&self, s: Swapchain, destroy_surface: bool) {
        unsafe {
            for f in s.frames {
                self.device.destroy_fence(f.fence, None);
                self.device.destroy_semaphore(f.available, None);
                self.device.destroy_semaphore(f.finished, None);
                self.device.destroy_command_pool(f.pool, None)
            }
            for v in s.views {
                self.device.destroy_image_view(v, None)
            }
            self.swapchain_loader.destroy_swapchain(s.raw, None);
            if destroy_surface {
                self.surface_loader.destroy_surface(s.surface, None)
            }
        }
    }
    fn present_impl(&mut self) -> Result<(), String> {
        self.check_operational()?;
        if self.active.is_some() {
            return Err("cannot present active Vulkan frame".into());
        }
        let Some(s) = self.swapchain.as_mut() else {
            return Err("ANativeWindow surface is not configured".into());
        };
        let fi = s.next_frame;
        let fence = s.frames[fi].fence;
        unsafe { self.device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| format!("wait present frame: {e}"))?;
        self.completed = self.completed.max(s.frames[fi].serial);
        self.collect();
        let s = self.swapchain.as_mut().unwrap();
        let frame = &mut s.frames[fi];
        let (index, suboptimal) = match unsafe {
            self.swapchain_loader.acquire_next_image(
                s.raw,
                u64::MAX,
                frame.available,
                vk::Fence::null(),
            )
        } {
            Ok(x) => x,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                let e = self.main.image.desc.extent;
                self.recreate_swapchain(e)?;
                return self.present_impl();
            }
            Err(e) => return Err(format!("acquire Vulkan image: {e}")),
        };
        unsafe {
            self.device
                .reset_command_pool(frame.pool, vk::CommandPoolResetFlags::empty())
                .map_err(|e| format!("reset present command pool: {e}"))?;
            self.device.begin_command_buffer(
                frame.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| format!("begin present command: {e}"))?;
        let old = s.layouts[index as usize];
        barrier(
            &self.device,
            frame.command,
            self.main.image.raw,
            self.main.image.layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        );
        barrier(
            &self.device,
            frame.command,
            s.images[index as usize],
            old,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        let src = self.main.image.desc.extent;
        let blit = [vk::ImageBlit::default()
            .src_subresource(layers())
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: src.width as i32,
                    y: src.height as i32,
                    z: 1,
                },
            ])
            .dst_subresource(layers())
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: s.extent.width as i32,
                    y: s.extent.height as i32,
                    z: 1,
                },
            ])];
        unsafe {
            self.device.cmd_blit_image(
                frame.command,
                self.main.image.raw,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                s.images[index as usize],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &blit,
                vk::Filter::LINEAR,
            )
        };
        barrier(
            &self.device,
            frame.command,
            self.main.image.raw,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            self.main.image.layout,
        );
        barrier(
            &self.device,
            frame.command,
            s.images[index as usize],
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
        );
        s.layouts[index as usize] = vk::ImageLayout::PRESENT_SRC_KHR;
        unsafe { self.device.end_command_buffer(frame.command) }
            .map_err(|e| format!("end present command: {e}"))?;
        let waits = [frame.available];
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let cmds = [frame.command];
        let signals = [frame.finished];
        let submit = [vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signals)];
        unsafe { self.device.reset_fences(&[frame.fence]) }
            .map_err(|e| format!("reset present fence: {e}"))?;
        if let Err(error) = unsafe { self.device.queue_submit(self.queue, &submit, frame.fence) } {
            unsafe { self.device.destroy_fence(frame.fence, None) };
            frame.fence = unsafe {
                self.device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
            }
            .map_err(|e| format!("recover present fence after {error}: {e}"))?;
            return Err(format!("submit Vulkan present: {error}"));
        }
        self.submitted = self.submitted.wrapping_add(1).max(1);
        frame.serial = self.submitted;
        let swaps = [s.raw];
        let indices = [index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&signals)
            .swapchains(&swaps)
            .image_indices(&indices);
        let out = unsafe { self.swapchain_loader.queue_present(self.queue, &info) };
        s.next_frame = (fi + 1) % s.frames.len();
        match out {
            Ok(present_suboptimal) if suboptimal || present_suboptimal => {
                let e = self.main.image.desc.extent;
                self.recreate_swapchain(e)
            }
            Ok(_) => Ok(()),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                let e = self.main.image.desc.extent;
                self.recreate_swapchain(e)
            }
            Err(e) => Err(format!("present Vulkan swapchain: {e}")),
        }
    }
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        let _ = unsafe { self.device.device_wait_idle() };
        self.destroy_swapchain();
        if let Some(active) = self.active.take() {
            self.discard_active(active);
        }
        while let Some(submission) = self.submissions.pop_front() {
            if let Some(scratch) = submission.scratch {
                destroy_scratch(&self.device, scratch);
            }
            for resource in submission.resources {
                self.destroy_retired(resource);
            }
            unsafe {
                self.device.destroy_fence(submission.fence, None);
                if let Some(pool) = submission.descriptors {
                    self.device.destroy_descriptor_pool(pool, None);
                }
                self.device.destroy_command_pool(submission.pool, None);
            }
        }
        for scratch in self.scratch_free.drain(..) {
            destroy_scratch(&self.device, scratch);
        }
        while let Some(resource) = self.retired.pop_front() {
            self.destroy_retired(resource.resource);
        }
        for (_, target) in self.render_targets.drain() {
            unsafe {
                self.device.destroy_framebuffer(target.framebuffer, None);
            }
        }
        let textures = self
            .textures
            .drain()
            .map(|(_, texture)| texture)
            .collect::<Vec<_>>();
        for texture in textures {
            destroy_image(&self.device, texture.image);
        }
        let groups = std::mem::take(&mut self.groups);
        let masks = std::mem::take(&mut self.masks);
        for target in groups.into_iter().chain(masks) {
            self.destroy_private(target);
        }
        unsafe {
            self.device.destroy_framebuffer(self.main.framebuffer, None);
        }
        let main = std::mem::replace(&mut self.main.image, empty_image());
        let white = std::mem::replace(&mut self.white, empty_image());
        let transparent = std::mem::replace(&mut self.transparent, empty_image());
        destroy_image(&self.device, main);
        destroy_image(&self.device, white);
        destroy_image(&self.device, transparent);
        let quad_vertex = std::mem::replace(&mut self.quad_vertex, empty_buffer());
        let quad_index = std::mem::replace(&mut self.quad_index, empty_buffer());
        destroy_buffer(&self.device, quad_vertex);
        destroy_buffer(&self.device, quad_index);
        unsafe {
            for (_, pipeline) in self.pipelines.drain() {
                self.device.destroy_pipeline(pipeline, None);
            }
            for (_, pass) in self.render_passes.drain() {
                self.device.destroy_render_pass(pass, None);
            }
            self.device.destroy_shader_module(self.shaders.vertex, None);
            self.device.destroy_shader_module(self.shaders.sprite, None);
            self.device
                .destroy_shader_module(self.shaders.alpha_mask, None);
            self.device.destroy_shader_module(self.shaders.group, None);
            self.device.destroy_shader_module(self.shaders.rule, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn empty_buffer() -> Buffer {
    Buffer {
        raw: vk::Buffer::null(),
        memory: vk::DeviceMemory::null(),
        size: 0,
    }
}
fn empty_image() -> Image {
    Image {
        raw: vk::Image::null(),
        memory: vk::DeviceMemory::null(),
        view: vk::ImageView::null(),
        desc: TextureDesc::sampled_rgba8(1, 1),
        layout: vk::ImageLayout::UNDEFINED,
    }
}

fn shader_kind(e: Option<&ShaderEffect>) -> ShaderKind {
    match e.map(|x| x.name.as_str()) {
        Some(crate::render_pipeline::shader::ALPHA_MASK_SHADER) => ShaderKind::AlphaMask,
        Some(crate::render_pipeline::shader::GROUP_COMPOSITE_SHADER) => ShaderKind::GroupComposite,
        Some(crate::render_pipeline::shader::RULE_TRANS_SHADER) => ShaderKind::RuleTransition,
        _ => ShaderKind::Sprite,
    }
}
fn next_group(
    f: &DrawList,
    start: usize,
    end: usize,
    limit: usize,
) -> Option<(usize, ShaderGroup)> {
    f.shader_groups
        .iter()
        .enumerate()
        .take(limit)
        .filter(|(_, g)| g.start == start && g.end > start && g.end <= end)
        .max_by_key(|(i, g)| (g.end, *i))
        .map(|(i, g)| (i, g.clone()))
}
fn group_blend(g: &ShaderGroup) -> BlendMode {
    match g
        .effect
        .uniforms
        .get("blendMode")
        .and_then(|x| x.first())
        .copied()
        .unwrap_or(0.) as i32
    {
        1 => BlendMode::PremultipliedAdd,
        2 => BlendMode::Screen,
        3 => BlendMode::Multiply,
        _ => BlendMode::PremultipliedAlpha,
    }
}
fn intersect_optional(a: Option<[f32; 4]>, b: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => intersect(a, b),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}
fn intersect(a: [f32; 4], b: [f32; 4]) -> Option<[f32; 4]> {
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    let r = (a[0] + a[2]).min(b[0] + b[2]);
    let d = (a[1] + a[3]).min(b[1] + b[3]);
    (r > x && d > y).then_some([x, y, r - x, d - y])
}
fn scissor(r: Option<[f32; 4]>, stage: Extent2D, target: Extent2D) -> Option<vk::Rect2D> {
    let r = intersect(
        r.unwrap_or([0., 0., stage.width as f32, stage.height as f32]),
        [0., 0., stage.width as f32, stage.height as f32],
    )?;
    let sx = target.width as f32 / stage.width as f32;
    let sy = target.height as f32 / stage.height as f32;
    let x = (r[0] * sx).floor().max(0.) as i32;
    let y = (r[1] * sy).floor().max(0.) as i32;
    let rr = ((r[0] + r[2]) * sx).ceil().min(target.width as f32) as i32;
    let dd = ((r[1] + r[3]) * sy).ceil().min(target.height as f32) as i32;
    Some(vk::Rect2D {
        offset: vk::Offset2D { x, y },
        extent: vk::Extent2D {
            width: (rr - x) as u32,
            height: (dd - y) as u32,
        },
    })
}
fn vertex_uniforms(c: &DrawCommand, stage: Extent2D) -> VertexUniforms {
    let m = c.transform.matrix2;
    let t = c.transform.translation;
    let model = glam::Mat4::from_cols(
        glam::Vec4::new(m.x_axis.x, m.x_axis.y, 0., 0.),
        glam::Vec4::new(m.y_axis.x, m.y_axis.y, 0., 0.),
        glam::Vec4::new(0., 0., 1., 0.),
        glam::Vec4::new(t.x, t.y, 0., 1.),
    );
    // WGSL clip space is Y-up, matching Metal. naga's SPIR-V writer keeps
    // ADJUST_COORDINATE_SPACE by default and flips Position.y for Vulkan.
    // Using a Vulkan-native Y-down projection here double-flips rasterized
    // sprites, while render-to-texture groups get flipped twice and look upright.
    let proj = glam::Mat4::from_cols(
        glam::Vec4::new(2. / stage.width as f32, 0., 0., 0.),
        glam::Vec4::new(0., -2. / stage.height as f32, 0., 0.),
        glam::Vec4::new(0., 0., 1., 0.),
        glam::Vec4::new(-1., 1., 0., 1.),
    );
    VertexUniforms {
        transform: (proj * model).to_cols_array(),
        size: if c.mesh.is_some() {
            [1., 1.]
        } else {
            c.clip.quad_size
        },
        uv_offset: c.clip.uv_offset,
        uv_scale: c.clip.uv_scale,
        padding: [0.; 2],
    }
}
fn sprite_uniforms(c: &DrawCommand) -> SpriteUniforms {
    let m = c.native_emote;
    let colors = m.map(|x| x.corner_colors).unwrap_or([[1.; 4]; 4]);
    SpriteUniforms {
        opacity_flags: [
            c.opacity,
            c.color.grayscale as u8 as f32,
            c.color.negative as u8 as f32,
            m.is_some() as u8 as f32,
        ],
        multiply: [
            c.color.multiply[0],
            c.color.multiply[1],
            c.color.multiply[2],
            0.,
        ],
        emote_uv_rect: m.map(|x| x.uv_rect).unwrap_or([0.; 4]),
        emote_color_tl: colors[0],
        emote_color_tr: colors[1],
        emote_color_bl: colors[2],
        emote_color_br: colors[3],
        emote_blend_mode: [m.map(|x| x.blend_mode as f32).unwrap_or(0.), 0., 0., 0.],
        emote_clip_rect: m.map(|x| x.clip_rect).unwrap_or([0., 0., 1., 1.]),
        emote_wipe: m
            .map(|x| [x.wipe[0], x.wipe[1], x.wipe[2], 0.])
            .unwrap_or([0.; 4]),
    }
}
fn effect_uniforms(c: &DrawCommand) -> EffectUniforms {
    let u = |n: &str, i: usize, d: f32| {
        c.shader
            .as_ref()
            .and_then(|e| e.uniforms.get(n))
            .and_then(|x| x.get(i))
            .copied()
            .unwrap_or(d)
    };
    EffectUniforms {
        alpha_progress_vague_opaque: [
            u("alpha", 0, c.opacity),
            u("progress", 0, 0.),
            u("vague", 0, 0.),
            u("opaque", 0, 0.),
        ],
        color_multiply_grayscale: [
            u("colorMultiply", 0, c.color.multiply[0]),
            u("colorMultiply", 1, c.color.multiply[1]),
            u("colorMultiply", 2, c.color.multiply[2]),
            u("grayscale", 0, c.color.grayscale as u8 as f32),
        ],
        negative_padding: [u("negative", 0, c.color.negative as u8 as f32), 0., 0., 0.],
    }
}
fn solid_command(e: Extent2D, m: [f32; 3], opacity: f32, b: Option<[f32; 4]>) -> DrawCommand {
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
            multiply: m,
            grayscale: false,
            negative: false,
        },
        clip: ClipRect {
            uv_offset: [0., 0.],
            uv_scale: [1., 1.],
            quad_size: [e.width as f32, e.height as f32],
        },
        clip_bounds: b,
        shader: None,
        mesh: None,
        stencil: None,
        native_emote: None,
    }
}
fn blend_state(b: PipelineBlend) -> vk::PipelineColorBlendAttachmentState {
    let mut x = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    if b == PipelineBlend::Replace {
        return x;
    }
    x = x
        .blend_enable(true)
        .color_blend_op(vk::BlendOp::ADD)
        .alpha_blend_op(vk::BlendOp::ADD);
    match b {
        PipelineBlend::Replace => x,
        PipelineBlend::Draw(BlendMode::Alpha) => x
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA),
        PipelineBlend::Draw(BlendMode::PremultipliedAlpha) => x
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA),
        PipelineBlend::Draw(BlendMode::PremultipliedAdd) => {
            factors(x, vk::BlendFactor::ONE, vk::BlendFactor::ONE)
        }
        PipelineBlend::Draw(BlendMode::Add) => {
            factors(x, vk::BlendFactor::SRC_ALPHA, vk::BlendFactor::ONE)
        }
        PipelineBlend::Draw(BlendMode::Screen) => factors(
            x,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        ),
        PipelineBlend::Draw(BlendMode::Multiply) => factors(
            x,
            vk::BlendFactor::DST_COLOR,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        ),
        PipelineBlend::Draw(BlendMode::NativeAdd) => native_factors(
            x,
            vk::BlendOp::ADD,
            vk::BlendFactor::SRC_ALPHA,
            vk::BlendFactor::ONE,
        ),
        PipelineBlend::Draw(BlendMode::NativeReverseSubtract) => native_factors(
            x,
            vk::BlendOp::REVERSE_SUBTRACT,
            vk::BlendFactor::SRC_ALPHA,
            vk::BlendFactor::ONE,
        ),
        PipelineBlend::Draw(BlendMode::NativeMultiply) => native_factors(
            x,
            vk::BlendOp::ADD,
            vk::BlendFactor::DST_COLOR,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        ),
        PipelineBlend::Draw(BlendMode::NativeScreen) => native_factors(
            x,
            vk::BlendOp::ADD,
            vk::BlendFactor::ONE_MINUS_DST_COLOR,
            vk::BlendFactor::ONE,
        ),
    }
}
fn factors(
    x: vk::PipelineColorBlendAttachmentState,
    s: vk::BlendFactor,
    d: vk::BlendFactor,
) -> vk::PipelineColorBlendAttachmentState {
    x.src_color_blend_factor(s)
        .dst_color_blend_factor(d)
        .src_alpha_blend_factor(s)
        .dst_alpha_blend_factor(d)
}
fn native_factors(
    x: vk::PipelineColorBlendAttachmentState,
    op: vk::BlendOp,
    s: vk::BlendFactor,
    d: vk::BlendFactor,
) -> vk::PipelineColorBlendAttachmentState {
    x.color_blend_op(op)
        .src_color_blend_factor(s)
        .dst_color_blend_factor(d)
        .src_alpha_blend_factor(vk::BlendFactor::ZERO)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
}

fn layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}
fn destroy_image(device: &Device, i: Image) {
    unsafe {
        device.destroy_image_view(i.view, None);
        device.destroy_image(i.raw, None);
        device.free_memory(i.memory, None)
    }
}
fn create_scratch_buffer(
    device: &Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    size: usize,
) -> Result<ScratchBuffer, String> {
    let buffer = create_buffer(
        device,
        memory,
        size.max(1),
        vk::BufferUsageFlags::UNIFORM_BUFFER | vk::BufferUsageFlags::VERTEX_BUFFER,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    )?;
    let mapped = match unsafe {
        device.map_memory(buffer.memory, 0, buffer.size, vk::MemoryMapFlags::empty())
    } {
        Ok(pointer) => pointer.cast(),
        Err(error) => {
            destroy_buffer(device, buffer);
            return Err(format!("map Vulkan scratch: {error}"));
        }
    };
    Ok(ScratchBuffer {
        buffer,
        mapped,
        cursor: 0,
    })
}
fn destroy_scratch(device: &Device, scratch: ScratchBuffer) {
    unsafe { device.unmap_memory(scratch.buffer.memory) };
    destroy_buffer(device, scratch.buffer);
}
fn choose_present_mode(modes: &[vk::PresentModeKHR]) -> vk::PresentModeKHR {
    const PREFERRED: [vk::PresentModeKHR; 3] = [
        vk::PresentModeKHR::MAILBOX,
        vk::PresentModeKHR::IMMEDIATE,
        vk::PresentModeKHR::FIFO_RELAXED,
    ];
    PREFERRED
        .into_iter()
        .find(|mode| modes.contains(mode))
        .unwrap_or(vk::PresentModeKHR::FIFO)
}
fn preferred_swapchain_image_count(min_image_count: u32, max_image_count: u32) -> u32 {
    let count = min_image_count.max(3);
    if max_image_count > 0 {
        count.min(max_image_count)
    } else {
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_key_separates_required_vulkan_state() {
        let key = PipelineKey {
            shader: ShaderKind::Sprite,
            format: vk::Format::R8G8B8A8_UNORM,
            blend: PipelineBlend::Draw(BlendMode::Alpha),
            stencil: StencilMode::Disabled,
            vertex: VertexLayout::IndexedQuad,
        };
        assert_ne!(
            key,
            PipelineKey {
                shader: ShaderKind::AlphaMask,
                ..key
            }
        );
        assert_ne!(
            key,
            PipelineKey {
                format: vk::Format::B8G8R8A8_UNORM,
                ..key
            }
        );
        assert_ne!(
            key,
            PipelineKey {
                blend: PipelineBlend::Draw(BlendMode::Add),
                ..key
            }
        );
        assert_ne!(
            key,
            PipelineKey {
                stencil: StencilMode::MaskComposite,
                ..key
            }
        );
        assert_ne!(
            key,
            PipelineKey {
                vertex: VertexLayout::Mesh,
                ..key
            }
        );
    }

    #[test]
    fn vulkan_scissor_keeps_top_left_stage_coordinates() {
        assert_eq!(
            scissor(
                Some([10.0, 5.0, 20.0, 10.0]),
                Extent2D::new(100, 50),
                Extent2D::new(200, 100)
            ),
            Some(vk::Rect2D {
                offset: vk::Offset2D { x: 20, y: 10 },
                extent: vk::Extent2D {
                    width: 40,
                    height: 20
                }
            })
        );
    }

    #[test]
    fn uniform_block_matches_wgsl_layout() {
        assert_eq!(std::mem::size_of::<VertexUniforms>(), 96);
        assert_eq!(std::mem::size_of::<SpriteUniforms>(), 160);
        assert_eq!(std::mem::size_of::<EffectUniforms>(), 48);
        assert_eq!(std::mem::size_of::<UniformBlock>(), 304);
    }

    #[test]
    fn vertex_projection_matches_wgsl_y_up_clip_space() {
        let uniforms = vertex_uniforms(
            &solid_command(Extent2D::new(100, 50), [1.0, 1.0, 1.0], 1.0, None),
            Extent2D::new(100, 50),
        );
        let transform = glam::Mat4::from_cols_array(&uniforms.transform);
        let top_left = transform * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        let bottom_right = transform * glam::Vec4::new(100.0, 50.0, 0.0, 1.0);
        assert!((top_left.x + 1.0).abs() < 1e-5 && (top_left.y - 1.0).abs() < 1e-5);
        assert!((bottom_right.x - 1.0).abs() < 1e-5 && (bottom_right.y + 1.0).abs() < 1e-5);
    }

    #[test]
    fn present_mode_prefers_non_blocking_modes() {
        assert_eq!(
            choose_present_mode(&[vk::PresentModeKHR::FIFO, vk::PresentModeKHR::MAILBOX]),
            vk::PresentModeKHR::MAILBOX
        );
        assert_eq!(
            choose_present_mode(&[vk::PresentModeKHR::FIFO, vk::PresentModeKHR::IMMEDIATE]),
            vk::PresentModeKHR::IMMEDIATE
        );
        assert_eq!(
            choose_present_mode(&[vk::PresentModeKHR::FIFO, vk::PresentModeKHR::FIFO_RELAXED]),
            vk::PresentModeKHR::FIFO_RELAXED
        );
        assert_eq!(
            choose_present_mode(&[vk::PresentModeKHR::FIFO]),
            vk::PresentModeKHR::FIFO
        );
    }

    #[test]
    fn swapchain_image_count_prefers_triple_buffering() {
        assert_eq!(preferred_swapchain_image_count(1, 0), 3);
        assert_eq!(preferred_swapchain_image_count(2, 8), 3);
        assert_eq!(preferred_swapchain_image_count(3, 3), 3);
        assert_eq!(preferred_swapchain_image_count(2, 2), 2);
    }
}
