texture textureFore;
texture textureMask;
texture textureUser;
sampler samplerFore = sampler_state { Texture = <textureFore>; };
sampler samplerMask = sampler_state { Texture = <textureMask>; };
sampler samplerUser = sampler_state { Texture = <textureUser>; };

float alpha;
float weights[2] = {0.25, 0.5};

void ps(float2 uv0 : TEXCOORD0, float2 uv1 : TEXCOORD1, out float4 color : COLOR0) {
    float4 fore = tex2D(samplerFore, uv1);
    float4 mask = tex2D(samplerMask, float2(0.5, uv0.y));
    float4 user = tex2D(samplerUser, uv1);
    color = float4(fore.r, mask.g * weights[0] + user.g * weights[1], user.b, alpha);
}
