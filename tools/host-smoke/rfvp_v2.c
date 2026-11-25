#include "art3m1s_rfvp_v2.h"
#include <assert.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const Art3m1sRfvpApiV2 *api;
static void *advance(void *arg) {
    uint64_t session = *(const uint64_t *)arg;
    uint32_t flags = UINT32_MAX;
    Art3m1sRfvpInputV2 focus = {
        .struct_size = sizeof(focus), .kind = ART3M1S_RFVP_V2_FOCUS, .phase = 1,
    };
    assert(api->push_input(session, &focus, 1) == 0);
    assert(api->step(session, 16, &flags) == 0 && flags == 0);
    return NULL;
}

static void *read_frame(void *arg) {
    uint64_t frame = *(const uint64_t *)arg;
    Art3m1sRfvpFrameV2 view = {0};
    Art3m1sRfvpTextureV2 texture = {0};
    assert(api->frame_view(frame, &view) == 0);
    assert(view.width == 640 && view.height == 480);
    assert(view.draw_count == 1 && view.texture_count == 1);
    assert(view.draws[0].kind == ART3M1S_RFVP_V2_DRAW_IMAGE);
    assert(view.draws[0].destination.x == 8 && view.draws[0].destination.y == 12);
    assert(view.draws[0].destination.width == 16 && view.draws[0].destination.height == 20);
    assert(api->frame_texture(frame, 0, &texture) == 0);
    assert(api->frame_texture(frame, 1, &texture) == ART3M1S_RFVP_V2_ARGUMENT);
    assert(texture.texture == view.draws[0].texture && texture.generation > 0);
    assert(texture.format == ART3M1S_RFVP_V2_RGBA8);
    assert(texture.width == 1 && texture.height == 1 && texture.pixels.len == 4);
    for (unsigned i = 0; i < 4; i++) {
        assert((uint32_t)texture.pixels.data[i] * view.draws[0].rgba[i] / 255 == 255);
    }
    return NULL;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    assert(art3m1s_rfvp_get_api_v2(3, sizeof(*api), &api) == ART3M1S_RFVP_V2_VERSION);
    assert(api == NULL);
    assert(art3m1s_rfvp_get_api_v2(2, sizeof(*api) + 1, &api) == ART3M1S_RFVP_V2_VERSION);
    assert(art3m1s_rfvp_get_api_v2(2, offsetof(Art3m1sRfvpApiV2, open_with_io), &api) == 0);
    assert(art3m1s_rfvp_get_api_v2(2, sizeof(*api), &api) == 0);
    assert(api->abi_major == 2 && api->struct_size >= sizeof(*api));
    assert(api->magic == ART3M1S_RFVP_V2_MAGIC);
    Art3m1sRfvpOpenV2 open = {
        .struct_size = sizeof(open), .nls = ART3M1S_RFVP_V2_UTF8,
        .project = { (const uint8_t *)argv[1], strlen(argv[1]) },
    };
    uint64_t session = 0, old = 0, next = 0;
    uint32_t flags;
    assert(api->open_directory(&open, &session) == 0 && session != 0);
    assert(api->open_directory(&open, &next) == ART3M1S_RFVP_V2_BUSY && next == 0);
    assert(api->acquire_frame(session, &old) == ART3M1S_RFVP_V2_NO_FRAME && old == 0);
    assert(api->step(session, 16, &flags) == 0);
    pthread_t thread;
    assert(pthread_create(&thread, NULL, advance, &session) == 0);
    assert(pthread_join(thread, NULL) == 0);
    assert(api->acquire_frame(session, &old) == 0);
    assert(api->step(session, 16, &flags) == 0);
    uint64_t leases[7];
    for (unsigned i = 0; i < 7; i++) assert(api->acquire_frame(session, &leases[i]) == 0);
    assert(api->acquire_frame(session, &next) == ART3M1S_RFVP_V2_LIMIT && next == 0);
    for (unsigned i = 0; i < 7; i++) assert(api->release_frame(leases[i]) == 0);
    assert(api->acquire_frame(session, &next) == 0);
    assert(api->close(session) == 0);
    assert(api->step(session, 16, &flags) == ART3M1S_RFVP_V2_HANDLE && flags == 0);
    uint64_t replacement = 0;
    assert(api->open_directory(&open, &replacement) == 0 && replacement != session);
    assert(api->close(session) == ART3M1S_RFVP_V2_HANDLE);
    assert(api->close(replacement) == 0);
    assert(pthread_create(&thread, NULL, read_frame, &old) == 0);
    assert(pthread_join(thread, NULL) == 0);
    read_frame(&next);
    assert(api->release_frame(old) == 0);
    assert(api->release_frame(next) == 0);
    assert(api->release_frame(old) == ART3M1S_RFVP_V2_HANDLE);
    Art3m1sRfvpFrameV2 view;
    assert(api->frame_view(old, &view) == ART3M1S_RFVP_V2_HANDLE);
    puts("RFVP v2 smoke passed");
    return 0;
}
