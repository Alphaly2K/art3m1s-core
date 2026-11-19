Texture2D textureUser : register(t1);
Texture2D textureFore : register(t3);
Texture2D textureMask : register(t0);
SamplerState sam : register(s0);

cbuffer Params : register(b0) {
    float param;
    float4 tint;
};

float4 main(float2 uv1 : TEXCOORD1, float4 position : SV_Position, float2 uv0 : TEXCOORD0) : SV_Target {
    float4 fore = textureFore.Sample(sam, uv0) * tint;
    return float4(fore.rgb * textureMask.Sample(sam, uv1).r + textureUser.Sample(sam, uv1).rgb * param, fore.a);
}
