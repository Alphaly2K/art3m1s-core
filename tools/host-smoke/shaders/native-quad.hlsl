struct Vertex {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

Vertex main(uint vertexID : SV_VertexID) {
    Vertex result;
    result.uv = float2(vertexID & 1, vertexID >> 1);
    result.position = float4(result.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return result;
}
