use super::*;
use crate::host::frame::{FrameGroup, FrameShader, FrameTexture, FrameTransition};
use art3m1s_render::draw::NativeEmoteMaterial;
use std::sync::Arc;

fn frame() -> FrameSnapshot {
    let shader = Arc::new(FrameShader {
        revision: 19,
        source: Box::from(&b"float opacity;"[..]),
    });
    let effect = FrameShaderEffect {
        shader,
        uniforms: [("opacity".into(), vec![0.5])].into(),
        mask: Some(1),
        user: Some(0),
    };
    let sprite = FrameSprite {
        layer_id: "1.80".into(),
        texture: 0,
        mask: Some(1),
        transform: [1.0, 0.0, 0.0, 1.0, 2.0, 3.0],
        quad_size: [1.0; 2],
        uv_offset: [0.25; 2],
        uv_scale: [0.5; 2],
        clip_bounds: Some([1.0, 2.0, 9.0, 11.0]),
        opacity: 0.75,
        color_multiply: [1.0, 0.5, 0.25],
        grayscale: true,
        negative: true,
        blend: SpriteBlend::Add,
        mesh: None,
        emote_material: None,
        shader: Some(effect.clone()),
    };
    let emote = FrameSprite {
        blend: SpriteBlend::EmoteAdd,
        mesh: Some(Arc::from([
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0, 1.0],
        ])),
        emote_material: Some(NativeEmoteMaterial {
            corner_colors: [[0.0, 0.25, 1.0, 0.5]; 4],
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            blend_mode: 2,
            clip_rect: [0.0, 0.0, 20.0, 10.0],
            wipe: [-0.5, 1.0, 1.0],
        }),
        shader: None,
        ..sprite.clone()
    };
    let mask = FrameSprite {
        layer_id: String::new(),
        mask: None,
        ..emote.clone()
    };
    let direct = FrameGroup {
        sprites: 0..1,
        mask: None,
        clip_bounds: Some([0.0, 1.0, 10.0, 11.0]),
        opacity: 1.0,
        color_multiply: [1.0; 3],
        grayscale: false,
        negative: false,
        opaque: false,
        blend: SpriteBlend::Alpha,
        stencil: None,
        shader: None,
    };
    let stencil = FrameGroup {
        stencil: Some(0..1),
        ..direct.clone()
    };
    let composite = FrameGroup {
        opacity: 0.5,
        grayscale: true,
        negative: true,
        opaque: true,
        mask: Some(1),
        blend: SpriteBlend::Screen,
        ..direct.clone()
    };
    let shader_group = FrameGroup {
        shader: Some(effect),
        ..direct
    };
    FrameSnapshot {
        session_epoch: 5,
        render_frame_id: 11,
        host_frame_id: 31,
        stage_size: [40, 30],
        sprites: Box::new([sprite]),
        groups: Box::new([composite, shader_group]),
        masks: Box::new([mask.clone()]),
        transition: Some(FrameTransition {
            source: Box::new([emote]),
            source_groups: Box::new([stencil]),
            source_masks: Box::new([mask]),
            progress: 0.25,
            rule: Some(1),
            vague: 0.125,
        }),
        textures: [1, 2]
            .map(|revision| {
                Arc::new(FrameTexture {
                    revision,
                    width: 1,
                    height: 1,
                    rgba: Box::new([0, 50, 100, 255]),
                })
            })
            .into(),
    }
}

#[test]
fn decoded_packet_preserves_scene_and_resource_indices() {
    let frame = frame();
    let packet = FramePacket::decode(&encode(&frame).unwrap()).unwrap();
    assert_eq!(
        (
            packet.session_epoch,
            packet.render_frame_id,
            packet.host_frame_id
        ),
        (5, 11, 31)
    );
    assert_eq!(packet.stage_size, [40, 30]);
    assert_eq!(
        packet.textures(),
        &[
            FrameTextureDescriptor {
                revision: 1,
                width: 1,
                height: 1
            },
            FrameTextureDescriptor {
                revision: 2,
                width: 1,
                height: 1
            },
        ]
    );
    assert_eq!(packet.sprites, frame.sprites);
    assert_eq!(packet.groups, frame.groups);
    assert_eq!(packet.masks, frame.masks);
    assert_eq!(packet.transition, frame.transition);
}

#[test]
fn malformed_packet_is_rejected_before_it_can_request_pixels() {
    let packet = encode(&frame()).unwrap();
    assert!(FramePacket::decode(&packet[..packet.len() - 1]).is_err());
    let texture_table = HEADER + 2 * TEXTURE;
    let first_sprite = texture_table + TRANSITION;
    for (offset, value) in [
        (12, u32::MAX),                          // texture count
        (HEADER + 16, 5),                        // row stride
        (texture_table + 8, 3),                  // old sprite count
        (first_sprite, 5),                       // record length
        (first_sprite + 8, 2),                   // texture index
        (first_sprite + 24, f32::NAN.to_bits()), // transform
    ] {
        let mut malformed = packet.to_vec();
        malformed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(
            FramePacket::decode(&malformed).is_err(),
            "accepted bad field at {offset}"
        );
    }
}
