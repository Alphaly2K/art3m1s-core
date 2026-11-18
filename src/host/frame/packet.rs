use super::{FrameShaderEffect, FrameSnapshot, FrameSprite, SpriteBlend};
use crate::host::protocol::ProtocolError;
use std::collections::BTreeMap;

mod decode;
pub use decode::{FramePacket, FrameTextureDescriptor};

pub const FRAME_PACKET_VERSION: u16 = 1;
const HEADER: usize = 56;
const TEXTURE: usize = 24;
const SPRITE: usize = 104;
const TRANSITION: usize = 24;
const GROUP: usize = 56;
const STENCIL: usize = 40;
const EMOTE_MATERIAL: usize = 112;
const SHADER: usize = 16;
const EFFECT: usize = 52;
const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
const NO_TEXTURE: u32 = u32::MAX;

pub(super) fn encode(frame: &FrameSnapshot) -> Result<Box<[u8]>, ProtocolError> {
    let texture_bytes = frame
        .textures
        .len()
        .checked_mul(TEXTURE)
        .ok_or(ProtocolError::Limit)?;
    let start = HEADER
        .checked_add(texture_bytes)
        .ok_or(ProtocolError::Limit)?;
    let transition = frame.transition.as_ref();
    let source = transition.map_or(&[][..], |transition| &transition.source);
    let source_groups = transition.map_or(&[][..], |transition| &transition.source_groups);
    let source_masks = transition.map_or(&[][..], |transition| &transition.source_masks);
    let sprite_effects = [(true, source), (false, &frame.sprites[..])]
        .into_iter()
        .flat_map(|(old, sprites)| {
            sprites
                .iter()
                .enumerate()
                .filter_map(move |(index, sprite)| Some((old, index, sprite.shader.as_ref()?)))
        })
        .collect::<Vec<_>>();
    let shaders = sprite_effects
        .iter()
        .map(|(_, _, effect)| *effect)
        .chain(
            source_groups
                .iter()
                .chain(&frame.groups)
                .filter_map(|group| group.shader.as_ref()),
        )
        .map(|effect| (effect.shader.revision, effect.shader.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let sprites = source
        .iter()
        .chain(&frame.sprites)
        .map(|sprite| (sprite, 0))
        .chain(source_masks.iter().map(|sprite| (sprite, 0x30)))
        .chain(frame.masks.iter().map(|sprite| (sprite, 0x10)));
    let group_count = source_groups
        .len()
        .checked_add(frame.groups.len())
        .ok_or(ProtocolError::Limit)?;
    let start = source_groups
        .iter()
        .chain(&frame.groups)
        .try_fold(start, |size, group| {
            size.checked_add(if let Some(effect) = &group.shader {
                effect_size(effect)?
            } else if group.stencil.is_some() {
                STENCIL
            } else {
                GROUP
            })
        })
        .and_then(|size| {
            shaders.values().try_fold(size, |size, shader| {
                size.checked_add(SHADER)?.checked_add(shader.source.len())
            })
        })
        .and_then(|size| {
            sprite_effects
                .iter()
                .try_fold(size, |size, (_, _, effect)| {
                    size.checked_add(effect_size(effect)?)
                })
        })
        .and_then(|size| size.checked_add(if transition.is_some() { TRANSITION } else { 0 }))
        .ok_or(ProtocolError::Limit)?;
    let size = sprites
        .clone()
        .try_fold(start, |size, (sprite, flags)| {
            size.checked_add(sprite_size(sprite, flags)?)
        })
        .filter(|&size| size <= MAX_PACKET_BYTES)
        .ok_or(ProtocolError::Limit)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| ProtocolError::Limit)?;
    out.extend_from_slice(b"ARFP");
    put_u16(&mut out, FRAME_PACKET_VERSION);
    put_u16(&mut out, HEADER as u16);
    put_u32(&mut out, size as u32);
    put_u32(&mut out, frame.textures.len() as u32);
    put_u32(
        &mut out,
        (source.len()
            + frame.sprites.len()
            + source_masks.len()
            + frame.masks.len()
            + group_count
            + shaders.len()
            + sprite_effects.len()
            + usize::from(transition.is_some())) as u32,
    );
    put_u32(&mut out, 0);
    put_u64(&mut out, frame.session_epoch);
    put_u64(&mut out, frame.render_frame_id);
    put_u64(&mut out, frame.host_frame_id);
    for dimension in frame.stage_size {
        put_u32(&mut out, dimension);
    }
    for texture in &frame.textures {
        let row_bytes = texture.width.checked_mul(4).ok_or(ProtocolError::Limit)?;
        if u64::from(row_bytes) * u64::from(texture.height) != texture.rgba().len() as u64 {
            return Err(ProtocolError::State);
        }
        put_u64(&mut out, texture.revision);
        put_u32(&mut out, texture.width);
        put_u32(&mut out, texture.height);
        put_u32(&mut out, row_bytes);
        put_u16(&mut out, 1); // RGBA8 straight, top row first.
        put_u16(&mut out, 0);
    }
    if let Some(transition) = transition {
        put_u32(&mut out, TRANSITION as u32);
        put_u16(&mut out, 2);
        put_u16(&mut out, 0);
        put_u32(&mut out, source.len() as u32);
        put_u32(&mut out, transition.progress.to_bits());
        put_u32(
            &mut out,
            transition.rule.map_or(NO_TEXTURE, |rule| rule as u32),
        );
        put_u32(&mut out, transition.vague.to_bits());
    }
    for (sprite, pass_flags) in sprites {
        if sprite.texture >= frame.textures.len()
            || sprite.mask.is_some_and(|mask| mask >= frame.textures.len())
        {
            return Err(ProtocolError::State);
        }
        let extended = is_extended(sprite, pass_flags);
        put_u32(
            &mut out,
            sprite_size(sprite, pass_flags).ok_or(ProtocolError::Limit)? as u32,
        );
        put_u16(&mut out, if extended { 4 } else { 1 });
        let flags = u16::from(sprite.clip_bounds.is_some())
            | (u16::from(sprite.grayscale) << 1)
            | (u16::from(sprite.negative) << 2)
            | (u16::from(sprite.emote_material.is_some()) << 3)
            | pass_flags;
        put_u16(&mut out, flags);
        put_u32(&mut out, sprite.texture as u32);
        put_u32(&mut out, sprite.mask.map_or(NO_TEXTURE, |mask| mask as u32));
        put_u32(&mut out, blend_id(sprite.blend));
        put_u32(&mut out, sprite.layer_id.len() as u32);
        for value in sprite
            .transform
            .into_iter()
            .chain(sprite.quad_size)
            .chain(sprite.uv_offset)
            .chain(sprite.uv_scale)
            .chain(sprite.clip_bounds.unwrap_or_default())
            .chain([sprite.opacity])
            .chain(sprite.color_multiply)
        {
            put_u32(&mut out, value.to_bits());
        }
        out.extend_from_slice(sprite.layer_id.as_bytes());
        if extended {
            let vertices = sprite.mesh.as_deref().unwrap_or_default();
            if !vertices.len().is_multiple_of(3)
                || vertices.iter().flatten().any(|value| !value.is_finite())
            {
                return Err(ProtocolError::State);
            }
            put_u32(&mut out, vertices.len() as u32);
            if let Some(material) = &sprite.emote_material {
                if !material
                    .corner_colors
                    .into_iter()
                    .flatten()
                    .chain(material.uv_rect)
                    .chain(material.clip_rect)
                    .chain(material.wipe)
                    .all(f32::is_finite)
                {
                    return Err(ProtocolError::State);
                }
                for value in material
                    .corner_colors
                    .into_iter()
                    .flatten()
                    .chain(material.uv_rect)
                {
                    put_u32(&mut out, value.to_bits());
                }
                put_u32(&mut out, material.blend_mode);
                for value in material.clip_rect.into_iter().chain(material.wipe) {
                    put_u32(&mut out, value.to_bits());
                }
            }
            for value in vertices.iter().flatten() {
                put_u32(&mut out, value.to_bits());
            }
        }
    }
    for shader in shaders.values() {
        put_u32(&mut out, (SHADER + shader.source.len()) as u32);
        put_u16(&mut out, 6);
        put_u16(&mut out, 0);
        put_u64(&mut out, shader.revision);
        out.extend_from_slice(&shader.source);
    }
    for (old, index, effect) in sprite_effects {
        write_effect(
            &mut out,
            frame,
            effect,
            index..index + 1,
            u16::from(old) << 4,
            None,
        )?;
    }
    for (is_source, len, masks, groups) in [
        (true, source.len(), source_masks.len(), source_groups),
        (
            false,
            frame.sprites.len(),
            frame.masks.len(),
            &frame.groups[..],
        ),
    ] {
        for group in groups {
            if group.sprites.start >= group.sprites.end
                || group.sprites.end > len
                || group
                    .mask
                    .is_some_and(|index| index >= frame.textures.len())
            {
                return Err(ProtocolError::State);
            }
            if let Some(effect) = &group.shader {
                let flags =
                    u16::from(group.clip_bounds.is_some()) | 2 | (u16::from(is_source) << 4);
                write_effect(
                    &mut out,
                    frame,
                    effect,
                    group.sprites.clone(),
                    flags,
                    group.clip_bounds,
                )?;
                continue;
            }
            if let Some(stencil) = &group.stencil {
                if stencil.start >= stencil.end || stencil.end > masks {
                    return Err(ProtocolError::State);
                }
                put_u32(&mut out, STENCIL as u32);
                put_u16(&mut out, 5);
                put_u16(
                    &mut out,
                    u16::from(group.clip_bounds.is_some()) | (u16::from(is_source) << 4),
                );
                put_u32(&mut out, group.sprites.start as u32);
                put_u32(&mut out, group.sprites.end as u32);
                put_u32(&mut out, stencil.start as u32);
                put_u32(&mut out, stencil.end as u32);
                for value in group.clip_bounds.unwrap_or_default() {
                    put_u32(&mut out, value.to_bits());
                }
                continue;
            }
            put_u32(&mut out, GROUP as u32);
            put_u16(&mut out, 3);
            let flags = u16::from(group.clip_bounds.is_some())
                | (u16::from(group.grayscale) << 1)
                | (u16::from(group.negative) << 2)
                | (u16::from(group.opaque) << 3)
                | (u16::from(is_source) << 4);
            put_u16(&mut out, flags);
            put_u32(&mut out, group.sprites.start as u32);
            put_u32(&mut out, group.sprites.end as u32);
            put_u32(&mut out, group.mask.map_or(NO_TEXTURE, |mask| mask as u32));
            put_u32(&mut out, blend_id(group.blend));
            for value in [group.opacity]
                .into_iter()
                .chain(group.color_multiply)
                .chain(group.clip_bounds.unwrap_or_default())
            {
                put_u32(&mut out, value.to_bits());
            }
        }
    }
    debug_assert_eq!(out.len(), size);
    Ok(out.into_boxed_slice())
}

fn effect_size(effect: &FrameShaderEffect) -> Option<usize> {
    effect
        .uniforms
        .iter()
        .try_fold(EFFECT, |size, (name, values)| {
            size.checked_add(4)?
                .checked_add(name.len())?
                .checked_add(values.len().checked_mul(4)?)
        })
}

fn write_effect(
    out: &mut Vec<u8>,
    frame: &FrameSnapshot,
    effect: &FrameShaderEffect,
    range: std::ops::Range<usize>,
    flags: u16,
    clip: Option<[f32; 4]>,
) -> Result<(), ProtocolError> {
    if [effect.mask, effect.user]
        .into_iter()
        .flatten()
        .any(|index| index >= frame.textures.len())
    {
        return Err(ProtocolError::State);
    }
    put_u32(out, effect_size(effect).ok_or(ProtocolError::Limit)? as u32);
    put_u16(out, 7);
    put_u16(out, flags);
    put_u64(out, effect.shader.revision);
    put_u32(out, range.start as u32);
    put_u32(out, range.end as u32);
    put_u32(out, effect.mask.map_or(NO_TEXTURE, |index| index as u32));
    put_u32(out, effect.user.map_or(NO_TEXTURE, |index| index as u32));
    put_u32(out, effect.uniforms.len() as u32);
    for value in clip.unwrap_or_default() {
        put_u32(out, value.to_bits());
    }
    for (name, values) in &effect.uniforms {
        put_u16(
            out,
            name.len().try_into().map_err(|_| ProtocolError::Limit)?,
        );
        put_u16(
            out,
            values.len().try_into().map_err(|_| ProtocolError::Limit)?,
        );
        out.extend_from_slice(name.as_bytes());
        for value in values {
            put_u32(out, value.to_bits());
        }
    }
    Ok(())
}

fn is_extended(sprite: &FrameSprite, pass_flags: u16) -> bool {
    sprite.mesh.is_some() || sprite.emote_material.is_some() || pass_flags != 0
}

fn sprite_size(sprite: &FrameSprite, pass_flags: u16) -> Option<usize> {
    let base = SPRITE.checked_add(sprite.layer_id.len())?;
    if !is_extended(sprite, pass_flags) {
        return Some(base);
    }
    let material = if sprite.emote_material.is_some() {
        EMOTE_MATERIAL
    } else {
        0
    };
    let vertices = sprite
        .mesh
        .as_deref()
        .map_or(0, <[_]>::len)
        .checked_mul(16)?;
    base.checked_add(4 + material)?.checked_add(vertices)
}

fn blend_id(blend: SpriteBlend) -> u32 {
    match blend {
        SpriteBlend::Alpha => 0,
        SpriteBlend::Add => 1,
        SpriteBlend::Screen => 2,
        SpriteBlend::Multiply => 3,
        SpriteBlend::EmoteAdd => 4,
        SpriteBlend::EmoteReverseSubtract => 5,
        SpriteBlend::EmoteMultiply => 6,
        SpriteBlend::EmoteScreen => 7,
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests;
