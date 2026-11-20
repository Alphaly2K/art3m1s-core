//! Artemis runtime and Host interfaces.
//!
//! The standalone package exposes a queued API for Hosts that provide their own
//! I/O, renderer, and media services. The legacy build includes the v1 adapters.

macro_rules! legacy_exports {
    ($($module:ident),+ $(,)?) => {$(
        #[cfg(feature = "legacy")]
        pub mod $module;
        #[cfg(not(feature = "legacy"))]
        #[allow(dead_code, unused_imports)]
        mod $module;
    )+};
}

legacy_exports!(compositor, host, render_pipeline, save, text);

#[cfg(not(feature = "legacy"))]
pub use host::api;
#[cfg(feature = "legacy")]
pub mod audio;
#[cfg(feature = "legacy")]
pub mod backend;
mod emote_playback;
#[cfg(feature = "legacy")]
pub mod ffi;
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub use ffi::api as ffi_api;
#[cfg(feature = "legacy")]
pub use host::{events as host_events, files as host_files, media as host_media};
#[cfg(not(feature = "legacy"))]
mod core_log;
#[cfg(not(feature = "legacy"))]
pub(crate) use core_log::{core_debug, core_info, core_warn, emit as __core_log};
#[cfg(feature = "legacy")]
mod legacy_project;
#[cfg(feature = "legacy")]
pub use legacy_project::{Project, resolve_project_path};
mod magic_path;
mod project_config;
#[cfg(not(feature = "legacy"))]
use project_config::{CoreError, ProjectConfig};
#[cfg(feature = "legacy")]
pub use project_config::{CoreError, ProjectConfig, Result};
#[cfg(all(
    feature = "legacy",
    any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod platform;
#[cfg(feature = "legacy")]
mod profiler;
#[cfg(feature = "legacy")]
pub mod runtime;
mod script_handler;
#[cfg(feature = "legacy")]
pub mod video;
mod video_layer;

#[cfg(feature = "legacy")]
pub use art3m1s_emote as emote;
#[cfg(feature = "legacy")]
pub use art3m1s_media as media;
#[cfg(feature = "legacy")]
pub use asb_interpreter as script;
#[cfg(feature = "legacy")]
pub use pfs_upk as archive;
