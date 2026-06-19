//! 帧构建：把场景树在某一时刻"压平"成有序的绘制列表。
//!
//! 每帧调用 [`build_frame`]：它从根到叶遍历场景树，沿途累积父图层的仿射变换与
//! 不透明度，对进行中的缓动求出当前值，剔除隐藏图层（连同其子树），最后对每个
//! 绑定了纹理的图层产出一条 [`DrawCommand`]，按遍历顺序（先根后子、同级按插入
//! 顺序）排列——也就是从底到顶的绘制次序。

use crate::compositor::props::LayerProps;
use crate::compositor::renderer::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, TextureProvider,
};
use crate::compositor::scene::Scene;
use glam::{Affine2, Vec2};

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
    text_for: Option<&dyn Fn(&str) -> Vec<DrawCommand>>,
) -> DrawList {
    let mut frame = DrawList::new();
    for root in scene.roots() {
        visit(scene, &root, now_ms, Affine2::IDENTITY, 1.0, provider, &mut frame, text_for);
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
    provider: &mut dyn TextureProvider,
    frame: &mut DrawList,
    text_for: Option<&dyn Fn(&str) -> Vec<DrawCommand>>,
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
        });
    }

    // 按 Artemis 图层顺序遍历子图层（数字优先，数字按值，字符串按字典序）。
    for child in scene.children(id) {
        visit(scene, &child, now_ms, world, opacity, provider, frame, text_for);
    }

    // 文本注入：文本命令为层内局部坐标，乘入世界变换与不透明度。
    if let Some(tf) = text_for {
        for mut cmd in tf(id) {
            cmd.transform = world * cmd.transform;
            cmd.opacity *= opacity;
            frame.push(cmd);
        }
    }
}

/// 复制属性并叠加当前时刻的缓动值。
fn resolved_props(layer: &crate::compositor::scene::Layer, now_ms: u64) -> LayerProps {
    let mut props = layer.props.clone();
    for tween in &layer.tweens {
        let value = tween.value_at(now_ms);
        // 缓动的 param 名沿用原始属性名，复用同一套解析逻辑写回。
        props.set_raw(&tween.param, &format_value(&tween.param, value));
    }
    props
}

/// 把缓动求得的数值格式化回属性字符串，交给 `set_raw` 解析。
/// alpha/visible 等整数属性按整数格式化，避免 "128.0" 落入浮点回退路径。
fn format_value(param: &str, value: f32) -> String {
    match param {
        "alpha" | "visible" | "reversex" | "reversey" | "grayscale" | "negative" => {
            (value.round() as i64).to_string()
        }
        _ => value.to_string(),
    }
}

/// 计算单个图层相对其父的本地仿射变换。
///
/// 按 Artemis 语义，缩放与旋转都绕锚点进行，最终再平移到 (left, top)：
/// `T(left,top) · T(anchor) · R(rotate) · S(scale) · T(-anchor)`
fn local_transform(props: &LayerProps) -> Affine2 {
    let (left, top) = props.offset();
    let (sx, sy) = props.scale();
    let (ax, ay) = props.anchor();
    let rot = props.rotation_radians();

    let translate = Affine2::from_translation(Vec2::new(left, top));
    let to_anchor = Affine2::from_translation(Vec2::new(ax, ay));
    let rotate = Affine2::from_angle(rot);
    let scale = Affine2::from_scale(Vec2::new(sx, sy));
    let from_anchor = Affine2::from_translation(Vec2::new(-ax, -ay));

    translate * to_anchor * rotate * scale * from_anchor
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
        let cmd = frame.commands[0];
        let origin = cmd.transform.transform_point2(Vec2::ZERO);
        assert_eq!(origin.x, 100.0);
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
        scene.set_props("1", &raw(&[("xscale", "200"), ("yscale", "200"), ("anchorx", "10"), ("anchory", "10")]));

        let mut provider = MockProvider::new();
        let frame = build_frame(&scene, 0, &mut provider, None);
        let cmd = frame.commands[0];
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
        assert_eq!(cmd.clip.quad_size, [TEXTURE_SIZE as f32, TEXTURE_SIZE as f32]);
        // UV offset 为 0
        assert_eq!(cmd.clip.uv_offset, [0.0, 0.0]);
        // UV scale 为 1
        assert_eq!(cmd.clip.uv_scale, [1.0, 1.0]);
    }
}
