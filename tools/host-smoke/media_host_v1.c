#include "art3m1s_media_host_v1.h"

#include <dlfcn.h>
#include <stdio.h>

static int probe(const ArtMediaHostApiV1 *api, const char *path) {
    ArtMediaVideo *video = api->open_video(path);
    if (!video) return 0;
    ArtMediaFrame frame;
    uint64_t previous_pts = 0;
    unsigned frames = 0;
    int32_t status;
    while ((status = api->next_video(video, &frame)) == 1) {
        if (frame.width != 32 || frame.height != 32 || frame.bytes != 32 * 32 * 4 ||
            !frame.rgba || frame.rgba[0] > 8 || frame.rgba[1] > 8 ||
            frame.rgba[2] < 245 || frame.rgba[3] != 255 ||
            (frames > 0 && frame.pts_us <= previous_pts) || frame.duration_us == 0) {
            status = -1;
            break;
        }
        previous_pts = frame.pts_us;
        if (++frames > 24) break;
    }
    api->close_video(video);
    return status == 0 && frames == 24;
}

static int probe_audio(const ArtMediaHostApiV1 *api, const char *path) {
    ArtMediaAudio *audio = NULL;
    if (api->open_audio(path, &audio) != 1 || !audio) return 0;
    ArtMediaPcm block;
    uint64_t previous_pts = 0;
    uint64_t frames = 0;
    int non_silent = 0;
    int32_t status;
    while ((status = api->next_audio(audio, &block)) == 1) {
        if (!block.stereo_i16 || !block.frames || (frames && block.pts_us <= previous_pts)) {
            status = -1;
            break;
        }
        for (uint32_t i = 0; i < block.frames * 2; ++i) {
            if (block.stereo_i16[i] != 0) non_silent = 1;
        }
        previous_pts = block.pts_us;
        frames += block.frames;
        if (frames > 52000) break;
    }
    api->close_audio(audio);
    return status == 0 && non_silent && frames >= 44000 && frames <= 52000;
}

int main(int argc, char **argv) {
    if (argc < 4) return 1;
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!library) {
        fprintf(stderr, "%s\n", dlerror());
        return 1;
    }
    const ArtMediaHostApiV1 *(*get_api)(void) =
        (const ArtMediaHostApiV1 *(*)(void))dlsym(library, "art3m1s_artemis_media_get_api_v1");
    const ArtMediaHostApiV1 *api = get_api ? get_api() : NULL;
    if (!api || api->abi_version != 1 || api->size < sizeof(*api)) return 1;
    ArtMediaVideo *unsupported = api->open_video(argv[2]);
    if (unsupported) {
        api->close_video(unsupported);
        fputs("The legacy decoder accepted an unsupported container.\n", stderr);
        return 1;
    }
    for (int i = 3; i < argc; ++i) {
        if (!probe(api, argv[i])) {
            fputs("Legacy video did not deliver 24 timestamped blue frames and EOF.\n", stderr);
            return 1;
        }
    }
    if (!probe_audio(api, argv[3])) {
        fputs("WMV did not deliver timestamped stereo PCM and EOF.\n", stderr);
        return 1;
    }
    for (int i = 4; i < argc; ++i) {
        ArtMediaAudio *silent = NULL;
        if (api->open_audio(argv[i], &silent) != 0 || silent) {
            api->close_audio(silent);
            fputs("Silent Theora video did not report an absent audio track.\n", stderr);
            return 1;
        }
    }
    dlclose(library);
    puts("Artemis media Host decode smoke passed");
    return 0;
}
