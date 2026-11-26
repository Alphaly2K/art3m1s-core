use art3m1s_emote::{EmoteModel, PsbDocument, PsbResourceData};
use asb_interpreter::EmoteLayerCommand;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::CoreRuntime;
use crate::artemis_emote::ArtemisEmoteLayer;
use crate::host_files::HostResources;
use crate::render_pipeline::draw::{DrawCommand, TextureId, TextureInfo, TextureProvider};

pub(super) type SharedEmoteState = Arc<Mutex<EmoteState>>;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct EmoteProfileStats {
    pub worker_eval_ns: u64,
    pub scene_clone_ns: u64,
    pub draw_build_ns: u64,
    pub mesh_build_ns: u64,
    pub worker_updates: u64,
    pub worker_input_frames: u64,
    pub worker_dropped_scenes: u64,
    pub sprites: u64,
    pub mesh_sprites: u64,
    pub mesh_vertices: u64,
}

pub(super) struct EmoteState {
    layers: BTreeMap<String, LayerSlots>,
    next_generation: u64,
}

impl Default for EmoteState {
    fn default() -> Self {
        Self {
            layers: BTreeMap::new(),
            next_generation: 0,
        }
    }
}

#[derive(Default)]
struct LayerSlots {
    active: Option<EmoteInstance>,
    pending: Option<EmoteInstance>,
    attach_to_scene: bool,
}

struct EmoteInstance {
    playback: ArtemisEmoteLayer,
    textures: BTreeMap<String, EmoteTextureState>,
    texture_source_bytes: usize,
    resources: HostResources,
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    astc_encoder: Option<crate::platform::mobile_astc::AstcEncoder>,
}

struct EmoteTextureState {
    name: String,
    width: u32,
    height: u32,
    gpu: Option<(TextureId, TextureInfo)>,
    source: Option<PsbResourceData>,
}

impl EmoteState {
    pub(super) fn profile_memory(&self) -> (usize, u64) {
        let mut instances = 0usize;
        let mut source_bytes = 0u64;
        for slots in self.layers.values() {
            for instance in [&slots.active, &slots.pending].into_iter().flatten() {
                instances += 1;
                source_bytes = source_bytes.saturating_add(instance.source_bytes());
            }
        }
        (instances, source_bytes)
    }

    /// 内置 E-Mote 不做耗时采样；保留接口供运行时 profiler 调用。
    pub(super) fn set_profile_enabled(&mut self, _enabled: bool) {}

    /// Eluna 后端移除后 E-Mote 不再产出细分耗时，快照字段保持为 0。
    pub(super) fn take_profile_stats(&self) -> EmoteProfileStats {
        EmoteProfileStats::default()
    }

    pub fn create_layer(
        &mut self,
        id: &str,
        files: Vec<(String, Vec<u8>)>,
        width: u32,
        height: u32,
        resources: HostResources,
    ) -> Result<bool, String> {
        if files.len() != 1 {
            return Err(format!(
                "E-Mote layer {id} requires exactly one embedded-texture PSB, got {}",
                files.len()
            ));
        }
        let (path, bytes) = files
            .into_iter()
            .next()
            .ok_or_else(|| format!("E-Mote layer {id} has no model file"))?;
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let instance = EmoteInstance::new(generation, &path, bytes, width, height, resources)?;
        let slots = self.layers.entry(id.to_string()).or_default();
        slots.attach_to_scene = true;
        if slots.active.is_none() {
            slots.active = Some(instance);
            Ok(false)
        } else {
            slots.pending = Some(instance);
            Ok(true)
        }
    }

    pub fn get_layer(&mut self, id: &str, next: bool) -> Option<bool> {
        let slots = self.layers.get_mut(id)?;
        if next && slots.pending.is_some() {
            slots.active = slots.pending.take();
        }
        if slots.active.is_some() {
            Some(false)
        } else if slots.pending.is_some() {
            Some(true)
        } else {
            None
        }
    }

    pub fn command(
        &mut self,
        id: &str,
        next: bool,
        command: EmoteLayerCommand,
    ) -> Result<(), String> {
        let slots = self
            .layers
            .get_mut(id)
            .ok_or_else(|| format!("unknown E-Mote layer {id}"))?;
        let instance = if next {
            slots.pending.as_mut()
        } else {
            slots.active.as_mut()
        }
        .ok_or_else(|| {
            format!(
                "E-Mote layer {id} has no {} instance",
                if next { "pending" } else { "active" }
            )
        })?;

        instance.playback.command(command);
        Ok(())
    }

    pub fn advance(&mut self, delta_ms: u64) -> bool {
        let frames = delta_ms as f32 * 60.0 / 1000.0;
        let mut changed = false;
        for slots in self.layers.values_mut() {
            for instance in [&mut slots.active, &mut slots.pending]
                .into_iter()
                .flatten()
            {
                changed |= instance.playback.advance(frames);
            }
        }
        changed
    }

    pub fn take_scene_attachments(&mut self) -> Vec<String> {
        self.layers
            .iter_mut()
            .filter_map(|(id, slots)| {
                std::mem::take(&mut slots.attach_to_scene).then(|| id.clone())
            })
            .collect()
    }

    pub fn retain_scene_layers(&mut self, scene_ids: &HashSet<String>) {
        self.layers.retain(|id, _| scene_ids.contains(id));
    }

    pub fn clear(&mut self) -> usize {
        let count = self.layers.len();
        self.layers.clear();
        count
    }

    pub fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
    ) -> (HashMap<String, Vec<DrawCommand>>, HashSet<String>) {
        let mut commands = HashMap::new();
        let mut retained = HashSet::new();
        for (layer_id, slots) in &mut self.layers {
            let Some(instance) = slots.pending.as_mut().or(slots.active.as_mut()) else {
                continue;
            };
            match instance.build_commands(provider, &mut retained) {
                Ok(draws) if !draws.is_empty() => {
                    commands.insert(layer_id.clone(), draws);
                }
                Ok(_) => {}
                Err(error) => {
                    crate::core_debug!("[E-Mote] layer {layer_id} render failed: {error}");
                }
            }
        }
        (commands, retained)
    }
}

impl EmoteInstance {
    fn new(
        generation: u64,
        path: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        resources: HostResources,
    ) -> Result<Self, String> {
        let document = PsbDocument::from_bytes(bytes)
            .map_err(|error| format!("failed to parse E-Mote model {path}: {error}"))?;
        let mut model = EmoteModel::from_document(document)
            .map_err(|error| format!("failed to load E-Mote model {path}: {error}"))?;
        let (texture_source_bytes, mut texture_data) = model
            .take_texture_data()
            .map_err(|error| format!("failed to detach E-Mote textures {path}: {error}"))?;
        let mut textures = BTreeMap::new();
        for (texture_id, texture) in model.atlas().textures() {
            textures.insert(
                texture_id.clone(),
                EmoteTextureState {
                    name: format!(":emote/{generation}/{texture_id}"),
                    width: texture.width,
                    height: texture.height,
                    gpu: None,
                    source: texture_data.remove(texture_id),
                },
            );
        }
        Ok(Self {
            playback: ArtemisEmoteLayer::new(model, generation, width, height),
            textures,
            texture_source_bytes,
            resources,
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            astc_encoder: None,
        })
    }

    fn source_bytes(&self) -> u64 {
        let live_sources = self
            .textures
            .values()
            .filter_map(|texture| texture.source.as_ref())
            .map(|source| source.as_bytes().len() as u64)
            .sum::<u64>();
        (self.texture_source_bytes as u64).max(live_sources)
    }
}

impl EmoteInstance {
    fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
        retained: &mut HashSet<String>,
    ) -> Result<Vec<DrawCommand>, String> {
        for (texture_id, texture) in &mut self.textures {
            retained.insert(texture.name.clone());
            if texture.gpu.is_none() {
                let compressed = texture.source.as_ref().ok_or_else(|| {
                    format!("E-Mote texture {texture_id} was evicted after source release")
                })?;
                let compressed = compressed.as_bytes();
                texture.gpu = provider.upload_dxt5_render_only(
                    &texture.name,
                    texture.width,
                    texture.height,
                    compressed,
                );

                #[cfg(any(
                    target_os = "android",
                    target_os = "ios",
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                let mut decoded_rgba = None;
                #[cfg(any(
                    target_os = "android",
                    target_os = "ios",
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                if texture.gpu.is_none() && provider.supports_astc_4x4() {
                    let cache_path = astc_cache_path(compressed, texture.width, texture.height);
                    if let Ok(cached) = self.resources.read_file(&cache_path)
                        && crate::platform::mobile_astc::astc_4x4_len(texture.width, texture.height)
                            == Some(cached.len())
                    {
                        texture.gpu = provider.upload_astc_4x4_render_only(
                            &texture.name,
                            texture.width,
                            texture.height,
                            &cached,
                        );
                    }
                    if texture.gpu.is_none() {
                        let rgba = self
                            .playback
                            .model()
                            .atlas()
                            .decode_texture_data_rgba8(texture_id, compressed)
                            .map_err(|error| {
                                format!("failed to decode E-Mote texture {texture_id}: {error}")
                            })?;
                        if self.astc_encoder.is_none() {
                            match crate::platform::mobile_astc::AstcEncoder::new() {
                                Ok(encoder) => self.astc_encoder = Some(encoder),
                                Err(error) => {
                                    crate::core_warn!("[E-Mote] ASTC encoder unavailable: {error}");
                                }
                            }
                        }
                        if let Some(encoder) = self.astc_encoder.as_mut() {
                            match encoder.encode_rgba8(texture.width, texture.height, &rgba) {
                                Ok(astc) => {
                                    if let Err(error) = self.resources.write(&cache_path, &astc) {
                                        crate::core_debug!(
                                            "[E-Mote] ASTC cache write failed {cache_path}: {error}"
                                        );
                                    }
                                    texture.gpu = provider.upload_astc_4x4_render_only(
                                        &texture.name,
                                        texture.width,
                                        texture.height,
                                        &astc,
                                    );
                                }
                                Err(error) => {
                                    crate::core_warn!(
                                        "[E-Mote] ASTC encode failed for {texture_id}: {error}"
                                    );
                                }
                            }
                        }
                        decoded_rgba = Some(rgba);
                    }
                }
                if texture.gpu.is_none() {
                    #[cfg(any(
                        target_os = "android",
                        target_os = "ios",
                        all(target_os = "macos", target_arch = "aarch64")
                    ))]
                    let rgba = if let Some(rgba) = decoded_rgba.take() {
                        rgba
                    } else {
                        self.playback
                            .model()
                            .atlas()
                            .decode_texture_data_rgba8(texture_id, compressed)
                            .map_err(|error| {
                                format!("failed to decode E-Mote texture {texture_id}: {error}")
                            })?
                    };
                    #[cfg(not(any(
                        target_os = "android",
                        target_os = "ios",
                        all(target_os = "macos", target_arch = "aarch64")
                    )))]
                    let rgba = self
                        .playback
                        .model()
                        .atlas()
                        .decode_texture_data_rgba8(texture_id, compressed)
                        .map_err(|error| {
                            format!("failed to decode E-Mote texture {texture_id}: {error}")
                        })?;
                    texture.gpu = provider.upload_rgba_render_only(
                        &texture.name,
                        texture.width,
                        texture.height,
                        &rgba,
                    );
                }
                if texture.gpu.is_some() {
                    texture.source = None;
                }
            }
        }
        if self
            .textures
            .values()
            .all(|texture| texture.source.is_none())
        {
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            {
                self.astc_encoder = None;
            }
            let released = std::mem::take(&mut self.texture_source_bytes);
            if released != 0 {
                crate::core_info!(
                    "[E-Mote] released {:.1} MiB shared texture source after GPU upload",
                    released as f64 / (1024.0 * 1024.0)
                );
            }
        }

        Ok(self
            .playback
            .evaluate()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|item| {
                let (texture, info) = self.textures.get(&item.texture_id)?.gpu?;
                self.playback.draw_command(item, texture, info)
            })
            .collect())
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "macos", target_arch = "aarch64")
))]
fn astc_cache_path(compressed: &[u8], width: u32, height: u32) -> String {
    let hash = compressed
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
        });
    format!("cache/emote/astc4x4/{hash:016x}-{width}x{height}.bin")
}

impl CoreRuntime {
    pub(super) fn clear_emote_state(&mut self, reason: &str) {
        let layers = self.emote.lock().unwrap().clear();
        self.gpu.begin_access();
        let textures = self.gpu.evict_texture_prefix(":emote/");
        self.gpu.end_access();
        if layers != 0 || textures != 0 {
            crate::core_info!(
                "[E-Mote] cleared {layers} layer(s) and {textures} GPU texture(s): {reason}"
            );
        }
        self.last_submitted_frame = None;
    }

    pub(super) fn sync_emote_scene(&mut self) {
        let attachments = self.emote.lock().unwrap().take_scene_attachments();
        for id in attachments {
            self.compositor.ensure_layer(&id);
        }
        let scene_ids = self
            .compositor
            .scene()
            .iter_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        self.emote.lock().unwrap().retain_scene_layers(&scene_ids);
    }

    pub(super) fn build_emote_commands(
        &mut self,
    ) -> (HashMap<String, Vec<DrawCommand>>, HashSet<String>) {
        self.emote.lock().unwrap().build_commands(&mut *self.gpu)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use art3m1s_emote::{EmoteMotionEvaluator, EmoteRenderState};

    use super::{EmoteLayerCommand, EmoteState};
    use crate::compositor::mock::MockProvider;
    use crate::render_pipeline::draw::DrawList;

    fn nekomiko_model_path() -> std::path::PathBuf {
        let root = std::env::var_os("ART3M1S_FIXTURE_NEKOMIKO_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("ART3M1S_FIXTURES_DIR")
                    .map(std::path::PathBuf::from)
                    .map(|base| base.join("nekomiko"))
            })
            .expect(
                "set ART3M1S_FIXTURE_NEKOMIKO_DIR or ART3M1S_FIXTURES_DIR before running ignored compatibility tests",
            );
        root.join("image/fhd/fg/aya/tay_0.psb")
    }

    #[test]
    #[ignore = "requires the external nekomiko fixture"]
    fn builds_nekomiko_draw_commands() {
        let path = nekomiko_model_path();
        let bytes = std::fs::read(&path).unwrap();

        let mut state = EmoteState::default();
        assert!(
            !state
                .create_layer(
                    "1.0",
                    vec![(path.display().to_string(), bytes)],
                    1600,
                    1350,
                    crate::host_files::default_resources().clone(),
                )
                .unwrap()
        );
        let instance = state.layers["1.0"].active.as_ref().unwrap();
        assert!(instance.playback.model().source_document().is_none());
        assert!(
            instance
                .textures
                .values()
                .all(|texture| texture.source.is_some())
        );
        {
            let instance = state.layers["1.0"].active.as_ref().unwrap();
            let items = EmoteMotionEvaluator::new(instance.playback.model())
                .evaluate_base(&EmoteRenderState {
                    motion_time: 0.0,
                    variables: BTreeMap::from([("face_eye_open".to_string(), 5.0)]),
                })
                .unwrap();
            let position = |label: &str| {
                items
                    .iter()
                    .position(|item| item.layer_label == label)
                    .unwrap_or_else(|| panic!("missing E-Mote eye layer {label}"))
            };
            assert!(position("eye_L") < position("mabuta"));
            assert!(position("shirome") < position("mabuta"));
        }
        state
            .command(
                "1.0",
                false,
                EmoteLayerCommand::SetScale {
                    scale: 0.6,
                    origin_x: 0.0,
                    origin_y: 0.0,
                },
            )
            .unwrap();
        state
            .command(
                "1.0",
                false,
                EmoteLayerCommand::PlayTimeline {
                    label: "笑顔_ボイス再生用".to_string(),
                    flags: 1,
                },
            )
            .unwrap();
        state.advance(16);
        state.advance(3_000);

        let mut provider = MockProvider::new();
        let (commands, retained) = state.build_commands(&mut provider);
        assert!(!commands["1.0"].is_empty());
        assert_eq!(retained.len(), 8);
        assert!(
            state.layers["1.0"]
                .active
                .as_ref()
                .unwrap()
                .playback
                .model()
                .source_document()
                .is_none()
        );
        assert!(
            state.layers["1.0"]
                .active
                .as_ref()
                .unwrap()
                .textures
                .values()
                .all(|texture| texture.source.is_none())
        );
        assert!(commands["1.0"].iter().any(|command| command.mesh.is_some()));
        let mut frame = DrawList {
            commands: commands["1.0"].clone(),
            ..DrawList::default()
        };
        frame.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        assert!(!frame.mask_commands.is_empty());
        assert!(
            frame
                .shader_groups
                .iter()
                .any(|group| group.mask_range.is_some())
        );
        assert_eq!(state.clear(), 1);
        assert!(state.layers.is_empty());
    }
}
