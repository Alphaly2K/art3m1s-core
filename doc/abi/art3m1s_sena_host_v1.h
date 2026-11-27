#ifndef ART3M1S_SENA_HOST_V1_H
#define ART3M1S_SENA_HOST_V1_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* All calls and callbacks run on the opening thread. The Host keeps the
 * callback context alive until close. Keys are UTF-8 relative project paths
 * with slash separators. Route keys under save/ to writable session storage.
 * stat and read return 0 on success, 1 for missing files, or a negative error.
 * stat writes the byte size and Unix modification time in milliseconds.
 * read fills the entire requested range. write replaces the complete file.
 * A null write buffer requests deletion. Callbacks may not reenter the table. */
typedef struct Art3m1sSenaFilesV1 {
    uint32_t struct_size;
    void* context;
    int32_t (*stat)(void* context, const char* key, uint64_t* size, uint64_t* modified_ms);
    int32_t (*read)(void* context, const char* key, uint64_t offset, uint8_t* output, size_t size);
    int32_t (*write)(void* context, const char* key, const uint8_t* data, size_t size);
} Art3m1sSenaFilesV1;

/* Functions return -1 for bad arguments, -2 for an invalid session, and -3
 * for engine failures. tick returns 1 for a halted session. rgba is a software
 * renderer for the latest frame and copies width*height*4 RGBA8 bytes. It
 * returns 1 before the first frame. pcm fills interleaved stereo signed 16-bit
 * samples at 48000 Hz (at most 4800 frames per call).
 * input kinds 0, 1, 2, 3, 4 and 5 are pointer move, button down, button up,
 * key down, key up and wheel. Button codes 0, 1 and 2 are left, right and middle.
 * device_audio actions 0, 1, 2 and 3 start, stop, suspend and resume optional
 * device output. Hosts that start device output should leave pcm to the adapter. */
typedef struct Art3m1sSenaHostV1 {
    uint32_t struct_size;
    uint32_t abi_version;
    int32_t (*open)(const Art3m1sSenaFilesV1* files, uint64_t* session);
    int32_t (*close)(uint64_t session);
    int32_t (*tick)(uint64_t session, uint32_t delta_ms, uint32_t* width, uint32_t* height);
    int32_t (*rgba)(uint64_t session, uint8_t* output, size_t capacity);
    int32_t (*pcm)(uint64_t session, int16_t* output, size_t frames);
    int32_t (*input)(uint64_t session, uint32_t kind, int32_t code, int32_t x, int32_t y,
                     const char* key);
    size_t (*last_error)(uint8_t* output, size_t capacity);
    int32_t (*device_audio)(uint64_t session, uint32_t action);
    /* Optional struct_size-gated entry. Copies twice the width and height.
     * Logical dimensions and input coordinates continue to come from tick. */
    int32_t (*rgba_2x)(uint64_t session, uint8_t* output, size_t capacity);
} Art3m1sSenaHostV1;

const Art3m1sSenaHostV1* art3m1s_sena_get_api_v1(void);

#ifdef __cplusplus
}
#endif
#endif
