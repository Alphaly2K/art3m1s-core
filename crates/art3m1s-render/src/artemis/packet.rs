use super::frame::{
    FrameGroup, FrameShader, FrameShaderEffect, FrameShaderLanguage, FrameSprite, FrameTransition,
    SpriteBlend,
};
use super::wire::*;
use crate::draw::NativeEmoteMaterial;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePacketError {
    Invalid,
    Limit,
}

impl fmt::Display for FramePacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid ARFP frame packet",
            Self::Limit => "ARFP frame packet exceeds the size limit",
        })
    }
}

impl std::error::Error for FramePacketError {}

/// A texture in an ARFP packet. Pixel bytes are supplied separately by the Host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTextureDescriptor {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
}

impl FrameTextureDescriptor {
    pub fn rgba_len(self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)
    }
}

/// Validated drawing instructions from an ABI frame borrow, without any texture pixels.
#[derive(Debug)]
pub struct FramePacket {
    pub session_epoch: u64,
    pub render_frame_id: u64,
    pub host_frame_id: u64,
    pub stage_size: [u32; 2],
    pub(super) sprites: Box<[FrameSprite]>,
    pub(super) groups: Box<[FrameGroup]>,
    pub(super) masks: Box<[FrameSprite]>,
    pub(super) transition: Option<FrameTransition>,
    textures: Box<[FrameTextureDescriptor]>,
}

impl FramePacket {
    pub fn textures(&self) -> &[FrameTextureDescriptor] {
        &self.textures
    }

    pub fn sprites(&self) -> &[FrameSprite] {
        &self.sprites
    }

    pub fn groups(&self) -> &[FrameGroup] {
        &self.groups
    }

    pub fn masks(&self) -> &[FrameSprite] {
        &self.masks
    }

    pub fn transition(&self) -> Option<&FrameTransition> {
        self.transition.as_ref()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FramePacketError> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(FramePacketError::Limit);
        }
        let mut packet = Reader(bytes);
        if packet.take(4)? != b"ARFP"
            || packet.u16()? != FRAME_PACKET_VERSION
            || packet.u16()? as usize != HEADER
            || packet.index()? != bytes.len()
        {
            return Err(invalid());
        }
        let texture_count = packet.index()?;
        let records = packet.index()?;
        if packet.u32()? != 0 {
            return Err(invalid());
        }
        let session_epoch = packet.u64()?;
        let render_frame_id = packet.u64()?;
        let host_frame_id = packet.u64()?;
        let stage_size = [packet.u32()?, packet.u32()?];
        if stage_size.contains(&0) {
            return Err(invalid());
        }
        let mut texture_table =
            Reader(packet.take(texture_count.checked_mul(TEXTURE).ok_or_else(invalid)?)?);
        if records > packet.0.len() / 8 {
            return Err(invalid());
        }
        let mut textures = Vec::with_capacity(texture_count);
        let mut revisions = HashSet::with_capacity(texture_count);
        for _ in 0..texture_count {
            let revision = texture_table.u64()?;
            let width = texture_table.u32()?;
            let height = texture_table.u32()?;
            let descriptor = FrameTextureDescriptor {
                revision,
                width,
                height,
            };
            let row = texture_table.u32()?;
            if descriptor.rgba_len().is_none_or(|len| len == 0)
                || width.checked_mul(4) != Some(row)
                || texture_table.u16()? != 1
                || texture_table.u16()? != 0
                || !revisions.insert(revision)
            {
                return Err(invalid());
            }
            textures.push(descriptor);
        }

        let mut sprites = Vec::new();
        let mut masks = Vec::new();
        let mut old_masks = Vec::new();
        let mut groups = Vec::new();
        let mut old_groups = Vec::new();
        let mut transition: Option<(usize, f32, Option<usize>, f32)> = None;
        let mut shaders = BTreeMap::new();
        let mut phase = 0;
        for index in 0..records {
            let len = packet.index()?.checked_sub(4).ok_or_else(invalid)?;
            let mut record = Reader(packet.take(len)?);
            let opcode = record.u16()?;
            let flags = record.u16()?;
            let old_count = transition.map_or(0, |(count, ..)| count);
            let next_phase = match opcode {
                1 | 4 if flags & 16 == 0 => 0,
                1 | 4 => 1,
                2 if index == 0 => 0,
                6 => 2,
                7 if flags & 2 == 0 => 3,
                3 | 5 | 7 => 4,
                _ => return Err(invalid()),
            };
            if next_phase < phase || (next_phase > 0 && old_count > sprites.len()) {
                return Err(invalid());
            }
            phase = next_phase;
            match opcode {
                1 | 4 => {
                    let sprite = record.sprite(opcode, flags, texture_count)?;
                    if flags & 32 != 0 {
                        if transition.is_none() || flags & 16 == 0 || !masks.is_empty() {
                            return Err(invalid());
                        }
                        old_masks.push(sprite);
                    } else if flags & 16 != 0 {
                        masks.push(sprite);
                    } else {
                        sprites.push(sprite);
                    }
                }
                2 => {
                    if flags != 0 {
                        return Err(invalid());
                    }
                    let count = record.index()?;
                    let progress = record.float()?;
                    let rule = record.texture(texture_count)?;
                    let vague = record.float()?;
                    if !(0.0..=1.0).contains(&progress) || vague < 0.0 {
                        return Err(invalid());
                    }
                    transition = Some((count, progress, rule, vague));
                }
                6 => {
                    let language = match flags {
                        0 => FrameShaderLanguage::Hlsl,
                        1 => FrameShaderLanguage::Glsl,
                        _ => return Err(invalid()),
                    };
                    let revision = record.u64()?;
                    let source = record.take(record.0.len())?;
                    if shaders
                        .insert(
                            revision,
                            Arc::new(FrameShader {
                                revision,
                                language,
                                source: source.into(),
                            }),
                        )
                        .is_some()
                    {
                        return Err(invalid());
                    }
                }
                3 | 5 | 7 => {
                    let old = flags & 16 != 0;
                    if old && transition.is_none() {
                        return Err(invalid());
                    }
                    let shader = if opcode == 7 {
                        Some(shaders.get(&record.u64()?).ok_or_else(invalid)?.clone())
                    } else {
                        None
                    };
                    let range = record.range(if old {
                        old_count
                    } else {
                        sprites.len() - old_count
                    })?;
                    let mut group = default_group(range.clone());
                    match opcode {
                        3 => {
                            check_flags(flags, 31)?;
                            group.mask = record.texture(texture_count)?;
                            group.blend = record.blend(false)?;
                            group.opacity = record.float()?;
                            group.color_multiply = record.floats()?;
                            group.clip_bounds = record.clip(flags)?;
                            group.grayscale = flags & 2 != 0;
                            group.negative = flags & 4 != 0;
                            group.opaque = flags & 8 != 0;
                        }
                        5 => {
                            check_flags(flags, 17)?;
                            group.stencil = Some(record.range(if old {
                                old_masks.len()
                            } else {
                                masks.len()
                            })?);
                            group.clip_bounds = record.clip(flags)?;
                        }
                        7 => {
                            check_flags(flags, 19)?;
                            let mask = record.texture(texture_count)?;
                            let user = record.texture(texture_count)?;
                            let uniform_count = record.index()?;
                            group.clip_bounds = record.clip(flags)?;
                            let effect = FrameShaderEffect {
                                shader: shader.unwrap(),
                                mask,
                                user,
                                uniforms: record.uniforms(uniform_count)?,
                            };
                            if flags & 2 == 0 {
                                if range.len() != 1 || group.clip_bounds.is_some() {
                                    return Err(invalid());
                                }
                                let sprite =
                                    &mut sprites[range.start + if old { 0 } else { old_count }];
                                if sprite.shader.replace(effect).is_some() {
                                    return Err(invalid());
                                }
                                record.finish()?;
                                continue;
                            }
                            group.shader = Some(effect);
                        }
                        _ => unreachable!(),
                    }
                    if old {
                        old_groups.push(group);
                    } else {
                        groups.push(group);
                    }
                }
                _ => unreachable!(),
            }
            record.finish()?;
        }
        packet.finish()?;
        let transition = if let Some((count, progress, rule, vague)) = transition {
            if count > sprites.len() {
                return Err(invalid());
            }
            let current = sprites.split_off(count);
            let source = std::mem::replace(&mut sprites, current).into_boxed_slice();
            Some(FrameTransition {
                source,
                source_groups: old_groups.into_boxed_slice(),
                source_masks: old_masks.into_boxed_slice(),
                progress,
                rule,
                vague,
            })
        } else {
            None
        };
        Ok(Self {
            session_epoch,
            render_frame_id,
            host_frame_id,
            stage_size,
            sprites: sprites.into_boxed_slice(),
            groups: groups.into_boxed_slice(),
            masks: masks.into_boxed_slice(),
            transition,
            textures: textures.into_boxed_slice(),
        })
    }
}

fn default_group(sprites: Range<usize>) -> FrameGroup {
    FrameGroup {
        sprites,
        mask: None,
        clip_bounds: None,
        opacity: 1.0,
        color_multiply: [1.0; 3],
        grayscale: false,
        negative: false,
        opaque: false,
        blend: SpriteBlend::Alpha,
        stencil: None,
        shader: None,
    }
}

fn invalid() -> FramePacketError {
    FramePacketError::Invalid
}

fn check_flags(flags: u16, allowed: u16) -> Result<(), FramePacketError> {
    if flags & !allowed != 0 {
        Err(invalid())
    } else {
        Ok(())
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8], FramePacketError> {
        let (value, rest) = self.0.split_at_checked(size).ok_or_else(invalid)?;
        self.0 = rest;
        Ok(value)
    }

    fn finish(self) -> Result<(), FramePacketError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    fn u16(&mut self) -> Result<u16, FramePacketError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, FramePacketError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, FramePacketError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn index(&mut self) -> Result<usize, FramePacketError> {
        Ok(self.u32()? as usize)
    }

    fn float(&mut self) -> Result<f32, FramePacketError> {
        let value = f32::from_bits(self.u32()?);
        value.is_finite().then_some(value).ok_or_else(invalid)
    }

    fn floats<const N: usize>(&mut self) -> Result<[f32; N], FramePacketError> {
        let mut values = [0.0; N];
        for value in &mut values {
            *value = self.float()?;
        }
        Ok(values)
    }

    fn clip(&mut self, flags: u16) -> Result<Option<[f32; 4]>, FramePacketError> {
        let clip = self.floats()?;
        Ok((flags & 1 != 0).then_some(clip))
    }

    fn texture(&mut self, count: usize) -> Result<Option<usize>, FramePacketError> {
        let index = self.u32()?;
        if index == NO_TEXTURE {
            Ok(None)
        } else if (index as usize) < count {
            Ok(Some(index as usize))
        } else {
            Err(invalid())
        }
    }

    fn blend(&mut self, emote: bool) -> Result<SpriteBlend, FramePacketError> {
        Ok(match self.u32()? {
            0 => SpriteBlend::Alpha,
            1 => SpriteBlend::Add,
            2 => SpriteBlend::Screen,
            3 => SpriteBlend::Multiply,
            4 if emote => SpriteBlend::EmoteAdd,
            5 if emote => SpriteBlend::EmoteReverseSubtract,
            6 if emote => SpriteBlend::EmoteMultiply,
            7 if emote => SpriteBlend::EmoteScreen,
            _ => return Err(invalid()),
        })
    }

    fn range(&mut self, len: usize) -> Result<Range<usize>, FramePacketError> {
        let start = self.index()?;
        let end = self.index()?;
        if start >= end || end > len {
            Err(invalid())
        } else {
            Ok(start..end)
        }
    }

    fn string(&mut self, size: usize) -> Result<String, FramePacketError> {
        let string = std::str::from_utf8(self.take(size)?).map_err(|_| invalid())?;
        if string.contains('\0') {
            return Err(invalid());
        }
        Ok(string.to_owned())
    }

    fn uniforms(&mut self, count: usize) -> Result<BTreeMap<String, Vec<f32>>, FramePacketError> {
        let mut uniforms = BTreeMap::new();
        for _ in 0..count {
            let name_len = self.u16()? as usize;
            let values_count = self.u16()? as usize;
            let name = self.string(name_len)?;
            let mut values = Reader(self.take(values_count * 4)?);
            let values = (0..values_count)
                .map(|_| values.float())
                .collect::<Result<_, _>>()?;
            if uniforms.insert(name, values).is_some() {
                return Err(invalid());
            }
        }
        Ok(uniforms)
    }

    fn sprite(
        &mut self,
        opcode: u16,
        flags: u16,
        textures: usize,
    ) -> Result<FrameSprite, FramePacketError> {
        check_flags(flags, if opcode == 4 { 63 } else { 7 })?;
        let texture = self.texture(textures)?.ok_or_else(invalid)?;
        let mask = self.texture(textures)?;
        let blend = self.blend(opcode == 4)?;
        let id_len = self.index()?;
        let mut sprite = FrameSprite {
            texture,
            mask,
            blend,
            layer_id: String::new(),
            transform: self.floats()?,
            quad_size: self.floats()?,
            uv_offset: self.floats()?,
            uv_scale: self.floats()?,
            clip_bounds: self.clip(flags)?,
            opacity: self.float()?,
            color_multiply: self.floats()?,
            grayscale: flags & 2 != 0,
            negative: flags & 4 != 0,
            mesh: None,
            emote_material: None,
            shader: None,
        };
        sprite.layer_id = self.string(id_len)?;
        if opcode == 4 {
            let count = self.index()?;
            if flags & 8 != 0 {
                sprite.emote_material = Some(NativeEmoteMaterial {
                    corner_colors: [
                        self.floats()?,
                        self.floats()?,
                        self.floats()?,
                        self.floats()?,
                    ],
                    uv_rect: self.floats()?,
                    blend_mode: self.u32()?,
                    clip_rect: self.floats()?,
                    wipe: self.floats()?,
                });
            }
            if !count.is_multiple_of(3) || count.checked_mul(16) != Some(self.0.len()) {
                return Err(invalid());
            }
            if count != 0 {
                sprite.mesh = Some(
                    (0..count)
                        .map(|_| self.floats::<4>())
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                );
            }
        }
        Ok(sprite)
    }
}
