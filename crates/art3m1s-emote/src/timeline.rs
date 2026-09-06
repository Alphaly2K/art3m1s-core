use std::collections::BTreeMap;

use crate::PsbValue;

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteKeyframe {
    pub frame: f32,
    /// Native TimelineVariableFrame::type == 0. Hold markers advance the
    /// cursor without issuing a variable write.
    pub hold: bool,
    pub value: f32,
    pub easing: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteTimelineTrack {
    pub label: String,
    pub frames: Vec<EmoteKeyframe>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteTimeline {
    pub label: String,
    pub diff: bool,
    pub last_time: f32,
    pub loop_begin: f32,
    pub loop_end: f32,
    pub tracks: Vec<EmoteTimelineTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteVariable {
    pub label: String,
    pub named_frames: Vec<(String, f32)>,
    pub instant: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteSelectorOption {
    pub label: String,
    pub on_value: f32,
    pub off_value: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteSelectorControl {
    pub label: String,
    pub enabled: bool,
    pub options: Vec<EmoteSelectorOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmoteEyeControl {
    pub label: String,
    pub enabled: bool,
    pub blink_enabled: bool,
    pub blink_frame_count: f32,
    pub blink_interval_min: f32,
    pub blink_interval_max: f32,
    pub begin_frame: f32,
    pub end_frame: f32,
    pub edges: Vec<[f32; 2]>,
    pub nodes: Vec<Vec<f32>>,
}

impl EmoteTimeline {
    pub(crate) fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            diff: value.get("diff").and_then(PsbValue::as_i64).unwrap_or(0) != 0,
            last_time: control_frame(value.get("lastTime")?)?,
            loop_begin: control_frame(value.get("loopBegin")?)?,
            loop_end: control_frame(value.get("loopEnd")?)?,
            tracks: value
                .get("variableList")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(EmoteTimelineTrack::parse)
                .collect(),
        })
    }

    pub fn sample(&self, frame: f32) -> BTreeMap<String, f32> {
        let frame = looped_frame(frame, self.loop_begin, self.loop_end, self.last_time);
        self.tracks
            .iter()
            .filter_map(|track| {
                track
                    .sample_looped(frame, self.loop_begin, self.loop_end)
                    .map(|value| (track.label.clone(), value))
            })
            .collect()
    }
}

fn looped_frame(frame: f32, loop_begin: f32, loop_end: f32, last_time: f32) -> f32 {
    if loop_end > loop_begin && frame >= loop_end {
        loop_begin + (frame - loop_begin).rem_euclid(loop_end - loop_begin)
    } else if last_time >= 0.0 {
        frame.min(last_time)
    } else {
        frame
    }
}

impl EmoteTimelineTrack {
    fn parse(value: &PsbValue) -> Option<Self> {
        let mut frames = value
            .get("frameList")
            .and_then(PsbValue::as_list)
            .unwrap_or_default()
            .iter()
            .filter_map(EmoteKeyframe::parse)
            .collect::<Vec<_>>();
        frames.sort_by(|left, right| left.frame.total_cmp(&right.frame));
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            frames,
        })
    }

    pub fn sample(&self, frame: f32) -> Option<f32> {
        let index = self
            .frames
            .iter()
            .rposition(|keyframe| keyframe.frame <= frame)?;
        let current = &self.frames[index];
        if current.hold {
            return self.frames[..index]
                .iter()
                .rev()
                .find(|keyframe| !keyframe.hold)
                .map(|keyframe| keyframe.value);
        }
        let Some(next) = self.frames.get(index + 1) else {
            return Some(current.value);
        };
        if next.hold {
            return Some(current.value);
        }
        let span = next.frame - current.frame;
        if span <= f32::EPSILON {
            return Some(next.value);
        }
        let ratio = ((frame - current.frame) / span).clamp(0.0, 1.0);
        Some(current.value + (next.value - current.value) * ratio)
    }

    fn sample_looped(&self, frame: f32, loop_begin: f32, loop_end: f32) -> Option<f32> {
        if loop_end <= loop_begin {
            return self.sample(frame);
        }
        let first = self.frames.first()?;
        let last = self.frames.last()?;
        if frame > last.frame && last.frame < loop_end {
            let wrapped_first = loop_end + (first.frame - loop_begin).max(0.0);
            let span = wrapped_first - last.frame;
            if span > f32::EPSILON {
                let ratio = ((frame - last.frame) / span).clamp(0.0, 1.0);
                return Some(last.value + (first.value - last.value) * ratio);
            }
        }
        if frame < first.frame && first.frame > loop_begin {
            let span = (loop_end - last.frame) + (first.frame - loop_begin);
            if span > f32::EPSILON {
                let ratio = ((frame - loop_begin) + (loop_end - last.frame)) / span;
                return Some(last.value + (first.value - last.value) * ratio.clamp(0.0, 1.0));
            }
        }
        self.sample(frame)
    }
}

impl EmoteKeyframe {
    fn parse(value: &PsbValue) -> Option<Self> {
        let hold = value.get("type").and_then(PsbValue::as_i64).unwrap_or(0) == 0;
        let content = value.get("content");
        if !hold && content.is_none() {
            return None;
        }
        Some(Self {
            frame: number(value.get("time")?)?,
            hold,
            value: content
                .and_then(|content| content.get("value"))
                .and_then(variable_number)
                .unwrap_or(0.0),
            easing: content
                .and_then(|content| content.get("easing"))
                .and_then(PsbValue::as_i64),
        })
    }
}

impl EmoteVariable {
    pub(crate) fn parse(value: &PsbValue, instant: bool) -> Option<Self> {
        match value {
            PsbValue::String(label) => Some(Self {
                label: label.clone(),
                named_frames: Vec::new(),
                instant,
            }),
            PsbValue::Object(_) => Some(Self {
                label: value.get("label")?.as_str()?.to_owned(),
                named_frames: value
                    .get("frameList")
                    .and_then(PsbValue::as_list)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|frame| {
                        Some((
                            frame.get("label")?.as_str()?.to_owned(),
                            number(frame.get("frame")?)?,
                        ))
                    })
                    .collect(),
                instant,
            }),
            _ => None,
        }
    }
}

impl EmoteSelectorControl {
    pub(crate) fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            enabled: value.get("enabled").and_then(PsbValue::as_i64).unwrap_or(1) != 0,
            options: value
                .get("optionList")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(EmoteSelectorOption::parse)
                .collect(),
        })
    }

    pub fn apply(&self, selector_value: f32, variables: &mut BTreeMap<String, f32>) {
        if !self.enabled || self.options.is_empty() {
            return;
        }
        let selector_value = selector_value.clamp(0.0, (self.options.len() - 1) as f32);
        for (index, option) in self.options.iter().enumerate() {
            let distance = (selector_value - index as f32).abs().min(1.0);
            variables.insert(
                option.label.clone(),
                option.on_value + (option.off_value - option.on_value) * distance,
            );
        }
    }
}

impl EmoteSelectorOption {
    fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            on_value: variable_number(value.get("onValue")?)?,
            off_value: variable_number(value.get("offValue")?)?,
        })
    }
}

impl EmoteEyeControl {
    pub(crate) fn parse(value: &PsbValue) -> Option<Self> {
        Some(Self {
            label: value.get("label")?.as_str()?.to_owned(),
            enabled: value.get("enabled").and_then(PsbValue::as_i64).unwrap_or(1) != 0,
            blink_enabled: value
                .get("blinkEnabled")
                .and_then(PsbValue::as_i64)
                .unwrap_or(1)
                != 0,
            blink_frame_count: number(value.get("blinkFrameCount")?)?.max(1.0),
            blink_interval_min: number(value.get("blinkIntervalMin")?)?.max(0.0),
            blink_interval_max: number(value.get("blinkIntervalMax")?)?.max(0.0),
            begin_frame: variable_number(value.get("beginFrame")?)?,
            end_frame: variable_number(value.get("endFrame")?)?,
            edges: value
                .get("edge")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|edge| {
                    let edge = edge.as_list()?;
                    Some([
                        variable_number(edge.first()?)?,
                        variable_number(edge.get(1)?)?,
                    ])
                })
                .collect(),
            nodes: value
                .get("node")
                .and_then(PsbValue::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|node| {
                    Some(node.as_list()?.iter().filter_map(variable_number).collect())
                })
                .collect(),
        })
    }

    pub fn blink_value(&self, value: f32, phase: f32) -> Option<f32> {
        if !self.enabled || !self.blink_enabled {
            return None;
        }
        if value < self.begin_frame
            || value > self.end_frame
            || (self.end_frame - self.begin_frame).abs() <= f32::EPSILON
        {
            return None;
        }
        let phase = phase.clamp(0.0, 1.0);
        Some(value + (self.end_frame - value) * phase)
    }
}

fn number(value: &PsbValue) -> Option<f32> {
    match value {
        PsbValue::Integer(value) => Some(*value as f32),
        PsbValue::Float(value) => Some(*value),
        PsbValue::Double(value) => Some(*value as f32),
        _ => None,
    }
}

fn control_frame(value: &PsbValue) -> Option<f32> {
    // 0xFF 单字节在 PSB 解码层已按符号扩展为 -1（"无限制"哨兵），无需特判。
    number(value)
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
    use super::*;

    #[test]
    fn samples_track_between_keyframes() {
        let track = EmoteTimelineTrack {
            label: "face_talk".into(),
            frames: vec![
                EmoteKeyframe {
                    frame: 0.0,
                    hold: false,
                    value: 0.0,
                    easing: Some(0),
                },
                EmoteKeyframe {
                    frame: 10.0,
                    hold: false,
                    value: 100.0,
                    easing: Some(0),
                },
            ],
        };
        assert_eq!(track.sample(5.0), Some(50.0));
    }

    #[test]
    fn selector_crossfades_adjacent_options() {
        let selector = EmoteSelectorControl {
            label: "arm_type".into(),
            enabled: true,
            options: ["fade_a", "fade_b", "fade_c"]
                .into_iter()
                .map(|label| EmoteSelectorOption {
                    label: label.into(),
                    on_value: 0.0,
                    off_value: 1.0,
                })
                .collect(),
        };
        let mut variables = BTreeMap::new();
        selector.apply(0.625, &mut variables);
        assert_eq!(variables["fade_a"], 0.625);
        assert_eq!(variables["fade_b"], 0.375);
        assert_eq!(variables["fade_c"], 1.0);
    }

    #[test]
    fn eye_control_interpolates_to_the_target_for_the_current_expression() {
        let control = EmoteEyeControl {
            label: "face_eye_open".into(),
            enabled: true,
            blink_enabled: true,
            blink_frame_count: 16.0,
            blink_interval_min: 30.0,
            blink_interval_max: 180.0,
            begin_frame: 0.0,
            end_frame: 10.0,
            edges: vec![[-10.0, 20.0], [30.0, 30.0]],
            nodes: vec![vec![10.0, 30.0, 32.0, 34.0, 36.0, 38.0, 40.0, 0.0]],
        };
        assert_eq!(control.blink_value(0.0, 0.0), Some(0.0));
        assert_eq!(control.blink_value(0.0, 0.25), Some(2.5));
        assert_eq!(control.blink_value(0.0, 0.5), Some(5.0));
        assert_eq!(control.blink_value(0.0, 1.0), Some(10.0));
        assert_eq!(control.blink_value(30.0, 0.0), None);
        assert_eq!(control.blink_value(23.0, 0.5), None);
    }

    #[test]
    fn loop_interpolates_back_to_first_keyframe_without_a_snap() {
        let timeline = EmoteTimeline {
            label: "idle".into(),
            diff: true,
            last_time: -1.0,
            loop_begin: 0.0,
            loop_end: 300.0,
            tracks: vec![EmoteTimelineTrack {
                label: "body_LR".into(),
                frames: vec![
                    EmoteKeyframe {
                        frame: 0.0,
                        hold: false,
                        value: 0.0,
                        easing: Some(0),
                    },
                    EmoteKeyframe {
                        frame: 200.0,
                        hold: false,
                        value: 100.0,
                        easing: Some(0),
                    },
                ],
            }],
        };
        assert_eq!(timeline.sample(250.0)["body_LR"], 50.0);
        assert_eq!(timeline.sample(299.0)["body_LR"], 1.0);
        assert_eq!(timeline.sample(300.0)["body_LR"], 0.0);
    }
}
