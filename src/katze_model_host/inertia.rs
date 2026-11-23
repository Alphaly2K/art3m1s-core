use super::difference::{Assignments, fields};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const GLOBAL: u32 = 3;
pub(super) const LEVEL: u32 = 8;
pub(super) const ENABLE: u32 = 9;

pub(super) struct Settings {
    enabled: i32,
    levels: BTreeMap<String, i32>,
    enables: BTreeMap<String, i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: 1,
            levels: BTreeMap::new(),
            enables: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub(super) fn get(&self, id: u32, group: &str) -> Option<i32> {
        Some(match id {
            GLOBAL => self.enabled,
            LEVEL => self.levels.get(group).copied().unwrap_or(100),
            ENABLE => self.enables.get(group).copied().unwrap_or(1),
            _ => return None,
        })
    }

    pub(super) fn set(&mut self, id: u32, group: String, value: i32) -> bool {
        match id {
            GLOBAL => self.enabled = value,
            LEVEL => {
                self.levels.insert(group, value);
            }
            ENABLE => {
                self.enables.insert(group, value);
            }
            _ => return false,
        }
        true
    }
}

#[derive(Clone, Copy)]
struct Material {
    mass: f32,
    stiffness: f32,
    drag: f32,
    step: f32,
    travel: f32,
    inverted: bool,
    amplitude: f32,
}

impl Material {
    fn read<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Self> {
        let mut next = || {
            let value = fields.next()?.parse::<f32>().ok()?;
            value.is_finite().then_some(value)
        };
        let result = Self {
            mass: next()?,
            stiffness: next()?,
            drag: next()?,
            step: next()?,
            travel: next()?,
            inverted: next()? != 0.,
            amplitude: next()?,
        };
        let _ = next()?;
        (result.mass > 0. && result.travel > 0. && result.step >= 0.).then_some(result)
    }

    fn at_level(base: Self, min: Self, max: Self, value: i32) -> Self {
        let level = value.clamp(0, 200) as f32 / 100.;
        let (from, to, amount) = if level < 1. {
            (min, base, level)
        } else {
            (base, max, level - 1.)
        };
        let mix = |a, b| a + (b - a) * amount;
        Self {
            mass: mix(from.mass, to.mass),
            stiffness: mix(from.stiffness, to.stiffness),
            drag: mix(from.drag, to.drag),
            step: mix(from.step, to.step),
            travel: mix(from.travel, to.travel),
            ..base
        }
    }
}

struct Spring {
    base: Material,
    min: Material,
    max: Material,
    level_group: Option<String>,
    enable_group: Option<String>,
    level: i32,
    enabled: bool,
    mode: u32,
    displacement: f32,
    speed: f32,
}

impl Spring {
    fn reset(&mut self) {
        self.displacement = 0.;
        self.speed = 0.;
    }

    fn advance(&mut self, ticks: u32, drive: f32, material: Material) -> f32 {
        self.displacement -= drive;
        for _ in 0..ticks {
            self.displacement = self.displacement.clamp(-material.travel, material.travel);
            let force = -material.stiffness * self.displacement - material.drag * self.speed;
            self.speed += force / material.mass * material.step;
            self.displacement += self.speed * material.step;
        }
        if !self.displacement.is_finite() || !self.speed.is_finite() {
            self.reset();
        }
        let sign = if material.inverted { -1. } else { 1. };
        self.displacement / material.travel * material.amplitude * sign
    }
}

struct Link {
    source: String,
    target: String,
    gain: f32,
    divisor: f32,
    last: Option<f32>,
}

#[derive(Default)]
pub(super) struct Inertia {
    springs: BTreeMap<String, Spring>,
    horizontal: BTreeSet<String>,
    vertical: BTreeSet<String>,
    links: Vec<Link>,
    last: Option<(u32, [f32; 2])>,
    offsets: Assignments,
}

impl Inertia {
    pub(super) fn parse(expression: &str, body: &str) -> Self {
        let mut result = Self::default();
        let mut materials = BTreeMap::new();
        let mut minima = BTreeMap::new();
        let mut maxima = BTreeMap::new();
        let mut levels = BTreeMap::new();
        let mut enables = BTreeMap::new();
        for text in [expression, body] {
            let mut section = String::new();
            let mut group = "";
            for line in text.split('\0').next().unwrap_or_default().lines() {
                let mut row = fields(line);
                let Some(label) = row.next() else { continue };
                if label.starts_with(['#', '$']) {
                    section = label.to_ascii_lowercase();
                    group = "";
                    continue;
                }
                match section.as_str() {
                    "#material" | "#material_min" | "#material_max" => {
                        let table = match section.as_str() {
                            "#material_min" => &mut minima,
                            "#material_max" => &mut maxima,
                            _ => &mut materials,
                        };
                        if let Some(material) = Material::read(row) {
                            table.insert(label.to_owned(), material);
                        }
                    }
                    "#inertia_lr" => {
                        result.horizontal.insert(label.to_owned());
                    }
                    "#inertia_ud" => {
                        result.vertical.insert(label.to_owned());
                    }
                    "#inertia_lv" | "#inertia_enable" => {
                        if let Some(name) = label.strip_prefix('%') {
                            group = name;
                        } else if !group.is_empty() {
                            let groups = if section == "#inertia_lv" {
                                &mut levels
                            } else {
                                &mut enables
                            };
                            groups.insert(label.to_owned(), group.to_ascii_lowercase());
                        }
                    }
                    "#inertia_dif" => {
                        if let (Some(target), Some(gain), Some(divisor)) =
                            (row.next(), row.next(), row.next())
                            && let (Ok(gain), Ok(divisor)) =
                                (gain.parse::<f32>(), divisor.parse::<f32>())
                            && gain.is_finite()
                            && divisor.is_finite()
                            && divisor != 0.
                        {
                            result.links.push(Link {
                                source: label.to_owned(),
                                target: target.to_owned(),
                                gain,
                                divisor,
                                last: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        for (label, base) in materials {
            result.springs.insert(
                label.clone(),
                Spring {
                    base,
                    min: minima.remove(&label).unwrap_or(base),
                    max: maxima.remove(&label).unwrap_or(base),
                    level_group: levels.remove(&label),
                    enable_group: enables.remove(&label),
                    level: 100,
                    enabled: true,
                    mode: 0,
                    displacement: 0.,
                    speed: 0.,
                },
            );
        }
        result
    }

    pub(super) fn is_empty(&self) -> bool {
        self.springs.is_empty()
    }

    pub(super) fn set_group(&mut self, group: Option<&str>, mode: u32) {
        for spring in self.springs.values_mut() {
            if group.is_none_or(|group| spring.enable_group.as_deref() == Some(group))
                && spring.mode != mode
            {
                spring.mode = mode;
                spring.reset();
            }
        }
    }

    pub(super) fn sample(
        &mut self,
        tick: u32,
        position: [f32; 2],
        settings: &Settings,
        pose: &Assignments,
    ) -> bool {
        let position = position.map(|value| if value.is_finite() { value } else { 0. });
        let (ticks, delta) = self.last.map_or((0, [0.; 2]), |(last, previous)| {
            (
                tick.wrapping_sub(last),
                std::array::from_fn(|axis| position[axis] - previous[axis]),
            )
        });
        // A discontinuous clock is a new baseline. Do not replay a task's entire history.
        let reset = self.last.is_none() || settings.enabled == 0 || ticks > 120;
        let mut drive = Assignments::new();
        if reset || ticks != 0 {
            self.last = Some((tick, position));
            for link in &mut self.links {
                let value = pose.get(&link.source).copied().unwrap_or(0.);
                let last = link.last.replace(value).unwrap_or(value);
                if !reset && value != last {
                    *drive.entry(link.target.clone()).or_default() +=
                        (value - last) / link.divisor * link.gain;
                }
            }
        }
        let mut offsets = Assignments::new();
        for (label, spring) in &mut self.springs {
            let level = spring
                .level_group
                .as_ref()
                .and_then(|group| settings.levels.get(group))
                .copied()
                .unwrap_or(100);
            let enabled = settings.enabled != 0
                && spring.mode == 0
                && spring
                    .enable_group
                    .as_ref()
                    .and_then(|group| settings.enables.get(group))
                    .is_none_or(|&value| value != 0);
            if reset || enabled != spring.enabled || level != spring.level {
                spring.reset();
                spring.enabled = enabled;
                spring.level = level;
            }
            if !enabled || reset {
                continue;
            }
            let material = Material::at_level(spring.base, spring.min, spring.max, level);
            let mut input = 0.;
            if ticks != 0 {
                if self.horizontal.contains(label) {
                    input += delta[0];
                }
                if self.vertical.contains(label) {
                    input += delta[1];
                }
                input += drive.get(label).copied().unwrap_or(0.) * material.travel;
            }
            let offset = spring.advance(ticks, input, material);
            if offset.is_finite() && offset != 0. {
                offsets.insert(label.clone(), offset);
            }
        }
        let changed = offsets != self.offsets;
        self.offsets = offsets;
        changed
    }

    pub(super) fn apply(&self, pose: &mut Assignments) {
        for (label, offset) in &self.offsets {
            *pose.entry(label.clone()).or_default() += offset;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPRESSION: &str = "\
#material
hair 4 2 1 1 20 0 20 0
coat 2 1 1 1 20 1 20 0
#inertia_lr
hair
#inertia_ud
coat
#inertia_dif
face hair 2 4
";
    const BODY: &str = "\
#material_min
hair 2 2 1 1 20 0 20 0
#material_max
hair 6 2 1 1 20 0 20 0
#inertia_lv
%head
hair
#inertia_enable
%head
hair
";

    #[test]
    fn logical_clock_pauses_and_uses_unmodified_pose_for_driving() {
        let mut inertia = Inertia::parse(EXPRESSION, BODY);
        let settings = Settings::default();
        let first_pose = Assignments::from([("face".into(), 8.), ("hair".into(), 3.)]);
        let next_pose = Assignments::from([("face".into(), 9.), ("hair".into(), 3.)]);
        assert!(!inertia.sample(u32::MAX, [40., 10.], &settings, &first_pose));
        assert!(!inertia.sample(u32::MAX, [44., 16.], &settings, &next_pose));
        assert!(inertia.sample(0, [44., 16.], &settings, &next_pose));
        let mut applied = next_pose.clone();
        inertia.apply(&mut applied);
        assert_eq!(applied["hair"], -4.);
        assert_eq!(applied["coat"], 3.);
        assert!(!inertia.sample(0, [44., 16.], &settings, &next_pose));
        assert!(inertia.sample(1, [44., 16.], &settings, &next_pose));
        let mut applied = next_pose.clone();
        inertia.apply(&mut applied);
        assert_eq!(applied["hair"], 4.75);
        assert!(!applied.contains_key("coat"));
    }

    #[test]
    fn group_override_restarts_motion_and_preserves_session_settings() {
        let mut inertia = Inertia::parse(EXPRESSION, BODY);
        let mut settings = Settings::default();
        let pose = Assignments::new();
        settings.set(LEVEL, "head".into(), 50);
        inertia.sample(1, [0., 0.], &settings, &pose);
        inertia.sample(2, [12., 0.], &settings, &pose);
        assert!((inertia.offsets["hair"] + 4.).abs() < 0.00001);
        inertia.set_group(Some("head"), 1);
        assert!(inertia.sample(2, [12., 0.], &settings, &pose));
        assert!(inertia.offsets.is_empty());
        settings.set(ENABLE, "head".into(), 0);
        inertia.set_group(None, 0);
        assert!(!inertia.sample(3, [24., 0.], &settings, &pose));
        settings.set(ENABLE, "head".into(), 1);
        assert!(!inertia.sample(3, [24., 0.], &settings, &pose));
        assert!(inertia.sample(4, [36., 0.], &settings, &pose));
        settings.set(GLOBAL, String::new(), 0);
        assert!(inertia.sample(4, [36., 0.], &settings, &pose));
        assert!(inertia.offsets.is_empty());
    }
}
