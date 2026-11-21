#include <CoreFoundation/CoreFoundation.h>
#include <CoreVideo/CVPixelBuffer.h>
#include "artemis_dxvk_device.hpp"
#include <vulkan/vulkan_metal.h>
#include "d3d11/d3d11_fence.h"
#include "dxvk/dxvk_adapter.h"

#include <chrono>
#include <cstring>
#include <stdexcept>
#include <thread>
#include <vector>

namespace artemis {

using dxvk::Com;

static void require(bool success, const char *message) {
    if (!success) throw std::runtime_error(message);
}

constexpr VkImageSubresourceRange colorRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};

DxvkDevice::DxvkDevice() {
    Com<IDXGIFactory1> factory;
    require(SUCCEEDED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void **>(&factory))),
            "CreateDXGIFactory1 failed");
    Com<IDXGIAdapter1> adapter;
    require(SUCCEEDED(factory->EnumAdapters1(0, &adapter)), "EnumAdapters1 failed");
    Com<IDXGIVkInteropAdapter> vkAdapter;
    require(SUCCEEDED(adapter->QueryInterface(__uuidof(IDXGIVkInteropAdapter), reinterpret_cast<void **>(&vkAdapter))),
            "DXVK Vulkan adapter interface unavailable");
    VkPhysicalDevice physical = VK_NULL_HANDLE;
    vkAdapter->GetVulkanHandles(nullptr, &physical);
    require(physical != VK_NULL_HANDLE, "DXVK Vulkan adapter has no physical device");
    uint32_t count = 0;
    require(vkEnumerateDeviceExtensionProperties(physical, nullptr, &count, nullptr) == VK_SUCCESS,
            "Cannot enumerate Vulkan device extensions");
    std::vector<VkExtensionProperties> extensions(count);
    require(vkEnumerateDeviceExtensionProperties(physical, nullptr, &count, extensions.data()) == VK_SUCCESS,
            "Cannot read Vulkan device extensions");
    dxvk::DxvkExtensionList required;
    for (const auto &extension : extensions) {
        if (std::strcmp(extension.extensionName, VK_EXT_METAL_OBJECTS_EXTENSION_NAME) == 0) {
            required.push_back(extension);
            break;
        }
    }
    require(!required.empty(), "VK_EXT_metal_objects unavailable");
    Com<IDXGIDXVKAdapter> nativeAdapter;
    require(SUCCEEDED(adapter->QueryInterface(__uuidof(IDXGIDXVKAdapter), reinterpret_cast<void **>(&nativeAdapter))),
            "Native DXVK adapter interface unavailable");
    nativeAdapter->GetDXVKAdapter()->enableExtensions(required);

    const D3D_FEATURE_LEVEL feature = D3D_FEATURE_LEVEL_11_0;
    require(SUCCEEDED(D3D11CreateDevice(adapter.ptr(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, &feature, 1, D3D11_SDK_VERSION, &device_, nullptr, &context_)),
        "D3D11CreateDevice (feature level 11.0) failed");
    require(SUCCEEDED(device_->QueryInterface(__uuidof(IDXGIVkInteropDevice1), reinterpret_cast<void **>(&interop_))),
            "DXVK Vulkan device interface unavailable");
    interop_->GetVulkanHandles(nullptr, nullptr, &vulkan_);
    require(vulkan_ != VK_NULL_HANDLE, "DXVK Vulkan device unavailable");
    Com<ID3D11Device5> fence_device;
    require(SUCCEEDED(device_->QueryInterface(__uuidof(ID3D11Device5), reinterpret_cast<void **>(&fence_device))),
            "DXVK device has no D3D11 fence support");
    require(SUCCEEDED(context_->QueryInterface(__uuidof(ID3D11DeviceContext4), reinterpret_cast<void **>(&signal_context_))),
            "DXVK context has no D3D11 fence support");
    // DXVK uses a zero flag mask for local fences.
    const auto flags = static_cast<D3D11_FENCE_FLAG>(0);
    require(SUCCEEDED(fence_device->CreateFence(0, flags, __uuidof(ID3D11Fence),
            reinterpret_cast<void **>(&completed_))), "Cannot create D3D11 completion fence");
    // The bundled DXVK fence owns this semaphore until completed_ is released.
    completion_semaphore_ = static_cast<dxvk::D3D11Fence *>(completed_.ptr())->GetFence()->handle();
    require(completion_semaphore_ != VK_NULL_HANDLE, "DXVK fence has no Vulkan timeline semaphore");
}

void *DxvkDevice::metal_device() const {
    const auto exportMetal = reinterpret_cast<PFN_vkExportMetalObjectsEXT>(
        vkGetDeviceProcAddr(vulkan_, "vkExportMetalObjectsEXT"));
    require(exportMetal != nullptr, "DXVK Metal export unavailable");
    VkExportMetalDeviceInfoEXT metal = {VK_STRUCTURE_TYPE_EXPORT_METAL_DEVICE_INFO_EXT};
    VkExportMetalObjectsInfoEXT info = {VK_STRUCTURE_TYPE_EXPORT_METAL_OBJECTS_INFO_EXT, &metal};
    exportMetal(vulkan_, &info);
    require(metal.mtlDevice != nullptr, "DXVK Metal device unavailable");
    return metal.mtlDevice;
}

void DxvkDevice::wait() {
    failed_ = true;
    needs_drain_ = true;
    const uint64_t value = ++completion_value_;
    require(SUCCEEDED(signal_context_->Signal(completed_.ptr(), value)), "Cannot signal D3D11 completion fence");
    VkSemaphoreWaitInfo wait = {VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO};
    wait.semaphoreCount = 1;
    wait.pSemaphores = &completion_semaphore_;
    wait.pValues = &value;
    constexpr uint64_t timeout_ns = UINT64_C(3000000000);
    const VkResult result = vkWaitSemaphores(vulkan_, &wait, timeout_ns);
    needs_drain_ = result != VK_SUCCESS && result != VK_ERROR_DEVICE_LOST;
    require(result == VK_SUCCESS, "D3D11 rendering did not complete");
    failed_ = false;
}

void DxvkDevice::finish_before_release() noexcept {
    if (!needs_drain_) return;

    // Imported images are owned by the bridge and may still be referenced by DXVK.
    interop_->FlushRenderingCommands();
    while (needs_drain_) {
        interop_->LockSubmissionQueue();
        const VkResult result = vkDeviceWaitIdle(vulkan_);
        interop_->ReleaseSubmissionQueue();
        needs_drain_ = result != VK_SUCCESS && result != VK_ERROR_DEVICE_LOST;
        if (needs_drain_) std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
}

void DxvkDevice::prepare_input(const DxvkSurface &surface) {
    require(available() && &surface.owner_ == this, "DXVK surface device unavailable or mismatched");
    interop_->TransitionSurfaceLayout(surface.surface_.ptr(), &colorRange, VK_IMAGE_LAYOUT_UNDEFINED, surface.layout_);
    wait();
}

void DxvkDevice::finish_output(const DxvkSurface &surface) {
    require(available() && &surface.owner_ == this, "DXVK surface device unavailable or mismatched");
    // Materialize deferred writes before handing the image to Metal.
    interop_->TransitionSurfaceLayout(surface.surface_.ptr(), &colorRange, surface.layout_, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
    interop_->TransitionSurfaceLayout(surface.surface_.ptr(), &colorRange, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, surface.layout_);
    wait();
}

DxvkSurface::DxvkSurface(DxvkDevice &device)
    : owner_(device), interop_(device.interop_), device_(device.vulkan_) {}

DxvkSurface::~DxvkSurface() {
    owner_.finish_before_release();
    surface_ = nullptr;
    texture_ = nullptr;
    if (image_) vkDestroyImage(device_, image_, nullptr);
    if (backing_) vkFreeMemory(device_, backing_, nullptr);
    if (iosurface_) CFRelease(iosurface_);
}

std::unique_ptr<DxvkSurface> DxvkSurface::import(DxvkDevice &device, IOSurfaceRef surface, UINT bind) {
    require(device.available(), "DXVK device unavailable");
    constexpr UINT supported = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    require(surface && bind && !(bind & ~supported), "Invalid DXVK IOSurface import arguments");
    const size_t width = IOSurfaceGetWidth(surface), height = IOSurfaceGetHeight(surface);
    require(width && height && width <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION && height <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION &&
            IOSurfaceGetPlaneCount(surface) == 0 && IOSurfaceGetPixelFormat(surface) == kCVPixelFormatType_32BGRA,
            "DXVK IOSurface must be a non-planar BGRA8 image");
    auto result = std::unique_ptr<DxvkSurface>(new DxvkSurface(device));
    CFRetain(surface);
    result->iosurface_ = surface;
    const auto vk = result->device_;
    VkImportMetalIOSurfaceInfoEXT import = {VK_STRUCTURE_TYPE_IMPORT_METAL_IO_SURFACE_INFO_EXT};
    import.ioSurface = surface;
    VkImageCreateInfo imageInfo = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    imageInfo.pNext = &import;
    imageInfo.imageType = VK_IMAGE_TYPE_2D;
    imageInfo.format = VK_FORMAT_B8G8R8A8_UNORM;
    imageInfo.extent = {static_cast<uint32_t>(width), static_cast<uint32_t>(height), 1};
    imageInfo.mipLevels = 1;
    imageInfo.arrayLayers = 1;
    imageInfo.samples = VK_SAMPLE_COUNT_1_BIT;
    imageInfo.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    if (bind & D3D11_BIND_RENDER_TARGET) imageInfo.usage |= VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    if (bind & D3D11_BIND_SHADER_RESOURCE) imageInfo.usage |= VK_IMAGE_USAGE_SAMPLED_BIT;
    require(vkCreateImage(vk, &imageInfo, nullptr, &result->image_) == VK_SUCCESS, "Cannot import IOSurface in Vulkan");
    VkMemoryRequirements memory;
    vkGetImageMemoryRequirements(vk, result->image_, &memory);
    uint32_t type = 0;
    while (type < VK_MAX_MEMORY_TYPES && !(memory.memoryTypeBits & (UINT32_C(1) << type))) ++type;
    require(type < VK_MAX_MEMORY_TYPES, "No Vulkan memory type for IOSurface");
    VkMemoryAllocateInfo allocation = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    allocation.allocationSize = memory.size;
    allocation.memoryTypeIndex = type;
    require(vkAllocateMemory(vk, &allocation, nullptr, &result->backing_) == VK_SUCCESS, "Cannot allocate IOSurface image memory");
    require(vkBindImageMemory(vk, result->image_, result->backing_, 0) == VK_SUCCESS, "Cannot bind IOSurface image memory");

    D3D11_TEXTURE2D_DESC1 desc = {};
    desc.Width = static_cast<UINT>(width);
    desc.Height = static_cast<UINT>(height);
    desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.BindFlags = bind;
    require(SUCCEEDED(result->interop_->CreateTexture2DFromVkImage(&desc, result->image_, &result->texture_)),
            "Cannot wrap Vulkan image in D3D11");
    require(SUCCEEDED(result->texture_->QueryInterface(__uuidof(IDXGIVkInteropSurface), reinterpret_cast<void **>(&result->surface_))),
            "DXVK surface interface unavailable");
    VkImage imported = VK_NULL_HANDLE;
    require(SUCCEEDED(result->surface_->GetVulkanImageInfo(&imported, &result->layout_, nullptr)) && imported == result->image_,
            "Cannot query imported image layout");
    return result;
}

}
