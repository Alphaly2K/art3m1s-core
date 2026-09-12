//! Versioned C ABI table exposed through the core facade.
//!
//! This is the single entry point hosts should query. Individual exported
//! symbols remain temporarily for migration, but new host code must use this
//! table so the eventual flat-symbol allowlist can be reduced without another
//! ABI change.

use crate::ffi::{
    art3m1s_clear_font_override, art3m1s_probe_caption, art3m1s_runtime_advance_and_present,
    art3m1s_runtime_advance_and_render, art3m1s_runtime_advance_without_render,
    art3m1s_runtime_backend_capabilities, art3m1s_runtime_clear_external_surface,
    art3m1s_runtime_configure_spatial_upscale, art3m1s_runtime_create, art3m1s_runtime_destroy,
    art3m1s_runtime_feed_click, art3m1s_runtime_feed_key, art3m1s_runtime_feed_mouse,
    art3m1s_runtime_feed_mouse_button, art3m1s_runtime_feed_touch,
    art3m1s_runtime_is_exit_requested, art3m1s_runtime_load_project,
    art3m1s_runtime_load_project_bytes, art3m1s_runtime_notify_lifecycle,
    art3m1s_runtime_notify_sound_finished, art3m1s_runtime_notify_video_finished,
    art3m1s_runtime_pixel_buffer_size, art3m1s_runtime_profiler_snapshot,
    art3m1s_runtime_set_emote_backend, art3m1s_runtime_set_external_surface,
    art3m1s_runtime_set_profiler_enabled, art3m1s_runtime_set_render_quality_preset,
    art3m1s_runtime_set_reported_os, art3m1s_runtime_set_resources,
    art3m1s_runtime_set_string_variable, art3m1s_runtime_set_volume, art3m1s_runtime_stage_height,
    art3m1s_runtime_stage_width, art3m1s_runtime_submit_dialog, art3m1s_runtime_submit_http_result,
    art3m1s_runtime_submit_text_translation, art3m1s_runtime_upload_video_layer_frame,
    art3m1s_set_angle_path, art3m1s_set_damage_visualization, art3m1s_set_debug,
    art3m1s_set_font_override,
};
use crate::host_events::{
    HostEvents, art3m1s_clear_host_state_v1, art3m1s_host_events_create,
    art3m1s_host_events_destroy, art3m1s_host_events_enable_v1, art3m1s_host_events_next_v1,
    art3m1s_poll_events_v1, art3m1s_set_font_list_v1, art3m1s_set_text_replacements_v1,
    art3m1s_set_text_translation_enabled_v1, art3m1s_set_window_state_v1,
};
use crate::host_files::{
    HostResources, art3m1s_resources_clear, art3m1s_resources_clear_overrides,
    art3m1s_resources_create, art3m1s_resources_destroy, art3m1s_resources_mount_directory,
    art3m1s_resources_mount_pfs, art3m1s_resources_set_override, art3m1s_resources_set_save_dir,
};
use crate::runtime::CoreRuntime;
use std::ffi::{c_char, c_int, c_void};

pub const ART3M1S_API_ABI_VERSION: u32 = 1;
pub const ART3M1S_API_ABI_MAGIC: u64 = 0x415254334D314150; // "ART3M1AP"

type HostEventsCreateFn = unsafe extern "C" fn() -> *mut HostEvents;
type HostEventsDestroyFn = unsafe extern "C" fn(*mut HostEvents);
type HostEventsEnableFn = unsafe extern "C" fn(*mut HostEvents, i32);
type HostEventsNextFn = unsafe extern "C" fn(*mut HostEvents) -> usize;
type PollEventsFn = unsafe extern "C" fn(*mut HostEvents, *mut u8, usize, *mut u32) -> usize;
type SetFontListFn = unsafe extern "C" fn(*mut HostEvents, i32, i32, *const u8, usize) -> i32;
type SetWindowStateFn = unsafe extern "C" fn(*mut HostEvents, i32);
type SetTextReplacementsFn = unsafe extern "C" fn(*mut HostEvents, *const u8, usize) -> i32;
type SetTextTranslationEnabledFn = unsafe extern "C" fn(*mut HostEvents, i32);
type ClearHostStateFn = unsafe extern "C" fn(*mut HostEvents);

type ResourcesCreateFn = unsafe extern "C" fn() -> *mut HostResources;
type ResourcesDestroyFn = unsafe extern "C" fn(*mut HostResources);
type ResourcesClearFn = unsafe extern "C" fn(*mut HostResources);
type ResourcesMountDirectoryFn = unsafe extern "C" fn(*mut HostResources, *const c_char) -> c_int;
type ResourcesMountPfsFn =
    unsafe extern "C" fn(*mut HostResources, *const c_char, *const c_char) -> c_int;
type ResourcesSetSaveDirFn = unsafe extern "C" fn(*mut HostResources, *const c_char) -> c_int;
type ResourcesSetOverrideFn =
    unsafe extern "C" fn(*mut HostResources, *const c_char, *const u8, usize) -> c_int;

type RuntimeCreateFn = unsafe extern "C" fn(u32, u32, i32) -> *mut CoreRuntime;
type RuntimeDestroyFn = unsafe extern "C" fn(*mut CoreRuntime);
type RuntimeSetResourcesFn = unsafe extern "C" fn(*mut CoreRuntime, *mut HostResources) -> i32;
type RuntimeLoadProjectFn =
    unsafe extern "C" fn(*mut CoreRuntime, *const c_char, *const c_char) -> i32;
type RuntimeLoadProjectBytesFn =
    unsafe extern "C" fn(*mut CoreRuntime, *const u8, usize, *const c_char) -> i32;
type RuntimeStageFn = unsafe extern "C" fn(*const CoreRuntime) -> u32;
type RuntimePixelBufferSizeFn = unsafe extern "C" fn(*const CoreRuntime) -> u32;
type RuntimeAdvanceAndRenderFn = unsafe extern "C" fn(*mut CoreRuntime, u32, *mut u8, u32) -> u32;
type RuntimeAdvanceFn = unsafe extern "C" fn(*mut CoreRuntime, u32) -> i32;
type RuntimeSetExternalSurfaceFn =
    unsafe extern "C" fn(*mut CoreRuntime, i32, *mut c_void, u32, u32) -> i32;
type RuntimeClearExternalSurfaceFn = unsafe extern "C" fn(*mut CoreRuntime);
type RuntimeFeedMouseFn = unsafe extern "C" fn(*mut CoreRuntime, i32, i32);
type RuntimeFeedClickFn = unsafe extern "C" fn(*mut CoreRuntime);
type RuntimeFeedMouseButtonFn = unsafe extern "C" fn(*mut CoreRuntime, u32, i32);
type RuntimeFeedTouchFn = unsafe extern "C" fn(*mut CoreRuntime, u32, u8, i32, i32);
type RuntimeFeedKeyFn = unsafe extern "C" fn(*mut CoreRuntime, u32, i32);
type RuntimeSubmitDialogFn = unsafe extern "C" fn(*mut CoreRuntime, i32, *const c_char) -> i32;
type RuntimeSubmitTextTranslationFn =
    unsafe extern "C" fn(*mut CoreRuntime, u64, *const c_char) -> i32;
type RuntimeSetReportedOsFn = unsafe extern "C" fn(*mut CoreRuntime, *const c_char);
type RuntimeSetEmoteBackendFn = unsafe extern "C" fn(*mut CoreRuntime, i32) -> i32;
type RuntimeConfigureSpatialUpscaleFn = unsafe extern "C" fn(*mut CoreRuntime, f32, f32) -> i32;
type RuntimeSetRenderQualityPresetFn = unsafe extern "C" fn(*mut CoreRuntime, i32) -> i32;
type RuntimeSetProfilerEnabledFn = unsafe extern "C" fn(*const CoreRuntime, i32);
type RuntimeProfilerSnapshotFn = unsafe extern "C" fn(*const CoreRuntime, *mut u8, u32) -> i32;
type RuntimeSetVolumeFn = unsafe extern "C" fn(*mut CoreRuntime, *const c_char, f32);
type RuntimeNotifyFinishedFn = unsafe extern "C" fn(*mut CoreRuntime, *const c_char);
type RuntimeNotifyLifecycleFn = unsafe extern "C" fn(*mut CoreRuntime, i32);
type RuntimeIsExitRequestedFn = unsafe extern "C" fn(*const CoreRuntime) -> i32;
type RuntimeBackendCapabilitiesFn = unsafe extern "C" fn(*const CoreRuntime) -> u64;
type RuntimeSubmitHttpResultFn = unsafe extern "C" fn(*mut CoreRuntime, i32, *const u8, i32) -> i32;
type RuntimeSetStringVariableFn =
    unsafe extern "C" fn(*mut CoreRuntime, *const c_char, *const c_char);
type RuntimeSetRuntimeMediaEnabledFn = unsafe extern "C" fn(*mut CoreRuntime, i32);
type RuntimeUploadVideoLayerFrameFn =
    unsafe extern "C" fn(*mut CoreRuntime, *const c_char, u32, u32, *const u8, usize) -> i32;

#[repr(C)]
pub struct Art3m1sApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,

    pub host_events_create: Option<HostEventsCreateFn>,
    pub host_events_destroy: Option<HostEventsDestroyFn>,
    pub host_events_enable: Option<HostEventsEnableFn>,
    pub host_events_next: Option<HostEventsNextFn>,
    pub poll_events: Option<PollEventsFn>,
    pub set_font_list: Option<SetFontListFn>,
    pub set_window_state: Option<SetWindowStateFn>,
    pub set_text_replacements: Option<SetTextReplacementsFn>,
    pub set_text_translation_enabled: Option<SetTextTranslationEnabledFn>,
    pub clear_host_state: Option<ClearHostStateFn>,

    pub resources_create: Option<ResourcesCreateFn>,
    pub resources_destroy: Option<ResourcesDestroyFn>,
    pub resources_clear: Option<ResourcesClearFn>,
    pub resources_mount_directory: Option<ResourcesMountDirectoryFn>,
    pub resources_mount_pfs: Option<ResourcesMountPfsFn>,
    pub resources_set_save_dir: Option<ResourcesSetSaveDirFn>,
    pub resources_set_override: Option<ResourcesSetOverrideFn>,
    pub resources_clear_overrides: Option<ResourcesClearFn>,

    pub runtime_create: Option<RuntimeCreateFn>,
    pub runtime_destroy: Option<RuntimeDestroyFn>,
    pub runtime_set_resources: Option<RuntimeSetResourcesFn>,
    pub runtime_set_runtime_media_enabled: Option<RuntimeSetRuntimeMediaEnabledFn>,
    pub runtime_advance_and_present: Option<RuntimeAdvanceFn>,
    pub runtime_advance_without_render: Option<RuntimeAdvanceFn>,
    pub runtime_stage_width: Option<RuntimeStageFn>,
    pub runtime_stage_height: Option<RuntimeStageFn>,

    pub runtime_load_project: Option<RuntimeLoadProjectFn>,
    pub runtime_load_project_bytes: Option<RuntimeLoadProjectBytesFn>,
    pub runtime_pixel_buffer_size: Option<RuntimePixelBufferSizeFn>,
    pub runtime_advance_and_render: Option<RuntimeAdvanceAndRenderFn>,
    pub runtime_set_external_surface: Option<RuntimeSetExternalSurfaceFn>,
    pub runtime_clear_external_surface: Option<RuntimeClearExternalSurfaceFn>,
    pub runtime_feed_mouse: Option<RuntimeFeedMouseFn>,
    pub runtime_feed_click: Option<RuntimeFeedClickFn>,
    pub runtime_feed_mouse_button: Option<RuntimeFeedMouseButtonFn>,
    pub runtime_feed_touch: Option<RuntimeFeedTouchFn>,
    pub runtime_feed_key: Option<RuntimeFeedKeyFn>,
    pub runtime_submit_dialog: Option<RuntimeSubmitDialogFn>,
    pub runtime_submit_text_translation: Option<RuntimeSubmitTextTranslationFn>,
    pub runtime_set_reported_os: Option<RuntimeSetReportedOsFn>,
    pub runtime_set_emote_backend: Option<RuntimeSetEmoteBackendFn>,
    pub runtime_configure_spatial_upscale: Option<RuntimeConfigureSpatialUpscaleFn>,
    pub runtime_set_render_quality_preset: Option<RuntimeSetRenderQualityPresetFn>,
    pub runtime_set_profiler_enabled: Option<RuntimeSetProfilerEnabledFn>,
    pub runtime_profiler_snapshot: Option<RuntimeProfilerSnapshotFn>,
    pub runtime_set_volume: Option<RuntimeSetVolumeFn>,
    pub runtime_notify_video_finished: Option<RuntimeNotifyFinishedFn>,
    pub runtime_notify_sound_finished: Option<RuntimeNotifyFinishedFn>,
    pub runtime_notify_lifecycle: Option<RuntimeNotifyLifecycleFn>,
    pub runtime_is_exit_requested: Option<RuntimeIsExitRequestedFn>,
    pub runtime_backend_capabilities: Option<RuntimeBackendCapabilitiesFn>,
    pub runtime_submit_http_result: Option<RuntimeSubmitHttpResultFn>,
    pub runtime_set_string_variable: Option<RuntimeSetStringVariableFn>,

    pub probe_caption: Option<
        unsafe extern "C" fn(
            *mut HostResources,
            *const u8,
            usize,
            *const c_char,
            *mut u8,
            i32,
        ) -> i32,
    >,
    pub set_angle_path: Option<unsafe extern "C" fn(*const c_char)>,
    pub set_debug: Option<unsafe extern "C" fn(i32)>,
    pub set_damage_visualization: Option<unsafe extern "C" fn(i32)>,
    pub set_font_override: Option<unsafe extern "C" fn(*const u8, i32) -> i32>,
    pub clear_font_override: Option<unsafe extern "C" fn()>,
    pub runtime_upload_video_layer_frame: Option<RuntimeUploadVideoLayerFrameFn>,
}

static API_V1: Art3m1sApiV1 = Art3m1sApiV1 {
    struct_size: std::mem::size_of::<Art3m1sApiV1>() as u32,
    abi_version: ART3M1S_API_ABI_VERSION,
    magic: ART3M1S_API_ABI_MAGIC,
    host_events_create: Some(art3m1s_host_events_create),
    host_events_destroy: Some(art3m1s_host_events_destroy),
    host_events_enable: Some(art3m1s_host_events_enable_v1),
    host_events_next: Some(art3m1s_host_events_next_v1),
    poll_events: Some(art3m1s_poll_events_v1),
    set_font_list: Some(art3m1s_set_font_list_v1),
    set_window_state: Some(art3m1s_set_window_state_v1),
    set_text_replacements: Some(art3m1s_set_text_replacements_v1),
    set_text_translation_enabled: Some(art3m1s_set_text_translation_enabled_v1),
    clear_host_state: Some(art3m1s_clear_host_state_v1),
    resources_create: Some(art3m1s_resources_create),
    resources_destroy: Some(art3m1s_resources_destroy),
    resources_clear: Some(art3m1s_resources_clear),
    resources_mount_directory: Some(art3m1s_resources_mount_directory),
    resources_mount_pfs: Some(art3m1s_resources_mount_pfs),
    resources_set_save_dir: Some(art3m1s_resources_set_save_dir),
    resources_set_override: Some(art3m1s_resources_set_override),
    resources_clear_overrides: Some(art3m1s_resources_clear_overrides),
    runtime_create: Some(art3m1s_runtime_create),
    runtime_destroy: Some(art3m1s_runtime_destroy),
    runtime_set_resources: Some(art3m1s_runtime_set_resources),
    #[cfg(feature = "ffmpeg")]
    runtime_set_runtime_media_enabled: Some(
        crate::ffi::art3m1s_runtime_set_runtime_media_enabled_v1,
    ),
    #[cfg(not(feature = "ffmpeg"))]
    runtime_set_runtime_media_enabled: None,
    runtime_advance_and_present: Some(art3m1s_runtime_advance_and_present),
    runtime_advance_without_render: Some(art3m1s_runtime_advance_without_render),
    runtime_stage_width: Some(art3m1s_runtime_stage_width),
    runtime_stage_height: Some(art3m1s_runtime_stage_height),
    runtime_load_project: Some(art3m1s_runtime_load_project),
    runtime_load_project_bytes: Some(art3m1s_runtime_load_project_bytes),
    runtime_pixel_buffer_size: Some(art3m1s_runtime_pixel_buffer_size),
    runtime_advance_and_render: Some(art3m1s_runtime_advance_and_render),
    runtime_set_external_surface: Some(art3m1s_runtime_set_external_surface),
    runtime_clear_external_surface: Some(art3m1s_runtime_clear_external_surface),
    runtime_feed_mouse: Some(art3m1s_runtime_feed_mouse),
    runtime_feed_click: Some(art3m1s_runtime_feed_click),
    runtime_feed_mouse_button: Some(art3m1s_runtime_feed_mouse_button),
    runtime_feed_touch: Some(art3m1s_runtime_feed_touch),
    runtime_feed_key: Some(art3m1s_runtime_feed_key),
    runtime_submit_dialog: Some(art3m1s_runtime_submit_dialog),
    runtime_submit_text_translation: Some(art3m1s_runtime_submit_text_translation),
    runtime_set_reported_os: Some(art3m1s_runtime_set_reported_os),
    runtime_set_emote_backend: Some(art3m1s_runtime_set_emote_backend),
    runtime_configure_spatial_upscale: Some(art3m1s_runtime_configure_spatial_upscale),
    runtime_set_render_quality_preset: Some(art3m1s_runtime_set_render_quality_preset),
    runtime_set_profiler_enabled: Some(art3m1s_runtime_set_profiler_enabled),
    runtime_profiler_snapshot: Some(art3m1s_runtime_profiler_snapshot),
    runtime_set_volume: Some(art3m1s_runtime_set_volume),
    runtime_notify_video_finished: Some(art3m1s_runtime_notify_video_finished),
    runtime_notify_sound_finished: Some(art3m1s_runtime_notify_sound_finished),
    runtime_notify_lifecycle: Some(art3m1s_runtime_notify_lifecycle),
    runtime_is_exit_requested: Some(art3m1s_runtime_is_exit_requested),
    runtime_backend_capabilities: Some(art3m1s_runtime_backend_capabilities),
    runtime_submit_http_result: Some(art3m1s_runtime_submit_http_result),
    runtime_set_string_variable: Some(art3m1s_runtime_set_string_variable),
    probe_caption: Some(art3m1s_probe_caption),
    set_angle_path: Some(art3m1s_set_angle_path),
    set_debug: Some(art3m1s_set_debug),
    set_damage_visualization: Some(art3m1s_set_damage_visualization),
    set_font_override: Some(art3m1s_set_font_override),
    clear_font_override: Some(art3m1s_clear_font_override),
    runtime_upload_video_layer_frame: Some(art3m1s_runtime_upload_video_layer_frame),
};

/// Returns the process-lifetime v1 API table.
///
/// `out_size` receives the exact struct size known to this build. Hosts must
/// reject a mismatch before reading any field.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_get_api_v1(out_size: *mut usize) -> *const Art3m1sApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<Art3m1sApiV1>() };
    }
    std::ptr::addr_of!(API_V1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_v1_identity_and_size_are_stable() {
        let mut size = 0usize;
        let api = unsafe { art3m1s_get_api_v1(&mut size) };
        assert!(!api.is_null());
        assert_eq!(size, std::mem::size_of::<Art3m1sApiV1>());
        let api = unsafe { &*api };
        assert_eq!(api.magic, ART3M1S_API_ABI_MAGIC);
        assert_eq!(api.abi_version, ART3M1S_API_ABI_VERSION);
        assert_eq!(api.struct_size as usize, size);
    }

    #[test]
    fn api_v1_required_entries_are_present() {
        let api = &API_V1;
        assert!(api.host_events_create.is_some());
        assert!(api.host_events_destroy.is_some());
        assert!(api.host_events_enable.is_some());
        assert!(api.poll_events.is_some());
        assert!(api.resources_create.is_some());
        assert!(api.resources_destroy.is_some());
        assert!(api.resources_mount_directory.is_some());
        assert!(api.resources_mount_pfs.is_some());
        assert!(api.runtime_create.is_some());
        assert!(api.runtime_destroy.is_some());
        assert!(api.runtime_set_resources.is_some());
        assert!(api.runtime_advance_and_present.is_some());
        assert!(api.runtime_feed_key.is_some());
    }
}
