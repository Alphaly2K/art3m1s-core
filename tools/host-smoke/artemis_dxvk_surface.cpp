#include <CoreVideo/CoreVideo.h>
#include "artemis_dxvk_metal_writer.h"
#include "../host-bindings/d3d11/artemis_dxvk_device.hpp"
#include "../host-bindings/d3d11/artemis_dxvk_pass.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <type_traits>

using dxvk::Com;
using PixelBuffer = std::unique_ptr<std::remove_pointer_t<CVPixelBufferRef>, decltype(&CVPixelBufferRelease)>;

static void check(bool success, const char *operation) {
    if (!success) {
        std::fprintf(stderr, "%s failed\n", operation);
        std::exit(1);
    }
}

static PixelBuffer make_buffer(uint32_t width, uint32_t height) {
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
    return PixelBuffer(buffer, CVPixelBufferRelease);
}

static void check_pixels(CVPixelBufferRef buffer, size_t frame, const uint8_t (&expected)[4]) {
    check(CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess, "lock pixel buffer");
    const auto *pixels = static_cast<const uint8_t *>(CVPixelBufferGetBaseAddress(buffer));
    const auto stride = CVPixelBufferGetBytesPerRow(buffer);
    check(pixels != nullptr, "access pixel buffer");
    for (size_t y = 0; y < CVPixelBufferGetHeight(buffer); ++y) {
        for (size_t x = 0; x < CVPixelBufferGetWidth(buffer); ++x) {
            const auto *actual = pixels + y * stride + x * 4;
            if (std::memcmp(actual, expected, 4) != 0) {
                std::fprintf(stderr, "Frame %zu pixel (%zu, %zu): BGRA %u,%u,%u,%u\n",
                    frame, x, y, actual[0], actual[1], actual[2], actual[3]);
                check(false, "compare shared D3D11 output pixel");
            }
        }
    }
    check(CVPixelBufferUnlockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
          "unlock pixel buffer");
}

static void clear_surfaces() {
    artemis::DxvkDevice host;
    const auto buffer = make_buffer(7, 5);
    const auto output = artemis::DxvkSurface::import(host, CVPixelBufferGetIOSurface(buffer.get()), D3D11_BIND_RENDER_TARGET);
    Com<ID3D11RenderTargetView> target;
    check(SUCCEEDED(host.device()->CreateRenderTargetView(output->texture(), nullptr, &target)), "create D3D11 target");
    const float colors[][4] = {{1, 0, 0, 1}, {0, 1, 1, 1}};
    const uint8_t pixels[][4] = {{0, 0, 255, 255}, {255, 255, 0, 255}};
    for (size_t i = 0; i < 2; ++i) {
        host.context()->ClearRenderTargetView(target.ptr(), colors[i]);
        host.finish_output(*output);
        check_pixels(buffer.get(), i + 1, pixels[i]);
    }
    std::puts("Native DXVK IOSurface: two D3D11 frames visible in the original pixel buffer");
}

static void draw_effect(const char *vertex, const char *pixel) {
    constexpr uint32_t width = 7, height = 5;
    const auto foreground = make_buffer(width, height);
    const auto output = make_buffer(width, height);
    const auto presentation = make_buffer(width, height);
    const auto source_surface = CVPixelBufferGetIOSurface(foreground.get());
    const auto output_surface = CVPixelBufferGetIOSurface(output.get());
    std::unique_ptr<ArtDxvkPass, decltype(&art_dxvk_pass_destroy)> pass(
        art_dxvk_pass_create(vertex, pixel, source_surface, output_surface), art_dxvk_pass_destroy);
    check(bool(pass), "create DXVK effect pass");
    auto *device = art_dxvk_pass_metal_device(pass.get());
    using Writer = std::unique_ptr<ArtMetalWriter, decltype(&art_metal_writer_destroy)>;
    Writer writer(art_metal_writer_create(device, source_surface, width, height), art_metal_writer_destroy);
    Writer presenter(art_metal_writer_create(device, CVPixelBufferGetIOSurface(presentation.get()), width, height),
                     art_metal_writer_destroy);
    check(writer && presenter, "create Metal foreground and presentation targets");
    const uint8_t colors[][4] = {{17, 34, 51, 255}, {17, 68, 102, 255}};
    const uint8_t pixels[][4] = {{24, 24, 5, 255}, {37, 41, 5, 255}, {26, 34, 0, 255}};
    const uint8_t mask[] = {128, 0, 0, 255}, user[] = {5, 7, 11, 255};
    const float tint[] = {0, 1, 0.5f, 1};
    for (size_t i = 0; i < 3; ++i) {
        if (i < 2) check(art_metal_writer_clear(writer.get(), colors[i]), "render Metal foreground");
        const float param = i < 2 ? 1.0f : 0.0f;
        const ArtDxvkUniform uniforms[] = {
            {reinterpret_cast<const uint8_t *>("tint"), 4, tint, 4},
            {reinterpret_cast<const uint8_t *>("param"), 5, &param, 1},
        };
        check(art_dxvk_pass_draw(pass.get(), {mask, 1, 1}, {user, 1, 1}, uniforms, 2) == 0, "render HLSL effect");
        check(art_metal_writer_copy(presenter.get(), output_surface), "consume DXVK output in Metal");
        check_pixels(presentation.get(), i + 1, pixels[i]);
    }
    std::puts("Native DXVK IOSurface: Metal presented all three reflected HLSL frames");
}

int main(int argc, char **argv) try {
    if (argc == 1) clear_surfaces();
    else if (argc == 3) draw_effect(argv[1], argv[2]);
    else {
        std::fputs("Usage: artemis-dxvk-native [vertex.dxbc pixel.ade]\n", stderr);
        return 2;
    }
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return 1;
}
