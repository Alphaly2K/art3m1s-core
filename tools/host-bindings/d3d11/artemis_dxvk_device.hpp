#ifndef ARTEMIS_DXVK_DEVICE_HPP
#define ARTEMIS_DXVK_DEVICE_HPP

#include <IOSurface/IOSurfaceRef.h>
#include <d3d11_4.h>
#include <vulkan/vulkan_core.h>
#include "dxgi/dxgi_interfaces.h"
#include "util/com/com_pointer.h"

#include <memory>

namespace artemis {

class DxvkSurface;

// An owner-thread D3D11 device and immediate context for a native macOS Host.
class DxvkDevice {
public:
    DxvkDevice();
    DxvkDevice(const DxvkDevice &) = delete;
    DxvkDevice &operator=(const DxvkDevice &) = delete;

    ID3D11Device *device() const { return device_.ptr(); }
    ID3D11DeviceContext *context() const { return context_.ptr(); }
    void *metal_device() const;
    bool available() const { return !failed_; }

    // Both operations wait for completion. Do not call while a foreign queue is using the surface.
    void prepare_input(const DxvkSurface &surface);
    void finish_output(const DxvkSurface &surface);

private:
    friend class DxvkSurface;
    void wait();
    void finish_before_release() noexcept;

    dxvk::Com<ID3D11Device> device_;
    dxvk::Com<ID3D11DeviceContext> context_;
    dxvk::Com<IDXGIVkInteropDevice1> interop_;
    dxvk::Com<ID3D11DeviceContext4> signal_context_;
    dxvk::Com<ID3D11Fence> completed_;
    VkSemaphore completion_semaphore_ = VK_NULL_HANDLE;
    uint64_t completion_value_ = 0;
    VkDevice vulkan_ = VK_NULL_HANDLE;
    bool failed_ = false;
    bool needs_drain_ = false;
};

class DxvkSurface {
public:
    // Retains a Host-owned, single-plane BGRA IOSurface. Unbind D3D11 views and
    // finish pending GPU work before destroying it.
    static std::unique_ptr<DxvkSurface> import(DxvkDevice &device, IOSurfaceRef surface, UINT bind);
    ~DxvkSurface();
    DxvkSurface(const DxvkSurface &) = delete;
    DxvkSurface &operator=(const DxvkSurface &) = delete;

    ID3D11Texture2D *texture() const { return texture_.ptr(); }

private:
    friend class DxvkDevice;
    explicit DxvkSurface(DxvkDevice &device);

    DxvkDevice &owner_;

    dxvk::Com<IDXGIVkInteropDevice1> interop_;
    VkDevice device_;
    IOSurfaceRef iosurface_ = nullptr;
    VkImage image_ = VK_NULL_HANDLE;
    VkDeviceMemory backing_ = VK_NULL_HANDLE;
    dxvk::Com<ID3D11Texture2D> texture_;
    dxvk::Com<IDXGIVkInteropSurface> surface_;
    VkImageLayout layout_ = VK_IMAGE_LAYOUT_UNDEFINED;
};

}

#endif
