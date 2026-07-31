use std::collections::BTreeMap;

use crate::{EmoteError, PsbDocument, PsbResourceData, PsbValue, ResourceRef, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextureFormat {
    Dxt5,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteTexture {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub truncated_width: u32,
    pub truncated_height: u32,
    pub format: TextureFormat,
    pub resource: ResourceRef,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AtlasIcon {
    pub id: String,
    pub texture_id: String,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub attr: i64,
    pub z_order: i64,
}

#[derive(Clone, Debug, Default)]
pub struct EmoteAtlas {
    textures: BTreeMap<String, EmoteTexture>,
    icons: BTreeMap<String, AtlasIcon>,
}

impl EmoteAtlas {
    pub fn from_document(document: &PsbDocument) -> Result<Self> {
        let source = document
            .root
            .get("source")
            .and_then(PsbValue::as_object)
            .ok_or_else(|| EmoteError::InvalidFormat("motion PSB has no source table".into()))?;

        let mut atlas = Self::default();
        for (texture_id, source_value) in source {
            let texture_value = source_value.get("texture").ok_or_else(|| {
                EmoteError::InvalidFormat(format!("{texture_id} has no texture descriptor"))
            })?;
            let texture = parse_texture(texture_id, texture_value)?;
            validate_texture_resource(document, &texture)?;

            if let Some(icons) = source_value.get("icon").and_then(PsbValue::as_object) {
                for (icon_id, icon_value) in icons {
                    let icon = parse_icon(texture_id, icon_id, icon_value)?;
                    if atlas.icons.insert(icon_id.clone(), icon).is_some() {
                        return Err(EmoteError::InvalidFormat(format!(
                            "duplicate atlas icon {icon_id}"
                        )));
                    }
                }
            }
            atlas.textures.insert(texture_id.clone(), texture);
        }
        Ok(atlas)
    }

    pub fn textures(&self) -> &BTreeMap<String, EmoteTexture> {
        &self.textures
    }

    pub fn icons(&self) -> &BTreeMap<String, AtlasIcon> {
        &self.icons
    }

    pub fn texture(&self, id: &str) -> Option<&EmoteTexture> {
        self.textures.get(id)
    }

    pub fn icon(&self, id: &str) -> Option<&AtlasIcon> {
        self.icons.get(id)
    }

    pub fn compressed_texture<'a>(&self, document: &'a PsbDocument, id: &str) -> Result<&'a [u8]> {
        let texture = self
            .texture(id)
            .ok_or_else(|| EmoteError::InvalidFormat(format!("unknown texture {id}")))?;
        document.resource(texture.resource)
    }

    pub fn texture_data(&self, document: &PsbDocument, id: &str) -> Result<PsbResourceData> {
        let texture = self
            .texture(id)
            .ok_or_else(|| EmoteError::InvalidFormat(format!("unknown texture {id}")))?;
        document.resource_data(texture.resource)
    }

    pub fn decode_texture_rgba8(&self, document: &PsbDocument, id: &str) -> Result<Vec<u8>> {
        let texture = self
            .texture(id)
            .ok_or_else(|| EmoteError::InvalidFormat(format!("unknown texture {id}")))?;
        let bytes = document.resource(texture.resource)?;
        match texture.format {
            TextureFormat::Dxt5 => decode_dxt5(bytes, texture.width, texture.height),
            TextureFormat::Other(ref format) => {
                Err(EmoteError::Unsupported(format!("texture format {format}")))
            }
        }
    }

    pub fn decode_texture_data_rgba8(&self, id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let texture = self
            .texture(id)
            .ok_or_else(|| EmoteError::InvalidFormat(format!("unknown texture {id}")))?;
        match texture.format {
            TextureFormat::Dxt5 => decode_dxt5(bytes, texture.width, texture.height),
            TextureFormat::Other(ref format) => {
                Err(EmoteError::Unsupported(format!("texture format {format}")))
            }
        }
    }
}

fn parse_texture(id: &str, value: &PsbValue) -> Result<EmoteTexture> {
    let format = required_string(value, "type")?;
    Ok(EmoteTexture {
        id: id.to_owned(),
        width: required_u32(value, "width")?,
        height: required_u32(value, "height")?,
        truncated_width: required_u32(value, "truncated_width")?,
        truncated_height: required_u32(value, "truncated_height")?,
        format: if format.eq_ignore_ascii_case("DXT5") {
            TextureFormat::Dxt5
        } else {
            TextureFormat::Other(format)
        },
        resource: match value.get("pixel") {
            Some(PsbValue::Resource(reference)) => *reference,
            _ => {
                return Err(EmoteError::InvalidFormat(format!(
                    "{id} texture has no pixel resource"
                )));
            }
        },
    })
}

fn parse_icon(texture_id: &str, id: &str, value: &PsbValue) -> Result<AtlasIcon> {
    Ok(AtlasIcon {
        id: id.to_owned(),
        texture_id: texture_id.to_owned(),
        left: required_f32(value, "left")?,
        top: required_f32(value, "top")?,
        width: required_f32(value, "width")?,
        height: required_f32(value, "height")?,
        origin_x: required_f32(value, "originX")?,
        origin_y: required_f32(value, "originY")?,
        attr: required_i64(value, "attr")?,
        z_order: value
            .at_path(&["metadata", "zorder"])
            .and_then(PsbValue::as_i64)
            .unwrap_or(0),
    })
}

fn validate_texture_resource(document: &PsbDocument, texture: &EmoteTexture) -> Result<()> {
    let bytes = document.resource(texture.resource)?;
    if texture.format == TextureFormat::Dxt5 {
        let blocks_x = texture.width.div_ceil(4) as usize;
        let blocks_y = texture.height.div_ceil(4) as usize;
        let expected = blocks_x
            .checked_mul(blocks_y)
            .and_then(|blocks| blocks.checked_mul(16))
            .ok_or_else(|| EmoteError::InvalidFormat("DXT5 texture size overflow".into()))?;
        if bytes.len() != expected {
            return Err(EmoteError::InvalidFormat(format!(
                "{} DXT5 resource has {} bytes, expected {expected}",
                texture.id,
                bytes.len()
            )));
        }
    }
    Ok(())
}

fn required_string(value: &PsbValue, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(PsbValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| EmoteError::InvalidFormat(format!("missing string field {key}")))
}

fn required_i64(value: &PsbValue, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(PsbValue::as_i64)
        .ok_or_else(|| EmoteError::InvalidFormat(format!("missing integer field {key}")))
}

fn required_u32(value: &PsbValue, key: &str) -> Result<u32> {
    u32::try_from(required_i64(value, key)?)
        .map_err(|_| EmoteError::InvalidFormat(format!("field {key} is outside u32")))
}

fn required_f32(value: &PsbValue, key: &str) -> Result<f32> {
    match value.get(key) {
        Some(PsbValue::Integer(value)) => Ok(*value as f32),
        Some(PsbValue::Float(value)) => Ok(*value),
        Some(PsbValue::Double(value)) => Ok(*value as f32),
        _ => Err(EmoteError::InvalidFormat(format!(
            "missing numeric field {key}"
        ))),
    }
}

fn decode_dxt5(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(16))
        .ok_or_else(|| EmoteError::InvalidFormat("DXT5 texture size overflow".into()))?;
    if data.len() != expected {
        return Err(EmoteError::InvalidFormat(format!(
            "DXT5 data has {} bytes, expected {expected}",
            data.len()
        )));
    }

    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| EmoteError::InvalidFormat("RGBA texture size overflow".into()))?;
    let mut output = vec![0; pixel_count * 4];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let block_offset = (block_y * blocks_x + block_x) * 16;
            decode_dxt5_block(
                &data[block_offset..block_offset + 16],
                &mut output,
                width as usize,
                height as usize,
                block_x * 4,
                block_y * 4,
            );
        }
    }
    Ok(output)
}

fn decode_dxt5_block(
    block: &[u8],
    output: &mut [u8],
    width: usize,
    height: usize,
    origin_x: usize,
    origin_y: usize,
) {
    let alphas = alpha_palette(block[0], block[1]);
    let alpha_bits = u64::from_le_bytes([
        block[2], block[3], block[4], block[5], block[6], block[7], 0, 0,
    ]);
    let colors = color_palette(
        u16::from_le_bytes([block[8], block[9]]),
        u16::from_le_bytes([block[10], block[11]]),
    );
    let color_bits = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);

    for pixel in 0..16 {
        let x = origin_x + pixel % 4;
        let y = origin_y + pixel / 4;
        if x >= width || y >= height {
            continue;
        }
        let color = colors[((color_bits >> (pixel * 2)) & 3) as usize];
        let alpha = alphas[((alpha_bits >> (pixel * 3)) & 7) as usize];
        let offset = (y * width + x) * 4;
        output[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
    }
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let mut result = [a0, a1, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for index in 1..=6 {
            result[index + 1] =
                (((7 - index) as u16 * a0 as u16 + index as u16 * a1 as u16) / 7) as u8;
        }
    } else {
        for index in 1..=4 {
            result[index + 1] =
                (((5 - index) as u16 * a0 as u16 + index as u16 * a1 as u16) / 5) as u8;
        }
        result[6] = 0;
        result[7] = 255;
    }
    result
}

fn color_palette(c0: u16, c1: u16) -> [[u8; 3]; 4] {
    let a = rgb565(c0);
    let b = rgb565(c1);
    [
        a,
        b,
        [
            ((2 * a[0] as u16 + b[0] as u16) / 3) as u8,
            ((2 * a[1] as u16 + b[1] as u16) / 3) as u8,
            ((2 * a[2] as u16 + b[2] as u16) / 3) as u8,
        ],
        [
            ((a[0] as u16 + 2 * b[0] as u16) / 3) as u8,
            ((a[1] as u16 + 2 * b[1] as u16) / 3) as u8,
            ((a[2] as u16 + 2 * b[2] as u16) / 3) as u8,
        ],
    ]
}

fn rgb565(value: u16) -> [u8; 3] {
    let r = ((value >> 11) & 0x1f) as u8;
    let g = ((value >> 5) & 0x3f) as u8;
    let b = (value & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_single_opaque_red_dxt5_block() {
        let block = [255, 0, 0, 0, 0, 0, 0, 0, 0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0];
        let rgba = decode_dxt5(&block, 4, 4).unwrap();
        assert!(rgba.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
    }
}
