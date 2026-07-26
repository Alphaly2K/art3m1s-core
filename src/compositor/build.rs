//! 帧构建：把场景树在某一时刻"压平"成有序的绘制列表。
//!
//! 每帧调用 [`build_frame`]：它从根到叶遍历场景树，沿途累积父图层的仿射变换与
//! 不透明度，对进行中的缓动求出当前值，剔除隐藏图层（连同其子树），最后对每个
//! 绑定了纹理的图层产出一条 [`DrawCommand`]，按遍历顺序（先根后子、同级按插入
//! 顺序）排列——也就是从底到顶的绘制次序。

use crate::compositor::props::LayerProps;
use crate::compositor::scene::Scene;
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, LayerDrawSource, ShaderEffect,
    ShaderGroup, TextureProvider,
};
use glam::{Affine2, Vec2};
use std::collections::BTreeMap;

/// 在时刻 `now_ms` 把 `scene` 构建成一帧绘制列表。
///
/// 纹理通过 `provider` 解析；解析不到的图层会被跳过（但其子图层仍会被处理，因为
/// 分组节点本就常常没有自己的纹理）。
///
/// `text_for` 是可选的文本注入回调：遍历到某层时，调用它获取该层对应的文本绘制
/// 命令，注入到子节点之后。这使文本子系统能正确继承 compositor 层的 z-order 与
/// visible 属性。
pub fn build_frame(
    scene: &Scene,
    now_ms: u64,
    provider: &mut dyn TextureProvider,
    text_for: Option<&LayerDrawSource<'_>>,
) -> DrawList {
    build_frame_with_content(scene, now_ms, provider, None, text_for)
}

/// Builds a frame with optional host-owned visual content injected before a
/// layer's children, plus text injected after its children.
pub fn build_frame_with_content(
    scene: &Scene,
    now_ms: u64,
    provider: &mut dyn TextureProvider,
    content_for: Option<&LayerDrawSource<'_>>,
    text_for: Option<&LayerDrawSource<'_>>,
) -> DrawList {
    let mut frame = DrawList::new();
    for root in scene.roots() {
        visit(
            scene,
            &root,
            now_ms,
            Affine2::IDENTITY,
            1.0,
            None,
            None,
            provider,
            &mut frame,
            content_for,
            text_for,
        );
    }
    frame
}

/// 递归访问一个节点：合成本地变换，向子节点继承，产出绘制命令。
fn visit(
    scene: &Scene,
    id: &str,
    now_ms: u64,
    parent_transform: Affine2,
    parent_opacity: f32,
    parent_clip: Option<[f32; 4]>,
    inherited_shader: Option<ShaderEffect>,
    provider: &mut dyn TextureProvider,
    frame: &mut DrawList,
    content_for: Option<&LayerDrawSource<'_>>,
    text_for: Option<&LayerDrawSource<'_>>,
) {
    let Some(layer) = scene.get(id) else {
        return;
    };

    // 把进行中的缓动应用到属性副本上（不改动场景里的原始属性）。
    let props = resolved_props(layer, now_ms);

    // 隐藏的图层连同整棵子树一起跳过。
    if !props.is_visible() {
        return;
    }

    let local = local_transform(&props);
    let world = parent_transform * local;
    let opacity = parent_opacity * props.opacity();
    let clip_bounds = subtree_clip_bounds(&props, world, parent_clip, provider);
    let children = scene.children(id);
    let local_shader = declared_shader(scene, &props, provider);
    let group_shader = local_shader
        .as_ref()
        .and_then(|shader| shader.clone())
        .filter(|_| props.intermediate_render.is_some() || !children.is_empty());
    let command_shader = if group_shader.is_some() {
        inherited_shader.clone()
    } else {
        local_shader.unwrap_or(inherited_shader.clone())
    };
    let group_start = frame.len();

    // 只有绑定了非空文件名且能解析到资源的节点才产出绘制命令；纯分组节点只传
    // 递变换。空文件名（如 config 的纯色图层 `lyc2{color=...}`，无 file，Create
    // 事件 file=""）不是纹理引用——跳过，否则 provider.resolve("") 会回退到品红
    // 占位纹理，在屏幕左上角显示紫黑块。注：合成器暂无纯色矩形渲染路径，故这类
    // 纯色图层（透明锚点 / 白条 / 黑底等）目前不绘制。
    if let Some(file) = &layer.file
        && !file.is_empty()
        && let Some((texture, info)) = provider.resolve(file)
    {
        // 计算裁剪矩形
        let clip = if let Some(clip_rect) = props.clip_rect() {
            let [x, y, w, h] = clip_rect;
            let tex_w = info.width as f32;
            let tex_h = info.height as f32;
            ClipRect {
                uv_offset: [x / tex_w, y / tex_h],
                uv_scale: [w / tex_w, h / tex_h],
                quad_size: [w, h],
            }
        } else {
            ClipRect::full(info)
        };
        frame.push(DrawCommand {
            texture,
            size: info,
            transform: world,
            opacity,
            blend: blend_mode(&props),
            color: color_filter(&props),
            clip,
            clip_bounds,
            shader: command_shader.clone(),
            mesh: None,
            stencil: None,
        });
    }

    // Host-owned layer content is local to this scene node and therefore
    // belongs before its child layers.
    if let Some(content) = content_for {
        for mut cmd in content(id) {
            cmd.transform = world * cmd.transform;
            cmd.opacity *= opacity;
            let content_clip = cmd.clip_bounds.map(|bounds| transform_rect(world, bounds));
            cmd.clip_bounds = intersect_clip_bounds(content_clip, clip_bounds);
            if cmd.shader.is_none() {
                cmd.shader = command_shader.clone();
            }
            frame.push(cmd);
        }
    }

    // 按 Artemis 图层顺序遍历子图层（数字优先，数字按值，字符串按字典序）。
    for child in children {
        visit(
            scene,
            &child,
            now_ms,
            world,
            opacity,
            clip_bounds,
            command_shader.clone(),
            provider,
            frame,
            content_for,
            text_for,
        );
    }

    // 文本注入：文本命令为层内局部坐标，乘入世界变换与不透明度。
    if let Some(tf) = text_for {
        for mut cmd in tf(id) {
            cmd.transform = world * cmd.transform;
            cmd.opacity *= opacity;
            cmd.clip_bounds = intersect_clip_bounds(cmd.clip_bounds, clip_bounds);
            frame.push(cmd);
        }
    }

    if let Some(effect) = group_shader {
        let end = frame.len();
        if end > group_start {
            frame.push_shader_group(ShaderGroup {
                start: group_start,
                end,
                effect,
                clip_bounds,
                mask_range: None,
            });
        }
    }
}

/// `Some(None)` means the layer explicitly specified `shader=""`.
fn declared_shader(
    scene: &Scene,
    props: &LayerProps,
    provider: &mut dyn TextureProvider,
) -> Option<Option<ShaderEffect>> {
    let Some(shader_name) = props.shader.as_deref() else {
        return None;
    };
    if shader_name.is_empty() {
        return Some(None);
    }

    let mut uniforms = BTreeMap::new();
    for name in &props.shader_constants {
        let value = shader_uniform_value(props, name);
        if let Some(value) = value {
            uniforms.insert(name.clone(), value);
        }
    }

    let user_texture = props
        .shader_textures
        .iter()
        .find(|name| name.eq_ignore_ascii_case("textureUser"))
        .and_then(|name| props.custom.get(name))
        .and_then(|reference| {
            scene
                .get(reference)
                .and_then(|layer| layer.file.as_deref())
                .or(Some(reference.as_str()))
        })
        .and_then(|file| provider.resolve(file).map(|(texture, _)| texture));
    let mask_texture = props
        .custom
        .get("mask")
        .and_then(|reference| {
            scene
                .get(reference)
                .and_then(|layer| layer.file.as_deref())
                .or(Some(reference.as_str()))
        })
        .and_then(|file| provider.resolve(file).map(|(texture, _)| texture));

    Some(Some(ShaderEffect {
        name: shader_name.to_string(),
        uniforms,
        mask_texture,
        user_texture,
    }))
}

fn shader_uniform_value(props: &LayerProps, name: &str) -> Option<Vec<f32>> {
    let raw = props
        .custom
        .get(name)
        .map(String::as_str)
        .or_else(|| match name {
            "width" => props.width.map(|_| ""),
            "height" => props.height.map(|_| ""),
            _ => None,
        });

    if raw == Some("") {
        return match name {
            "width" => props.width.map(|value| vec![value]),
            "height" => props.height.map(|value| vec![value]),
            _ => None,
        };
    }

    let values: Vec<f32> = raw?
        .split(',')
        .filter_map(|part| part.trim().parse::<f32>().ok())
        .collect();
    (!values.is_empty()).then_some(values)
}

/// 复制属性并叠加当前时刻的缓动值。
fn resolved_props(layer: &crate::compositor::scene::Layer, now_ms: u64) -> LayerProps {
    let mut props = layer.props.clone();
    for tween in &layer.tweens {
        let value = tween.value_at(now_ms);
        props.set_raw(&tween.param, &LayerProps::format_value(&tween.param, value));
    }
    props
}

fn local_transform(props: &LayerProps) -> Affine2 {
    props.local_transform()
}

fn subtree_clip_bounds(
    props: &LayerProps,
    world: Affine2,
    parent_clip: Option<[f32; 4]>,
    provider: &mut dyn TextureProvider,
) -> Option<[f32; 4]> {
    let local = props
        .custom
        .get("intermediate_render_mask")
        .and_then(|mask| provider.resolve(mask))
        .map(|(_, info)| [0.0, 0.0, info.width as f32, info.height as f32])
        .or_else(|| {
            if props.intermediate_render.is_some() {
                props.clip.map(|[x, y, w, h]| [x, y, w, h])
            } else {
                None
            }
        });

    let local = local.map(|rect| transform_rect(world, rect));
    intersect_clip_bounds(parent_clip, local)
}

fn intersect_clip_bounds(a: Option<[f32; 4]>, b: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let left = a[0].max(b[0]);
            let top = a[1].max(b[1]);
            let right = (a[0] + a[2]).min(b[0] + b[2]);
            let bottom = (a[1] + a[3]).min(b[1] + b[3]);
            Some([left, top, (right - left).max(0.0), (bottom - top).max(0.0)])
        }
        (Some(rect), None) | (None, Some(rect)) => Some(rect),
        (None, None) => None,
    }
}

fn transform_rect(transform: Affine2, rect: [f32; 4]) -> [f32; 4] {
    let [x, y, w, h] = rect;
    let points = [
        transform.transform_point2(Vec2::new(x, y)),
        transform.transform_point2(Vec2::new(x + w, y)),
        transform.transform_point2(Vec2::new(x, y + h)),
        transform.transform_point2(Vec2::new(x + w, y + h)),
    ];
    let min_x = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let min_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let max_y = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    [min_x, min_y, max_x - min_x, max_y - min_y]
}

fn blend_mode(props: &LayerProps) -> BlendMode {
    match props.layer_mode.as_deref() {
        Some("add") | Some("additive") => BlendMode::Add,
        Some("screen") => BlendMode::Screen,
        Some("multiply") | Some("mul") => BlendMode::Multiply,
        _ => BlendMode::Alpha,
    }
}

fn color_filter(props: &LayerProps) -> ColorFilter {
    ColorFilter {
        multiply: props.color_multiply.unwrap_or([1.0, 1.0, 1.0]),
        grayscale: props.grayscale.unwrap_or(false),
        negative: props.negative.unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::anim::{Easing, Tween};
    use crate::compositor::mock::{MockProvider, TEXTURE_SIZE};
    use crate::render_pipeline::draw::{TextureId, TextureInfo};
    use std::collections::HashMap;

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn culls_invisible_layer_and_subtree() {
        let mut scene = Scene::new();
        scene.create("1", Some("bg".into()));
        scene.create("1.0", Some("fg".into()));
        scene.set_props("1", &raw(&[("visible", "0")]));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        // 父被隐藏，父与子都不该出现。
        assert!(frame.is_empty());
    }

    #[test]
    fn grouping_node_without_file_emits_nothing_but_passes_transform() {
        let mut scene = Scene::new();
        // "1" 是纯分组节点（无 file），"1.0" 才有纹理。
        scene.set_props("1", &raw(&[("left", "100")]));
        scene.create("1.0", Some("fg".into()));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        assert_eq!(frame.len(), 1); // 只有 1.0 产出
        // 子图层继承了父的平移 100。
        let cmd = &frame.commands[0];
        let origin = cmd.transform.transform_point2(Vec2::ZERO);
        assert_eq!(origin.x, 100.0);
    }

    #[test]
    fn host_content_inherits_parent_transform_and_precedes_children() {
        let mut scene = Scene::new();
        scene.set_props("1", &raw(&[("left", "100"), ("top", "50")]));
        scene.create("1.0", Some("child".into()));

        let info = TextureInfo {
            width: 16,
            height: 16,
        };
        let injected = DrawCommand {
            texture: TextureId(999),
            size: info,
            transform: Affine2::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect::full(info),
            clip_bounds: Some([0.0, 0.0, 16.0, 16.0]),
            shader: None,
            mesh: None,
            stencil: None,
        };
        let content_for = |id: &str| {
            if id == "1" {
                vec![injected.clone()]
            } else {
                Vec::new()
            }
        };

        let mut provider = MockProvider::new();
        let frame = build_frame_with_content(&scene, 0, &mut provider, Some(&content_for), None);
        assert_eq!(frame.commands.len(), 2);
        assert_eq!(frame.commands[0].texture, TextureId(999));
        assert_eq!(provider.name_of(frame.commands[1].texture), "child");
        assert_eq!(
            frame.commands[0].transform.transform_point2(Vec2::ZERO),
            Vec2::new(100.0, 50.0)
        );
        assert_eq!(
            frame.commands[0].clip_bounds,
            Some([100.0, 50.0, 16.0, 16.0])
        );
    }

    #[test]
    fn intermediate_render_mask_clips_subtree_to_mask_size() {
        let mut scene = Scene::new();
        scene.set_props(
            "1",
            &raw(&[
                ("left", "100"),
                ("top", "50"),
                ("intermediate_render", "1"),
                ("intermediate_render_mask", "mask"),
            ]),
        );
        scene.create("1.0", Some("fg".into()));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        assert_eq!(frame.len(), 1);
        assert_eq!(
            frame.commands[0].clip_bounds,
            Some([100.0, 50.0, TEXTURE_SIZE as f32, TEXTURE_SIZE as f32])
        );
    }

    #[test]
    fn shader_constants_and_user_texture_reach_draw_command() {
        let mut scene = Scene::new();
        scene.create("1", Some("foreground".into()));
        scene.create("900", Some("effect.png".into()));
        scene.set_props("900", &raw(&[("visible", "0")]));
        scene.set_props(
            "1",
            &raw(&[
                ("shader", "compbr"),
                ("shaderconstant", "param,weights"),
                ("param", "0.5"),
                ("weights", "0.1,0.2,0.3"),
                ("shadertexture", "textureUser"),
                ("textureUser", "900"),
            ]),
        );

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let command = &frame.commands[0];
        let effect = command.shader.as_ref().unwrap();
        assert_eq!(effect.name, "compbr");
        assert_eq!(effect.uniforms["param"], [0.5]);
        assert_eq!(effect.uniforms["weights"], [0.1, 0.2, 0.3]);
        assert_eq!(provider.name_of(effect.user_texture.unwrap()), "effect.png");
    }

    #[test]
    fn group_shader_creates_one_subtree_pass() {
        let mut scene = Scene::new();
        scene.set_props("1", &raw(&[("shader", "gray")]));
        scene.create("1.0", Some("a".into()));
        scene.create("1.1", Some("b".into()));
        scene.set_props("1.1", &raw(&[("shader", "")]));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        assert_eq!(frame.shader_groups.len(), 1);
        let group = &frame.shader_groups[0];
        assert_eq!(group.start, 0);
        assert_eq!(group.end, 2);
        assert_eq!(group.effect.name, "gray");
        let b = frame
            .commands
            .iter()
            .find(|command| provider.name_of(command.texture) == "b")
            .unwrap();
        assert!(b.shader.is_none());
    }

    #[test]
    fn draw_order_follows_artemis_layer_id_order() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        scene.create("1.1", Some("b".into()));
        scene.create("1.0", Some("c".into()));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let names: Vec<&str> = frame
            .commands
            .iter()
            .map(|c| provider.name_of(c.texture))
            .collect();
        // 父先画，子按 Artemis 图层顺序（数字部分按值排序：1.0 < 1.1）。
        assert_eq!(names, vec!["a", "c", "b"]);
    }

    #[test]
    fn opacity_multiplies_down_the_tree() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        scene.create("1.0", Some("b".into()));
        scene.set_props("1", &raw(&[("alpha", "128")])); // ~0.5
        scene.set_props("1.0", &raw(&[("alpha", "128")])); // ~0.5

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let child = frame
            .commands
            .iter()
            .find(|c| provider.name_of(c.texture) == "b")
            .unwrap();
        // 0.5 * 0.5 = 0.25
        assert!((child.opacity - 0.25).abs() < 0.01);
    }

    #[test]
    fn scale_uses_percent_and_anchor() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        // 锚点在 (10,10)，放大 2 倍：锚点本身不动。
        scene.set_props(
            "1",
            &raw(&[
                ("xscale", "200"),
                ("yscale", "200"),
                ("anchorx", "10"),
                ("anchory", "10"),
            ]),
        );

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let cmd = &frame.commands[0];
        let anchor = cmd.transform.transform_point2(Vec2::new(10.0, 10.0));
        assert!((anchor.x - 10.0).abs() < 1e-4);
        assert!((anchor.y - 10.0).abs() < 1e-4);
        // 原点被放大推到 -10。
        let origin = cmd.transform.transform_point2(Vec2::ZERO);
        assert!((origin.x - (-10.0)).abs() < 1e-4);
    }

    #[test]
    fn tween_drives_alpha_over_time() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        scene.set_props("1", &raw(&[("alpha", "0")]));
        scene.get_mut("1").unwrap().tweens.push(Tween {
            param: "alpha".into(),
            from: 0.0,
            to: 255.0,
            easing: Easing::Linear,
            start_ms: 0,
            duration_ms: 1000,
            infinite_loop: false,
            loop_count: None,
            yoyo: false,
            yoyo_reverse: false,
            loop_delay_ms: 0,
            delete_on_finish: false,
            handler: None,
        });

        let mut provider = MockProvider::new();
        // 中点：alpha≈127 → opacity≈0.5
        let frame = build_frame(&scene, 500, &mut provider, None);
        assert!((frame.commands[0].opacity - 0.5).abs() < 0.02);
        // 末尾：alpha=255 → opacity=1.0
        let frame = build_frame(&scene, 1000, &mut provider, None);
        assert!((frame.commands[0].opacity - 1.0).abs() < 1e-4);
    }

    #[test]
    fn texture_size_is_propagated() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        assert_eq!(frame.commands[0].size.width, TEXTURE_SIZE);
    }

    #[test]
    fn clip_rect_is_computed_from_props() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        // 纹理是 TEXTURE_SIZE x TEXTURE_SIZE (256x256)
        // 裁剪矩形：从 (10,20) 开始，宽高 (100,50)
        scene.set_props("1", &raw(&[("clip", "10,20,100,50")]));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let cmd = &frame.commands[0];

        // quad_size 应该是裁剪矩形的宽高
        assert_eq!(cmd.clip.quad_size, [100.0, 50.0]);
        // UV offset 应该是裁剪起点除以纹理尺寸
        assert!((cmd.clip.uv_offset[0] - 10.0 / 256.0).abs() < 1e-6);
        assert!((cmd.clip.uv_offset[1] - 20.0 / 256.0).abs() < 1e-6);
        // UV scale 应该是裁剪宽高除以纹理尺寸
        assert!((cmd.clip.uv_scale[0] - 100.0 / 256.0).abs() < 1e-6);
        assert!((cmd.clip.uv_scale[1] - 50.0 / 256.0).abs() < 1e-6);
    }

    #[test]
    fn no_clip_defaults_to_full_texture() {
        let mut scene = Scene::new();
        scene.create("1", Some("a".into()));
        // 不设置 clip

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let cmd = &frame.commands[0];

        // 无裁剪时，quad_size 等于纹理尺寸
        assert_eq!(
            cmd.clip.quad_size,
            [TEXTURE_SIZE as f32, TEXTURE_SIZE as f32]
        );
        // UV offset 为 0
        assert_eq!(cmd.clip.uv_offset, [0.0, 0.0]);
        // UV scale 为 1
        assert_eq!(cmd.clip.uv_scale, [1.0, 1.0]);
    }
}
