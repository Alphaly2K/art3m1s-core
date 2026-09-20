use super::Compositor;
use crate::compositor::build::resolved_props;
use crate::compositor::props::LayerProps;
use crate::render_pipeline::draw::{TextureId, TextureProvider};
use glam::{Affine2, Vec2};
use std::collections::HashSet;

impl Compositor {
    /// Returns whether texture uploads since the previous pointer sample can
    /// actually change alpha-based hit testing.
    ///
    /// The GPU revision is global: text glyphs, video frames and unrelated
    /// layers all advance it. Re-running pointer hit testing for those uploads
    /// can feed a rollover handler back into itself (rollover changes a sprite
    /// clip, text rendering uploads glyphs, the global revision changes, and
    /// the same stationary pointer is tested again). Only textures belonging
    /// to interactive layers that opt into `clickablethreshold` are relevant.
    pub fn pointer_hit_textures_changed(
        &self,
        changed: &HashSet<TextureId>,
        provider: &mut dyn TextureProvider,
    ) -> bool {
        if changed.is_empty() {
            return false;
        }
        self.scene.all_layers().any(|layer| {
            layer.event_handlers.values().any(|handler| handler.enabled)
                && layer.props.custom.contains_key("clickablethreshold")
                && layer.file.as_deref().is_some_and(|file| {
                    provider
                        .resolve(file)
                        .is_some_and(|(texture, _)| changed.contains(&texture))
                })
        })
    }

    /// 命中测试：返回舞台坐标 (x, y) 处最上层、可接收指针输入的图层 ID。
    ///
    /// 返回最上层的潜在交互图层。完整事件派发使用 [`Self::hit_test_all`]：先取得
    /// 同一点的全部潜在目标，再按 click/rollover 等事件类型过滤处理器，并由最上层
    /// 对应处理器的 `penetration` 决定是否继续向下派发。
    ///
    /// 「可交互」需同时满足：visible != false、注册了至少一个事件处理器、且未被
    /// `clickablethreshold` 判为透明。`clickablethreshold` 是 Artemis 的指针命中
    /// 阈值：纹理像素 alpha 低于或等于该阈值时对指针透明（不吃事件）。图层自身 alpha
    /// 只影响渲染，不应让脚本用 alpha=0 做出的不可见热区失效。
    ///
    /// 命中用图层 left/top/width/height 做 AABB 判定。没有可推断宽高的纯分组节点跳过。
    pub fn hit_test(&self, x: f32, y: f32, provider: &mut dyn TextureProvider) -> Option<String> {
        self.hit_test_all(x, y, provider).into_iter().next()
    }

    /// 返回舞台坐标处所有可接收指针输入的图层 ID，按从上到下排序。
    ///
    /// hover/rollout 和 click 穿透需要这个集合。命中测试本身只做几何、可见性和
    /// alpha 阈值判定；事件类型过滤及 `penetration` 链由输入派发层处理。
    pub fn hit_test_all(&self, x: f32, y: f32, provider: &mut dyn TextureProvider) -> Vec<String> {
        let mut hits = Vec::new();
        // `[lyprop id="!"]` 根图层属性同样作用于命中检测：整树隐藏时无命中，
        // 根变换让命中几何与绘制几何一致。
        let root_props = self.scene.root_props();
        if !root_props.is_visible() {
            return hits;
        }
        let root_transform = root_props.local_transform();
        let roots = self.scene.roots();
        let scale = self.stage_scale;
        for root in roots.iter().rev() {
            self.hit_test_subtree(root, root_transform, x, y, scale, provider, &mut hits);
        }
        hits
    }

    fn hit_test_subtree(
        &self,
        id: &str,
        parent_transform: Affine2,
        mx: f32,
        my: f32,
        scale: f32,
        provider: &mut dyn TextureProvider,
        hits: &mut Vec<String>,
    ) {
        let Some(layer) = self.scene.get(id) else {
            return;
        };
        // Match the render path: an in-flight `lytween` changes the geometry
        // visible on screen without mutating the base properties until it
        // finishes.  Hit testing the base props would leave the interactive
        // area behind while a layer is moving (or otherwise being animated).
        let props = resolved_props(layer, self.clock_ms);

        if !props.is_visible() {
            return;
        }

        let world = parent_transform * local_transform(&props);

        // 先递归检测子层（高 z-order 优先，reverse 遍历）。
        // 注意按 Artemis 图层顺序排序（与绘制次序一致），不能用原始插入顺序，
        // 否则命中的 z-order 与画面不符。
        let children = self.scene.children(id);
        for child_id in children.iter().rev() {
            self.hit_test_subtree(child_id, world, mx, my, scale, provider, hits);
        }

        // 再检测本层：注册了任意事件处理器。
        if layer.event_handlers.values().any(|handler| handler.enabled) {
            // 宽高优先级：
            // 1. props.width/height（显式设置的逻辑尺寸）
            // 2. clip 的宽高（精灵表裁剪区域，已经是逻辑坐标）
            // 3. 纹理物理尺寸 / scale（整张纹理的逻辑尺寸）
            let (w, h) = if let (Some(w), Some(h)) = (props.width, props.height) {
                (w, h)
            } else if let Some(clip) = props.clip_rect() {
                // clip = [x, y, w, h]，取 w 和 h
                (clip[2], clip[3])
            } else if let Some(file) = &layer.file {
                if let Some((_, info)) = provider.resolve(file) {
                    (info.width as f32 / scale, info.height as f32 / scale)
                } else {
                    return;
                }
            } else {
                return;
            };

            let local = world.inverse().transform_point2(Vec2::new(mx, my));

            if local.x >= 0.0
                && local.x < w
                && local.y >= 0.0
                && local.y < h
                && !self.is_pointer_transparent_at(
                    &props,
                    local.x,
                    local.y,
                    scale,
                    provider,
                    &layer.file,
                )
            {
                hits.push(id.to_string());
            }
        }
    }

    /// 按 `clickablethreshold` 判断图层在指定坐标处是否对指针透明。
    ///
    /// Artemis 的 `clickablethreshold` 是指针命中的 alpha 阈值：**坐标处的纹理像素
    /// alpha** 低于或等于阈值时，指针穿透该图层。例如，圆形按钮四角的透明像素
    /// alpha=0，低于阈值 128，点击穿透；中心像素 alpha=255，高于阈值，点击被该图层接收。
    /// 图层 alpha 不参与像素阈值判断，否则 alpha=0 的不可见输入热区无法接收 hover。
    ///
    /// 未设 `clickablethreshold` 的图层一律可点（默认行为）。
    fn is_pointer_transparent_at(
        &self,
        props: &LayerProps,
        local_x: f32,
        local_y: f32,
        scale: f32,
        provider: &mut dyn TextureProvider,
        file: &Option<String>,
    ) -> bool {
        let Some(threshold) = props
            .custom
            .get("clickablethreshold")
            .and_then(|v| v.trim().parse::<i32>().ok())
        else {
            return false;
        };

        // 计算该点在纹理中的局部像素坐标。
        // local_x/local_y 是逆变换后的图层局部坐标；scale 是舞台到物理像素的缩放因子。
        let local_x = (local_x * scale) as u32;
        let local_y = (local_y * scale) as u32;

        // 加上 clip 偏移（如果图层有 clip 属性）。
        let (tex_x, tex_y) = if let Some(clip) = props.clip_rect() {
            (local_x + clip[0] as u32, local_y + clip[1] as u32)
        } else {
            (local_x, local_y)
        };

        // 采样纹理像素 alpha。
        let pixel_alpha = if let Some(file) = file {
            provider
                .resolve(file)
                .and_then(|(tid, _)| provider.pixel_alpha(tid, tex_x, tex_y))
        } else {
            None
        };

        let hit_alpha = match pixel_alpha {
            Some(pa) => pa as i32,
            None => props.alpha.unwrap_or(255) as i32, // 无法采样时退回图层 alpha
        };

        hit_alpha <= threshold
    }
}

fn local_transform(props: &LayerProps) -> Affine2 {
    props.local_transform()
}
