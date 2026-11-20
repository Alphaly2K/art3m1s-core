#import <Metal/Metal.h>

void *metal_smoke_texture_new(const uint8_t *pixels, bool privateStorage) {
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    MTLTextureDescriptor *desc =
        [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm
                                                          width:4 height:4 mipmapped:NO];
    desc.storageMode = MTLStorageModeShared;
    desc.usage = MTLTextureUsageShaderRead | MTLTextureUsageRenderTarget;
    id<MTLTexture> texture = [device newTextureWithDescriptor:desc];
    [texture replaceRegion:MTLRegionMake2D(0, 0, 4, 4) mipmapLevel:0 withBytes:pixels bytesPerRow:16];
    if (privateStorage) {
        desc.storageMode = MTLStorageModePrivate;
        id<MTLTexture> target = [device newTextureWithDescriptor:desc];
        id<MTLCommandQueue> queue = [device newCommandQueue];
        id<MTLCommandBuffer> commands = [queue commandBuffer];
        id<MTLBlitCommandEncoder> copy = [commands blitCommandEncoder];
        if (!texture || !target || !copy) return NULL;
        [copy copyFromTexture:texture toTexture:target];
        [copy endEncoding];
        [commands commit];
        [commands waitUntilCompleted];
        if (commands.status != MTLCommandBufferStatusCompleted) return NULL;
        texture = target;
    }
    return (__bridge_retained void *)texture;
}

void metal_smoke_texture_read(void *texture, uint8_t *pixels) {
    [(__bridge id<MTLTexture>)texture getBytes:pixels bytesPerRow:16
                                  fromRegion:MTLRegionMake2D(0, 0, 4, 4) mipmapLevel:0];
}

void metal_smoke_texture_free(void *texture) {
    (void)CFBridgingRelease(texture);
}
