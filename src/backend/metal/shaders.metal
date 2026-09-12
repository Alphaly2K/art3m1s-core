#include <metal_stdlib>
using namespace metal;

struct VertexInput {
    float2 position;
    float2 uv;
};

struct VertexUniforms {
    float4x4 transform;
    float2 size;
    float2 uv_offset;
    float2 uv_scale;
    float2 padding;
};

struct RasterData {
    float4 position [[position]];
    float2 uv [[user(locn0)]];
    float2 model_position [[user(locn1)]];
};

struct SpriteUniforms {
    float4 opacity_flags; // opacity, grayscale, negative, emote enabled
    float4 multiply;
    float4 emote_uv_rect;
    float4 emote_color_tl;
    float4 emote_color_tr;
    float4 emote_color_bl;
    float4 emote_color_br;
    float4 emote_blend_mode;
    float4 emote_clip_rect;
    float4 emote_wipe;
};

struct EffectUniforms {
    float4 alpha_progress_vague_opaque;
    float4 color_multiply_grayscale;
    float4 negative_padding;
};

vertex RasterData sprite_vertex(
    uint vertex_id [[vertex_id]],
    constant VertexInput *vertices [[buffer(0)]],
    constant VertexUniforms &uniforms [[buffer(1)]]) {
    VertexInput input = vertices[vertex_id];
    RasterData out;
    float2 local = input.position * uniforms.size;
    out.position = uniforms.transform * float4(local, 0.0, 1.0);
    out.uv = uniforms.uv_offset + input.uv * uniforms.uv_scale;
    out.model_position = input.position;
    return out;
}

fragment float4 sprite_fragment(
    RasterData in [[stage_in]],
    texture2d<float> source [[texture(0)]],
    sampler linear_sampler [[sampler(0)]],
    constant SpriteUniforms &uniforms [[buffer(0)]]) {
    float4 color = source.sample(linear_sampler, in.uv);
    if (uniforms.opacity_flags.w != 0.0) {
        float4 clip = uniforms.emote_clip_rect;
        if (in.model_position.x < clip.x || in.model_position.y < clip.y ||
            in.model_position.x > clip.z || in.model_position.y > clip.w) {
            discard_fragment();
        }
        float2 uv_span = uniforms.emote_uv_rect.zw - uniforms.emote_uv_rect.xy;
        float2 divisor = float2(
            abs(uv_span.x) > 0.000001 ? uv_span.x : 1.0,
            abs(uv_span.y) > 0.000001 ? uv_span.y : 1.0);
        float2 local_uv = clamp(
            (in.uv - uniforms.emote_uv_rect.xy) / divisor,
            float2(0.0), float2(1.0));
        float4 top = mix(uniforms.emote_color_tl, uniforms.emote_color_tr, local_uv.x);
        float4 bottom = mix(uniforms.emote_color_bl, uniforms.emote_color_br, local_uv.x);
        color *= mix(top, bottom, local_uv.y);
        if (uniforms.emote_wipe.z > 0.5) {
            color.a = clamp(
                color.a * uniforms.emote_wipe.x + uniforms.emote_wipe.y,
                0.0, 1.0);
        }
        uint blend_mode = uint(uniforms.emote_blend_mode.x);
        if ((blend_mode & 0xF0) == 0x10) {
            color.rgb = clamp(color.rgb * 2.0, float3(0.0), float3(1.0));
        }
        uint native_mode = blend_mode & 0x0F;
        if (native_mode == 3 || native_mode == 4) {
            color.rgb *= color.a;
        } else if (native_mode == 5) {
            color.rgb = float3(1.0) - color.rgb;
        }
        if (color.a <= 0.003) {
            discard_fragment();
        }
    }
    color.rgb *= uniforms.multiply.xyz;
    if (uniforms.opacity_flags.y != 0.0) {
        float gray = dot(color.rgb, float3(0.299, 0.587, 0.114));
        color.rgb = float3(gray);
    }
    if (uniforms.opacity_flags.z != 0.0) {
        color.rgb = float3(1.0) - color.rgb;
    }
    color.a *= uniforms.opacity_flags.x;
    return color;
}

fragment float4 alpha_mask_fragment(
    RasterData in [[stage_in]],
    texture2d<float> source [[texture(0)]],
    texture2d<float> mask [[texture(1)]],
    sampler linear_sampler [[sampler(0)]],
    constant EffectUniforms &uniforms [[buffer(1)]]) {
    float4 color = source.sample(linear_sampler, in.uv);
    float mask_alpha = uniforms.alpha_progress_vague_opaque.x *
        mask.sample(linear_sampler, in.uv).a;
    color.rgb *= uniforms.color_multiply_grayscale.xyz * mask_alpha;
    color.a *= mask_alpha;
    return color;
}

fragment float4 group_composite_fragment(
    RasterData in [[stage_in]],
    texture2d<float> source [[texture(0)]],
    texture2d<float> mask [[texture(1)]],
    sampler linear_sampler [[sampler(0)]],
    constant EffectUniforms &uniforms [[buffer(1)]]) {
    float4 color = source.sample(linear_sampler, in.uv);
    float source_alpha = color.a;
    float3 rgb = source_alpha > 0.0 ? color.rgb / source_alpha : float3(0.0);
    rgb *= uniforms.color_multiply_grayscale.xyz;
    if (uniforms.color_multiply_grayscale.w != 0.0) {
        float gray = dot(rgb, float3(0.299, 0.587, 0.114));
        rgb = float3(gray);
    }
    if (uniforms.negative_padding.x != 0.0) {
        rgb = float3(1.0) - rgb;
    }
    float mask_alpha = mask.sample(linear_sampler, in.uv).a;
    float output_alpha = mix(
        source_alpha,
        1.0,
        uniforms.alpha_progress_vague_opaque.w) *
        uniforms.alpha_progress_vague_opaque.x * mask_alpha;
    return float4(rgb * output_alpha, output_alpha);
}

fragment float4 rule_transition_fragment(
    RasterData in [[stage_in]],
    texture2d<float> source [[texture(0)]],
    texture2d<float> mask [[texture(1)]],
    sampler linear_sampler [[sampler(0)]],
    constant EffectUniforms &uniforms [[buffer(1)]]) {
    float4 color = source.sample(linear_sampler, in.uv);
    float rule = mask.sample(linear_sampler, in.uv).r;
    float progress = uniforms.alpha_progress_vague_opaque.y;
    float vague = max(uniforms.alpha_progress_vague_opaque.z, 1.0 / 255.0);
    float threshold = progress * (1.0 + vague) - vague;
    float keep = smoothstep(threshold, threshold + vague, rule);
    color.rgb *= uniforms.color_multiply_grayscale.xyz;
    color.a *= uniforms.alpha_progress_vague_opaque.x * keep;
    return color;
}


vertex RasterData fullscreen_vertex(uint vertex_id [[vertex_id]]) {
    float2 ndc[4] = {
        float2(-1.0, 1.0),
        float2(1.0, 1.0),
        float2(-1.0, -1.0),
        float2(1.0, -1.0)
    };
    float2 uv[4] = {
        float2(0.0, 0.0),
        float2(1.0, 0.0),
        float2(0.0, 1.0),
        float2(1.0, 1.0)
    };
    RasterData out;
    out.position = float4(ndc[vertex_id], 0.0, 1.0);
    out.uv = uv[vertex_id];
    out.model_position = uv[vertex_id];
    return out;
}

fragment float4 yuv_convert_fragment(
    RasterData in [[stage_in]],
    texture2d<float> luma [[texture(0)]],
    texture2d<float> chroma [[texture(1)]],
    sampler linear_sampler [[sampler(0)]],
    constant float4 &params [[buffer(0)]]) {
    float y = luma.sample(linear_sampler, in.uv).r;
    float2 cbcr = chroma.sample(linear_sampler, in.uv).rg;
    if (params.x > 0.5) {
        y = (y - 16.0 / 255.0) * (255.0 / 219.0);
        cbcr = (cbcr - 16.0 / 255.0) * (255.0 / 224.0);
    }
    float cb = cbcr.x - 0.5;
    float cr = cbcr.y - 0.5;
    float r = y + 1.5748 * cr;
    float g = y - 0.1873 * cb - 0.4681 * cr;
    float b = y + 1.8556 * cb;
    return float4(clamp(float3(r, g, b), 0.0, 1.0), 1.0);
}

fragment float4 yuv420p_convert_fragment(
    RasterData in [[stage_in]],
    texture2d<float> luma [[texture(0)]],
    texture2d<float> chroma_u [[texture(1)]],
    texture2d<float> chroma_v [[texture(2)]],
    sampler linear_sampler [[sampler(0)]]) {
    float y = (luma.sample(linear_sampler, in.uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    float u = chroma_u.sample(linear_sampler, in.uv).r - 0.5;
    float v = chroma_v.sample(linear_sampler, in.uv).r - 0.5;
    float r = y + 1.5748 * v;
    float g = y - 0.1873 * u - 0.4681 * v;
    float b = y + 1.8556 * u;
    return float4(clamp(float3(r, g, b), 0.0, 1.0), 1.0);
}
