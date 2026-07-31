use ctt_astcenc::bindings::{astcenc_image, astcenc_type_ASTCENC_TYPE_U8};
use ctt_astcenc::{Context, Flags, Preset, Profile, Swizzle, config_init};
use std::ffi::c_void;

pub(crate) struct AstcEncoder {
    context: Context,
}

impl AstcEncoder {
    pub(crate) fn new() -> Result<Self, String> {
        let config = config_init(
            Profile::Ldr,
            4,
            4,
            1,
            Preset::Fastest,
            Flags::USE_DECODE_UNORM8 | Flags::USE_ALPHA_WEIGHT,
        )
        .map_err(|error| error.to_string())?;
        let context = Context::new(&config).map_err(|error| error.to_string())?;
        Ok(Self { context })
    }

    pub(crate) fn encode_rgba8(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<Vec<u8>, String> {
        let rgba_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "ASTC RGBA dimensions overflow".to_string())?;
        if width == 0 || height == 0 || rgba.len() != rgba_len {
            return Err("ASTC RGBA input length mismatch".to_string());
        }
        let output_len = astc_4x4_len(width, height)
            .ok_or_else(|| "ASTC output dimensions overflow".to_string())?;
        let mut output = vec![0; output_len];
        let mut plane = rgba.as_ptr().cast_mut().cast::<c_void>();
        let mut image = astcenc_image {
            dim_x: width,
            dim_y: height,
            dim_z: 1,
            data_type: astcenc_type_ASTCENC_TYPE_U8,
            data: &mut plane,
        };
        self.context
            .compress(&mut image, Swizzle::IDENTITY, &mut output)
            .map_err(|error| error.to_string())?;
        self.context
            .compress_reset()
            .map_err(|error| error.to_string())?;
        Ok(output)
    }
}

pub(crate) fn astc_4x4_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_add(3)?
        .checked_div(4)?
        .checked_mul((height as usize).checked_add(3)?.checked_div(4)?)?
        .checked_mul(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_length_rounds_up_partial_blocks() {
        assert_eq!(astc_4x4_len(4, 4), Some(16));
        assert_eq!(astc_4x4_len(5, 7), Some(64));
        assert_eq!(astc_4x4_len(0, 4), Some(0));
    }

    #[test]
    fn encoder_produces_one_block_for_a_small_rgba_texture() {
        let rgba = [255_u8; 4 * 4 * 4];
        let encoded = AstcEncoder::new()
            .and_then(|mut encoder| encoder.encode_rgba8(4, 4, &rgba))
            .expect("ASTC encoder should accept RGBA8 input");

        assert_eq!(encoded.len(), 16);
        assert!(encoded.iter().any(|byte| *byte != 0));
    }
}
