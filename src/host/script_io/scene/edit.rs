use super::*;
use crate::compositor::{Scene, lyedit::apply_mode, props::parse_hex_color};

const EDIT_PREFIX: &str = ":host/layer-edit/";

#[derive(Default)]
pub(super) struct LayerEdits {
    layers: HashMap<String, Edited>,
    revision: u64,
}

struct Edited {
    base: String,
    texture: Arc<FrameTexture>,
}

impl LayerEdits {
    pub fn clear(&mut self) {
        self.layers.clear();
    }

    pub fn remove(&mut self, id: &str) {
        self.layers.remove(id);
    }

    pub fn retain_bound(&mut self, scene: &Scene) {
        self.layers.retain(|id, edited| {
            scene.get(id).and_then(|layer| layer.file.as_deref()) == Some(&edited.base)
        });
    }

    pub fn files<'a>(&'a self, scene: &'a Scene) -> impl Iterator<Item = (String, String)> + 'a {
        self.layers
            .iter()
            .filter(|(id, edit)| {
                scene.get(id).and_then(|layer| layer.file.as_deref()) == Some(&edit.base)
            })
            .map(|(id, _)| (id.clone(), format!("{EDIT_PREFIX}{id}")))
    }

    pub fn resolve(&self, name: &str) -> Option<Arc<FrameTexture>> {
        Some(
            self.layers
                .get(name.strip_prefix(EDIT_PREFIX)?)?
                .texture
                .clone(),
        )
    }

    pub fn is_edit_key(name: &str) -> bool {
        name.starts_with(EDIT_PREFIX)
    }

    pub fn apply(
        &mut self,
        scene: &Scene,
        assets: &HashMap<String, Option<Arc<FrameTexture>>>,
        event: &Event,
    ) -> bool {
        let Event::LayerEdit {
            id,
            mode,
            color,
            file,
            left,
            top,
        } = event
        else {
            return false;
        };
        let Some(base) = scene.get(id).and_then(|layer| layer.file.as_ref()) else {
            return false;
        };
        let Some(source) = self
            .layers
            .get(id)
            .filter(|edited| &edited.base == base)
            .map(|edited| &edited.texture)
            .or_else(|| assets.get(base)?.as_ref())
        else {
            return false;
        };
        let overlay = if mode == "blend" {
            let Some(texture) = file
                .as_ref()
                .and_then(|file| assets.get(file))
                .and_then(Option::as_ref)
            else {
                return false;
            };
            Some((texture.width, texture.height, texture.rgba()))
        } else if matches!(
            mode.as_str(),
            "negative" | "gray" | "sepia" | "add" | "multiply"
        ) {
            None
        } else {
            return false;
        };
        let used: usize = self
            .layers
            .iter()
            .filter(|(layer, _)| *layer != id)
            .map(|(_, edited)| edited.texture.rgba.len())
            .sum();
        if source.rgba.len() > MAX_TEXTURE_BYTES - used {
            return false;
        }
        let Some(revision) = self.revision.checked_add(1).filter(|id| *id < 1 << 60) else {
            return false;
        };
        let color = color
            .as_deref()
            .and_then(parse_hex_color)
            .map(|[_, r, g, b]| [r, g, b]);
        let mut rgba = source.rgba.clone();
        let (width, height) = (source.width, source.height);
        apply_mode(
            &mut rgba,
            width,
            height,
            mode,
            color,
            overlay,
            left.unwrap_or(0),
            top.unwrap_or(0),
        );
        self.revision = revision;
        self.layers.insert(
            id.clone(),
            Edited {
                base: base.clone(),
                texture: Arc::new(FrameTexture {
                    revision: (1 << 60) | revision,
                    width,
                    height,
                    rgba,
                }),
            },
        );
        true
    }
}
