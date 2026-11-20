#define COBJMACROS
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <d3dcompiler.h>
#include <d3d11shader.h>
#include <fcntl.h>
#include <io.h>
#define semantic_casecmp _stricmp
#else
#define INITGUID
#define COM_NO_WINDOWS_H
#include <vkd3d_windows.h>
#include <vkd3d_d3d11shader.h>
#include <vkd3d_d3dcompiler.h>
#include <strings.h>
#define semantic_casecmp strcasecmp
#endif
#include "artemis_dxbc_effect.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { MAX_SOURCE = 1024 * 1024 };

_Static_assert(sizeof(ArtDxbcHeader) == 20, "ADE1 header size");
_Static_assert(sizeof(ArtDxbcTexture) == 4 + ART_DXBC_NAME_BYTES, "ADE1 texture record size");
_Static_assert(sizeof(ArtDxbcUniform) == 8 + ART_DXBC_NAME_BYTES, "ADE1 uniform record size");

static int copy_name(char name[ART_DXBC_NAME_BYTES], const char *source) {
    if (!source || !*source || strlen(source) >= ART_DXBC_NAME_BYTES) return 0;
    memcpy(name, source, strlen(source) + 1);
    return 1;
}

static void report_diagnostics(ID3DBlob *diagnostics) {
    if (!diagnostics) return;
    fwrite(ID3D10Blob_GetBufferPointer(diagnostics), 1, ID3D10Blob_GetBufferSize(diagnostics), stderr);
    ID3D10Blob_Release(diagnostics);
}

enum QuadField { QUAD_POSITION, QUAD_UV0, QUAD_UV1, QUAD_FIELD_COUNT };

static int quad_field(const D3D11_SIGNATURE_PARAMETER_DESC *input) {
    if (input->ComponentType != D3D_REGISTER_COMPONENT_FLOAT32 ||
        input->MinPrecision != D3D_MIN_PRECISION_DEFAULT || input->Stream || !input->SemanticName) return -1;
    if (input->SystemValueType == D3D_NAME_POSITION && !input->SemanticIndex && input->Mask == 15)
        return QUAD_POSITION;
    if (input->SystemValueType == D3D_NAME_UNDEFINED && !semantic_casecmp(input->SemanticName, "TEXCOORD") &&
        input->SemanticIndex <= 1 && (input->Mask == 3 || input->Mask == 12))
        return QUAD_UV0 + (int)input->SemanticIndex;
    return -1;
}

static int matching_parameter(const D3D11_SIGNATURE_PARAMETER_DESC *input,
                              const D3D11_SIGNATURE_PARAMETER_DESC *output) {
    return quad_field(input) == quad_field(output) && output->Register == input->Register &&
        output->Mask == input->Mask;
}

static HRESULT quad_vertex(ID3D11ShaderReflection *pixel, UINT count, ID3DBlob **code) {
    const char *const fields[] = {
        "float4 position : SV_Position;\n",
        "float2 uv0 : TEXCOORD0;\n",
        "float2 uv1 : TEXCOORD1;\n",
    };
    const char *const assignments[] = {
        "result.position = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);\n",
        "result.uv0 = uv;\n",
        "result.uv1 = uv;\n",
    };
    if (count > QUAD_FIELD_COUNT) {
        fputs("Too many pixel inputs for the Artemis quad\n", stderr);
        return E_NOTIMPL;
    }
    int order[QUAD_FIELD_COUNT];
    unsigned seen = 0;
    D3D11_SIGNATURE_PARAMETER_DESC inputs[QUAD_FIELD_COUNT];
    for (UINT i = 0; i < count; ++i) {
        HRESULT status = pixel->lpVtbl->GetInputParameterDesc(pixel, i, &inputs[i]);
        if (FAILED(status)) return status;
        int field = quad_field(&inputs[i]);
        if (field < 0 || (seen & (1u << field))) {
            fprintf(stderr, "Unsupported pixel input: %s%u (register %u, mask 0x%x)\n",
                    inputs[i].SemanticName ? inputs[i].SemanticName : "<unnamed>", inputs[i].SemanticIndex,
                    inputs[i].Register, (unsigned)inputs[i].Mask);
            return E_NOTIMPL;
        }
        order[i] = field;
        seen |= 1u << field;
    }
    UINT fields_count = count;
    if (!(seen & (1u << QUAD_POSITION))) order[fields_count++] = QUAD_POSITION;

    // Each fragment is fixed above; the maximum source is under 512 bytes.
    char source[512] = "struct QuadOutput {\n";
    for (UINT i = 0; i < fields_count; ++i) strcat(source, fields[order[i]]);
    strcat(source, "};\nQuadOutput main(uint id : SV_VertexID) {\n"
                   "float2 uv = float2(id & 1, id >> 1);\nQuadOutput result;\n");
    for (UINT i = 0; i < fields_count; ++i) strcat(source, assignments[order[i]]);
    strcat(source, "return result;\n}\n");

    ID3DBlob *diagnostics = NULL;
    HRESULT status = D3DCompile(source, strlen(source), "artemis-quad.hlsl", NULL, NULL,
                                "main", "vs_5_0", 0, 0, code, &diagnostics);
    report_diagnostics(diagnostics);
    if (FAILED(status)) return status;

    ID3D11ShaderReflection *vertex = NULL;
    status = D3DReflect(ID3D10Blob_GetBufferPointer(*code), ID3D10Blob_GetBufferSize(*code),
                        &IID_ID3D11ShaderReflection, (void **)&vertex);
    if (FAILED(status)) return status;
    D3D11_SHADER_DESC desc;
    status = vertex->lpVtbl->GetDesc(vertex, &desc);
    if (SUCCEEDED(status) && desc.OutputParameters != fields_count) status = E_FAIL;
    for (UINT i = 0; SUCCEEDED(status) && i < count; ++i) {
        D3D11_SIGNATURE_PARAMETER_DESC output;
        status = vertex->lpVtbl->GetOutputParameterDesc(vertex, i, &output);
        if (SUCCEEDED(status) && !matching_parameter(&inputs[i], &output)) {
            fprintf(stderr, "Generated vertex output does not match pixel input %s%u (register %u, mask 0x%x)\n",
                    inputs[i].SemanticName, inputs[i].SemanticIndex, inputs[i].Register, (unsigned)inputs[i].Mask);
            status = E_NOTIMPL;
        }
    }
    vertex->lpVtbl->Release(vertex);
    return status;
}

static HRESULT reflect_uniforms(ID3D11ShaderReflection *reflection, const char *name,
                                ArtDxbcHeader *header, ArtDxbcUniform *uniforms, unsigned char *defaults,
                                int legacy) {
    ID3D11ShaderReflectionConstantBuffer *buffer = reflection->lpVtbl->GetConstantBufferByName(reflection, name);
    D3D11_SHADER_BUFFER_DESC desc;
    const uint32_t base = header->constant_bytes;
    if (!buffer || FAILED(buffer->lpVtbl->GetDesc(buffer, &desc)) || !desc.Size ||
        desc.Size > ART_DXBC_MAX_CONSTANTS - base || (desc.Size & 15) ||
        desc.Variables > ART_DXBC_MAX_UNIFORMS - header->uniforms) return E_INVALIDARG;
    header->constant_bytes += desc.Size;
    for (UINT i = 0; i < desc.Variables; ++i) {
        ID3D11ShaderReflectionVariable *variable = buffer->lpVtbl->GetVariableByIndex(buffer, i);
        D3D11_SHADER_VARIABLE_DESC field;
        if (!variable || FAILED(variable->lpVtbl->GetDesc(variable, &field))) return E_INVALIDARG;
        if (!(field.uFlags & D3D_SVF_USED)) continue;
        ID3D11ShaderReflectionType *type = variable->lpVtbl->GetType(variable);
        D3D11_SHADER_TYPE_DESC shape;
        ArtDxbcUniform *uniform = &uniforms[header->uniforms];
        if (!type || FAILED(type->lpVtbl->GetDesc(type, &shape)) || shape.Type != D3D_SVT_FLOAT ||
            (shape.Class != D3D_SVC_SCALAR && shape.Class != D3D_SVC_VECTOR) ||
            !shape.Columns || shape.Columns > 4 ||
            shape.Elements > ART_DXBC_MAX_CONSTANTS / 16 ||
            (shape.Elements && (!defaults || (field.StartOffset & 15))) ||
            (field.StartOffset & 3) || (uint64_t)field.StartOffset + field.Size > desc.Size ||
            !copy_name(uniform->name, field.Name)) {
            fprintf(stderr, "Unsupported constant field: %s\n", field.Name ? field.Name : "<unnamed>");
            return E_NOTIMPL;
        }
        uniform->offset = base + field.StartOffset;
        uniform->components = shape.Columns | (shape.Elements << 8);
        const uint32_t span = art_dxbc_uniform_span(uniform->components);
        if (field.Size < span || field.Size > ((span + 15) & ~15u) ||
            (!shape.Elements && field.Size != span)) {
            fprintf(stderr, "Unsupported constant layout: %s\n", field.Name);
            return E_NOTIMPL;
        }
        if (defaults && field.DefaultValue) {
            memcpy(defaults + uniform->offset, field.DefaultValue, span);
        } else if (legacy && !shape.Elements && ((!strcmp(field.Name, "alpha") && shape.Columns == 1) ||
                   (!strcmp(field.Name, "colorMultiply") && shape.Columns == 3))) {
            const float identity[] = {1.0f, 1.0f, 1.0f};
            memcpy(defaults + uniform->offset, identity, span);
        }
        ++header->uniforms;
    }
    return S_OK;
}

static HRESULT write_effect(const void *data, uint32_t bytes, int paired, int legacy) {
    ID3D11ShaderReflection *reflection = NULL;
    ID3DBlob *vertex = NULL;
    uint32_t vertex_bytes = 0;
    HRESULT status = D3DReflect(data, bytes, &IID_ID3D11ShaderReflection, (void **)&reflection);
    if (FAILED(status)) return status;
    ArtDxbcHeader header = {paired ? ART_DXBC_EFFECT_PAIR_MAGIC : ART_DXBC_EFFECT_MAGIC, bytes, 0, 0, 0};
    ArtDxbcTexture textures[ART_DXBC_TEXTURE_SLOTS] = {0};
    ArtDxbcUniform uniforms[ART_DXBC_MAX_UNIFORMS] = {0};
    unsigned char defaults[ART_DXBC_MAX_CONSTANTS] = {0};
    const char *buffers[ART_DXBC_CONSTANT_SLOTS] = {0};
    uint32_t first_buffer_bytes = 0;
    D3D11_SHADER_DESC desc;
    status = reflection->lpVtbl->GetDesc(reflection, &desc);
    if (FAILED(status)) goto done;
    for (UINT i = 0; i < desc.BoundResources; ++i) {
        D3D11_SHADER_INPUT_BIND_DESC binding;
        status = reflection->lpVtbl->GetResourceBindingDesc(reflection, i, &binding);
        if (FAILED(status)) goto done;
        if (binding.Type == D3D_SIT_CBUFFER && binding.BindCount == 1 && binding.Name &&
            binding.BindPoint < (paired ? ART_DXBC_CONSTANT_SLOTS : 1) && !buffers[binding.BindPoint]) {
            buffers[binding.BindPoint] = binding.Name;
        } else if (binding.Type == D3D_SIT_TEXTURE && binding.Dimension == D3D_SRV_DIMENSION_TEXTURE2D &&
                   binding.BindCount == 1 && binding.BindPoint < ART_DXBC_TEXTURE_SLOTS &&
                   header.textures < ART_DXBC_TEXTURE_SLOTS &&
                   copy_name(textures[header.textures].name, binding.Name)) {
            textures[header.textures++].slot = binding.BindPoint;
        } else if (binding.Type != D3D_SIT_SAMPLER || binding.BindCount != 1 ||
                   binding.BindPoint >= (legacy ? ART_DXBC_TEXTURE_SLOTS : 1) ||
                   (binding.uFlags & D3D_SIF_COMPARISON_SAMPLER)) {
            fprintf(stderr, "Unsupported shader resource: %s\n", binding.Name ? binding.Name : "<unnamed>");
            status = E_NOTIMPL;
            goto done;
        }
    }
    for (UINT slot = 0; slot < ART_DXBC_CONSTANT_SLOTS; ++slot) {
        if (buffers[slot]) {
            status = reflect_uniforms(reflection, buffers[slot], &header, uniforms,
                                      paired ? defaults : NULL, legacy);
            if (FAILED(status)) goto done;
        }
        if (slot == 0) first_buffer_bytes = header.constant_bytes;
    }
    if (paired) {
        status = quad_vertex(reflection, desc.InputParameters, &vertex);
        if (FAILED(status)) goto done;
        const size_t length = ID3D10Blob_GetBufferSize(vertex);
        if (length < 4 || length > ART_DXBC_MAX_BYTECODE || memcmp(ID3D10Blob_GetBufferPointer(vertex), "DXBC", 4)) {
            status = E_FAIL;
            goto done;
        }
        vertex_bytes = (uint32_t)length;
    }
    uint32_t default_bytes = header.constant_bytes;
    while (default_bytes && !defaults[default_bytes - 1]) --default_bytes;
    if (fwrite(&header, sizeof(header), 1, stdout) != 1 ||
        fwrite(textures, sizeof(*textures), header.textures, stdout) != header.textures ||
        fwrite(uniforms, sizeof(*uniforms), header.uniforms, stdout) != header.uniforms ||
        fwrite(data, 1, bytes, stdout) != bytes ||
        (paired && (fwrite(&vertex_bytes, sizeof(vertex_bytes), 1, stdout) != 1 ||
                    fwrite(ID3D10Blob_GetBufferPointer(vertex), 1, vertex_bytes, stdout) != vertex_bytes)) ||
        ((default_bytes || buffers[1]) &&
            fwrite(defaults, 1, header.constant_bytes, stdout) != header.constant_bytes) ||
        (buffers[1] && fwrite(&first_buffer_bytes, sizeof(first_buffer_bytes), 1, stdout) != 1) ||
        fflush(stdout) != 0) status = E_FAIL;
done:
    if (vertex) ID3D10Blob_Release(vertex);
    reflection->lpVtbl->Release(reflection);
    return status;
}

int main(int argc, char **argv) {
    const int paired = argc == 2 && strcmp(argv[1], "effect-pair") == 0;
    const int effect = paired || (argc == 2 && strcmp(argv[1], "effect") == 0);
    if (argc != 2 || (!effect && strcmp(argv[1], "vs_5_0") && strcmp(argv[1], "ps_5_0"))) {
        fputs("Usage: artemis-hlsl-compile vs_5_0|ps_5_0|effect|effect-pair < source.hlsl > bytecode\n", stderr);
        return 2;
    }
#ifdef _WIN32
    if (_setmode(_fileno(stdin), _O_BINARY) == -1 || _setmode(_fileno(stdout), _O_BINARY) == -1) return 1;
#endif
    char *source = malloc(MAX_SOURCE + 1);
    if (!source) return 1;
    const size_t length = fread(source, 1, MAX_SOURCE + 1, stdin);
    if (ferror(stdin) || !length || length > MAX_SOURCE) {
        fputs("Invalid HLSL source length\n", stderr);
        free(source);
        return 1;
    }

    ID3DBlob *code = NULL, *diagnostics = NULL;
    const char *profile = effect ? "ps_5_0" : argv[1];
    int legacy = 0;
    HRESULT status = D3DCompile(source, length, "artemis.hlsl", NULL, NULL, "main", profile,
                                0, 0, &code, &diagnostics);
    if (paired && FAILED(status)) {
        ID3DBlob *legacy_code = NULL, *legacy_diagnostics = NULL;
        HRESULT legacy_status = D3DCompile(source, length, "artemis.hlsl", NULL, NULL, "ps", profile,
                                    D3DCOMPILE_ENABLE_BACKWARDS_COMPATIBILITY, 0,
                                    &legacy_code, &legacy_diagnostics);
        if (SUCCEEDED(legacy_status) && legacy_code) {
            if (code) ID3D10Blob_Release(code);
            if (diagnostics) ID3D10Blob_Release(diagnostics);
            code = legacy_code;
            diagnostics = legacy_diagnostics;
            status = legacy_status;
            legacy = 1;
        } else {
            if (legacy_code) ID3D10Blob_Release(legacy_code);
            report_diagnostics(legacy_diagnostics);
        }
    }
    free(source);
    report_diagnostics(diagnostics);
    int result = 1;
    if (SUCCEEDED(status) && code) {
        const void *data = ID3D10Blob_GetBufferPointer(code);
        const size_t bytes = ID3D10Blob_GetBufferSize(code);
        if (bytes >= 4 && bytes <= ART_DXBC_MAX_BYTECODE && memcmp(data, "DXBC", 4) == 0) {
            if (effect) {
                const HRESULT reflected = write_effect(data, (uint32_t)bytes, paired, legacy);
                result = FAILED(reflected);
                if (result) fprintf(stderr, "Shader reflection failed (0x%08lx)\n", (unsigned long)(DWORD)reflected);
            } else {
                result = !(fwrite(data, 1, bytes, stdout) == bytes && fflush(stdout) == 0);
            }
        } else {
            fputs("Compiler did not return valid DXBC\n", stderr);
        }
    } else {
        fprintf(stderr, "D3DCompile %s failed (0x%08lx)\n", profile, (unsigned long)(DWORD)status);
    }
    if (code) ID3D10Blob_Release(code);
    return result;
}
