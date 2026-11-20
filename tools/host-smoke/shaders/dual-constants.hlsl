#ifndef SKIP_B0
cbuffer Primary : register(b0) { float red = 1; };
#else
static const float red = 0;
#endif
cbuffer Secondary : register(b1) { float green = 1; };

float4 main() : SV_Target {
    return float4(red, green, 0, 1);
}
