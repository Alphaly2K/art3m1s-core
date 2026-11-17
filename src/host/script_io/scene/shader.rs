use super::*;
use crate::host::frame::{FrameShader, FrameShaderEffect};
use crate::render_pipeline::draw::ShaderEffect;

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_SHADER_BYTES: usize = 4 * 1024 * 1024;
const MAX_SHADERS: usize = 256;

#[derive(Default)]
pub(super) struct SceneShaders {
    sources: HashMap<String, Arc<FrameShader>>,
    bytes: usize,
    revision: u64,
}

impl SceneShaders {
    fn register(&mut self, id: String, source: Vec<u8>) -> bool {
        let previous = self.sources.get(&id);
        if id.is_empty()
            || source.is_empty()
            || source.len() > MAX_SOURCE_BYTES
            || (previous.is_none() && self.sources.len() >= MAX_SHADERS)
        {
            return false;
        }
        let bytes = self.bytes - previous.map_or(0, |shader| shader.source.len()) + source.len();
        let Some(revision) = self
            .revision
            .checked_add(1)
            .filter(|_| bytes <= MAX_SHADER_BYTES)
        else {
            return false;
        };
        self.sources.insert(
            id,
            Arc::new(FrameShader {
                revision,
                source: source.into_boxed_slice(),
            }),
        );
        self.revision = revision;
        self.bytes = bytes;
        true
    }

    pub(super) fn effect(&self, effect: &ShaderEffect) -> Option<FrameShaderEffect> {
        Some(FrameShaderEffect {
            shader: self.sources.get(&effect.name)?.clone(),
            uniforms: effect
                .uniforms
                .iter()
                .filter(|(name, values)| {
                    name.len() <= 255 && values.iter().all(|value| value.is_finite())
                })
                .take(64)
                .map(|(name, values)| (name.clone(), values.iter().copied().take(64).collect()))
                .collect(),
            mask: effect.mask_texture.map(|id| id.0 as usize),
            user: effect.user_texture.map(|id| id.0 as usize),
        })
    }
}

impl SceneIo {
    pub(in crate::host::script_io) fn shader_pending(&self) -> bool {
        self.pending_shader.is_some()
    }

    pub(in crate::host::script_io) fn prepare_shader(
        &mut self,
        id: &str,
        path: &str,
    ) -> Option<IoRequest> {
        let path = self.resolve_media_asset(path).ok()?;
        self.pending_shader = Some(id.into());
        Some(IoRequest::Read { path })
    }

    pub(in crate::host::script_io) fn complete_shader(&mut self, response: IoResponse) {
        let Some(id) = self.pending_shader.take() else {
            return;
        };
        if let IoResponse::Read(source) = response {
            let mut scene = self.scene.lock().unwrap();
            scene.dirty |= scene.shaders.register(id, source);
        }
    }
}
