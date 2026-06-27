use super::Compositor;
use crate::compositor::props::LayerProps;
use crate::render_pipeline::draw::TextureProvider;

impl Compositor {
    /// 命中测试：返回舞台坐标 (x, y) 处最上层、可接收指针输入的图层 ID。
    ///
    /// Artemis 的命中是「单次取最上层」的：找到顶端的可交互图层后，宿主再按事件
    /// 类型（click/rollover/...）去它的 `event_handlers` 取处理器——并**不**分事件
    /// 类型各做一次命中。
    ///
    /// 「可交互」需同时满足：visible != false、注册了至少一个事件处理器、且未被
    /// `clickablethreshold` 判为透明。`clickablethreshold` 是 Artemis 的指针命中
    /// 阈值：图层有效 alpha 低于该阈值时对指针透明（不吃事件）。tablet 的
    /// `mw.zmask` 正是一张 alpha=0、clickablethreshold=128 的全宽透明遮罩，只用于
    /// 程序化拖拽，不应拦截落到其下工具栏按钮的 hover/click——靠这个阈值放行。
    ///
    /// 命中用图层 left/top/width/height 做 AABB 判定。没有可推断宽高的纯分组节点跳过。
    pub fn hit_test(&self, x: f32, y: f32, provider: &mut dyn TextureProvider) -> Option<String> {
        let roots = self.scene.roots();
        let scale = self.stage_scale;
        for root in roots.iter().rev() {
            if let Some(hit) = self.hit_test_subtree(root, 0.0, 0.0, x, y, scale, provider) {
                return Some(hit);
            }
        }
        None
    }

    fn hit_test_subtree(
        &self,
        id: &str,
        parent_x: f32,
        parent_y: f32,
        mx: f32,
        my: f32,
        scale: f32,
        provider: &mut dyn TextureProvider,
    ) -> Option<String> {
        let layer = self.scene.get(id)?;
        let props = &layer.props;

        if props.visible == Some(false) {
            return None;
        }

        let (lx, ly) = props.offset();
        let abs_x = parent_x + lx;
        let abs_y = parent_y + ly;

        // 先递归检测子层（高 z-order 优先，reverse 遍历）。
        // 注意按 Artemis 图层顺序排序（与绘制次序一致），不能用原始插入顺序，
        // 否则命中的 z-order 与画面不符。
        let children = self.scene.children(id);
        for child_id in children.iter().rev() {
            if let Some(hit) =
                self.hit_test_subtree(child_id, abs_x, abs_y, mx, my, scale, provider)
            {
                return Some(hit);
            }
        }

        // 再检测本层：注册了任意事件处理器。
        if !layer.event_handlers.is_empty() {
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
                    return None;
                }
            } else {
                return None;
            };

            if mx >= abs_x
                && mx < abs_x + w
                && my >= abs_y
                && my < abs_y + h
                && !self.is_pointer_transparent_at(
                    props,
                    mx,
                    my,
                    abs_x,
                    abs_y,
                    scale,
                    provider,
                    &layer.file,
                )
            {
                return Some(id.to_string());
            }
        }

        None
    }

    /// 按 `clickablethreshold` 判断图层在指定坐标处是否对指针透明。
    ///
    /// Artemis 的 `clickablethreshold` 是指针命中的 alpha 阈值：**坐标处的像素
    /// alpha**（乘以图层 alpha 后的有效 alpha）低于阈值时，指针穿透该图层。
    /// 例如，圆形按钮四角的透明像素 alpha=0，低于阈值 128，点击穿透；中心像素
    /// alpha=255，高于阈值，点击被该图层接收。
    ///
    /// 未设 `clickablethreshold` 的图层一律可点（默认行为）。
    fn is_pointer_transparent_at(
        &self,
        props: &LayerProps,
        mx: f32,
        my: f32,
        abs_x: f32,
        abs_y: f32,
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
        // mx, my 是舞台坐标；abs_x, abs_y 是图层左上角的舞台坐标；
        // scale 是舞台到物理像素的缩放因子。
        let local_x = ((mx - abs_x) * scale) as u32;
        let local_y = ((my - abs_y) * scale) as u32;

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

        // 有效 alpha = 像素 alpha × 图层 alpha / 255。
        let layer_alpha = props.alpha.unwrap_or(255) as i32;
        let effective_alpha = match pixel_alpha {
            Some(pa) => (pa as i32) * layer_alpha / 255,
            None => layer_alpha, // 无法采样时只用图层 alpha
        };

        effective_alpha < threshold
    }
}
