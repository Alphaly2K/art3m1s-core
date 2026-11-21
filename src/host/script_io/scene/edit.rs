use super::*;
use crate::compositor::{Scene, lyedit::apply_mode, props::parse_hex_color};
use std::collections::BTreeMap;

const EDIT_PREFIX: &str = ":host/layer-edit/";
const MAX_EDIT_STEPS: usize = 4096;
const MAX_REPLAY_BYTES: usize = 512 * 1024 * 1024;

pub(in crate::host::script_io) type EditSnapshot = BTreeMap<String, Vec<EditStep>>;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(in crate::host::script_io) enum EditStep {
    Negative,
    Gray,
    Sepia,
    Add { color: Option<[u8; 3]> },
    Multiply { color: Option<[u8; 3]> },
    Blend { asset: String, left: i32, top: i32 },
}

impl EditStep {
    fn work_bytes(
        &self,
        size: usize,
        assets: &HashMap<String, Option<Arc<FrameTexture>>>,
    ) -> Option<usize> {
        let overlay = match self {
            Self::Blend { asset, .. } => assets.get(asset)?.as_ref()?.rgba.len(),
            _ => 0,
        };
        Some(size.saturating_add(overlay))
    }

    fn apply(
        &self,
        width: u32,
        height: u32,
        rgba: &mut [u8],
        assets: &HashMap<String, Option<Arc<FrameTexture>>>,
    ) -> bool {
        let (mode, color, overlay, left, top) = match self {
            Self::Negative => ("negative", None, None, 0, 0),
            Self::Gray => ("gray", None, None, 0, 0),
            Self::Sepia => ("sepia", None, None, 0, 0),
            Self::Add { color } => ("add", *color, None, 0, 0),
            Self::Multiply { color } => ("multiply", *color, None, 0, 0),
            Self::Blend { asset, left, top } => {
                let Some(texture) = assets.get(asset).and_then(Option::as_ref) else {
                    return false;
                };
                (
                    "blend",
                    None,
                    Some((texture.width, texture.height, texture.rgba())),
                    *left,
                    *top,
                )
            }
        };
        apply_mode(rgba, width, height, mode, color, overlay, left, top);
        true
    }
}

#[derive(Default)]
pub(in crate::host::script_io) struct LayerEdits {
    layers: HashMap<String, Edited>,
    revision: u64,
}

struct Edited {
    base: String,
    texture: Arc<FrameTexture>,
    steps: Option<Vec<EditStep>>,
}

impl LayerEdits {
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

    pub(super) fn overlay_assets(&self) -> impl Iterator<Item = &str> {
        self.layers
            .values()
            .flat_map(|edited| edited.steps.iter().flatten())
            .filter_map(|step| match step {
                EditStep::Blend { asset, .. } => Some(asset.as_str()),
                _ => None,
            })
    }

    pub(super) fn capture(
        &self,
        assets: &HashMap<String, Option<Arc<FrameTexture>>>,
    ) -> Result<EditSnapshot, ProtocolError> {
        let mut work = 0usize;
        let mut snapshot = EditSnapshot::new();
        for (id, edited) in &self.layers {
            let steps = edited.steps.as_ref().ok_or(ProtocolError::Limit)?;
            for step in steps {
                work = work.saturating_add(
                    step.work_bytes(edited.texture.rgba.len(), assets)
                        .ok_or(ProtocolError::Argument("saved edit overlay is unavailable"))?,
                );
                if work > MAX_REPLAY_BYTES {
                    return Err(ProtocolError::Limit);
                }
            }
            snapshot.insert(id.clone(), steps.clone());
        }
        Ok(snapshot)
    }

    pub(super) fn required_assets<'a>(
        scene: &'a Scene,
        snapshot: &'a EditSnapshot,
    ) -> Result<Vec<&'a str>, ProtocolError> {
        let mut assets = Vec::new();
        let mut count = 0;
        for (id, steps) in snapshot {
            let base = scene
                .get(id)
                .and_then(|layer| layer.file.as_deref())
                .ok_or(ProtocolError::Argument(
                    "saved edit references a missing layer texture",
                ))?;
            if steps.is_empty() || steps.len() > MAX_EDIT_STEPS - count {
                return Err(ProtocolError::Limit);
            }
            count += steps.len();
            assets.push(base);
            assets.extend(steps.iter().filter_map(|step| match step {
                EditStep::Blend { asset, .. } => Some(asset.as_str()),
                _ => None,
            }));
        }
        Ok(assets)
    }

    pub(super) fn restore(
        &self,
        scene: &Scene,
        snapshot: EditSnapshot,
        assets: &HashMap<String, Option<Arc<FrameTexture>>>,
    ) -> Result<Self, IoFailure> {
        let mut edits = Self {
            revision: self.revision,
            ..Default::default()
        };
        let mut bytes = 0usize;
        let mut work = 0usize;
        for (id, steps) in snapshot {
            let base = scene
                .get(&id)
                .and_then(|layer| layer.file.as_ref())
                .ok_or(IoFailure::IoError)?;
            let source = assets
                .get(base)
                .and_then(Option::as_ref)
                .ok_or(IoFailure::NotFound)?;
            let size = source.rgba.len();
            bytes = bytes.saturating_add(size);
            if bytes > MAX_TEXTURE_BYTES {
                return Err(IoFailure::Limit);
            }
            let mut rgba = source.rgba.clone();
            for step in &steps {
                work =
                    work.saturating_add(step.work_bytes(size, assets).ok_or(IoFailure::NotFound)?);
                if work > MAX_REPLAY_BYTES {
                    return Err(IoFailure::Limit);
                }
                if !step.apply(source.width, source.height, &mut rgba, assets) {
                    return Err(IoFailure::NotFound);
                }
            }
            if !edits.store(
                id,
                base.clone(),
                source.width,
                source.height,
                rgba,
                Some(steps),
            ) {
                return Err(IoFailure::Limit);
            }
        }
        Ok(edits)
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
        let previous = self.layers.get(id).filter(|edited| &edited.base == base);
        let Some(source) = previous
            .map(|edited| &edited.texture)
            .or_else(|| assets.get(base)?.as_ref())
        else {
            return false;
        };
        let color = color
            .as_deref()
            .and_then(parse_hex_color)
            .map(|[_, r, g, b]| [r, g, b]);
        let step = match mode.as_str() {
            "negative" => EditStep::Negative,
            "gray" => EditStep::Gray,
            "sepia" => EditStep::Sepia,
            "add" => EditStep::Add { color },
            "multiply" => EditStep::Multiply { color },
            "blend" => {
                let Some(asset) = file.clone() else {
                    return false;
                };
                EditStep::Blend {
                    asset,
                    left: left.unwrap_or(0),
                    top: top.unwrap_or(0),
                }
            }
            _ => return false,
        };
        let (used, steps_elsewhere) = self.layers.iter().filter(|(layer, _)| *layer != id).fold(
            (0, 0),
            |(bytes, count), (_, edited)| {
                (
                    bytes + edited.texture.rgba.len(),
                    count + edited.steps.as_ref().map_or(0, Vec::len),
                )
            },
        );
        if source.rgba.len() > MAX_TEXTURE_BYTES - used {
            return false;
        }
        let mut rgba = source.rgba.clone();
        let (width, height) = (source.width, source.height);
        if !step.apply(width, height, &mut rgba, assets) {
            return false;
        }
        if self.revision >= (1 << 60) - 1 {
            return false;
        }
        let steps = match self
            .layers
            .get_mut(id)
            .filter(|edited| &edited.base == base)
        {
            Some(edited) => edited.steps.take(),
            None => Some(Vec::new()),
        }
        .filter(|steps| steps.len() + steps_elsewhere < MAX_EDIT_STEPS)
        .map(|mut steps| {
            steps.push(step);
            steps
        });
        self.store(id.clone(), base.clone(), width, height, rgba, steps)
    }

    fn store(
        &mut self,
        id: String,
        base: String,
        width: u32,
        height: u32,
        rgba: Box<[u8]>,
        steps: Option<Vec<EditStep>>,
    ) -> bool {
        let Some(revision) = self.revision.checked_add(1).filter(|id| *id < 1 << 60) else {
            return false;
        };
        self.revision = revision;
        self.layers.insert(
            id,
            Edited {
                base,
                texture: Arc::new(FrameTexture {
                    revision: (1 << 60) | revision,
                    width,
                    height,
                    rgba,
                }),
                steps,
            },
        );
        true
    }
}
