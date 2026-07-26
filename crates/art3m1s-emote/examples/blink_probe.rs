//! 空跑渲染：在不同 face_eye_open 值下枚举眼睛相关 draw items。
//! 用法: cargo run --example blink_probe -- <model.psb>

use std::collections::BTreeMap;
use std::env;

use art3m1s_emote::{EmoteModel, EmoteMotionEvaluator, EmoteRenderState};

fn main() {
    let path = env::args().nth(1).expect("usage: blink_probe <model.psb>");
    let model = EmoteModel::open(&path).expect("open model");
    let evaluator = EmoteMotionEvaluator::new(&model);

    for value in [5.0f32, 7.0, 9.0, 9.9] {
        let mut variables = BTreeMap::new();
        variables.insert("face_eye_open".to_string(), value);
        let state = EmoteRenderState {
            motion_time: 0.0,
            variables,
        };
        let items = evaluator.evaluate_base(&state).expect("evaluate");
        println!("== face_eye_open = {value} ({} items total) ==", items.len());
        for item in &items {
            let label = &item.layer_label;
            if label.contains("目")
                || label.contains("mabuta")
                || label.contains("shirome")
                || label.contains("瞳")
                || label.contains("eye")
                || label.contains("matsuge")
                || label.contains("睫")
                || label.contains("mask")
            {
                let mesh_head: Vec<String> = item
                    .mesh
                    .as_ref()
                    .and_then(|m| m.blend_points.as_ref())
                    .map(|p| p.iter().take(6).map(|v| format!("{v:.3}")).collect())
                    .unwrap_or_default();
                println!(
                    "  {} icon={} pos=({:.1},{:.1}) mesh[..6]={:?}",
                    label, item.icon_id, item.translation[0], item.translation[1], mesh_head,
                );
            }
        }
    }
}
