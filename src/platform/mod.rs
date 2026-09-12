//! Platform-specific core helpers.

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) mod mobile_astc;
