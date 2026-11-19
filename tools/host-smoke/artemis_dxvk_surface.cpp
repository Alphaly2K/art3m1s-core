#include <CoreVideo/CoreVideo.h>
#include <d3d11_3.h>
#include <vulkan/vulkan_core.h>
#include <vulkan/vulkan_metal.h>

#include "dxgi/dxgi_interfaces.h"
#include "dxvk/dxvk_adapter.h"
#include "artemis_dxvk_metal_writer.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <memory>
#include <thread>
#include <vector>

using dxvk::Com;

static void check(bool success, const char *operation) {
    if (!success) {
        std::fprintf(stderr, "%s failed\n", operation);
        std::exit(1);
    }
}

constexpr VkImageSubresourceRange colorRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};

struct SharedTexture {
    explicit SharedTexture(VkDevice device) : device(device) {}
    SharedTexture(const SharedTexture &) = delete;
    SharedTexture &operator=(const SharedTexture &) = delete;
    ~SharedTexture() {
        surface = nullptr;
        texture = nullptr;
        if (image) vkDestroyImage(device, image, nullptr);
        if (backing) vkFreeMemory(device, backing, nullptr);
        if (buffer) CVPixelBufferRelease(buffer);
    }

    void create(IDXGIVkInteropDevice1 *interop, uint32_t width, uint32_t height, UINT bind) {
        CFDictionaryRef properties = CFDictionaryCreate(nullptr, nullptr, nullptr, 0,
            &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        const void *keys[] = {kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey};
        const void *values[] = {properties, kCFBooleanTrue};
        CFDictionaryRef attributes = CFDictionaryCreate(nullptr, keys, values, 2,
            &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        check(CVPixelBufferCreate(nullptr, width, height, kCVPixelFormatType_32BGRA,
            attributes, &buffer) == kCVReturnSuccess, "create BGRA pixel buffer");
        CFRelease(attributes);
        CFRelease(properties);

        VkImportMetalIOSurfaceInfoEXT import = {VK_STRUCTURE_TYPE_IMPORT_METAL_IO_SURFACE_INFO_EXT};
        import.ioSurface = CVPixelBufferGetIOSurface(buffer);
        check(import.ioSurface != nullptr, "get IOSurface");
        VkImageCreateInfo imageInfo = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
        imageInfo.pNext = &import;
        imageInfo.imageType = VK_IMAGE_TYPE_2D;
        imageInfo.format = VK_FORMAT_B8G8R8A8_UNORM;
        imageInfo.extent = {width, height, 1};
        imageInfo.mipLevels = 1;
        imageInfo.arrayLayers = 1;
        imageInfo.samples = VK_SAMPLE_COUNT_1_BIT;
        imageInfo.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
            (bind == D3D11_BIND_RENDER_TARGET ? VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT : VK_IMAGE_USAGE_SAMPLED_BIT);
        check(vkCreateImage(device, &imageInfo, nullptr, &image) == VK_SUCCESS, "import IOSurface as VkImage");
        VkMemoryRequirements memory;
        vkGetImageMemoryRequirements(device, image, &memory);
        uint32_t type = 0;
        while (type < VK_MAX_MEMORY_TYPES && !(memory.memoryTypeBits & (UINT32_C(1) << type))) ++type;
        check(type < VK_MAX_MEMORY_TYPES, "find IOSurface image memory type");
        VkMemoryAllocateInfo allocation = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
        allocation.allocationSize = memory.size;
        allocation.memoryTypeIndex = type;
        check(vkAllocateMemory(device, &allocation, nullptr, &backing) == VK_SUCCESS, "allocate image memory");
        check(vkBindImageMemory(device, image, backing, 0) == VK_SUCCESS, "bind image memory");

        D3D11_TEXTURE2D_DESC1 desc = {};
        desc.Width = width;
        desc.Height = height;
        desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.BindFlags = bind;
        check(SUCCEEDED(interop->CreateTexture2DFromVkImage(&desc, image, &texture)), "wrap VkImage in D3D11");
        check(SUCCEEDED(texture->QueryInterface(__uuidof(IDXGIVkInteropSurface),
                                               reinterpret_cast<void **>(&surface))),
              "QueryInterface(IDXGIVkInteropSurface)");
        VkImage imported = VK_NULL_HANDLE;
        check(SUCCEEDED(surface->GetVulkanImageInfo(&imported, &layout, nullptr)) && imported == image,
              "get imported D3D11 image and layout");
    }

    VkDevice device;
    CVPixelBufferRef buffer = nullptr;
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory backing = VK_NULL_HANDLE;
    Com<ID3D11Texture2D> texture;
    Com<IDXGIVkInteropSurface> surface;
    VkImageLayout layout = VK_IMAGE_LAYOUT_UNDEFINED;
};

static void complete_draw(ID3D11DeviceContext *context, ID3D11Query *completed) {
    context->End(completed);
    context->Flush();
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
    HRESULT ready;
    while ((ready = context->GetData(completed, nullptr, 0, 0)) == S_FALSE &&
           std::chrono::steady_clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    check(ready == S_OK, "wait for D3D11 rendering");
}

static std::vector<char> load_bytecode(const char *path) {
    std::ifstream file(path, std::ios::binary | std::ios::ate);
    const auto bytes = file.tellg();
    check(bytes >= 4 && bytes <= 16 * 1024 * 1024, "get DXBC bytecode length");
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

struct EffectParams {
    float tint[4] = {0, 1, 0.5f, 1};
    float param = 1;
    float padding[3] = {};
};

static Com<ID3D11Buffer> bind_effect(ID3D11Device *device, ID3D11DeviceContext *context,
                                    const char *vsPath, const char *psPath, uint32_t width, uint32_t height,
                                    ID3D11Texture2D *foreground) {
    const auto vs = load_bytecode(vsPath);
    const auto ps = load_bytecode(psPath);
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
    ID3D11ShaderResourceView *textures[] = {user.ptr(), nullptr, fore.ptr(), mask.ptr()};
    context->PSSetShaderResources(0, 4, textures);

    D3D11_BUFFER_DESC constantDesc = {};
    constantDesc.ByteWidth = sizeof(EffectParams);
    constantDesc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    Com<ID3D11Buffer> constants;
    check(SUCCEEDED(device->CreateBuffer(&constantDesc, nullptr, &constants)), "create pixel constants");
    ID3D11Buffer *activeConstants = constants.ptr();
    context->PSSetConstantBuffers(0, 1, &activeConstants);
    return constants;
}

int main(int argc, char **argv) {
    if (argc != 1 && argc != 3) {
        std::fputs("Usage: artemis-dxvk-native [vertex.dxbc pixel.dxbc]\n", stderr);
        return 2;
    }
    const bool drawEffect = argc == 3;
    Com<IDXGIFactory1> factory;
    check(SUCCEEDED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void **>(&factory))),
          "CreateDXGIFactory1");
    Com<IDXGIAdapter1> adapter;
    check(SUCCEEDED(factory->EnumAdapters1(0, &adapter)), "EnumAdapters1");
    Com<IDXGIVkInteropAdapter> vulkanAdapter;
    check(SUCCEEDED(adapter->QueryInterface(__uuidof(IDXGIVkInteropAdapter),
                                           reinterpret_cast<void **>(&vulkanAdapter))),
          "QueryInterface(IDXGIVkInteropAdapter)");
    VkPhysicalDevice physical = VK_NULL_HANDLE;
    vulkanAdapter->GetVulkanHandles(nullptr, &physical);
    check(physical != VK_NULL_HANDLE, "GetVulkanHandles");

    uint32_t count = 0;
    check(vkEnumerateDeviceExtensionProperties(physical, nullptr, &count, nullptr) == VK_SUCCESS,
          "enumerate Vulkan device extension count");
    std::vector<VkExtensionProperties> extensions(count);
    check(vkEnumerateDeviceExtensionProperties(physical, nullptr, &count, extensions.data()) == VK_SUCCESS,
          "enumerate Vulkan device extensions");
    dxvk::DxvkExtensionList required;
    for (const auto &extension : extensions) {
        if (std::strcmp(extension.extensionName, VK_EXT_METAL_OBJECTS_EXTENSION_NAME) == 0) {
            required.push_back(extension);
            break;
        }
    }
    check(!required.empty(), "find VK_EXT_metal_objects");
    Com<IDXGIDXVKAdapter> nativeAdapter;
    check(SUCCEEDED(adapter->QueryInterface(__uuidof(IDXGIDXVKAdapter),
                                           reinterpret_cast<void **>(&nativeAdapter))),
          "QueryInterface(IDXGIDXVKAdapter)");
    nativeAdapter->GetDXVKAdapter()->enableExtensions(required);

    const D3D_FEATURE_LEVEL feature = D3D_FEATURE_LEVEL_11_0;
    Com<ID3D11Device> device;
    Com<ID3D11DeviceContext> context;
    check(SUCCEEDED(D3D11CreateDevice(adapter.ptr(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, &feature, 1, D3D11_SDK_VERSION, &device, nullptr, &context)),
        "D3D11CreateDevice (feature level 11.0)");
    Com<IDXGIVkInteropDevice1> interop;
    check(SUCCEEDED(device->QueryInterface(__uuidof(IDXGIVkInteropDevice1),
                                           reinterpret_cast<void **>(&interop))),
          "QueryInterface(IDXGIVkInteropDevice1)");
    VkDevice vkDevice = VK_NULL_HANDLE;
    interop->GetVulkanHandles(nullptr, nullptr, &vkDevice);
    check(vkDevice != VK_NULL_HANDLE, "get DXVK Vulkan device");
    const auto exportMetal = reinterpret_cast<PFN_vkExportMetalObjectsEXT>(
        vkGetDeviceProcAddr(vkDevice, "vkExportMetalObjectsEXT"));
    check(exportMetal != nullptr, "resolve Metal objects extension on the DXVK device");

    constexpr uint32_t width = 7, height = 5;
    SharedTexture output(vkDevice);
    output.create(interop.ptr(), width, height, D3D11_BIND_RENDER_TARGET);
    SharedTexture foreground(vkDevice);
    std::unique_ptr<ArtMetalWriter, decltype(&art_metal_writer_destroy)> writer(nullptr, art_metal_writer_destroy);
    Com<ID3D11RenderTargetView> target;
    check(SUCCEEDED(device->CreateRenderTargetView(output.texture.ptr(), nullptr, &target)), "create D3D11 target");
    const D3D11_QUERY_DESC queryDesc = {D3D11_QUERY_EVENT, 0};
    Com<ID3D11Query> completed;
    check(SUCCEEDED(device->CreateQuery(&queryDesc, &completed)), "create D3D11 completion query");
    Com<ID3D11Buffer> constants;
    if (drawEffect) {
        VkExportMetalDeviceInfoEXT metal = {VK_STRUCTURE_TYPE_EXPORT_METAL_DEVICE_INFO_EXT};
        VkExportMetalObjectsInfoEXT exportInfo = {VK_STRUCTURE_TYPE_EXPORT_METAL_OBJECTS_INFO_EXT, &metal};
        exportMetal(vkDevice, &exportInfo);
        check(metal.mtlDevice != nullptr, "export DXVK Metal device");
        foreground.create(interop.ptr(), width, height, D3D11_BIND_SHADER_RESOURCE);
        writer.reset(art_metal_writer_create(metal.mtlDevice, CVPixelBufferGetIOSurface(foreground.buffer), width, height));
        check(writer != nullptr, "create Metal foreground writer");
        // Initialize the imported image layout before Metal produces its first contents.
        interop->TransitionSurfaceLayout(foreground.surface.ptr(), &colorRange,
                                         VK_IMAGE_LAYOUT_UNDEFINED, foreground.layout);
        complete_draw(context.ptr(), completed.ptr());
        constants = bind_effect(device.ptr(), context.ptr(), argv[1], argv[2], width, height, foreground.texture.ptr());
    }

    const float clearColors[][4] = {{1, 0, 0, 1}, {0, 1, 1, 1}};
    const uint8_t clearPixels[][4] = {{0, 0, 255, 255}, {255, 255, 0, 255}};
    const uint8_t metalColors[][4] = {{17, 34, 51, 255}, {17, 68, 102, 255}};
    const uint8_t effectPixels[][4] = {{24, 24, 5, 255}, {37, 41, 5, 255}, {26, 34, 0, 255}};
    const size_t frameCount = drawEffect ? 3 : 2;
    for (size_t i = 0; i < frameCount; ++i) {
        if (drawEffect) {
            if (i < 2) check(art_metal_writer_clear(writer.get(), metalColors[i]), "render Metal foreground");
            EffectParams params;
            params.param = i < 2 ? 1.0f : 0.0f;
            context->UpdateSubresource(constants.ptr(), 0, nullptr, &params, 0, 0);
            ID3D11RenderTargetView *activeTarget = target.ptr();
            context->OMSetRenderTargets(1, &activeTarget, nullptr);
            context->Draw(4, 0);
            context->OMSetRenderTargets(0, nullptr, nullptr);
        } else {
            context->ClearRenderTargetView(target.ptr(), clearColors[i]);
        }
        // Materialize deferred writes and restore DXVK's layout before the next draw.
        interop->TransitionSurfaceLayout(output.surface.ptr(), &colorRange,
                                         output.layout, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
        interop->TransitionSurfaceLayout(output.surface.ptr(), &colorRange,
                                         VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, output.layout);
        complete_draw(context.ptr(), completed.ptr());
        check(CVPixelBufferLockBaseAddress(output.buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
              "lock pixel buffer");
        const auto *pixels = static_cast<const uint8_t *>(CVPixelBufferGetBaseAddress(output.buffer));
        const size_t stride = CVPixelBufferGetBytesPerRow(output.buffer);
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
        check(CVPixelBufferUnlockBaseAddress(output.buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
              "unlock pixel buffer");
    }
    context->ClearState();
    std::puts(drawEffect ? "Native DXVK IOSurface: Metal foreground and HLSL constant updates matched all three frames"
                         : "Native DXVK IOSurface: two D3D11 frames visible in the original pixel buffer");
}
