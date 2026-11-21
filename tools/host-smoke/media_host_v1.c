#include "art3m1s_media_host_v1.h"

#include <dlfcn.h>
#include <stdio.h>

static int probe(const ArtMediaHostApiV1 *api, const char *path, unsigned expected_frames,
                 uint64_t expected_hold_end_us) {
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
            (frames == 0 ? frame.pts_us > 50000 : frame.pts_us <= previous_pts) ||
            frame.duration_us == 0) {
            status = -1;
            break;
        }
        previous_pts = frame.pts_us;
        if (++frames > expected_frames) break;
    }
    api->close_video(video);
    return status == 0 && frames == expected_frames && frame.pts_us == expected_hold_end_us;
}

static int probe_audio(const ArtMediaHostApiV1 *api, const char *path, int delayed) {
    ArtMediaAudio *audio = NULL;
    if (api->open_audio(path, &audio) != 1 || !audio) return 0;
    ArtMediaPcm block;
    uint64_t previous_pts = 0;
    uint64_t end = 0;
    uint64_t frames = 0;
    unsigned gaps = 0;
    uint64_t first_pts = 0;
    int non_silent = 0;
    int32_t status;
    while ((status = api->next_audio(audio, &block)) == 1) {
        if (!block.stereo_i16 || !block.frames || (frames && block.pts_us <= previous_pts)) {
            status = -1;
            break;
        }
        if (!frames) {
            first_pts = block.pts_us;
        } else if (block.pts_us > end + 100000) {
            const uint64_t gap = block.pts_us - end;
            if (gap < 850000 || gap > 1150000) {
                status = -1;
                break;
            }
            gaps++;
        }
        for (uint32_t i = 0; i < block.frames * 2; ++i) {
            if (block.stereo_i16[i] != 0) non_silent = 1;
        }
        previous_pts = block.pts_us;
        end = block.pts_us + (uint64_t)block.frames * 1000000 / 48000;
        frames += block.frames;
        if (frames > (delayed ? 200000u : 52000u)) break;
    }
    api->close_audio(audio);
    if (status != 0 || !non_silent) return 0;
    if (delayed) {
        return first_pts >= 650000 && first_pts <= 950000 && gaps == 1 &&
            end >= 5600000 && end <= 6000000 && frames >= 185000 && frames <= 200000;
    }
    return first_pts <= 120000 && gaps == 0 && frames >= 44000 && frames <= 52000;
}

int main(int argc, char **argv) {
    if (argc != 8) return 1;
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
    for (int i = 3; i < 6; ++i) {
        if (!probe(api, argv[i], i == 4 ? 144 : 24, 0)) {
            fputs("Legacy video did not deliver normalized blue frames and EOF.\n", stderr);
            return 1;
        }
    }
    if (!probe(api, argv[6], 2, 1000000)) {
        fputs("Theora did not preserve its implicit frame hold through EOF.\n", stderr);
        return 1;
    }
    if (!probe(api, argv[7], 24, 0)) {
        fputs("AVI did not deliver its 24 decoded frames and EOF.\n", stderr);
        return 1;
    }
    if (!probe_audio(api, argv[3], 0)) {
        fputs("WMV did not deliver timestamped stereo PCM and EOF.\n", stderr);
        return 1;
    }
    if (!probe_audio(api, argv[4], 1)) {
        fputs("Delayed WMV audio did not preserve its initial offset and midstream gap.\n", stderr);
        return 1;
    }
    ArtMediaAudio *silent = NULL;
    if (api->open_audio(argv[5], &silent) != 0 || silent) {
        api->close_audio(silent);
        fputs("Silent Theora video did not report an absent audio track.\n", stderr);
        return 1;
    }
    dlclose(library);
    puts("Artemis media Host decode smoke passed");
    return 0;
}
