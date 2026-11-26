use super::*;
use crate::artemis_emote::ArtemisEmoteLayer;
use crate::render_pipeline::draw::DrawCommand;
use art3m1s_emote::{EmoteDrawItem, EmoteModel, PsbDocument};
use asb_interpreter::EmoteLayerCommand;

const MAX_EMOTE_LAYERS: usize = 128;
// Two 128 MiB models can each have a pending replacement during a transition.
const MAX_EMOTE_BYTES: usize = 512 * 1024 * 1024;
pub(super) const EMOTE_TEXTURE_BASE: u64 = 1 << 59;

#[derive(Default)]
pub(super) struct SceneEmote {
    layers: std::collections::BTreeMap<String, Layer>,
    generation: u64,
    texture_revision: u64,
}

struct Layer {
    active: Model,
    pending: Option<Model>,
    attach_to_scene: bool,
}

struct Model {
    playback: ArtemisEmoteLayer,
    textures: HashMap<String, Arc<FrameTexture>>,
    texture_bytes: usize,
    items: Vec<EmoteDrawItem>,
    dirty: bool,
}

impl SceneEmote {
    pub(super) fn retain(&mut self, scene: &crate::compositor::Scene) {
        self.layers
            .retain(|id, layer| layer.attach_to_scene || scene.get(id).is_some());
    }

    pub(super) fn attach(&mut self, compositor: &mut crate::compositor::Compositor) {
        for (id, layer) in &mut self.layers {
            if std::mem::take(&mut layer.attach_to_scene) {
                compositor.ensure_layer(id);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.layers.clear();
    }

    pub(super) fn advance(&mut self, delta_ms: u64) -> bool {
        let frames = delta_ms as f32 * 60.0 / 1000.0;
        let mut changed = false;
        for layer in self.layers.values_mut() {
            for model in std::iter::once(&mut layer.active).chain(&mut layer.pending) {
                let advanced = model.playback.advance(frames);
                model.dirty |= advanced;
                changed |= advanced;
            }
        }
        changed
    }

    fn create(
        &mut self,
        id: &str,
        bytes: Vec<u8>,
        [width, height]: [u32; 2],
    ) -> Result<bool, String> {
        if id.is_empty() || width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("invalid E-Mote layer dimensions or ID".into());
        }
        if self.layers.len() >= MAX_EMOTE_LAYERS && !self.layers.contains_key(id) {
            return Err("E-Mote layer limit reached".into());
        }
        let mut remaining = MAX_EMOTE_BYTES
            - self
                .layers
                .iter()
                .map(|(key, layer)| {
                    layer.active.texture_bytes
                        + layer
                            .pending
                            .as_ref()
                            .filter(|_| key.as_str() != id)
                            .map_or(0, |pending| pending.texture_bytes)
                })
                .sum::<usize>();
        let available = remaining;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("E-Mote generation limit reached")?;
        let document = PsbDocument::from_bytes(bytes).map_err(|error| error.to_string())?;
        let mut model = EmoteModel::from_document(document).map_err(|error| error.to_string())?;
        let (_, sources) = model
            .take_texture_data()
            .map_err(|error| error.to_string())?;
        let mut textures = HashMap::new();
        let mut revision = self.texture_revision;
        for (id, texture) in model.atlas().textures() {
            let (width, height) = (texture.width, texture.height);
            let size = usize::try_from(u64::from(width) * u64::from(height))
                .ok()
                .and_then(|pixels| pixels.checked_mul(4))
                .filter(|&size| size > 0 && size <= remaining && width <= 8192 && height <= 8192)
                .ok_or("E-Mote atlas exceeds the texture budget")?;
            let source = sources.get(id).ok_or("missing embedded E-Mote texture")?;
            let rgba = model
                .atlas()
                .decode_texture_data_rgba8(id, source.as_bytes())
                .map_err(|error| error.to_string())?;
            if rgba.len() != size {
                return Err("invalid E-Mote atlas size".into());
            }
            revision = revision
                .checked_add(1)
                .filter(|id| *id < EMOTE_TEXTURE_BASE)
                .ok_or("E-Mote texture revision limit reached")?;
            textures.insert(
                id.clone(),
                Arc::new(FrameTexture {
                    revision: EMOTE_TEXTURE_BASE | revision,
                    width,
                    height,
                    rgba: rgba.into_boxed_slice(),
                }),
            );
            remaining -= size;
        }
        let instance = Model {
            playback: ArtemisEmoteLayer::new(model, generation, width, height),
            textures,
            texture_bytes: available - remaining,
            items: Vec::new(),
            dirty: true,
        };
        self.generation = generation;
        self.texture_revision = revision;
        if let Some(layer) = self.layers.get_mut(id) {
            layer.pending = Some(instance);
            layer.attach_to_scene = true;
            Ok(true)
        } else {
            self.layers.insert(
                id.into(),
                Layer {
                    active: instance,
                    pending: None,
                    attach_to_scene: true,
                },
            );
            Ok(false)
        }
    }

    pub(super) fn commands(
        &mut self,
        mut texture: impl FnMut(Arc<FrameTexture>) -> (crate::compositor::TextureId, TextureInfo),
    ) -> HashMap<String, Vec<DrawCommand>> {
        self.layers
            .iter_mut()
            .map(|(id, layer)| {
                let model = layer.pending.as_mut().unwrap_or(&mut layer.active);
                if std::mem::take(&mut model.dirty) {
                    model.items = model.playback.evaluate().unwrap_or_default();
                }
                let draws = model
                    .items
                    .iter()
                    .filter_map(|item| {
                        let (index, info) = texture(model.textures.get(&item.texture_id)?.clone());
                        model.playback.draw_command(item.clone(), index, info)
                    })
                    .collect();
                (id.clone(), draws)
            })
            .collect()
    }
}

fn emote_error(message: impl ToString) -> asb_interpreter::Error {
    asb_interpreter::Error::RuntimeError {
        line: 0,
        message: message.to_string(),
    }
}

impl SceneCallbacks {
    pub(super) fn resolve_emote_path(&self, path: &str) -> asb_interpreter::Result<String> {
        let scene = self.0.lock().unwrap();
        if !scene.emote_enabled {
            return Err(emote_error("E-Mote renderer capability is unavailable"));
        }
        logical_key(&crate::magic_path::resolve_path(&scene.magic_paths, path)).map_err(emote_error)
    }

    pub(super) fn create_emote_model(
        &self,
        id: &str,
        file: &str,
        bytes: Vec<u8>,
        size: [u32; 2],
    ) -> asb_interpreter::Result<bool> {
        let mut scene = self.0.lock().unwrap();
        if !scene.emote_enabled {
            return Err(emote_error("E-Mote renderer capability is unavailable"));
        }
        let pending = scene
            .emote
            .create(id, bytes, size)
            .map_err(|error| emote_error(format!("E-Mote model {file}: {error}")))?;
        // Lua can queue deletion of the old scene layer before loading its replacement.
        // Attach the new model after those tags have run.
        scene.dirty = true;
        Ok(pending)
    }

    pub(super) fn get_emote_model(&self, id: &str, next: bool) -> Option<bool> {
        let mut scene = self.0.lock().unwrap();
        let layer = scene.emote.layers.get_mut(id)?;
        if next && let Some(pending) = layer.pending.take() {
            layer.active = pending;
            scene.dirty = true;
        }
        Some(false)
    }

    pub(super) fn update_emote_model(
        &self,
        id: &str,
        next: bool,
        command: EmoteLayerCommand,
    ) -> asb_interpreter::Result<()> {
        let mut scene = self.0.lock().unwrap();
        let layer = scene
            .emote
            .layers
            .get_mut(id)
            .ok_or_else(|| emote_error("unknown E-Mote layer"))?;
        let model = if next {
            layer
                .pending
                .as_mut()
                .ok_or_else(|| emote_error("no pending E-Mote model"))?
        } else {
            &mut layer.active
        };
        model.playback.command(command);
        model.dirty = true;
        scene.dirty = true;
        Ok(())
    }
}
