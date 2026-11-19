#include <CoreVideo/CoreVideo.h>
#include "artemis_dxvk_metal_writer.h"
#include "../host-bindings/d3d11/artemis_dxvk_device.hpp"
#include "../host-bindings/d3d11/artemis_dxbc_effect.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <memory>
#include <type_traits>
#include <vector>

using dxvk::Com;
using PixelBuffer = std::unique_ptr<std::remove_pointer_t<CVPixelBufferRef>, decltype(&CVPixelBufferRelease)>;

static void check(bool success, const char *operation) {
    if (!success) {
        std::fprintf(stderr, "%s failed\n", operation);
        std::exit(1);
    }
}

static CVPixelBufferRef make_buffer(uint32_t width, uint32_t height) {
    CFDictionaryRef properties = CFDictionaryCreate(nullptr, nullptr, nullptr, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *keys[] = {kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey};
    const void *values[] = {properties, kCFBooleanTrue};
    CFDictionaryRef attributes = CFDictionaryCreate(nullptr, keys, values, 2,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CVPixelBufferRef buffer = nullptr;
    const auto status = CVPixelBufferCreate(nullptr, width, height, kCVPixelFormatType_32BGRA, attributes, &buffer);
    CFRelease(attributes);
    CFRelease(properties);
    check(status == kCVReturnSuccess, "create BGRA pixel buffer");
    return buffer;
}

static std::vector<char> load_bytecode(const char *path) {
    std::ifstream file(path, std::ios::binary | std::ios::ate);
    const auto bytes = file.tellg();
    check(bytes >= 4 && bytes <= ART_DXBC_MAX_BYTECODE, "get DXBC bytecode length");
    std::vector<char> result(static_cast<size_t>(bytes));
    file.seekg(0);
    check(bool(file.read(result.data(), bytes)) && std::memcmp(result.data(), "DXBC", 4) == 0,
          "read DXBC bytecode");
    return result;
}

static Com<ID3D11ShaderResourceView> make_sample(ID3D11Device *device, const uint8_t (&bgra)[4]) {
    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = desc.Height = desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.Usage = D3D11_USAGE_IMMUTABLE;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    const D3D11_SUBRESOURCE_DATA data = {bgra, 4, 0};
    Com<ID3D11Texture2D> texture;
    check(SUCCEEDED(device->CreateTexture2D(&desc, &data, &texture)), "create shader input texture");
    Com<ID3D11ShaderResourceView> view;
    check(SUCCEEDED(device->CreateShaderResourceView(texture.ptr(), nullptr, &view)), "create shader input view");
    return view;
}

static Com<ID3D11Buffer> bind_effect(ID3D11Device *device, ID3D11DeviceContext *context,
                                    const char *vsPath, const artemis::DxbcEffect &effect, uint32_t width, uint32_t height,
                                    ID3D11Texture2D *foreground) {
    const auto vs = load_bytecode(vsPath);
    const auto &ps = effect.bytecode();
    Com<ID3D11VertexShader> vertex;
    Com<ID3D11PixelShader> pixel;
    check(SUCCEEDED(device->CreateVertexShader(vs.data(), vs.size(), nullptr, &vertex)), "create native vertex shader");
    check(SUCCEEDED(device->CreatePixelShader(ps.data(), ps.size(), nullptr, &pixel)), "create native pixel shader");
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
    context->VSSetShader(vertex.ptr(), nullptr, 0);
    context->PSSetShader(pixel.ptr(), nullptr, 0);

    D3D11_RASTERIZER_DESC rasterizerDesc = {};
    rasterizerDesc.FillMode = D3D11_FILL_SOLID;
    rasterizerDesc.CullMode = D3D11_CULL_NONE;
    rasterizerDesc.DepthClipEnable = TRUE;
    Com<ID3D11RasterizerState> rasterizer;
    check(SUCCEEDED(device->CreateRasterizerState(&rasterizerDesc, &rasterizer)), "create rasterizer");
    context->RSSetState(rasterizer.ptr());
    const D3D11_VIEWPORT viewport = {0, 0, float(width), float(height), 0, 1};
    context->RSSetViewports(1, &viewport);

    D3D11_SAMPLER_DESC samplerDesc = {};
    samplerDesc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
    samplerDesc.AddressU = samplerDesc.AddressV = samplerDesc.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    samplerDesc.MaxAnisotropy = 1;
    samplerDesc.ComparisonFunc = D3D11_COMPARISON_NEVER;
    Com<ID3D11SamplerState> sampler;
    check(SUCCEEDED(device->CreateSamplerState(&samplerDesc, &sampler)), "create pixel sampler");
    ID3D11SamplerState *activeSampler = sampler.ptr();
    context->PSSetSamplers(0, 1, &activeSampler);

    const auto user = make_sample(device, {11, 7, 5, 255});
    Com<ID3D11ShaderResourceView> fore;
    check(SUCCEEDED(device->CreateShaderResourceView(foreground, nullptr, &fore)), "create shared input view");
    const auto mask = make_sample(device, {0, 0, 128, 255});
    ID3D11ShaderResourceView *textures[ART_DXBC_TEXTURE_SLOTS] = {};
    const auto bind_texture = [&](const char *name, ID3D11ShaderResourceView *view) {
        const int slot = effect.texture_slot(name);
        check(slot >= 0, "resolve reflected texture slot");
        textures[slot] = view;
    };
    bind_texture("textureFore", fore.ptr());
    bind_texture("textureMask", mask.ptr());
    bind_texture("textureUser", user.ptr());
    context->PSSetShaderResources(0, ART_DXBC_TEXTURE_SLOTS, textures);

    D3D11_BUFFER_DESC constantDesc = {};
    constantDesc.ByteWidth = effect.constant_bytes();
    constantDesc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    Com<ID3D11Buffer> constants;
    check(SUCCEEDED(device->CreateBuffer(&constantDesc, nullptr, &constants)), "create pixel constants");
    ID3D11Buffer *activeConstants = constants.ptr();
    context->PSSetConstantBuffers(0, 1, &activeConstants);
    return constants;
}

int main(int argc, char **argv) try {
    if (argc != 1 && argc != 3) {
        std::fputs("Usage: artemis-dxvk-native [vertex.dxbc pixel.ade]\n", stderr);
        return 2;
    }
    const bool drawEffect = argc == 3;
    artemis::DxvkDevice host;
    auto *device = host.device();
    auto *context = host.context();

    constexpr uint32_t width = 7, height = 5;
    PixelBuffer outputBuffer(make_buffer(width, height), CVPixelBufferRelease);
    const auto output = artemis::DxvkSurface::import(host, CVPixelBufferGetIOSurface(outputBuffer.get()), D3D11_BIND_RENDER_TARGET);
    std::unique_ptr<artemis::DxvkSurface> foreground;
    using MetalWriter = std::unique_ptr<ArtMetalWriter, decltype(&art_metal_writer_destroy)>;
    MetalWriter writer(nullptr, art_metal_writer_destroy);
    MetalWriter presenter(nullptr, art_metal_writer_destroy);
    PixelBuffer presentation(nullptr, CVPixelBufferRelease);
    Com<ID3D11RenderTargetView> target;
    check(SUCCEEDED(device->CreateRenderTargetView(output->texture(), nullptr, &target)), "create D3D11 target");
    Com<ID3D11Buffer> constants;
    std::unique_ptr<artemis::DxbcEffect> effect;
    std::vector<char> params;
    if (drawEffect) {
        std::ifstream file(argv[2], std::ios::binary);
        effect = std::make_unique<artemis::DxbcEffect>(file);
        PixelBuffer foregroundBuffer(make_buffer(width, height), CVPixelBufferRelease);
        auto surface = CVPixelBufferGetIOSurface(foregroundBuffer.get());
        foreground = artemis::DxvkSurface::import(host, surface, D3D11_BIND_SHADER_RESOURCE);
        auto *metalDevice = host.metal_device();
        writer.reset(art_metal_writer_create(metalDevice, surface, width, height));
        presentation.reset(make_buffer(width, height));
        presenter.reset(art_metal_writer_create(metalDevice, CVPixelBufferGetIOSurface(presentation.get()), width, height));
        check(writer && presenter, "create Metal foreground and presentation targets");
        host.prepare_input(*foreground);
        constants = bind_effect(device, context, argv[1], *effect, width, height, foreground->texture());
        params.resize(effect->constant_bytes());
        const float tint[4] = {0, 1, 0.5f, 1};
        check(effect->set_uniform(params, "tint", tint, 4), "bind tint by reflection");
    }

    const float clearColors[][4] = {{1, 0, 0, 1}, {0, 1, 1, 1}};
    const uint8_t clearPixels[][4] = {{0, 0, 255, 255}, {255, 255, 0, 255}};
    const uint8_t metalColors[][4] = {{17, 34, 51, 255}, {17, 68, 102, 255}};
    const uint8_t effectPixels[][4] = {{24, 24, 5, 255}, {37, 41, 5, 255}, {26, 34, 0, 255}};
    const size_t frameCount = drawEffect ? 3 : 2;
    for (size_t i = 0; i < frameCount; ++i) {
        if (drawEffect) {
            if (i < 2) check(art_metal_writer_clear(writer.get(), metalColors[i]), "render Metal foreground");
            const float param = i < 2 ? 1.0f : 0.0f;
            check(effect->set_uniform(params, "param", &param, 1), "bind param by reflection");
            context->UpdateSubresource(constants.ptr(), 0, nullptr, params.data(), 0, 0);
            ID3D11RenderTargetView *activeTarget = target.ptr();
            context->OMSetRenderTargets(1, &activeTarget, nullptr);
            context->Draw(4, 0);
            context->OMSetRenderTargets(0, nullptr, nullptr);
        } else {
            context->ClearRenderTargetView(target.ptr(), clearColors[i]);
        }
        host.finish_output(*output);
        if (presenter) {
            check(art_metal_writer_copy(presenter.get(), CVPixelBufferGetIOSurface(outputBuffer.get())),
                  "consume DXVK output in Metal");
        }
        const auto pixelsBuffer = presenter ? presentation.get() : outputBuffer.get();
        check(CVPixelBufferLockBaseAddress(pixelsBuffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess, "lock pixel buffer");
        const auto *pixels = static_cast<const uint8_t *>(CVPixelBufferGetBaseAddress(pixelsBuffer));
        const size_t stride = CVPixelBufferGetBytesPerRow(pixelsBuffer);
        check(pixels != nullptr, "access pixel buffer");
        for (uint32_t y = 0; y < height; ++y) {
            for (uint32_t x = 0; x < width; ++x) {
                const auto *actual = pixels + y * stride + x * 4;
                if (std::memcmp(actual, drawEffect ? effectPixels[i] : clearPixels[i], 4) != 0) {
                    std::fprintf(stderr, "Frame %zu pixel (%u, %u): BGRA %u,%u,%u,%u\n",
                        i + 1, x, y, actual[0], actual[1], actual[2], actual[3]);
                    check(false, "compare shared D3D11 output pixel");
                }
            }
        }
        check(CVPixelBufferUnlockBaseAddress(pixelsBuffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
              "unlock pixel buffer");
    }
    context->ClearState();
    std::puts(drawEffect ? "Native DXVK IOSurface: Metal presented all three reflected HLSL frames"
                         : "Native DXVK IOSurface: two D3D11 frames visible in the original pixel buffer");
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return 1;
}
