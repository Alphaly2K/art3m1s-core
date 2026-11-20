use super::*;
use crate::host::frame::{FrameShader, FrameShaderEffect, FrameShaderLanguage};
use crate::host::runtime::{CAP_GLSL, CAP_HLSL};
use crate::render_pipeline::draw::ShaderEffect;
use std::collections::BTreeMap;

const MAX_SHADER_BYTES: usize = 4 * 1024 * 1024;
const MAX_SHADERS: usize = 256;
const DIAGNOSTIC_SHADER_UNAVAILABLE: u32 = 1;

pub(in crate::host::script_io) fn shader_language(asset: &str) -> FrameShaderLanguage {
    let leaf = asset.rsplit('/').next().unwrap_or(asset);
    if leaf.rsplit_once('.').is_some_and(|(_, ext)| {
        ["glsl", "frag", "fsh", "fs"]
            .iter()
            .any(|suffix| ext.eq_ignore_ascii_case(suffix))
    }) {
        FrameShaderLanguage::Glsl
    } else {
        FrameShaderLanguage::Hlsl
    }
}

pub(in crate::host::script_io) fn shader_language_name(
    language: FrameShaderLanguage,
) -> &'static str {
    match language {
        FrameShaderLanguage::Hlsl => "hlsl",
        FrameShaderLanguage::Glsl => "glsl",
    }
}

pub(in crate::host::script_io) fn supports_shader_asset(caps: u64, asset: &str) -> bool {
    let required = match shader_language(asset) {
        FrameShaderLanguage::Hlsl => CAP_HLSL,
        FrameShaderLanguage::Glsl => CAP_GLSL,
    };
    caps & required != 0
}

#[derive(Default)]
pub(in crate::host::script_io) struct SceneShaders {
    sources: HashMap<String, RegisteredShader>,
    bytes: usize,
    revision: u64,
}

struct RegisteredShader {
    asset: String,
    frame: Arc<FrameShader>,
}

impl SceneShaders {
    pub(in crate::host::script_io) fn register(
        &mut self,
        id: String,
        asset: String,
        source: Vec<u8>,
    ) -> bool {
        let previous = self.sources.get(&id);
        if id.is_empty()
            || source.is_empty()
            || source.len() > MAX_SHADER_SOURCE_BYTES
            || (previous.is_none() && self.sources.len() >= MAX_SHADERS)
        {
            return false;
        }
        let bytes =
            self.bytes - previous.map_or(0, |shader| shader.frame.source.len()) + source.len();
        let Some(revision) = self
            .revision
            .checked_add(1)
            .filter(|_| bytes <= MAX_SHADER_BYTES)
        else {
            return false;
        };
        self.sources.insert(
            id,
            RegisteredShader {
                frame: Arc::new(FrameShader {
                    revision,
                    language: shader_language(&asset),
                    source: source.into_boxed_slice(),
                }),
                asset,
            },
        );
        self.revision = revision;
        self.bytes = bytes;
        true
    }

    pub(super) fn effect(&self, effect: &ShaderEffect) -> Option<FrameShaderEffect> {
        Some(FrameShaderEffect {
            shader: self.sources.get(&effect.name)?.frame.clone(),
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
    pub(in crate::host::script_io) fn capture_shader_assets(
        &self,
    ) -> Option<BTreeMap<String, String>> {
        let scene = self.scene.lock().unwrap();
        (scene.shader_caps != 0).then(|| {
            scene
                .shaders
                .sources
                .iter()
                .map(|(id, shader)| (id.clone(), shader.asset.clone()))
                .collect()
        })
    }

    pub(in crate::host::script_io) fn prepare_saved_shaders(
        &self,
        assets: &BTreeMap<String, String>,
    ) -> Result<Option<SceneShaders>, ProtocolError> {
        let scene = self.scene.lock().unwrap();
        if scene.shader_caps == 0 {
            return Ok(None);
        }
        if assets.len() > MAX_SHADERS {
            return Err(ProtocolError::Limit);
        }
        for (id, path) in assets {
            if id.is_empty() || logical_key(path)? != *path {
                return Err(ProtocolError::Argument("invalid saved shader id or asset"));
            }
        }
        Ok(Some(SceneShaders {
            revision: scene.shaders.revision,
            ..Default::default()
        }))
    }

    pub(in crate::host::script_io) fn shader_pending(&self) -> bool {
        self.pending_shader.is_some()
    }

    pub(in crate::host::script_io) fn prepare_shader(
        &mut self,
        id: &str,
        path: &str,
        transport: &mut SessionTransport,
    ) -> Result<Option<String>, ProtocolError> {
        let Ok(path) = self.resolve_asset_key(path) else {
            return Ok(None);
        };
        if !supports_shader_asset(self.scene.lock().unwrap().shader_caps, &path) {
            let language = shader_language_name(shader_language(&path));
            transport.diagnostic(
                DIAGNOSTIC_SHADER_UNAVAILABLE,
                MessageBody::new(
                    json!({"language": language, "id": id, "key": path}).to_string(),
                    vec![],
                )?,
            )?;
            return Ok(None);
        }
        self.pending_shader = Some((id.into(), path.clone()));
        Ok(Some(path))
    }

    pub(in crate::host::script_io) fn complete_shader(&mut self, response: IoResponse) {
        let Some((id, asset)) = self.pending_shader.take() else {
            return;
        };
        if let IoResponse::Read(source) = response {
            let mut scene = self.scene.lock().unwrap();
            scene.dirty |= scene.shaders.register(id, asset, source);
        }
    }
}
