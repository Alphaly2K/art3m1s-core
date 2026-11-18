#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <d3d11.h>
#include <d3dcompiler.h>
#include <fcntl.h>
#include <io.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { extent = 4 };

#define VARYINGS \
    "struct Varyings { float4 position : SV_Position; float2 uv0 : TEXCOORD0; float2 uv1 : TEXCOORD1; };\n"

static const char vertex_source[] = VARYINGS
    "Varyings main(float2 xy : POSITION, float2 back : TEXCOORD0, float2 fore : TEXCOORD1) {\n"
    "    Varyings result;\n"
    "    result.position = float4(xy, 0, 1);\n"
    "    result.uv0 = back;\n"
    "    result.uv1 = fore;\n"
    "    return result;\n"
    "}\n";

static const char pixel_source[] = VARYINGS
    "Texture2D textureFore : register(t0);\n"
    "Texture2D textureBack : register(t1);\n"
    "SamplerState samplerPoint : register(s0);\n"
    "cbuffer Params : register(b0) { float4 tint; };\n"
    "float4 main(Varyings input) : SV_Target {\n"
    "    float3 fore = textureFore.Sample(samplerPoint, input.uv1).rgb * tint.rgb;\n"
    "    float3 back = textureBack.Sample(samplerPoint, input.uv0).rgb * 0.25;\n"
    "    return float4(fore + back, tint.a);\n"
    "}\n";

static const uint32_t foreground[] = { 0xff804020, 0xff206040, 0xff4080c0, 0xffc02080 };
static const uint32_t background[] = { 0xff102040, 0xff401020, 0xff204010, 0xff304050 };
static const uint32_t clear_color = 0xff101820;

static void check(HRESULT result, const char *operation) {
    if (FAILED(result)) {
        fprintf(stderr, "%s failed: 0x%08lx\n", operation, (unsigned long)(DWORD)result);
        exit(1);
    }
}

#define CHECK(call) check((call), #call)

static ID3DBlob *compile(const char *source, const char *profile) {
    ID3DBlob *shader = NULL, *messages = NULL;
    HRESULT result = D3DCompile(source, strlen(source), "artemis_d3d11.hlsl", NULL, NULL,
                                "main", profile, 0, 0, &shader, &messages);
    if (messages) {
        fwrite(ID3D10Blob_GetBufferPointer(messages), 1, ID3D10Blob_GetBufferSize(messages), stderr);
        ID3D10Blob_Release(messages);
    }
    check(result, profile);
    size_t size = ID3D10Blob_GetBufferSize(shader);
    if (size < 4 || memcmp(ID3D10Blob_GetBufferPointer(shader), "DXBC", 4) != 0) {
        fprintf(stderr, "%s did not compile to DXBC\n", profile);
        exit(1);
    }
    fprintf(stderr, "%s: compiled DXBC (%zu bytes)\n", profile, size);
    return shader;
}

static ID3D11ShaderResourceView *make_texture(ID3D11Device *device, const uint32_t *pixels) {
    const D3D11_TEXTURE2D_DESC desc = {
        .Width = 2, .Height = 2, .MipLevels = 1, .ArraySize = 1,
        .Format = DXGI_FORMAT_B8G8R8A8_UNORM, .SampleDesc = { 1, 0 },
        .Usage = D3D11_USAGE_IMMUTABLE, .BindFlags = D3D11_BIND_SHADER_RESOURCE,
    };
    const D3D11_SUBRESOURCE_DATA data = { .pSysMem = pixels, .SysMemPitch = 2 * sizeof(*pixels) };
    ID3D11Texture2D *texture = NULL;
    ID3D11ShaderResourceView *view = NULL;
    CHECK(ID3D11Device_CreateTexture2D(device, &desc, &data, &texture));
    CHECK(ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)texture, NULL, &view));
    ID3D11Texture2D_Release(texture);
    return view;
}

static int channel(uint32_t pixel, int shift) {
    return (int)((pixel >> shift) & 255);
}

static int check_pixels(const D3D11_MAPPED_SUBRESOURCE *map, unsigned width,
                        unsigned height, BOOL dim_red) {
    int mismatches = 0;
    for (unsigned y = 0; y < height; ++y) {
        const uint32_t *row = (const uint32_t *)((const char *)map->pData + y * map->RowPitch);
        for (unsigned x = 0; x < width; ++x) {
            unsigned row_index = (y >= height / 2) * 2;
            unsigned column = x >= width / 2;
            uint32_t fore = foreground[row_index + 1 - column];
            uint32_t back = background[row_index + column];
            for (int shift = 0; shift <= 24; shift += 8) {
                int expected = shift == 24 ? 255 :
                    ((shift == 16 && !dim_red ? 4 : 2) * channel(fore, shift) + channel(back, shift) +
                     4 * channel(clear_color, shift)) / 8;
                if (abs(channel(row[x], shift) - expected) > 1) {
                    if (!mismatches) {
                        fprintf(stderr, "ps_5_0 pixel (%u,%u) = %08lx; channel %d expected %d\n",
                                x, y, (unsigned long)row[x], shift, expected);
                    }
                    ++mismatches;
                    break;
                }
            }
        }
    }
    if (mismatches) {
        for (unsigned y = 0; y < height; ++y) {
            const uint32_t *row = (const uint32_t *)((const char *)map->pData + y * map->RowPitch);
            fprintf(stderr, "ps_5_0 row %u:", y);
            for (unsigned x = 0; x < width; ++x) {
                fprintf(stderr, " %08lx", (unsigned long)row[x]);
            }
            fputc('\n', stderr);
        }
        return 1;
    }
    fprintf(stderr, "ps_5_0: passed %u RGBA pixels\n", width * height);
    return 0;
}

static void write_bytes(const void *bytes, size_t size) {
    if (fwrite(bytes, 1, size, stdout) != size) {
        fputs("Cannot write the BGRA frame stream\n", stderr);
        exit(1);
    }
}

static int draw(ID3D11Device *device, ID3D11DeviceContext *context, ID3D11Buffer *constant,
                uint32_t width, uint32_t height, BOOL dim_red, uint32_t frame_id) {
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width, .Height = height, .MipLevels = 1, .ArraySize = 1,
        .Format = DXGI_FORMAT_B8G8R8A8_UNORM, .SampleDesc = { 1, 0 },
        .Usage = D3D11_USAGE_DEFAULT, .BindFlags = D3D11_BIND_RENDER_TARGET,
    };
    ID3D11Texture2D *target = NULL, *readback = NULL;
    ID3D11RenderTargetView *view = NULL;
    CHECK(ID3D11Device_CreateTexture2D(device, &desc, NULL, &target));
    CHECK(ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)target, NULL, &view));
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    CHECK(ID3D11Device_CreateTexture2D(device, &desc, NULL, &readback));

    const D3D11_VIEWPORT viewport = { 0, 0, width, height, 0, 1 };
    const float clear[] = { 16 / 255.0f, 24 / 255.0f, 32 / 255.0f, 1 };
    const float tint[] = { dim_red ? 0.5f : 1, 0.5f, 0.5f, 0.5f };
    ID3D11DeviceContext_UpdateSubresource(context, (ID3D11Resource *)constant, 0, NULL, tint, 0, 0);
    ID3D11DeviceContext_OMSetRenderTargets(context, 1, &view, NULL);
    ID3D11DeviceContext_RSSetViewports(context, 1, &viewport);
    ID3D11DeviceContext_ClearRenderTargetView(context, view, clear);
    ID3D11DeviceContext_Draw(context, 4, 0);
    ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)readback, (ID3D11Resource *)target);

    D3D11_MAPPED_SUBRESOURCE map;
    CHECK(ID3D11DeviceContext_Map(context, (ID3D11Resource *)readback, 0, D3D11_MAP_READ, 0, &map));
    int result = check_pixels(&map, width, height, dim_red);
    if (!result && frame_id) {
        const uint32_t header[] = { frame_id, width, height };
        write_bytes(header, sizeof(header));
        for (uint32_t y = 0; y < height; ++y) {
            write_bytes((const char *)map.pData + y * map.RowPitch, width * sizeof(uint32_t));
        }
    }
    ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)readback, 0);
    ID3D11DeviceContext_OMSetRenderTargets(context, 0, NULL, NULL);
    ID3D11Texture2D_Release(readback);
    ID3D11RenderTargetView_Release(view);
    ID3D11Texture2D_Release(target);
    return result;
}

int main(int argc, char **argv) {
    const BOOL stream = argc == 2 && strcmp(argv[1], "--stream-bgra") == 0;
    if (argc != 1 && !stream) {
        fputs("Usage: artemis-d3d11.exe [--stream-bgra]\n", stderr);
        return 2;
    }
    if (stream && _setmode(_fileno(stdout), _O_BINARY) == -1) {
        fputs("Cannot set the BGRA stream to binary mode\n", stderr);
        return 1;
    }
    const D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    ID3D11Device *device = NULL;
    ID3D11DeviceContext *context = NULL;
    CHECK(D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                            &level, 1, D3D11_SDK_VERSION, &device, NULL, &context));
    fprintf(stderr, "D3D11 device: feature level 11_0\n");

    ID3DBlob *vs_blob = compile(vertex_source, "vs_5_0");
    ID3DBlob *ps_blob = compile(pixel_source, "ps_5_0");
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    CHECK(ID3D11Device_CreateVertexShader(device, ID3D10Blob_GetBufferPointer(vs_blob),
                                          ID3D10Blob_GetBufferSize(vs_blob), NULL, &vs));
    CHECK(ID3D11Device_CreatePixelShader(device, ID3D10Blob_GetBufferPointer(ps_blob),
                                         ID3D10Blob_GetBufferSize(ps_blob), NULL, &ps));
    const D3D11_INPUT_ELEMENT_DESC elements[] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 1, DXGI_FORMAT_R32G32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    ID3D11InputLayout *layout = NULL;
    CHECK(ID3D11Device_CreateInputLayout(device, elements, 3, ID3D10Blob_GetBufferPointer(vs_blob),
                                        ID3D10Blob_GetBufferSize(vs_blob), &layout));
    ID3D10Blob_Release(vs_blob);
    ID3D10Blob_Release(ps_blob);

    const float vertices[][6] = { { -1,  1, 0, 0, 1, 0 }, { 1,  1, 1, 0, 0, 0 },
                                  { -1, -1, 0, 1, 1, 1 }, { 1, -1, 1, 1, 0, 1 } };
    const D3D11_BUFFER_DESC vertex_desc = {
        .ByteWidth = sizeof(vertices), .Usage = D3D11_USAGE_IMMUTABLE, .BindFlags = D3D11_BIND_VERTEX_BUFFER,
    };
    const D3D11_SUBRESOURCE_DATA vertex_data = { .pSysMem = vertices };
    ID3D11Buffer *vertex = NULL;
    CHECK(ID3D11Device_CreateBuffer(device, &vertex_desc, &vertex_data, &vertex));

    ID3D11ShaderResourceView *textures[] = { make_texture(device, foreground), make_texture(device, background) };
    const D3D11_SAMPLER_DESC sampler_desc = {
        .Filter = D3D11_FILTER_MIN_MAG_MIP_POINT,
        .AddressU = D3D11_TEXTURE_ADDRESS_CLAMP, .AddressV = D3D11_TEXTURE_ADDRESS_CLAMP,
        .AddressW = D3D11_TEXTURE_ADDRESS_CLAMP, .MaxAnisotropy = 1,
        .ComparisonFunc = D3D11_COMPARISON_NEVER,
    };
    ID3D11SamplerState *sampler = NULL;
    CHECK(ID3D11Device_CreateSamplerState(device, &sampler_desc, &sampler));
    const D3D11_BUFFER_DESC constant_desc = {
        .ByteWidth = 4 * sizeof(float), .Usage = D3D11_USAGE_DEFAULT, .BindFlags = D3D11_BIND_CONSTANT_BUFFER,
    };
    ID3D11Buffer *constant = NULL;
    CHECK(ID3D11Device_CreateBuffer(device, &constant_desc, NULL, &constant));

    const D3D11_BLEND_DESC blend_desc = {
        .RenderTarget[0] = {
            .BlendEnable = TRUE, .SrcBlend = D3D11_BLEND_SRC_ALPHA, .DestBlend = D3D11_BLEND_INV_SRC_ALPHA,
            .BlendOp = D3D11_BLEND_OP_ADD, .SrcBlendAlpha = D3D11_BLEND_ONE,
            .DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA, .BlendOpAlpha = D3D11_BLEND_OP_ADD,
            .RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL,
        },
    };
    ID3D11BlendState *blend = NULL;
    CHECK(ID3D11Device_CreateBlendState(device, &blend_desc, &blend));
    const D3D11_RASTERIZER_DESC rasterizer_desc = {
        .FillMode = D3D11_FILL_SOLID, .CullMode = D3D11_CULL_NONE, .DepthClipEnable = TRUE,
    };
    ID3D11RasterizerState *rasterizer = NULL;
    CHECK(ID3D11Device_CreateRasterizerState(device, &rasterizer_desc, &rasterizer));
    ID3D11DeviceContext_OMSetBlendState(context, blend, NULL, UINT32_MAX);
    ID3D11DeviceContext_RSSetState(context, rasterizer);
    const UINT stride = sizeof(vertices[0]), offset = 0;
    ID3D11DeviceContext_IASetInputLayout(context, layout);
    ID3D11DeviceContext_IASetVertexBuffers(context, 0, 1, &vertex, &stride, &offset);
    ID3D11DeviceContext_IASetPrimitiveTopology(context, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
    ID3D11DeviceContext_VSSetShader(context, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(context, ps, NULL, 0);
    ID3D11DeviceContext_PSSetShaderResources(context, 0, 2, textures);
    ID3D11DeviceContext_PSSetSamplers(context, 0, 1, &sampler);
    ID3D11DeviceContext_PSSetConstantBuffers(context, 0, 1, &constant);
    int result = draw(device, context, constant, extent, extent, FALSE, stream ? 1 : 0);
    if (stream && !result) {
        result = draw(device, context, constant, extent, extent, TRUE, 2);
        if (!result) result = draw(device, context, constant, 6, 8, FALSE, 3);
    }
    ID3D11DeviceContext_ClearState(context);
    ID3D11RasterizerState_Release(rasterizer);
    ID3D11BlendState_Release(blend);
    ID3D11Buffer_Release(constant);
    ID3D11SamplerState_Release(sampler);
    ID3D11ShaderResourceView_Release(textures[1]);
    ID3D11ShaderResourceView_Release(textures[0]);
    ID3D11Buffer_Release(vertex);
    ID3D11InputLayout_Release(layout);
    ID3D11PixelShader_Release(ps);
    ID3D11VertexShader_Release(vs);
    ID3D11DeviceContext_Release(context);
    ID3D11Device_Release(device);
    return result;
}
