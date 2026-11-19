#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <d3dcompiler.h>
#include <d3d11shader.h>
#include "artemis_dxbc_effect.h"

#include <fcntl.h>
#include <io.h>
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

static HRESULT reflect_uniforms(ID3D11ShaderReflection *reflection, const char *name,
                                ArtDxbcHeader *header, ArtDxbcUniform *uniforms) {
    ID3D11ShaderReflectionConstantBuffer *buffer = reflection->lpVtbl->GetConstantBufferByName(reflection, name);
    D3D11_SHADER_BUFFER_DESC desc;
    if (!buffer || FAILED(buffer->lpVtbl->GetDesc(buffer, &desc)) || desc.Size > ART_DXBC_MAX_CONSTANTS ||
        (desc.Size & 15) || desc.Variables > ART_DXBC_MAX_UNIFORMS) return E_INVALIDARG;
    header->constant_bytes = desc.Size;
    for (UINT i = 0; i < desc.Variables; ++i) {
        ID3D11ShaderReflectionVariable *variable = buffer->lpVtbl->GetVariableByIndex(buffer, i);
        D3D11_SHADER_VARIABLE_DESC field;
        if (!variable || FAILED(variable->lpVtbl->GetDesc(variable, &field))) return E_INVALIDARG;
        if (!(field.uFlags & D3D_SVF_USED)) continue;
        ID3D11ShaderReflectionType *type = variable->lpVtbl->GetType(variable);
        D3D11_SHADER_TYPE_DESC shape;
        ArtDxbcUniform *uniform = &uniforms[header->uniforms];
        if (!type || FAILED(type->lpVtbl->GetDesc(type, &shape)) || shape.Type != D3D_SVT_FLOAT ||
            (shape.Class != D3D_SVC_SCALAR && shape.Class != D3D_SVC_VECTOR) || shape.Elements ||
            !shape.Columns || shape.Columns > 4 || field.Size != shape.Columns * 4 ||
            (field.StartOffset & 3) || (uint64_t)field.StartOffset + field.Size > desc.Size ||
            !copy_name(uniform->name, field.Name)) {
            fprintf(stderr, "Unsupported b0 field: %s\n", field.Name ? field.Name : "<unnamed>");
            return E_NOTIMPL;
        }
        uniform->offset = field.StartOffset;
        uniform->components = shape.Columns;
        ++header->uniforms;
    }
    return S_OK;
}

static HRESULT write_effect(const void *data, uint32_t bytes) {
    ID3D11ShaderReflection *reflection = NULL;
    HRESULT status = D3DReflect(data, bytes, &IID_ID3D11ShaderReflection, (void **)&reflection);
    if (FAILED(status)) return status;
    ArtDxbcHeader header = {ART_DXBC_EFFECT_MAGIC, bytes, 0, 0, 0};
    ArtDxbcTexture textures[ART_DXBC_TEXTURE_SLOTS] = {0};
    ArtDxbcUniform uniforms[ART_DXBC_MAX_UNIFORMS] = {0};
    D3D11_SHADER_DESC desc;
    status = reflection->lpVtbl->GetDesc(reflection, &desc);
    if (FAILED(status)) goto done;
    for (UINT i = 0; i < desc.BoundResources; ++i) {
        D3D11_SHADER_INPUT_BIND_DESC binding;
        status = reflection->lpVtbl->GetResourceBindingDesc(reflection, i, &binding);
        if (FAILED(status)) goto done;
        if (binding.Type == D3D_SIT_CBUFFER && binding.BindPoint == 0 && binding.BindCount == 1) {
            status = reflect_uniforms(reflection, binding.Name, &header, uniforms);
            if (FAILED(status)) goto done;
        } else if (binding.Type == D3D_SIT_TEXTURE && binding.Dimension == D3D_SRV_DIMENSION_TEXTURE2D &&
                   binding.BindCount == 1 && binding.BindPoint < ART_DXBC_TEXTURE_SLOTS &&
                   header.textures < ART_DXBC_TEXTURE_SLOTS &&
                   copy_name(textures[header.textures].name, binding.Name)) {
            textures[header.textures++].slot = binding.BindPoint;
        } else if (binding.Type != D3D_SIT_SAMPLER || binding.BindPoint || binding.BindCount != 1) {
            fprintf(stderr, "Unsupported shader resource: %s\n", binding.Name ? binding.Name : "<unnamed>");
            status = E_NOTIMPL;
            goto done;
        }
    }
    if (fwrite(&header, sizeof(header), 1, stdout) != 1 ||
        fwrite(textures, sizeof(*textures), header.textures, stdout) != header.textures ||
        fwrite(uniforms, sizeof(*uniforms), header.uniforms, stdout) != header.uniforms ||
        fwrite(data, 1, bytes, stdout) != bytes || fflush(stdout) != 0) status = E_FAIL;
done:
    reflection->lpVtbl->Release(reflection);
    return status;
}

int main(int argc, char **argv) {
    const int effect = argc == 2 && strcmp(argv[1], "effect") == 0;
    if (argc != 2 || (!effect && strcmp(argv[1], "vs_5_0") && strcmp(argv[1], "ps_5_0"))) {
        fputs("Usage: artemis-hlsl-compile.exe vs_5_0|ps_5_0|effect < source.hlsl > bytecode\n", stderr);
        return 2;
    }
    if (_setmode(_fileno(stdin), _O_BINARY) == -1 || _setmode(_fileno(stdout), _O_BINARY) == -1) return 1;
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
    const HRESULT status = D3DCompile(source, length, "artemis.hlsl", NULL, NULL, "main", profile,
                                      0, 0, &code, &diagnostics);
    free(source);
    if (diagnostics) {
        size_t bytes = ID3D10Blob_GetBufferSize(diagnostics);
        fwrite(ID3D10Blob_GetBufferPointer(diagnostics), 1, bytes, stderr);
        ID3D10Blob_Release(diagnostics);
    }
    int result = 1;
    if (SUCCEEDED(status) && code) {
        const void *data = ID3D10Blob_GetBufferPointer(code);
        const size_t bytes = ID3D10Blob_GetBufferSize(code);
        if (bytes >= 4 && bytes <= ART_DXBC_MAX_BYTECODE && memcmp(data, "DXBC", 4) == 0) {
            if (effect) {
                const HRESULT reflected = write_effect(data, (uint32_t)bytes);
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
