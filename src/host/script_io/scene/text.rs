use super::*;
use crate::text::{GlyphTextRenderer, ScetweenConfig, TextRenderer, message_layer_scene_id};

pub(super) struct SceneText {
    pub renderer: GlyphTextRenderer,
    pub atlases: HashMap<String, Arc<FrameTexture>>,
    pub atlas_revision: u64,
}

impl Default for SceneText {
    fn default() -> Self {
        Self {
            renderer: GlyphTextRenderer::with_atlas_limit(16),
            atlases: HashMap::new(),
            atlas_revision: 0,
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
            .set_font(bytes)
            .map_err(|_| ProtocolError::Argument("invalid fallback font"))
    }

    pub(in crate::host::script_io) fn dispatch_text_event(
        &mut self,
    ) -> Result<bool, ProtocolError> {
        let (Some(scene), Some(event)) = (&self.scene, &self.scenario_event) else {
            return Ok(false);
        };
        if !scene.scene.lock().unwrap().apply_text(event) {
            return Ok(false);
        }
        self.resume_scenario()?;
        Ok(true)
    }
}

impl SceneIo {
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
    fn apply_text(&mut self, event: &Event) -> bool {
        let renderer = &mut self.text.renderer;
        let old_layer = renderer.font_state().active_layer.clone();
        match event {
            Event::ScenarioText { content, inline } => renderer.push_text(content, *inline),
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
        self.dirty = true;
        true
    }
}
