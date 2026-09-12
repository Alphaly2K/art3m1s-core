#include "art3m1s_krkr.h"
#include "capture_backend.h"

#define SDL_MAIN_USE_CALLBACKS
#define SDL_MAIN_NOIMPL
#include <SDL3/SDL_main.h>

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <filesystem>
#include <limits>
#include <memory>
#include <string>
#include <vector>

#include "TVPApplication.h"
#include "TVPCompositor.h"
#include "TVPSystem.h"
#include "TVPWindow.h"
#include "WindowManager.h"

namespace
{
constexpr uint32_t kAbiVersion = 1;
constexpr uint64_t kAbiMagic = 0x31564B524D334152ULL; // "RA3MKRV1"

struct Runtime
{
    art3m1s::krkr::CaptureBackend* capture = nullptr;
    uint32_t width = 0;
    uint32_t height = 0;
    bool exit_requested = false;
    std::string game_root;
};

std::string LowerAscii(std::string value)
{
    std::transform(value.begin(), value.end(), value.begin(),
                   [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    return value;
}

bool FileNameEquals(const std::filesystem::path& path, const char* expected)
{
    return LowerAscii(path.filename().string()) == LowerAscii(expected);
}

bool HasExtension(const std::filesystem::path& path, const char* expected)
{
    return LowerAscii(path.extension().string()) == std::string(".") + LowerAscii(expected);
}

std::string NormalizeRuntimePath(std::string path)
{
    std::error_code error;
    const std::filesystem::path parsed(path);
    if (std::filesystem::is_directory(parsed, error) && !error &&
        (path.empty() || (path.back() != '/' && path.back() != '\\')))
    {
        path.push_back('/');
    }
    return path;
}

void DestroyWindowTextures()
{
    for (tjs_int index = 0; index < TVPGetWindowCount(); ++index)
    {
        TVPWindow* window = TVPGetWindowListAt(index);
        if (window && window->pSprite && window->pSprite->texture)
            krkrsdl3::TVPDestroyTexture(window->pSprite);
    }
}

void RecreateWindowTextures()
{
    for (tjs_int index = 0; index < TVPGetWindowCount(); ++index)
    {
        TVPWindow* window = TVPGetWindowListAt(index);
        if (window && window->pSprite && !window->pSprite->texture)
        {
            krkrsdl3::TVPCreateTexture(*window->pSprite);
            if (window->pSprite->texture)
                window->UpdateContent();
        }
    }
}

uint32_t PreferredKind(const Art3m1sKrkrProbeV1& probe)
{
    if (probe.has_data_xp3)
        return ART3M1S_KRKR_PROBE_DATA_XP3;
    if (probe.root_xp3_count)
        return ART3M1S_KRKR_PROBE_ROOT_XP3;
    if (probe.has_startup_tjs)
        return ART3M1S_KRKR_PROBE_STARTUP_TJS;
    if (probe.has_system_initialize_tjs)
        return ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS;
    return 0;
}

int32_t ProbeProjectImpl(const char* game_root_utf8, Art3m1sKrkrProbeV1* out_probe)
{
    if (!game_root_utf8 || !out_probe ||
        out_probe->struct_size != sizeof(Art3m1sKrkrProbeV1))
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

    std::error_code error;
    const std::filesystem::path root(game_root_utf8);
    if (!std::filesystem::exists(root, error) || error)
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

    Art3m1sKrkrProbeV1 probe{};
    probe.struct_size = sizeof(Art3m1sKrkrProbeV1);

    if (std::filesystem::is_regular_file(root, error) && !error)
    {
        if (!HasExtension(root, "xp3"))
        {
            *out_probe = probe;
            return ART3M1S_KRKR_STATUS_OK;
        }
        if (FileNameEquals(root, "data.xp3"))
            probe.has_data_xp3 = 1;
        else
            probe.root_xp3_count = 1;
        probe.preferred_kind = PreferredKind(probe);
        *out_probe = probe;
        return ART3M1S_KRKR_STATUS_OK;
    }

    if (!std::filesystem::is_directory(root, error) || error)
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

    std::filesystem::path system_dir;
    for (const auto& entry : std::filesystem::directory_iterator(root, error))
    {
        if (error)
            return ART3M1S_KRKR_STATUS_ENGINE;
        if (entry.is_directory(error) && !error)
        {
            if (FileNameEquals(entry.path(), "system"))
                system_dir = entry.path();
            continue;
        }
        if (!entry.is_regular_file(error) || error)
            continue;

        const auto& path = entry.path();
        if (FileNameEquals(path, "data.xp3"))
            probe.has_data_xp3 = 1;
        else if (HasExtension(path, "xp3"))
            ++probe.root_xp3_count;
        else if (FileNameEquals(path, "startup.tjs"))
            probe.has_startup_tjs = 1;
        else if (FileNameEquals(path, "patch.tjs"))
            probe.has_patch_tjs = 1;
    }

    if (!system_dir.empty())
    {
        for (const auto& entry : std::filesystem::directory_iterator(system_dir, error))
        {
            if (error)
                return ART3M1S_KRKR_STATUS_ENGINE;
            if (entry.is_regular_file(error) && !error &&
                FileNameEquals(entry.path(), "Initialize.tjs"))
            {
                probe.has_system_initialize_tjs = 1;
                break;
            }
        }
    }

    probe.preferred_kind = PreferredKind(probe);
    *out_probe = probe;
    return ART3M1S_KRKR_STATUS_OK;
}

tTVPMouseButton ToMouseButton(uint32_t code)
{
    if (code == ART3M1S_KRKR_POINTER_RIGHT)
        return mbRight;
    if (code == ART3M1S_KRKR_POINTER_MIDDLE)
        return mbMiddle;
    if (code == ART3M1S_KRKR_POINTER_X1)
        return mbX1;
    if (code == ART3M1S_KRKR_POINTER_X2)
        return mbX2;
    return mbLeft;
}

int32_t RuntimeCreateImpl(const char* game_root_utf8,
                          const char* save_root_utf8,
                          const Art3m1sKrkrRuntimeConfigV1* config,
                          uint64_t* out_runtime)
{
    if (!game_root_utf8 || !config || !out_runtime ||
        config->struct_size != sizeof(Art3m1sKrkrRuntimeConfigV1) ||
        config->width == 0 || config->height == 0)
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
    if (save_root_utf8 && *save_root_utf8)
        return ART3M1S_KRKR_STATUS_UNSUPPORTED;
    if (Application)
        return ART3M1S_KRKR_STATUS_ENGINE;

#ifdef ART3M1S_KRKR_RESOURCE_EXE
    std::string program = ART3M1S_KRKR_RESOURCE_EXE;
#else
    std::string program = "art3m1s-krkr";
#endif
    std::string game_root = NormalizeRuntimePath(game_root_utf8);
    std::string window_arg =
        "-window=" + std::to_string(config->width) + "x" + std::to_string(config->height);
    std::string render_arg = "-render=software";
    std::vector<char*> argv{
        program.data(),
        game_root.data(),
        window_arg.data(),
        render_arg.data(),
        nullptr,
    };

    const SDL_AppResult result =
        SDL_AppInit(nullptr, static_cast<int>(argv.size() - 1), argv.data());
    if (result == SDL_APP_FAILURE)
        return ART3M1S_KRKR_STATUS_ENGINE;
    if (!Application || TVPGetWindowCount() <= 0)
    {
        SDL_AppQuit(nullptr, SDL_APP_FAILURE);
        return ART3M1S_KRKR_STATUS_ENGINE;
    }

    // SDL_AppInit creates the first window with the SDL software backend.
    // Release those handles before deleting the backend, then create equivalent
    // capture-backed textures for every existing window.
    DestroyWindowTextures();
    krkrsdl3::TVPShutdownRenderBackend();
    auto* capture = new art3m1s::krkr::CaptureBackend;
    krkrsdl3::TVPSetRenderBackend(capture);
    RecreateWindowTextures();

    tjs_int width = 0;
    tjs_int height = 0;
    TVPGetWindowListAt(0)->GetSize(width, height);
    if (width <= 0 || height <= 0)
    {
        delete capture;
        krkrsdl3::TVPSetRenderBackend(nullptr);
        SDL_AppQuit(nullptr, SDL_APP_FAILURE);
        return ART3M1S_KRKR_STATUS_ENGINE;
    }

    auto runtime = std::make_unique<Runtime>();
    runtime->capture = capture;
    runtime->width = static_cast<uint32_t>(width);
    runtime->height = static_cast<uint32_t>(height);
    runtime->game_root = std::move(game_root);
    *out_runtime = reinterpret_cast<uint64_t>(runtime.release());
    return ART3M1S_KRKR_STATUS_OK;
}

Runtime* GetRuntime(uint64_t handle)
{
    return reinterpret_cast<Runtime*>(handle);
}

void RuntimeDestroyImpl(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime)
        return;
    runtime->capture = nullptr;
    SDL_AppQuit(nullptr, SDL_APP_SUCCESS);
    delete runtime;
}

uint32_t RuntimeStageWidth(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    return runtime ? runtime->width : 0;
}

uint32_t RuntimeStageHeight(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    return runtime ? runtime->height : 0;
}

uint32_t RuntimePixelBufferSize(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime)
        return 0;
    const uint64_t size =
        static_cast<uint64_t>(runtime->width) * runtime->height * 4ULL;
    return size <= std::numeric_limits<uint32_t>::max()
               ? static_cast<uint32_t>(size)
               : 0;
}

int32_t RuntimePushInputImpl(uint64_t handle,
                             const Art3m1sKrkrInputEventV1* events,
                             size_t event_count)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime || (!events && event_count))
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

    for (size_t index = 0; index < event_count; ++index)
    {
        const Art3m1sKrkrInputEventV1& event = events[index];
        if (event.struct_size != sizeof(Art3m1sKrkrInputEventV1))
            return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

        switch (event.kind)
        {
            case ART3M1S_KRKR_INPUT_KEY:
                if (event.phase == ART3M1S_KRKR_INPUT_PHASE_UP)
                    krkrsdl3::KRKR_Trig_KeyUp(static_cast<int>(event.code));
                else
                    krkrsdl3::KRKR_Trig_KeyDown(static_cast<int>(event.code));
                break;
            case ART3M1S_KRKR_INPUT_TEXT:
            {
                TVPWindow* window = TVPGetActiveWindow();
                if (window && event.code <= 0xFFFF)
                    window->PostKeyPress(static_cast<tjs_uint16>(event.code));
                break;
            }
            case ART3M1S_KRKR_INPUT_POINTER_MOVE:
                krkrsdl3::KRKR_Trig_MouseMove(event.x, event.y);
                break;
            case ART3M1S_KRKR_INPUT_POINTER_BUTTON:
            {
                const tTVPMouseButton button = ToMouseButton(event.code);
                if (event.phase == ART3M1S_KRKR_INPUT_PHASE_UP)
                    krkrsdl3::KRKR_Trig_MouseUp(button, event.x, event.y);
                else
                    krkrsdl3::KRKR_Trig_MouseDown(button, event.x, event.y);
                break;
            }
            case ART3M1S_KRKR_INPUT_WHEEL:
                krkrsdl3::KRKR_Trig_MouseScroll(0, event.value, event.x, event.y);
                break;
            case ART3M1S_KRKR_INPUT_FOCUS:
                if (event.phase == ART3M1S_KRKR_INPUT_PHASE_UP)
                    TVPPostApplicationDeactivateEvent();
                else
                    TVPPostApplicationActivateEvent();
                break;
            case ART3M1S_KRKR_INPUT_QUIT:
                runtime->exit_requested = true;
                Application->Terminate();
                break;
            default:
                return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
        }
    }

    return ART3M1S_KRKR_STATUS_OK;
}

int32_t RuntimeTickImpl(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime)
        return ART3M1S_KRKR_STATUS_INVALID_HANDLE;
    const SDL_AppResult result = SDL_AppIterate(nullptr);
    if (result == SDL_APP_FAILURE)
        return ART3M1S_KRKR_STATUS_ENGINE;
    if (result == SDL_APP_SUCCESS)
        runtime->exit_requested = true;
    return ART3M1S_KRKR_STATUS_OK;
}

int32_t RuntimeAcquireFrameImpl(uint64_t handle, Art3m1sKrkrFrameV1* out_frame)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime || !out_frame || out_frame->struct_size != sizeof(Art3m1sKrkrFrameV1))
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;

    const art3m1s::krkr::FrameView frame = runtime->capture->AcquireFrame();
    if (!frame.pixels || !frame.width || !frame.height)
        return ART3M1S_KRKR_STATUS_NO_FRAME;

    *out_frame = {};
    out_frame->struct_size = sizeof(Art3m1sKrkrFrameV1);
    out_frame->format = ART3M1S_KRKR_FRAME_FORMAT_RGBA8;
    out_frame->width = frame.width;
    out_frame->height = frame.height;
    out_frame->stride = frame.stride;
    out_frame->frame_id = frame.frame_id;
    out_frame->generation = frame.generation;
    out_frame->pixels = frame.pixels;
    out_frame->pixels_len =
        static_cast<size_t>(frame.stride) * static_cast<size_t>(frame.height);
    return ART3M1S_KRKR_STATUS_OK;
}

int32_t RuntimeReleaseFrameImpl(uint64_t handle, uint64_t frame_id)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime)
        return ART3M1S_KRKR_STATUS_INVALID_HANDLE;
    const auto frame = runtime->capture->AcquireFrame();
    return frame.frame_id == frame_id ? ART3M1S_KRKR_STATUS_OK
                                      : ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
}

int32_t RuntimePollAudioCommandImpl(uint64_t handle, Art3m1sKrkrAudioCommandV1* out_command)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime || !out_command ||
        out_command->struct_size != sizeof(Art3m1sKrkrAudioCommandV1))
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
    return ART3M1S_KRKR_STATUS_NO_COMMAND;
}

int32_t RuntimeSubmitAudioConsumedImpl(
    uint64_t handle, const Art3m1sKrkrAudioConsumedV1* consumed)
{
    Runtime* runtime = GetRuntime(handle);
    if (!runtime || !consumed ||
        consumed->struct_size != sizeof(Art3m1sKrkrAudioConsumedV1))
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t RuntimeIsExitRequestedImpl(uint64_t handle)
{
    Runtime* runtime = GetRuntime(handle);
    return runtime && (runtime->exit_requested || !Application || Application->IsTarminate());
}

int32_t RuntimeSetExternalSurfaceImpl(uint64_t, int32_t, void*, uint32_t, uint32_t)
{
    return ART3M1S_KRKR_STATUS_UNSUPPORTED;
}

int32_t ProbeProjectNoThrow(const char* game_root_utf8, Art3m1sKrkrProbeV1* out_probe) noexcept
{
    try
    {
        return ProbeProjectImpl(game_root_utf8, out_probe);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeCreateNoThrow(const char* game_root_utf8,
                             const char* save_root_utf8,
                             const Art3m1sKrkrRuntimeConfigV1* config,
                             uint64_t* out_runtime) noexcept
{
    try
    {
        return RuntimeCreateImpl(game_root_utf8, save_root_utf8, config, out_runtime);
    }
    catch (...)
    {
        if (Application)
        {
            try
            {
                SDL_AppQuit(nullptr, SDL_APP_FAILURE);
            }
            catch (...)
            {
            }
        }
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

void RuntimeDestroyNoThrow(uint64_t handle) noexcept
{
    try
    {
        RuntimeDestroyImpl(handle);
    }
    catch (...)
    {
    }
}

uint32_t RuntimeStageNoThrow(uint64_t handle) noexcept
{
    try
    {
        return RuntimeStageWidth(handle);
    }
    catch (...)
    {
        return 0;
    }
}

uint32_t RuntimeStageHeightNoThrow(uint64_t handle) noexcept
{
    try
    {
        return RuntimeStageHeight(handle);
    }
    catch (...)
    {
        return 0;
    }
}

uint32_t RuntimePixelBufferSizeNoThrow(uint64_t handle) noexcept
{
    try
    {
        return RuntimePixelBufferSize(handle);
    }
    catch (...)
    {
        return 0;
    }
}

int32_t RuntimePushInputNoThrow(uint64_t handle,
                                const Art3m1sKrkrInputEventV1* events,
                                size_t event_count) noexcept
{
    try
    {
        return RuntimePushInputImpl(handle, events, event_count);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeTickNoThrow(uint64_t handle) noexcept
{
    try
    {
        return RuntimeTickImpl(handle);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeAcquireFrameNoThrow(uint64_t handle, Art3m1sKrkrFrameV1* out_frame) noexcept
{
    try
    {
        return RuntimeAcquireFrameImpl(handle, out_frame);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeReleaseFrameNoThrow(uint64_t handle, uint64_t frame_id) noexcept
{
    try
    {
        return RuntimeReleaseFrameImpl(handle, frame_id);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimePollAudioCommandNoThrow(
    uint64_t handle, Art3m1sKrkrAudioCommandV1* out_command) noexcept
{
    try
    {
        return RuntimePollAudioCommandImpl(handle, out_command);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeSubmitAudioConsumedNoThrow(
    uint64_t handle, const Art3m1sKrkrAudioConsumedV1* consumed) noexcept
{
    try
    {
        return RuntimeSubmitAudioConsumedImpl(handle, consumed);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

int32_t RuntimeIsExitRequestedNoThrow(uint64_t handle) noexcept
{
    try
    {
        return RuntimeIsExitRequestedImpl(handle);
    }
    catch (...)
    {
        return 1;
    }
}

int32_t RuntimeSetExternalSurfaceNoThrow(
    uint64_t handle, int32_t kind, void* surface, uint32_t width, uint32_t height) noexcept
{
    try
    {
        return RuntimeSetExternalSurfaceImpl(handle, kind, surface, width, height);
    }
    catch (...)
    {
        return ART3M1S_KRKR_STATUS_ENGINE;
    }
}

const Art3m1sKrkrApiV1 kApi = {
    sizeof(Art3m1sKrkrApiV1),
    kAbiVersion,
    kAbiMagic,
    ProbeProjectNoThrow,
    RuntimeCreateNoThrow,
    RuntimeDestroyNoThrow,
    RuntimeStageNoThrow,
    RuntimeStageHeightNoThrow,
    RuntimePixelBufferSizeNoThrow,
    RuntimePushInputNoThrow,
    RuntimeTickNoThrow,
    RuntimeAcquireFrameNoThrow,
    RuntimeReleaseFrameNoThrow,
    RuntimePollAudioCommandNoThrow,
    RuntimeSubmitAudioConsumedNoThrow,
    RuntimeIsExitRequestedNoThrow,
    RuntimeSetExternalSurfaceNoThrow,
};
} // namespace

extern "C" ART3M1S_KRKR_EXPORT const Art3m1sKrkrApiV1* art3m1s_krkr_get_api_v1(
    size_t* out_size)
{
    if (out_size)
        *out_size = sizeof(Art3m1sKrkrApiV1);
    return &kApi;
}
