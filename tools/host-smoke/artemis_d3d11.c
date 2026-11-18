#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include "../host-bindings/d3d11/artemis_shader_pass.h"

#include <fcntl.h>
#include <io.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char pixel_source[] =
    "struct Varyings { float4 position : SV_Position; float2 uv0 : TEXCOORD0; float2 uv1 : TEXCOORD1; };\n"
    "Texture2D textureFore : register(t0);\n"
    "Texture2D textureBack : register(t1);\n"
    "SamplerState samplerPoint : register(s0);\n"
    "cbuffer Params : register(b0) { float gain; float4 tint; };\n"
    "float4 main(Varyings input) : SV_Target {\n"
    "    float3 fore = textureFore.Sample(samplerPoint, input.uv1).rgb * tint.rgb;\n"
    "    float3 back = textureBack.Sample(samplerPoint, input.uv0).rgb * 0.25;\n"
    "    return float4((fore + back) * gain, tint.a);\n"
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

static int channel(uint32_t pixel, int shift) {
    return (int)((pixel >> shift) & 255);
}

static int check_pixels(const uint32_t *pixels, unsigned width, unsigned height, BOOL dim_red) {
    for (unsigned y = 0; y < height; ++y) {
        for (unsigned x = 0; x < width; ++x) {
            unsigned row_index = (y >= height / 2) * 2;
            unsigned column = x >= width / 2;
            uint32_t fore = foreground[row_index + 1 - column];
            uint32_t back = background[row_index + column];
            uint32_t pixel = pixels[y * width + x];
            for (int shift = 0; shift <= 24; shift += 8) {
                int expected = shift == 24 ? 255 :
                    ((shift == 16 && !dim_red ? 4 : 2) * channel(fore, shift) + channel(back, shift) +
                     4 * channel(clear_color, shift)) / 8;
                if (abs(channel(pixel, shift) - expected) > 1) {
                    fprintf(stderr, "ps_5_0 pixel (%u,%u) = %08lx; channel %d expected %d\n",
                            x, y, (unsigned long)pixel, shift, expected);
                    return 1;
                }
            }
        }
    }
    fprintf(stderr, "ps_5_0: passed %u RGBA pixels\n", width * height);
    return 0;
}

static int draw(ArtD3D11Pass *pass, ArtD3D11Program *program, uint32_t width, uint32_t height,
                BOOL dim_red, uint32_t frame_id) {
    const float tint[] = { dim_red ? 0.5f : 1, 0.5f, 0.5f, 0.5f };
    const float gain = 1;
    uint8_t constants[32] = { 0 };
    if (art_d3d11_constant_bytes(program) != sizeof(constants) ||
        art_d3d11_set_uniform(program, "gain", &gain, 1, constants, sizeof(constants)) != S_OK ||
        art_d3d11_set_uniform(program, "tint", tint, 4, constants, sizeof(constants)) != S_OK) {
        fputs("ps_5_0 constants did not match the original shader layout\n", stderr);
        return 1;
    }
    const ArtD3D11Draw draw = {
        .width = width, .height = height,
        .clear_rgba = { 16 / 255.0f, 24 / 255.0f, 32 / 255.0f, 1 },
        .quad = { { { -1, 1 }, { 0, 0 }, { 1, 0 } }, { { 1, 1 }, { 1, 0 }, { 0, 0 } },
                  { { -1, -1 }, { 0, 1 }, { 1, 1 } }, { { 1, -1 }, { 1, 1 }, { 0, 1 } } },
        .textures = { { 2, 2, 2 * sizeof(uint32_t), sizeof(foreground), foreground },
                      { 2, 2, 2 * sizeof(uint32_t), sizeof(background), background } },
        .constants = constants, .constant_bytes = sizeof(constants), .alpha_blend = TRUE, .point_sampler = TRUE,
    };
    uint32_t pixels[48];
    check(art_d3d11_pass_draw(pass, program, &draw, pixels, sizeof(pixels)), "D3D11 effect pass");
    int result = check_pixels(pixels, width, height, dim_red);
    if (!result && frame_id) {
        const uint32_t header[] = { frame_id, width, height };
        if (fwrite(header, sizeof(header), 1, stdout) != 1 ||
            fwrite(pixels, sizeof(*pixels), width * height, stdout) != width * height) {
            fputs("Cannot write the BGRA frame stream\n", stderr);
            result = 1;
        }
    }
    return result;
}

int main(int argc, char **argv) {
    const BOOL stream = argc == 2 && strcmp(argv[1], "--stream-bgra") == 0;
    if (argc != 1 && !stream) {
        fputs("Usage: artemis-d3d11.exe [--stream-bgra]\n", stderr);
        return 2;
    }
    if (stream && _setmode(_fileno(stdout), _O_BINARY) == -1) return 1;

    ArtD3D11Pass *pass = NULL;
    check(art_d3d11_pass_create(&pass), "D3D11 effect device");
    fprintf(stderr, "D3D11 device: feature level 11_0\n");

    ArtD3D11Program *program = NULL;
    ID3DBlob *diagnostics = NULL;
    HRESULT status = art_d3d11_pass_compile(pass, pixel_source, sizeof(pixel_source) - 1, &program, &diagnostics);
    if (diagnostics) {
        fwrite(ID3D10Blob_GetBufferPointer(diagnostics), 1, ID3D10Blob_GetBufferSize(diagnostics), stderr);
        ID3D10Blob_Release(diagnostics);
    }
    check(status, "ps_5_0");
    fprintf(stderr, "ps_5_0: compiled DXBC\n");

    int result = draw(pass, program, 4, 4, FALSE, stream ? 1 : 0);
    if (stream && !result) {
        result = draw(pass, program, 4, 4, TRUE, 2);
        if (!result) result = draw(pass, program, 6, 8, FALSE, 3);
    }
    art_d3d11_program_destroy(program);
    art_d3d11_pass_destroy(pass);
    return result;
}
