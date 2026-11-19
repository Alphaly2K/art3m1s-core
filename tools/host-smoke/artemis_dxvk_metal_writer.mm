#include "artemis_dxvk_metal_writer.h"
#import <Metal/Metal.h>
#include <new>

struct ArtMetalWriter {
    id<MTLTexture> texture;
    id<MTLCommandQueue> queue;
};

ArtMetalWriter *art_metal_writer_create(void *device, IOSurfaceRef surface, uint32_t width, uint32_t height) {
    @autoreleasepool {
        id<MTLDevice> metal = (__bridge id<MTLDevice>)device;
        MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
            width:width height:height mipmapped:NO];
        descriptor.storageMode = MTLStorageModeShared;
        descriptor.usage = MTLTextureUsageRenderTarget;
        id<MTLTexture> texture = [metal newTextureWithDescriptor:descriptor iosurface:surface plane:0];
        id<MTLCommandQueue> queue = [metal newCommandQueue];
        if (!texture || !queue) return nullptr;
        return new (std::nothrow) ArtMetalWriter{texture, queue};
    }
}

bool art_metal_writer_clear(ArtMetalWriter *writer, const uint8_t rgba[4]) {
    @autoreleasepool {
        id<MTLCommandBuffer> command = [writer->queue commandBuffer];
        MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = writer->texture;
        pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore;
        pass.colorAttachments[0].clearColor = MTLClearColorMake(rgba[0] / 255.0, rgba[1] / 255.0,
                                                               rgba[2] / 255.0, rgba[3] / 255.0);
        id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
        if (!encoder) return false;
        [encoder endEncoding];
        [command commit];
        [command waitUntilCompleted];
        return command.status == MTLCommandBufferStatusCompleted;
    }
}

bool art_metal_writer_copy(ArtMetalWriter *writer, IOSurfaceRef source) {
    @autoreleasepool {
        const NSUInteger width = writer->texture.width, height = writer->texture.height;
        if (IOSurfaceGetWidth(source) != width || IOSurfaceGetHeight(source) != height) return false;
        MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
            width:width height:height mipmapped:NO];
        descriptor.storageMode = MTLStorageModeShared;
        id<MTLTexture> texture = [writer->texture.device newTextureWithDescriptor:descriptor iosurface:source plane:0];
        if (!texture) return false;
        id<MTLCommandBuffer> command = [writer->queue commandBuffer];
        id<MTLBlitCommandEncoder> encoder = [command blitCommandEncoder];
        if (!encoder) return false;
        [encoder copyFromTexture:texture sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0)
                      sourceSize:MTLSizeMake(width, height, 1) toTexture:writer->texture destinationSlice:0
                 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
        [encoder endEncoding];
        [command commit];
        [command waitUntilCompleted];
        return command.status == MTLCommandBufferStatusCompleted;
    }
}

void art_metal_writer_destroy(ArtMetalWriter *writer) {
    delete writer;
}
