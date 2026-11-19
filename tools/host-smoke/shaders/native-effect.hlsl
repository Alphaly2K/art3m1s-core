Texture2D textureUser : register(t1);
Texture2D textureFore : register(t3);
Texture2D textureMask : register(t0);
SamplerState sam : register(s0);

cbuffer Params : register(b0) {
    float param;
    float4 tint;
};

float4 main(float2 uv : TEXCOORD0) : SV_Target {
    float4 fore = textureFore.Sample(sam, uv) * tint;
    return float4(fore.rgb * textureMask.Sample(sam, uv).r + textureUser.Sample(sam, uv).rgb * param, fore.a);
}
