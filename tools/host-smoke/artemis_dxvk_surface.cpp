#include <CoreVideo/CoreVideo.h>
#include "artemis_dxvk_metal_writer.h"
#include "../host-bindings/d3d11/artemis_dxbc_effect.h"
#include "../host-bindings/d3d11/artemis_dxvk_device.hpp"
#include "../host-bindings/d3d11/artemis_dxvk_pass.h"

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

using Program = std::unique_ptr<ArtDxvkProgram, decltype(&art_dxvk_program_destroy)>;
using Targets = std::unique_ptr<ArtDxvkTargets, decltype(&art_dxvk_targets_destroy)>;
using Writer = std::unique_ptr<ArtMetalWriter, decltype(&art_metal_writer_destroy)>;

static std::vector<uint8_t> read_artifact(const char *path) {
    std::ifstream file(path, std::ios::binary | std::ios::ate);
    const auto bytes = file.tellg();
    check(bytes > 0, "open shader artifact");
    std::vector<uint8_t> ade(static_cast<size_t>(bytes));
    file.seekg(0);
    check(bool(file.read(reinterpret_cast<char *>(ade.data()), bytes)), "read shader artifact");
    return ade;
}

static Program load_program(ArtDxvkContext *context, const std::vector<uint8_t> &artifact) {
    Program program(art_dxvk_program_create(context, artifact.data(), artifact.size()), art_dxvk_program_destroy);
    check(bool(program), "prepare DXVK program");
    return program;
}

struct Presentation {
    PixelBuffer foreground, output, presentation;
    Targets targets;
    Writer writer, presenter;

    Presentation(ArtDxvkContext *context, uint32_t width, uint32_t height)
        : foreground(make_buffer(width, height)), output(make_buffer(width, height)),
          presentation(make_buffer(width, height)),
          targets(art_dxvk_targets_create(context, CVPixelBufferGetIOSurface(foreground.get()), output_surface()),
                  art_dxvk_targets_destroy),
          writer(art_metal_writer_create(art_dxvk_context_metal_device(context), CVPixelBufferGetIOSurface(foreground.get()),
                                         width, height), art_metal_writer_destroy),
          presenter(art_metal_writer_create(art_dxvk_context_metal_device(context), CVPixelBufferGetIOSurface(presentation.get()),
                                            width, height), art_metal_writer_destroy) {
        check(targets && writer && presenter, "create DXVK and Metal surfaces");
    }

    IOSurfaceRef output_surface() const { return CVPixelBufferGetIOSurface(output.get()); }

    void present() const {
        check(art_metal_writer_copy(presenter.get(), output_surface()), "consume DXVK output in Metal");
    }
};

static void draw_effect(const char *effect, const char *alternate, const char *legacy,
                        const char *dual_constants, const char *second_slot) {
    std::unique_ptr<ArtDxvkContext, decltype(&art_dxvk_context_destroy)> context(art_dxvk_context_create(),
                                                                                art_dxvk_context_destroy);
    check(bool(context), "create DXVK context");
    auto primary = load_program(context.get(), read_artifact(effect));
    auto second = load_program(context.get(), read_artifact(alternate));
    Presentation original(context.get(), 7, 5);
    const uint8_t initial[] = {17, 34, 51, 255}, changed[] = {17, 68, 102, 255};
    const uint8_t pixels[][4] = {{24, 24, 5, 255}, {255, 0, 0, 255}, {26, 34, 0, 255}};
    const uint8_t mask[] = {128, 0, 0, 255}, user[] = {5, 7, 11, 255};
    const float tint[] = {0, 1, 0.5f, 1};
    const auto draw_primary = [&](const Presentation &surface, const uint8_t (&color)[4], float param) {
        check(art_metal_writer_clear(surface.writer.get(), color), "render Metal foreground");
        const ArtDxvkUniform uniforms[] = {
            {reinterpret_cast<const uint8_t *>("tint"), 4, tint, 4},
            {reinterpret_cast<const uint8_t *>("param"), 5, &param, 1},
        };
        check(art_dxvk_draw(context.get(), primary.get(), surface.targets.get(), {mask, 1, 1}, {user, 1, 1},
                            uniforms, 2) == 0, "render reflected HLSL effect");
        surface.present();
    };
    draw_primary(original, initial, 1);
    check_pixels(original.presentation.get(), 1, pixels[0]);
    check(art_dxvk_draw(context.get(), second.get(), original.targets.get(), {}, {}, nullptr, 0) == 0,
          "switch shader on the same DXVK target");
    original.present();
    check_pixels(original.presentation.get(), 2, pixels[1]);
    second.reset();

    original.targets.reset();
    Presentation replacement(context.get(), 5, 3);
    draw_primary(replacement, changed, 0);
    check_pixels(replacement.presentation.get(), 3, pixels[2]);
    check_pixels(original.presentation.get(), 2, pixels[1]);

    auto combined = load_program(context.get(), read_artifact(legacy));
    const uint8_t legacy_mask[] = {0, 0, 0, 255, 0, 128, 0, 255}, legacy_user[] = {5, 128, 11, 255};
    const uint8_t default_pixel[] = {11, 80, 17, 255}, custom_pixel[] = {11, 96, 17, 128};
    const float weights[] = {1, 0.25f}, alpha = 0.5f;
    const ArtDxvkUniform overrides[] = {
        {reinterpret_cast<const uint8_t *>("weights"), 7, weights, 2},
        {reinterpret_cast<const uint8_t *>("alpha"), 5, &alpha, 1},
        {reinterpret_cast<const uint8_t *>("unused"), 6, &alpha, 1},
    };
    for (size_t i = 0; i < 3; ++i) {
        check(art_dxvk_draw(context.get(), combined.get(), replacement.targets.get(), {legacy_mask, 2, 1},
                            {legacy_user, 1, 1}, i == 1 ? overrides : nullptr, i == 1 ? 3 : 0) == 0,
              "render HLSL with combined samplers and reflected defaults");
        replacement.present();
        check_pixels(replacement.presentation.get(), 4 + i, i == 1 ? custom_pixel : default_pixel);
    }
    auto artifact = read_artifact(dual_constants);
    auto dual = load_program(context.get(), artifact);
    auto only_b1 = load_program(context.get(), read_artifact(second_slot));
    const float off = 0;
    const ArtDxvkUniform dual_overrides[] = {
        {reinterpret_cast<const uint8_t *>("red"), 3, &off, 1},
        {reinterpret_cast<const uint8_t *>("green"), 5, &off, 1},
    };
    const uint8_t yellow[] = {0, 255, 255, 255}, red[] = {0, 0, 255, 255};
    const uint8_t green[] = {0, 255, 0, 255}, black[] = {0, 0, 0, 255};
    const auto draw_constants = [&](ArtDxvkProgram *program, const ArtDxvkUniform *overrides,
                                    size_t count, size_t frame, const uint8_t (&expected)[4]) {
        check(art_dxvk_draw(context.get(), program, replacement.targets.get(), {}, {}, overrides, count) == 0,
              "render HLSL with multiple constant buffer slots");
        replacement.present();
        check_pixels(replacement.presentation.get(), frame, expected);
    };
    draw_constants(dual.get(), nullptr, 0, 7, yellow);
    draw_constants(dual.get(), dual_overrides, 2, 8, black);
    draw_constants(only_b1.get(), nullptr, 0, 9, green);
    draw_constants(only_b1.get(), &dual_overrides[1], 1, 10, black);
    draw_constants(dual.get(), &dual_overrides[1], 1, 11, red);
    draw_constants(dual.get(), nullptr, 0, 12, yellow);

    auto aliased_artifact = artifact;
    const auto second_uniform = sizeof(ArtDxbcHeader) + sizeof(ArtDxbcUniform);
    const auto second_name = second_uniform + offsetof(ArtDxbcUniform, name);
    check(aliased_artifact.size() >= second_name + ART_DXBC_NAME_BYTES &&
          std::memcmp(aliased_artifact.data() + second_name, "green", sizeof("green")) == 0,
          "find the b1 uniform record");
    std::memset(aliased_artifact.data() + second_name, 0, ART_DXBC_NAME_BYTES);
    std::memcpy(aliased_artifact.data() + second_name, "red", sizeof("red"));
    auto aliased = load_program(context.get(), aliased_artifact);
    draw_constants(aliased.get(), nullptr, 0, 13, yellow);
    draw_constants(aliased.get(), &dual_overrides[0], 1, 14, black);
    draw_constants(aliased.get(), nullptr, 0, 15, yellow);

    artifact[artifact.size() - 4] |= 1;
    Program invalid(art_dxvk_program_create(context.get(), artifact.data(), artifact.size()), art_dxvk_program_destroy);
    check(!invalid, "reject an unaligned constant buffer boundary");

    draw_primary(replacement, changed, 0);
    check_pixels(replacement.presentation.get(), 16, pixels[2]);
    std::puts("Native DXVK verified modern and legacy programs, shared targets, and constant overrides in b0 and b1");
}

int main(int argc, char **argv) try {
    if (argc == 1) clear_surfaces();
    else if (argc == 6) draw_effect(argv[1], argv[2], argv[3], argv[4], argv[5]);
    else {
        std::fputs("Usage: artemis-dxvk-native [effect.ade alternate.ade legacy.ade dual.ade b1.ade]\n", stderr);
        return 2;
    }
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return 1;
}
