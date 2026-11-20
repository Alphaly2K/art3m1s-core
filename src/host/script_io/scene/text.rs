use super::*;
use crate::save::MessageTextSnapshot;
use crate::text::{
    BacklogTag, FontDesc, GlyphTextRenderer, ScetweenConfig, TextRenderer, TextSpanToken,
    capture_message_text, font_fallback_candidates, message_layer_scene_id,
};
use std::collections::HashSet;

const DEFAULT_FACE: &str = ":host/default-font";
const MAX_FONT_FACES: usize = 128;

pub(in crate::host::script_io) struct FontRead {
    face: String,
    remaining: VecDeque<String>,
}

pub(super) struct SceneText {
    pub renderer: GlyphTextRenderer,
    pub atlases: HashMap<String, Arc<FrameTexture>>,
    pub atlas_revision: u64,
    known_faces: HashSet<String>,
    font_bytes: usize,
}

impl Default for SceneText {
    fn default() -> Self {
        Self {
            renderer: GlyphTextRenderer::with_atlas_limit(16),
            atlases: HashMap::new(),
            atlas_revision: 0,
            known_faces: HashSet::new(),
            font_bytes: 0,
        }
    }
}

impl SceneText {
    pub fn advance(&mut self, delta_ms: u64) -> bool {
        let active = !self.renderer.is_reveal_complete();
        if active {
            self.renderer.advance_reveal(delta_ms);
        }
        active
    }
}

impl ScriptIoSession {
    pub(in crate::host) fn set_default_font(
        &mut self,
        epoch: u64,
        bytes: &[u8],
    ) -> Result<(), ProtocolError> {
        if epoch != self.transport.state().epoch {
            return Err(ProtocolError::Stale);
        }
        if self.faulted || self.frame.is_some() {
            return Err(ProtocolError::State);
        }
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(ProtocolError::Limit);
        }
        let scene = self.scene.as_ref().ok_or(ProtocolError::State)?;
        scene
            .scene
            .lock()
            .unwrap()
            .text
            .renderer
            .set_named_font_bytes(DEFAULT_FACE, bytes.to_vec())
            .map_err(|_| ProtocolError::Argument("invalid fallback font"))
    }

    pub(in crate::host::script_io) fn dispatch_text_event(
        &mut self,
    ) -> Result<bool, ProtocolError> {
        let (Some(scene), Some(event)) = (&self.scene, &self.scenario_event) else {
            return Ok(false);
        };
        {
            let mut scene = scene.scene.lock().unwrap();
            let identity = match event {
                Event::ScenarioText { content, .. } if self.translation_enabled => {
                    let state = scene.text.renderer.font_state();
                    let active = state
                        .active_layer
                        .as_deref()
                        .unwrap_or(crate::text::glyph::DEFAULT_MESSAGE_LAYER);
                    let ruby = state
                        .layers
                        .get(active)
                        .and_then(|layer| layer.open_ruby.as_ref());
                    self.translations.request(
                        &mut self.transport,
                        content,
                        ruby.map(|(_, text)| text.as_str()),
                    )?
                }
                _ => None,
            };
            let mut span = None;
            if !scene.apply_text_tracking(event, identity.is_some().then_some(&mut span)) {
                return Ok(false);
            }
            if let Some(identity) = identity {
                if let Some(span) = span {
                    self.translations.track(identity, span);
                } else {
                    self.transport.cancel(identity)?;
                }
            }
        }
        self.resume_scenario()?;
        Ok(true)
    }
}

impl SceneIo {
    pub(in crate::host::script_io) fn translation_span_ready(
        &self,
        span: &TextSpanToken,
    ) -> Option<bool> {
        let scene = self.scene.lock().unwrap();
        let layer = scene
            .text
            .renderer
            .font_state()
            .layers
            .get(&span.layer_id)?;
        (layer.generation == span.generation && layer.font.face == span.font_face)
            .then_some(!layer.reveal_pending)
    }

    pub(in crate::host::script_io) fn replace_translation(
        &self,
        span: &TextSpanToken,
        text: &str,
    ) -> Option<isize> {
        let mut scene = self.scene.lock().unwrap();
        let delta = scene.text.renderer.replace_text_span(span, text)?;
        scene.dirty = true;
        Some(delta)
    }

    pub(in crate::host::script_io) fn hide_message_layers(
        &self,
        windows: &[String],
    ) -> Vec<(String, Option<bool>)> {
        let mut scene = self.scene.lock().unwrap();
        let mut targets = windows.to_vec();
        let text = scene.text.renderer.font_state();
        if let Some(id) = text.active_layer.as_ref()
            && let Some(layer) = text.layers.get(id)
        {
            targets.push(message_layer_scene_id(id, layer.layered));
        }
        targets.sort_unstable();
        targets.dedup();
        let saved: Vec<_> = targets
            .into_iter()
            .filter_map(|id| {
                let visible = scene.compositor.scene().get(&id)?.props.visible;
                Some((id, visible))
            })
            .collect();
        for (id, _) in &saved {
            scene.set_control_layer_visible(id, false);
        }
        saved
    }

    pub(in crate::host::script_io) fn restore_hidden_layers(
        &self,
        layers: Vec<(String, Option<bool>)>,
    ) {
        let mut scene = self.scene.lock().unwrap();
        for (id, visible) in layers {
            if let Some(layer) = scene.compositor.scene.get_mut(&id) {
                layer.props.visible = visible;
                scene.dirty = true;
            }
        }
    }

    pub(in crate::host::script_io) fn capture_text(&self) -> Option<MessageTextSnapshot> {
        capture_message_text(self.scene.lock().unwrap().text.renderer.font_state())
    }

    pub(in crate::host::script_io) fn prepare_saved_font(
        &self,
        snapshot: &MessageTextSnapshot,
    ) -> Option<(FontRead, IoRequest)> {
        snapshot
            .layers
            .iter()
            .flat_map(|layer| {
                std::iter::once(&layer.page_font).chain(layer.tags.iter().filter_map(
                    |tag| match tag {
                        BacklogTag::Font(settings) => Some(settings),
                        _ => None,
                    },
                ))
            })
            .filter_map(|settings| settings.get("face"))
            .find_map(|face| self.prepare_font(face))
    }

    pub(in crate::host::script_io) fn restore_text(&self, snapshot: Option<MessageTextSnapshot>) {
        let mut scene = self.scene.lock().unwrap();
        scene.text.renderer.clear_scene_text();
        scene.text.renderer.font_state_mut().active_layer = None;
        if let Some(snapshot) = snapshot {
            for page in snapshot.layers {
                let overlay = message_layer_scene_id(&page.id, false);
                let layered = scene.compositor.scene().get(&overlay).is_none()
                    && scene.compositor.scene().get(&page.id).is_some();
                scene.apply_text(&Event::MessageLayerSwitch {
                    id: Some(page.id),
                    stack: false,
                    layered: Some(i32::from(layered)),
                });
                let layer = scene.text.renderer.font_state_mut().active_layer_mut();
                layer.font = FontDesc::from_raw(&page.page_font);
                layer.page_font = page.page_font;
                layer.text_hidden = false;
                scene.select_text_face();
                for tag in page.tags {
                    match tag {
                        BacklogTag::Text(content) => scene.text.renderer.push_text(&content, false),
                        BacklogTag::LineBreak => scene.text.renderer.push_line_break(),
                        BacklogTag::Font(settings) => {
                            scene.text.renderer.apply_font_settings(&settings);
                            scene.select_text_face();
                        }
                        BacklogTag::RubyStart(text) => scene.text.renderer.ruby_start(&text),
                        BacklogTag::RubyEnd => scene.text.renderer.ruby_end(),
                    }
                }
            }
            scene.text.renderer.font_state_mut().active_layer = snapshot.active_layer;
            scene.text.renderer.reveal_all();
        }
        let active = scene.text.renderer.font_state().active_layer.clone();
        scene.compositor.set_default_message_layer(active);
        scene.select_text_face();
        scene.dirty = true;
    }

    pub(in crate::host::script_io) fn font_pending(&self) -> bool {
        self.pending_font.is_some()
    }

    pub(in crate::host::script_io) fn prepare_event_font(
        &mut self,
        event: &Event,
    ) -> Option<IoRequest> {
        let (Event::FontSettings(settings) | Event::FontDefault(settings)) = event else {
            return None;
        };
        let (font, request) = self.prepare_font(settings.get("face")?)?;
        self.pending_font = Some((event.clone(), font));
        Some(request)
    }

    fn prepare_font(&self, face: &str) -> Option<(FontRead, IoRequest)> {
        let key = self.resolve_asset_key(face).ok()?;
        let scene = self.scene.lock().unwrap();
        if scene.text.font_bytes >= MAX_TEXTURE_BYTES
            || scene.text.known_faces.len() >= MAX_FONT_FACES
            || scene.text.known_faces.contains(&key)
        {
            return None;
        }
        Some((
            FontRead {
                face: key.clone(),
                remaining: font_fallback_candidates(&key).into(),
            },
            IoRequest::Read { path: key },
        ))
    }

    pub(in crate::host::script_io) fn complete_event_font(
        &mut self,
        response: IoResponse,
    ) -> Option<IoRequest> {
        let (event, mut font) = self.pending_font.take().unwrap();
        if let Some(request) = self.complete_font(&mut font, response) {
            self.pending_font = Some((event, font));
            return Some(request);
        }
        self.scene.lock().unwrap().apply_text(&event);
        None
    }

    pub(in crate::host::script_io) fn complete_font(
        &self,
        font: &mut FontRead,
        response: IoResponse,
    ) -> Option<IoRequest> {
        let mut scene = self.scene.lock().unwrap();
        if matches!(
            &response,
            IoResponse::Failed {
                kind: IoFailure::Cancelled,
                ..
            }
        ) {
            return None;
        }
        if let IoResponse::Read(bytes) = response {
            let len = bytes.len();
            if len <= MAX_TEXTURE_BYTES - scene.text.font_bytes
                && scene.text.renderer.cache_font(&font.face, bytes).is_ok()
            {
                scene.text.font_bytes += len;
                scene.text.known_faces.insert(font.face.clone());
                return None;
            }
        }
        if let Some(path) = font.remaining.pop_front() {
            return Some(IoRequest::Read { path });
        }
        scene.text.known_faces.insert(font.face.clone());
        None
    }

    pub(in crate::host::script_io) fn text_revealed(&self) -> bool {
        self.scene
            .lock()
            .unwrap()
            .text
            .renderer
            .is_reveal_complete()
    }

    pub(in crate::host::script_io) fn reveal_text(&self) {
        let mut scene = self.scene.lock().unwrap();
        scene.text.renderer.reveal_all();
        scene.dirty = true;
    }
}

impl SceneProjection {
    pub(super) fn link_under_pointer(
        &mut self,
        position: Option<(i32, i32)>,
    ) -> Option<crate::text::LinkHitArea> {
        let renderer = &self.text.renderer;
        let mut points = HashMap::new();
        let mut hits = HashMap::new();
        if let Some((x, y)) = position {
            let point = glam::Vec2::new(x as f32, y as f32);
            for area in renderer.link_hit_areas() {
                let local = points.entry(area.layer_id.clone()).or_insert_with(|| {
                    let layer = renderer.font_state().layers.get(&area.layer_id)?;
                    if layer.text_hidden
                        || !self.compositor.is_message_layer_visible(&area.layer_id)
                    {
                        return None;
                    }
                    let node = message_layer_scene_id(&area.layer_id, layer.layered);
                    text_local_point(&self.compositor, &node, point)
                });
                if local.is_some_and(|p| area.contains(p.x, p.y)) {
                    hits.entry((area.layer_id.clone(), area.link_index))
                        .or_insert(area);
                }
            }
        }
        let active = renderer.font_state().active_layer.as_deref();
        let target = hits
            .values()
            .filter(|area| area.file.is_some() || area.label.is_some())
            .min_by_key(|area| {
                (
                    Some(area.layer_id.as_str()) != active,
                    &area.layer_id,
                    area.link_index,
                )
            })
            .cloned();
        for (id, layer) in &mut self.text.renderer.font_state_mut().layers {
            for (index, link) in layer.links.iter_mut().enumerate() {
                let hovered = hits.contains_key(&(id.clone(), index));
                self.dirty |= link.hovered != hovered;
                link.hovered = hovered;
            }
        }
        target
    }

    fn select_text_face(&mut self) {
        let key = self.text.renderer.active_font_face().and_then(|face| {
            logical_key(&crate::magic_path::resolve_path(&self.magic_paths, face)).ok()
        });
        if !key.is_some_and(|key| self.text.renderer.select_cached_font(&key)) {
            self.text.renderer.select_cached_font(DEFAULT_FACE);
        }
    }

    fn apply_text(&mut self, event: &Event) -> bool {
        self.apply_text_tracking(event, None)
    }

    fn apply_text_tracking(
        &mut self,
        event: &Event,
        span: Option<&mut Option<TextSpanToken>>,
    ) -> bool {
        let renderer = &mut self.text.renderer;
        let old_layer = renderer.font_state().active_layer.clone();
        match event {
            Event::ScenarioText { content, inline } => {
                if let Some(span) = span {
                    *span = renderer.push_text_tracked(content, *inline);
                } else {
                    renderer.push_text(content, *inline);
                }
            }
            Event::FontSettings(settings) => renderer.apply_font_settings(settings),
            Event::FontDefault(settings) => renderer.font_default(settings),
            Event::FontInit => renderer.font_init(),
            Event::FontClose => renderer.font_pop(),
            Event::MessageLayerSwitch { id, stack, layered } => {
                renderer.switch_message_layer(id.as_deref(), *stack);
                renderer.font_state_mut().active_layer_mut().layered = *layered == Some(1);
            }
            Event::MessageLayerPop => renderer.pop_message_layer(),
            Event::LineBreak { omitblankline } => {
                renderer
                    .font_state_mut()
                    .set_rt_omit_blank_line(*omitblankline);
                renderer.push_line_break();
            }
            Event::PageBreak { backlog } => renderer.push_page_break(*backlog),
            Event::TextAnimation(params) => {
                renderer.set_scetween(ScetweenConfig::from_params(params))
            }
            Event::SceneIn => renderer.show_text(),
            Event::SceneOut => renderer.hide_text(),
            Event::RubyStart { text } => renderer.ruby_start(text),
            Event::RubyEnd => renderer.ruby_end(),
            Event::GlyphConfig(settings) => renderer.set_glyph_config(settings),
            Event::LinkStart {
                file,
                label,
                link_type,
                color,
                shadowcolor,
                outlinecolor,
            } => renderer.link_start(
                file.as_deref(),
                label.as_deref(),
                *link_type,
                color.as_deref(),
                shadowcolor.as_deref(),
                outlinecolor.as_deref(),
            ),
            Event::LinkEnd => renderer.link_end(),
            Event::LinkEnable => renderer.set_links_enabled(true),
            Event::LinkDisable => renderer.set_links_enabled(false),
            Event::WriteBacklogConfig { mode } => {
                renderer.font_state_mut().backlog.set_write_mode(*mode)
            }
            Event::BacklogConfig {
                allow,
                messagelayer,
                includefont,
                hide,
                layer,
                clear,
            } => {
                let backlog = &mut renderer.font_state_mut().backlog;
                backlog.settings.allow = *allow;
                if let Some(id) = messagelayer {
                    backlog.settings.message_layer = id.clone();
                }
                if let Some(include) = includefont {
                    backlog.settings.include_font = *include;
                }
                if let Some(hide) = hide {
                    backlog.settings.hide = hide.clone();
                }
                backlog.settings.layer = layer.clone();
                if *clear {
                    backlog.clear();
                }
            }
            Event::ProhibitConfig { head, foot } => renderer
                .font_state_mut()
                .set_prohibit(Some(head), Some(foot)),
            Event::WordpartsConfig { parts } => renderer.font_state_mut().set_wordparts(parts),
            Event::IndentConfig { pair, range, nest } => {
                renderer
                    .font_state_mut()
                    .set_indent(Some(pair), *range, Some(*nest))
            }
            _ => return false,
        }
        let active = renderer.font_state().active_layer.as_ref();
        let switching = matches!(event, Event::MessageLayerSwitch { .. });
        if switching || active != old_layer.as_ref() {
            if let Some(id) = active {
                let layered = renderer
                    .font_state()
                    .layers
                    .get(id)
                    .is_some_and(|layer| layer.layered);
                let scene_id = message_layer_scene_id(id, layered);
                self.compositor.ensure_layer(&scene_id);
                self.compositor.set_message_layer_binding(id, &scene_id);
                if switching {
                    self.compositor.revive_message_layer(id);
                }
            }
            self.compositor.set_default_message_layer(active.cloned());
        }
        if matches!(
            event,
            Event::FontSettings(_)
                | Event::FontDefault(_)
                | Event::FontInit
                | Event::FontClose
                | Event::MessageLayerSwitch { .. }
                | Event::MessageLayerPop
        ) {
            self.select_text_face();
        }
        self.dirty = true;
        true
    }
}

fn text_local_point(
    compositor: &Compositor,
    mut id: &str,
    point: glam::Vec2,
) -> Option<glam::Vec2> {
    let scene = compositor.scene();
    let mut transform = glam::Affine2::IDENTITY;
    loop {
        let layer = scene.get(id)?;
        let props = crate::compositor::build::resolved_props(layer, compositor.clock_ms());
        transform = props.local_transform() * transform;
        let Some((parent, _)) = id.rsplit_once('.') else {
            break;
        };
        id = parent;
    }
    let world = scene.root_props().local_transform() * transform;
    let local = world.inverse().transform_point2(point);
    local.is_finite().then_some(local)
}
