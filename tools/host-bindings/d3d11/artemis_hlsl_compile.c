#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <d3dcompiler.h>

#include <fcntl.h>
#include <io.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { MAX_SOURCE = 1024 * 1024, MAX_BYTECODE = 16 * 1024 * 1024 };

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "vs_5_0") && strcmp(argv[1], "ps_5_0"))) {
        fputs("Usage: artemis-hlsl-compile.exe vs_5_0|ps_5_0 < source.hlsl > shader.dxbc\n", stderr);
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
    const HRESULT status = D3DCompile(source, length, "artemis.hlsl", NULL, NULL, "main", argv[1],
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
        if (bytes >= 4 && bytes <= MAX_BYTECODE && memcmp(data, "DXBC", 4) == 0) {
            result = !(fwrite(data, 1, bytes, stdout) == bytes && fflush(stdout) == 0);
        } else {
            fputs("Compiler did not return valid DXBC\n", stderr);
        }
    } else {
        fprintf(stderr, "D3DCompile %s failed (0x%08lx)\n", argv[1], (unsigned long)(DWORD)status);
    }
    if (code) ID3D10Blob_Release(code);
    return result;
}
