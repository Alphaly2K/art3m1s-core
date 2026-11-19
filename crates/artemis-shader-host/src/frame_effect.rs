use crate::{Draw, Error, Image, Program, Worker};
use art3m1s_render::artemis::{FrameShader, FrameShaderEffect, FrameShaderLanguage};
use std::io::{Read, Write};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum TextureRole {
    Foreground,
    Mask,
    User,
    Background,
}

/// A compiled HLSL effect and its reflected texture slots, owned by one worker.
pub struct FrameEffectProgram {
    program: Program,
    shader: Arc<FrameShader>,
    slots: [Option<TextureRole>; 4],
}

/// The Host supplies the offscreen target, quad, and pixels for the current draw.
pub struct FrameEffectDraw<'a> {
    pub width: u32,
    pub height: u32,
    pub alpha_blend: bool,
    pub point_sampler: bool,
    pub clear_rgba: [f32; 4],
    pub quad: [[f32; 6]; 4],
    pub foreground: Image<'a>,
    pub background: Option<Image<'a>>,
}

impl<R: Read, W: Write> Worker<R, W> {
    pub fn compile_frame_effect(
        &mut self,
        effect: &FrameShaderEffect,
    ) -> Result<FrameEffectProgram, Error> {
        if effect.shader.language != FrameShaderLanguage::Hlsl {
            return Err(Error::Argument("D3D11 frame effects require HLSL"));
        }
        let program = self.compile(&effect.shader.source)?;
        match self.frame_effect_slots(program) {
            Ok(slots) => Ok(FrameEffectProgram {
                program,
                shader: Arc::clone(&effect.shader),
                slots,
            }),
            Err(error) => {
                let _ = self.release(program);
                Err(error)
            }
        }
    }

    fn frame_effect_slots(&mut self, program: Program) -> Result<[Option<TextureRole>; 4], Error> {
        let mut slots = [None; 4];
        for (role, names) in [
            (TextureRole::Foreground, ["textureFore", "samplerFore"]),
            (TextureRole::Mask, ["textureMask", "samplerMask"]),
            (TextureRole::User, ["textureUser", "samplerUser"]),
            (TextureRole::Background, ["textureBack", "samplerBack"]),
        ] {
            for name in names {
                if let Some(binding) = self.texture_binding(program, name)? {
                    if binding.count != 1 {
                        return Err(Error::Argument(
                            "frame effect texture arrays are unsupported",
                        ));
                    }
                    slots[binding.slot as usize] = Some(role);
                }
            }
        }
        Ok(slots)
    }

    /// Draw with current constants and texture indices. `texture` supplies BGRA for
    /// a mask or user texture index in the current packet, when the shader uses it.
    pub fn draw_frame_effect<'a>(
        &mut self,
        program: &FrameEffectProgram,
        effect: &FrameShaderEffect,
        target: &FrameEffectDraw<'a>,
        mut texture: impl FnMut(usize) -> Option<Image<'a>>,
    ) -> Result<Vec<u8>, Error> {
        self.check_owner(program.program)?;
        if program.shader != effect.shader {
            return Err(Error::Argument("frame effect shader changed"));
        }
        let image = |bgra| Image {
            width: 1,
            height: 1,
            bgra,
        };
        let mut textures = [None; 4];
        for (slot, role) in program.slots.iter().enumerate() {
            textures[slot] = Some(match role {
                Some(TextureRole::Foreground) => target.foreground,
                Some(TextureRole::Mask | TextureRole::User) => {
                    let (index, fallback) = if matches!(role, Some(TextureRole::Mask)) {
                        (effect.mask, image(&[255; 4]))
                    } else {
                        (effect.user, image(&[0; 4]))
                    };
                    match index {
                        Some(index) => {
                            texture(index).ok_or(Error::Argument("missing frame effect texture"))?
                        }
                        None => fallback,
                    }
                }
                Some(TextureRole::Background) => target
                    .background
                    .ok_or(Error::Argument("missing frame effect background"))?,
                None => continue,
            });
        }
        let uniforms = effect
            .uniforms
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_slice()))
            .collect::<Vec<_>>();
        self.draw(
            program.program,
            &Draw {
                width: target.width,
                height: target.height,
                alpha_blend: target.alpha_blend,
                point_sampler: target.point_sampler,
                clear_rgba: target.clear_rgba,
                quad: target.quad,
                uniforms: &uniforms,
                textures,
            },
        )
    }

    /// Release the compiled program on its originating worker. The handle is
    /// no longer valid after a successful release.
    pub fn release_frame_effect(&mut self, program: &FrameEffectProgram) -> Result<(), Error> {
        self.release(program.program)
    }
}
