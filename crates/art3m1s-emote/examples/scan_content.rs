use std::collections::{BTreeMap, BTreeSet};
use std::env;

use art3m1s_emote::{EmoteModel, PsbValue};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example scan_content -- <model.psb>");
        std::process::exit(2);
    });
    let model = EmoteModel::open(&path).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });

    let mut key_sets = BTreeMap::<Vec<String>, usize>::new();
    let mut sources = BTreeSet::new();
    scan(&model.document().root, &mut key_sets, &mut sources, false);

    println!("frame content key sets:");
    for (keys, count) in key_sets {
        println!("  {count:6} {}", keys.join(","));
    }
    println!("sources ({}): {:?}", sources.len(), sources);
    scan_mesh_shapes(&model);
    scan_coordinate_modes(&model);
    scan_mask_values(&model);
    scan_wrapped_transform_values(&model);
}

fn scan_wrapped_transform_values(model: &EmoteModel) {
    let mut values = BTreeMap::<String, BTreeSet<i64>>::new();
    scan_transform_values(&model.document().root, None, &mut values);
    for key in ["angle", "coord", "timeOffset"] {
        let wrapped = values
            .get(key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|value| (128..=255).contains(value))
            .collect::<Vec<_>>();
        println!("{key} possible wrapped i8 values: {wrapped:?}");
    }
}

fn scan_transform_values(
    value: &PsbValue,
    key: Option<&str>,
    values: &mut BTreeMap<String, BTreeSet<i64>>,
) {
    match value {
        PsbValue::Integer(number)
            if matches!(key, Some("angle") | Some("coord") | Some("timeOffset")) =>
        {
            values
                .entry(key.unwrap().to_owned())
                .or_default()
                .insert(*number);
        }
        PsbValue::Object(object) => {
            for (child_key, child) in object {
                scan_transform_values(child, Some(child_key), values);
            }
        }
        PsbValue::List(list) => {
            for child in list {
                scan_transform_values(child, key, values);
            }
        }
        _ => {}
    }
}

fn scan_mask_values(model: &EmoteModel) {
    let mut masks = BTreeMap::<i64, usize>::new();
    for motions in model.motions().characters().values() {
        for motion in motions.values() {
            for layer in &motion.layers {
                scan_layer_masks(layer, &mut masks);
            }
        }
    }
    println!("content masks: {masks:?}");
}

fn scan_layer_masks(layer: &art3m1s_emote::EmoteLayer, masks: &mut BTreeMap<i64, usize>) {
    for frame in &layer.frames {
        if let Some(content) = &frame.content {
            *masks.entry(content.mask).or_default() += 1;
        }
    }
    for child in &layer.children {
        scan_layer_masks(child, masks);
    }
}

fn scan_coordinate_modes(model: &EmoteModel) {
    let mut modes = BTreeMap::<i64, (usize, f32)>::new();
    for motions in model.motions().characters().values() {
        for value in motions.values() {
            for layer in &value.layers {
                scan_layer_coordinates(layer, &mut modes);
            }
        }
    }
    println!("coordinate modes (layers, max |coord|): {modes:?}");
}

fn scan_layer_coordinates(
    layer: &art3m1s_emote::EmoteLayer,
    modes: &mut BTreeMap<i64, (usize, f32)>,
) {
    let entry = modes.entry(layer.coordinate).or_default();
    entry.0 += 1;
    for frame in &layer.frames {
        if let Some(coord) = frame
            .content
            .as_ref()
            .and_then(|content| content.coord.as_deref())
        {
            entry.1 = coord
                .iter()
                .fold(entry.1, |max, value| max.max(value.abs()));
        }
    }
    for child in &layer.children {
        scan_layer_coordinates(child, modes);
    }
}

fn scan(
    value: &PsbValue,
    key_sets: &mut BTreeMap<Vec<String>, usize>,
    sources: &mut BTreeSet<String>,
    is_frame: bool,
) {
    match value {
        PsbValue::Object(object) => {
            if is_frame && let Some(content) = object.get("content").and_then(PsbValue::as_object) {
                *key_sets
                    .entry(content.keys().cloned().collect())
                    .or_default() += 1;
                if let Some(source) = content.get("src").and_then(PsbValue::as_str) {
                    sources.insert(source.to_owned());
                }
            }
            for (key, child) in object {
                scan(child, key_sets, sources, key == "frameList");
            }
        }
        PsbValue::List(list) => {
            for child in list {
                scan(child, key_sets, sources, is_frame);
            }
        }
        _ => {}
    }
}

fn scan_mesh_shapes(model: &EmoteModel) {
    let mut shapes = BTreeMap::<(usize, usize), usize>::new();
    let mut first = None;
    for motions in model.motions().characters().values() {
        for motion in motions.values() {
            for layer in &motion.layers {
                scan_layer_meshes(layer, &mut shapes, &mut first);
            }
        }
    }
    println!("mesh shapes (bp floats, cc floats):");
    for (shape, count) in shapes {
        println!("  {count:6} {:?}", shape);
    }
    if let Some((bp, cc)) = first {
        println!("first mesh bp={bp:?}");
        println!("first mesh cc={cc:?}");
    }
}

fn scan_layer_meshes(
    layer: &art3m1s_emote::EmoteLayer,
    shapes: &mut BTreeMap<(usize, usize), usize>,
    first: &mut Option<(Vec<f32>, Vec<f32>)>,
) {
    for frame in &layer.frames {
        let Some(mesh) = frame
            .content
            .as_ref()
            .and_then(|content| content.mesh.as_ref())
        else {
            continue;
        };
        let bp = mesh.blend_points.as_deref().unwrap_or_default();
        let cc = mesh.control_coordinates.as_deref().unwrap_or_default();
        *shapes.entry((bp.len(), cc.len())).or_default() += 1;
        if first.is_none() {
            *first = Some((bp.to_vec(), cc.to_vec()));
        }
    }
    for child in &layer.children {
        scan_layer_meshes(child, shapes, first);
    }
}
