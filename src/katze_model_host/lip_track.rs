pub(super) enum Track {
    Level(Vec<i32>),
    Pattern(Vec<PatternSample>),
}

pub(super) struct PatternSample {
    tick: u32,
    openness: f32,
}

impl Track {
    pub(super) fn parse(kind: u32, bytes: &[u8]) -> Option<Self> {
        match kind {
            1 => Self::parse_pattern(bytes),
            2 if bytes.len() >= 4 => Some(Self::Level(
                bytes
                    .chunks_exact(4)
                    .map(|word| i32::from_le_bytes(word.try_into().unwrap()))
                    .collect(),
            )),
            _ => None,
        }
    }

    fn parse_pattern(bytes: &[u8]) -> Option<Self> {
        if !bytes.starts_with(b"LPD\0") || word(bytes, 8)? != 32 {
            return None;
        }
        let count = word(bytes, 12)? as usize;
        let quietest = -f32::from_bits(word(bytes, 16)?);
        if count == 0 || !quietest.is_finite() || quietest <= 20. {
            return None;
        }
        let rows = bytes.get(20..20usize.checked_add(count.checked_mul(32)?)?)?;
        let mut samples = Vec::with_capacity(count);
        let weights = [0., 1., 0.4, 0.2, 0.8, 0.6];
        for row in rows.chunks_exact(32) {
            let tick = word(row, 0)?;
            let weight = weights.get(word(row, 4)? as usize)?;
            let level = -f32::from_bits(word(row, 28)?);
            if !level.is_finite()
                || samples
                    .last()
                    .is_some_and(|last: &PatternSample| tick <= last.tick)
            {
                return None;
            }
            let level = (quietest - level.clamp(20., quietest)) / (quietest - 20.);
            samples.push(PatternSample {
                tick,
                openness: (0.6 + 0.4 * level) * weight,
            });
        }
        Some(Self::Pattern(samples))
    }

    pub(super) fn sample(&self, tick: u32) -> Option<f32> {
        match self {
            Self::Level(levels) => {
                let level = *levels.get(tick as usize)?;
                Some((level.clamp(30, 65) as f32 - 30.) / 35.)
            }
            Self::Pattern(samples) => {
                let last_tick = samples.last()?.tick;
                if tick > last_tick {
                    return None;
                }
                if samples.len() == 1 {
                    return Some(0.);
                }
                let at = |time| {
                    let index = samples.partition_point(|sample| sample.tick <= time);
                    samples[index.saturating_sub(1)].openness
                };
                Some(if tick > 0 && tick < last_tick {
                    (at(tick - 1) + at(tick) + at(tick + 1)) / 3.
                } else {
                    at(tick)
                })
            }
        }
    }
}

fn word(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_tracks_hold_sparse_keys_and_smooth_only_interior_ticks() {
        let mut bytes = b"LPD\0".to_vec();
        for word in [1, 32, 3, (-80.0f32).to_bits()] {
            bytes.extend(word.to_le_bytes());
        }
        for (tick, phoneme, level) in [(0u32, 0u32, -80.0f32), (2, 1, -20.), (4, 0, -80.)] {
            for word in [tick, phoneme, 0, 0, 0, 0, 0, level.to_bits()] {
                bytes.extend(word.to_le_bytes());
            }
        }
        let track = Track::parse(1, &bytes).unwrap();
        for (tick, value) in [0., 1. / 3., 2. / 3., 2. / 3., 0.].into_iter().enumerate() {
            assert_eq!(track.sample(tick as u32), Some(value));
        }
        assert!(track.sample(5).is_none());
        assert!(Track::parse(1, &bytes[..bytes.len() - 1]).is_none());
    }
}
