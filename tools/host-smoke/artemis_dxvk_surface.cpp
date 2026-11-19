#include <CoreVideo/CoreVideo.h>
#include <d3d11_3.h>
#include <vulkan/vulkan_core.h>
#include <vulkan/vulkan_metal.h>

#include "dxgi/dxgi_interfaces.h"
#include "dxvk/dxvk_adapter.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <thread>
#include <vector>

using dxvk::Com;

static void check(bool success, const char *operation) {
    if (!success) {
        std::fprintf(stderr, "%s failed\n", operation);
        std::exit(1);
    }
}

int main() {
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
    check(vkDevice && vkGetDeviceProcAddr(vkDevice, "vkExportMetalObjectsEXT"),
          "resolve Metal objects extension on the DXVK device");

    constexpr uint32_t width = 7, height = 5;
    CVPixelBufferRef buffer = nullptr;
    CFDictionaryRef properties = CFDictionaryCreate(nullptr, nullptr, nullptr, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *key = kCVPixelBufferIOSurfacePropertiesKey;
    const void *value = properties;
    CFDictionaryRef attributes = CFDictionaryCreate(nullptr, &key, &value, 1,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    check(CVPixelBufferCreate(nullptr, width, height, kCVPixelFormatType_32BGRA,
        attributes, &buffer) == kCVReturnSuccess, "create BGRA pixel buffer");
    CFRelease(attributes);
    CFRelease(properties);
    IOSurfaceRef surface = CVPixelBufferGetIOSurface(buffer);
    check(surface != nullptr, "get IOSurface");

    VkImportMetalIOSurfaceInfoEXT import = {VK_STRUCTURE_TYPE_IMPORT_METAL_IO_SURFACE_INFO_EXT};
    import.ioSurface = surface;
    VkImageCreateInfo imageInfo = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    imageInfo.pNext = &import;
    imageInfo.imageType = VK_IMAGE_TYPE_2D;
    imageInfo.format = VK_FORMAT_B8G8R8A8_UNORM;
    imageInfo.extent = {width, height, 1};
    imageInfo.mipLevels = 1;
    imageInfo.arrayLayers = 1;
    imageInfo.samples = VK_SAMPLE_COUNT_1_BIT;
    imageInfo.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
        VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    VkImage image = VK_NULL_HANDLE;
    check(vkCreateImage(vkDevice, &imageInfo, nullptr, &image) == VK_SUCCESS, "import IOSurface as VkImage");
    VkMemoryRequirements memory;
    vkGetImageMemoryRequirements(vkDevice, image, &memory);
    uint32_t type = 0;
    while (type < 32 && !(memory.memoryTypeBits & (UINT32_C(1) << type))) ++type;
    check(type < 32, "find IOSurface image memory type");
    VkMemoryAllocateInfo allocation = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    allocation.allocationSize = memory.size;
    allocation.memoryTypeIndex = type;
    VkDeviceMemory backing = VK_NULL_HANDLE;
    check(vkAllocateMemory(vkDevice, &allocation, nullptr, &backing) == VK_SUCCESS, "allocate image memory");
    check(vkBindImageMemory(vkDevice, image, backing, 0) == VK_SUCCESS, "bind image memory");

    {
        D3D11_TEXTURE2D_DESC1 desc = {};
        desc.Width = width;
        desc.Height = height;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.BindFlags = D3D11_BIND_RENDER_TARGET;
        Com<ID3D11Texture2D> texture;
        check(SUCCEEDED(interop->CreateTexture2DFromVkImage(&desc, image, &texture)), "wrap VkImage in D3D11");
        Com<IDXGIVkInteropSurface> interopSurface;
        check(SUCCEEDED(texture->QueryInterface(__uuidof(IDXGIVkInteropSurface),
                                               reinterpret_cast<void **>(&interopSurface))),
              "QueryInterface(IDXGIVkInteropSurface)");
        VkImage imported = VK_NULL_HANDLE;
        VkImageLayout layout = VK_IMAGE_LAYOUT_UNDEFINED;
        check(SUCCEEDED(interopSurface->GetVulkanImageInfo(&imported, &layout, nullptr)) && imported == image,
              "get imported D3D11 image and layout");
        Com<ID3D11RenderTargetView> target;
        check(SUCCEEDED(device->CreateRenderTargetView(texture.ptr(), nullptr, &target)), "create D3D11 target");
        const D3D11_QUERY_DESC queryDesc = {D3D11_QUERY_EVENT, 0};
        Com<ID3D11Query> completed;
        check(SUCCEEDED(device->CreateQuery(&queryDesc, &completed)), "create D3D11 completion query");
        const float colors[][4] = {{1, 0, 0, 1}, {0, 1, 1, 1}};
        const unsigned char expected[][4] = {{0, 0, 255, 255}, {255, 255, 0, 255}};
        const VkImageSubresourceRange subresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
        for (size_t i = 0; i < 2; ++i) {
            context->ClearRenderTargetView(target.ptr(), colors[i]);
            // Materialize the deferred clear and restore DXVK's layout before the next draw.
            interop->TransitionSurfaceLayout(interopSurface.ptr(), &subresource,
                                             layout, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
            interop->TransitionSurfaceLayout(interopSurface.ptr(), &subresource,
                                             VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, layout);
            context->End(completed.ptr());
            context->Flush();
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
            HRESULT ready;
            while ((ready = context->GetData(completed.ptr(), nullptr, 0, 0)) == S_FALSE &&
                   std::chrono::steady_clock::now() < deadline) {
                std::this_thread::sleep_for(std::chrono::milliseconds(1));
            }
            check(ready == S_OK, "wait for D3D11 rendering");
            check(CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
                  "lock pixel buffer");
            const auto *pixels = static_cast<const unsigned char *>(CVPixelBufferGetBaseAddress(buffer));
            const size_t stride = CVPixelBufferGetBytesPerRow(buffer);
            check(pixels != nullptr, "access pixel buffer");
            for (uint32_t y = 0; y < height; ++y) {
                for (uint32_t x = 0; x < width; ++x) {
                    const auto *actual = pixels + y * stride + x * 4;
                    if (std::memcmp(actual, expected[i], 4) != 0) {
                        std::fprintf(stderr, "Frame %zu pixel (%u, %u): BGRA %u,%u,%u,%u\n",
                            i + 1, x, y, actual[0], actual[1], actual[2], actual[3]);
                        check(false, "compare shared D3D11 output pixel");
                    }
                }
            }
            check(CVPixelBufferUnlockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
                  "unlock pixel buffer");
        }
    }
    vkDestroyImage(vkDevice, image, nullptr);
    vkFreeMemory(vkDevice, backing, nullptr);
    CVPixelBufferRelease(buffer);
    std::puts("Native DXVK IOSurface: two D3D11 frames visible in the original pixel buffer");
}
