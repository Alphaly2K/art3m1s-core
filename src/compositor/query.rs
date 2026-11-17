use std::collections::HashMap;

/// 单个图层暴露给脚本层的属性，数值使用 Artemis 原始刻度。
pub(crate) fn layer_info_entry(
    layer: &crate::compositor::Layer,
    now_ms: u64,
    texture_info: Option<crate::compositor::TextureInfo>,
) -> HashMap<String, String> {
    // get_layer_info 查询的是画面当前状态。缓动期间若仍返回静态 props，
    // 脚本会在图层已经移动到中央时读到旧坐标，进而走错展开/收回分支。
    let props = crate::compositor::build::resolved_props(layer, now_ms);
    let (left, top) = props.offset();
    let clip = props.clip_rect();
    let width = props
        .width
        .or_else(|| clip.map(|rect| rect[2]))
        .or_else(|| texture_info.map(|info| info.width as f32))
        .unwrap_or(0.0);
    let height = props
        .height
        .or_else(|| clip.map(|rect| rect[3]))
        .or_else(|| texture_info.map(|info| info.height as f32))
        .unwrap_or(0.0);
    let mut info = props.custom.clone();
    info.extend([
        ("left".to_string(), trim_layer_float(left)),
        ("top".to_string(), trim_layer_float(top)),
        ("width".to_string(), trim_layer_float(width)),
        ("height".to_string(), trim_layer_float(height)),
        ("visible".into(), u8::from(props.is_visible()).to_string()),
        ("alpha".into(), props.alpha.unwrap_or(255).to_string()),
        (
            "anchorx".into(),
            trim_layer_float(props.anchor_x.unwrap_or(0.0)),
        ),
        (
            "anchory".into(),
            trim_layer_float(props.anchor_y.unwrap_or(0.0)),
        ),
        (
            "xscale".into(),
            trim_layer_float(props.x_scale.unwrap_or(100.0)),
        ),
        (
            "yscale".into(),
            trim_layer_float(props.y_scale.unwrap_or(100.0)),
        ),
        (
            "rotate".into(),
            trim_layer_float(props.rotate.unwrap_or(0.0)),
        ),
        (
            "reversex".into(),
            u8::from(props.reverse_x.unwrap_or(false)).to_string(),
        ),
        (
            "reversey".into(),
            u8::from(props.reverse_y.unwrap_or(false)).to_string(),
        ),
        (
            "grayscale".into(),
            u8::from(props.grayscale.unwrap_or(false)).to_string(),
        ),
        (
            "negative".into(),
            u8::from(props.negative.unwrap_or(false)).to_string(),
        ),
    ]);
    info
}

fn trim_layer_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        (value as i32).to_string()
    } else {
        value.to_string()
    }
}
