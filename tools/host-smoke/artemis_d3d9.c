#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <d3dcompiler.h>
#include <fcntl.h>
#include <io.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { probe_size = 4 };

static const char source[] =
    "sampler2D samplerFore : register(s0);\n"
    "sampler2D samplerBack : register(s1);\n"
    "float alpha : register(c0);\n"
    "float4 tint : register(c1);\n"
    "void ps(float2 uv0 : TEXCOORD0, float2 uv1 : TEXCOORD1, out float4 result : COLOR0) {\n"
    "    float3 fore = tex2D(samplerFore, uv1).rgb * tint.rgb;\n"
    "    float3 back = tex2D(samplerBack, uv0).rgb * 0.25;\n"
    "    result = float4(fore + back, alpha);\n"
    "}\n";

static const DWORD foreground[] = { 0xff804020, 0xff206040, 0xff4080c0, 0xffc02080 };
static const DWORD background[] = { 0xff102040, 0xff401020, 0xff204010, 0xff304050 };
static const DWORD clear_color = 0xff101820;

static void check(HRESULT result, const char *operation) {
    if (FAILED(result)) {
        fprintf(stderr, "%s failed: 0x%08lx\n", operation, (unsigned long)(DWORD)result);
        exit(1);
    }
}

#define CHECK(call) check((call), #call)

static IDirect3DTexture9 *make_texture(IDirect3DDevice9 *device, const DWORD *pixels) {
    IDirect3DTexture9 *texture = NULL;
    D3DLOCKED_RECT lock;
    CHECK(IDirect3DDevice9_CreateTexture(device, 2, 2, 1, 0, D3DFMT_A8R8G8B8,
                                        D3DPOOL_MANAGED, &texture, NULL));
    CHECK(IDirect3DTexture9_LockRect(texture, 0, &lock, NULL, 0));
    for (int y = 0; y < 2; ++y) {
        memcpy((char *)lock.pBits + y * lock.Pitch, pixels + y * 2, 2 * sizeof(DWORD));
    }
    CHECK(IDirect3DTexture9_UnlockRect(texture, 0));
    return texture;
}

static IDirect3DPixelShader9 *compile_shader(IDirect3DDevice9 *device, const char *profile,
                                           DWORD token) {
    ID3DBlob *bytecode = NULL, *messages = NULL;
    HRESULT result = D3DCompile(source, sizeof(source) - 1, "artemis_d3d9.hlsl", NULL, NULL,
                                "ps", profile, 0, 0, &bytecode, &messages);
    if (messages) {
        fputs((const char *)ID3D10Blob_GetBufferPointer(messages), stderr);
        ID3D10Blob_Release(messages);
    }
    check(result, profile);
    const DWORD *words = ID3D10Blob_GetBufferPointer(bytecode);
    if (ID3D10Blob_GetBufferSize(bytecode) < sizeof(*words) || words[0] != token) {
        fprintf(stderr, "%s did not compile to the expected D3D9 shader format\n", profile);
        exit(1);
    }
    fprintf(stderr, "%s: compiled D3D9 bytecode (%zu bytes)\n", profile, ID3D10Blob_GetBufferSize(bytecode));
    IDirect3DPixelShader9 *shader = NULL;
    CHECK(IDirect3DDevice9_CreatePixelShader(device, words, &shader));
    ID3D10Blob_Release(bytecode);
    return shader;
}

static int channel(DWORD pixel, int shift) {
    return (int)((pixel >> shift) & 255);
}

static void write_bytes(const void *bytes, size_t size) {
    if (fwrite(bytes, 1, size, stdout) != size) {
        fputs("Cannot write the BGRA frame stream\n", stderr);
        exit(1);
    }
}

static BOOL check_pixels(IDirect3DSurface9 *pixels, const char *profile, uint32_t width,
                         uint32_t height, BOOL dim_red, uint32_t frame_id) {
    D3DLOCKED_RECT lock;
    BOOL matched = TRUE;
    CHECK(IDirect3DSurface9_LockRect(pixels, &lock, NULL, D3DLOCK_READONLY));
    for (uint32_t y = 0; y < height; ++y) {
        const DWORD *row = (const DWORD *)((const char *)lock.pBits + y * lock.Pitch);
        for (uint32_t x = 0; x < width; ++x) {
            uint32_t row_idx = (y >= height / 2) * 2;
            uint32_t column = x >= width / 2;
            DWORD fore = foreground[row_idx + 1 - column];
            DWORD back = background[row_idx + column];
            DWORD actual = row[x];
            for (int shift = 0; shift <= 24; shift += 8) {
                int tint_eighths = shift == 16 && !dim_red ? 4 : 2;
                int expected = shift == 24 ? 255 :
                    (tint_eighths * channel(fore, shift) + channel(back, shift) +
                     4 * channel(clear_color, shift)) / 8;
                if (abs(channel(actual, shift) - expected) > 1) {
                    if (matched) {
                        fprintf(stderr, "%s pixel (%u,%u) = %08lx; channel %d expected %d\n",
                                profile, x, y, (unsigned long)actual, shift, expected);
                    }
                    matched = FALSE;
                }
            }
        }
    }
    if (matched && frame_id) {
        /* The Windows probe targets little-endian D3D9. Copy only the valid BGRA pixels in each row. */
        const uint32_t header[] = { frame_id, width, height };
        write_bytes(header, sizeof(header));
        for (uint32_t y = 0; y < height; ++y) {
            write_bytes((const char *)lock.pBits + y * lock.Pitch, width * sizeof(DWORD));
        }
    }
    CHECK(IDirect3DSurface9_UnlockRect(pixels));
    if (matched && frame_id && fflush(stdout) != 0) {
        fputs("Cannot flush the BGRA frame stream\n", stderr);
        exit(1);
    }
    if (matched) fprintf(stderr, "%s: passed %u RGBA pixels\n", profile, width * height);
    return matched;
}

static BOOL draw(IDirect3DDevice9 *device, const char *profile, DWORD token,
                 uint32_t width, uint32_t height, BOOL dim_red, uint32_t frame_id) {
    struct Vertex { float x, y, z, rhw, u0, v0, u1, v1; };
    /* XYZRHW positions use D3D9 pixel centers. The foreground UV flips X. */
    const float right = width - 0.5f, bottom = height - 0.5f;
    const struct Vertex quad[] = {
        { -0.5f, -0.5f, 0, 1, 0, 0, 1, 0 },
        { right, -0.5f, 0, 1, 1, 0, 0, 0 },
        { -0.5f, bottom, 0, 1, 0, 1, 1, 1 },
        { right, bottom, 0, 1, 1, 1, 0, 1 },
    };
    const float constants[] = { 0.5f, 0, 0, 0, dim_red ? 0.5f : 1, 0.5f, 0.5f, 1 };
    IDirect3DSurface9 *target = NULL, *pixels = NULL;
    CHECK(IDirect3DDevice9_CreateRenderTarget(device, width, height, D3DFMT_A8R8G8B8,
                                            D3DMULTISAMPLE_NONE, 0, FALSE, &target, NULL));
    CHECK(IDirect3DDevice9_CreateOffscreenPlainSurface(device, width, height, D3DFMT_A8R8G8B8,
                                                     D3DPOOL_SYSTEMMEM, &pixels, NULL));
    CHECK(IDirect3DDevice9_SetRenderTarget(device, 0, target));
    IDirect3DPixelShader9 *shader = compile_shader(device, profile, token);
    CHECK(IDirect3DDevice9_SetPixelShader(device, shader));
    CHECK(IDirect3DDevice9_SetPixelShaderConstantF(device, 0, constants, 2));
    CHECK(IDirect3DDevice9_Clear(device, 0, NULL, D3DCLEAR_TARGET, clear_color, 1, 0));
    CHECK(IDirect3DDevice9_BeginScene(device));
    CHECK(IDirect3DDevice9_DrawPrimitiveUP(device, D3DPT_TRIANGLESTRIP, 2, quad, sizeof(quad[0])));
    CHECK(IDirect3DDevice9_EndScene(device));
    CHECK(IDirect3DDevice9_GetRenderTargetData(device, target, pixels));
    BOOL matched = check_pixels(pixels, profile, width, height, dim_red, frame_id);
    CHECK(IDirect3DDevice9_SetPixelShader(device, NULL));
    IDirect3DPixelShader9_Release(shader);
    IDirect3DSurface9_Release(pixels);
    IDirect3DSurface9_Release(target);
    return matched;
}

int main(int argc, char **argv) {
    BOOL stream = argc == 2 && strcmp(argv[1], "--stream-bgra") == 0;
    if (argc != 1 && !stream) {
        fputs("Usage: artemis-d3d9.exe [--stream-bgra]\n", stderr);
        return 2;
    }
    if (stream && _setmode(_fileno(stdout), _O_BINARY) == -1) {
        fputs("Cannot set the BGRA stream to binary mode\n", stderr);
        return 1;
    }
    HWND window = CreateWindowExA(0, "STATIC", "Artemis D3D9 probe", WS_POPUP,
                                  0, 0, probe_size, probe_size, NULL, NULL, GetModuleHandleA(NULL), NULL);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!window || !d3d) {
        fputs("Cannot create the D3D9 probe device\n", stderr);
        return 1;
    }
    D3DADAPTER_IDENTIFIER9 adapter;
    CHECK(IDirect3D9_GetAdapterIdentifier(d3d, D3DADAPTER_DEFAULT, 0, &adapter));
    fprintf(stderr, "D3D9 adapter: %s\n", adapter.Description);
    D3DPRESENT_PARAMETERS present = {
        .BackBufferWidth = probe_size, .BackBufferHeight = probe_size,
        .SwapEffect = D3DSWAPEFFECT_DISCARD, .hDeviceWindow = window, .Windowed = TRUE,
    };
    IDirect3DDevice9 *device = NULL;
    CHECK(IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, window,
                                D3DCREATE_HARDWARE_VERTEXPROCESSING, &present, &device));
    IDirect3DTexture9 *fore = make_texture(device, foreground);
    IDirect3DTexture9 *back = make_texture(device, background);
    CHECK(IDirect3DDevice9_SetTexture(device, 0, (IDirect3DBaseTexture9 *)fore));
    CHECK(IDirect3DDevice9_SetTexture(device, 1, (IDirect3DBaseTexture9 *)back));
    for (DWORD sampler = 0; sampler < 2; ++sampler) {
        CHECK(IDirect3DDevice9_SetSamplerState(device, sampler, D3DSAMP_MINFILTER, D3DTEXF_POINT));
        CHECK(IDirect3DDevice9_SetSamplerState(device, sampler, D3DSAMP_MAGFILTER, D3DTEXF_POINT));
    }
    CHECK(IDirect3DDevice9_SetFVF(device, D3DFVF_XYZRHW | D3DFVF_TEX2));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_CULLMODE, D3DCULL_NONE));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_ZENABLE, FALSE));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_ALPHABLENDENABLE, TRUE));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_SEPARATEALPHABLENDENABLE, TRUE));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_SRCBLENDALPHA, D3DBLEND_ONE));
    CHECK(IDirect3DDevice9_SetRenderState(device, D3DRS_DESTBLENDALPHA, D3DBLEND_INVSRCALPHA));
    BOOL ps2 = draw(device, "ps_2_0", UINT32_C(0xffff0200), probe_size, probe_size, FALSE, stream ? 1 : 0);
    BOOL ps3 = draw(device, "ps_3_0", UINT32_C(0xffff0300), probe_size, probe_size, stream, stream ? 2 : 0);
    BOOL resized = !stream || draw(device, "ps_3_0", UINT32_C(0xffff0300), 6, 8, FALSE, 3);
    IDirect3DTexture9_Release(back);
    IDirect3DTexture9_Release(fore);
    IDirect3DDevice9_Release(device);
    IDirect3D9_Release(d3d);
    DestroyWindow(window);
    return ps2 && ps3 && resized ? 0 : 1;
}
