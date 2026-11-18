#include <CoreVideo/CoreVideo.h>
#include <vulkan/vulkan_core.h>
#include <vulkan/vulkan_metal.h>
#include <stdio.h>
#include <stdlib.h>

static void require(int matched, const char *message) {
    if (!matched) {
        fprintf(stderr, "%s\n", message);
        exit(1);
    }
}

#define CHECK(call) do {                                            \
    VkResult result = (call);                                       \
    if (result != VK_SUCCESS) {                                     \
        fprintf(stderr, "%s: %d\n", #call, result);                  \
        exit(1);                                                    \
    }                                                               \
} while (0)

static CVPixelBufferRef make_surface(size_t width, size_t height) {
    CFDictionaryRef surface_properties = CFDictionaryCreate(NULL, NULL, NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *key = kCVPixelBufferIOSurfacePropertiesKey;
    const void *value = surface_properties;
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, &key, &value, 1,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CVPixelBufferRef buffer = NULL;
    CVReturn status = CVPixelBufferCreate(NULL, width, height, kCVPixelFormatType_32BGRA,
                                         attributes, &buffer);
    CFRelease(attributes);
    CFRelease(surface_properties);
    require(status == kCVReturnSuccess && CVPixelBufferGetIOSurface(buffer), "Cannot create BGRA IOSurface");
    return buffer;
}

static void verify_pixels(CVPixelBufferRef buffer, const unsigned char bgra[4]) {
    require(CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
            "Cannot lock pixel buffer");
    const unsigned char *pixels = CVPixelBufferGetBaseAddress(buffer);
    require(pixels != NULL, "Pixel buffer has no base address");
    size_t stride = CVPixelBufferGetBytesPerRow(buffer);
    for (size_t y = 0; y < CVPixelBufferGetHeight(buffer); ++y) {
        for (size_t x = 0; x < CVPixelBufferGetWidth(buffer); ++x) {
            for (size_t c = 0; c < 4; ++c) {
                require(pixels[y * stride + x * 4 + c] == bgra[c], "Unexpected IOSurface pixel");
            }
        }
    }
    require(CVPixelBufferUnlockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess,
            "Cannot unlock pixel buffer");
}

int main(void) {
    const uint32_t width = 7, height = 5;
    VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .apiVersion = VK_API_VERSION_1_1 };
    VkInstanceCreateInfo instance_info = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app };
    VkInstance instance;
    CHECK(vkCreateInstance(&instance_info, NULL, &instance));
    uint32_t count = 1;
    VkPhysicalDevice physical;
    VkResult enumerated = vkEnumeratePhysicalDevices(instance, &count, &physical);
    require((enumerated == VK_SUCCESS || enumerated == VK_INCOMPLETE) && count, "No Vulkan device");
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &count, NULL);
    VkQueueFamilyProperties *families = calloc(count, sizeof(*families));
    require(families != NULL, "Cannot allocate queue family list");
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &count, families);
    uint32_t family = 0;
    while (family < count && !(families[family].queueFlags & VK_QUEUE_GRAPHICS_BIT)) ++family;
    free(families);
    require(family < count, "No graphics queue");
    const float priority = 1;
    VkDeviceQueueCreateInfo queue_info = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family, .queueCount = 1, .pQueuePriorities = &priority };
    const char *extensions[] = { VK_EXT_METAL_OBJECTS_EXTENSION_NAME, "VK_KHR_portability_subset" };
    VkDeviceCreateInfo device_info = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &queue_info,
        .enabledExtensionCount = 2, .ppEnabledExtensionNames = extensions };
    VkDevice device;
    CHECK(vkCreateDevice(physical, &device_info, NULL, &device));
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);

    CVPixelBufferRef buffer = make_surface(width, height);
    VkImportMetalIOSurfaceInfoEXT import = { .sType = VK_STRUCTURE_TYPE_IMPORT_METAL_IO_SURFACE_INFO_EXT,
        .ioSurface = CVPixelBufferGetIOSurface(buffer) };
    VkImageCreateInfo image_info = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .pNext = &import,
        .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_B8G8R8A8_UNORM, .extent = { width, height, 1 },
        .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT,
        .usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL };
    VkImage image;
    CHECK(vkCreateImage(device, &image_info, NULL, &image));
    VkMemoryRequirements memory;
    vkGetImageMemoryRequirements(device, image, &memory);
    uint32_t type = 0;
    while (type < 32 && !(memory.memoryTypeBits & (UINT32_C(1) << type))) ++type;
    require(type < 32, "No IOSurface image memory type");
    VkMemoryAllocateInfo allocation = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = memory.size, .memoryTypeIndex = type };
    VkDeviceMemory backing;
    CHECK(vkAllocateMemory(device, &allocation, NULL, &backing));
    CHECK(vkBindImageMemory(device, image, backing, 0));

    VkCommandPoolCreateInfo pool_info = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = family, .flags = VK_COMMAND_POOL_CREATE_TRANSIENT_BIT };
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(device, &pool_info, NULL, &pool));
    VkCommandBufferAllocateInfo commands = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .commandBufferCount = 1 };
    const unsigned char pixels[][4] = {{ 0, 0, 255, 255 }, { 255, 255, 0, 255 }};
    for (size_t i = 0; i < sizeof(pixels) / sizeof(pixels[0]); ++i) {
        VkCommandBuffer command;
        CHECK(vkAllocateCommandBuffers(device, &commands, &command));
        VkCommandBufferBeginInfo begin = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
        CHECK(vkBeginCommandBuffer(command, &begin));
        VkImageMemoryBarrier barrier = { .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
            .oldLayout = i ? VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED,
            .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            .srcAccessMask = i ? VK_ACCESS_TRANSFER_WRITE_BIT : 0,
            .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
            .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .image = image, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
        vkCmdPipelineBarrier(command, i ? VK_PIPELINE_STAGE_TRANSFER_BIT : VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
            VK_PIPELINE_STAGE_TRANSFER_BIT,
            0, 0, NULL, 0, NULL, 1, &barrier);
        VkClearColorValue color = { .float32 = { (float)pixels[i][2] / 255, (float)pixels[i][1] / 255,
                                                (float)pixels[i][0] / 255, (float)pixels[i][3] / 255 } };
        vkCmdClearColorImage(command, image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1,
                             &barrier.subresourceRange);
        CHECK(vkEndCommandBuffer(command));
        VkSubmitInfo submit = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
            .commandBufferCount = 1, .pCommandBuffers = &command };
        CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
        CHECK(vkQueueWaitIdle(queue));
        verify_pixels(buffer, pixels[i]);
    }
    vkDestroyCommandPool(device, pool, NULL);
    vkDestroyImage(device, image, NULL);
    vkFreeMemory(device, backing, NULL);
    CVPixelBufferRelease(buffer);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    puts("MoltenVK IOSurface: two BGRA frames visible in the original pixel buffer");
    return 0;
}
