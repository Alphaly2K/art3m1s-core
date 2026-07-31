//! `[lyedit]` 图层像素加工。
//!
//! Artemis 语义：对图层中已加载的图像做 **CPU 像素级、永久性** 的加工（非渲染期
//! 滤镜）。支持 negative / gray / sepia / add / multiply / blend 六种模式。
//!
//! 流程：事件在 [`crate::compositor::Compositor`] 上排队（无 GPU 依赖），渲染管线
//! 在帧构建前调用 [`process_pending`]——从 provider 读出图层当前纹理的 CPU 像素、
//! 做像素变换、以 `__lyedit_*` 名字重新上传，并记录"图层 → 加工后纹理"的重定向，
//! 帧构建时按重定向取纹理。同一图层连续多次 lyedit 在上一次结果上继续叠加。

use crate::compositor::scene::Scene;
use crate::render_pipeline::draw::TextureProvider;
use std::collections::HashMap;

/// 一次排队的 `[lyedit]` 请求。
#[derive(Debug, Clone)]
pub(crate) struct LayerEditRequest {
    pub(crate) id: String,
    pub(crate) mode: String,
    /// add/multiply 用的 `RRGGBB` 颜色。
    pub(crate) color: Option<String>,
    /// blend 用的合成源图路径。
    pub(crate) file: Option<String>,
    /// blend 合成目标位置。
    pub(crate) left: i32,
    pub(crate) top: i32,
}

/// 某图层最近一次加工的结果。
#[derive(Debug, Clone)]
pub(crate) struct EditedLayer {
    /// 加工所基于的图层 file 名——图层换图后此项失配，重定向即失效。
    pub(crate) base_file: String,
    /// 加工结果上传到 provider 的纹理名。
    pub(crate) texture_name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// CPU 侧像素副本，供同图层的后续 lyedit 叠加。
    pub(crate) pixels: Vec<u8>,
}

/// lyedit 的排队与结果状态（挂在 Compositor 上，RefCell 内部可变）。
#[derive(Debug, Default)]
pub(crate) struct LayerEditQueue {
    pub(crate) pending: Vec<LayerEditRequest>,
    /// 图层 ID → 最近一次加工结果。
    pub(crate) states: HashMap<String, EditedLayer>,
    /// 单调递增，保证每次加工产生新纹理名（provider 按名换纹理）。
    counter: u64,
}

impl LayerEditQueue {
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.states.clear();
    }

    /// 计算当前有效的"图层 → 加工后纹理名"重定向表。
    /// 图层已删除或换图（file 与加工基准不一致）的条目不再生效。
    pub(crate) fn valid_overrides(&self, scene: &Scene) -> HashMap<String, String> {
        self.states
            .iter()
            .filter(|(id, state)| {
                scene
                    .get(id)
                    .and_then(|layer| layer.file.as_deref())
                    .is_some_and(|file| file == state.base_file)
            })
            .map(|(id, state)| (id.clone(), state.texture_name.clone()))
            .collect()
    }
}

/// 处理所有排队的 lyedit 请求（渲染管线每帧调用一次）。
pub(crate) fn process_pending(
    scene: &Scene,
    queue: &mut LayerEditQueue,
    provider: &mut dyn TextureProvider,
) {
    if queue.pending.is_empty() {
        return;
    }
    let pending = std::mem::take(&mut queue.pending);
    for request in pending {
        process_one(scene, queue, provider, &request);
    }
}

fn process_one(
    scene: &Scene,
    queue: &mut LayerEditQueue,
    provider: &mut dyn TextureProvider,
    request: &LayerEditRequest,
) {
    let Some(base_file) = scene
        .get(&request.id)
        .and_then(|layer| layer.file.clone())
        .filter(|file| !file.is_empty())
    else {
        crate::core_warn!("[lyedit] 图层 {} 无已加载图像，忽略", request.id);
        return;
    };

    // 源像素：同图层上一次加工的结果（且基准未变）优先，否则从 provider 读原图。
    let source = match queue.states.get(&request.id) {
        Some(state) if state.base_file == base_file => {
            Some((state.width, state.height, state.pixels.clone()))
        }
        _ => provider.pixels_of(&base_file),
    };
    let Some((width, height, mut pixels)) = source else {
        crate::core_warn!("[lyedit] 无法读取 {} 的像素，忽略", base_file);
        return;
    };

    // blend 模式需要合成源图的像素。
    let overlay = if request.mode == "blend" {
        let Some(overlay) = request
            .file
            .as_deref()
            .filter(|f| !f.is_empty())
            .and_then(|f| provider.pixels_of(f))
        else {
            crate::core_warn!("[lyedit] blend 缺少可读取的 file 参数，忽略");
            return;
        };
        Some(overlay)
    } else {
        None
    };

    let color = request
        .color
        .as_deref()
        .and_then(crate::compositor::props::parse_hex_color)
        .map(|[_, r, g, b]| [r, g, b]);

    apply_mode(
        &mut pixels,
        width,
        height,
        &request.mode,
        color,
        overlay.as_ref().map(|(w, h, p)| (*w, *h, p.as_slice())),
        request.left,
        request.top,
    );

    queue.counter += 1;
    let texture_name = format!("__lyedit_{}_{}__", request.id, queue.counter);
    if provider
        .upload_rgba_render_only(&texture_name, width, height, &pixels)
        .is_none()
    {
        crate::core_warn!("[lyedit] 上传加工结果失败: {}", texture_name);
        return;
    }
    queue.states.insert(
        request.id.clone(),
        EditedLayer {
            base_file,
            texture_name,
            width,
            height,
            pixels,
        },
    );
}

/// 对一块 RGBA8 像素就地应用 lyedit 模式变换（纯函数，便于单测）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_mode(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    mode: &str,
    color: Option<[u8; 3]>,
    overlay: Option<(u32, u32, &[u8])>,
    left: i32,
    top: i32,
) {
    match mode {
        "negative" => {
            for px in pixels.chunks_exact_mut(4) {
                px[0] = 255 - px[0];
                px[1] = 255 - px[1];
                px[2] = 255 - px[2];
            }
        }
        "gray" => {
            for px in pixels.chunks_exact_mut(4) {
                let g = luminance(px[0], px[1], px[2]);
                px[0] = g;
                px[1] = g;
                px[2] = g;
            }
        }
        "sepia" => {
            // 经典 sepia 矩阵（灰度 + 暖色调）。
            for px in pixels.chunks_exact_mut(4) {
                let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
                px[0] = (0.393 * r + 0.769 * g + 0.189 * b).min(255.0) as u8;
                px[1] = (0.349 * r + 0.686 * g + 0.168 * b).min(255.0) as u8;
                px[2] = (0.272 * r + 0.534 * g + 0.131 * b).min(255.0) as u8;
            }
        }
        "add" => {
            let [cr, cg, cb] = color.unwrap_or([0, 0, 0]);
            for px in pixels.chunks_exact_mut(4) {
                px[0] = px[0].saturating_add(cr);
                px[1] = px[1].saturating_add(cg);
                px[2] = px[2].saturating_add(cb);
            }
        }
        "multiply" => {
            let [cr, cg, cb] = color.unwrap_or([255, 255, 255]);
            for px in pixels.chunks_exact_mut(4) {
                px[0] = ((px[0] as u16 * cr as u16) / 255) as u8;
                px[1] = ((px[1] as u16 * cg as u16) / 255) as u8;
                px[2] = ((px[2] as u16 * cb as u16) / 255) as u8;
            }
        }
        "blend" => {
            let Some((ow, oh, opx)) = overlay else {
                return;
            };
            blend_src_over(pixels, width, height, ow, oh, opx, left, top);
        }
        other => {
            crate::core_warn!("[lyedit] 未知 mode: {other}");
        }
    }
}

/// 把 overlay 以 src-over 方式合成到 dst 的 (left, top) 处。
fn blend_src_over(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    ow: u32,
    oh: u32,
    overlay: &[u8],
    left: i32,
    top: i32,
) {
    for oy in 0..oh {
        let dy = top + oy as i32;
        if dy < 0 || dy >= dh as i32 {
            continue;
        }
        for ox in 0..ow {
            let dx = left + ox as i32;
            if dx < 0 || dx >= dw as i32 {
                continue;
            }
            let si = ((oy * ow + ox) * 4) as usize;
            let di = ((dy as u32 * dw + dx as u32) * 4) as usize;
            let sa = overlay[si + 3] as f32 / 255.0;
            let da = dst[di + 3] as f32 / 255.0;
            let out_a = sa + da * (1.0 - sa);
            for c in 0..3 {
                let sc = overlay[si + c] as f32;
                let dc = dst[di + c] as f32;
                // 非预乘 src-over：结果按合成后 alpha 归一化。
                let out = if out_a > 0.0 {
                    (sc * sa + dc * da * (1.0 - sa)) / out_a
                } else {
                    0.0
                };
                dst[di + c] = out.round().min(255.0) as u8;
            }
            dst[di + 3] = (out_a * 255.0).round().min(255.0) as u8;
        }
    }
}

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32)
        .round()
        .min(255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        vec![r, g, b, a]
    }

    #[test]
    fn negative_inverts_rgb_keeps_alpha() {
        let mut p = px(10, 200, 0, 128);
        apply_mode(&mut p, 1, 1, "negative", None, None, 0, 0);
        assert_eq!(p, px(245, 55, 255, 128));
    }

    #[test]
    fn gray_uses_luminance() {
        let mut p = px(255, 0, 0, 255);
        apply_mode(&mut p, 1, 1, "gray", None, None, 0, 0);
        let g = (0.299f32 * 255.0).round() as u8;
        assert_eq!(p, px(g, g, g, 255));
    }

    #[test]
    fn sepia_produces_warm_tone_and_clamps() {
        // 白色：r/g 通道溢出并截断到 255，b 通道系数和 <1 不溢出。
        let mut white = px(255, 255, 255, 255);
        apply_mode(&mut white, 1, 1, "sepia", None, None, 0, 0);
        assert_eq!(&white[0..2], &[255, 255]);
        assert!(white[2] < 255);

        // 中间调：呈暖色（r > g > b），alpha 不变。
        let mut p = px(200, 180, 160, 255);
        apply_mode(&mut p, 1, 1, "sepia", None, None, 0, 0);
        assert!(p[0] > p[1] && p[1] > p[2], "sepia 应为暖色调: {p:?}");
        assert_eq!(p[3], 255);
    }

    #[test]
    fn add_saturates() {
        let mut p = px(250, 10, 0, 255);
        apply_mode(&mut p, 1, 1, "add", Some([20, 20, 20]), None, 0, 0);
        assert_eq!(p, px(255, 30, 20, 255));
    }

    #[test]
    fn multiply_scales_channels() {
        let mut p = px(255, 128, 0, 255);
        apply_mode(&mut p, 1, 1, "multiply", Some([128, 255, 255]), None, 0, 0);
        assert_eq!(p, px(128, 128, 0, 255));
    }

    #[test]
    fn blend_composites_at_offset_with_alpha() {
        // 2x1 目标：全红不透明。overlay 1x1 半透明绿，落在 x=1。
        let mut dst = vec![255, 0, 0, 255, 255, 0, 0, 255];
        let overlay = px(0, 255, 0, 128);
        apply_mode(&mut dst, 2, 1, "blend", None, Some((1, 1, &overlay)), 1, 0);
        // x=0 不变。
        assert_eq!(&dst[0..4], &[255, 0, 0, 255]);
        // x=1 是 50.2% 绿 over 红。
        let sa = 128.0f32 / 255.0;
        let expect_r = (255.0 * (1.0 - sa)).round() as u8;
        let expect_g = (255.0 * sa).round() as u8;
        assert_eq!(dst[4], expect_r);
        assert_eq!(dst[5], expect_g);
        assert_eq!(dst[7], 255);
    }

    #[test]
    fn blend_out_of_bounds_is_clipped() {
        let mut dst = px(1, 2, 3, 4);
        let overlay = px(9, 9, 9, 255);
        apply_mode(&mut dst, 1, 1, "blend", None, Some((1, 1, &overlay)), 5, 5);
        assert_eq!(dst, px(1, 2, 3, 4));
    }
}
