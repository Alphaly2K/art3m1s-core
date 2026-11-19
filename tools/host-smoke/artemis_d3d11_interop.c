#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#define VK_NO_PROTOTYPES
#include <d3d11.h>
#include <dxgi1_2.h>
#include <vulkan/vulkan_core.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct InteropDevice InteropDevice;
typedef struct InteropVtbl {
    HRESULT (STDMETHODCALLTYPE *query)(InteropDevice *, REFIID, void **);
    ULONG (STDMETHODCALLTYPE *retain)(InteropDevice *);
    ULONG (STDMETHODCALLTYPE *release)(InteropDevice *);
    void (STDMETHODCALLTYPE *handles)(InteropDevice *, VkInstance *, VkPhysicalDevice *, VkDevice *);
    void (STDMETHODCALLTYPE *queue)(InteropDevice *, VkQueue *, uint32_t *);
} InteropVtbl;
struct InteropDevice { const InteropVtbl *vtbl; };

static const GUID iid_interop_device = {
    0xe2ef5fa5, 0xdc21, 0x4af7, { 0x90, 0xc4, 0xf6, 0x7e, 0xf6, 0xa0, 0x93, 0x23 }
};

static void check_shared_texture(ID3D11Device *device) {
    const D3D11_TEXTURE2D_DESC desc = {
        .Width = 4, .Height = 4, .MipLevels = 1, .ArraySize = 1,
        .Format = DXGI_FORMAT_B8G8R8A8_UNORM, .SampleDesc = { 1, 0 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        .MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
    };
    const char *operation = "CreateTexture2D";
    ID3D11Texture2D *texture = NULL;
    IDXGIResource1 *resource = NULL;
    HANDLE handle = NULL;
    HRESULT status = ID3D11Device_CreateTexture2D(device, &desc, NULL, &texture);
    if (SUCCEEDED(status) && !texture) status = E_POINTER;
    if (SUCCEEDED(status)) {
        operation = "QueryInterface(IDXGIResource1)";
        status = ID3D11Texture2D_QueryInterface(texture, &IID_IDXGIResource1, (void **)&resource);
        if (SUCCEEDED(status) && !resource) status = E_POINTER;
    }
    if (SUCCEEDED(status)) {
        operation = "CreateSharedHandle";
        status = IDXGIResource1_CreateSharedHandle(resource, NULL,
            DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, NULL, &handle);
        if (SUCCEEDED(status) && (!handle || handle == INVALID_HANDLE_VALUE)) status = E_HANDLE;
    }
    if (SUCCEEDED(status)) {
        fprintf(stderr, "D3D11 interop: shareable BGRA texture created an NT handle\n");
        CloseHandle(handle);
    } else {
        fprintf(stderr, "D3D11 interop: shareable BGRA texture %s failed (0x%08lx)\n",
                operation, (unsigned long)(DWORD)status);
    }
    if (resource) IDXGIResource1_Release(resource);
    if (texture) ID3D11Texture2D_Release(texture);
}

int main(void) {
    const D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    ID3D11Device *d3d = NULL;
    HRESULT status = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                        &level, 1, D3D11_SDK_VERSION, &d3d, NULL, NULL);
    if (FAILED(status)) {
        fprintf(stderr, "D3D11 interop: device creation failed (0x%08lx)\n", (unsigned long)(DWORD)status);
        return 1;
    }
    InteropDevice *interop = NULL;
    status = ID3D11Device_QueryInterface(d3d, &iid_interop_device, (void **)&interop);
    if (FAILED(status)) {
        fprintf(stderr, "D3D11 interop: IDXGIVkInteropDevice unavailable (0x%08lx)\n", (unsigned long)(DWORD)status);
        ID3D11Device_Release(d3d);
        return status == E_NOINTERFACE ? 0 : 1;
    }
    VkInstance instance = VK_NULL_HANDLE;
    VkPhysicalDevice adapter = VK_NULL_HANDLE;
    VkDevice device = VK_NULL_HANDLE;
    VkQueue queue = VK_NULL_HANDLE;
    uint32_t family = UINT32_MAX;
    interop->vtbl->handles(interop, &instance, &adapter, &device);
    interop->vtbl->queue(interop, &queue, &family);
    HMODULE loader = LoadLibraryW(L"vulkan-1.dll");
    VkExtensionProperties *extensions = NULL;
    int result = 1;
    if (!loader || !instance || !adapter || !device || !queue || family == UINT32_MAX) {
        fputs("D3D11 interop: incomplete Vulkan handles\n", stderr);
        goto done;
    }

    FARPROC symbol = GetProcAddress(loader, "vkGetInstanceProcAddr");
    PFN_vkGetInstanceProcAddr instance_proc;
    _Static_assert(sizeof(instance_proc) == sizeof(symbol), "Vulkan loader function pointer size");
    memcpy(&instance_proc, &symbol, sizeof(instance_proc));
    if (!instance_proc) goto done;
    PFN_vkEnumerateDeviceExtensionProperties enumerate = (PFN_vkEnumerateDeviceExtensionProperties)
        instance_proc(instance, "vkEnumerateDeviceExtensionProperties");
    PFN_vkGetDeviceProcAddr device_proc = (PFN_vkGetDeviceProcAddr)instance_proc(instance, "vkGetDeviceProcAddr");
    uint32_t count = 0;
    if (!enumerate || !device_proc || enumerate(adapter, NULL, &count, NULL) != VK_SUCCESS || count > 4096) goto done;
    extensions = calloc(count ? count : 1, sizeof(*extensions));
    if (!extensions || enumerate(adapter, NULL, &count, extensions) != VK_SUCCESS) goto done;

    const struct { const char *extension, *entrypoint; } checks[] = {
        { "VK_EXT_metal_objects", "vkExportMetalObjectsEXT" },
        { "VK_KHR_external_memory_win32", "vkGetMemoryWin32HandleKHR" },
        { "VK_KHR_external_semaphore_win32", "vkGetSemaphoreWin32HandleKHR" },
    };
    int win32_memory = 0;
    fprintf(stderr, "D3D11 interop: Vulkan device and queue family %u available\n", family);
    for (unsigned index = 0; index < sizeof(checks) / sizeof(checks[0]); ++index) {
        const char *name = checks[index].extension;
        const char *entrypoint = checks[index].entrypoint;
        int exposed = 0;
        for (uint32_t i = 0; i < count; ++i) {
            if (strcmp(name, extensions[i].extensionName) == 0) exposed = 1;
        }
        int resolved = device_proc(device, entrypoint) != NULL;
        fprintf(stderr, "D3D11 interop: %s exposed=%s\n", name, exposed ? "yes" : "no");
        fprintf(stderr, "D3D11 interop: %s entrypoint=%s\n", entrypoint, resolved ? "yes" : "no");
        if (strcmp(name, "VK_KHR_external_memory_win32") == 0) win32_memory = exposed && resolved;
    }
    if (win32_memory) {
        check_shared_texture(d3d);
    } else {
        fputs("D3D11 interop: shareable BGRA texture skipped (Win32 external memory unavailable)\n", stderr);
    }
    result = 0;
done:
    free(extensions);
    if (loader) FreeLibrary(loader);
    interop->vtbl->release(interop);
    ID3D11Device_Release(d3d);
    return result;
}
