//! 图层属性。
//!
//! 解释器把 `[lyc2]` / `[lyprop]` 解析成 `LayerEvent`，其中属性是一个
//! `HashMap<String, String>`（原始字符串），而不是强类型结构。Artemis 脚本里
//! 用到的属性远比解释器的 `LayerProperties` 多（`anchorx` / `xscale` / `clip` /
//! `colormultiply` / `layermode` …），所以合成器自己解析成 typed 字段，并把无法
//! 识别的长尾属性原样保留在 `custom` 里，留给后续后端按需消费。

use std::collections::HashMap;

/// 图层的可视属性。
///
/// 所有字段都是 `Option`：`None` 表示"本次没有提供"，用于增量合并（`[lyprop]`
/// 通常只设置改动的几个属性）。坐标/缩放沿用 Artemis 的约定：
/// - `left` / `top`：相对父图层的像素偏移。
/// - `xscale` / `yscale`：百分比，`100` 表示 1.0 倍。
/// - `alpha`：0-255。
/// - `anchorx` / `anchory`：变换锚点（缩放/旋转的中心），像素。
/// - `rotate`：角度，单位度。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerProps {
    pub left: Option<f32>,
    pub top: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub anchor_x: Option<f32>,
    pub anchor_y: Option<f32>,
    pub x_scale: Option<f32>,
    pub y_scale: Option<f32>,
    pub rotate: Option<f32>,
    pub alpha: Option<u8>,
    pub visible: Option<bool>,
    pub reverse_x: Option<bool>,
    pub reverse_y: Option<bool>,
    pub grayscale: Option<bool>,
    pub negative: Option<bool>,
    /// 颜色乘算，形如 `"255,128,128"`，解析为归一化 RGB 三元组。
    pub color_multiply: Option<[f32; 3]>,
    /// 合成/混合模式（`add` / `alpha` / `screen` …），原样保留交给后端解释。
    pub layer_mode: Option<String>,
    /// 精灵裁剪矩形 `[x, y, w, h]`（纹理像素坐标）。用于从一张精灵图集里
    /// 取出子区域显示——标题菜单的多个按钮就共用一张 `btn.png`，靠 `clip` 区分。
    /// 注意：若同时设置了 `intermediate_render`，clip 应被忽略（见 `clip_rect()`）。
    pub clip: Option<[f32; 4]>,
    /// Artemis 离屏中间渲染标记。设置后该图层作为渲染目标，`clip` 为目标尺寸而
    /// 非可视裁剪，不应影响纹理的采样区域。
    pub intermediate_render: Option<u8>,
    /// 未被识别的属性，原样保留。
    pub custom: HashMap<String, String>,
}

impl LayerProps {
    /// 从解释器传来的原始属性 map 解析出 typed 属性。
    pub fn from_raw(raw: &HashMap<String, String>) -> Self {
        let mut props = LayerProps::default();
        props.merge_raw(raw);
        props
    }

    /// 把一批原始属性合并进来：已识别的写入 typed 字段，其余进 `custom`。
    /// 只有出现在 `raw` 中的键会被改动，未出现的保持原值（增量语义）。
    pub fn merge_raw(&mut self, raw: &HashMap<String, String>) {
        for (key, value) in raw {
            self.set_raw(key, value);
        }
    }

    /// 设置单个原始属性。无法解析为目标类型时回退到 `custom`，避免静默丢弃。
    pub fn set_raw(&mut self, key: &str, value: &str) {
        let v = value.trim();
        match key {
            "left" | "x" => self.left = parse_f32(v).or(self.left),
            "top" | "y" => self.top = parse_f32(v).or(self.top),
            "width" => self.width = parse_f32(v).or(self.width),
            "height" => self.height = parse_f32(v).or(self.height),
            "anchorx" => self.anchor_x = parse_f32(v).or(self.anchor_x),
            "anchory" => self.anchor_y = parse_f32(v).or(self.anchor_y),
            "xscale" => self.x_scale = parse_f32(v).or(self.x_scale),
            "yscale" => self.y_scale = parse_f32(v).or(self.y_scale),
            // `zoom` 是 x/y 同时缩放的简写。
            "zoom" => {
                if let Some(z) = parse_f32(v) {
                    self.x_scale = Some(z);
                    self.y_scale = Some(z);
                }
            }
            "rotate" => self.rotate = parse_f32(v).or(self.rotate),
            "alpha" => self.alpha = parse_u8(v).or(self.alpha),
            "visible" => self.visible = parse_bool(v).or(self.visible),
            "reversex" => self.reverse_x = parse_bool(v).or(self.reverse_x),
            "reversey" => self.reverse_y = parse_bool(v).or(self.reverse_y),
            "grayscale" => self.grayscale = parse_bool(v).or(self.grayscale),
            "negative" => self.negative = parse_bool(v).or(self.negative),
            "colormultiply" => self.color_multiply = parse_rgb(v).or(self.color_multiply),
            "layermode" => self.layer_mode = Some(v.to_string()),
            "clip" => self.clip = parse_clip(v).or(self.clip),
            // intermediate_render 是离屏中间渲染标记，记录下来以便 clip_rect() 能
            // 正确忽略同组的 clip（那是渲染目标尺寸，不是可视裁剪）。
            "intermediate_render" => {
                self.intermediate_render = v.parse::<u8>().ok().or(self.intermediate_render);
            }
            // file 属性在 SetProperties 事件里偶尔出现，但图层文件已在 Create 时确定，忽略。
            "file" => {}
            _ => {
                self.custom.insert(key.to_string(), value.to_string());
            }
        }
    }

    // ── 带默认值的取值器，供帧构建使用 ──────────────────────────────

    pub fn offset(&self) -> (f32, f32) {
        (self.left.unwrap_or(0.0), self.top.unwrap_or(0.0))
    }

    /// 缩放因子（已从百分比换算为倍率），并应用 reverse 翻转的符号。
    pub fn scale(&self) -> (f32, f32) {
        let mut sx = self.x_scale.unwrap_or(100.0) / 100.0;
        let mut sy = self.y_scale.unwrap_or(100.0) / 100.0;
        if self.reverse_x == Some(true) {
            sx = -sx;
        }
        if self.reverse_y == Some(true) {
            sy = -sy;
        }
        (sx, sy)
    }

    pub fn anchor(&self) -> (f32, f32) {
        (self.anchor_x.unwrap_or(0.0), self.anchor_y.unwrap_or(0.0))
    }

    pub fn rotation_radians(&self) -> f32 {
        self.rotate.unwrap_or(0.0).to_radians()
    }

    /// 精灵裁剪矩形 `[x, y, w, h]`（纹理像素），未设置时返回 `None`（画整张）。
    pub fn clip_rect(&self) -> Option<[f32; 4]> {
        // intermediate_render 图层的 clip 是渲染目标尺寸，不是可视裁剪，忽略。
        if self.intermediate_render.is_some() {
            return None;
        }
        self.clip
    }

    /// 本图层自身的不透明度，归一化到 0.0-1.0。
    pub fn opacity(&self) -> f32 {
        self.alpha.unwrap_or(255) as f32 / 255.0
    }

    /// 是否显式隐藏（默认可见）。
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }
}

fn parse_f32(value: &str) -> Option<f32> {
    value.parse().ok()
}

fn parse_u8(value: &str) -> Option<u8> {
    // alpha 偶尔写成浮点（如 "255.0"），先按整数再回退浮点。
    value
        .parse::<u8>()
        .ok()
        .or_else(|| value.parse::<f32>().ok().map(|f| f.clamp(0.0, 255.0) as u8))
}

/// Artemis 的布尔属性用 `"1"` / `"0"`，也兼容 `on`/`off`/`true`/`false`。
fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// 解析 `"r,g,b"`（0-255）为归一化 RGB。
fn parse_rgb(value: &str) -> Option<[f32; 3]> {
    let mut it = value.split(',').map(|c| c.trim().parse::<f32>());
    let r = it.next()?.ok()?;
    let g = it.next()?.ok()?;
    let b = it.next()?.ok()?;
    Some([r / 255.0, g / 255.0, b / 255.0])
}

/// 解析 `"x,y,w,h"`（纹理像素）为裁剪矩形。四个分量缺一不可。
fn parse_clip(value: &str) -> Option<[f32; 4]> {
    let mut it = value.split(',').map(|c| c.trim().parse::<f32>());
    let x = it.next()?.ok()?;
    let y = it.next()?.ok()?;
    let w = it.next()?.ok()?;
    let h = it.next()?.ok()?;
    Some([x, y, w, h])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_known_properties() {
        let props = LayerProps::from_raw(&raw(&[
            ("left", "100"),
            ("top", "-50"),
            ("xscale", "200"),
            ("yscale", "50"),
            ("alpha", "128"),
            ("visible", "1"),
            ("rotate", "90"),
        ]));

        assert_eq!(props.offset(), (100.0, -50.0));
        assert_eq!(props.scale(), (2.0, 0.5));
        assert!((props.opacity() - 128.0 / 255.0).abs() < 1e-6);
        assert!(props.is_visible());
        assert!((props.rotation_radians() - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn merge_is_incremental() {
        let mut props = LayerProps::from_raw(&raw(&[("left", "10"), ("alpha", "255")]));
        props.merge_raw(&raw(&[("alpha", "0")]));

        // left 未在第二批出现，应保持不变。
        assert_eq!(props.left, Some(10.0));
        assert_eq!(props.alpha, Some(0));
    }

    #[test]
    fn unknown_properties_go_to_custom() {
        let props = LayerProps::from_raw(&raw(&[("clip", "0,0,100,100"), ("draggable", "1")]));
        // clip 是已识别属性，应被解析到 typed 字段。
        assert_eq!(props.clip, Some([0.0, 0.0, 100.0, 100.0]));
        // draggable 是未识别属性，应留在 custom 里。
        assert_eq!(props.custom.get("draggable").map(String::as_str), Some("1"));
        // clip 不应出现在 custom 里。
        assert_eq!(props.custom.get("clip"), None);
    }

    #[test]
    fn zoom_sets_both_scales() {
        let props = LayerProps::from_raw(&raw(&[("zoom", "150")]));
        assert_eq!(props.scale(), (1.5, 1.5));
    }

    #[test]
    fn reverse_flips_scale_sign() {
        let props = LayerProps::from_raw(&raw(&[("reversex", "1")]));
        let (sx, sy) = props.scale();
        assert_eq!(sx, -1.0);
        assert_eq!(sy, 1.0);
    }

    #[test]
    fn color_multiply_normalizes() {
        let props = LayerProps::from_raw(&raw(&[("colormultiply", "255,128,0")]));
        let c = props.color_multiply.unwrap();
        assert!((c[0] - 1.0).abs() < 1e-6);
        assert!((c[1] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(c[2], 0.0);
    }
}
