use super::{
    Assignments, Differences,
    difference::{fields, keys},
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU32, Ordering},
};

#[derive(Default)]
pub(super) struct Blink {
    profiles: BTreeMap<(usize, String), Profile>,
    active: Vec<Active>,
    random: u32,
}

#[derive(Clone, Copy)]
struct Profile {
    from: f32,
    to: f32,
    duration: u32,
    minimum_delay: u32,
    delay_range: u32,
}

struct Active {
    label: String,
    profile: Profile,
    start: Option<u32>,
}

impl Blink {
    pub(super) fn parse(text: &str) -> Self {
        static SEED: AtomicU32 = AtomicU32::new(1);
        let mut blink = Self {
            random: SEED.fetch_add(1, Ordering::Relaxed).max(1),
            ..Self::default()
        };
        let mut in_blink = false;
        for line in text.split('\0').next().unwrap_or_default().lines() {
            let mut row = fields(line);
            let Some(first) = row.next() else { continue };
            if first.starts_with(['#', '$']) {
                in_blink = first.eq_ignore_ascii_case("#blink");
            } else if in_blink && let Some((key, profile)) = parse_profile(first, row) {
                blink.profiles.entry(key).or_insert(profile);
            }
        }
        blink
    }

    pub(super) fn select(&mut self, differences: &Differences, selected: &str) {
        self.active.clear();
        for (index, key) in keys(selected).enumerate() {
            let key = (index + 1, key.to_ascii_lowercase());
            if let (Some(&profile), Some(label)) = (
                self.profiles.get(&key),
                differences.first_parameter(key.0, &key.1),
            ) {
                self.active.push(Active {
                    label: label.to_owned(),
                    profile,
                    start: None,
                });
            }
        }
    }

    pub(super) fn sample(&mut self, tick: u32, enabled: bool) -> Assignments {
        let mut values = Assignments::new();
        for active in &mut self.active {
            if !enabled {
                active.start = None;
                continue;
            }
            let profile = active.profile;
            let start = *active.start.get_or_insert_with(|| {
                self.random ^= self.random << 13;
                self.random ^= self.random >> 17;
                self.random ^= self.random << 5;
                tick.wrapping_add(profile.minimum_delay + self.random % profile.delay_range)
            });
            let elapsed = tick.wrapping_sub(start);
            if elapsed > i32::MAX as u32 {
                continue;
            }
            if elapsed >= profile.duration {
                active.start = None;
                continue;
            }
            let phase = elapsed as f32 / profile.duration as f32 * 2.0;
            let value = if phase <= 1.0 {
                profile.from + (profile.to - profile.from) * phase
            } else {
                profile.to + (profile.from - profile.to) * (phase - 1.0)
            };
            values.insert(active.label.clone(), value);
        }
        values
    }
}

fn parse_profile<'a>(
    group: &str,
    mut row: impl Iterator<Item = &'a str>,
) -> Option<((usize, String), Profile)> {
    let group = group.parse().ok()?;
    let key = row.next()?.to_ascii_lowercase();
    let from: f32 = row.next()?.parse().ok()?;
    let to: f32 = row.next()?.parse().ok()?;
    let duration = row.next()?.parse().ok()?;
    let minimum_delay: u32 = row.next()?.parse().ok()?;
    let maximum_delay: u32 = row.next()?.parse().ok()?;
    (from.is_finite()
        && to.is_finite()
        && (1..=i32::MAX as u32).contains(&duration)
        && (minimum_delay..=i32::MAX as u32).contains(&maximum_delay))
    .then(|| {
        (
            (group, key),
            Profile {
                from,
                to,
                duration,
                minimum_delay,
                delay_range: (maximum_delay - minimum_delay).max(1),
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_follows_the_task_clock_and_restarts_on_reselection() {
        let table = "#pattern\n%2 neutral\neye 4\na_secondary 12\n#blink\n2 neutral 2 10 8 16 16";
        let differences = Differences::parse(table);
        let mut blink = Blink::parse(table);
        blink.select(&differences, "other,NEUTRAL");
        assert!(blink.sample(u32::MAX - 7, true).is_empty());
        assert!(blink.sample(7, true).is_empty());
        for (tick, value) in [(8, 2.), (10, 6.), (12, 10.), (14, 6.)] {
            assert_eq!(
                blink.sample(tick, true),
                Assignments::from([("eye".to_owned(), value)])
            );
        }
        assert!(blink.sample(16, true).is_empty());
        assert!(blink.sample(17, true).is_empty());
        assert_eq!(blink.sample(37, true)["eye"], 10.);
        blink.select(&differences, "other,neutral");
        assert!(blink.sample(37, true).is_empty());
        assert_eq!(blink.sample(57, true)["eye"], 10.);
        assert!(blink.sample(57, false).is_empty());
        assert!(blink.sample(58, true).is_empty());
    }
}
