use super::{FrameSnapshot, SpriteBlend};
use crate::host::protocol::ProtocolError;

pub const FRAME_PACKET_VERSION: u16 = 1;
const HEADER: usize = 56;
const TEXTURE: usize = 24;
const SPRITE: usize = 104;
const TRANSITION: usize = 24;
const GROUP: usize = 56;
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
    let sprites = source.iter().chain(&frame.sprites);
    let group_count = source_groups
        .len()
        .checked_add(frame.groups.len())
        .ok_or(ProtocolError::Limit)?;
    let start = start
        .checked_add(group_count.checked_mul(GROUP).ok_or(ProtocolError::Limit)?)
        .and_then(|start| start.checked_add(if transition.is_some() { TRANSITION } else { 0 }))
        .ok_or(ProtocolError::Limit)?;
    let size = sprites
        .clone()
        .try_fold(start, |size, sprite| {
            size.checked_add(SPRITE)?.checked_add(sprite.layer_id.len())
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
        (source.len() + frame.sprites.len() + group_count + usize::from(transition.is_some()))
            as u32,
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
    for sprite in sprites {
        if sprite.texture >= frame.textures.len()
            || sprite.mask.is_some_and(|mask| mask >= frame.textures.len())
        {
            return Err(ProtocolError::State);
        }
        put_u32(&mut out, (SPRITE + sprite.layer_id.len()) as u32);
        put_u16(&mut out, 1); // Sprite.
        let flags = u16::from(sprite.clip_bounds.is_some())
            | (u16::from(sprite.grayscale) << 1)
            | (u16::from(sprite.negative) << 2);
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
    }
    for (is_source, len, groups) in [
        (true, source.len(), source_groups),
        (false, frame.sprites.len(), &frame.groups[..]),
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

fn blend_id(blend: SpriteBlend) -> u32 {
    match blend {
        SpriteBlend::Alpha => 0,
        SpriteBlend::Add => 1,
        SpriteBlend::Screen => 2,
        SpriteBlend::Multiply => 3,
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
