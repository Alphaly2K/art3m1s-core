//! GLSL sources owned by the reference GL backend.

use crate::render_pipeline::shader::{
    ALPHA_MASK_SHADER, GROUP_COMPOSITE_SHADER, RULE_TRANS_SHADER, SPRITE_SHADER,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShaderProgramSource {
    pub vertex_body: &'static str,
    pub fragment_body: &'static str,
}

pub(super) fn program(name: &str) -> Option<ShaderProgramSource> {
    match name {
        SPRITE_SHADER => Some(ShaderProgramSource {
            vertex_body: SPRITE_VERTEX_BODY,
            fragment_body: SPRITE_FRAGMENT_BODY,
        }),
        ALPHA_MASK_SHADER => Some(ShaderProgramSource {
            vertex_body: SPRITE_VERTEX_BODY,
            fragment_body: ALPHA_MASK_FRAGMENT_BODY,
        }),
        GROUP_COMPOSITE_SHADER => Some(ShaderProgramSource {
            vertex_body: SPRITE_VERTEX_BODY,
            fragment_body: GROUP_COMPOSITE_FRAGMENT_BODY,
        }),
        RULE_TRANS_SHADER => Some(ShaderProgramSource {
            vertex_body: SPRITE_VERTEX_BODY,
            fragment_body: RULE_TRANS_FRAGMENT_BODY,
        }),
        _ => None,
    }
}

const SPRITE_VERTEX_BODY: &str = r#"
layout(location = 0) in vec2 a_pos;   // unit quad 0..1
layout(location = 1) in vec2 a_uv;

uniform mat3 u_projection;  // stage pixels -> NDC
uniform mat3 u_transform;   // layer world transform in stage pixels
uniform vec2 u_size;        // drawn quad size in pixels
uniform vec2 u_uv_offset;   // normalized UV origin
uniform vec2 u_uv_scale;    // normalized UV span

out vec2 v_uv;
out vec2 v_model_position;

void main() {
    vec2 local = a_pos * u_size;
    vec3 world = u_transform * vec3(local, 1.0);
    vec3 ndc = u_projection * vec3(world.xy, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
    v_uv = u_uv_offset + a_uv * u_uv_scale;
    v_model_position = a_pos;
}
"#;

const SPRITE_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
in vec2 v_model_position;
out vec4 frag_color;

uniform sampler2D u_sampler;
uniform float u_opacity;
uniform vec3 u_multiply;
uniform int u_grayscale;
uniform int u_negative;
uniform int u_emote_enabled;
uniform vec4 u_emote_uv_rect;
uniform vec4 u_emote_color_tl;
uniform vec4 u_emote_color_tr;
uniform vec4 u_emote_color_bl;
uniform vec4 u_emote_color_br;
uniform int u_emote_blend_mode;
uniform vec4 u_emote_clip_rect;
uniform vec3 u_emote_wipe;

void main() {
    vec4 c = texture(u_sampler, v_uv);
    if (u_emote_enabled != 0) {
        if (v_model_position.x < u_emote_clip_rect.x ||
            v_model_position.y < u_emote_clip_rect.y ||
            v_model_position.x > u_emote_clip_rect.z ||
            v_model_position.y > u_emote_clip_rect.w) {
            discard;
        }
        vec2 uv_span = u_emote_uv_rect.zw - u_emote_uv_rect.xy;
        vec2 local_uv = clamp(
            (v_uv - u_emote_uv_rect.xy) /
                vec2(abs(uv_span.x) > 0.000001 ? uv_span.x : 1.0,
                     abs(uv_span.y) > 0.000001 ? uv_span.y : 1.0),
            vec2(0.0),
            vec2(1.0)
        );
        vec4 top = mix(u_emote_color_tl, u_emote_color_tr, local_uv.x);
        vec4 bottom = mix(u_emote_color_bl, u_emote_color_br, local_uv.x);
        c *= mix(top, bottom, local_uv.y);
        if (u_emote_wipe.z > 0.5) {
            c.a = clamp(c.a * u_emote_wipe.x + u_emote_wipe.y, 0.0, 1.0);
        }
        if ((u_emote_blend_mode & 0xF0) == 0x10) {
            c.rgb = clamp(c.rgb * 2.0, vec3(0.0), vec3(1.0));
        }
        int native_mode = u_emote_blend_mode & 0x0F;
        if (native_mode == 3 || native_mode == 4) {
            c.rgb *= c.a;
        } else if (native_mode == 5) {
            c.rgb = vec3(1.0) - c.rgb;
        }
        if (c.a <= 0.003) {
            discard;
        }
    }
    c.rgb *= u_multiply;
    if (u_grayscale != 0) {
        float g = dot(c.rgb, vec3(0.299, 0.587, 0.114));
        c.rgb = vec3(g);
    }
    if (u_negative != 0) {
        c.rgb = vec3(1.0) - c.rgb;
    }
    c.a *= u_opacity;
    frag_color = c;
}
"#;

/// 规则图像转场（Artemis `[trans type=2]`）。
///
/// - `u_texture_fore`：转场开始时捕获的旧画面。
/// - `u_texture_mask`：rule 灰度规则图（拉伸铺满全屏采样）。
/// - `progress`：0.0-1.0 转场进度。
/// - `vague`：边缘软化宽度（已归一化到 0-1，脚本 vague≈32 → 32/255）。
///
/// 语义：规则图亮度低的像素先切换到新画面。把进度重映射到 `[-vague, 1]`，
/// 用 smoothstep 在 `[t, t+vague]` 区间做软边——progress=0 时旧帧完全可见，
/// progress=1 时旧帧完全消失。
const RULE_TRANS_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_texture_fore;
uniform sampler2D u_texture_mask;
uniform float alpha;
uniform vec3 colorMultiply;
uniform float progress;
uniform float vague;

void main() {
    vec4 c = texture(u_texture_fore, v_uv);
    float rule = texture(u_texture_mask, v_uv).r;
    float v = max(vague, 1.0 / 255.0);
    float t = progress * (1.0 + v) - v;
    float keep = smoothstep(t, t + v, rule);
    c.rgb *= colorMultiply;
    c.a *= alpha * keep;
    frag_color = c;
}
"#;

const ALPHA_MASK_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_texture_fore;
uniform sampler2D u_texture_mask;
uniform float alpha;
uniform vec3 colorMultiply;

void main() {
    vec4 c = texture(u_texture_fore, v_uv);
    float mask_alpha = alpha * texture(u_texture_mask, v_uv).a;
    // Group targets store premultiplied RGB. A mask must scale RGB and alpha
    // together so the result stays premultiplied for the final group blend.
    c.rgb *= colorMultiply * mask_alpha;
    c.a *= mask_alpha;
    frag_color = c;
}
"#;

const GROUP_COMPOSITE_FRAGMENT_BODY: &str = r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_texture_fore;
uniform sampler2D u_texture_mask;
uniform float alpha;
uniform vec3 colorMultiply;
uniform float grayscale;
uniform float negative;
uniform float opaque;

void main() {
    vec4 c = texture(u_texture_fore, v_uv);
    float source_alpha = c.a;
    vec3 rgb = source_alpha > 0.0 ? c.rgb / source_alpha : vec3(0.0);
    rgb *= colorMultiply;
    if (grayscale != 0.0) {
        float gray = dot(rgb, vec3(0.299, 0.587, 0.114));
        rgb = vec3(gray);
    }
    if (negative != 0.0) {
        rgb = vec3(1.0) - rgb;
    }
    float mask_alpha = texture(u_texture_mask, v_uv).a;
    float output_alpha = mix(source_alpha, 1.0, opaque) * alpha * mask_alpha;
    frag_color = vec4(rgb * output_alpha, output_alpha);
}
"#;
