use std::collections::BTreeMap;

use crate::{PsbValue, Result};

// The field names and frame payload coverage mirror the public
// `krkrsdl3/plugins/emoteplayer` reader. This is an independent Rust parser;
// no krkrsdl3 source is copied. Source and license details are recorded in
// `crates/art3m1s-emote/THIRD_PARTY_NOTICES.md`. This file remains under the
// repository's AGPL-3.0-or-later license.

#[derive(Clone, Debug, Default)]
pub struct EmoteMotionLibrary {
    characters: BTreeMap<String, BTreeMap<String, EmoteMotion>>,
    /// Shared native frame easing table in compact parsed form.
    easing: Vec<EmoteEasingCurve>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmoteEasingCurve {
    /// Native cubic-spline knots. An empty `p` denotes a four-point Bezier
    /// value curve used by older E-Mote writers.
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub p: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmoteBezierPath {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub t: Vec<f32>,
    pub splines: Vec<EmoteEasingCurve>,
}

#[derive(Clone, Debug)]
pub struct EmoteMotion {
    pub character: String,
    pub label: String,
    pub last_time: f32,
    pub loop_time: f32,
    pub parameters: Vec<EmoteMotionParameter>,
    pub parameter_index: Option<usize>,
    pub inline_parameter: Option<EmoteMotionParameter>,
    /// Precompiled emission ranks, indexed by structural layer preorder.
    pub priorities: Vec<EmoteMotionPriority>,
    pub layers: Vec<EmoteLayer>,
    pub layer_index_map: BTreeMap<String, i64>,
}

#[derive(Clone, Debug)]
pub struct EmoteMotionParameter {
    pub id: String,
    pub range_begin: f32,
    pub range_end: f32,
    pub division: f32,
    pub enabled: bool,
    pub discretization: bool,
}

#[derive(Clone, Debug)]
pub struct EmoteLayer {
    pub label: String,
    pub layer_type: i64,
    pub coordinate: i64,
    /// Native mesh transform switch. A mesh payload alone is not enough to
    /// deform descendants; E-Mote only activates shape propagation when this
    /// is enabled together with the shape bit in `mesh_sync_child_mask`.
    pub mesh_transform: i64,
    pub mesh_sync_child_mask: i64,
    /// Complete E-Mote inheritance bit mask.  The old renderer retained only
    /// the inheritShape bit, which made nested eyes/face layers lose the
    /// parent's rotation, zoom and shear state.
    pub inherit_mask: i64,
    pub inherit_shape: bool,
    /// Nested motion players may opt out of the enclosing player's root
    /// matrix compensation. This is the native
    /// `motionIndependentLayerInherit` switch.
    pub motion_independent_layer_inherit: bool,
    /// Native transform stage permutation (flip, angle, zoom, shear).
    pub transform_order: Vec<i64>,
    pub mesh_combine: bool,
    pub stencil_type: i64,
    pub stencil_mask_layers: Vec<String>,
    pub parameter_index: Option<usize>,
    pub inline_parameter: Option<EmoteMotionParameter>,
    pub frames: Vec<EmoteLayerFrame>,
    pub children: Vec<EmoteLayer>,
}

#[derive(Clone, Debug)]
pub struct EmoteMotionPriority {
    pub time: f32,
    pub ranks: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct EmoteLayerFrame {
    pub time: f32,
    pub frame_type: i64,
    pub content: Option<EmoteFrameContent>,
}

#[derive(Clone, Debug)]
pub struct EmoteFrameContent {
    pub mask: i64,
    pub source: Option<String>,
    pub icon: Option<String>,
    pub coord: Option<Vec<f32>>,
    pub offset: [f32; 2],
    pub flip_x: bool,
    pub flip_y: bool,
    pub scale_x: f32,
    pub scale_y: f32,
    pub shear_x: f32,
    pub shear_y: f32,
    pub angle: Option<f32>,
    /// Rare authored interpolation payload kept out of the common frame
    /// allocation. NekoMiko's thousands of ordinary frames therefore remain
    /// compact.
    pub curves: Option<Box<EmoteFrameCurves>>,
    pub time_interval: Option<f32>,
    pub opacity: Option<f32>,
    pub blend_mode: Option<i64>,
    pub color: Option<Vec<f32>>,
    pub mesh: Option<EmoteMesh>,
    pub motion: Option<EmoteMotionRef>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmoteFrameCurves {
    pub coordinate: Option<EmoteEasingCurve>,
    pub angle: Option<EmoteEasingCurve>,
    pub zoom: Option<EmoteEasingCurve>,
    pub shear: Option<EmoteEasingCurve>,
    pub opacity: Option<EmoteEasingCurve>,
    pub color: Option<EmoteEasingCurve>,
    pub path: Option<EmoteBezierPath>,
}

impl Default for EmoteFrameContent {
    fn default() -> Self {
        Self {
            mask: 0,
            source: None,
            icon: None,
            coord: None,
            offset: [0.0; 2],
            flip_x: false,
            flip_y: false,
            scale_x: 1.0,
            scale_y: 1.0,
            shear_x: 0.0,
            shear_y: 0.0,
            angle: None,
            curves: None,
            time_interval: None,
            opacity: None,
            blend_mode: None,
            color: None,
            mesh: None,
            motion: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EmoteMesh {
    pub blend_points: Option<Vec<f32>>,
    pub control_coordinates: Option<Vec<f32>>,
}

#[derive(Clone, Debug, Default)]
pub struct EmoteMotionRef {
    pub mask: i64,
    pub time_offset: f32,
}

impl EmoteMotionLibrary {
    pub fn parse(root: &PsbValue) -> Result<Self> {
        let mut result = Self::default();
        result.easing = root
            .get("easing")
            .and_then(PsbValue::as_list)
            .unwrap_or_default()
            .iter()
            .map(|value| parse_curve(value).unwrap_or_default())
            .collect();
        let Some(characters) = root.get("object").and_then(PsbValue::as_object) else {
            return Ok(result);
        };
        for (character, value) in characters {
            let motions = value
                .get("motion")
                .and_then(PsbValue::as_object)
                .map(|motions| {
                    motions
                        .iter()
                        .filter_map(|(label, value)| {
                            EmoteMotion::parse(character, label, value, &result.easing)
                                .map(|motion| (label.clone(), motion))
                        })
                        .collect()
                })
                .unwrap_or_default();
            result.characters.insert(character.clone(), motions);
        }
        Ok(result)
    }

    pub fn characters(&self) -> &BTreeMap<String, BTreeMap<String, EmoteMotion>> {
        &self.characters
    }

    pub fn motion(&self, character: &str, label: &str) -> Option<&EmoteMotion> {
        self.characters.get(character)?.get(label)
    }

    pub fn easing(&self) -> &[EmoteEasingCurve] {
        &self.easing
    }

    pub fn motion_count(&self) -> usize {
        self.characters.values().map(BTreeMap::len).sum()
    }

    pub fn layer_count(&self) -> usize {
        self.characters
            .values()
            .flat_map(BTreeMap::values)
            .map(|motion| {
                motion
                    .layers
                    .iter()
                    .map(EmoteLayer::node_count)
                    .sum::<usize>()
            })
            .sum()
    }

    pub fn frame_count(&self) -> usize {
        self.characters
            .values()
            .flat_map(BTreeMap::values)
            .map(|motion| {
                motion
                    .layers
                    .iter()
                    .map(EmoteLayer::frame_count)
                    .sum::<usize>()
            })
            .sum()
    }
}

impl EmoteMotion {
    fn parse(character: &str, label: &str, value: &PsbValue, easing: &[EmoteEasingCurve]) -> Option<Self> {
        let layers: Vec<_> = value.get("layer").and_then(PsbValue::as_list)
            .unwrap_or_default().iter().filter_map(|value| EmoteLayer::parse(value, easing)).collect();
        let count = layers.iter().map(EmoteLayer::node_count).sum();
        let priorities = parse_priorities(value.get("priority"), count);
        Some(Self {
            character: character.to_owned(),
            label: label.to_owned(),
            last_time: number(value.get("lastTime")?).unwrap_or(0.0),
            loop_time: number(value.get("loopTime")?).unwrap_or(0.0),
            parameters: value
                .get("parameter")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(EmoteMotionParameter::parse)
                .collect(),
            layers,
            priorities,
            parameter_index: value.get("parameterize").and_then(PsbValue::as_i64)
                .and_then(|i| usize::try_from(i).ok()),
            inline_parameter: value.get("parameterize").and_then(EmoteMotionParameter::parse),
            layer_index_map: value
                .get("layerIndexMap")
                .and_then(PsbValue::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(label, value)| {
                            value.as_i64().map(|index| (label.clone(), index))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

fn parse_priorities(value: Option<&PsbValue>, count: usize) -> Vec<EmoteMotionPriority> {
    let mut frames: Vec<_> = value.and_then(PsbValue::as_list).unwrap_or_default().iter()
        .filter_map(|frame| {
            let content = frame.get("content")?.as_list()?;
            let mut ranks = vec![usize::MAX; count];
            for (rank, entry) in content.iter().rev().enumerate() {
                if let Some(index) = entry.as_i64().and_then(|i| usize::try_from(i).ok()) {
                    if let Some(slot) = ranks.get_mut(index) {
                        if *slot == usize::MAX { *slot = rank; }
                    }
                }
            }
            let mut next = content.len();
            for slot in &mut ranks {
                if *slot == usize::MAX { *slot = next; next += 1; }
            }
            Some(EmoteMotionPriority { time: frame.get("time").and_then(number).unwrap_or(0.0), ranks })
        }).collect();
    frames.sort_by(|a, b| a.time.total_cmp(&b.time));
    frames
}

impl EmoteMotion {
    pub(crate) fn priority_at(&self, time: f32) -> Option<&[usize]> {
        self.priorities.iter().rfind(|frame| frame.time <= time)
            .or_else(|| self.priorities.first()).map(|frame| frame.ranks.as_slice())
    }
}

impl EmoteLayer {
    fn parse(value: &PsbValue, easing: &[EmoteEasingCurve]) -> Option<Self> {
        let mut frames = value
            .get("frameList")
            .and_then(PsbValue::as_list)
            .unwrap_or_default()
            .iter()
            .filter_map(|value| EmoteLayerFrame::parse(value, easing))
            .collect::<Vec<_>>();
        frames.sort_by(|left, right| left.time.total_cmp(&right.time));
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            layer_type: value.get("type").and_then(PsbValue::as_i64).unwrap_or(0),
            coordinate: value
                .get("coordinate")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0),
            mesh_transform: value
                .get("meshTransform")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0),
            mesh_sync_child_mask: value
                .get("meshSyncChildMask")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0),
            inherit_mask: value
                .get("inheritMask")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0x0200_07fc),
            inherit_shape: value
                .get("inheritMask")
                .and_then(PsbValue::as_i64)
                .map(|mask| mask & 0x0200_0000 != 0)
                .unwrap_or(true),
            motion_independent_layer_inherit: value
                .get("motionIndependentLayerInherit")
                .and_then(bool_like)
                .unwrap_or(false),
            transform_order: value
                .get("transformOrder")
                .and_then(PsbValue::as_list)
                .map(|values| values.iter().filter_map(PsbValue::as_i64).collect())
                .unwrap_or_default(),
            mesh_combine: value
                .get("meshCombine")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0)
                != 0,
            stencil_type: value
                .get("stencilType")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0),
            stencil_mask_layers: value
                .get("stencilCompositeMaskLayerList")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(PsbValue::as_str)
                .map(str::to_owned)
                .collect(),
            parameter_index: value
                .get("parameterize")
                .and_then(PsbValue::as_i64)
                .and_then(|index| usize::try_from(index).ok()),
            inline_parameter: value.get("parameterize").and_then(EmoteMotionParameter::parse),
            frames,
            children: value
                .get("children")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|value| EmoteLayer::parse(value, easing))
                .collect(),
        })
    }

    fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    fn frame_count(&self) -> usize {
        self.frames.len() + self.children.iter().map(Self::frame_count).sum::<usize>()
    }
}

impl EmoteMotionParameter {
    fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            id: value.get("id")?.as_str()?.to_owned(),
            range_begin: variable_number(value.get("rangeBegin")?)?,
            range_end: variable_number(value.get("rangeEnd")?)?,
            division: number(value.get("division")?)?,
            enabled: value.get("enabled").and_then(PsbValue::as_i64).unwrap_or(1) != 0,
            discretization: value
                .get("discretization")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0)
                != 0,
        })
    }

    pub fn frame_for_value(&self, value: f32) -> Option<f32> {
        let span = self.range_end - self.range_begin;
        if !self.enabled || self.division <= 0.0 || span.abs() <= f32::EPSILON {
            return None;
        }
        let mut frame =
            ((value - self.range_begin) * self.division / span).clamp(0.0, self.division);
        if self.discretization {
            frame = frame.round();
        }
        Some(frame)
    }
}

impl EmoteLayerFrame {
    fn parse(value: &PsbValue, easing: &[EmoteEasingCurve]) -> Option<Self> {
        Some(Self {
            time: number(value.get("time")?)?,
            frame_type: value.get("type").and_then(PsbValue::as_i64).unwrap_or(3),
            content: value
                .get("content")
                .filter(|value| !matches!(value, PsbValue::Null))
                .map(|value| EmoteFrameContent::parse(value, easing)),
        })
    }
}

impl EmoteFrameContent {
    fn parse(value: &PsbValue, easing: &[EmoteEasingCurve]) -> Self {
        let curves = EmoteFrameCurves {
            coordinate: value.get("ccc").and_then(|value| resolve_curve(value, easing)),
            angle: value.get("acc").and_then(|value| resolve_curve(value, easing)),
            zoom: value.get("zcc").and_then(|value| resolve_curve(value, easing)),
            shear: value.get("scc").and_then(|value| resolve_curve(value, easing)),
            // OCC is the native colour interpolation curve. WCC belongs to
            // stencil wipe layers, which the compact renderer treats as a
            // step property.
            opacity: None,
            color: value.get("occ").and_then(|value| resolve_curve(value, easing)),
            path: value.get("cp").and_then(parse_path),
        };
        Self {
            mask: value.get("mask").and_then(PsbValue::as_i64).unwrap_or(0),
            source: value
                .get("src")
                .and_then(PsbValue::as_str)
                .map(str::to_owned),
            icon: value
                .get("icon")
                .and_then(PsbValue::as_str)
                .map(str::to_owned),
            coord: value.get("coord").and_then(number_list),
            offset: [
                value.get("ox").and_then(variable_number).unwrap_or(0.0),
                value.get("oy").and_then(variable_number).unwrap_or(0.0),
            ],
            flip_x: value.get("fx").and_then(bool_like).unwrap_or(false),
            flip_y: value.get("fy").and_then(bool_like).unwrap_or(false),
            scale_x: value
                .get("zx")
                .or_else(|| value.get("scale_x"))
                .or_else(|| value.get("scaleX"))
                .or_else(|| value.get("scale"))
                .or_else(|| value.get("zoom"))
                .and_then(variable_number)
                .unwrap_or(1.0),
            scale_y: value
                .get("zy")
                .or_else(|| value.get("scale_y"))
                .or_else(|| value.get("scaleY"))
                .or_else(|| value.get("scale"))
                .or_else(|| value.get("zoom"))
                .and_then(variable_number)
                .unwrap_or(1.0),
            shear_x: value.get("sx").and_then(variable_number).unwrap_or(0.0),
            shear_y: value.get("sy").and_then(variable_number).unwrap_or(0.0),
            angle: value
                .get("angle")
                .or_else(|| value.get("rot"))
                .or_else(|| value.get("rotation"))
                .and_then(variable_number),
            curves: (!curves.is_empty()).then(|| Box::new(curves)),
            time_interval: value.get("ti").and_then(number),
            opacity: value.get("opa").and_then(number),
            blend_mode: value.get("bm").and_then(PsbValue::as_i64),
            color: value.get("color").and_then(color_list),
            mesh: value.get("mesh").map(EmoteMesh::parse),
            motion: value.get("motion").map(EmoteMotionRef::parse),
        }
    }
}

impl EmoteFrameCurves {
    fn is_empty(&self) -> bool {
        self.coordinate.is_none()
            && self.angle.is_none()
            && self.zoom.is_none()
            && self.shear.is_none()
            && self.opacity.is_none()
            && self.color.is_none()
            && self.path.is_none()
    }
}

fn resolve_curve(value: &PsbValue, easing: &[EmoteEasingCurve]) -> Option<EmoteEasingCurve> {
    if let Some(index) = value.as_i64() {
        return usize::try_from(index).ok().and_then(|index| easing.get(index)).cloned();
    }
    parse_curve(value)
}

fn parse_curve(value: &PsbValue) -> Option<EmoteEasingCurve> {
    let object = value.as_object()?;
    let x = object.get("x").and_then(number_list)?;
    let y = object.get("y").and_then(number_list)?;
    if x.len() < 2 || x.len() != y.len() {
        return None;
    }
    let p = object.get("p").and_then(number_list).unwrap_or_default();
    Some(EmoteEasingCurve { x, y, p })
}

fn parse_path(value: &PsbValue) -> Option<EmoteBezierPath> {
    let object = value.as_object()?;
    let x = object.get("x").and_then(number_list)?;
    let y = object.get("y").and_then(number_list)?;
    let t = object.get("t").and_then(number_list)?;
    let splines = object
        .get("s")
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(parse_curve)
        .collect::<Vec<_>>();
    (t.len() >= 2 && !splines.is_empty()).then_some(EmoteBezierPath { x, y, t, splines })
}

impl EmoteMesh {
    fn parse(value: &PsbValue) -> Self {
        Self {
            blend_points: value.get("bp").and_then(number_list),
            control_coordinates: value.get("cc").and_then(number_list),
        }
    }
}

impl EmoteMotionRef {
    fn parse(value: &PsbValue) -> Self {
        Self {
            mask: value.get("mask").and_then(PsbValue::as_i64).unwrap_or(0),
            time_offset: value.get("timeOffset").and_then(number).unwrap_or(0.0),
        }
    }
}

fn number_list(value: &PsbValue) -> Option<Vec<f32>> {
    value
        .as_list()?
        .iter()
        .map(number)
        .collect::<Option<Vec<_>>>()
}

fn color_list(value: &PsbValue) -> Option<Vec<f32>> {
    if let Some(values) = value.as_list() {
        return values.iter().map(number).collect::<Option<Vec<_>>>();
    }
    // Native E-Mote stores a single color as 0xRRGGBBAA. Keep the public
    // representation in byte channels so the runtime's existing color filter
    // handles both scalar and four-corner forms consistently.
    let packed = match value {
        PsbValue::Integer(value) => *value as u32,
        _ => return None,
    };
    Some(vec![
        ((packed >> 24) & 0xff) as f32,
        ((packed >> 16) & 0xff) as f32,
        ((packed >> 8) & 0xff) as f32,
        (packed & 0xff) as f32,
    ])
}

fn number(value: &PsbValue) -> Option<f32> {
    match value {
        PsbValue::Integer(value) => Some(*value as f32),
        PsbValue::Float(value) => Some(*value),
        PsbValue::Double(value) => Some(*value as f32),
        _ => None,
    }
}

fn variable_number(value: &PsbValue) -> Option<f32> {
    match value {
        PsbValue::Integer(value) => Some(*value as f32),
        PsbValue::Float(value) => Some(*value),
        PsbValue::Double(value) => Some(*value as f32),
        _ => None,
    }
}

fn bool_like(value: &PsbValue) -> Option<bool> {
    match value {
        PsbValue::Bool(value) => Some(*value),
        _ => value.as_i64().map(|value| value != 0),
    }
}

#[cfg(test)]
mod tests {
    use super::EmoteMotionParameter;

    #[test]
    fn frame_resolves_indexed_easing_without_shifting_invalid_slots() {
        use crate::PsbValue as V;
        use std::collections::BTreeMap;
        let curve = super::EmoteEasingCurve { x: vec![0.0, 1.0], y: vec![0.0, 0.5], p: vec![] };
        let content = V::Object(BTreeMap::from([("ccc".into(), V::Integer(1))]));
        let parsed = super::EmoteFrameContent::parse(&content, &[Default::default(), curve.clone()]);
        assert_eq!(parsed.curves.unwrap().coordinate, Some(curve));
        let invalid = V::Object(BTreeMap::from([("ccc".into(), V::Integer(-1))]));
        assert!(super::EmoteFrameContent::parse(&invalid, &[]).curves.is_none());
    }

    #[test]
    fn priority_frames_use_structural_indices_and_preserve_first_emission() {
        use crate::PsbValue as V;
        use std::collections::BTreeMap;
        let frame = |time, order: &[i64]| V::Object(BTreeMap::from([
            ("time".into(), V::Float(time)),
            ("content".into(), V::List(order.iter().copied().map(V::Integer).collect())),
        ]));
        let value = V::List(vec![frame(0.0, &[0, 2, 1]), frame(10.0, &[2, 0, 1, 1, 99])]);
        let frames = super::parse_priorities(Some(&value), 4);
        assert_eq!(frames[0].ranks, [2, 0, 1, 3]);
        assert_eq!(frames[1].ranks, [3, 1, 4, 5]);
    }

    #[test]
    fn maps_wrapped_parameter_ranges_to_motion_frames() {
        // 符号扩展现在发生在 PSB 解码层（0xE2 单字节 → -30）。
        let parameter = EmoteMotionParameter {
            id: "body_UD".into(),
            range_begin: -30.0,
            range_end: 30.0,
            division: 60.0,
            enabled: true,
            discretization: false,
        };
        assert_eq!(parameter.frame_for_value(0.0), Some(30.0));
    }
}
