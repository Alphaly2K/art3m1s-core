use std::{collections::BTreeMap, path::PathBuf};

use art3m1s_emote::{EmoteModel, EmoteMotionEvaluator, EmoteRenderState};

#[test]
#[ignore = "requires the external nekomiko fixture"]
fn validates_nekomiko_motion_models() {
    let root = std::env::var_os("ART3M1S_FIXTURE_NEKOMIKO_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("ART3M1S_FIXTURES_DIR")
                .map(PathBuf::from)
                .map(|base| base.join("nekomiko"))
        })
        .expect(
            "set ART3M1S_FIXTURE_NEKOMIKO_DIR or ART3M1S_FIXTURES_DIR before running ignored compatibility tests",
        );
    assert!(
        root.join("system.ini").is_file(),
        "NekoMiko fixture at {} is not a project root",
        root.display()
    );
    let models = [
        "image/fhd/fg/aya/tay_0.psb",
        "image/fhd/fg/aya/tay_1.psb",
        "image/fhd/fg/kae/tka_0.psb",
        "image/fhd/fg/kae/tka_1.psb",
    ];

    for relative in models {
        let model = EmoteModel::open(root.join(relative)).unwrap();
        assert_eq!(model.info().type_id.as_deref(), Some("motion"));
        assert_eq!(model.info().screen_width, 800);
        assert_eq!(model.info().screen_height, 1080);
        assert!(model.timelines().contains_key("通常待機"));
        assert!(model.timelines().contains_key("笑顔_ボイス再生用"));
        assert!(model.variables().contains_key("face_talk"));
        assert_eq!(
            model.atlas().textures().len(),
            model.document().resource_count()
        );

        let draws = EmoteMotionEvaluator::new(&model)
            .evaluate_base(&EmoteRenderState::default())
            .unwrap();
        assert!(!draws.is_empty());
        assert!(draws.iter().all(|draw| draw.opacity.is_finite()));
        assert!(draws.iter().all(|draw| {
            draw.mesh
                .as_ref()
                .and_then(|mesh| mesh.blend_points.as_ref())
                .into_iter()
                .flatten()
                .all(|value| value.is_finite())
        }));
        assert_eq!(model.selectors().len(), 1);
        assert_eq!(model.selectors()[0].label, "arm_type");
        assert_eq!(model.selectors()[0].options.len(), 5);
        assert_eq!(model.eye_controls().len(), 1);
        let eye = &model.eye_controls()[0];
        assert_eq!(eye.label, "face_eye_open");
        assert_eq!(eye.blink_frame_count, 16.0);
        assert_eq!(eye.blink_value(0.0, 0.0), Some(0.0));
        assert_eq!(eye.blink_value(0.0, 0.5), Some(10.0));
        assert_eq!(eye.blink_value(0.0, 1.0), Some(0.0));

        let eye_open = EmoteMotionEvaluator::new(&model)
            .evaluate_base(&EmoteRenderState {
                motion_time: 0.0,
                variables: BTreeMap::from([("face_eye_open".to_string(), 0.0)]),
            })
            .unwrap();
        let eye_closed = EmoteMotionEvaluator::new(&model)
            .evaluate_base(&EmoteRenderState {
                motion_time: 0.0,
                variables: BTreeMap::from([("face_eye_open".to_string(), 10.0)]),
            })
            .unwrap();
        assert!(eye_open.iter().zip(&eye_closed).any(|(open, closed)| {
            open.layer_label == "mabuta" && open.icon_id != closed.icon_id
        }));

        let talking = EmoteMotionEvaluator::new(&model)
            .evaluate_base(&EmoteRenderState {
                motion_time: 0.0,
                variables: BTreeMap::from([("face_talk".to_string(), 4.0)]),
            })
            .unwrap();
        assert!(draws.iter().zip(&talking).any(|(closed, open)| {
            closed.icon_id != open.icon_id
                || closed.translation != open.translation
                || closed.opacity != open.opacity
                || closed
                    .mesh
                    .as_ref()
                    .and_then(|mesh| mesh.blend_points.as_ref())
                    != open
                        .mesh
                        .as_ref()
                        .and_then(|mesh| mesh.blend_points.as_ref())
        }));

        let body_motion = model
            .motions()
            .motion("body_parts", "全身変形基礎")
            .unwrap();
        let body_ud = body_motion
            .parameters
            .iter()
            .find(|parameter| parameter.id == "body_UD")
            .unwrap();
        assert_eq!((body_ud.range_begin, body_ud.range_end), (-30.0, 30.0));
        assert_eq!(body_ud.frame_for_value(0.0), Some(30.0));

        let idle_body_ud = model.timelines()["通常待機"]
            .tracks
            .iter()
            .find(|track| track.label == "body_UD")
            .unwrap();
        assert!(idle_body_ud.frames.iter().any(|frame| frame.value < 0.0));

        let base = &model.timelines()["笑顔_ボイス再生用"];
        let idle = &model.timelines()["通常待機"];
        let render_at = |frame: f32| {
            let mut variables = base.sample(frame);
            for (label, value) in idle.sample(frame) {
                *variables.entry(label).or_default() += value;
            }
            EmoteMotionEvaluator::new(&model)
                .evaluate_base(&EmoteRenderState {
                    motion_time: 0.0,
                    variables,
                })
                .unwrap()
        };
        let earlier = render_at(5.0);
        let later = render_at(10.0);
        let visible_fades = earlier
            .iter()
            .filter(|draw| draw.layer_label.contains("_fade_") && draw.opacity > f32::EPSILON)
            .map(|draw| draw.layer_label.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!visible_fades.is_empty());
        assert!(visible_fades.len() <= 2);
        assert!(earlier.iter().zip(later).any(|(earlier, later)| {
            earlier.translation != later.translation
                || earlier.angle != later.angle
                || earlier
                    .mesh
                    .as_ref()
                    .and_then(|mesh| mesh.blend_points.as_ref())
                    != later
                        .mesh
                        .as_ref()
                        .and_then(|mesh| mesh.blend_points.as_ref())
        }));
    }
}
