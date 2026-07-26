use std::collections::{BTreeMap, BTreeSet};

use crate::{
    EmoteAtlas, EmoteError, EmoteEyeControl, EmoteMotionLibrary, EmoteSelectorControl,
    EmoteTimeline, EmoteVariable, PsbDocument, PsbValue, Result,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmoteModelInfo {
    pub type_id: Option<String>,
    pub spec: Option<String>,
    pub base_chara: Option<String>,
    pub base_motion: Option<String>,
    pub characters: Vec<String>,
    pub motions: Vec<String>,
    pub timelines: Vec<String>,
    pub variables: Vec<String>,
    pub screen_width: u32,
    pub screen_height: u32,
    pub texture_count: usize,
    pub icon_count: usize,
}

#[derive(Debug)]
pub struct EmoteModel {
    document: PsbDocument,
    info: EmoteModelInfo,
    atlas: EmoteAtlas,
    motions: EmoteMotionLibrary,
    timelines: BTreeMap<String, EmoteTimeline>,
    variables: BTreeMap<String, EmoteVariable>,
    selectors: Vec<EmoteSelectorControl>,
    eye_controls: Vec<EmoteEyeControl>,
}

impl EmoteModel {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::from_document(PsbDocument::open(path)?)
    }

    pub fn from_document(document: PsbDocument) -> Result<Self> {
        let atlas = EmoteAtlas::from_document(&document)?;
        let motions = EmoteMotionLibrary::parse(&document.root)?;
        let (timelines, variables, selectors, eye_controls) = parse_controls(&document.root);
        let info = inspect_model(&document.root, &atlas, &timelines, &variables)?;
        if info.type_id.as_deref() != Some("motion") {
            return Err(EmoteError::InvalidFormat(format!(
                "PSB type is {:?}, expected motion",
                info.type_id
            )));
        }
        Ok(Self {
            document,
            info,
            atlas,
            motions,
            timelines,
            variables,
            selectors,
            eye_controls,
        })
    }

    pub fn document(&self) -> &PsbDocument {
        &self.document
    }

    pub fn info(&self) -> &EmoteModelInfo {
        &self.info
    }

    pub fn atlas(&self) -> &EmoteAtlas {
        &self.atlas
    }

    pub fn timelines(&self) -> &BTreeMap<String, EmoteTimeline> {
        &self.timelines
    }

    pub fn motions(&self) -> &EmoteMotionLibrary {
        &self.motions
    }

    pub fn variables(&self) -> &BTreeMap<String, EmoteVariable> {
        &self.variables
    }

    pub fn selectors(&self) -> &[EmoteSelectorControl] {
        &self.selectors
    }

    pub fn eye_controls(&self) -> &[EmoteEyeControl] {
        &self.eye_controls
    }

    pub fn apply_selector_controls(&self, variables: &mut BTreeMap<String, f32>) {
        for selector in &self.selectors {
            let value = variables.get(&selector.label).copied().unwrap_or(0.0);
            selector.apply(value, variables);
        }
    }
}

fn inspect_model(
    root: &PsbValue,
    atlas: &EmoteAtlas,
    timelines: &BTreeMap<String, EmoteTimeline>,
    variables: &BTreeMap<String, EmoteVariable>,
) -> Result<EmoteModelInfo> {
    let object = root
        .as_object()
        .ok_or_else(|| EmoteError::InvalidFormat("motion PSB root is not an object".into()))?;

    let type_id = object
        .get("id")
        .and_then(PsbValue::as_str)
        .map(str::to_owned);
    let spec = object
        .get("spec")
        .and_then(PsbValue::as_str)
        .map(str::to_owned);
    let base_chara = root
        .at_path(&["metadata", "base", "chara"])
        .and_then(PsbValue::as_str)
        .map(str::to_owned);
    let base_motion = root
        .at_path(&["metadata", "base", "motion"])
        .and_then(PsbValue::as_str)
        .map(str::to_owned);

    let mut characters = BTreeSet::new();
    let mut motions = BTreeSet::new();
    if let Some(character_map) = object.get("object").and_then(PsbValue::as_object) {
        for (character, value) in character_map {
            characters.insert(character.clone());
            if let Some(motion_map) = value.get("motion").and_then(PsbValue::as_object) {
                motions.extend(motion_map.keys().cloned());
            }
        }
    }

    Ok(EmoteModelInfo {
        type_id,
        spec,
        base_chara,
        base_motion,
        characters: characters.into_iter().collect(),
        motions: motions.into_iter().collect(),
        timelines: timelines.keys().cloned().collect(),
        variables: variables.keys().cloned().collect(),
        screen_width: root
            .at_path(&["screenSize", "width"])
            .and_then(PsbValue::as_i64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0),
        screen_height: root
            .at_path(&["screenSize", "height"])
            .and_then(PsbValue::as_i64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0),
        texture_count: atlas.textures().len(),
        icon_count: atlas.icons().len(),
    })
}

fn parse_controls(
    root: &PsbValue,
) -> (
    BTreeMap<String, EmoteTimeline>,
    BTreeMap<String, EmoteVariable>,
    Vec<EmoteSelectorControl>,
    Vec<EmoteEyeControl>,
) {
    let timelines = root
        .at_path(&["metadata", "timelineControl"])
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(EmoteTimeline::parse)
        .map(|timeline| (timeline.label.clone(), timeline))
        .collect();

    let mut variables = BTreeMap::new();
    for variable in root
        .at_path(&["metadata", "variableList"])
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(|value| EmoteVariable::parse(value, false))
    {
        variables.insert(variable.label.clone(), variable);
    }
    for variable in root
        .at_path(&["metadata", "instantVariableList"])
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(|value| EmoteVariable::parse(value, true))
    {
        variables
            .entry(variable.label.clone())
            .and_modify(|existing| existing.instant = true)
            .or_insert(variable);
    }
    let selectors = root
        .at_path(&["metadata", "selectorControl"])
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(EmoteSelectorControl::parse)
        .collect();
    let eye_controls = root
        .at_path(&["metadata", "eyeControl"])
        .and_then(PsbValue::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(EmoteEyeControl::parse)
        .collect();
    (timelines, variables, selectors, eye_controls)
}
