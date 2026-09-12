#include "capture_backend.h"

#include <algorithm>
#include <cmath>
#include <cstring>

namespace art3m1s::krkr
{
namespace
{
uint8_t Lerp8(uint8_t dst, uint8_t src, uint8_t alpha)
{
    return static_cast<uint8_t>(dst + ((static_cast<int>(src) - dst) * alpha >> 8));
}
} // namespace

CaptureBackend::~CaptureBackend()
{
}

CaptureBackend::WindowTexture* CaptureBackend::FindWindowTexture(void* handle) const
{
    auto* texture = static_cast<WindowTexture*>(handle);
    return window_textures_.find(texture) == window_textures_.end() ? nullptr : texture;
}

void CaptureBackend::BeginFrame(int winWidth, int winHeight)
{
    width_ = static_cast<uint32_t>(std::max(0, winWidth));
    height_ = static_cast<uint32_t>(std::max(0, winHeight));
    canvas_.assign(static_cast<size_t>(width_) * height_ * 4, 0);
}

void CaptureBackend::EndFrame()
{
    published_.swap(canvas_);
    published_width_ = width_;
    published_height_ = height_;
    ++frame_id_;
    generation_ = frame_id_;
}

void* CaptureBackend::CreateWindowTexture(int width, int height)
{
    if (width <= 0 || height <= 0)
        return nullptr;
    auto* texture = new WindowTexture;
    texture->width = static_cast<uint32_t>(width);
    texture->height = static_cast<uint32_t>(height);
    texture->pixels.resize(static_cast<size_t>(width) * height * 4);
    window_textures_.insert(texture);
    return texture;
}

void CaptureBackend::UpdateWindowTexture(
    void* handle, const uint8_t* buff, int width, int height, int pitch)
{
    auto* texture = FindWindowTexture(handle);
    if (!texture || !buff || width <= 0 || height <= 0 || pitch < width * 4)
        return;

    texture->width = static_cast<uint32_t>(width);
    texture->height = static_cast<uint32_t>(height);
    texture->pixels.resize(static_cast<size_t>(width) * height * 4);
    for (int y = 0; y < height; ++y)
    {
        std::memcpy(texture->pixels.data() + static_cast<size_t>(y) * width * 4,
                    buff + static_cast<size_t>(y) * pitch,
                    static_cast<size_t>(width) * 4);
    }
}

void CaptureBackend::DestroyWindowTexture(void* handle)
{
    auto* texture = FindWindowTexture(handle);
    if (!texture)
        return;
    window_textures_.erase(texture);
    delete texture;
}

void CaptureBackend::DrawWindowTexture(
    void* handle, float posX, float posY, float width, float height)
{
    auto* texture = FindWindowTexture(handle);
    if (!texture || texture->pixels.empty() || canvas_.empty() || width <= 0 || height <= 0)
        return;

    const int dstLeft = std::max(0, static_cast<int>(std::lround(posX)));
    const int dstTop = std::max(0, static_cast<int>(std::lround(posY)));
    const int dstRight =
        std::min(static_cast<int>(width_), static_cast<int>(std::lround(posX + width)));
    const int dstBottom =
        std::min(static_cast<int>(height_), static_cast<int>(std::lround(posY + height)));
    if (dstLeft >= dstRight || dstTop >= dstBottom)
        return;

    const float scaleX = static_cast<float>(texture->width) / std::max(1.0f, width);
    const float scaleY = static_cast<float>(texture->height) / std::max(1.0f, height);

    for (int y = dstTop; y < dstBottom; ++y)
    {
        const uint32_t srcY =
            std::min(texture->height - 1, static_cast<uint32_t>((y - posY) * scaleY));
        const uint8_t* srcRow =
            texture->pixels.data() + static_cast<size_t>(srcY) * texture->width * 4;
        uint8_t* dstRow = canvas_.data() + static_cast<size_t>(y) * width_ * 4;

        for (int x = dstLeft; x < dstRight; ++x)
        {
            const uint32_t srcX =
                std::min(texture->width - 1, static_cast<uint32_t>((x - posX) * scaleX));
            const uint8_t* src = srcRow + static_cast<size_t>(srcX) * 4;
            uint8_t* dst = dstRow + static_cast<size_t>(x) * 4;

            const uint8_t alpha = src[3];
            if (alpha == 0)
                continue;
            if (alpha == 255)
            {
                std::memcpy(dst, src, 4);
                continue;
            }

            dst[0] = Lerp8(dst[0], src[0], alpha);
            dst[1] = Lerp8(dst[1], src[1], alpha);
            dst[2] = Lerp8(dst[2], src[2], alpha);
            dst[3] = Lerp8(dst[3], 255, alpha);
        }
    }
}

void* CaptureBackend::CreateTarget(int width, int height)
{
    return software_.CreateTarget(width, height);
}

void CaptureBackend::DestroyTarget(void* target)
{
    software_.DestroyTarget(target);
}

void CaptureBackend::SetTarget(void* target)
{
    software_.SetTarget(target);
}

void CaptureBackend::ClearTarget(bool clearColor)
{
    software_.ClearTarget(clearColor);
}

uint8_t* CaptureBackend::LockTarget(void* target, int& pitch)
{
    return software_.LockTarget(target, pitch);
}

void CaptureBackend::UnlockTarget(void* target)
{
    software_.UnlockTarget(target);
}

void* CaptureBackend::GetTargetTexture(void* target)
{
    return software_.GetTargetTexture(target);
}

void CaptureBackend::UpdateTargetTexture(
    void* target, const uint8_t* pixels, int width, int height, int pitch)
{
    software_.UpdateTargetTexture(target, pixels, width, height, pitch);
}

void* CaptureBackend::CreateTexture(int width, int height)
{
    return software_.CreateTexture(width, height);
}

void CaptureBackend::UpdateTexture(
    void* texture, const uint8_t* pixels, int width, int height, int pitch)
{
    software_.UpdateTexture(texture, pixels, width, height, pitch);
}

void CaptureBackend::DestroyTexture(void* texture)
{
    software_.DestroyTexture(texture);
}

uint8_t* CaptureBackend::LockTexture(void* texture, int& pitch)
{
    return software_.LockTexture(texture, pitch);
}

void CaptureBackend::SetMask(void* maskTarget)
{
    software_.SetMask(maskTarget);
}

void CaptureBackend::SetBlendMode(int mode, const float* uniformColor)
{
    software_.SetBlendMode(mode, uniformColor);
}

void CaptureBackend::DrawMesh(const float* vertices,
                              int vertexCount,
                              const uint16_t* indices,
                              int indexCount,
                              void* texture,
                              float opacity,
                              const float* colorModulation)
{
    software_.DrawMesh(
        vertices, vertexCount, indices, indexCount, texture, opacity, colorModulation);
}

void CaptureBackend::LayerSetBlend(int method, float opacity, const float* uniformColor)
{
    software_.LayerSetBlend(method, opacity, uniformColor);
}

void CaptureBackend::LayerDrawRect(void* texture,
                                   float x,
                                   float y,
                                   float w,
                                   float h,
                                   float u0,
                                   float v0,
                                   float u1,
                                   float v1)
{
    software_.LayerDrawRect(texture, x, y, w, h, u0, v0, u1, v1);
}

FrameView CaptureBackend::AcquireFrame() const
{
    FrameView frame;
    frame.pixels = published_.empty() ? nullptr : published_.data();
    frame.width = published_width_;
    frame.height = published_height_;
    frame.stride = published_width_ * 4;
    frame.frame_id = frame_id_;
    frame.generation = generation_;
    return frame;
}
} // namespace art3m1s::krkr
