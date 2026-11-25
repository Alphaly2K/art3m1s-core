#pragma once

struct Art3m1sKrkrProjectHostV1;

namespace krkrsdl3
{
class iTVPRenderBackend;
}

// Embedded KRKR lifecycle used by Art3m1s. It deliberately initializes no SDL
// video subsystem and never creates an SDL_Window; the engine's logical Window
// objects render through the supplied backend instead.
bool Art3m1sKrkrHeadlessInit(
    int argc, char* argv[], krkrsdl3::iTVPRenderBackend* backend,
    const char* save_root_utf8, const Art3m1sKrkrProjectHostV1* project = nullptr,
    const char* archive_key = nullptr);
bool Art3m1sKrkrHeadlessIterate();
void Art3m1sKrkrHeadlessQuit();
