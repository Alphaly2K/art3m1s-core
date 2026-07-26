//! 转储 頭部変形基礎 的参数与 目L/目R 链路各层的关键帧+网格头部。
use art3m1s_emote::{EmoteLayer, EmoteModel};
use std::env;

fn main() {
    let path = env::args().nth(1).expect("model.psb");
    let model = EmoteModel::open(&path).expect("open");
    for (character, motions) in model.motions().characters() {
        for (label, motion) in motions {
            if !label.contains("頭部変形基礎") {
                continue;
            }
            println!("motion {character}/{label}");
            for (i, p) in motion.parameters.iter().enumerate() {
                println!(
                    "  param[{i}] id={} range=[{},{}] div={} disc={}",
                    p.id, p.range_begin, p.range_end, p.division, p.discretization
                );
            }
            for layer in &motion.layers {
                walk(layer, 1);
            }
        }
    }
}

fn walk(layer: &EmoteLayer, depth: usize) {
    if true {
        let indent = "  ".repeat(depth);
        println!(
            "{indent}{} param_idx={:?} inherit_shape={} mesh_combine={} frames={}",
            layer.label, layer.parameter_index, layer.inherit_shape, layer.mesh_combine,
            layer.frames.len()
        );
        for frame in layer.frames.iter().take(4) {
            let mesh_head: Vec<String> = frame
                .content
                .as_ref()
                .and_then(|c| c.mesh.as_ref())
                .and_then(|m| m.blend_points.as_ref())
                .map(|p| p.iter().take(4).map(|v| format!("{v:.3}")).collect())
                .unwrap_or_default();
            let coord: Vec<String> = frame
                .content
                .as_ref()
                .and_then(|c| c.coord.as_ref())
                .map(|c| c.iter().map(|v| format!("{v:.1}")).collect())
                .unwrap_or_default();
            println!(
                "{indent}  t={} type={} coord={:?} mesh[..4]={:?}",
                frame.time, frame.frame_type, coord, mesh_head
            );
        }
    }
    for child in &layer.children {
        walk(child, depth + 1);
    }
}
