struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VertexUniforms {
    transform: mat4x4<f32>,
    size: vec2<f32>,
    uv_offset: vec2<f32>,
    uv_scale: vec2<f32>,
    padding: vec2<f32>,
}

struct SpriteUniforms {
    opacity_flags: vec4<f32>,
    multiply: vec4<f32>,
    emote_uv_rect: vec4<f32>,
    emote_color_tl: vec4<f32>,
    emote_color_tr: vec4<f32>,
    emote_color_bl: vec4<f32>,
    emote_color_br: vec4<f32>,
    emote_blend_mode: vec4<f32>,
    emote_clip_rect: vec4<f32>,
    emote_wipe: vec4<f32>,
}

struct EffectUniforms {
    alpha_progress_vague_opaque: vec4<f32>,
    color_multiply_grayscale: vec4<f32>,
    negative_padding: vec4<f32>,
}

struct UniformBlock {
    vertex: VertexUniforms,
    sprite: SpriteUniforms,
    effect: EffectUniforms,
}

struct RasterData {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) model_position: vec2<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: UniformBlock;
@group(0) @binding(1) var source_texture: texture_2d<f32>;
@group(0) @binding(2) var mask_texture: texture_2d<f32>;
@group(0) @binding(3) var user_texture: texture_2d<f32>;
@group(0) @binding(4) var linear_sampler: sampler;

@vertex
fn sprite_vertex(input: VertexInput) -> RasterData {
    var out: RasterData;
    let local = input.position * uniforms.vertex.size;
    out.position = uniforms.vertex.transform * vec4<f32>(local, 0.0, 1.0);
    out.uv = uniforms.vertex.uv_offset + input.uv * uniforms.vertex.uv_scale;
    out.model_position = input.position;
    return out;
}

@fragment
fn sprite_fragment(in: RasterData) -> @location(0) vec4<f32> {
    var color = textureSample(source_texture, linear_sampler, in.uv);
    if (uniforms.sprite.opacity_flags.w != 0.0) {
        let clip = uniforms.sprite.emote_clip_rect;
        if (in.model_position.x < clip.x || in.model_position.y < clip.y ||
            in.model_position.x > clip.z || in.model_position.y > clip.w) {
            discard;
        }
        let uv_span = uniforms.sprite.emote_uv_rect.zw - uniforms.sprite.emote_uv_rect.xy;
        let divisor = vec2<f32>(
            select(1.0, uv_span.x, abs(uv_span.x) > 0.000001),
            select(1.0, uv_span.y, abs(uv_span.y) > 0.000001),
        );
        let local_uv = clamp(
            (in.uv - uniforms.sprite.emote_uv_rect.xy) / divisor,
            vec2<f32>(0.0),
            vec2<f32>(1.0),
        );
        let top = mix(
            uniforms.sprite.emote_color_tl,
            uniforms.sprite.emote_color_tr,
            local_uv.x,
        );
        let bottom = mix(
            uniforms.sprite.emote_color_bl,
            uniforms.sprite.emote_color_br,
            local_uv.x,
        );
        color *= mix(top, bottom, local_uv.y);
        if (uniforms.sprite.emote_wipe.z > 0.5) {
            color.a = clamp(
                color.a * uniforms.sprite.emote_wipe.x + uniforms.sprite.emote_wipe.y,
                0.0,
                1.0,
            );
        }
        let blend_mode = u32(uniforms.sprite.emote_blend_mode.x);
        if ((blend_mode & 0xF0u) == 0x10u) {
            color = vec4<f32>(clamp(color.rgb * 2.0, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
        }
        let native_mode = blend_mode & 0x0Fu;
        if (native_mode == 3u || native_mode == 4u) {
            color = vec4<f32>(color.rgb * color.a, color.a);
        } else if (native_mode == 5u) {
            color = vec4<f32>(vec3<f32>(1.0) - color.rgb, color.a);
        }
        if (color.a <= 0.003) {
            discard;
        }
    }
    color = vec4<f32>(color.rgb * uniforms.sprite.multiply.xyz, color.a);
    if (uniforms.sprite.opacity_flags.y != 0.0) {
        let gray = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));
        color = vec4<f32>(vec3<f32>(gray), color.a);
    }
    if (uniforms.sprite.opacity_flags.z != 0.0) {
        color = vec4<f32>(vec3<f32>(1.0) - color.rgb, color.a);
    }
    color.a *= uniforms.sprite.opacity_flags.x;
    return color;
}

@fragment
fn alpha_mask_fragment(in: RasterData) -> @location(0) vec4<f32> {
    var color = textureSample(source_texture, linear_sampler, in.uv);
    let mask_alpha = uniforms.effect.alpha_progress_vague_opaque.x *
        textureSample(mask_texture, linear_sampler, in.uv).a;
    color = vec4<f32>(
        color.rgb * uniforms.effect.color_multiply_grayscale.xyz * mask_alpha,
        color.a * mask_alpha,
    );
    return color;
}

@fragment
fn group_composite_fragment(in: RasterData) -> @location(0) vec4<f32> {
    let color = textureSample(source_texture, linear_sampler, in.uv);
    let source_alpha = color.a;
    var rgb = vec3<f32>(0.0);
    if (source_alpha > 0.0) {
        rgb = color.rgb / source_alpha;
    }
    rgb *= uniforms.effect.color_multiply_grayscale.xyz;
    if (uniforms.effect.color_multiply_grayscale.w != 0.0) {
        let gray = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
        rgb = vec3<f32>(gray);
    }
    if (uniforms.effect.negative_padding.x != 0.0) {
        rgb = vec3<f32>(1.0) - rgb;
    }
    let mask_alpha = textureSample(mask_texture, linear_sampler, in.uv).a;
    let output_alpha = mix(
        source_alpha,
        1.0,
        uniforms.effect.alpha_progress_vague_opaque.w,
    ) * uniforms.effect.alpha_progress_vague_opaque.x * mask_alpha;
    return vec4<f32>(rgb * output_alpha, output_alpha);
}

@fragment
fn rule_transition_fragment(in: RasterData) -> @location(0) vec4<f32> {
    var color = textureSample(source_texture, linear_sampler, in.uv);
    let rule = textureSample(mask_texture, linear_sampler, in.uv).r;
    let progress = uniforms.effect.alpha_progress_vague_opaque.y;
    let vague = max(uniforms.effect.alpha_progress_vague_opaque.z, 1.0 / 255.0);
    let threshold = progress * (1.0 + vague) - vague;
    let keep = smoothstep(threshold, threshold + vague, rule);
    color = vec4<f32>(
        color.rgb * uniforms.effect.color_multiply_grayscale.xyz,
        color.a * uniforms.effect.alpha_progress_vague_opaque.x * keep,
    );
    return color;
}
