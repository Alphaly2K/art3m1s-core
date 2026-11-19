mod shared_surface;

use art3m1s_render::artemis::{
    FramePacket, FramePlan, FrameShaderEffect, FrameShaderLanguage, SpriteRenderer,
};
use art3m1s_render::backend::{FrameTarget, GpuBackend, metal::MetalBackend};
use art3m1s_render::{Extent2D, NativeSurface, NativeSurfaceKind};
use shared_surface::SharedSurface;
use std::{
    ffi::{CString, c_char, c_void},
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    ptr::{self, NonNull},
};

#[repr(C)]
struct Image {
    rgba: *const u8,
    width: u32,
    height: u32,
}

#[repr(C)]
struct Uniform {
    name: *const u8,
    name_length: usize,
    values: *const f32,
    count: usize,
}

unsafe extern "C" {
    fn art_dxvk_pass_create(
        effect: *const c_char,
        foreground: *mut c_void,
        output: *mut c_void,
    ) -> *mut c_void;
    fn art_dxvk_pass_metal_device(pass: *mut c_void) -> *mut c_void;
    fn art_dxvk_pass_draw(
        pass: *mut c_void,
        mask: Image,
        user: Image,
        uniforms: *const Uniform,
        count: usize,
    ) -> i32;
    fn art_dxvk_pass_destroy(pass: *mut c_void);
    fn art_metal_writer_create(
        device: *mut c_void,
        output: *mut c_void,
        width: u32,
        height: u32,
    ) -> *mut c_void;
    fn art_metal_writer_copy(writer: *mut c_void, source: *mut c_void) -> bool;
    fn art_metal_writer_destroy(writer: *mut c_void);
}

struct Pass(NonNull<c_void>);
impl Drop for Pass {
    fn drop(&mut self) {
        unsafe { art_dxvk_pass_destroy(self.0.as_ptr()) }
    }
}

struct Presenter(NonNull<c_void>);
impl Drop for Presenter {
    fn drop(&mut self) {
        unsafe { art_metal_writer_destroy(self.0.as_ptr()) }
    }
}

struct Scene<'a> {
    packet: FramePacket,
    textures: Vec<&'a [u8]>,
}

impl<'a> Scene<'a> {
    fn read(data: &mut &'a [u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let length = u32::from_le_bytes(data.get(8..12).ok_or("truncated ARFP header")?.try_into()?)
            as usize;
        let (packet, mut pixels) = data
            .split_at_checked(length)
            .ok_or("truncated ARFP packet")?;
        let packet = FramePacket::decode(packet)?;
        let mut textures = Vec::with_capacity(packet.textures().len());
        for descriptor in packet.textures() {
            let (bytes, rest) = pixels
                .split_at_checked(descriptor.rgba_len().ok_or("invalid ARFP extent")?)
                .ok_or("truncated ARFP texture")?;
            textures.push(bytes);
            pixels = rest;
        }
        *data = pixels;
        Ok(Self { packet, textures })
    }

    fn effect(&self) -> &FrameShaderEffect {
        let [group] = self.packet.groups() else {
            panic!(
                "expected one ARFP group, got {}",
                self.packet.groups().len()
            )
        };
        assert_eq!(group.sprites, 0..self.packet.sprites().len());
        assert!(self.packet.masks().is_empty() && self.packet.transition().is_none());
        let effect = group.shader.as_ref().expect("group has no ARFP effect");
        assert_eq!(effect.shader.language, FrameShaderLanguage::Hlsl);
        effect
    }

    fn image(&self, index: Option<usize>) -> Image {
        if let Some(index) = index {
            let descriptor = self.packet.textures()[index];
            Image {
                rgba: self.textures[index].as_ptr(),
                width: descriptor.width,
                height: descriptor.height,
            }
        } else {
            Image {
                rgba: ptr::null(),
                width: 0,
                height: 0,
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let data = fs::read(args.first().ok_or("expected the ARFP fixture path")?)?;
    let mut data = data.as_slice();
    let first = Scene::read(&mut data)?;
    if args.len() == 1 {
        std::io::stdout().write_all(&first.effect().shader.source)?;
        return Ok(());
    }
    let [_, effect] = args.as_slice() else {
        return Err("expected ARFP and ADE2 paths".into());
    };
    let scenes = [first, Scene::read(&mut data)?, Scene::read(&mut data)?];
    assert!(data.is_empty(), "trailing fixture bytes");
    let extent = Extent2D::new(4, 4);
    let foreground = SharedSurface::new(extent)?;
    let output = SharedSurface::new(extent)?;
    let final_surface = SharedSurface::new(extent)?;
    let foreground_handle = foreground.handle()?.as_ptr();
    let output_handle = output.handle()?.as_ptr();
    let effect = CString::new(effect.as_bytes())?;
    // Initialize the D3D11 image layout before Metal first writes the foreground.
    let pass = Pass(
        NonNull::new(unsafe {
            art_dxvk_pass_create(effect.as_ptr(), foreground_handle, output_handle)
        })
        .ok_or("cannot create native DXVK pass")?,
    );
    let presenter = Presenter(
        NonNull::new(unsafe {
            art_metal_writer_create(
                art_dxvk_pass_metal_device(pass.0.as_ptr()),
                final_surface.handle()?.as_ptr(),
                extent.width,
                extent.height,
            )
        })
        .ok_or("cannot create Metal presentation texture")?,
    );
    let mut renderer = SpriteRenderer::new(MetalBackend::new(extent.width, extent.height)?);
    renderer.backend_mut().set_native_surface(NativeSurface {
        kind: NativeSurfaceKind::AppleIoSurface,
        handle: foreground_handle,
        extent,
    })?;

    let expected = [
        [[5, 24, 24, 255], [5, 41, 37, 255]],
        [[0, 17, 13, 255], [0, 34, 26, 255]],
        [[0, 34, 26, 255], [0, 17, 13, 255]],
    ];
    for (index, scene) in scenes.iter().enumerate() {
        let packet = &scene.packet;
        assert_eq!(packet.stage_size, [4, 4]);
        assert_eq!(
            scene.effect().shader,
            scenes[0].effect().shader,
            "effect changed between frames"
        );
        renderer.backend_mut().begin_frame(FrameTarget::Main)?;
        let draw = renderer.render_plan(
            FramePlan {
                runtime: 1,
                epoch: packet.session_epoch,
                stage_size: packet.stage_size,
                sprites: packet.sprites(),
                groups: &[],
                masks: &[],
                transition: None,
            },
            |i| packet.textures().get(i).copied(),
            |i| Ok(scene.textures[i]),
            [0.0; 4],
        );
        renderer.backend_mut().end_frame();
        draw?;
        renderer.backend_mut().present(None)?;

        let effect = scene.effect();
        assert!(effect.mask.is_some() && effect.user.is_some());
        let uniforms = effect
            .uniforms
            .iter()
            .map(|(name, values)| Uniform {
                name: name.as_ptr(),
                name_length: name.len(),
                values: values.as_ptr(),
                count: values.len(),
            })
            .collect::<Vec<_>>();
        let result = unsafe {
            art_dxvk_pass_draw(
                pass.0.as_ptr(),
                scene.image(effect.mask),
                scene.image(effect.user),
                uniforms.as_ptr(),
                uniforms.len(),
            )
        };
        assert_eq!(result, 0, "DXVK pass failed on frame {index}");
        assert!(unsafe { art_metal_writer_copy(presenter.0.as_ptr(), output_handle) });
        let rendered = final_surface.pixels()?;
        for (pixel, rgba) in rendered.chunks_exact(4).enumerate() {
            if rgba != expected[index][pixel / 8] {
                return Err(format!(
                    "ARFP frame {index}, pixel {pixel}: got {rgba:?}, expected {:?}; final {rendered:?}; DXVK {:?}",
                    expected[index][pixel / 8], output.pixels()?,
                ).into());
            }
        }
    }
    println!("Native DXVK presented three ARFP frames with updated constants and layer transforms");
    Ok(())
}
