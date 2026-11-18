#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#define VK_NO_PROTOTYPES
#include <d3d11.h>
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

    const char *names[] = { "VK_EXT_metal_objects", "VK_KHR_external_memory_win32",
                            "VK_KHR_external_semaphore_win32" };
    fprintf(stderr, "D3D11 interop: Vulkan device and queue family %u available\n", family);
    for (unsigned index = 0; index < sizeof(names) / sizeof(names[0]); ++index) {
        int exposed = 0;
        for (uint32_t i = 0; i < count; ++i) {
            if (strcmp(names[index], extensions[i].extensionName) == 0) exposed = 1;
        }
        fprintf(stderr, "D3D11 interop: %s exposed=%s\n", names[index], exposed ? "yes" : "no");
    }
    const char *functions[] = { "vkExportMetalObjectsEXT", "vkGetMemoryWin32HandleKHR", "vkGetSemaphoreWin32HandleKHR" };
    for (unsigned i = 0; i < sizeof(functions) / sizeof(functions[0]); ++i) {
        fprintf(stderr, "D3D11 interop: %s entrypoint=%s\n", functions[i],
                device_proc(device, functions[i]) ? "yes" : "no");
    }
    result = 0;
done:
    free(extensions);
    if (loader) FreeLibrary(loader);
    interop->vtbl->release(interop);
    ID3D11Device_Release(d3d);
    return result;
}
