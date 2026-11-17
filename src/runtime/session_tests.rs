use super::CoreRuntime;
use crate::backend::*;
use crate::compositor::mock::MockProvider;
use crate::host::{HostSession, ResourceProvider};
use crate::host_events::{EVENT_KIND_LOG, EVENT_KIND_MEDIA, EVENT_KIND_UI};
use crate::host_files::HostResources;
use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use asb_interpreter::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

struct Assets(HashMap<String, Vec<u8>>);

impl ResourceProvider for Assets {
    fn query_size(&self, key: &str) -> Result<Option<u64>, String> {
        Ok(self.0.get(key).map(|bytes| bytes.len() as u64))
    }

    fn read_range(&self, key: &str, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        let bytes = self.0.get(key).ok_or("absent")?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let count = out.len().min(bytes.len() - start);
        out[..count].copy_from_slice(&bytes[start..start + count]);
        Ok(count)
    }
}

// The session tests execute logic only. Unexpected GPU work fails immediately.
#[derive(Default)]
struct LogicRenderer(MockProvider, Option<RenderTrace>);

#[derive(Default)]
struct RenderTrace {
    uploads: Vec<(TextureId, Vec<u8>)>,
    retired: Vec<TextureId>,
    draws: Vec<DrawList>,
    fail_upload_at: Option<usize>,
}

impl TextureProvider for LogicRenderer {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        self.0.resolve(name)
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.0.upload_rgba(name, w, h, rgba)
    }
}

impl GpuBackend for LogicRenderer {
    fn backend_info(&self) -> BackendInfo {
        BackendInfo {
            name: "logic-test",
            kind: BackendKind::GlReference,
            stability: BackendStability::Legacy,
            capabilities: BackendCapabilities::default(),
        }
    }
    fn begin_access(&mut self) {
        art3m1s_log::global().unwrap().log(art3m1s_log::Record::new(
            art3m1s_log::Level::Info,
            "session-test",
            "renderer-access",
        ));
    }
    fn end_access(&mut self) {}
    fn create_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
    ) -> Result<TextureId, String> {
        let TextureData::Rgba8(bytes) = data else {
            panic!("unexpected texture format")
        };
        if let Some(trace) = self.1.as_mut()
            && trace.fail_upload_at == Some(trace.uploads.len())
        {
            trace.fail_upload_at = None;
            return Err("test upload failure".into());
        }
        let texture = self
            .upload_rgba(name, desc.extent.width, desc.extent.height, bytes)
            .unwrap()
            .0;
        if let Some(trace) = self.1.as_mut() {
            trace.uploads.push((texture, bytes.to_vec()));
        }
        Ok(texture)
    }
    fn update_texture(&mut self, _: TextureId, _: TextureUpdate<'_>) -> Result<(), String> {
        panic!("unexpected GPU update")
    }
    fn destroy_texture(&mut self, texture: TextureId) {
        if let Some(trace) = self.1.as_mut() {
            trace.retired.push(texture);
        }
    }
    fn create_render_target(
        &mut self,
        _: &str,
        _: RenderTargetDesc,
    ) -> Result<RenderTarget, String> {
        panic!("unexpected target")
    }
    fn destroy_render_target(&mut self, _: RenderTargetId) {}
    fn resize(&mut self, _: Extent2D) -> Result<(), String> {
        Ok(())
    }
    fn begin_frame(&mut self, _: FrameTarget) -> Result<(), String> {
        panic!("unexpected frame")
    }
    fn clear(&mut self, _: [f32; 4]) {
        panic!("unexpected clear")
    }
    fn end_frame(&mut self) {
        panic!("unexpected frame")
    }
    fn capture_frame(&mut self, _: &str, _: Extent2D) -> FrameCapture {
        panic!("unexpected capture")
    }
    fn readback(&mut self, _: FrameTarget, _: Extent2D, _: &mut [u8]) -> Result<usize, String> {
        panic!("unexpected readback")
    }
    fn render(&mut self, draw: &DrawList) -> RenderRegion {
        self.1
            .as_mut()
            .expect("unexpected render")
            .draws
            .push(draw.clone());
        RenderRegion::Full
    }
    fn render_damage(&mut self, _: &DrawList, _: [f32; 4]) -> RenderRegion {
        panic!("unexpected render")
    }
    fn render_damage_visualized(&mut self, _: &DrawList, _: [f32; 4]) -> RenderRegion {
        panic!("unexpected render")
    }
    fn render_visualized(&mut self, _: &DrawList) -> RenderRegion {
        panic!("unexpected render")
    }
    fn clear_damage_overlay(&mut self, _: &DrawList) -> Option<RenderRegion> {
        None
    }
    fn replace_asset_source(&mut self, _: Box<AssetSource>) {}
    fn cached_texture_info(&self, _: &str) -> Option<TextureInfo> {
        None
    }
    fn texture_content_revision(&self) -> u64 {
        0
    }
    fn changed_texture_ids_since(&self, _: u64) -> HashSet<TextureId> {
        HashSet::new()
    }
    fn evict_texture_prefix(&mut self, _: &str) -> usize {
        0
    }
    fn upload_video_rgba(&mut self, _: &str, _: u32, _: u32, _: &[u8]) -> bool {
        panic!("unexpected video")
    }
    fn set_native_surface(&mut self, _: NativeSurface) -> Result<(), String> {
        panic!("unexpected surface")
    }
    fn clear_native_surface(&mut self) {}
    fn present(&mut self, _: Option<[f32; 4]>) -> Result<(), String> {
        panic!("unexpected present")
    }
    fn register_hlsl_shader(&mut self, _: &str, _: &[u8]) -> Result<ShaderId, ShaderCompileError> {
        panic!("unexpected shader")
    }
    fn unregister_shader(&mut self, _: &str) -> bool {
        false
    }
    fn set_profile_enabled(&self, _: bool) {}
    fn take_profile_stats(&mut self) -> GpuProfileStats {
        GpuProfileStats::default()
    }
}

#[test]
fn host_sprite_renderer_reuses_uploads_and_rolls_back_failed_frames() {
    use crate::host::frame::{
        FramePublisher, FrameSnapshot, FrameSprite, FrameTexture, SpriteBlend,
    };
    use crate::host::renderer::SpriteRenderer;
    use crate::host::runtime::RuntimeHandle;
    use crate::render_pipeline::draw::BlendMode;

    let texture = |revision, rgba: [u8; 4]| {
        Arc::new(FrameTexture {
            revision,
            width: 1,
            height: 1,
            rgba: Box::new(rgba),
        })
    };
    let sprite = |layer_id: &str, texture, mask| FrameSprite {
        layer_id: layer_id.to_owned(),
        texture,
        mask,
        transform: [1.0, 0.0, 0.0, 1.0, 8.0, 15.0],
        quad_size: [1.0, 1.0],
        uv_offset: [0.0, 0.0],
        uv_scale: [1.0, 1.0],
        clip_bounds: Some([5.0, 5.0, 10.0, 10.0]),
        opacity: 0.75,
        color_multiply: [1.0, 0.9, 0.8],
        grayscale: false,
        negative: false,
        blend: SpriteBlend::Add,
        mesh: None,
        emote_material: None,
        shader: None,
    };
    let snapshot =
        |epoch, sprites: Vec<FrameSprite>, textures: Vec<Arc<FrameTexture>>| FrameSnapshot {
            session_epoch: epoch,
            render_frame_id: 0,
            host_frame_id: 1,
            stage_size: [64, 48],
            sprites: sprites.into_boxed_slice(),
            groups: Box::new([]),
            masks: Box::new([]),
            transition: None,
            textures: textures.into_boxed_slice(),
        };
    let original = texture(1, [40, 25, 27, 192]);
    let mask = texture(2, [96, 96, 96, 255]);
    let mut frames = FramePublisher::default();
    let runtime = RuntimeHandle::from_raw(9);
    frames
        .publish(snapshot(
            1,
            vec![sprite("1.80", 0, Some(1))],
            vec![original.clone(), mask],
        ))
        .unwrap();
    let first = frames.acquire(runtime, 0).unwrap().unwrap();

    let mut renderer = SpriteRenderer::new(LogicRenderer(
        MockProvider::default(),
        Some(RenderTrace::default()),
    ));
    renderer.render(&first).unwrap();
    renderer.render(&first).unwrap();
    let trace = renderer.backend_mut().1.as_mut().unwrap();
    assert_eq!(trace.uploads.len(), 1);
    let (first_texture, pixels) = &trace.uploads[0];
    let first_texture = *first_texture;
    assert_eq!(pixels.as_slice(), [40, 25, 27, 72]);
    let draw = trace.draws.last().unwrap();
    assert_eq!(draw.command_key(0).unwrap().layer_id, "1.80");
    assert_eq!(draw.commands[0].texture, first_texture);
    assert_eq!(
        draw.commands[0].transform.translation.to_array(),
        [8.0, 15.0]
    );
    assert_eq!(draw.commands[0].blend, BlendMode::Add);
    assert_eq!(draw.commands[0].clip_bounds, Some([5.0, 5.0, 10.0, 10.0]));
    trace.fail_upload_at = Some(2);

    frames
        .publish(snapshot(
            2,
            vec![sprite("0", 0, None), sprite("1.80", 1, None)],
            vec![
                texture(1, [99, 99, 19, 255]),
                texture(2, [255, 255, 255, 255]),
            ],
        ))
        .unwrap();
    let second = frames
        .acquire(runtime, first.render_frame_id)
        .unwrap()
        .unwrap();
    assert_eq!(renderer.render(&second).unwrap_err(), "test upload failure");
    let trace = renderer.backend_mut().1.as_ref().unwrap();
    assert_eq!(trace.retired, [trace.uploads[1].0]);
    assert_eq!(trace.draws.len(), 2);

    renderer.render(&first).unwrap();
    assert_eq!(renderer.backend_mut().1.as_ref().unwrap().uploads.len(), 2);
    renderer.render(&second).unwrap();
    let trace = renderer.backend_mut().1.as_ref().unwrap();
    assert_eq!(trace.uploads.len(), 4);
    assert_eq!(trace.uploads[2].1.as_slice(), [99, 99, 19, 255]);
    assert!(trace.retired.contains(&first_texture));
    let draw = trace.draws.last().unwrap();
    assert_eq!(draw.command_key(0).unwrap().layer_id, "0");
    assert_eq!(draw.command_key(1).unwrap().layer_id, "1.80");
    renderer.clear_cached_textures();
    assert_eq!(renderer.backend_mut().1.as_ref().unwrap().retired.len(), 4);
}

#[test]
fn host_sprite_renderer_preserves_emote_mesh_material_and_stencil() {
    use crate::host::frame::{
        FrameGroup, FramePublisher, FrameSnapshot, FrameSprite, FrameTexture, SpriteBlend,
    };
    use crate::host::renderer::SpriteRenderer;
    use crate::host::runtime::RuntimeHandle;
    use crate::render_pipeline::draw::{BlendMode, NativeEmoteMaterial};

    let vertices: Arc<[[f32; 4]]> = Arc::new([
        [0.0, 0.0, 0.0, 0.0],
        [4.0, 0.0, 1.0, 0.0],
        [4.0, 5.0, 1.0, 1.0],
    ]);
    let material = NativeEmoteMaterial {
        corner_colors: [[0.25, 0.5, 0.75, 0.8]; 4],
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        blend_mode: 0x11,
        clip_rect: [0.0, 0.0, 4.0, 5.0],
        wipe: [1.0, 0.0, 1.0],
    };
    let sprite = FrameSprite {
        layer_id: "model".into(),
        texture: 0,
        mask: None,
        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        quad_size: [4.0, 5.0],
        uv_offset: [0.0, 0.0],
        uv_scale: [1.0, 1.0],
        clip_bounds: None,
        opacity: 1.0,
        color_multiply: [1.0; 3],
        grayscale: false,
        negative: false,
        blend: SpriteBlend::EmoteAdd,
        mesh: Some(vertices.clone()),
        emote_material: Some(material.clone()),
        shader: None,
    };
    let mask = FrameSprite {
        layer_id: String::new(),
        blend: SpriteBlend::Alpha,
        ..sprite.clone()
    };
    let mut publisher = FramePublisher::default();
    publisher
        .publish(FrameSnapshot {
            session_epoch: 1,
            render_frame_id: 0,
            host_frame_id: 1,
            stage_size: [4, 5],
            sprites: Box::new([sprite]),
            masks: Box::new([mask]),
            groups: Box::new([FrameGroup {
                sprites: 0..1,
                stencil: Some(0..1),
                shader: None,
                mask: None,
                clip_bounds: None,
                opacity: 1.0,
                color_multiply: [1.0; 3],
                grayscale: false,
                negative: false,
                opaque: false,
                blend: SpriteBlend::Alpha,
            }]),
            textures: Box::new([Arc::new(FrameTexture {
                revision: 1,
                width: 1,
                height: 1,
                rgba: Box::new([255; 4]),
            })]),
            transition: None,
        })
        .unwrap();
    let frame = publisher
        .acquire(RuntimeHandle::from_raw(9), 0)
        .unwrap()
        .unwrap();
    let packet = frame.packet();
    let u16_at = |i| u16::from_le_bytes(packet[i..i + 2].try_into().unwrap());
    let u32_at = |i| u32::from_le_bytes(packet[i..i + 4].try_into().unwrap());
    let draw = 56 + 24;
    assert_eq!(u16_at(draw + 4), 4);
    assert_eq!(u32_at(draw + 104 + 5), 3);
    assert_eq!(u32_at(draw + 104 + 5 + 4 + 80), 0x11);
    let mask = draw + u32_at(draw) as usize;
    assert_eq!(u16_at(mask + 6), 0x18);
    let group = mask + u32_at(mask) as usize;
    assert_eq!(u16_at(group + 4), 5);
    assert_eq!(u32_at(group + 20), 1);

    let mut renderer = SpriteRenderer::new(LogicRenderer(
        MockProvider::default(),
        Some(RenderTrace::default()),
    ));
    renderer.render(&frame).unwrap();
    let trace = renderer.backend_mut().1.as_ref().unwrap();
    let draw = &trace.draws[0];
    assert_eq!(draw.commands[0].blend, BlendMode::NativeAdd);
    assert_eq!(draw.commands[0].mesh.as_ref().unwrap().vertices, vertices);
    assert_eq!(draw.mask_commands[0].native_emote.as_ref(), Some(&material));
    assert_eq!(draw.shader_groups[0].mask_range, Some([0, 1]));
}

const INI: &str = "[WINDOWS]\nWIDTH=64\nHEIGHT=64\nBOOT=boot.ast\nCHARSET=UTF-8\n";

fn runtime(name: &str, has_asset: bool) -> CoreRuntime {
    let boot = format!(
        r#"[lua]
local e = __engine
e:tag{{"var", system="file_exists", name="t.loaded", file="probe.dat"}}
function probe(e)
    e:tag{{"var", system="file_exists", name="t.exists", file="probe.dat"}}
    e:tag{{"var", system="get_sound_info", name="t.sound"}}
    e:tag{{"var", system="get_backlog_size", name="t.backlog"}}
    e:tag{{"var", system="get_message_layer_width", name="t.width"}}
    e:tag{{"var", system="fullscreen", name="t.fullscreen"}}
    e:tag{{"var", system="get_font", name="t.fonts"}}
end
[/lua]
*top
[caption data="{name}"]
[seplay id="{name}.1.80" file="tone.ogg" loop="1"]
[stop]
"#
    );
    let mut files = HashMap::from([("boot.ast".into(), boot.into_bytes())]);
    if has_asset {
        files.insert("probe.dat".into(), vec![1]);
    }
    let resources = HostResources::from_provider(Arc::new(Assets(files)));
    CoreRuntime::with_host_backend(64, 64, resources, Box::<LogicRenderer>::default()).unwrap()
}

fn query(runtime: &mut CoreRuntime) {
    runtime
        .interpreter
        .lua()
        .load("probe(__engine)")
        .exec()
        .unwrap();
}

fn drain(host: &HostSession) -> Vec<(u32, String)> {
    let mut bytes = vec![0; host.events().queued_bytes()];
    let (written, count) = host.events().poll_events(&mut bytes);
    let mut offset = 0;
    let mut records = Vec::new();
    while offset < written {
        let kind = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let length =
            u32::from_le_bytes(bytes[offset + 16..offset + 20].try_into().unwrap()) as usize;
        records.push((
            kind,
            String::from_utf8(bytes[offset + 24..offset + 24 + length].to_vec()).unwrap(),
        ));
        offset += 24 + length;
    }
    assert_eq!(records.len(), count as usize);
    records
}

#[test]
fn interleaved_runtimes_keep_queries_events_and_host_state() {
    let mut a = runtime("A", true);
    let mut b = runtime("B", false);
    let host_a = a.host_session().unwrap().clone();
    let host_b = b.host_session().unwrap().clone();
    host_a.events().set_window_state(1);
    host_b.events().set_window_state(2);
    host_a
        .events()
        .set_font_list(false, false, vec!["Font A".into()]);
    host_b
        .events()
        .set_font_list(false, false, vec!["Font B".into()]);
    a.load_project(INI, "WINDOWS").unwrap();
    b.load_project(INI, "WINDOWS").unwrap();
    assert_eq!(
        a.interpreter
            .get_variable("t.loaded")
            .and_then(|v| v.as_int()),
        Some(1)
    );
    assert_eq!(
        b.interpreter
            .get_variable("t.loaded")
            .and_then(|v| v.as_int()),
        Some(0)
    );
    a.advance_without_render(16);
    b.advance_without_render(16);
    a.query_snapshots
        .lock()
        .unwrap()
        .backlog
        .pages
        .resize(3, Default::default());
    b.query_snapshots
        .lock()
        .unwrap()
        .backlog
        .pages
        .resize(1, Default::default());
    a.query_snapshots.lock().unwrap().text_metrics = (100.0, 20.0, 80.0);
    b.query_snapshots.lock().unwrap().text_metrics = (200.0, 30.0, 90.0);
    for _ in 0..2 {
        query(&mut a);
        query(&mut b);
        assert_eq!(
            a.interpreter
                .get_variable("t.exists")
                .and_then(|v| v.as_int()),
            Some(1)
        );
        assert_eq!(
            b.interpreter
                .get_variable("t.exists")
                .and_then(|v| v.as_int()),
            Some(0)
        );
        assert_eq!(
            a.interpreter
                .get_variable("t.backlog")
                .and_then(|v| v.as_int()),
            Some(3)
        );
        assert_eq!(
            b.interpreter
                .get_variable("t.backlog")
                .and_then(|v| v.as_int()),
            Some(1)
        );
        assert_eq!(
            a.interpreter
                .get_variable("t.width")
                .and_then(|v| v.as_float()),
            Some(100.0)
        );
        assert_eq!(
            b.interpreter
                .get_variable("t.width")
                .and_then(|v| v.as_float()),
            Some(200.0)
        );
        assert_eq!(
            a.interpreter.get_variable("t.sound.0.id"),
            Some(Value::String("A.1.80".into()))
        );
        assert_eq!(
            b.interpreter.get_variable("t.sound.0.id"),
            Some(Value::String("B.1.80".into()))
        );
        assert_eq!(
            a.interpreter
                .get_variable("t.fullscreen")
                .and_then(|v| v.as_int()),
            Some(1)
        );
        assert_eq!(
            b.interpreter
                .get_variable("t.fullscreen")
                .and_then(|v| v.as_int()),
            Some(0)
        );
        assert_eq!(
            a.interpreter.get_variable("t.fonts.0"),
            Some(Value::String("Font A".into()))
        );
        assert_eq!(
            b.interpreter.get_variable("t.fonts.0"),
            Some(Value::String("Font B".into()))
        );
    }
    let check_output = |host: &HostSession, owner: &str| {
        let records = drain(host);
        assert!(
            records
                .iter()
                .any(|(kind, text)| *kind == EVENT_KIND_LOG && text == "renderer-access")
        );
        let commands: Vec<serde_json::Value> = records
            .iter()
            .filter(|(kind, _)| matches!(*kind, EVENT_KIND_MEDIA | EVENT_KIND_UI))
            .map(|(_, text)| serde_json::from_str(text).unwrap())
            .collect();
        let captions: Vec<_> = commands
            .iter()
            .filter(|v| v["kind"] == "caption")
            .map(|v| v["payload"]["data"].as_str().unwrap())
            .collect();
        assert_eq!(captions, [owner]);
    };
    check_output(&host_a, "A");
    check_output(&host_b, "B");

    let saved: asb_interpreter::VariableStore =
        serde_json::from_str(&serde_json::to_string(&a.interpreter.variables()).unwrap()).unwrap();
    b.interpreter.restore_variables(saved);
    query(&mut b);
    assert_eq!(
        b.interpreter
            .get_variable("t.exists")
            .and_then(|v| v.as_int()),
        Some(0)
    );
    assert_eq!(
        b.interpreter
            .get_variable("t.backlog")
            .and_then(|v| v.as_int()),
        Some(1)
    );
    b.interpreter.restore_variables(a.interpreter.variables());
    query(&mut b);
    assert_eq!(
        b.interpreter
            .get_variable("t.exists")
            .and_then(|v| v.as_int()),
        Some(0)
    );
    a.load_project(INI, "WINDOWS").unwrap();
    query(&mut a);
    assert_eq!(
        a.interpreter
            .get_variable("t.backlog")
            .and_then(|v| v.as_int()),
        Some(0)
    );
    a.set_profiler_enabled(true);
    b.set_profiler_enabled(true);
    host_b.record_profile_io(5, 9);
    drop(a);
    assert!(host_b.profile_io_enabled());
    assert_eq!(host_b.take_profile_io_counters().bytes, 9);
    query(&mut b);
    assert_eq!(
        b.interpreter
            .get_variable("t.backlog")
            .and_then(|v| v.as_int()),
        Some(1)
    );
    assert_eq!(
        b.interpreter.get_variable("t.sound.0.id"),
        Some(Value::String("B.1.80".into()))
    );
    assert!(drain(&host_b).is_empty());
}
