use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AtlasIcon, EmoteError, EmoteFrameContent, EmoteLayer, EmoteMesh, EmoteModel,
    EmoteMotionParameter, EmoteMotionRef, Result,
};

const MAX_MOTION_RECURSION: usize = 128;
const DEFORMED_MESH_SIDE: usize = 8;

#[derive(Clone, Debug)]
struct MeshDeformer {
    combine: bool,
    translation: [f32; 2],
    angle: f32,
    size: [f32; 2],
    origin: [f32; 2],
    points: Vec<f32>,
    side: usize,
}

#[derive(Clone, Debug, Default)]
pub struct EmoteRenderState {
    pub motion_time: f32,
    pub variables: BTreeMap<String, f32>,
}

#[derive(Clone, Debug)]
pub struct EmoteDrawItem {
    pub layer_label: String,
    pub texture_id: String,
    pub icon_id: String,
    pub atlas_rect: [f32; 4],
    pub origin: [f32; 2],
    pub translation: [f32; 3],
    pub angle: f32,
    pub opacity: f32,
    pub blend_mode: i64,
    pub color: Vec<f32>,
    pub z_order: i64,
    /// Layer order path across nested motions. E-Mote's layerIndexMap uses
    /// larger indices for back layers, so comparison is descending per level.
    pub draw_order: Vec<i64>,
    pub mesh: Option<EmoteMesh>,
    pub stencil_mask_layers: Vec<String>,
}

pub struct EmoteMotionEvaluator<'a> {
    model: &'a EmoteModel,
}

impl<'a> EmoteMotionEvaluator<'a> {
    pub fn new(model: &'a EmoteModel) -> Self {
        Self { model }
    }

    pub fn evaluate_base(&self, state: &EmoteRenderState) -> Result<Vec<EmoteDrawItem>> {
        let character = self
            .model
            .info()
            .base_chara
            .as_deref()
            .ok_or_else(|| EmoteError::InvalidFormat("model has no base character".into()))?;
        let motion = self
            .model
            .info()
            .base_motion
            .as_deref()
            .ok_or_else(|| EmoteError::InvalidFormat("model has no base motion".into()))?;
        self.evaluate(character, motion, state)
    }

    pub fn evaluate(
        &self,
        character: &str,
        motion: &str,
        state: &EmoteRenderState,
    ) -> Result<Vec<EmoteDrawItem>> {
        let mut resolved_state = state.clone();
        self.model
            .apply_selector_controls(&mut resolved_state.variables);
        let mut items = Vec::new();
        let mut stack = BTreeSet::new();
        let mut deformers = Vec::new();
        self.visit_motion(
            character,
            motion,
            resolved_state.motion_time,
            &resolved_state,
            [0.0; 3],
            0.0,
            1.0,
            0,
            &[],
            &[],
            &mut stack,
            &mut deformers,
            &mut items,
        )?;
        items.sort_by(|left, right| {
            left.z_order
                .cmp(&right.z_order)
                .then_with(|| compare_draw_order(&left.draw_order, &right.draw_order))
        });
        Ok(items)
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_motion(
        &self,
        character: &str,
        motion_label: &str,
        motion_time: f32,
        state: &EmoteRenderState,
        translation: [f32; 3],
        angle: f32,
        opacity: f32,
        depth: usize,
        stencil_mask_layers: &[String],
        order_prefix: &[i64],
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
    ) -> Result<()> {
        if depth > MAX_MOTION_RECURSION {
            return Err(EmoteError::InvalidFormat(
                "E-Mote motion recursion limit exceeded".into(),
            ));
        }
        let key = (character.to_owned(), motion_label.to_owned());
        if !stack.insert(key.clone()) {
            return Ok(());
        }
        let motion = self
            .model
            .motions()
            .motion(character, motion_label)
            .ok_or_else(|| {
                EmoteError::InvalidFormat(format!(
                    "missing referenced motion {character}/{motion_label}"
                ))
            })?;
        // loop_time < 0（原始 0xFF 哨兵）表示不循环，走 clamp 分支。
        let motion_time = if motion.loop_time >= 0.0
            && motion.last_time > motion.loop_time
            && motion_time > motion.last_time
        {
            motion.loop_time
                + (motion_time - motion.loop_time) % (motion.last_time - motion.loop_time)
        } else {
            motion_time.min(motion.last_time.max(0.0))
        };
        for (fallback_index, layer) in motion.layers.iter().enumerate() {
            self.visit_layer(
                layer,
                &motion.parameters,
                &motion.layer_index_map,
                motion_time,
                state,
                translation,
                angle,
                opacity,
                depth,
                stencil_mask_layers,
                order_prefix,
                fallback_index as i64,
                stack,
                deformers,
                items,
            )?;
        }
        stack.remove(&key);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_layer(
        &self,
        layer: &EmoteLayer,
        parameters: &[EmoteMotionParameter],
        layer_index_map: &BTreeMap<String, i64>,
        motion_time: f32,
        state: &EmoteRenderState,
        parent_translation: [f32; 3],
        parent_angle: f32,
        parent_opacity: f32,
        depth: usize,
        parent_stencil_mask_layers: &[String],
        order_prefix: &[i64],
        fallback_index: i64,
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
    ) -> Result<()> {
        let suspended_deformers = (!layer.inherit_shape).then(|| std::mem::take(deformers));
        let mut draw_order = order_prefix.to_vec();
        draw_order.push(
            layer_index_map
                .get(&layer.label)
                .copied()
                .unwrap_or(fallback_index),
        );
        let layer_time = resolve_layer_time(layer, parameters, motion_time, state);
        let content = sample_content(layer, layer_time);
        let content_ref = content.as_ref();
        let translation = add_translation(
            parent_translation,
            parent_angle,
            content_ref.and_then(|content| content.coord.as_deref()),
        );
        let angle = parent_angle + content_ref.and_then(|content| content.angle).unwrap_or(0.0);
        let opacity =
            parent_opacity * normalized_opacity(content_ref.and_then(|value| value.opacity));
        let stencil_mask_layers =
            if layer.stencil_type == 5 && !layer.stencil_mask_layers.is_empty() {
                layer.stencil_mask_layers.as_slice()
            } else {
                parent_stencil_mask_layers
            };

        let deformer = content.as_ref().and_then(|content| {
            MeshDeformer::from_content(content, layer.mesh_combine, translation, angle)
        });
        let pushed_deformer = deformer.is_some();
        if let Some(deformer) = deformer {
            deformers.push(deformer);
        }

        if let Some(content) = content.as_ref() {
            self.visit_content(
                &layer.label,
                content,
                motion_time,
                state,
                translation,
                angle,
                opacity,
                depth,
                stencil_mask_layers,
                &draw_order,
                stack,
                deformers,
                items,
            )?;
        }

        for (fallback_index, child) in layer.children.iter().enumerate() {
            self.visit_layer(
                child,
                parameters,
                layer_index_map,
                motion_time,
                state,
                translation,
                angle,
                opacity,
                depth,
                stencil_mask_layers,
                order_prefix,
                fallback_index as i64,
                stack,
                deformers,
                items,
            )?;
        }
        if pushed_deformer {
            deformers.pop();
        }
        if let Some(mut inherited) = suspended_deformers {
            inherited.append(deformers);
            *deformers = inherited;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_content(
        &self,
        layer_label: &str,
        content: &EmoteFrameContent,
        motion_time: f32,
        state: &EmoteRenderState,
        translation: [f32; 3],
        angle: f32,
        opacity: f32,
        depth: usize,
        stencil_mask_layers: &[String],
        draw_order: &[i64],
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
    ) -> Result<()> {
        let (Some(source), Some(icon_id)) = (&content.source, &content.icon) else {
            return Ok(());
        };
        if source == "blank" {
            return Ok(());
        }
        if source.starts_with("tex#") {
            let icon = self.model.atlas().icon(icon_id).ok_or_else(|| {
                EmoteError::InvalidFormat(format!(
                    "texture source {source} references missing icon {icon_id}"
                ))
            })?;
            if icon.texture_id != *source {
                return Err(EmoteError::InvalidFormat(format!(
                    "icon {icon_id} belongs to {}, referenced through {source}",
                    icon.texture_id
                )));
            }
            let mut item = draw_item(
                layer_label,
                icon,
                content,
                translation,
                angle,
                opacity,
                stencil_mask_layers,
                draw_order,
            );
            apply_deformers(&mut item, deformers);
            items.push(item);
            return Ok(());
        }

        let referenced_time = motion_time
            + content
                .motion
                .as_ref()
                .map(|motion| motion.time_offset)
                .unwrap_or(0.0);
        self.visit_motion(
            source,
            icon_id,
            referenced_time,
            state,
            translation,
            angle,
            opacity,
            depth + 1,
            stencil_mask_layers,
            draw_order,
            stack,
            deformers,
            items,
        )
    }
}

impl MeshDeformer {
    fn from_content(
        content: &EmoteFrameContent,
        combine: bool,
        translation: [f32; 3],
        angle: f32,
    ) -> Option<Self> {
        if content.source.as_deref() != Some("blank") {
            return None;
        }
        let [width, height, origin_x, origin_y] = parse_blank_icon(content.icon.as_deref()?)?;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let points = content.mesh.as_ref()?.blend_points.as_ref()?.clone();
        let point_count = points.len() / 2;
        let side = (point_count as f32).sqrt() as usize;
        if points.len() % 2 != 0 || side < 2 || side * side != point_count {
            return None;
        }
        Some(Self {
            combine,
            translation: [translation[0], translation[1]],
            angle,
            size: [width, height],
            origin: [origin_x, origin_y],
            points,
            side,
        })
    }

    fn deform(&self, point: [f32; 2]) -> [f32; 2] {
        let local = rotate(
            [
                point[0] - self.translation[0],
                point[1] - self.translation[1],
            ],
            -self.angle,
        );
        let normalized = [
            (local[0] + self.origin[0]) / self.size[0],
            (local[1] + self.origin[1]) / self.size[1],
        ];
        let warped = sample_grid(&self.points, self.side, normalized);
        let warped_local = [
            warped[0] * self.size[0] - self.origin[0],
            warped[1] * self.size[1] - self.origin[1],
        ];
        let warped_world = rotate(warped_local, self.angle);
        [
            warped_world[0] + self.translation[0],
            warped_world[1] + self.translation[1],
        ]
    }
}

fn parse_blank_icon(icon: &str) -> Option<[f32; 4]> {
    let mut values = icon.split(':').map(str::parse::<f32>);
    let parsed = [
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ];
    values.next().is_none().then_some(parsed)
}

fn sample_grid(points: &[f32], side: usize, normalized: [f32; 2]) -> [f32; 2] {
    if side == 4 {
        return sample_bezier_patch(points, normalized);
    }
    let sample_axis = |value: f32| {
        let scaled = value.clamp(0.0, 1.0) * (side - 1) as f32;
        let cell = (scaled.floor() as usize).min(side - 2);
        (cell, scaled - cell as f32)
    };
    let (x, tx) = sample_axis(normalized[0]);
    let (y, ty) = sample_axis(normalized[1]);
    let point = |x: usize, y: usize| {
        let index = (y * side + x) * 2;
        [points[index], points[index + 1]]
    };
    let top = lerp_point(point(x, y), point(x + 1, y), tx);
    let bottom = lerp_point(point(x, y + 1), point(x + 1, y + 1), tx);
    lerp_point(top, bottom, ty)
}

fn sample_bezier_patch(points: &[f32], normalized: [f32; 2]) -> [f32; 2] {
    let basis = |value: f32| {
        let value = value.clamp(0.0, 1.0);
        let inverse = 1.0 - value;
        [
            inverse * inverse * inverse,
            3.0 * inverse * inverse * value,
            3.0 * inverse * value * value,
            value * value * value,
        ]
    };
    let x_basis = basis(normalized[0]);
    let y_basis = basis(normalized[1]);
    let mut result = [0.0; 2];
    for (y, y_weight) in y_basis.into_iter().enumerate() {
        for (x, x_weight) in x_basis.into_iter().enumerate() {
            let weight = x_weight * y_weight;
            let index = (y * 4 + x) * 2;
            result[0] += points[index] * weight;
            result[1] += points[index + 1] * weight;
        }
    }
    result
}

fn lerp_point(from: [f32; 2], to: [f32; 2], ratio: f32) -> [f32; 2] {
    [
        from[0] + (to[0] - from[0]) * ratio,
        from[1] + (to[1] - from[1]) * ratio,
    ]
}

fn rotate(point: [f32; 2], angle: f32) -> [f32; 2] {
    let (sin, cos) = angle.to_radians().sin_cos();
    [
        point[0] * cos - point[1] * sin,
        point[0] * sin + point[1] * cos,
    ]
}

fn apply_deformers(item: &mut EmoteDrawItem, deformers: &[MeshDeformer]) {
    if item.atlas_rect[2] <= 0.0 || item.atlas_rect[3] <= 0.0 {
        return;
    }
    let source_patch = item
        .mesh
        .as_ref()
        .and_then(|mesh| mesh.blend_points.as_deref())
        .and_then(mesh_patch);
    if deformers.is_empty() && source_patch.is_none() {
        return;
    }
    let combined_deformers = combine_deformers(deformers);
    let mut deformed = Vec::with_capacity(DEFORMED_MESH_SIDE * DEFORMED_MESH_SIDE * 2);
    for y in 0..DEFORMED_MESH_SIDE {
        for x in 0..DEFORMED_MESH_SIDE {
            let normalized = [
                x as f32 / (DEFORMED_MESH_SIDE - 1) as f32,
                y as f32 / (DEFORMED_MESH_SIDE - 1) as f32,
            ];
            let point = source_patch
                .as_ref()
                .map(|(points, side)| sample_grid(points, *side, normalized))
                .unwrap_or(normalized);
            let local = [
                point[0] * item.atlas_rect[2] - item.origin[0],
                point[1] * item.atlas_rect[3] - item.origin[1],
            ];
            let rotated = rotate(local, item.angle);
            let mut world = [
                rotated[0] + item.translation[0],
                rotated[1] + item.translation[1],
            ];
            for deformer in combined_deformers.iter().rev() {
                world = deformer.deform(world);
            }
            let final_local = rotate(
                [
                    world[0] - item.translation[0],
                    world[1] - item.translation[1],
                ],
                -item.angle,
            );
            deformed.push((final_local[0] + item.origin[0]) / item.atlas_rect[2]);
            deformed.push((final_local[1] + item.origin[1]) / item.atlas_rect[3]);
        }
    }
    item.mesh = Some(EmoteMesh {
        blend_points: Some(deformed),
        control_coordinates: item
            .mesh
            .as_ref()
            .and_then(|mesh| mesh.control_coordinates.clone()),
    });
}

fn mesh_patch(points: &[f32]) -> Option<(&[f32], usize)> {
    if points.is_empty() || !points.len().is_multiple_of(2) {
        return None;
    }
    let point_count = points.len() / 2;
    let side = (point_count as f32).sqrt() as usize;
    (side >= 2 && side * side == point_count).then_some((points, side))
}

fn combine_deformers(deformers: &[MeshDeformer]) -> Vec<MeshDeformer> {
    let mut combined: Vec<MeshDeformer> = Vec::with_capacity(deformers.len());
    for deformer in deformers {
        if deformer.combine
            && let Some(parent) = combined.last_mut()
            && parent.same_space(deformer)
        {
            for (index, point) in parent.points.iter_mut().enumerate() {
                *point += deformer.points[index] - identity_coordinate(index, deformer.side);
            }
            continue;
        }
        let mut deformer = deformer.clone();
        deformer.combine = false;
        combined.push(deformer);
    }
    combined
}

impl MeshDeformer {
    fn same_space(&self, other: &Self) -> bool {
        self.side == other.side
            && self.points.len() == other.points.len()
            && approximately_equal(self.translation[0], other.translation[0])
            && approximately_equal(self.translation[1], other.translation[1])
            && approximately_equal(self.angle, other.angle)
            && approximately_equal(self.size[0], other.size[0])
            && approximately_equal(self.size[1], other.size[1])
            && approximately_equal(self.origin[0], other.origin[0])
            && approximately_equal(self.origin[1], other.origin[1])
    }
}

fn identity_coordinate(index: usize, side: usize) -> f32 {
    let point_index = index / 2;
    let axis_index = if index.is_multiple_of(2) {
        point_index % side
    } else {
        point_index / side
    };
    axis_index as f32 / (side - 1) as f32
}

fn approximately_equal(left: f32, right: f32) -> bool {
    (left - right).abs() < 0.001
}

#[cfg(test)]
fn identity_grid() -> Vec<f32> {
    let mut points = Vec::with_capacity(32);
    for y in 0..4 {
        for x in 0..4 {
            points.push(x as f32 / 3.0);
            points.push(y as f32 / 3.0);
        }
    }
    points
}

fn sample_content(layer: &EmoteLayer, time: f32) -> Option<EmoteFrameContent> {
    let index = layer.frames.iter().rposition(|frame| frame.time <= time)?;
    let frame = &layer.frames[index];
    match frame.frame_type {
        0 => None,
        1 if (time - frame.time).abs() > 0.001 => None,
        3 => {
            let Some(next) = layer.frames.get(index + 1) else {
                return frame.content.clone();
            };
            let (Some(from), Some(to)) = (&frame.content, &next.content) else {
                return frame.content.clone();
            };
            let span = next.time - frame.time;
            if span <= f32::EPSILON {
                next.content.clone()
            } else {
                Some(interpolate_content(
                    from,
                    to,
                    ((time - frame.time) / span).clamp(0.0, 1.0),
                ))
            }
        }
        _ => frame.content.clone(),
    }
}

fn resolve_layer_time(
    layer: &EmoteLayer,
    parameters: &[EmoteMotionParameter],
    motion_time: f32,
    state: &EmoteRenderState,
) -> f32 {
    layer
        .parameter_index
        .or_else(|| (!parameters.is_empty()).then_some(0))
        .and_then(|index| parameters.get(index))
        .and_then(|parameter| {
            parameter.frame_for_value(state.variables.get(&parameter.id).copied().unwrap_or(0.0))
        })
        .unwrap_or(motion_time)
}

fn interpolate_content(
    from: &EmoteFrameContent,
    to: &EmoteFrameContent,
    ratio: f32,
) -> EmoteFrameContent {
    let choose_to = ratio >= 1.0;
    EmoteFrameContent {
        mask: if choose_to { to.mask } else { from.mask },
        source: if choose_to {
            to.source.clone()
        } else {
            from.source.clone()
        },
        icon: if choose_to {
            to.icon.clone()
        } else {
            from.icon.clone()
        },
        coord: interpolate_list(from.coord.as_deref(), to.coord.as_deref(), ratio, 0.0),
        angle: interpolate_number(from.angle, to.angle, ratio, 0.0),
        opacity: interpolate_number(from.opacity, to.opacity, ratio, 255.0),
        blend_mode: if choose_to {
            to.blend_mode
        } else {
            from.blend_mode
        },
        color: interpolate_list(from.color.as_deref(), to.color.as_deref(), ratio, 255.0),
        mesh: interpolate_mesh(from.mesh.as_ref(), to.mesh.as_ref(), ratio),
        motion: interpolate_motion(from.motion.as_ref(), to.motion.as_ref(), ratio),
    }
}

fn interpolate_number(from: Option<f32>, to: Option<f32>, ratio: f32, default: f32) -> Option<f32> {
    match (from, to) {
        (Some(from), Some(to)) => Some(from + (to - from) * ratio),
        (Some(from), None) => Some(from + (default - from) * ratio),
        (None, Some(to)) => Some(default + (to - default) * ratio),
        (None, None) => None,
    }
}

fn interpolate_list(
    from: Option<&[f32]>,
    to: Option<&[f32]>,
    ratio: f32,
    default: f32,
) -> Option<Vec<f32>> {
    match (from, to) {
        (Some(from), Some(to)) if from.len() == to.len() => Some(
            from.iter()
                .zip(to)
                .map(|(from, to)| from + (to - from) * ratio)
                .collect(),
        ),
        (Some(from), None) => Some(
            from.iter()
                .map(|from| from + (default - from) * ratio)
                .collect(),
        ),
        (None, Some(to)) => Some(
            to.iter()
                .map(|to| default + (to - default) * ratio)
                .collect(),
        ),
        (Some(value), Some(_)) => Some(value.to_vec()),
        (None, None) => None,
    }
}

fn interpolate_mesh(
    from: Option<&EmoteMesh>,
    to: Option<&EmoteMesh>,
    ratio: f32,
) -> Option<EmoteMesh> {
    if from.is_none() && to.is_none() {
        return None;
    }
    Some(EmoteMesh {
        blend_points: interpolate_mesh_points(
            from.and_then(|mesh| mesh.blend_points.as_deref()),
            to.and_then(|mesh| mesh.blend_points.as_deref()),
            ratio,
        ),
        control_coordinates: interpolate_list(
            from.and_then(|mesh| mesh.control_coordinates.as_deref()),
            to.and_then(|mesh| mesh.control_coordinates.as_deref()),
            ratio,
            0.0,
        ),
    })
}

fn interpolate_mesh_points(
    from: Option<&[f32]>,
    to: Option<&[f32]>,
    ratio: f32,
) -> Option<Vec<f32>> {
    match (from, to) {
        (Some(from), Some(to)) if from.len() == to.len() => Some(
            from.iter()
                .zip(to)
                .map(|(from, to)| from + (to - from) * ratio)
                .collect(),
        ),
        (Some(from), None) => interpolate_mesh_with_identity(from, ratio, false),
        (None, Some(to)) => interpolate_mesh_with_identity(to, ratio, true),
        (Some(value), Some(_)) => Some(value.to_vec()),
        (None, None) => None,
    }
}

fn interpolate_mesh_with_identity(
    points: &[f32],
    ratio: f32,
    identity_is_from: bool,
) -> Option<Vec<f32>> {
    let point_count = points.len() / 2;
    let side = (point_count as f32).sqrt() as usize;
    if points.len() % 2 != 0 || side < 2 || side * side != point_count {
        return Some(points.to_vec());
    }
    Some(
        points
            .iter()
            .copied()
            .enumerate()
            .map(|(index, point)| {
                let identity = identity_coordinate(index, side);
                if identity_is_from {
                    identity + (point - identity) * ratio
                } else {
                    point + (identity - point) * ratio
                }
            })
            .collect(),
    )
}

fn interpolate_motion(
    from: Option<&EmoteMotionRef>,
    to: Option<&EmoteMotionRef>,
    ratio: f32,
) -> Option<EmoteMotionRef> {
    match (from, to) {
        (Some(from), Some(to)) => Some(EmoteMotionRef {
            mask: from.mask,
            time_offset: from.time_offset + (to.time_offset - from.time_offset) * ratio,
        }),
        (Some(value), None) | (None, Some(value)) => Some(value.clone()),
        (None, None) => None,
    }
}

fn add_translation(parent: [f32; 3], parent_angle: f32, coord: Option<&[f32]>) -> [f32; 3] {
    let x = coord
        .and_then(|value| value.first())
        .copied()
        .unwrap_or(0.0);
    let y = coord.and_then(|value| value.get(1)).copied().unwrap_or(0.0);
    let radians = parent_angle.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        parent[0] + x * cos - y * sin,
        parent[1] + x * sin + y * cos,
        parent[2] + coord.and_then(|value| value.get(2)).copied().unwrap_or(0.0),
    ]
}

fn normalized_opacity(opacity: Option<f32>) -> f32 {
    let opacity = opacity.unwrap_or(255.0);
    if opacity > 1.0 {
        (opacity / 255.0).clamp(0.0, 1.0)
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

fn draw_item(
    layer_label: &str,
    icon: &AtlasIcon,
    content: &EmoteFrameContent,
    translation: [f32; 3],
    angle: f32,
    opacity: f32,
    stencil_mask_layers: &[String],
    draw_order: &[i64],
) -> EmoteDrawItem {
    EmoteDrawItem {
        layer_label: layer_label.to_owned(),
        texture_id: icon.texture_id.clone(),
        icon_id: icon.id.clone(),
        atlas_rect: [icon.left, icon.top, icon.width, icon.height],
        origin: [icon.origin_x, icon.origin_y],
        translation,
        angle,
        opacity,
        blend_mode: content.blend_mode.unwrap_or(0),
        color: content
            .color
            .clone()
            .unwrap_or_else(|| vec![255.0, 255.0, 255.0, 255.0]),
        z_order: icon.z_order,
        draw_order: draw_order.to_vec(),
        mesh: content.mesh.clone(),
        stencil_mask_layers: stencil_mask_layers.to_vec(),
    }
}

fn compare_draw_order(left: &[i64], right: &[i64]) -> Ordering {
    for (left, right) in left.iter().zip(right) {
        let order = right.cmp(left);
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampled_layer(frame_type: i64) -> EmoteLayer {
        EmoteLayer {
            label: "sample".into(),
            layer_type: 0,
            coordinate: 0,
            inherit_shape: true,
            mesh_combine: false,
            stencil_type: 0,
            stencil_mask_layers: Vec::new(),
            parameter_index: None,
            frames: vec![
                crate::EmoteLayerFrame {
                    time: 0.0,
                    frame_type,
                    content: Some(EmoteFrameContent {
                        coord: Some(vec![0.0, 0.0]),
                        ..EmoteFrameContent::default()
                    }),
                },
                crate::EmoteLayerFrame {
                    time: 10.0,
                    frame_type: 2,
                    content: Some(EmoteFrameContent {
                        coord: Some(vec![10.0, 0.0]),
                        ..EmoteFrameContent::default()
                    }),
                },
            ],
            children: Vec::new(),
        }
    }

    #[test]
    fn treats_byte_opacity_as_normalized_alpha() {
        assert_eq!(normalized_opacity(Some(255.0)), 1.0);
        assert_eq!(normalized_opacity(Some(0.5)), 0.5);
    }

    #[test]
    fn larger_layer_indices_are_drawn_first_within_each_motion_level() {
        let mut paths = vec![vec![2], vec![10, 1], vec![4], vec![10, 3]];
        paths.sort_by(|left, right| compare_draw_order(left, right));
        assert_eq!(paths, vec![vec![10, 3], vec![10, 1], vec![4], vec![2]]);
    }

    #[test]
    fn rotates_child_translation_in_parent_space() {
        let translated = add_translation([10.0, 20.0, 0.0], 90.0, Some(&[100.0, 0.0]));
        assert!((translated[0] - 10.0).abs() < 0.001);
        assert!((translated[1] - 120.0).abs() < 0.001);
    }

    #[test]
    fn interpolates_parameterized_mesh_points() {
        let from = EmoteFrameContent {
            mesh: Some(EmoteMesh {
                blend_points: Some(vec![0.0, 0.0, 1.0, 1.0]),
                control_coordinates: None,
            }),
            ..EmoteFrameContent::default()
        };
        let to = EmoteFrameContent {
            mesh: Some(EmoteMesh {
                blend_points: Some(vec![0.2, 0.4, 0.8, 0.6]),
                control_coordinates: None,
            }),
            ..EmoteFrameContent::default()
        };
        let content = interpolate_content(&from, &to, 0.5);
        assert_eq!(
            content.mesh.unwrap().blend_points.unwrap(),
            [0.1, 0.2, 0.9, 0.8]
        );
    }

    #[test]
    fn omitted_parameter_index_uses_the_first_motion_parameter() {
        let layer = sampled_layer(3);
        let parameters = [EmoteMotionParameter {
            id: "face_eye_open".into(),
            range_begin: 0.0,
            range_end: 10.0,
            division: 10.0,
            enabled: true,
            discretization: false,
        }];
        let state = EmoteRenderState {
            motion_time: 0.0,
            variables: BTreeMap::from([("face_eye_open".to_string(), 7.0)]),
        };

        assert_eq!(resolve_layer_time(&layer, &parameters, 2.0, &state), 7.0);
        assert_eq!(resolve_layer_time(&layer, &[], 2.0, &state), 2.0);
    }

    #[test]
    fn omitted_opacity_interpolates_from_opaque_default() {
        let from = EmoteFrameContent::default();
        let to = EmoteFrameContent {
            opacity: Some(0.0),
            ..EmoteFrameContent::default()
        };
        assert_eq!(interpolate_content(&from, &to, 0.0).opacity, Some(255.0));
        assert_eq!(interpolate_content(&from, &to, 0.5).opacity, Some(127.5));
    }

    #[test]
    fn omitted_mesh_points_interpolate_to_identity_patch() {
        let mut shifted = identity_grid();
        for point in shifted.chunks_exact_mut(2) {
            point[0] += 0.3;
        }
        let sampled = interpolate_mesh_points(Some(&shifted), None, 0.5).unwrap();
        for (index, value) in sampled.into_iter().enumerate() {
            let expected =
                identity_coordinate(index, 4) + if index.is_multiple_of(2) { 0.15 } else { 0.0 };
            assert!((value - expected).abs() < 0.0001);
        }
    }

    #[test]
    fn identity_bezier_patch_preserves_coordinates() {
        let points = identity_grid();
        for point in [[0.0, 0.0], [0.2, 0.7], [0.5, 0.5], [1.0, 1.0]] {
            let sampled = sample_bezier_patch(&points, point);
            assert!((sampled[0] - point[0]).abs() < 0.0001);
            assert!((sampled[1] - point[1]).abs() < 0.0001);
        }
    }

    #[test]
    fn tessellates_bezier_control_points_before_rendering() {
        let mut points = identity_grid();
        points[(1 * 4 + 1) * 2 + 1] += 0.6;
        let mut item = EmoteDrawItem {
            layer_label: "face".into(),
            texture_id: "tex#000".into(),
            icon_id: "1".into(),
            atlas_rect: [0.0, 0.0, 100.0, 100.0],
            origin: [0.0, 0.0],
            translation: [0.0; 3],
            angle: 0.0,
            opacity: 1.0,
            blend_mode: 0,
            color: vec![255.0; 4],
            z_order: 0,
            draw_order: Vec::new(),
            mesh: Some(EmoteMesh {
                blend_points: Some(points.clone()),
                control_coordinates: None,
            }),
            stencil_mask_layers: Vec::new(),
        };
        apply_deformers(&mut item, &[]);
        let tessellated = item.mesh.unwrap().blend_points.unwrap();
        assert_eq!(
            tessellated.len(),
            DEFORMED_MESH_SIDE * DEFORMED_MESH_SIDE * 2
        );
        let sample_index = (3 * DEFORMED_MESH_SIDE + 3) * 2;
        let normalized = [
            3.0 / (DEFORMED_MESH_SIDE - 1) as f32,
            3.0 / (DEFORMED_MESH_SIDE - 1) as f32,
        ];
        let expected = sample_bezier_patch(&points, normalized);
        assert!((tessellated[sample_index] - expected[0]).abs() < 0.0001);
        assert!((tessellated[sample_index + 1] - expected[1]).abs() < 0.0001);
    }

    #[test]
    fn mesh_combine_layers_add_control_point_deltas() {
        let make_deformer = |combine: bool, x_offset: f32| {
            let mut points = identity_grid();
            for point in points.chunks_exact_mut(2) {
                point[0] += x_offset;
            }
            MeshDeformer {
                combine,
                translation: [10.0, 20.0],
                angle: 0.0,
                size: [300.0, 600.0],
                origin: [150.0, 300.0],
                points,
                side: 4,
            }
        };
        let combined = combine_deformers(&[make_deformer(false, 0.1), make_deformer(true, 0.2)]);
        assert_eq!(combined.len(), 1);
        for (index, value) in combined[0].points.iter().copied().enumerate() {
            let expected =
                identity_coordinate(index, 4) + if index.is_multiple_of(2) { 0.3 } else { 0.0 };
            assert!((value - expected).abs() < 0.0001);
        }
    }

    #[test]
    fn respects_single_hold_and_tween_frame_types() {
        assert!(sample_content(&sampled_layer(1), 5.0).is_none());
        assert_eq!(
            sample_content(&sampled_layer(2), 5.0).unwrap().coord,
            Some(vec![0.0, 0.0])
        );
        assert_eq!(
            sample_content(&sampled_layer(3), 5.0).unwrap().coord,
            Some(vec![5.0, 0.0])
        );
    }
}
