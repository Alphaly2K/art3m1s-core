use super::{Device, Extent2D, IOSurfaceRef, PrivateRenderTarget, ShaderEffect, Texture};
use crate::{backend::ShaderId, hlsl::ShaderRegistry};
use libloading::Library;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained};
use objc2_io_surface::{
    kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLResource, MTLSize, MTLStorageMode,
    MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use std::{cell::OnceCell, collections::HashMap, ffi::c_void, path::Path, ptr::NonNull};

type Handle = NonNull<c_void>;
type Destroy = unsafe extern "C" fn(Handle);
type CreateContext = unsafe extern "C" fn() -> Option<Handle>;
type MetalDevice = unsafe extern "C" fn(Handle) -> *const ProtocolObject<dyn MTLDevice>;

#[repr(C)]
struct Image {
    rgba: *const u8,
    width: u32,
    height: u32,
}

#[repr(C)]
struct Uniform {
    name: *const u8,
    length: usize,
    values: *const f32,
    count: usize,
}

struct Resource {
    raw: Handle,
    destroy: Destroy,
}

impl Drop for Resource {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.raw) };
    }
}

type CreateProgram = unsafe extern "C" fn(Handle, *const u8, usize) -> Option<Handle>;
type ProgramStraightAlpha = unsafe extern "C" fn(Handle) -> i32;
type CreateTargets = unsafe extern "C" fn(Handle, &IOSurfaceRef, &IOSurfaceRef) -> Option<Handle>;
type DrawEffect =
    unsafe extern "C" fn(Handle, Handle, Handle, Image, Image, *const Uniform, usize) -> i32;

#[derive(Clone, Copy)]
struct EffectProgram {
    raw: Handle,
    straight_alpha: bool,
}

pub(super) struct Targets {
    handle: Resource,
    pub foreground: PrivateRenderTarget,
    pub output: Texture,
}

pub(super) struct Effects {
    // Programs and targets must drop before the context, and all before the library.
    targets: Option<Targets>,
    programs: HashMap<Box<[u8]>, (Resource, EffectProgram)>,
    active: ShaderRegistry<EffectProgram>,
    context: OnceCell<Result<Resource, String>>,
    create_context: CreateContext,
    destroy_context: Destroy,
    metal_device: MetalDevice,
    create_program: CreateProgram,
    program_straight_alpha: Option<ProgramStraightAlpha>,
    destroy_program: Destroy,
    create_targets: CreateTargets,
    destroy_targets: Destroy,
    draw: DrawEffect,
    _library: Library,
}

impl Effects {
    pub fn open(directory: &Path) -> Result<Self, String> {
        if !directory.is_absolute() {
            return Err("DXVK runtime directory must be absolute".into());
        }
        // Keep the library alive until the last call through one of its symbols.
        unsafe {
            let library = Library::new(directory.join("libart3m1s_artemis_dxvk_host.dylib"))
                .map_err(|e| e.to_string())?;
            let create_context = *library
                .get::<CreateContext>(b"art_dxvk_context_create")
                .map_err(|e| e.to_string())?;
            let destroy_context = *library
                .get::<Destroy>(b"art_dxvk_context_destroy")
                .map_err(|e| e.to_string())?;
            let metal_device = *library
                .get::<MetalDevice>(b"art_dxvk_context_metal_device")
                .map_err(|e| e.to_string())?;
            let create_program = *library
                .get::<CreateProgram>(b"art_dxvk_program_create")
                .map_err(|e| e.to_string())?;
            let program_straight_alpha = library
                .get::<ProgramStraightAlpha>(b"art_dxvk_program_straight_alpha")
                .ok()
                .map(|symbol| *symbol);
            let destroy_program = *library
                .get::<Destroy>(b"art_dxvk_program_destroy")
                .map_err(|e| e.to_string())?;
            let create_targets = *library
                .get::<CreateTargets>(b"art_dxvk_targets_create")
                .map_err(|e| e.to_string())?;
            let destroy_targets = *library
                .get::<Destroy>(b"art_dxvk_targets_destroy")
                .map_err(|e| e.to_string())?;
            let draw = *library
                .get::<DrawEffect>(b"art_dxvk_draw")
                .map_err(|e| e.to_string())?;
            Ok(Self {
                targets: None,
                programs: HashMap::new(),
                active: ShaderRegistry::default(),
                context: OnceCell::new(),
                create_context,
                destroy_context,
                metal_device,
                create_program,
                program_straight_alpha,
                destroy_program,
                create_targets,
                destroy_targets,
                draw,
                _library: library,
            })
        }
    }

    fn context(&self) -> Result<Handle, String> {
        self.context
            .get()
            .ok_or("DXVK effect context has not been prepared")?
            .as_ref()
            .map(|context| context.raw)
            .map_err(Clone::clone)
    }

    pub fn cache(&mut self, device: &Device, source: &[u8], artifact: &[u8]) -> Result<(), String> {
        if !self.programs.contains_key(source) {
            self.context.get_or_init(|| unsafe {
                let context = Resource {
                    raw: (self.create_context)().ok_or("cannot create DXVK effect context")?,
                    destroy: self.destroy_context,
                };
                let selected = (self.metal_device)(context.raw)
                    .as_ref()
                    .ok_or("DXVK has no Metal device")?;
                if selected.registryID() != device.registryID() {
                    return Err("DXVK and Metal selected different devices".into());
                }
                Ok(context)
            });
            let context = self.context()?;
            let raw = unsafe { (self.create_program)(context, artifact.as_ptr(), artifact.len()) }
                .ok_or("cannot prepare ADE2 shader")?;
            let straight_alpha = self
                .program_straight_alpha
                .is_some_and(|query| unsafe { query(raw) != 0 });
            self.programs.insert(
                source.into(),
                (
                    Resource {
                        raw,
                        destroy: self.destroy_program,
                    },
                    EffectProgram {
                        raw,
                        straight_alpha,
                    },
                ),
            );
        }
        Ok(())
    }

    pub fn register(&mut self, name: &str, source: &[u8]) -> Option<ShaderId> {
        let (_, program) = *self.programs.get(source)?;
        Some(self.active.insert_or_replace(name, program).0)
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.active.remove(name).is_some()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.active.id(name).is_some()
    }

    pub fn straight_alpha(&self, name: &str) -> bool {
        self.active
            .get_by_name(name)
            .is_some_and(|program| program.straight_alpha)
    }

    pub fn foreground(
        &mut self,
        device: &Device,
        extent: Extent2D,
    ) -> Result<&PrivateRenderTarget, String> {
        if self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.foreground.extent != extent)
        {
            let context = self.context()?;
            let (fore_surface, foreground) = shared_texture(device, extent)?;
            let (out_surface, output) = shared_texture(device, extent)?;
            let raw = unsafe { (self.create_targets)(context, &fore_surface, &out_surface) }
                .ok_or("cannot import DXVK effect targets")?;
            self.targets = Some(Targets {
                handle: Resource {
                    raw,
                    destroy: self.destroy_targets,
                },
                foreground: PrivateRenderTarget {
                    texture: foreground,
                    extent,
                    format: MTLPixelFormat::BGRA8Unorm,
                },
                output,
            });
        }
        Ok(&self.targets.as_ref().unwrap().foreground)
    }

    pub fn draw(
        &self,
        effect: &ShaderEffect,
        mask: Option<&Texture>,
        user: Option<&Texture>,
    ) -> Result<Texture, String> {
        let targets = self.targets.as_ref().ok_or("DXVK targets unavailable")?;
        let context = self.context()?;
        let program = *self
            .active
            .get_by_name(&effect.name)
            .ok_or("DXVK program unavailable")?;
        let mask = ReadImage::new(mask)?;
        let user = ReadImage::new(user)?;
        let uniforms = effect
            .uniforms
            .iter()
            .map(|(name, values)| Uniform {
                name: name.as_ptr(),
                length: name.len(),
                values: values.as_ptr(),
                count: values.len(),
            })
            .collect::<Vec<_>>();
        let code = unsafe {
            (self.draw)(
                context,
                program.raw,
                targets.handle.raw,
                mask.as_image(),
                user.as_image(),
                uniforms.as_ptr(),
                uniforms.len(),
            )
        };
        if code != 0 {
            return Err(format!("DXVK draw returned {code}"));
        }
        Ok(targets.output.clone())
    }
}

fn shared_texture(
    device: &Device,
    extent: Extent2D,
) -> Result<(CFRetained<IOSurfaceRef>, Texture), String> {
    let width = CFNumber::new_i64(i64::from(extent.width));
    let height = CFNumber::new_i64(i64::from(extent.height));
    let stride = CFNumber::new_i32(4);
    let format = CFNumber::new_i32(i32::from_be_bytes(*b"BGRA"));
    let keys = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfacePixelFormat,
        ]
    };
    let properties = CFDictionary::from_slices(&keys, &[&*width, &*height, &*stride, &*format]);
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }
        .ok_or("cannot create DXVK IOSurface")?;
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            extent.width as usize,
            extent.height as usize,
            false,
        )
    };
    descriptor.setStorageMode(MTLStorageMode::Shared);
    descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
    let texture = device
        .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
        .ok_or("cannot create Metal view of DXVK IOSurface")?;
    Ok((surface, texture))
}

struct ReadImage {
    pixels: Vec<u8>,
    extent: Extent2D,
}

impl ReadImage {
    fn new(texture: Option<&Texture>) -> Result<Self, String> {
        let Some(texture) = texture else {
            return Ok(Self {
                pixels: Vec::new(),
                extent: Extent2D::new(0, 0),
            });
        };
        if texture.pixelFormat() != MTLPixelFormat::RGBA8Unorm
            || texture.storageMode() != MTLStorageMode::Shared
        {
            return Err("DXVK mask and user inputs must be shared RGBA8 textures".into());
        }
        let extent = Extent2D::new(texture.width() as u32, texture.height() as u32);
        let mut pixels = vec![0; extent.rgba8_len().ok_or("DXVK texture size overflow")?];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: texture.width(),
                height: texture.height(),
                depth: 1,
            },
        };
        unsafe {
            texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::from(&mut pixels[0]).cast(),
                texture.width() * 4,
                region,
                0,
            )
        };
        Ok(Self { pixels, extent })
    }

    fn as_image(&self) -> Image {
        Image {
            rgba: if self.pixels.is_empty() {
                std::ptr::null()
            } else {
                self.pixels.as_ptr()
            },
            width: self.extent.width,
            height: self.extent.height,
        }
    }
}
