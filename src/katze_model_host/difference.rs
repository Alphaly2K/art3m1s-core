use std::{
    collections::{BTreeMap, BTreeSet},
    f32::consts::{FRAC_PI_2, PI},
};

pub(super) type Assignments = BTreeMap<String, f32>;
type Patterns = BTreeMap<String, Assignments>;

#[derive(Default)]
pub(super) struct Differences {
    pub(super) patterns: BTreeMap<usize, Patterns>,
    chains: BTreeMap<usize, Chain>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Curve {
    #[default]
    Fix,
    Linear,
    SinFastSlow,
    SinSlowFast,
    SinSlowSlow,
    Fade,
    NoMove,
}

impl Curve {
    fn parse(word: &str) -> Self {
        match word {
            "linear" => Self::Linear,
            "sin_fs" => Self::SinFastSlow,
            "sin_sf" => Self::SinSlowFast,
            "sin_ss" => Self::SinSlowSlow,
            "fade" => Self::Fade,
            "nomove" => Self::NoMove,
            _ => Self::Fix,
        }
    }

    fn ease(self, position: f32) -> f32 {
        match self {
            Self::SinFastSlow => (position * FRAC_PI_2).sin(),
            Self::SinSlowFast => 1. - (position * FRAC_PI_2).cos(),
            Self::SinSlowSlow => (1. - (position * PI).cos()) * 0.5,
            _ => position,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Timing {
    curve: Curve,
    duration_ms: u32,
}

#[derive(Clone, Default)]
struct Chain {
    default: Timing,
    rules: Vec<Rule>,
    fade: BTreeSet<String>,
    fix: BTreeSet<String>,
}

#[derive(Clone)]
struct Rule {
    from: String,
    to: String,
    timing: Timing,
    via: Vec<String>,
}

impl Chain {
    fn route<'a>(&'a self, from: &'a str, to: &'a str) -> (Timing, Vec<&'a str>) {
        let exact = self
            .rules
            .iter()
            .find(|rule| rule.from == from && rule.to == to);
        let reverse = || {
            self.rules.iter().find(|rule| {
                rule.from == to && rule.to == from && rule.timing.curve != Curve::NoMove
            })
        };
        let mut keys = vec![from];
        let timing = if let Some(rule) = exact {
            keys.extend(rule.via.iter().map(String::as_str));
            rule.timing
        } else if let Some(rule) = reverse() {
            keys.extend(rule.via.iter().rev().map(String::as_str));
            rule.timing
        } else {
            let curve = if self.fade.contains(from) || self.fade.contains(to) {
                Curve::Fade
            } else if self.fix.contains(from) || self.fix.contains(to) {
                Curve::Fix
            } else {
                self.default.curve
            };
            Timing {
                curve,
                ..self.default
            }
        };
        keys.push(to);
        (timing, keys)
    }
}

fn fields(line: &str) -> impl Iterator<Item = &str> {
    line.split_ascii_whitespace()
        .take_while(|word| !word.starts_with([';', '/']))
        .map(|word| word.trim_matches('"'))
}

fn keys(text: &str) -> impl Iterator<Item = &str> {
    text.split([',', ':']).map(str::trim)
}

fn duration(value: Option<&str>) -> u32 {
    value
        .and_then(|word| word.parse::<u32>().ok())
        .unwrap_or_default()
}

impl Differences {
    pub(super) fn parse(text: &str) -> Self {
        let text = text.split('\0').next().unwrap_or_default();
        let mut result = Self::default();
        let mut section = "";
        let mut current = None;
        let mut named = BTreeMap::<&str, Chain>::new();
        let mut references = BTreeMap::new();
        for line in text.lines() {
            let mut row = fields(line);
            let Some(first) = row.next() else { continue };
            if first.starts_with(['#', '$']) {
                section = first;
                current = None;
                continue;
            }
            if section.eq_ignore_ascii_case("#pattern") {
                let Some(value) = row.next() else { continue };
                if let Some(group) = first.strip_prefix('%') {
                    current = group
                        .parse()
                        .ok()
                        .map(|group| (group, value.to_ascii_lowercase()));
                } else if let Some((group, key)) = &current
                    && let Ok(value) = value.parse::<f32>()
                    && value.is_finite()
                    && value != 99.0
                {
                    result
                        .patterns
                        .entry(*group)
                        .or_default()
                        .entry(key.clone())
                        .or_default()
                        .insert(first.to_owned(), value);
                }
            } else if section.eq_ignore_ascii_case("#chain") {
                let (Ok(group), Some(value)) = (first.parse::<usize>(), row.next()) else {
                    continue;
                };
                if value.starts_with('$') {
                    references.insert(group, value);
                } else if let Some(ms) = row.next() {
                    references.remove(&group);
                    result.chains.entry(group).or_default().default = Timing {
                        curve: Curve::parse(value),
                        duration_ms: duration(Some(ms)),
                    };
                }
            } else if section.starts_with('$') {
                let chain = named.entry(section).or_default();
                match first {
                    "default" => {
                        chain.default.curve = row.next().map(Curve::parse).unwrap_or_default()
                    }
                    "frame" => chain.default.duration_ms = duration(row.next()),
                    "fade" => chain.fade.extend(row.map(str::to_owned)),
                    "fix" => chain.fix.extend(row.map(str::to_owned)),
                    _ => {
                        if let (Some(to), Some(curve)) = (row.next(), row.next()) {
                            chain.rules.push(Rule {
                                from: first.to_owned(),
                                to: to.to_owned(),
                                timing: Timing {
                                    curve: Curve::parse(curve),
                                    duration_ms: duration(row.next()),
                                },
                                via: row.map(str::to_owned).collect(),
                            });
                        }
                    }
                }
            }
        }
        for (group, name) in references {
            if let Some(chain) = named.get(name) {
                result.chains.insert(group, chain.clone());
            }
        }
        result
    }

    fn pattern(&self, group: usize, key: &str) -> Option<&Assignments> {
        self.patterns.get(&group)?.get(&key.to_ascii_lowercase())
    }
}

struct Track {
    values: Vec<f32>,
    timing: Timing,
    started: Option<u32>,
}

impl Track {
    fn sample(&mut self, time: u32, speed: f32) -> Option<f32> {
        let start = *self.started.get_or_insert(time);
        let progress = (time.wrapping_sub(start) as f32 / self.timing.duration_ms as f32 * speed)
            .clamp(0., 1.);
        if progress >= 1. {
            return None;
        }
        let position = progress * (self.values.len() - 1) as f32;
        let index = position as usize;
        let Some(next) = self.values.get(index + 1) else {
            return None;
        };
        let from = self.values[index];
        Some(from + (next - from) * self.timing.curve.ease(position - index as f32))
    }
}

#[derive(Default)]
pub(super) struct Selection {
    keys: String,
    groups: BTreeMap<usize, Assignments>,
    tracks: BTreeMap<usize, BTreeMap<String, Track>>,
}

impl Selection {
    pub(super) fn select(&mut self, table: &Differences, next: &str, animate: bool) -> Option<u32> {
        let previous = keys(&self.keys).collect::<Vec<_>>();
        let mut fade = None;
        let groups = previous.len().max(keys(next).count());
        for (index, to) in keys(next)
            .chain(std::iter::repeat(""))
            .take(groups)
            .enumerate()
        {
            let from = previous.get(index).copied().unwrap_or_default();
            if from == to && animate {
                continue;
            }
            let group = index + 1;
            let timing_and_route = animate
                .then(|| table.chains.get(&group))
                .flatten()
                .map(|chain| chain.route(from, to));
            if timing_and_route
                .as_ref()
                .is_some_and(|(timing, _)| timing.curve == Curve::NoMove)
            {
                continue;
            }
            self.tracks.remove(&group);
            if let Some(pattern) = table.pattern(group, to) {
                self.groups.insert(group, pattern.clone());
            } else {
                self.groups.remove(&group);
            }
            let Some((timing, route)) = timing_and_route else {
                continue;
            };
            match timing.curve {
                Curve::Fade => fade = Some(timing.duration_ms),
                Curve::Fix | Curve::NoMove => (),
                _ if timing.duration_ms != 0 => {
                    let mut values = BTreeMap::<String, Vec<f32>>::new();
                    for pose in route {
                        if let Some(pattern) = table.pattern(group, pose) {
                            for (label, value) in pattern {
                                values.entry(label.clone()).or_default().push(*value);
                            }
                        }
                    }
                    let tracks = values
                        .into_iter()
                        .filter(|(_, values)| values.len() > 1)
                        .map(|(label, values)| {
                            (
                                label,
                                Track {
                                    values,
                                    timing,
                                    started: None,
                                },
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    if !tracks.is_empty() {
                        self.tracks.insert(group, tracks);
                    }
                }
                _ => (),
            }
        }
        self.keys.clear();
        self.keys.push_str(next);
        if fade.is_some() {
            self.finish();
        }
        fade
    }

    pub(super) fn finish(&mut self) {
        self.tracks.clear();
    }

    pub(super) fn active(&self) -> bool {
        !self.tracks.is_empty()
    }

    pub(super) fn sample(&mut self, time: u32, speed: &Scale) -> Assignments {
        let mut output = Assignments::new();
        let mut groups = self.groups.iter().peekable();
        self.tracks.retain(|group, tracks| {
            while groups.peek().is_some_and(|(index, _)| *index <= group) {
                output.extend(groups.next().unwrap().1.clone());
            }
            tracks.retain(|label, track| {
                if let Some(value) = track.sample(time, speed.get(label)) {
                    output.insert(label.clone(), value);
                    true
                } else {
                    false
                }
            });
            !tracks.is_empty()
        });
        for (_, values) in groups {
            output.extend(values.clone());
        }
        output
    }
}

#[derive(Default)]
pub(super) struct Scale {
    all: Option<f32>,
    named: Assignments,
}

impl Scale {
    pub(super) fn set(&mut self, parameter: Option<String>, multiplier: f32) {
        if let Some(parameter) = parameter {
            self.named.insert(parameter, multiplier);
        } else {
            self.all = Some(multiplier);
            self.named.clear();
        }
    }

    pub(super) fn get(&self, parameter: &str) -> f32 {
        self.named
            .get(parameter)
            .copied()
            .or(self.all)
            .unwrap_or(1.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
#pattern
%1 a
pose 0
partial 0
%1 b
pose 30
partial 30
%1 x
pose 10
partial 99
%1 y
pose 50
%1 c
pose 60
%1 d
pose 70
%2 fixed
pose 80
#chain
1 $routes
$routes
default linear
frame 30
fade d
a b sin_fs 60 x y
a c nomove 25
";

    #[test]
    fn samples_each_segment_and_restarts_from_the_previously_selected_pose() {
        let table = Differences::parse(TABLE);
        let mut selection = Selection::default();
        let speed = Scale::default();
        assert_eq!(selection.select(&table, "a", false), None);
        assert_eq!(selection.select(&table, "b", true), None);
        let start = u32::MAX - 5;
        for (elapsed, expected) in [
            (0, 0.),
            (10, 7.071068),
            (20, 10.),
            (30, 38.28427),
            (40, 50.),
        ] {
            let actual = selection.sample(start.wrapping_add(elapsed), &speed)["pose"];
            assert!((actual - expected).abs() < 0.00001, "{elapsed}: {actual}");
            assert!(selection.active());
        }
        assert_eq!(
            selection.sample(start.wrapping_add(30), &speed)["partial"],
            21.213203
        );

        selection.select(&table, "a", true);
        assert_eq!(selection.sample(36, &speed)["pose"], 30.);
        assert_eq!(selection.sample(56, &speed)["pose"], 50.);
        assert_eq!(selection.sample(76, &speed)["pose"], 10.);
        assert_eq!(selection.sample(96, &speed)["pose"], 0.);
        assert!(!selection.active());
    }

    #[test]
    fn respects_directional_nomove_fade_and_later_group_priority() {
        let table = Differences::parse(TABLE);
        let mut selection = Selection::default();
        let mut speed = Scale::default();
        selection.select(&table, "a", false);
        selection.select(&table, "c", true);
        assert_eq!(selection.sample(1, &speed)["pose"], 0.);
        assert!(!selection.active());
        // The reverse of nomove uses the default rule.
        speed.set(Some("pose".into()), 2.);
        selection.select(&table, "a", true);
        assert_eq!(selection.sample(10, &speed)["pose"], 60.);
        assert_eq!(selection.sample(25, &speed)["pose"], 0.);
        assert!(!selection.active());

        assert_eq!(selection.select(&table, "d", true), Some(30));
        assert_eq!(selection.sample(30, &speed)["pose"], 70.);
        assert!(!selection.active());
        assert_eq!(selection.select(&table, "a,fixed", false), None);
        selection.select(&table, "b,fixed", true);
        assert_eq!(selection.sample(40, &speed)["pose"], 80.);
        assert!(selection.active());
        selection.finish();
        assert_eq!(selection.sample(50, &speed)["pose"], 80.);
        assert!(!selection.active());
    }
}
