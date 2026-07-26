use std::env;

use art3m1s_emote::{EmoteModel, EmoteMotionEvaluator, EmoteRenderState, PsbValue};

fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: cargo run --example inspect -- <model.psb> [tree-path] [depth]");
        std::process::exit(2);
    });
    let tree_path = args.next();
    let tree_depth = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4);

    match EmoteModel::open(&path) {
        Ok(model) => {
            let document = model.document();
            let info = model.info();
            println!("path: {path}");
            println!(
                "psb: v{} key={:?} resources={} extra_resources={}",
                document.header.version,
                document.header.header_key,
                document.resource_count(),
                document.extra_resource_count()
            );
            println!("type: {:?} spec: {:?}", info.type_id, info.spec);
            println!(
                "base: chara={:?} motion={:?}",
                info.base_chara, info.base_motion
            );
            println!(
                "screen: {}x{} textures={} icons={}",
                info.screen_width, info.screen_height, info.texture_count, info.icon_count
            );
            println!(
                "characters ({}): {:?}",
                info.characters.len(),
                info.characters
            );
            println!("motions ({}): {:?}", info.motions.len(), info.motions);
            println!("timelines ({}): {:?}", info.timelines.len(), info.timelines);
            println!("variables ({}): {:?}", info.variables.len(), info.variables);
            let track_count = model
                .timelines()
                .values()
                .map(|timeline| timeline.tracks.len())
                .sum::<usize>();
            let keyframe_count = model
                .timelines()
                .values()
                .flat_map(|timeline| &timeline.tracks)
                .map(|track| track.frames.len())
                .sum::<usize>();
            println!("timeline tracks={track_count} keyframes={keyframe_count}");
            println!(
                "timeline modes: {:?}",
                model
                    .timelines()
                    .values()
                    .map(|timeline| (
                        timeline.label.as_str(),
                        timeline.diff,
                        timeline.last_time,
                        timeline.loop_begin,
                        timeline.loop_end,
                    ))
                    .collect::<Vec<_>>()
            );
            println!(
                "motion graph: motions={} layers={} frames={}",
                model.motions().motion_count(),
                model.motions().layer_count(),
                model.motions().frame_count()
            );
            match EmoteMotionEvaluator::new(&model).evaluate_base(&EmoteRenderState::default()) {
                Ok(items) => {
                    println!("base draw items={}", items.len());
                    let bounds = items.iter().fold(
                        [
                            f32::INFINITY,
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                            f32::NEG_INFINITY,
                        ],
                        |mut bounds, item| {
                            bounds[0] = bounds[0].min(item.translation[0] - item.origin[0]);
                            bounds[1] = bounds[1].min(item.translation[1] - item.origin[1]);
                            bounds[2] = bounds[2]
                                .max(item.translation[0] - item.origin[0] + item.atlas_rect[2]);
                            bounds[3] = bounds[3]
                                .max(item.translation[1] - item.origin[1] + item.atlas_rect[3]);
                            bounds
                        },
                    );
                    println!("base approximate bounds={bounds:?}");
                    if let Some(item) = items
                        .iter()
                        .max_by(|a, b| a.translation[1].total_cmp(&b.translation[1]))
                    {
                        println!(
                            "max-y item layer={} icon={} translation={:?} origin={:?} rect={:?}",
                            item.layer_label,
                            item.icon_id,
                            item.translation,
                            item.origin,
                            item.atlas_rect
                        );
                    }
                }
                Err(error) => println!("base draw evaluation failed: {error}"),
            }
            let evaluator = EmoteMotionEvaluator::new(&model);
            println!("base motion samples (frame: draw/visible):");
            for frame in [0.0, 1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 254.0, 255.0, 300.0] {
                let state = EmoteRenderState {
                    motion_time: frame,
                    ..EmoteRenderState::default()
                };
                match evaluator.evaluate_base(&state) {
                    Ok(items) => {
                        let visible = items.iter().filter(|item| item.opacity > 0.001).count();
                        println!("  {frame:>6.1}: {}/{visible}", items.len());
                    }
                    Err(error) => println!("  {frame:>6.1}: error: {error}"),
                }
            }
            if let Some(tree_path) = tree_path {
                match select_path(&document.root, &tree_path) {
                    Some(value) => print_tree(value, 0, tree_depth),
                    None => {
                        eprintln!("tree path not found: {tree_path}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn select_path<'a>(root: &'a PsbValue, path: &str) -> Option<&'a PsbValue> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .try_fold(root, |value, segment| match value {
            PsbValue::Object(object) => object.get(segment),
            PsbValue::List(list) => segment
                .parse::<usize>()
                .ok()
                .and_then(|index| list.get(index)),
            _ => None,
        })
}

fn print_tree(value: &PsbValue, indent: usize, remaining_depth: usize) {
    let prefix = "  ".repeat(indent);
    match value {
        PsbValue::Object(object) => {
            println!("{prefix}object({})", object.len());
            if remaining_depth == 0 {
                return;
            }
            for (key, child) in object {
                print!("{prefix}  {key}: ");
                print_tree(child, indent + 1, remaining_depth - 1);
            }
        }
        PsbValue::List(list) => {
            println!("{prefix}list({})", list.len());
            if remaining_depth == 0 {
                return;
            }
            for (index, child) in list.iter().take(3).enumerate() {
                print!("{prefix}  [{index}]: ");
                print_tree(child, indent + 1, remaining_depth - 1);
            }
            if list.len() > 3 {
                println!("{prefix}  ...");
            }
        }
        PsbValue::Array(values) => {
            println!(
                "{prefix}array({}) {:?}",
                values.len(),
                &values[..values.len().min(8)]
            );
        }
        PsbValue::String(value) => println!("{prefix}{value:?}"),
        PsbValue::Resource(value) => println!("{prefix}{value:?}"),
        other => println!("{prefix}{other:?}"),
    }
}
