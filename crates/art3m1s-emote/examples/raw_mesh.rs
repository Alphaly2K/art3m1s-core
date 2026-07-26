//! 打印指定 motion/layer 每个关键帧的原始 blend_points 头部。
use art3m1s_emote::{EmoteLayer, EmoteModel};
use std::env;

fn main() {
    let path = env::args().nth(1).expect("model.psb");
    let motion_label = env::args().nth(2).expect("motion label");
    let layer_label = env::args().nth(3).expect("layer label");
    let model = EmoteModel::open(&path).expect("open");
    for (character, motions) in model.motions().characters() {
        for (label, motion) in motions {
            if label != &motion_label {
                continue;
            }
            for layer in &motion.layers {
                walk(layer, &layer_label, character, label);
            }
        }
    }
}

fn walk(layer: &EmoteLayer, needle: &str, character: &str, motion: &str) {
    if layer.label == needle {
        println!("{}/{} layer {}", character, motion, layer.label);
        for frame in &layer.frames {
            let head: Vec<String> = frame
                .content
                .as_ref()
                .and_then(|c| c.mesh.as_ref())
                .and_then(|m| m.blend_points.as_ref())
                .map(|p| p.iter().take(8).map(|v| format!("{v:.3}")).collect())
                .unwrap_or_default();
            println!("  t={} type={} raw_mesh[..8]={:?}", frame.time, frame.frame_type, head);
        }
    }
    for child in &layer.children {
        walk(child, needle, character, motion);
    }
}
