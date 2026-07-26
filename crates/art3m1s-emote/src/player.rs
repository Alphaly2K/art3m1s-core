use std::collections::{BTreeMap, VecDeque};

use crate::EmoteModel;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmoteTransform {
    pub scale: [f32; 3],
    pub coord: [f32; 4],
}

impl Default for EmoteTransform {
    fn default() -> Self {
        Self {
            scale: [1.0, 0.0, 0.0],
            coord: [0.0; 4],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableState {
    pub value: f32,
    pub target: f32,
    pub remaining_frames: f32,
    pub easing: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineState {
    pub label: String,
    pub flags: u32,
    pub position: f32,
    pub weight: f32,
    pub target_weight: f32,
    pub remaining_frames: f32,
    pub easing: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EmoteCommand {
    SetScale([f32; 3]),
    SetCoord([f32; 4]),
    SetVariable {
        label: String,
        value: f32,
        frames: f32,
        easing: u32,
    },
    PlayTimeline {
        label: String,
        flags: u32,
    },
    FadeInTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    FadeOutTimeline {
        label: String,
        frames: f32,
        easing: u32,
    },
    StopTimeline {
        label: String,
    },
    Pass,
    Step,
    Skip,
}

#[derive(Debug, Default)]
pub struct EmotePlayer {
    transform: EmoteTransform,
    variables: BTreeMap<String, VariableState>,
    timelines: BTreeMap<String, TimelineState>,
    commands: VecDeque<EmoteCommand>,
}

impl EmotePlayer {
    pub fn transform(&self) -> EmoteTransform {
        self.transform
    }

    pub fn variables(&self) -> &BTreeMap<String, VariableState> {
        &self.variables
    }

    pub fn timelines(&self) -> &BTreeMap<String, TimelineState> {
        &self.timelines
    }

    pub fn set_scale(&mut self, scale: f32, origin_x: f32, origin_y: f32) {
        self.transform.scale = [scale, origin_x, origin_y];
        self.commands
            .push_back(EmoteCommand::SetScale(self.transform.scale));
    }

    pub fn set_coord(&mut self, x: f32, y: f32, z: f32, angle: f32) {
        self.transform.coord = [x, y, z, angle];
        self.commands
            .push_back(EmoteCommand::SetCoord(self.transform.coord));
    }

    pub fn set_variable(&mut self, label: impl Into<String>, value: f32, frames: f32, easing: u32) {
        let label = label.into();
        let current = self
            .variables
            .get(&label)
            .map(|state| state.value)
            .unwrap_or(0.0);
        let frames = frames.max(0.0);
        self.variables.insert(
            label.clone(),
            VariableState {
                value: if frames == 0.0 { value } else { current },
                target: value,
                remaining_frames: frames,
                easing,
            },
        );
        self.commands.push_back(EmoteCommand::SetVariable {
            label,
            value,
            frames,
            easing,
        });
    }

    pub fn play_timeline(&mut self, label: impl Into<String>, flags: u32) {
        let label = label.into();
        self.timelines.insert(
            label.clone(),
            TimelineState {
                label: label.clone(),
                flags,
                position: 0.0,
                weight: 1.0,
                target_weight: 1.0,
                remaining_frames: 0.0,
                easing: 0,
            },
        );
        self.commands
            .push_back(EmoteCommand::PlayTimeline { label, flags });
    }

    pub fn play_model_timeline(
        &mut self,
        model: &EmoteModel,
        label: impl Into<String>,
        flags: u32,
    ) {
        let label = label.into();
        if model
            .timelines()
            .get(&label)
            .is_some_and(|timeline| !timeline.diff)
        {
            self.timelines.retain(|active, _| {
                model
                    .timelines()
                    .get(active)
                    .is_some_and(|timeline| timeline.diff)
            });
        }
        self.play_timeline(label, flags);
    }

    pub fn fade_in_timeline(&mut self, label: impl Into<String>, frames: f32, easing: u32) {
        self.fade_timeline(label.into(), 1.0, frames, easing, true);
    }

    pub fn fade_out_timeline(&mut self, label: impl Into<String>, frames: f32, easing: u32) {
        self.fade_timeline(label.into(), 0.0, frames, easing, false);
    }

    pub fn stop_timeline(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.timelines.remove(&label);
        self.commands
            .push_back(EmoteCommand::StopTimeline { label });
    }

    pub fn pass(&mut self) {
        self.commands.push_back(EmoteCommand::Pass);
    }

    pub fn step(&mut self) {
        self.commands.push_back(EmoteCommand::Step);
    }

    pub fn skip(&mut self) {
        self.commands.push_back(EmoteCommand::Skip);
    }

    pub fn advance(&mut self, frames: f32) {
        let frames = frames.max(0.0);
        for state in self.variables.values_mut() {
            advance_scalar(
                &mut state.value,
                state.target,
                &mut state.remaining_frames,
                frames,
            );
        }
        for state in self.timelines.values_mut() {
            state.position += frames;
            advance_scalar(
                &mut state.weight,
                state.target_weight,
                &mut state.remaining_frames,
                frames,
            );
        }
        self.timelines
            .retain(|_, timeline| timeline.weight != 0.0 || timeline.target_weight != 0.0);
    }

    pub fn take_commands(&mut self) -> impl Iterator<Item = EmoteCommand> + '_ {
        self.commands.drain(..)
    }

    pub fn active_timeline_samples(
        &self,
        model: &EmoteModel,
    ) -> Vec<(&TimelineState, BTreeMap<String, f32>)> {
        self.timelines
            .values()
            .filter_map(|state| {
                model
                    .timelines()
                    .get(&state.label)
                    .map(|timeline| (state, timeline.sample(state.position)))
            })
            .collect()
    }

    fn fade_timeline(
        &mut self,
        label: String,
        target: f32,
        frames: f32,
        easing: u32,
        fade_in: bool,
    ) {
        let frames = frames.max(0.0);
        let state = self
            .timelines
            .entry(label.clone())
            .or_insert_with(|| TimelineState {
                label: label.clone(),
                flags: 0,
                position: 0.0,
                weight: if fade_in { 0.0 } else { 1.0 },
                target_weight: target,
                remaining_frames: frames,
                easing,
            });
        state.target_weight = target;
        state.remaining_frames = frames;
        state.easing = easing;
        if frames == 0.0 {
            state.weight = target;
        }

        self.commands.push_back(if fade_in {
            EmoteCommand::FadeInTimeline {
                label,
                frames,
                easing,
            }
        } else {
            EmoteCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            }
        });
    }
}

fn advance_scalar(value: &mut f32, target: f32, remaining: &mut f32, frames: f32) {
    if *remaining <= 0.0 {
        *value = target;
        return;
    }
    if frames >= *remaining {
        *value = target;
        *remaining = 0.0;
        return;
    }
    let ratio = frames / *remaining;
    *value += (target - *value) * ratio;
    *remaining -= frames;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_nekomiko_script_command_sequence() {
        let mut player = EmotePlayer::default();
        player.set_scale(0.6, 0.0, 0.0);
        player.set_coord(0.0, 720.0, 0.0, 0.0);
        player.pass();
        player.play_timeline("笑顔_ボイス再生用", 1);
        player.fade_in_timeline("通常待機", 0.0, 0);
        player.set_variable("face_talk", 0.75, 0.0, 0);

        assert_eq!(player.transform().scale, [0.6, 0.0, 0.0]);
        assert_eq!(player.transform().coord, [0.0, 720.0, 0.0, 0.0]);
        assert_eq!(player.variables()["face_talk"].value, 0.75);
        assert_eq!(player.timelines()["通常待機"].weight, 1.0);
        assert_eq!(player.take_commands().count(), 6);
    }

    #[test]
    fn advances_variable_tween_without_using_wall_clock_time() {
        let mut player = EmotePlayer::default();
        player.set_variable("face_talk", 1.0, 10.0, 0);
        player.advance(4.0);
        assert!((player.variables()["face_talk"].value - 0.4).abs() < 0.0001);
        player.advance(6.0);
        assert_eq!(player.variables()["face_talk"].value, 1.0);
    }
}
