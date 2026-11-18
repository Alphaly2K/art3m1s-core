#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include "artemis_shader_pass.h"
#include <d3d11shader.h>

#include <stdlib.h>
#include <string.h>

struct ArtD3D11Pass {
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    ID3D11VertexShader *vertex_shader;
    ID3D11InputLayout *layout;
    ID3D11Buffer *quad;
    ID3D11SamplerState *point, *linear;
    ID3D11BlendState *alpha;
    ID3D11RasterizerState *rasterizer;
    uint32_t width, height;
    ID3D11Texture2D *target, *readback;
    ID3D11RenderTargetView *view;
};

struct ArtD3D11Program {
    ID3D11PixelShader *shader;
    ID3D11ShaderReflection *reflection;
    ID3D11ShaderReflectionConstantBuffer *constants;
    uint32_t constant_bytes;
};

static const char vertex_source[] =
    "struct Varyings { float4 position : SV_Position; float2 uv0 : TEXCOORD0; float2 uv1 : TEXCOORD1; };\n"
    "Varyings main(float2 xy : POSITION, float2 uv0 : TEXCOORD0, float2 uv1 : TEXCOORD1) {\n"
    "    Varyings result;\n"
    "    result.position = float4(xy, 0, 1);\n"
    "    result.uv0 = uv0;\n"
    "    result.uv1 = uv1;\n"
    "    return result;\n"
    "}\n";

#define RELEASE(kind, object) do { if (object) kind##_Release(object); } while (0)
#define TRY(call) do { result = (call); if (FAILED(result)) goto done; } while (0)

HRESULT art_d3d11_pass_create(ArtD3D11Pass **output) {
    if (!output) return E_INVALIDARG;
    *output = NULL;
    ArtD3D11Pass *pass = calloc(1, sizeof(*pass));
    if (!pass) return E_OUTOFMEMORY;
    HRESULT result;
    ID3DBlob *vertex = NULL;
    const D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    TRY(D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                          &level, 1, D3D11_SDK_VERSION, &pass->device, NULL, &pass->context));
    TRY(D3DCompile(vertex_source, sizeof(vertex_source) - 1, "artemis_vertex.hlsl", NULL, NULL,
                   "main", "vs_5_0", 0, 0, &vertex, NULL));
    const void *code = ID3D10Blob_GetBufferPointer(vertex);
    const size_t code_bytes = ID3D10Blob_GetBufferSize(vertex);
    TRY(ID3D11Device_CreateVertexShader(pass->device, code, code_bytes, NULL, &pass->vertex_shader));
    const D3D11_INPUT_ELEMENT_DESC elements[] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, offsetof(ArtD3D11Vertex, xy), D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, offsetof(ArtD3D11Vertex, uv0), D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 1, DXGI_FORMAT_R32G32_FLOAT, 0, offsetof(ArtD3D11Vertex, uv1), D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    TRY(ID3D11Device_CreateInputLayout(pass->device, elements, 3, code, code_bytes, &pass->layout));

    const D3D11_BUFFER_DESC quad = {
        .ByteWidth = sizeof(ArtD3D11Vertex) * 4, .BindFlags = D3D11_BIND_VERTEX_BUFFER,
    };
    TRY(ID3D11Device_CreateBuffer(pass->device, &quad, NULL, &pass->quad));
    D3D11_SAMPLER_DESC sampler = {
        .Filter = D3D11_FILTER_MIN_MAG_MIP_POINT,
        .AddressU = D3D11_TEXTURE_ADDRESS_CLAMP, .AddressV = D3D11_TEXTURE_ADDRESS_CLAMP,
        .AddressW = D3D11_TEXTURE_ADDRESS_CLAMP, .MaxAnisotropy = 1,
        .ComparisonFunc = D3D11_COMPARISON_NEVER,
    };
    TRY(ID3D11Device_CreateSamplerState(pass->device, &sampler, &pass->point));
    sampler.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    TRY(ID3D11Device_CreateSamplerState(pass->device, &sampler, &pass->linear));

    const D3D11_BLEND_DESC blend = {
        .RenderTarget[0] = {
            .BlendEnable = TRUE, .SrcBlend = D3D11_BLEND_SRC_ALPHA, .DestBlend = D3D11_BLEND_INV_SRC_ALPHA,
            .BlendOp = D3D11_BLEND_OP_ADD, .SrcBlendAlpha = D3D11_BLEND_ONE,
            .DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA, .BlendOpAlpha = D3D11_BLEND_OP_ADD,
            .RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL,
        },
    };
    TRY(ID3D11Device_CreateBlendState(pass->device, &blend, &pass->alpha));
    const D3D11_RASTERIZER_DESC rasterizer = {
        .FillMode = D3D11_FILL_SOLID, .CullMode = D3D11_CULL_NONE, .DepthClipEnable = TRUE,
    };
    TRY(ID3D11Device_CreateRasterizerState(pass->device, &rasterizer, &pass->rasterizer));
    *output = pass;
done:
    RELEASE(ID3D10Blob, vertex);
    if (FAILED(result)) art_d3d11_pass_destroy(pass);
    return result;
}

void art_d3d11_pass_destroy(ArtD3D11Pass *pass) {
    if (!pass) return;
    if (pass->context) ID3D11DeviceContext_ClearState(pass->context);
    RELEASE(ID3D11RenderTargetView, pass->view);
    RELEASE(ID3D11Texture2D, pass->readback);
    RELEASE(ID3D11Texture2D, pass->target);
    RELEASE(ID3D11RasterizerState, pass->rasterizer);
    RELEASE(ID3D11BlendState, pass->alpha);
    RELEASE(ID3D11SamplerState, pass->linear);
    RELEASE(ID3D11SamplerState, pass->point);
    RELEASE(ID3D11Buffer, pass->quad);
    RELEASE(ID3D11InputLayout, pass->layout);
    RELEASE(ID3D11VertexShader, pass->vertex_shader);
    RELEASE(ID3D11DeviceContext, pass->context);
    RELEASE(ID3D11Device, pass->device);
    free(pass);
}

HRESULT art_d3d11_pass_compile(ArtD3D11Pass *pass, const void *source, size_t bytes,
                               ArtD3D11Program **output, ID3DBlob **diagnostics) {
    if (!pass || !source || bytes == 0 || bytes > 1024 * 1024 || !output) return E_INVALIDARG;
    *output = NULL;
    if (diagnostics) *diagnostics = NULL;
    ArtD3D11Program *program = calloc(1, sizeof(*program));
    if (!program) return E_OUTOFMEMORY;
    ID3DBlob *code = NULL;
    HRESULT result;
    TRY(D3DCompile(source, bytes, "artemis_effect.hlsl", NULL, NULL, "main", "ps_5_0",
                   0, 0, &code, diagnostics));
    const void *data = ID3D10Blob_GetBufferPointer(code);
    const size_t size = ID3D10Blob_GetBufferSize(code);
    if (size < 4 || memcmp(data, "DXBC", 4) != 0) { result = E_FAIL; goto done; }
    TRY(D3DReflect(data, size, &IID_ID3D11ShaderReflection, (void **)&program->reflection));
    ID3D11ShaderReflection *reflection = program->reflection;
    D3D11_SHADER_DESC desc;
    TRY(reflection->lpVtbl->GetDesc(reflection, &desc));
    for (UINT i = 0; i < desc.BoundResources; ++i) {
        D3D11_SHADER_INPUT_BIND_DESC binding;
        TRY(reflection->lpVtbl->GetResourceBindingDesc(reflection, i, &binding));
        if (binding.Type == D3D_SIT_CBUFFER && binding.BindPoint == 0 && binding.BindCount == 1) {
            program->constants = reflection->lpVtbl->GetConstantBufferByName(reflection, binding.Name);
            if (!program->constants) { result = E_FAIL; goto done; }
            D3D11_SHADER_BUFFER_DESC buffer;
            TRY(program->constants->lpVtbl->GetDesc(program->constants, &buffer));
            program->constant_bytes = buffer.Size;
        } else if (binding.Type == D3D_SIT_TEXTURE && binding.Dimension == D3D_SRV_DIMENSION_TEXTURE2D &&
                   binding.BindPoint < 4 && binding.BindCount <= 4 - binding.BindPoint) {
            continue;
        } else if (binding.Type == D3D_SIT_SAMPLER && binding.BindPoint == 0 && binding.BindCount == 1) {
            continue;
        } else {
            result = E_NOTIMPL;
            goto done;
        }
    }
    TRY(ID3D11Device_CreatePixelShader(pass->device, data, size, NULL, &program->shader));
    *output = program;
done:
    RELEASE(ID3D10Blob, code);
    if (FAILED(result)) art_d3d11_program_destroy(program);
    return result;
}

void art_d3d11_program_destroy(ArtD3D11Program *program) {
    if (!program) return;
    RELEASE(ID3D11PixelShader, program->shader);
    if (program->reflection) program->reflection->lpVtbl->Release(program->reflection);
    free(program);
}

uint32_t art_d3d11_constant_bytes(const ArtD3D11Program *program) {
    return program ? program->constant_bytes : 0;
}

HRESULT art_d3d11_set_uniform(const ArtD3D11Program *program, const char *name,
                              const float *values, uint32_t count, void *block, size_t bytes) {
    if (!program || !name || !values || !block) return E_INVALIDARG;
    ID3D11ShaderReflectionConstantBuffer *constants = program->constants;
    if (!constants) return S_FALSE;
    ID3D11ShaderReflectionVariable *variable = constants->lpVtbl->GetVariableByName(constants, name);
    D3D11_SHADER_VARIABLE_DESC desc;
    if (!variable || FAILED(variable->lpVtbl->GetDesc(variable, &desc))) return S_FALSE;
    ID3D11ShaderReflectionType *type = variable->lpVtbl->GetType(variable);
    D3D11_SHADER_TYPE_DESC shape;
    if (!type || FAILED(type->lpVtbl->GetDesc(type, &shape)) || shape.Type != D3D_SVT_FLOAT ||
        (shape.Class != D3D_SVC_SCALAR && shape.Class != D3D_SVC_VECTOR) || shape.Elements != 0 ||
        count == 0 || count > 4 || count != shape.Columns || desc.Size != count * sizeof(float) ||
        bytes < program->constant_bytes || (uint64_t)desc.StartOffset + desc.Size > bytes) return E_INVALIDARG;
    memcpy((char *)block + desc.StartOffset, values, desc.Size);
    return S_OK;
}

static int valid_extent(uint32_t width, uint32_t height) {
    return width && height && width <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION &&
        height <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION && (uint64_t)width * height * 4 <= 64 * 1024 * 1024;
}

static HRESULT upload(ID3D11Device *device, const ArtD3D11Image *image, ID3D11ShaderResourceView **view) {
    if (!image->bgra) return S_OK;
    if (!valid_extent(image->width, image->height) || image->row_bytes < image->width * 4 ||
        (uint64_t)(image->height - 1) * image->row_bytes + image->width * 4 > image->bytes) return E_INVALIDARG;
    const D3D11_TEXTURE2D_DESC desc = {
        .Width = image->width, .Height = image->height, .MipLevels = 1, .ArraySize = 1,
        .Format = DXGI_FORMAT_B8G8R8A8_UNORM, .SampleDesc = { 1, 0 },
        .Usage = D3D11_USAGE_IMMUTABLE, .BindFlags = D3D11_BIND_SHADER_RESOURCE,
    };
    const D3D11_SUBRESOURCE_DATA data = { .pSysMem = image->bgra, .SysMemPitch = image->row_bytes };
    ID3D11Texture2D *texture = NULL;
    HRESULT result = ID3D11Device_CreateTexture2D(device, &desc, &data, &texture);
    if (SUCCEEDED(result)) {
        result = ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)texture, NULL, view);
    }
    RELEASE(ID3D11Texture2D, texture);
    return result;
}

static HRESULT resize_target(ArtD3D11Pass *pass, uint32_t width, uint32_t height) {
    if (pass->width == width && pass->height == height) return S_OK;
    HRESULT result;
    ID3D11Texture2D *target = NULL, *readback = NULL;
    ID3D11RenderTargetView *view = NULL;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width, .Height = height, .MipLevels = 1, .ArraySize = 1,
        .Format = DXGI_FORMAT_B8G8R8A8_UNORM, .SampleDesc = { 1, 0 },
        .Usage = D3D11_USAGE_DEFAULT, .BindFlags = D3D11_BIND_RENDER_TARGET,
    };
    TRY(ID3D11Device_CreateTexture2D(pass->device, &desc, NULL, &target));
    TRY(ID3D11Device_CreateRenderTargetView(pass->device, (ID3D11Resource *)target, NULL, &view));
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    TRY(ID3D11Device_CreateTexture2D(pass->device, &desc, NULL, &readback));
    RELEASE(ID3D11RenderTargetView, pass->view);
    RELEASE(ID3D11Texture2D, pass->readback);
    RELEASE(ID3D11Texture2D, pass->target);
    pass->target = target;
    pass->readback = readback;
    pass->view = view;
    pass->width = width;
    pass->height = height;
    return S_OK;
done:
    RELEASE(ID3D11RenderTargetView, view);
    RELEASE(ID3D11Texture2D, readback);
    RELEASE(ID3D11Texture2D, target);
    return result;
}

HRESULT art_d3d11_pass_draw(ArtD3D11Pass *pass, const ArtD3D11Program *program,
                            const ArtD3D11Draw *draw, void *bgra, size_t capacity) {
    if (!pass || !program || !draw || !bgra || !valid_extent(draw->width, draw->height) ||
        capacity < (size_t)draw->width * draw->height * 4 ||
        draw->constant_bytes != program->constant_bytes ||
        (draw->constant_bytes && (!draw->constants || draw->constant_bytes % 16))) return E_INVALIDARG;

    HRESULT result;
    ID3D11Device *device = pass->device;
    ID3D11DeviceContext *context = pass->context;
    ID3D11ShaderResourceView *textures[4] = { NULL };
    ID3D11Buffer *constants = NULL;
    TRY(resize_target(pass, draw->width, draw->height));
    for (unsigned i = 0; i < 4; ++i) TRY(upload(device, &draw->textures[i], &textures[i]));
    if (draw->constant_bytes) {
        const D3D11_BUFFER_DESC desc = {
            .ByteWidth = draw->constant_bytes, .Usage = D3D11_USAGE_IMMUTABLE,
            .BindFlags = D3D11_BIND_CONSTANT_BUFFER,
        };
        const D3D11_SUBRESOURCE_DATA data = { .pSysMem = draw->constants };
        TRY(ID3D11Device_CreateBuffer(device, &desc, &data, &constants));
    }

    const UINT stride = sizeof(ArtD3D11Vertex), offset = 0;
    const D3D11_VIEWPORT viewport = { 0, 0, (FLOAT)draw->width, (FLOAT)draw->height, 0, 1 };
    ID3D11SamplerState *sampler = draw->point_sampler ? pass->point : pass->linear;
    ID3D11DeviceContext_UpdateSubresource(context, (ID3D11Resource *)pass->quad, 0, NULL, draw->quad, 0, 0);
    ID3D11DeviceContext_IASetInputLayout(context, pass->layout);
    ID3D11DeviceContext_IASetVertexBuffers(context, 0, 1, &pass->quad, &stride, &offset);
    ID3D11DeviceContext_IASetPrimitiveTopology(context, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
    ID3D11DeviceContext_VSSetShader(context, pass->vertex_shader, NULL, 0);
    ID3D11DeviceContext_PSSetShader(context, program->shader, NULL, 0);
    ID3D11DeviceContext_PSSetShaderResources(context, 0, 4, textures);
    ID3D11DeviceContext_PSSetSamplers(context, 0, 1, &sampler);
    ID3D11DeviceContext_PSSetConstantBuffers(context, 0, 1, &constants);
    ID3D11DeviceContext_OMSetBlendState(context, draw->alpha_blend ? pass->alpha : NULL, NULL, UINT32_MAX);
    ID3D11DeviceContext_OMSetRenderTargets(context, 1, &pass->view, NULL);
    ID3D11DeviceContext_RSSetViewports(context, 1, &viewport);
    ID3D11DeviceContext_RSSetState(context, pass->rasterizer);
    ID3D11DeviceContext_ClearRenderTargetView(context, pass->view, draw->clear_rgba);
    ID3D11DeviceContext_Draw(context, 4, 0);
    ID3D11DeviceContext_OMSetRenderTargets(context, 0, NULL, NULL);
    ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)pass->readback, (ID3D11Resource *)pass->target);

    D3D11_MAPPED_SUBRESOURCE map;
    TRY(ID3D11DeviceContext_Map(context, (ID3D11Resource *)pass->readback, 0, D3D11_MAP_READ, 0, &map));
    for (uint32_t y = 0; y < draw->height; ++y) {
        memcpy((char *)bgra + (size_t)y * draw->width * 4,
               (const char *)map.pData + (size_t)y * map.RowPitch, draw->width * 4);
    }
    ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)pass->readback, 0);
done:
    ID3D11DeviceContext_ClearState(context);
    RELEASE(ID3D11Buffer, constants);
    for (unsigned i = 0; i < 4; ++i) RELEASE(ID3D11ShaderResourceView, textures[i]);
    return result;
}
