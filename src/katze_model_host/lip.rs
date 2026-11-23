use super::{Assignments, Differences};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct LipSync {
    profiles: BTreeMap<(usize, String), Profile>,
    active: Vec<Active>,
}

#[derive(Clone)]
struct Profile {
    minimum: u32,
    linear: bool,
    steps: Vec<Step>,
    rest: f32,
}

#[derive(Clone, Copy)]
struct Step {
    value: f32,
    duration_ms: f32,
    check_end: bool,
}

struct Active {
    key: (usize, String),
    labels: Vec<String>,
    profile: Profile,
    last_level: u32,
    position: Option<Position>,
    ending: bool,
}

struct Position {
    step: usize,
    started: Option<u32>,
    closing: bool,
}

impl LipSync {
    pub(super) fn parse(text: &str) -> Self {
        let mut tables = BTreeMap::new();
        let mut flows = BTreeMap::new();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                section = line.to_ascii_lowercase();
                continue;
            }
            if !matches!(section.as_str(), "#lip_table" | "#lip_flow") {
                continue;
            }
            let mut fields = line
                .split_ascii_whitespace()
                .take_while(|field| !field.starts_with("//") && !field.starts_with(';'));
            let Some(group) = fields.next().and_then(|word| word.parse::<usize>().ok()) else {
                continue;
            };
            let Some(key) = fields.next() else { continue };
            let key = (group, key.trim_matches('"').to_ascii_lowercase());
            if section == "#lip_table" {
                let linear = match fields.next() {
                    Some(word) if word.eq_ignore_ascii_case("fix") => false,
                    Some(word) if word.eq_ignore_ascii_case("linear") => true,
                    _ => continue,
                };
                let Some(minimum) = fields.next().and_then(|word| word.parse::<u32>().ok()) else {
                    continue;
                };
                fields.next(); // Maximum level belongs to the authored level-track path.
                let Some(values) = fields
                    .map(|word| word.parse::<f32>().ok().filter(|v| v.is_finite()))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                if !values.is_empty() {
                    tables.insert(key, (minimum, linear, values));
                }
            } else {
                let flow = fields
                    .map(|word| {
                        let mut parts = word.split(',');
                        let index = parts.next()?.parse::<usize>().ok()?;
                        let duration_ms = parts.next()?.parse::<f32>().ok()?;
                        let check_end = parts.next().unwrap_or("1").parse::<i32>().ok()? != 0;
                        (duration_ms.is_finite() && duration_ms >= 0.0).then_some((
                            index,
                            duration_ms,
                            check_end,
                        ))
                    })
                    .collect::<Option<Vec<_>>>();
                if let Some(flow) = flow.filter(|flow| !flow.is_empty()) {
                    flows.insert(key, flow);
                }
            }
        }
        let profiles = tables
            .into_iter()
            .filter_map(|(key, (minimum, linear, values))| {
                let steps = flows
                    .remove(&key)?
                    .into_iter()
                    .map(|(index, duration_ms, check_end)| Step {
                        value: values.get(index).copied().unwrap_or(values[0]),
                        duration_ms,
                        check_end,
                    })
                    .collect();
                Some((
                    key,
                    Profile {
                        minimum,
                        linear,
                        steps,
                        rest: values[0],
                    },
                ))
            })
            .collect();
        Self {
            profiles,
            active: Vec::new(),
        }
    }

    pub(super) fn select(&mut self, differences: &Differences, keys: &str) {
        let mut previous = std::mem::take(&mut self.active);
        for (index, key) in keys.split([',', ':']).enumerate() {
            let key = (index + 1, key.trim().to_ascii_lowercase());
            let (Some(profile), Some(vars)) = (
                self.profiles.get(&key),
                differences.0.get(&key.0).and_then(|p| p.get(&key.1)),
            ) else {
                continue;
            };
            if let Some(index) = previous.iter().position(|entry| entry.key == key) {
                self.active.push(previous.swap_remove(index));
            } else {
                self.active.push(Active {
                    key,
                    labels: vars.keys().cloned().collect(),
                    profile: profile.clone(),
                    last_level: 0,
                    position: None,
                    ending: false,
                });
            }
        }
    }

    pub(super) fn observe(&mut self, levels: Option<&[u32]>) {
        for active in &mut self.active {
            active.observe(levels);
        }
    }

    pub(super) fn sample(&mut self, time: u32) -> Assignments {
        let mut values = Assignments::new();
        for active in &mut self.active {
            if let Some(value) = active.sample(time) {
                values.extend(active.labels.iter().map(|label| (label.clone(), value)));
            }
        }
        values
    }
}

impl Active {
    fn observe(&mut self, levels: Option<&[u32]>) {
        let Some(levels) = levels else {
            self.last_level = 0;
            self.position = None;
            self.ending = false;
            return;
        };
        // The first sample is the current window. The player opens on the following one.
        let Some(&level) = levels.get(1) else { return };
        if std::mem::replace(&mut self.last_level, level) == level {
            return;
        }
        if level >= self.profile.minimum {
            if self.position.is_none() {
                self.position = Some(Position {
                    step: 0,
                    started: None,
                    closing: false,
                });
                self.ending = false;
            }
        } else if self.position.is_some() {
            let voiced = levels[1..]
                .iter()
                .filter(|&&value| value >= self.profile.minimum)
                .count();
            self.ending |= voiced == 0 || voiced < levels.len().saturating_mul(4) / 10;
        }
    }

    fn sample(&mut self, time: u32) -> Option<f32> {
        let position = self.position.as_mut()?;
        let step = self.profile.steps[position.step];
        let elapsed = time.wrapping_sub(*position.started.get_or_insert(time)) as f32;
        if elapsed >= step.duration_ms {
            // The native update advances one segment and restarts its clock at this tick.
            position.started = Some(time);
            if position.closing {
                self.position = None;
                self.ending = false;
                return None;
            }
            if self.ending && step.check_end {
                position.closing = true;
            } else {
                position.step = (position.step + 1) % self.profile.steps.len();
            }
            return Some(self.profile.steps[position.step].value);
        }
        if self.profile.linear {
            let target = if position.closing {
                self.profile.rest
            } else {
                self.profile.steps[(position.step + 1) % self.profile.steps.len()].value
            };
            return Some(step.value + (target - step.value) * (elapsed / step.duration_ms));
        }
        Some(step.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_uses_the_following_signal_window_and_finishes_at_a_marked_segment() {
        let text = "#pattern\n%1 neutral\nmouth 10\n#lip_table\n1 neutral linear 30 65 10 20 30\n#lip_flow\n1 neutral 2,8,1 1,10,0 0,12,0";
        let mut lip = LipSync::parse(text);
        lip.select(&Differences::parse(text), "neutral");
        lip.observe(Some(&[80, 0, 80, 80]));
        assert!(lip.sample(96).is_empty());
        lip.observe(Some(&[0, 30, 80, 80]));
        assert_eq!(lip.sample(100)["mouth"], 30.0);
        lip.observe(Some(&[0, 0, 0, 0]));
        assert_eq!(lip.sample(104)["mouth"], 25.0);
        assert_eq!(lip.sample(108)["mouth"], 30.0);
        assert_eq!(lip.sample(112)["mouth"], 20.0);
        assert!(lip.sample(116).is_empty());
        lip.observe(Some(&[0, 80]));
        assert_eq!(lip.sample(u32::MAX - 1)["mouth"], 30.0);
        assert_eq!(lip.sample(2)["mouth"], 25.0);
        lip.observe(None);
        assert!(lip.sample(3).is_empty());
    }
}
