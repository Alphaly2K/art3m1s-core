//! Artemis magic path resolution for runtime asset requests.
//!
//! Scripts use paths such as `:bg/room` and populate the prefix table through
//! `e:setMagicPath`. Runtime resolves those prefixes before asking the host to
//! load assets, while physical I/O stays in the host.

use std::collections::HashMap;
use std::sync::Mutex;

pub type MagicPathTable = Mutex<HashMap<String, String>>;

/// Resolve `:name/rest` through the magic-path table.
///
/// Names without a `:` prefix are returned unchanged. If a prefix is not
/// registered, Artemis image assets conventionally fall back to `image/rest`.
pub fn resolve_path(table: &MagicPathTable, name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        let (ns, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let map = table.lock().unwrap();
        if let Some(prefix) = map.get(ns) {
            return format!("{prefix}/{tail}");
        }
        return format!("image/{rest}");
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_registered_prefix() {
        let table = MagicPathTable::new(HashMap::from([("mv".to_string(), "movie".to_string())]));

        assert_eq!(resolve_path(&table, ":mv/opening"), "movie/opening");
    }

    #[test]
    fn falls_back_for_unknown_prefix() {
        let table = MagicPathTable::new(HashMap::new());

        assert_eq!(resolve_path(&table, ":bg/title"), "image/bg/title");
    }

    #[test]
    fn keeps_plain_paths_unchanged() {
        let table = MagicPathTable::new(HashMap::new());

        assert_eq!(resolve_path(&table, "voice/line001"), "voice/line001");
    }
}
