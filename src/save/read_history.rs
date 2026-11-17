use std::collections::{HashMap, HashSet};

pub(crate) const AREAD_FILE: &str = "aread.dat";

#[derive(Clone, Debug, Default)]
pub(crate) struct ReadHistory(HashMap<String, HashSet<usize>>);

impl ReadHistory {
    pub(crate) fn contains(&self, script: &str, line: usize) -> bool {
        self.0
            .get(script)
            .is_some_and(|lines| lines.contains(&line))
    }

    pub(crate) fn insert(&mut self, script: &str, line: usize) -> bool {
        if let Some(lines) = self.0.get_mut(script) {
            lines.insert(line)
        } else {
            self.0.insert(script.to_owned(), HashSet::from([line]));
            true
        }
    }

    pub(crate) fn export(&self) -> HashMap<String, Vec<usize>> {
        self.0
            .iter()
            .map(|(script, lines)| {
                let mut lines: Vec<_> = lines.iter().copied().collect();
                lines.sort_unstable();
                (script.clone(), lines)
            })
            .collect()
    }

    pub(crate) fn import(&mut self, data: HashMap<String, Vec<usize>>) {
        for (script, lines) in data {
            self.0.entry(script).or_default().extend(lines);
        }
    }
}
