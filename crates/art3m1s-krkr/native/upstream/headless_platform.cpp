#include "headless_platform.h"

#include "tjsCommHead.h"

#include <SDL3/SDL_init.h>
#include <SDL3/SDL_scancode.h>

#include <string>
#include <vector>

#include "Platform.h"
#include "PlatformView.h"
#include "TVPApplication.h"
#include "TVPCompositor.h"
#include "TVPSettings.h"
#include "TVPSystem.h"
#include "TVPWindow.h"
#include "WindowManager.h"

namespace
{
int g_width = 1280;
int g_height = 720;
std::string g_title = "TVP Engine";
bool g_initialized = false;
// Kirikiri's TJS object pools are process-global and cannot be reconstructed
// after TVPUninitScriptEngine(). A second HeadlessInit in the same process
// previously SIGSEGV'd in tTJSScriptBlock::tTJSScriptBlock. Refuse it.
bool g_process_consumed = false;
}

// The embedded host may deliver input while a script is inside showModal().
// Pump it on every nested KRKR iteration, not only on top-level runtime ticks.
void Art3m1sKrkrPumpInput();

bool Art3m1sKrkrHeadlessInit(
    int argc, char* argv[], krkrsdl3::iTVPRenderBackend* backend)
{
    if (g_process_consumed || g_initialized || Application || argc < 2 || !backend)
        return false;
    if (!TVPParseArguments(argc, argv))
        return false;

    // SDL remains a utility dependency for files, timers and threading, but
    // no subsystem that can manufacture a native window is initialized here.
    if (!SDL_Init(0))
        return false;

    g_width = TVPSettings.window_width;
    g_height = TVPSettings.window_height;
    krkrsdl3::TVPSetRenderBackend(backend);

    Application = new tTVPApplication;
    // StartApplication() constructs the process-global TJS engine. From this
    // point the process cannot host another KRKR runtime, even if startup fails.
    g_process_consumed = true;
    if (!Application->StartApplication())
    {
        delete Application;
        Application = nullptr;
        krkrsdl3::TVPShutdownRenderBackend();
        SDL_Quit();
        TVPClearAllArguments();
        return false;
    }

    g_initialized = true;
    if (Art3m1sKrkrHeadlessIterate())
        return true;
    Art3m1sKrkrHeadlessQuit();
    return false;
}

bool Art3m1sKrkrHeadlessIterate()
{
    if (!g_initialized || !Application)
        return false;
    Art3m1sKrkrPumpInput();
    if (!Application->Run())
        return false;
    krkrsdl3::TVPRenderOnce(g_width, g_height);
    return true;
}

void Art3m1sKrkrHeadlessQuit()
{
    if (Application)
    {
        Application->OnExit();
        delete Application;
        Application = nullptr;
    }
    krkrsdl3::TVPShutdownRenderBackend();
    SDL_Quit();
    TVPClearAllArguments();
    g_initialized = false;
}

void TVPSetWindowTitle(const char* title)
{
    g_title = title ? title : "";
}

std::string TVPGetWindowTitle()
{
    return g_title;
}

void TVPSetWindowFullscreen(bool)
{
}

void TVPGetWindowSize(int* width, int* height)
{
    if (width)
        *width = g_width;
    if (height)
        *height = g_height;
}

void TVPGetWindowSizeInPixels(int* width, int* height)
{
    TVPGetWindowSize(width, height);
}

void TVPSetWindowSize(int width, int height)
{
    if (width > 0)
        g_width = width;
    if (height > 0)
        g_height = height;
}

int TVPDrawSceneOnce(int interval)
{
    static tjs_uint64 last_tick = TVPGetRoughTickCount();
    const tjs_uint64 current_tick = TVPGetRoughTickCount();
    const int remain = interval - static_cast<int>(current_tick - last_tick);
    if (remain > 0)
        return remain;
    Art3m1sKrkrHeadlessIterate();
    last_tick = current_tick;
    return 0;
}

void TVPShowIME(int, int, int, int)
{
}

void TVPHideIME()
{
}

int TVPShowSimpleInputBox(ttstr&,
                          const ttstr&,
                          const ttstr&,
                          const std::vector<ttstr>&)
{
    // A host-owned input-dialog command will replace this compatibility stub.
    // Creating a private SDL window here would violate the embedding contract.
    return 1;
}

int TVPConvertKeyCodeToVKCode(int key_code)
{
#define CASE(name) \
    case SDL_SCANCODE_##name: \
        return VK_##name

    switch (static_cast<SDL_Scancode>(key_code))
    {
        CASE(0);
        CASE(1);
        CASE(2);
        CASE(3);
        CASE(4);
        CASE(5);
        CASE(6);
        CASE(7);
        CASE(8);
        CASE(9);
        CASE(A);
        CASE(B);
        CASE(C);
        CASE(D);
        CASE(E);
        CASE(F);
        CASE(G);
        CASE(H);
        CASE(I);
        CASE(J);
        CASE(K);
        CASE(L);
        CASE(M);
        CASE(N);
        CASE(O);
        CASE(P);
        CASE(Q);
        CASE(R);
        CASE(S);
        CASE(T);
        CASE(U);
        CASE(V);
        CASE(W);
        CASE(X);
        CASE(Y);
        CASE(Z);
        CASE(F1);
        CASE(F2);
        CASE(F3);
        CASE(F4);
        CASE(F5);
        CASE(F6);
        CASE(F7);
        CASE(F8);
        CASE(F9);
        CASE(F10);
        CASE(F11);
        CASE(F12);
        CASE(PAUSE);
        CASE(ESCAPE);
        CASE(CANCEL);
        CASE(INSERT);
        CASE(HOME);
        CASE(DELETE);
        CASE(END);
        CASE(SPACE);
        case SDL_SCANCODE_PRINTSCREEN:
            return VK_PRINT;
        CASE(TAB);
        CASE(RETURN);
        case SDL_SCANCODE_SCROLLLOCK:
            return VK_SCROLL;
        case SDL_SCANCODE_SYSREQ:
            return VK_SNAPSHOT;
        case SDL_SCANCODE_BACKSPACE:
            return VK_BACK;
        case SDL_SCANCODE_CAPSLOCK:
            return VK_CAPITAL;
        case SDL_SCANCODE_LSHIFT:
        case SDL_SCANCODE_RSHIFT:
            return VK_SHIFT;
        case SDL_SCANCODE_LCTRL:
        case SDL_SCANCODE_RCTRL:
            return VK_CONTROL;
        case SDL_SCANCODE_LALT:
        case SDL_SCANCODE_RALT:
            return VK_MENU;
        case SDL_SCANCODE_MENU:
            return VK_APPS;
        case SDL_SCANCODE_PAGEUP:
            return VK_PRIOR;
        case SDL_SCANCODE_PAGEDOWN:
            return VK_NEXT;
        case SDL_SCANCODE_LEFT:
            return VK_LEFT;
        case SDL_SCANCODE_RIGHT:
            return VK_RIGHT;
        case SDL_SCANCODE_UP:
            return VK_UP;
        case SDL_SCANCODE_DOWN:
            return VK_DOWN;
        case SDL_SCANCODE_NUMLOCKCLEAR:
            return VK_NUMLOCK;
        case SDL_SCANCODE_KP_PLUS:
            return VK_ADD;
        case SDL_SCANCODE_KP_MINUS:
            return VK_SUBTRACT;
        case SDL_SCANCODE_KP_MULTIPLY:
            return VK_MULTIPLY;
        case SDL_SCANCODE_KP_DIVIDE:
            return VK_DIVIDE;
        case SDL_SCANCODE_KP_ENTER:
            return VK_RETURN;
        case SDL_SCANCODE_COMMA:
            return VK_OEM_COMMA;
        case SDL_SCANCODE_MINUS:
            return VK_OEM_MINUS;
        case SDL_SCANCODE_PERIOD:
            return VK_OEM_PERIOD;
        case SDL_SCANCODE_EQUALS:
            return VK_OEM_PLUS;
        case SDL_SCANCODE_SLASH:
            return VK_OEM_2;
        case SDL_SCANCODE_SEMICOLON:
            return VK_OEM_1;
        case SDL_SCANCODE_BACKSLASH:
            return VK_OEM_5;
        case SDL_SCANCODE_LEFTBRACKET:
            return VK_OEM_4;
        case SDL_SCANCODE_RIGHTBRACKET:
            return VK_OEM_6;
        case SDL_SCANCODE_MEDIA_PLAY:
            return VK_PLAY;
        default:
            return 0;
    }
#undef CASE
}

std::vector<std::string> TVPListAllRenderBackend()
{
    return {"art3m1s-headless"};
}

bool TVPSoftwareRenderBackendAvailable()
{
    return false;
}

void TVPCreateTextureBackend(TVPSprite&)
{
}

void TVPUpdateTextureBackend(TVPSprite*, uint8_t*, int, int, int)
{
}

void TVPDestroyTextureBackend(TVPSprite*)
{
}

void TVPRenderTextureBackend(TVPSprite*, int, int, int, int)
{
}

void TVPRenderClearBackend()
{
}

void TVPRenderPresentBackend()
{
}

void TVPSwapBuffersBackend()
{
}
