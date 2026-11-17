//! Native render selection for platform builds of the legacy runtime.

#![cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux",
    target_os = "android"
))]

pub use art3m1s_render::backend;
