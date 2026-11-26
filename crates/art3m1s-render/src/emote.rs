//! Converts evaluated E-Mote layers to backend-neutral draw commands.

use crate::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, NativeEmoteMaterial, StencilMetadata,
    TextureId, TextureInfo,
};
use art3m1s_emote::EmoteDrawItem;
use glam::{Affine2, Vec2};

#[derive(Clone, Copy)]
pub struct EmoteDrawPlacement {
    /// Maps model coordinates into the destination's coordinate space.
    pub transform: Affine2,
    /// Optional clip in destination coordinates, as [left, top, right, bottom].
    pub clip_bounds: Option<[f32; 4]>,
    /// Models in the same draw list must have distinct stencil namespaces.
    pub stencil_namespace: u64,
}

/// Builds a draw command using an atlas texture resolved by the caller.
/// Layers must be submitted in evaluation order.
pub fn draw_command(
    item: EmoteDrawItem,
    texture_id: TextureId,
    texture_info: TextureInfo,
    placement: EmoteDrawPlacement,
) -> Option<DrawCommand> {
    let [atlas_x, atlas_y, width, height] = item.atlas_rect;
    if width <= 0.0 || height <= 0.0 || texture_info.width == 0 || texture_info.height == 0 {
        return None;
    }

    let native_material = native_emote_material(&item, texture_info);
    let m = item.world_transform;
    let sprite_transform = Affine2::from_cols_array(&[m[0], m[2], m[1], m[3], m[4], m[5]])
        * Affine2::from_translation(Vec2::new(
            -item.origin[0] - item.frame_offset[0],
            -item.origin[1] - item.frame_offset[1],
        ));

    Some(DrawCommand {
        texture: texture_id,
        size: texture_info,
        transform: placement.transform * sprite_transform,
        // E-Mote's corner colors, MODULATE2X and low-nibble blend modes
        // are evaluated by the native-emote shader. Keep opacity in the
        // material alpha so it is applied exactly once for both quad and
        // tessellated mesh paths.
        opacity: 1.0,
        blend: emote_blend(item.blend_mode),
        color: ColorFilter {
            multiply: [1.0, 1.0, 1.0],
            grayscale: false,
            negative: false,
        },
        clip: ClipRect {
            uv_offset: [
                atlas_x / texture_info.width as f32,
                atlas_y / texture_info.height as f32,
            ],
            uv_scale: [
                width / texture_info.width as f32,
                height / texture_info.height as f32,
            ],
            quad_size: [width, height],
        },
        clip_bounds: placement.clip_bounds,
        shader: None,
        mesh: item
            .mesh
            .as_ref()
            .and_then(|mesh| draw_mesh(mesh.blend_points.as_deref(), width, height)),
        stencil: Some(StencilMetadata {
            namespace: placement.stencil_namespace,
            source_label: item.layer_label,
            mask_labels: item.stencil_mask_layers,
            inverted: item.stencil_mask_inverted,
        }),
        native_emote: Some(native_material),
    })
}

fn color_component(color: &[f32], index: usize) -> f32 {
    let value = color.get(index).copied().unwrap_or(255.0);
    if value > 1.0 {
        (value / 255.0).clamp(0.0, 1.0)
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn emote_blend(value: i64) -> BlendMode {
    match value & 0x0f {
        1 => BlendMode::NativeAdd,
        2 | 5 => BlendMode::NativeReverseSubtract,
        3 => BlendMode::NativeMultiply,
        4 => BlendMode::NativeScreen,
        _ => BlendMode::Alpha,
    }
}

fn native_emote_material(item: &EmoteDrawItem, texture_info: TextureInfo) -> NativeEmoteMaterial {
    // `color` is either four byte channels decoded from a packed scalar or a
    // list of packed 0xRRGGBBAA corner colors. The latter is distinguished by
    // values wider than one byte, matching the independent emoteplayer parser.
    let corners = if item.color.len() >= 4 && item.color.iter().take(4).any(|value| *value > 255.0)
    {
        item.color
            .iter()
            .take(4)
            .map(|value| packed_color(*value as u32, item.opacity))
            .collect::<Vec<_>>()
    } else {
        let rgba = [
            color_component(&item.color, 0),
            color_component(&item.color, 1),
            color_component(&item.color, 2),
            color_component(&item.color, 3) * item.opacity,
        ];
        vec![rgba; 4]
    };
    let mut corner_colors = [
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
    ];
    for (dst, src) in corner_colors.iter_mut().zip(corners.into_iter()) {
        *dst = src;
    }
    NativeEmoteMaterial {
        corner_colors,
        uv_rect: [
            item.atlas_rect[0] / texture_info.width as f32,
            item.atlas_rect[1] / texture_info.height as f32,
            (item.atlas_rect[0] + item.atlas_rect[2]) / texture_info.width as f32,
            (item.atlas_rect[1] + item.atlas_rect[3]) / texture_info.height as f32,
        ],
        blend_mode: item.blend_mode as u32,
        clip_rect: [-1.0e30, -1.0e30, 1.0e30, 1.0e30],
        wipe: [0.0, 0.0, 0.0],
    }
}

fn packed_color(value: u32, opacity: f32) -> [f32; 4] {
    [
        ((value >> 24) & 0xff) as f32 / 255.0,
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        ((value & 0xff) as f32 / 255.0 * opacity).clamp(0.0, 1.0),
    ]
}

fn draw_mesh(points: Option<&[f32]>, width: f32, height: f32) -> Option<DrawMesh> {
    let points = points?;
    if points.is_empty() || !points.len().is_multiple_of(2) {
        return None;
    }
    let point_count = points.len() / 2;
    let side = (point_count as f32).sqrt() as usize;
    if side < 2 || side * side != point_count {
        return None;
    }

    let vertex = |x: usize, y: usize| {
        let index = (y * side + x) * 2;
        [
            points[index] * width,
            points[index + 1] * height,
            x as f32 / (side - 1) as f32,
            y as f32 / (side - 1) as f32,
        ]
    };
    let mut vertices = Vec::with_capacity((side - 1) * (side - 1) * 6);
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            let top_left = vertex(x, y);
            let top_right = vertex(x + 1, y);
            let bottom_left = vertex(x, y + 1);
            let bottom_right = vertex(x + 1, y + 1);
            vertices.extend_from_slice(&[
                top_left,
                top_right,
                bottom_right,
                top_left,
                bottom_right,
                bottom_left,
            ]);
        }
    }
    Some(DrawMesh {
        vertices: vertices.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_four_by_four_blend_points_into_nine_quads() {
        let mut points = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                points.push(x as f32 / 3.0);
                points.push(y as f32 / 3.0);
            }
        }
        let mesh = draw_mesh(Some(&points), 300.0, 600.0).unwrap();
        assert_eq!(mesh.vertices.len(), 54);
        assert_eq!(mesh.vertices[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[2], [100.0, 200.0, 1.0 / 3.0, 1.0 / 3.0]);
    }
}
