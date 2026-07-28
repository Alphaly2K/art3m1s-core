use std::collections::BTreeMap;

use crate::{PsbValue, Result};

#[derive(Clone, Debug, Default)]
pub struct EmoteMotionLibrary {
    characters: BTreeMap<String, BTreeMap<String, EmoteMotion>>,
}

#[derive(Clone, Debug)]
pub struct EmoteMotion {
    pub character: String,
    pub label: String,
    pub last_time: f32,
    pub loop_time: f32,
    pub parameters: Vec<EmoteMotionParameter>,
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
    pub inherit_shape: bool,
    pub mesh_combine: bool,
    pub stencil_type: i64,
    pub stencil_mask_layers: Vec<String>,
    pub parameter_index: Option<usize>,
    pub frames: Vec<EmoteLayerFrame>,
    pub children: Vec<EmoteLayer>,
}

#[derive(Clone, Debug)]
pub struct EmoteLayerFrame {
    pub time: f32,
    pub frame_type: i64,
    pub content: Option<EmoteFrameContent>,
}

#[derive(Clone, Debug, Default)]
pub struct EmoteFrameContent {
    pub mask: i64,
    pub source: Option<String>,
    pub icon: Option<String>,
    pub coord: Option<Vec<f32>>,
    pub angle: Option<f32>,
    pub opacity: Option<f32>,
    pub blend_mode: Option<i64>,
    pub color: Option<Vec<f32>>,
    pub mesh: Option<EmoteMesh>,
    pub motion: Option<EmoteMotionRef>,
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
                            EmoteMotion::parse(character, label, value)
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
    fn parse(character: &str, label: &str, value: &PsbValue) -> Option<Self> {
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
            layers: value
                .get("layer")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(EmoteLayer::parse)
                .collect(),
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

impl EmoteLayer {
    fn parse(value: &PsbValue) -> Option<Self> {
        let mut frames = value
            .get("frameList")
            .and_then(PsbValue::as_list)
            .unwrap_or_default()
            .iter()
            .filter_map(EmoteLayerFrame::parse)
            .collect::<Vec<_>>();
        frames.sort_by(|left, right| left.time.total_cmp(&right.time));
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            layer_type: value.get("type").and_then(PsbValue::as_i64).unwrap_or(0),
            coordinate: value
                .get("coordinate")
                .and_then(PsbValue::as_i64)
                .unwrap_or(0),
            inherit_shape: value
                .get("inheritMask")
                .and_then(PsbValue::as_i64)
                .map(|mask| mask & 0x0200_0000 != 0)
                .unwrap_or(true),
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
            frames,
            children: value
                .get("children")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(EmoteLayer::parse)
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
    fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            time: number(value.get("time")?)?,
            frame_type: value.get("type").and_then(PsbValue::as_i64).unwrap_or(0),
            content: value
                .get("content")
                .filter(|value| !matches!(value, PsbValue::Null))
                .map(EmoteFrameContent::parse),
        })
    }
}

impl EmoteFrameContent {
    fn parse(value: &PsbValue) -> Self {
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
            angle: value.get("angle").and_then(variable_number),
            opacity: value.get("opa").and_then(number),
            blend_mode: value.get("bm").and_then(PsbValue::as_i64),
            color: value.get("color").and_then(number_list),
            mesh: value.get("mesh").map(EmoteMesh::parse),
            motion: value.get("motion").map(EmoteMotionRef::parse),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::EmoteMotionParameter;

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
