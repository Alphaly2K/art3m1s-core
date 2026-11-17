use asb_interpreter::{Interpreter, Value, VariableStore};
use std::collections::HashMap;

pub(crate) const SAVEG_FILE: &str = "saveg.dat";
pub(crate) const SYSTEM_FILE: &str = "system.dat";

// Only persisted settings survive a runtime restart. Derived s.* values
// (status, message layers, dimensions and paths) are initialized per runtime.
pub(crate) fn is_persistent_system_variable(name: &str) -> bool {
    matches!(
        name,
        "bgmvol" | "sevol" | "videovol" | "automodewait" | "enablemaximizedwindow"
    ) || name
        .strip_prefix("segain.")
        .is_some_and(|id| !id.is_empty())
}

pub(crate) fn apply_persistent_variables(
    interpreter: &mut Interpreter,
    values: HashMap<String, Value>,
    is_global: bool,
) -> usize {
    let prefix = if is_global { "g." } else { "s." };
    let mut count = 0;
    for (name, value) in values {
        if is_global || is_persistent_system_variable(&name) {
            interpreter.set_variable(&format!("{prefix}{name}"), value);
            count += 1;
        }
    }
    count
}

pub(crate) fn persistent_system_snapshot(store: &VariableStore) -> HashMap<String, Value> {
    store
        .iter_system()
        .filter(|(name, _)| is_persistent_system_variable(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}
