//! Compatibility checks against local game data.
//!
//! These tests are ignored by default because the fixtures cannot be shipped
//! with the repository. See `tests/README.md` for setup and invocation.

mod common;

#[path = "compatibility/dialog_save_chain.rs"]
mod dialog_save_chain;
#[path = "compatibility/full_boot_restore.rs"]
mod full_boot_restore;
#[path = "compatibility/game_entry.rs"]
mod game_entry;
#[path = "compatibility/nekomiko_entry.rs"]
mod nekomiko_entry;
#[path = "compatibility/popfunc_backtoback.rs"]
mod popfunc_backtoback;
#[path = "compatibility/sv_save_real.rs"]
mod sv_save_real;
#[path = "compatibility/syssave_roundtrip.rs"]
mod syssave_roundtrip;
