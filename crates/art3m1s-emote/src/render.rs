use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::{
    AtlasIcon, EmoteBezierPath, EmoteEasingCurve, EmoteError, EmoteFrameContent, EmoteLayer,
    EmoteMesh, EmoteModel, EmoteMotionParameter, EmoteMotionRef, Result,
};

const MAX_MOTION_RECURSION: usize = 128;
const DEFORMED_MESH_SIDE: usize = 8;

/// Small affine type kept local to the model crate so evaluating a motion does
/// not pull a renderer/math dependency into the parser. The channel-wise
/// inheritance and transform-order rules below are adapted from this
/// repository's `crates/eluna/src/emote.rs`; the frame fields and draw order
/// follow `krkrsdl3/plugins/emoteplayer`. That reference is Copyright (c)
/// W.Dee and contributors. Source and license details are recorded in
/// `crates/art3m1s-emote/THIRD_PARTY_NOTICES.md`. No krkrsdl3 source is copied
/// here; this implementation remains under the repository's
/// AGPL-3.0-or-later license.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EmoteAffine {
    m11: f32,
    m12: f32,
    m21: f32,
    m22: f32,
    tx: f32,
    ty: f32,
}

impl EmoteAffine {
    const fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    fn then(self, rhs: Self) -> Self {
        Self {
            m11: self.m11 * rhs.m11 + self.m12 * rhs.m21,
            m12: self.m11 * rhs.m12 + self.m12 * rhs.m22,
            m21: self.m21 * rhs.m11 + self.m22 * rhs.m21,
            m22: self.m21 * rhs.m12 + self.m22 * rhs.m22,
            tx: self.m11 * rhs.tx + self.m12 * rhs.ty + self.tx,
            ty: self.m21 * rhs.tx + self.m22 * rhs.ty + self.ty,
        }
    }

    fn apply(self, point: [f32; 2]) -> [f32; 2] {
        [
            self.m11 * point[0] + self.m12 * point[1] + self.tx,
            self.m21 * point[0] + self.m22 * point[1] + self.ty,
        ]
    }

    fn inverse_apply(self, point: [f32; 2]) -> [f32; 2] {
        let dx = point[0] - self.tx;
        let dy = point[1] - self.ty;
        let det = self.m11 * self.m22 - self.m12 * self.m21;
        if !det.is_finite() || det.abs() <= f32::EPSILON {
            return [dx, dy];
        }
        let inv = 1.0 / det;
        [
            (self.m22 * dx - self.m12 * dy) * inv,
            (-self.m21 * dx + self.m11 * dy) * inv,
        ]
    }

    fn as_array(self) -> [f32; 6] {
        [self.m11, self.m12, self.m21, self.m22, self.tx, self.ty]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameLinearState {
    flip_x: bool,
    flip_y: bool,
    rotation_degrees: f32,
    scale_x: f32,
    scale_y: f32,
    shear_x: f32,
    shear_y: f32,
}

impl Default for FrameLinearState {
    fn default() -> Self {
        Self {
            flip_x: false,
            flip_y: false,
            rotation_degrees: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            shear_x: 0.0,
            shear_y: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
struct TransformContext {
    linear: EmoteAffine,
    state: FrameLinearState,
    location: [f32; 3],
    coordinate: i64,
    opacity: f32,
    /// The ancestor selected by the native `inheritParent` walk. A layer
    /// carrying bit 0x400000 is transparent and does not replace this source
    /// for its descendants.
    inherit_source: InheritSource,
    /// Synthetic layer-0 state of the enclosing motion player. Native
    /// partial-inherit layers remove the selected root channels while building
    /// their local matrix, then multiply this root matrix back in.
    motion_root: InheritSource,
    motion_independent_layer_inherit: bool,
}

#[derive(Clone, Debug)]
struct InheritSource {
    linear: EmoteAffine,
    state: FrameLinearState,
    location: [f32; 3],
    coordinate: i64,
    opacity: f32,
    /// Native meshSyncChildMask low-bit (coord/angle/zoom) channel state of the
    /// selected ancestor. Replaced at every inheritance-source boundary rather
    /// than accumulated.
    mesh_sync: Option<std::sync::Arc<MeshSyncState>>,
}

/// Parent mesh patch state consumed by a child's coord/angle/zoom channels
/// (native sub_10335500 semantics, see `apply_mesh_sync_to_content`).
#[derive(Clone, Debug)]
struct MeshSyncState {
    points: Vec<f32>,
    side: usize,
    /// Authored pixel domain of the parent surface: `[left, top, w, h]`.
    domain: [f32; 4],
    mask: i64,
    coordinate: i64,
}

impl MeshSyncState {
    /// Native patch sampling for coord/angle/zoom warping deliberately does
    /// not clamp: the +-0.0001 Jacobian diamond legitimately crosses edges.
    fn warp_point(&self, point: [f32; 2]) -> Option<[f32; 2]> {
        let [left, top, width, height] = self.domain;
        if !width.is_finite()
            || !height.is_finite()
            || width.abs() <= f32::EPSILON
            || height.abs() <= f32::EPSILON
        {
            return None;
        }
        let u = (point[0] - left) / width;
        let v = (point[1] - top) / height;
        let mapped = sample_grid_unclamped(&self.points, self.side, [u, v]);
        Some([left + mapped[0] * width, top + mapped[1] * height])
    }
}

/// Applies the native meshSyncChild coord/angle/zoom channels to the child's
/// evaluated frame content before its transform is composed.
fn apply_mesh_sync_to_content(
    content: &mut EmoteFrameContent,
    sync: &MeshSyncState,
    inherit_mask: i64,
) {
    let Some(coord) = content.coord.as_mut() else {
        return;
    };
    let use_xz = sync.coordinate != 0;
    let point = if use_xz {
        [coord.first().copied().unwrap_or(0.0), coord.get(2).copied().unwrap_or(0.0)]
    } else {
        [coord.first().copied().unwrap_or(0.0), coord.get(1).copied().unwrap_or(0.0)]
    };
    let Some(mapped) = sync.warp_point(point) else {
        return;
    };

    // The native code samples a diamond around the original child coordinate
    // at +/-0.0001 to recover the local mesh Jacobian with finite differences.
    const EPS: f32 = 0.0001;
    let xm = sync.warp_point([point[0] - EPS, point[1]]);
    let xp = sync.warp_point([point[0] + EPS, point[1]]);
    let ym = sync.warp_point([point[0], point[1] - EPS]);
    let yp = sync.warp_point([point[0], point[1] + EPS]);

    if (sync.mask & 0x2) != 0 && (inherit_mask & 0x10) != 0 {
        if let (Some(xm), Some(xp), Some(ym), Some(yp)) = (xm, xp, ym, yp) {
            let dx = [xp[0] - xm[0], xp[1] - xm[1]];
            let dy = [yp[0] - ym[0], yp[1] - ym[1]];
            if dx[0].is_finite() && dx[1].is_finite() && dy[0].is_finite() && dy[1].is_finite() {
                let ax = dx[1].atan2(dx[0]);
                let ay = (-dy[0]).atan2(dy[1]);
                let delta = ((ax + ay) * 0.5).to_degrees();
                *content.angle.get_or_insert(0.0) += delta;
            }
        }
    }

    if (sync.mask & 0x4) != 0 && (inherit_mask & 0x60) != 0 {
        if let (Some(xm), Some(xp), Some(ym), Some(yp)) = (xm, xp, ym, yp) {
            // Two triangle areas of the warped diamond, then
            // sqrt(2 * area) / 0.0002 — not the Jacobian determinant.
            let tri_area = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * 0.5
            };
            let area = tri_area(xm, xp, ym) + tri_area(xm, xp, yp);
            let scale = (2.0 * area).sqrt() / (2.0 * EPS);
            if scale.is_finite() {
                if (inherit_mask & 0x20) != 0 {
                    content.scale_x *= scale;
                }
                if (inherit_mask & 0x40) != 0 {
                    content.scale_y *= scale;
                }
            }
        }
    }

    if (sync.mask & 0x1) != 0 {
        if !coord.is_empty() {
            coord[0] = mapped[0];
        }
        if use_xz {
            if coord.len() > 2 {
                coord[2] = mapped[1];
            }
        } else if coord.len() > 1 {
            coord[1] = mapped[1];
        }
    }
}

fn sample_grid_unclamped(points: &[f32], side: usize, normalized: [f32; 2]) -> [f32; 2] {
    if side == 4 {
        return sample_bezier_patch_unclamped(points, normalized);
    }
    sample_grid(points, side, normalized)
}

fn sample_bezier_patch_unclamped(points: &[f32], normalized: [f32; 2]) -> [f32; 2] {
    let basis = |value: f32| {
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

impl Default for InheritSource {
    fn default() -> Self {
        Self {
            linear: EmoteAffine::identity(),
            state: FrameLinearState::default(),
            location: [0.0; 3],
            coordinate: 0,
            opacity: 1.0,
            mesh_sync: None,
        }
    }
}

impl Default for TransformContext {
    fn default() -> Self {
        Self {
            linear: EmoteAffine::identity(),
            state: FrameLinearState::default(),
            location: [0.0; 3],
            coordinate: 0,
            opacity: 1.0,
            inherit_source: InheritSource::default(),
            motion_root: InheritSource::default(),
            motion_independent_layer_inherit: false,
        }
    }
}

#[derive(Clone, Debug)]
struct MeshDeformer {
    #[cfg(test)]
    combine: bool,
    transform: EmoteAffine,
    #[cfg(test)]
    translation: [f32; 2],
    #[cfg(test)]
    angle: f32,
    size: [f32; 2],
    origin: [f32; 2],
    offset: [f32; 2],
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
    /// Final model-space affine transform. `translation` and `angle` are kept
    /// as compatibility/debug fields; drawing must use this matrix.
    pub world_transform: [f32; 6],
    pub frame_offset: [f32; 2],
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

/// Per-instance local frame history. Only layers containing a HOLD key need
/// storage; ordinary layers remain stateless and do not allocate history.
#[derive(Default, Debug)]
pub struct EmoteEvaluationHistory {
    model: std::sync::Weak<()>,
    generation: u64,
    frames: HashMap<Vec<usize>, (u64, EmoteFrameContent)>,
}

impl EmoteEvaluationHistory {
    fn begin(&mut self, model: &EmoteModel) {
        let identity = std::sync::Arc::downgrade(model.evaluation_identity());
        if !self.model.ptr_eq(&identity) || self.generation == u64::MAX {
            self.frames.clear();
            self.generation = 0;
        }
        self.model = identity;
        self.generation += 1;
    }
}

pub struct EmoteMotionEvaluator<'a> {
    model: &'a EmoteModel,
}

impl<'a> EmoteMotionEvaluator<'a> {
    pub fn new(model: &'a EmoteModel) -> Self {
        Self { model }
    }

    pub fn evaluate_base(&self, state: &EmoteRenderState) -> Result<Vec<EmoteDrawItem>> {
        self.evaluate_base_with_history(state, &mut EmoteEvaluationHistory::default())
    }

    pub fn evaluate_base_with_history(
        &self,
        state: &EmoteRenderState,
        history: &mut EmoteEvaluationHistory,
    ) -> Result<Vec<EmoteDrawItem>> {
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
        self.evaluate_with_history(character, motion, state, history)
    }

    pub fn evaluate(
        &self,
        character: &str,
        motion: &str,
        state: &EmoteRenderState,
    ) -> Result<Vec<EmoteDrawItem>> {
        self.evaluate_with_history(
            character,
            motion,
            state,
            &mut EmoteEvaluationHistory::default(),
        )
    }

    pub fn evaluate_with_history(
        &self,
        character: &str,
        motion: &str,
        state: &EmoteRenderState,
        history: &mut EmoteEvaluationHistory,
    ) -> Result<Vec<EmoteDrawItem>> {
        history.begin(self.model);
        let mut history_path = Vec::new();
        let mut resolved_state = state.clone();
        self.model
            .apply_selector_controls(&mut resolved_state.variables);
        self.model
            .apply_clamp_controls(&mut resolved_state.variables);
        let mut items = Vec::new();
        let mut stack = BTreeSet::new();
        let mut deformers = Vec::new();
        self.visit_motion(
            character,
            motion,
            resolved_state.motion_time,
            &resolved_state,
            TransformContext::default(),
            0,
            &[],
            &[],
            &mut stack,
            &mut deformers,
            &mut items,
            history,
            &mut history_path,
        )?;
        history
            .frames
            .retain(|_, (generation, _)| *generation == history.generation);
        items.sort_by(|left, right| {
            left.translation[2]
                .total_cmp(&right.translation[2])
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
        parent_context: TransformContext,
        depth: usize,
        stencil_mask_layers: &[String],
        order_prefix: &[i64],
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
        history: &mut EmoteEvaluationHistory,
        history_path: &mut Vec<usize>,
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
            && motion_time >= motion.last_time
        {
            motion.loop_time
                + (motion_time - motion.loop_time) % (motion.last_time - motion.loop_time)
        } else {
            motion_time.min(motion.last_time.max(0.0))
        };
        let motion_time = resolve_parameter_time(
            motion.parameter_index,
            motion.inline_parameter.as_ref(),
            &motion.parameters,
            motion_time,
            state,
        );
        let priority = motion.priority_at(motion_time);
        let mut structural_index = 0;
        for layer in &motion.layers {
            self.visit_layer(
                layer,
                &motion.parameters,
                priority,
                motion_time,
                state,
                parent_context.clone(),
                depth,
                stencil_mask_layers,
                order_prefix,
                &mut structural_index,
                stack,
                deformers,
                items,
                history,
                history_path,
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
        priority: Option<&[usize]>,
        motion_time: f32,
        state: &EmoteRenderState,
        parent_context: TransformContext,
        depth: usize,
        parent_stencil_mask_layers: &[String],
        order_prefix: &[i64],
        structural_index: &mut usize,
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
        history: &mut EmoteEvaluationHistory,
        history_path: &mut Vec<usize>,
    ) -> Result<()> {
        let suspended_deformers = (!layer.inherit_shape).then(|| std::mem::take(deformers));
        let mut draw_order = order_prefix.to_vec();
        let index = *structural_index;
        *structural_index += 1;
        // Priority references preorder LayerInfo indices, never authored names.
        // Negate emission ranks for the existing descending key comparator.
        draw_order.push(
            priority
                .and_then(|ranks| ranks.get(index))
                .map_or(index as i64, |rank| -(*rank as i64)),
        );
        history_path.push(layer as *const EmoteLayer as usize);
        let layer_time = resolve_layer_time(layer, parameters, motion_time, state);
        let mut content = sample_content_with_history(layer, layer_time, history, history_path);
        let frame_valid = layer
            .frames
            .iter()
            .rfind(|frame| frame.time <= layer_time)
            .is_some_and(|frame| frame.frame_type != 0);
        // Native meshSyncChild low bits (coord/angle/zoom): the selected
        // ancestor's patch warps this layer's evaluated frame channels before
        // its transform is composed.  The history snapshot stays unwarped so
        // re-evaluation never applies the warp twice.
        if let (Some(sync), Some(content)) = (
            parent_context.inherit_source.mesh_sync.as_deref(),
            content.as_mut(),
        ) {
            apply_mesh_sync_to_content(content, sync, layer.inherit_mask);
        }
        let content_ref = content.as_ref();
        let frame_start = active_frame_start(layer, layer_time).unwrap_or(layer_time);
        let layer_context = apply_layer_transform(parent_context, layer, content_ref);
        // This layer's own patch becomes the coord/angle/zoom sync source for
        // its direct children when the low mask bits are set.
        let own_sync = if layer.mesh_transform == 1 && (layer.mesh_sync_child_mask & 0x7) != 0 {
            content_ref.and_then(|content| {
                let (points, side) = content
                    .mesh
                    .as_ref()
                    .and_then(|mesh| mesh.blend_points.as_deref())
                    .and_then(mesh_patch)?;
                let icon = content.icon.as_deref()?;
                let mut parts = icon.split(':');
                let width = parts.next()?.parse::<f32>().ok()?;
                let height = parts.next()?.parse::<f32>().ok()?;
                let origin_x = parts.next()?.parse::<f32>().ok()?;
                let origin_y = parts.next()?.parse::<f32>().ok()?;
                if parts.next().is_some() || width <= 0.0 || height <= 0.0 {
                    return None;
                }
                Some(std::sync::Arc::new(MeshSyncState {
                    points: points.to_vec(),
                    side,
                    domain: [-origin_x, -origin_y, width, height],
                    mask: layer.mesh_sync_child_mask & 0x7,
                    coordinate: layer.coordinate,
                }))
            })
        } else {
            None
        };
        let mut child_context = layer_context.clone();
        prepare_child_inherit_source(&mut child_context, layer, own_sync);
        let stencil_mask_layers =
            if (layer.stencil_type & 0x4) != 0 && !layer.stencil_mask_layers.is_empty() {
                layer.stencil_mask_layers.as_slice()
            } else {
                parent_stencil_mask_layers
            };

        // Shape-sync layers (meshTransform + native sync bit) propagate their
        // mesh payload to descendants. The payload lives on icon frames as
        // often as on blank helper frames, so both kinds become deformers.
        let shape_sync = layer.mesh_transform == 1 && (layer.mesh_sync_child_mask & 0x8) != 0;
        let deformer = if shape_sync {
            content.as_ref().and_then(|content| {
                let extent = if content.source.as_deref() == Some("blank") {
                    parse_blank_icon(content.icon.as_deref()?)
                } else {
                    let icon_id = content.icon.as_deref()?;
                    let icon = self.model.atlas().icon(icon_id)?;
                    Some([icon.width, icon.height, icon.origin_x, icon.origin_y])
                };
                MeshDeformer::from_content(content, layer_context.clone(), extent?)
            })
        } else {
            None
        };

        if let Some(content) = content.as_ref() {
            self.visit_content(
                &layer.label,
                layer.layer_type,
                frame_valid,
                content,
                layer_time,
                frame_start,
                state,
                layer_context,
                layer.motion_independent_layer_inherit,
                depth,
                stencil_mask_layers,
                &draw_order,
                stack,
                deformers,
                items,
                history,
                history_path,
            )?;
        }

        // The layer's own sprite consumes its mesh through the draw item's
        // authored patch (see apply_deformers); the chain entry exists for
        // descendants only, so it is pushed after the content visit.
        let pushed_deformer = deformer.is_some();
        if let Some(deformer) = deformer {
            deformers.push(deformer);
        }
        for child in &layer.children {
            self.visit_layer(
                child,
                parameters,
                priority,
                motion_time,
                state,
                child_context.clone(),
                depth,
                stencil_mask_layers,
                order_prefix,
                structural_index,
                stack,
                deformers,
                items,
                history,
                history_path,
            )?;
        }
        if pushed_deformer {
            deformers.pop();
        }
        if let Some(mut inherited) = suspended_deformers {
            inherited.append(deformers);
            *deformers = inherited;
        }
        history_path.pop();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_content(
        &self,
        layer_label: &str,
        layer_type: i64,
        frame_valid: bool,
        content: &EmoteFrameContent,
        parent_local_time: f32,
        frame_start: f32,
        state: &EmoteRenderState,
        context: TransformContext,
        motion_independent_layer_inherit: bool,
        depth: usize,
        stencil_mask_layers: &[String],
        draw_order: &[i64],
        stack: &mut BTreeSet<(String, String)>,
        deformers: &mut Vec<MeshDeformer>,
        items: &mut Vec<EmoteDrawItem>,
        history: &mut EmoteEvaluationHistory,
        history_path: &mut Vec<usize>,
    ) -> Result<()> {
        // Native HOLD frames (serialized type 0) leave the layer's decoded
        // state intact for transform/inheritance purposes, but the layer is
        // not drawn and a nested motion reference is not entered.  The held
        // `content` reaching this function is the history snapshot; gate both
        // the icon path and the nested-motion path on the live frame cursor.
        if !frame_valid {
            return Ok(());
        }
        let (Some(source), Some(icon_id)) = (&content.source, &content.icon) else {
            return Ok(());
        };
        if source == "blank" {
            return Ok(());
        }
        if self.model.atlas().textures().contains_key(source) {
            if !matches!(layer_type, 0 | 10 | 12) {
                return Ok(());
            }
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
                context,
                stencil_mask_layers,
                draw_order,
            );
            apply_deformers(&mut item, deformers);
            items.push(item);
            return Ok(());
        }

        if layer_type != 3 {
            return Ok(());
        }

        // Nested motions are local players. Their clock starts at the active
        // parent keyframe, rather than at the enclosing motion's absolute
        // time; this is the offset used by both Eluna and emoteplayer.
        let referenced_time = parent_local_time - frame_start
            + content
                .motion
                .as_ref()
                .map(|motion| motion.time_offset)
                .unwrap_or(0.0);
        let mut nested_context = context.clone();
        let root = InheritSource {
            linear: EmoteAffine {
                tx: 0.0,
                ty: 0.0,
                ..context.linear
            },
            state: context.state,
            location: context.location,
            coordinate: context.coordinate,
            opacity: context.opacity,
            mesh_sync: None,
        };
        // A nested layer starts a new MMotionPlayer. Its enclosing layer is
        // the synthetic layer-0 root used for native partial-inherit
        // compensation; independent nested players skip that compensation.
        nested_context.motion_root = root.clone();
        nested_context.inherit_source = root;
        nested_context.motion_independent_layer_inherit = motion_independent_layer_inherit;
        // Nested players keep the currently active shape-sync chain.  The
        // chain describes the parent surface on which this local player is
        // attached; dropping it at a type-3 boundary makes the iris ignore
        // head/face deformation and visibly drift or collapse inside the eye.
        self.visit_motion(
            source,
            icon_id,
            referenced_time,
            state,
            nested_context,
            depth + 1,
            stencil_mask_layers,
            draw_order,
            stack,
            deformers,
            items,
            history,
            history_path,
        )
    }
}

impl MeshDeformer {
    /// `extent` is `[width, height, origin_x, origin_y]` of the layer surface
    /// (from the blank descriptor or the atlas icon).
    fn from_content(
        content: &EmoteFrameContent,
        context: TransformContext,
        extent: [f32; 4],
    ) -> Option<Self> {
        let [width, height, origin_x, origin_y] = extent;
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
            #[cfg(test)]
            combine: false,
            transform: EmoteAffine {
                tx: context.location[0],
                ty: context.location[1],
                ..context.linear
            },
            #[cfg(test)]
            translation: [context.location[0], context.location[1]],
            #[cfg(test)]
            angle: context.state.rotation_degrees,
            size: [width, height],
            origin: [origin_x, origin_y],
            offset: content.offset,
            points,
            side,
        })
    }

    fn deform(&self, point: [f32; 2]) -> [f32; 2] {
        let local = self.transform.inverse_apply(point);
        let normalized = [
            (local[0] + self.origin[0] + self.offset[0]) / self.size[0],
            (local[1] + self.origin[1] + self.offset[1]) / self.size[1],
        ];
        // Shape-sync meshes own an authored pixel domain. Native E-Mote leaves
        // points outside that rectangle untouched; clamping them into the
        // edge control points would pull unrelated layers (notably the iris)
        // toward the mesh boundary.
        if !normalized[0].is_finite()
            || !normalized[1].is_finite()
            || !(0.0..=1.0).contains(&normalized[0])
            || !(0.0..=1.0).contains(&normalized[1])
        {
            return point;
        }
        let warped = sample_grid(&self.points, self.side, normalized);
        let warped_local = [
            warped[0] * self.size[0] - self.origin[0] - self.offset[0],
            warped[1] * self.size[1] - self.origin[1] - self.offset[1],
        ];
        self.transform.apply(warped_local)
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

fn apply_deformers(item: &mut EmoteDrawItem, deformers: &[MeshDeformer]) {
    if item.atlas_rect[2] <= 0.0 || item.atlas_rect[3] <= 0.0 {
        return;
    }
    let source_patch = item
        .mesh
        .as_ref()
        .and_then(|mesh| mesh.blend_points.as_deref())
        .and_then(mesh_patch);
    if source_patch.is_none() && deformers.is_empty() {
        return;
    }
    // Authored 4x4 patches are already the native E-Mote control grid. Keep
    // them intact when no ancestor surface has to be applied; expanding every
    // static patch to 8x8 creates four times as many vertices for no visual
    // gain and used to dominate scene rebuilds.
    if deformers.is_empty() {
        return;
    }
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
                point[0] * item.atlas_rect[2] - item.origin[0] - item.frame_offset[0],
                point[1] * item.atlas_rect[3] - item.origin[1] - item.frame_offset[1],
            ];
            let item_transform = affine_from_array(item.world_transform);
            let mut world = item_transform.apply(local);
            // Apply the authored ancestor chain in local space.  This keeps
            // each patch's domain intact while avoiding intermediate grids.
            world = deform_chain(world, deformers);
            let final_local = item_transform.inverse_apply(world);
            deformed.push(
                (final_local[0] + item.origin[0] + item.frame_offset[0]) / item.atlas_rect[2],
            );
            deformed.push(
                (final_local[1] + item.origin[1] + item.frame_offset[1]) / item.atlas_rect[3],
            );
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

fn deform_chain(mut point: [f32; 2], deformers: &[MeshDeformer]) -> [f32; 2] {
    // Each ancestor owns its own authored domain.  Apply the nearest patch
    // first, then walk outward through the chain.  `meshCombine` is resolved
    // while building the layer's effective patch; combining these already
    // resolved ancestor patches again here changes the sampling domain and
    // visibly shrinks eye/iris sprites.
    for deformer in deformers.iter().rev() {
        point = deformer.deform(point);
    }
    point
}

fn affine_from_array(values: [f32; 6]) -> EmoteAffine {
    EmoteAffine {
        m11: values[0],
        m12: values[1],
        m21: values[2],
        m22: values[3],
        tx: values[4],
        ty: values[5],
    }
}

fn mesh_patch(points: &[f32]) -> Option<(&[f32], usize)> {
    if points.is_empty() || !points.len().is_multiple_of(2) {
        return None;
    }
    let point_count = points.len() / 2;
    let side = (point_count as f32).sqrt() as usize;
    (side >= 2 && side * side == point_count).then_some((points, side))
}

#[cfg(test)]
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

#[cfg(test)]
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
            && self.transform == other.transform
            && self.offset == other.offset
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

#[cfg(test)]
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

fn sample_content_with_history(
    layer: &EmoteLayer,
    time: f32,
    history: &mut EmoteEvaluationHistory,
    path: &[usize],
) -> Option<EmoteFrameContent> {
    if !layer.frames.iter().any(|frame| frame.frame_type == 0) {
        return sample_content(layer, time);
    }
    let frame = layer.frames.iter().rfind(|frame| frame.time <= time)?;
    if frame.frame_type == 0 {
        let (generation, content) = history.frames.get_mut(path)?;
        *generation = history.generation;
        return Some(content.clone());
    }
    let content = sample_content(layer, time)?;
    history
        .frames
        .insert(path.to_vec(), (history.generation, content.clone()));
    Some(content)
}

fn sample_content(layer: &EmoteLayer, time: f32) -> Option<EmoteFrameContent> {
    let index = layer.frames.iter().rposition(|frame| frame.time <= time)?;
    let frame = &layer.frames[index];
    match frame.frame_type {
        // A fresh evaluator has no decoded local state to hold. Playback
        // instances supply history through sample_content_with_history.
        0 => None,
        3 => {
            let Some(next) = layer.frames.get(index + 1) else {
                return frame.content.clone();
            };
            if next.frame_type == 0 {
                return frame.content.clone();
            }
            let (Some(from), Some(to)) = (&frame.content, &next.content) else {
                return frame.content.clone();
            };
            let span = next.time - frame.time;
            if span <= f32::EPSILON {
                next.content.clone()
            } else {
                let mut elapsed = (time - frame.time).max(0.0);
                if let Some(interval) = from.time_interval.filter(|value| *value > 0.0) {
                    elapsed = (elapsed / interval).trunc() * interval;
                }
                Some(interpolate_content(
                    from,
                    to,
                    (elapsed / span).clamp(0.0, 1.0),
                ))
            }
        }
        _ => frame.content.clone(),
    }
}

fn active_frame_start(layer: &EmoteLayer, time: f32) -> Option<f32> {
    layer
        .frames
        .iter()
        .rfind(|frame| frame.time <= time)
        .map(|frame| frame.time)
}

fn resolve_layer_time(
    layer: &EmoteLayer,
    parameters: &[EmoteMotionParameter],
    motion_time: f32,
    state: &EmoteRenderState,
) -> f32 {
    resolve_parameter_time(
        layer.parameter_index,
        layer.inline_parameter.as_ref(),
        parameters,
        motion_time,
        state,
    )
}

fn resolve_parameter_time(
    index: Option<usize>,
    inline: Option<&EmoteMotionParameter>,
    parameters: &[EmoteMotionParameter],
    motion_time: f32,
    state: &EmoteRenderState,
) -> f32 {
    if index.is_none() && inline.is_none() {
        return motion_time;
    }
    inline
        .or_else(|| index.and_then(|index| parameters.get(index)))
        .and_then(|parameter| {
            parameter.frame_for_value(state.variables.get(&parameter.id).copied().unwrap_or(0.0))
        })
        .unwrap_or(0.0)
}

fn interpolate_content(
    from: &EmoteFrameContent,
    to: &EmoteFrameContent,
    ratio: f32,
) -> EmoteFrameContent {
    let choose_to = ratio >= 1.0;
    let coord_ratio = curve_ratio(
        from.curves.as_deref().and_then(|c| c.coordinate.as_ref()),
        ratio,
    );
    let angle_ratio = curve_ratio(from.curves.as_deref().and_then(|c| c.angle.as_ref()), ratio);
    let zoom_ratio = curve_ratio(from.curves.as_deref().and_then(|c| c.zoom.as_ref()), ratio);
    let shear_ratio = curve_ratio(from.curves.as_deref().and_then(|c| c.shear.as_ref()), ratio);
    let color_ratio = curve_ratio(from.curves.as_deref().and_then(|c| c.color.as_ref()), ratio);
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
        coord: interpolate_coordinate(
            from.coord.as_deref(),
            to.coord.as_deref(),
            coord_ratio,
            from.curves.as_deref().and_then(|c| c.path.as_ref()),
        ),
        // Native StepFrame copies the active key's pivot; ox/oy are not
        // tween channels. Interpolating them moves otherwise fixed parts.
        offset: from.offset,
        flip_x: if choose_to { to.flip_x } else { from.flip_x },
        flip_y: if choose_to { to.flip_y } else { from.flip_y },
        scale_x: from.scale_x + (to.scale_x - from.scale_x) * zoom_ratio,
        scale_y: from.scale_y + (to.scale_y - from.scale_y) * zoom_ratio,
        shear_x: from.shear_x + (to.shear_x - from.shear_x) * shear_ratio,
        shear_y: from.shear_y + (to.shear_y - from.shear_y) * shear_ratio,
        angle: interpolate_angle(from.angle, to.angle, angle_ratio),
        opacity: interpolate_number(from.opacity, to.opacity, ratio, 255.0),
        blend_mode: if choose_to {
            to.blend_mode
        } else {
            from.blend_mode
        },
        color: interpolate_list(
            from.color.as_deref(),
            to.color.as_deref(),
            color_ratio,
            255.0,
        ),
        mesh: interpolate_mesh(from.mesh.as_ref(), to.mesh.as_ref(), ratio),
        motion: interpolate_motion(from.motion.as_ref(), to.motion.as_ref(), ratio),
        curves: from.curves.clone(),
        time_interval: from.time_interval,
    }
}

fn curve_ratio(curve: Option<&EmoteEasingCurve>, ratio: f32) -> f32 {
    curve
        .and_then(|curve| evaluate_curve(curve, ratio))
        .unwrap_or(ratio)
}

fn evaluate_curve(curve: &EmoteEasingCurve, value: f32) -> Option<f32> {
    if curve.x.len() < 2 || curve.x.len() != curve.y.len() {
        return None;
    }
    let value = value.clamp(0.0, 1.0);
    if curve.p.len() == curve.x.len() {
        let mut index = 0;
        while index + 1 < curve.x.len() - 1 && value > curve.x[index + 1] {
            index += 1;
        }
        while index > 0 && value < curve.x[index] {
            index -= 1;
        }
        let x0 = curve.x[index];
        let x1 = curve.x[index + 1];
        let span = x1 - x0;
        if span.abs() <= f32::EPSILON {
            return curve.y.get(index).copied();
        }
        let u = (value - x0) / span;
        let v = 1.0 - u;
        let cubic_u = u * u * u - u;
        let cubic_v = v * v * v - v;
        return Some(
            v * curve.y[index]
                + u * curve.y[index + 1]
                + span * span * (cubic_u * curve.p[index + 1] + cubic_v * curve.p[index]) / 6.0,
        );
    }
    if curve.x.len() == 4 {
        // Older E-Mote writers store four Bezier controls directly. Their x
        // controls describe the input curve; solve x(u)=value with a few
        // Newton steps, then return y(u). This costs only on authored curve
        // frames and leaves the common linear path allocation-free.
        let mut u = value;
        for _ in 0..5 {
            let x = cubic_bezier(curve.x[0], curve.x[1], curve.x[2], curve.x[3], u);
            let dx = cubic_bezier_derivative(curve.x[0], curve.x[1], curve.x[2], curve.x[3], u);
            if dx.abs() <= 1.0e-5 {
                break;
            }
            u = (u - (x - value) / dx).clamp(0.0, 1.0);
        }
        return Some(cubic_bezier(
            curve.y[0], curve.y[1], curve.y[2], curve.y[3], u,
        ));
    }
    let mut index = 0;
    while index + 1 < curve.x.len() - 1 && value > curve.x[index + 1] {
        index += 1;
    }
    let span = curve.x[index + 1] - curve.x[index];
    Some(if span.abs() <= f32::EPSILON {
        curve.y[index]
    } else {
        curve.y[index] + (curve.y[index + 1] - curve.y[index]) * (value - curve.x[index]) / span
    })
}

fn cubic_bezier(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
}

fn cubic_bezier_derivative(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * (b - a) + 6.0 * u * t * (c - b) + 3.0 * t * t * (d - c)
}

fn interpolate_coordinate(
    from: Option<&[f32]>,
    to: Option<&[f32]>,
    ratio: f32,
    path: Option<&EmoteBezierPath>,
) -> Option<Vec<f32>> {
    // Native interpolation always yields a coordinate: a missing channel
    // tweens from [0,0,0] and stays present, so a parent meshSync coord warp
    // still displaces helper layers that author no coordinate at all.
    let mut result = interpolate_list(from, to, ratio, 0.0).unwrap_or_else(|| vec![0.0; 3]);
    if let Some(path) = path.and_then(|path| evaluate_path(path, ratio)) {
        if result.len() >= 2 {
            result[0] = path[0];
            result[1] = path[1];
        }
        if result.len() >= 3
            && let (Some(a), Some(b)) = (from.and_then(|v| v.get(2)), to.and_then(|v| v.get(2)))
        {
            result[2] = a + (b - a) * ratio;
        }
    }
    Some(result)
}

fn evaluate_path(path: &EmoteBezierPath, value: f32) -> Option<[f32; 2]> {
    if path.t.len() < 2 || path.splines.is_empty() {
        return None;
    }
    let segment_count = path.t.len() - 1;
    let segment = (0..segment_count)
        .find(|&index| value <= path.t[index + 1])
        .unwrap_or(segment_count - 1);
    let span = path.t[segment + 1] - path.t[segment];
    let local = if span.abs() <= f32::EPSILON {
        0.0
    } else {
        (value - path.t[segment]) / span
    };
    let u = evaluate_curve(path.splines.get(segment)?, local).unwrap_or(local);
    let base = segment * 3;
    if path.x.len() < base + 4 || path.y.len() < base + 4 {
        return None;
    }
    Some([
        cubic_bezier(
            path.x[base],
            path.x[base + 1],
            path.x[base + 2],
            path.x[base + 3],
            u,
        ),
        cubic_bezier(
            path.y[base],
            path.y[base + 1],
            path.y[base + 2],
            path.y[base + 3],
            u,
        ),
    ])
}

fn interpolate_number(from: Option<f32>, to: Option<f32>, ratio: f32, default: f32) -> Option<f32> {
    match (from, to) {
        (Some(from), Some(to)) => Some(from + (to - from) * ratio),
        (Some(from), None) => Some(from + (default - from) * ratio),
        (None, Some(to)) => Some(default + (to - default) * ratio),
        (None, None) => None,
    }
}

fn interpolate_angle(from: Option<f32>, to: Option<f32>, ratio: f32) -> Option<f32> {
    let a = from.unwrap_or(0.0);
    let mut b = to.unwrap_or(0.0);
    let delta = b - a;
    if delta > 180.0 {
        b -= 360.0;
    } else if delta < -180.0 {
        b += 360.0;
    }
    Some((a + (b - a) * ratio).rem_euclid(360.0))
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

fn normalized_transform_order(order: &[i64]) -> [i64; 4] {
    if order.len() == 4 {
        let mut seen = [false; 4];
        let mut result = [0; 4];
        let mut valid = true;
        for (index, value) in order.iter().copied().enumerate() {
            if !(0..=3).contains(&value) || seen[value as usize] {
                valid = false;
                break;
            }
            seen[value as usize] = true;
            result[index] = value;
        }
        if valid {
            return result;
        }
    }
    // Native files overwhelmingly omit transformOrder and use this order.
    [0, 3, 2, 1]
}

fn build_linear_transform(order: &[i64], state: FrameLinearState) -> EmoteAffine {
    let mut linear = EmoteAffine::identity();
    for stage in normalized_transform_order(order) {
        let op = match stage {
            0 => EmoteAffine {
                m11: if state.flip_x { -1.0 } else { 1.0 },
                m12: 0.0,
                m21: 0.0,
                m22: if state.flip_y { -1.0 } else { 1.0 },
                tx: 0.0,
                ty: 0.0,
            },
            1 => {
                let (sin, cos) = state.rotation_degrees.to_radians().sin_cos();
                EmoteAffine {
                    m11: cos,
                    m12: -sin,
                    m21: sin,
                    m22: cos,
                    tx: 0.0,
                    ty: 0.0,
                }
            }
            2 => EmoteAffine {
                m11: finite_or(state.scale_x, 1.0),
                m12: 0.0,
                m21: 0.0,
                m22: finite_or(state.scale_y, 1.0),
                tx: 0.0,
                ty: 0.0,
            },
            3 => EmoteAffine {
                m11: 1.0,
                m12: finite_or(state.shear_x, 0.0),
                m21: finite_or(state.shear_y, 0.0),
                m22: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
            _ => unreachable!(),
        };
        linear = op.then(linear);
    }
    linear
}

fn inherit_linear_state(
    own: FrameLinearState,
    parent: FrameLinearState,
    mask: i64,
) -> FrameLinearState {
    FrameLinearState {
        flip_x: own.flip_x ^ ((mask & 0x4) != 0 && parent.flip_x),
        flip_y: own.flip_y ^ ((mask & 0x8) != 0 && parent.flip_y),
        rotation_degrees: own.rotation_degrees
            + if mask & 0x10 != 0 {
                parent.rotation_degrees
            } else {
                0.0
            },
        scale_x: own.scale_x
            * if mask & 0x20 != 0 {
                parent.scale_x
            } else {
                1.0
            },
        scale_y: own.scale_y
            * if mask & 0x40 != 0 {
                parent.scale_y
            } else {
                1.0
            },
        shear_x: own.shear_x
            + if mask & 0x80 != 0 {
                parent.shear_x
            } else {
                0.0
            },
        shear_y: own.shear_y
            + if mask & 0x100 != 0 {
                parent.shear_y
            } else {
                0.0
            },
    }
}

fn apply_layer_transform(
    parent: TransformContext,
    layer: &EmoteLayer,
    content: Option<&EmoteFrameContent>,
) -> TransformContext {
    let mask = layer.inherit_mask;
    let content = content.cloned().unwrap_or_default();
    let own = FrameLinearState {
        flip_x: content.flip_x,
        flip_y: content.flip_y,
        rotation_degrees: content.angle.unwrap_or(0.0),
        scale_x: finite_or(content.scale_x, 1.0),
        scale_y: finite_or(content.scale_y, 1.0),
        shear_x: finite_or(content.shear_x, 0.0),
        shear_y: finite_or(content.shear_y, 0.0),
    };
    let source = parent.inherit_source.clone();
    let inherited = inherit_linear_state(own, source.state, mask);
    let own_linear = build_linear_transform(&layer.transform_order, own);
    let linear = if mask & 0x1fc == 0x1fc {
        source.linear.then(own_linear)
    } else if !parent.motion_independent_layer_inherit {
        let relative = remove_motion_root_linear_state(inherited, parent.motion_root.state, mask);
        parent
            .motion_root
            .linear
            .then(build_linear_transform(&layer.transform_order, relative))
    } else {
        build_linear_transform(&layer.transform_order, inherited)
    };
    let delta = content.coord.as_deref().unwrap_or(&[0.0, 0.0, 0.0]);
    let x = finite_or(delta.first().copied().unwrap_or(0.0), 0.0);
    let y = finite_or(delta.get(1).copied().unwrap_or(0.0), 0.0);
    let z = finite_or(delta.get(2).copied().unwrap_or(0.0), 0.0);
    let mapped = if source.coordinate != 0 {
        let p = source.linear.apply([x, z]);
        [
            source.location[0] + p[0],
            source.location[1] + y,
            source.location[2] + p[1],
        ]
    } else {
        let p = source.linear.apply([x, y]);
        [
            source.location[0] + p[0],
            source.location[1] + p[1],
            source.location[2] + z,
        ]
    };
    let opacity_base = if mask & 0x400 != 0 {
        source.opacity
    } else if !parent.motion_independent_layer_inherit {
        parent.motion_root.opacity
    } else {
        1.0
    };
    TransformContext {
        linear,
        state: inherited,
        location: mapped,
        coordinate: layer.coordinate,
        opacity: opacity_base * normalized_opacity(content.opacity),
        inherit_source: parent.inherit_source.clone(),
        motion_root: parent.motion_root.clone(),
        motion_independent_layer_inherit: parent.motion_independent_layer_inherit,
    }
}

fn remove_motion_root_linear_state(
    mut state: FrameLinearState,
    root: FrameLinearState,
    inherit_mask: i64,
) -> FrameLinearState {
    // Native partial-inherit nested players remove only channels selected by
    // inheritMask before rebuilding their local matrix. Flips are XOR,
    // rotation/shear are additive, and zoom is multiplicative.
    if inherit_mask & 0x4 != 0 {
        state.flip_x ^= root.flip_x;
    }
    if inherit_mask & 0x8 != 0 {
        state.flip_y ^= root.flip_y;
    }
    if inherit_mask & 0x10 != 0 {
        state.rotation_degrees -= root.rotation_degrees;
    }
    if inherit_mask & 0x20 != 0 {
        if root.scale_x.abs() > f32::EPSILON {
            state.scale_x /= root.scale_x;
        }
    }
    if inherit_mask & 0x40 != 0 {
        if root.scale_y.abs() > f32::EPSILON {
            state.scale_y /= root.scale_y;
        }
    }
    if inherit_mask & 0x80 != 0 {
        state.shear_x -= root.shear_x;
    }
    if inherit_mask & 0x100 != 0 {
        state.shear_y -= root.shear_y;
    }
    state
}

fn prepare_child_inherit_source(
    context: &mut TransformContext,
    layer: &EmoteLayer,
    mesh_sync: Option<std::sync::Arc<MeshSyncState>>,
) {
    // 0x400000 marks a transparent parent. Descendants continue to inherit
    // from the source selected above this layer.
    if layer.inherit_mask & 0x0040_0000 != 0 {
        return;
    }
    context.inherit_source = InheritSource {
        linear: EmoteAffine {
            tx: 0.0,
            ty: 0.0,
            ..context.linear
        },
        state: context.state,
        location: context.location,
        coordinate: context.coordinate,
        opacity: context.opacity,
        mesh_sync,
    };
}

#[cfg(test)]
fn add_translation(parent: [f32; 3], parent_angle: f32, coord: Option<&[f32]>) -> [f32; 3] {
    let state = FrameLinearState {
        rotation_degrees: parent_angle,
        ..FrameLinearState::default()
    };
    let linear = build_linear_transform(&[1, 2, 3, 0], state);
    let delta = coord.unwrap_or(&[0.0, 0.0]);
    let p = linear.apply([
        delta.first().copied().unwrap_or(0.0),
        delta.get(1).copied().unwrap_or(0.0),
    ]);
    [
        parent[0] + p[0],
        parent[1] + p[1],
        parent[2] + coord.and_then(|v| v.get(2)).copied().unwrap_or(0.0),
    ]
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
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
    context: TransformContext,
    stencil_mask_layers: &[String],
    draw_order: &[i64],
) -> EmoteDrawItem {
    EmoteDrawItem {
        layer_label: layer_label.to_owned(),
        texture_id: icon.texture_id.clone(),
        icon_id: icon.id.clone(),
        atlas_rect: [icon.left, icon.top, icon.width, icon.height],
        origin: [icon.origin_x, icon.origin_y],
        translation: context.location,
        angle: context.state.rotation_degrees,
        // Keep the matrix in the same form as Eluna's world_transform:
        // translation and linear state only. The icon pivot and frame offset
        // are applied by the host draw command, after mesh points are built.
        world_transform: EmoteAffine {
            tx: context.location[0],
            ty: context.location[1],
            ..context.linear
        }
        .as_array(),
        frame_offset: content.offset,
        opacity: context.opacity,
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
            mesh_transform: 0,
            mesh_sync_child_mask: 0,
            inherit_mask: 0x0200_07fc,
            inherit_shape: true,
            motion_independent_layer_inherit: false,
            transform_order: Vec::new(),
            mesh_combine: false,
            stencil_type: 0,
            stencil_mask_layers: Vec::new(),
            parameter_index: None,
            inline_parameter: None,
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
    fn tween_keeps_active_pivot_until_next_key() {
        let from = EmoteFrameContent {
            offset: [12.0, -4.0],
            ..Default::default()
        };
        let to = EmoteFrameContent {
            offset: [-6.0, 8.0],
            ..Default::default()
        };
        assert_eq!(interpolate_content(&from, &to, 0.5).offset, from.offset);
    }

    #[test]
    fn mesh_sync_coord_warps_child_coordinate_through_parent_patch() {
        // 4x4 patch shifted +10 on x everywhere (a pure translation field).
        let mut points = identity_grid();
        for point in points.chunks_exact_mut(2) {
            point[0] += 10.0 / 100.0;
        }
        let sync = MeshSyncState {
            points,
            side: 4,
            domain: [0.0, 0.0, 100.0, 100.0],
            mask: 0x1,
            coordinate: 0,
        };
        let mut content = EmoteFrameContent {
            coord: Some(vec![20.0, 30.0, 0.0]),
            ..EmoteFrameContent::default()
        };
        apply_mesh_sync_to_content(&mut content, &sync, 0x0200_07fc);
        let coord = content.coord.unwrap();
        assert!((coord[0] - 30.0).abs() < 0.001);
        assert!((coord[1] - 30.0).abs() < 0.001);
    }

    #[test]
    fn mesh_sync_without_coord_bit_leaves_coordinate_untouched() {
        let mut points = identity_grid();
        for point in points.chunks_exact_mut(2) {
            point[0] += 0.1;
        }
        let sync = MeshSyncState {
            points,
            side: 4,
            domain: [0.0, 0.0, 100.0, 100.0],
            mask: 0x6,
            coordinate: 0,
        };
        let mut content = EmoteFrameContent {
            coord: Some(vec![20.0, 30.0, 0.0]),
            ..EmoteFrameContent::default()
        };
        apply_mesh_sync_to_content(&mut content, &sync, 0x0200_07fc);
        assert_eq!(content.coord.unwrap(), vec![20.0, 30.0, 0.0]);
    }

    #[test]
    fn mesh_sync_angle_bit_rotates_with_warped_axes() {
        // Patch rotating the domain around its center by 90 degrees.
        let mut points = identity_grid();
        for (index, point) in points.chunks_exact_mut(2).enumerate() {
            let ix = (index % 4) as f32 / 3.0 - 0.5;
            let iy = (index / 4) as f32 / 3.0 - 0.5;
            point[0] = 0.5 + iy;
            point[1] = 0.5 - ix;
        }
        let sync = MeshSyncState {
            points,
            side: 4,
            domain: [0.0, 0.0, 100.0, 100.0],
            mask: 0x2,
            coordinate: 0,
        };
        let mut content = EmoteFrameContent {
            coord: Some(vec![50.0, 50.0, 0.0]),
            ..EmoteFrameContent::default()
        };
        apply_mesh_sync_to_content(&mut content, &sync, 0x0200_07fc);
        let angle = content.angle.unwrap();
        assert!((angle + 90.0).abs() < 0.7, "angle was {angle}");
        // 坐标位未置位，位置不动。
        assert_eq!(content.coord.unwrap(), vec![50.0, 50.0, 0.0]);
    }

    #[test]
    fn hold_keeps_the_last_sample_and_does_not_share_nested_instance_history() {
        let mut layer = sampled_layer(3);
        layer.frames.push(crate::EmoteLayerFrame {
            time: 20.0,
            frame_type: 0,
            content: None,
        });
        let mut history = EmoteEvaluationHistory::default();
        assert!(sample_content_with_history(&layer, 20.0, &mut history, &[1]).is_none());
        assert_eq!(
            sample_content_with_history(&layer, 5.0, &mut history, &[1])
                .unwrap()
                .coord,
            Some(vec![5.0, 0.0])
        );
        assert_eq!(
            sample_content_with_history(&layer, 10.0, &mut history, &[2])
                .unwrap()
                .coord,
            Some(vec![10.0, 0.0])
        );
        assert_eq!(
            sample_content_with_history(&layer, 20.0, &mut history, &[1])
                .unwrap()
                .coord,
            Some(vec![5.0, 0.0])
        );
        assert_eq!(
            sample_content_with_history(&layer, 20.0, &mut history, &[2])
                .unwrap()
                .coord,
            Some(vec![10.0, 0.0])
        );
        assert_eq!(active_frame_start(&layer, 20.0), Some(20.0));
        // An invalid successor is not an interpolation target, even if its
        // serialized record contains a payload.
        layer.frames[1].frame_type = 0;
        assert_eq!(
            sample_content(&layer, 5.0).unwrap().coord,
            Some(vec![0.0, 0.0])
        );
    }

    #[test]
    fn explicit_motion_binding_drives_unbound_children() {
        let parameter = EmoteMotionParameter {
            id: "pose".into(),
            range_begin: -1.0,
            range_end: 1.0,
            division: 10.0,
            enabled: true,
            discretization: false,
        };
        let state = EmoteRenderState {
            motion_time: 2.0,
            variables: BTreeMap::from([("pose".into(), 0.5)]),
        };
        let time = resolve_parameter_time(None, Some(&parameter), &[], 2.0, &state);
        assert_eq!(time, 7.5);
        let layer = sampled_layer(3);
        assert_eq!(resolve_layer_time(&layer, &[parameter], time, &state), 7.5);
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
    fn channel_inheritance_keeps_scale_and_shear_out_of_partial_masks() {
        let parent = TransformContext {
            linear: build_linear_transform(
                &[0, 3, 2, 1],
                FrameLinearState {
                    rotation_degrees: 20.0,
                    scale_x: 2.0,
                    scale_y: 3.0,
                    shear_x: 0.4,
                    ..FrameLinearState::default()
                },
            ),
            state: FrameLinearState {
                rotation_degrees: 20.0,
                scale_x: 2.0,
                scale_y: 3.0,
                shear_x: 0.4,
                ..FrameLinearState::default()
            },
            inherit_source: InheritSource {
                linear: build_linear_transform(
                    &[0, 3, 2, 1],
                    FrameLinearState {
                        rotation_degrees: 20.0,
                        scale_x: 2.0,
                        scale_y: 3.0,
                        shear_x: 0.4,
                        ..FrameLinearState::default()
                    },
                ),
                state: FrameLinearState {
                    rotation_degrees: 20.0,
                    scale_x: 2.0,
                    scale_y: 3.0,
                    shear_x: 0.4,
                    ..FrameLinearState::default()
                },
                ..InheritSource::default()
            },
            ..TransformContext::default()
        };
        let layer = EmoteLayer {
            label: "partial".into(),
            layer_type: 0,
            coordinate: 0,
            mesh_transform: 0,
            mesh_sync_child_mask: 0,
            inherit_mask: 0x10,
            inherit_shape: true,
            motion_independent_layer_inherit: false,
            transform_order: vec![0, 3, 2, 1],
            mesh_combine: false,
            stencil_type: 0,
            stencil_mask_layers: Vec::new(),
            parameter_index: None,
            inline_parameter: None,
            frames: Vec::new(),
            children: Vec::new(),
        };
        let content = EmoteFrameContent {
            coord: Some(vec![10.0, 0.0]),
            angle: Some(5.0),
            scale_x: 4.0,
            scale_y: 5.0,
            shear_x: 1.0,
            ..EmoteFrameContent::default()
        };
        let result = apply_layer_transform(parent, &layer, Some(&content));
        assert!((result.state.rotation_degrees - 25.0).abs() < 0.001);
        assert!((result.state.scale_x - 4.0).abs() < 0.001);
        assert!((result.state.scale_y - 5.0).abs() < 0.001);
        assert!((result.state.shear_x - 1.0).abs() < 0.001);
    }

    #[test]
    fn xz_coordinate_plane_maps_depth_without_moving_y() {
        let parent = TransformContext {
            coordinate: 1,
            location: [4.0, 5.0, 6.0],
            linear: build_linear_transform(
                &[0, 3, 2, 1],
                FrameLinearState {
                    rotation_degrees: 90.0,
                    ..FrameLinearState::default()
                },
            ),
            inherit_source: InheritSource {
                linear: build_linear_transform(
                    &[0, 3, 2, 1],
                    FrameLinearState {
                        rotation_degrees: 90.0,
                        ..FrameLinearState::default()
                    },
                ),
                state: FrameLinearState::default(),
                location: [4.0, 5.0, 6.0],
                coordinate: 1,
                ..InheritSource::default()
            },
            ..TransformContext::default()
        };
        let layer = EmoteLayer {
            label: "xz".into(),
            layer_type: 0,
            coordinate: 1,
            mesh_transform: 0,
            mesh_sync_child_mask: 0,
            inherit_mask: 0x0200_07fc,
            inherit_shape: true,
            motion_independent_layer_inherit: false,
            transform_order: Vec::new(),
            mesh_combine: false,
            stencil_type: 0,
            stencil_mask_layers: Vec::new(),
            parameter_index: None,
            inline_parameter: None,
            frames: Vec::new(),
            children: Vec::new(),
        };
        let content = EmoteFrameContent {
            coord: Some(vec![2.0, 3.0, 4.0]),
            ..EmoteFrameContent::default()
        };
        let result = apply_layer_transform(parent, &layer, Some(&content));
        assert!((result.location[0] - 0.0).abs() < 0.001);
        assert!((result.location[1] - 8.0).abs() < 0.001);
        assert!((result.location[2] - 8.0).abs() < 0.001);
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
    fn interpolated_coordinate_defaults_to_origin_so_mesh_sync_can_move_it() {
        // A coordinate-less helper layer inside an interpolation span still
        // receives the parent meshSync coord warp: native tweens the missing
        // channel from [0,0,0] and keeps the channel present.
        let content = interpolate_content(
            &EmoteFrameContent::default(),
            &EmoteFrameContent::default(),
            0.5,
        );
        assert_eq!(content.coord.as_deref(), Some([0.0; 3].as_slice()));
    }

    #[test]
    fn omitted_parameter_index_uses_motion_time() {
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

        assert_eq!(resolve_layer_time(&layer, &parameters, 2.0, &state), 2.0);
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
    fn authored_curve_remaps_interpolation_ratio() {
        let from = EmoteFrameContent {
            coord: Some(vec![0.0, 0.0]),
            curves: Some(Box::new(crate::EmoteFrameCurves {
                coordinate: Some(EmoteEasingCurve {
                    x: vec![0.0, 1.0],
                    y: vec![0.0, 0.25],
                    p: Vec::new(),
                }),
                ..crate::EmoteFrameCurves::default()
            })),
            ..EmoteFrameContent::default()
        };
        let to = EmoteFrameContent {
            coord: Some(vec![100.0, 0.0]),
            ..EmoteFrameContent::default()
        };
        let sampled = interpolate_content(&from, &to, 0.5);
        assert!((sampled.coord.unwrap()[0] - 12.5).abs() < 0.001);
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
    fn preserves_authored_mesh_when_no_ancestor_deformer_is_active() {
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
            world_transform: EmoteAffine::identity().as_array(),
            frame_offset: [0.0; 2],
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
        let preserved = item.mesh.unwrap().blend_points.unwrap();
        assert_eq!(preserved, points);
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
                transform: EmoteAffine {
                    tx: 10.0,
                    ty: 20.0,
                    ..EmoteAffine::identity()
                },
                translation: [10.0, 20.0],
                angle: 0.0,
                size: [300.0, 600.0],
                origin: [150.0, 300.0],
                offset: [0.0, 0.0],
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
    fn nested_mesh_patches_sample_nearest_first() {
        let mut scale = identity_grid();
        let mut translate = identity_grid();
        for (index, point) in scale.chunks_exact_mut(2).enumerate() {
            point[0] *= 0.5;
            translate[index * 2] += 0.2;
        }
        let make = |points| MeshDeformer {
            combine: true,
            transform: EmoteAffine::identity(),
            translation: [0.0, 0.0],
            angle: 0.0,
            size: [1.0, 1.0],
            origin: [0.0, 0.0],
            offset: [0.0, 0.0],
            points,
            side: 4,
        };
        // Applying the translation in the inner space and then the outer
        // scale gives 0.5 * (0.4 + 0.2) = 0.3. Pointwise combination would
        // incorrectly produce 0.4 by evaluating both patches at 0.4.
        let point = deform_chain([0.4, 0.4], &[make(scale), make(translate)]);
        assert!((point[0] - 0.3).abs() < 0.001);
        assert!((point[1] - 0.4).abs() < 0.001);
    }

    #[test]
    fn respects_single_hold_and_tween_frame_types() {
        assert_eq!(
            sample_content(&sampled_layer(1), 5.0).unwrap().coord,
            Some(vec![0.0, 0.0])
        );
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
