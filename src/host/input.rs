//! Key configuration and input shared by Artemis runtimes.

use std::collections::HashMap;

pub(crate) fn parse_keyconfig(params: &HashMap<String, String>) -> Option<(i32, Vec<u32>)> {
    let role = params.get("role")?.trim().parse::<i32>().ok()?;
    let keys = params
        .get("keys")
        .map(|raw| {
            raw.split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|key| key.parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default();
    Some((role, keys))
}

pub(crate) const OVERRIDE_IS_PUSH: u32 = 2;
pub(crate) const OVERRIDE_IS_DOWN: u32 = 4;
pub(crate) const OVERRIDE_IS_DOWN_EDGE: u32 = 8;
pub(crate) const OVERRIDE_IS_UP_EDGE: u32 = 16;
pub(crate) const OVERRIDE_IS_DECIDE: u32 = 32;

#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
mod queued;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
pub(in crate::host) use queued::InputFrame;
#[cfg(any(test, not(feature = "legacy"), feature = "host-abi-v2"))]
pub use queued::{HostInput, TouchPhase};
