//! 临时调试工具：转储眼睛控制、参数映射与眼睛相关图层的关键帧结构。
//! 用法: cargo run --example eye_debug -- <model.psb>

use std::env;

use art3m1s_emote::{EmoteLayer, EmoteModel};

fn main() {
    let path = env::args().nth(1).expect("usage: eye_debug <model.psb>");
    let model = EmoteModel::open(&path).expect("open model");

    println!("== eye controls ==");
    for control in model.eye_controls() {
        println!(
            "label={} enabled={} blink_enabled={} frames={} interval=[{},{}] begin={} end={}",
            control.label,
            control.enabled,
            control.blink_enabled,
            control.blink_frame_count,
            control.blink_interval_min,
            control.blink_interval_max,
            control.begin_frame,
            control.end_frame,
        );
        println!("  edges={:?}", control.edges);
        println!("  nodes={:?}", control.nodes);
    }

    let eye_labels: Vec<String> = model
        .eye_controls()
        .iter()
        .map(|control| control.label.clone())
        .collect();

    println!("\n== motions & parameters ==");
    for (character, motions) in model.motions().characters() {
        for (label, motion) in motions {
            let has_eye_parameter = motion
                .parameters
                .iter()
                .any(|parameter| eye_labels.contains(&parameter.id));
            if !has_eye_parameter && !motion.parameters.is_empty() {
                continue;
            }
            if motion.parameters.is_empty() {
                continue;
            }
            println!("motion {character}/{label} last={} loop={}", motion.last_time, motion.loop_time);
            for (index, parameter) in motion.parameters.iter().enumerate() {
                println!(
                    "  param[{index}] id={} range=[{},{}] division={} enabled={} discretization={}",
                    parameter.id,
                    parameter.range_begin,
                    parameter.range_end,
                    parameter.division,
                    parameter.enabled,
                    parameter.discretization,
                );
            }
            for layer in &motion.layers {
                dump_layer(layer, 1);
            }
        }
    }

    println!("\n== refs to 目 ==");
    find_motion_refs(&model, "目");
}

fn dump_layer(layer: &EmoteLayer, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}layer {} type={} param_idx={:?} frames={}",
        layer.label,
        layer.layer_type,
        layer.parameter_index,
        layer.frames.len(),
    );
    for frame in &layer.frames {
        let (source, icon, opacity, has_mesh, has_motion) = match &frame.content {
            Some(content) => (
                content.source.clone().unwrap_or_default(),
                content.icon.clone().unwrap_or_default(),
                content
                    .opacity
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".into()),
                content.mesh.is_some(),
                content.motion.is_some(),
            ),
            None => ("<none>".into(), String::new(), "-".into(), false, false),
        };
        println!(
            "{indent}  t={:>5} type={} opacity={:>5} mesh={} motion={} src={} icon={}",
            frame.time, frame.frame_type, opacity, has_mesh, has_motion, source, icon,
        );
    }
    for child in &layer.children {
        dump_layer(child, depth + 1);
    }
}

// 第二遍：找出所有含 motion 引用的图层（谁引用了眼睛子动作）。
fn find_motion_refs(model: &EmoteModel, needle: &str) {
    for (character, motions) in model.motions().characters() {
        for (label, motion) in motions {
            for layer in &motion.layers {
                walk_refs(layer, character, label, needle, &mut Vec::new());
            }
        }
    }
}

fn walk_refs(
    layer: &EmoteLayer,
    character: &str,
    motion_label: &str,
    needle: &str,
    path: &mut Vec<String>,
) {
    path.push(layer.label.clone());
    let mut hits = Vec::new();
    for frame in &layer.frames {
        if let Some(content) = &frame.content {
            if content.motion.is_some() {
                let target = content.icon.clone().unwrap_or_default();
                if target.contains(needle) {
                    hits.push(format!(
                        "t={} type={} -> {}/{} offset={}",
                        frame.time,
                        frame.frame_type,
                        content.source.clone().unwrap_or_default(),
                        target,
                        content.motion.as_ref().unwrap().time_offset,
                    ));
                }
            }
        }
    }
    if !hits.is_empty() {
        println!(
            "ref in {}/{} :: {} (param_idx={:?}, frames={})",
            character,
            motion_label,
            path.join(" > "),
            layer.parameter_index,
            layer.frames.len(),
        );
        for frame in &layer.frames {
            let desc = match &frame.content {
                Some(c) => format!(
                    "src={} icon={} motion={} opacity={:?}",
                    c.source.clone().unwrap_or_default(),
                    c.icon.clone().unwrap_or_default(),
                    c.motion.is_some(),
                    c.opacity,
                ),
                None => "<none>".into(),
            };
            println!("    t={:>5} type={} {}", frame.time, frame.frame_type, desc);
        }
    }
    for child in &layer.children {
        walk_refs(child, character, motion_label, needle, path);
    }
    path.pop();
}
