#include "art3m1s_krkr.h"

namespace
{
constexpr uint32_t kAbiVersion = 1;
constexpr uint64_t kAbiMagic = 0x31564B524D334152ULL; // "RA3MKRV1"

int32_t ProbeProject(const char*, Art3m1sKrkrProbeV1*)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeCreate(const char*,
                      const char*,
                      const Art3m1sKrkrRuntimeConfigV1*,
                      uint64_t*)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

void RuntimeDestroy(uint64_t)
{
}

uint32_t RuntimeStage(uint64_t)
{
    return 0;
}

uint32_t RuntimePixelBufferSize(uint64_t)
{
    return 0;
}

int32_t RuntimePushInput(uint64_t, const Art3m1sKrkrInputEventV1*, size_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeTick(uint64_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeAcquireFrame(uint64_t, Art3m1sKrkrFrameV1*)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeReleaseFrame(uint64_t, uint64_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimePollAudioCommand(uint64_t, Art3m1sKrkrAudioCommandV1*)
{
    return ART3M1S_KRKR_STATUS_NO_COMMAND;
}

int32_t RuntimeSubmitAudioConsumed(uint64_t, const Art3m1sKrkrAudioConsumedV1*)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeIsExitRequested(uint64_t)
{
    return 0;
}

int32_t RuntimeSetExternalSurface(uint64_t, int32_t, void*, uint32_t, uint32_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

const Art3m1sKrkrApiV1 kApi = {
    sizeof(Art3m1sKrkrApiV1),
    kAbiVersion,
    kAbiMagic,
    ProbeProject,
    RuntimeCreate,
    RuntimeDestroy,
    RuntimeStage,
    RuntimeStage,
    RuntimePixelBufferSize,
    RuntimePushInput,
    RuntimeTick,
    RuntimeAcquireFrame,
    RuntimeReleaseFrame,
    RuntimePollAudioCommand,
    RuntimeSubmitAudioConsumed,
    RuntimeIsExitRequested,
    RuntimeSetExternalSurface,
};
} // namespace

extern "C" ART3M1S_KRKR_EXPORT const Art3m1sKrkrApiV1* art3m1s_krkr_native_get_api_v1(
    size_t* out_size)
{
    if (out_size)
        *out_size = sizeof(Art3m1sKrkrApiV1);
    return &kApi;
}

extern "C" ART3M1S_KRKR_EXPORT int32_t art3m1s_krkr_native_set_render_host_v1(
    const Art3m1sKrkrRenderHostV1*)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

extern "C" ART3M1S_KRKR_EXPORT size_t art3m1s_krkr_native_log_next_bytes(void)
{
    return 0;
}

extern "C" ART3M1S_KRKR_EXPORT size_t art3m1s_krkr_native_poll_log(uint8_t*, size_t)
{
    return 0;
}

extern "C" ART3M1S_KRKR_EXPORT int32_t art3m1s_krkr_native_set_debug(int32_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}
