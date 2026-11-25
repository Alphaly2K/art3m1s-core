#ifndef ART3M1S_RFVP_V2_H
#define ART3M1S_RFVP_V2_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ART3M1S_RFVP_V2_MAGIC UINT64_C(0x325646524D334152)
#define ART3M1S_RFVP_V2_MAJOR UINT32_C(2)

enum {
    ART3M1S_RFVP_V2_OK = 0,
    ART3M1S_RFVP_V2_NO_FRAME = 1,
    ART3M1S_RFVP_V2_ARGUMENT = -1,
    ART3M1S_RFVP_V2_HANDLE = -2,
    ART3M1S_RFVP_V2_ENGINE = -3,
    ART3M1S_RFVP_V2_VERSION = -4,
    ART3M1S_RFVP_V2_BUSY = -5,
    ART3M1S_RFVP_V2_LIMIT = -6,
};

enum {
    ART3M1S_RFVP_V2_SHIFT_JIS = 1,
    ART3M1S_RFVP_V2_GBK = 2,
    ART3M1S_RFVP_V2_UTF8 = 3,
    ART3M1S_RFVP_V2_EXITED = 1,
};

typedef struct Art3m1sRfvpBytesV2 {
    const uint8_t *data;
    uint64_t len;
} Art3m1sRfvpBytesV2;

typedef struct Art3m1sRfvpOpenV2 {
    uint32_t struct_size;
    uint32_t nls;
    Art3m1sRfvpBytesV2 project; /* Directory path in UTF-8. */
    Art3m1sRfvpBytesV2 saves; /* Empty selects the project's save directory. */
} Art3m1sRfvpOpenV2;

enum {
    ART3M1S_RFVP_V2_KEY = 1,
    ART3M1S_RFVP_V2_TEXT = 2,
    ART3M1S_RFVP_V2_POINTER_MOVE = 3,
    ART3M1S_RFVP_V2_POINTER_BUTTON = 4,
    ART3M1S_RFVP_V2_WHEEL = 5,
    ART3M1S_RFVP_V2_TOUCH = 6,
    ART3M1S_RFVP_V2_FOCUS = 7,
    ART3M1S_RFVP_V2_QUIT = 8,
    ART3M1S_RFVP_V2_DOWN = 0,
    ART3M1S_RFVP_V2_UP = 1,
    ART3M1S_RFVP_V2_REPEAT = 2,
    ART3M1S_RFVP_V2_MOVE = 3,
    ART3M1S_RFVP_V2_LEFT = 1,
    ART3M1S_RFVP_V2_RIGHT = 2,
    ART3M1S_RFVP_V2_MIDDLE = 4,
};

/* Keys use Windows virtual-key codes. Text supplies a Unicode scalar in code.
 * Focus sets phase to 1 to gain focus or 0 to lose it. Wheel uses x and y as
 * signed deltas. Pointer and touch coordinates use the logical stage. */
typedef struct Art3m1sRfvpInputV2 {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t code;
    uint32_t phase;
    int32_t x;
    int32_t y;
    int32_t value;
    uint32_t modifiers;
    uint64_t id;
} Art3m1sRfvpInputV2;

typedef struct Art3m1sRfvpRectV2 {
    int32_t x, y, width, height;
} Art3m1sRfvpRectV2;

typedef struct Art3m1sRfvpVertexV2 {
    float x, y, u, v;
    float rgba[4];
} Art3m1sRfvpVertexV2;

enum {
    ART3M1S_RFVP_V2_DRAW_IMAGE = 1,
    ART3M1S_RFVP_V2_DRAW_GLYPH = 2,
    ART3M1S_RFVP_V2_SET_CLIP = 3,
    ART3M1S_RFVP_V2_CLEAR_CLIP = 4,
    ART3M1S_RFVP_V2_NORMAL = 0,
    ART3M1S_RFVP_V2_ADD = 1,
    ART3M1S_RFVP_V2_SUBTRACT = 2,
    ART3M1S_RFVP_V2_MULTIPLY = 3,
    ART3M1S_RFVP_V2_NEAREST = 0,
    ART3M1S_RFVP_V2_LINEAR = 1,
    ART3M1S_RFVP_V2_RGBA8 = 1,
    ART3M1S_RFVP_V2_LUMA_ALPHA8 = 2,
    ART3M1S_RFVP_V2_HIT_ENABLED = 1,
    ART3M1S_RFVP_V2_HIT_VISIBLE = 2,
};

/* Clear the stage to opaque black before replay. Commands are in draw order.
 * Image quads use stage positions and normalized
 * UVs in vertices. Glyphs use source texels and logical destination bounds.
 * Clip coordinates are in the logical stage. has_clip marks a per-draw clip. */
typedef struct Art3m1sRfvpDrawV2 {
    uint32_t kind, texture, blend, filter, has_clip, effect;
    Art3m1sRfvpRectV2 source, destination, clip;
    uint8_t rgba[4];
    Art3m1sRfvpVertexV2 vertices[4];
} Art3m1sRfvpDrawV2;

typedef struct Art3m1sRfvpHitV2 {
    uint32_t prim_id, flags;
    Art3m1sRfvpRectV2 bounds;
    uint32_t order;
} Art3m1sRfvpHitV2;

typedef struct Art3m1sRfvpFrameV2 {
    uint32_t width, height, draw_count, hit_count, texture_count, reserved;
    const Art3m1sRfvpDrawV2 *draws;
    const Art3m1sRfvpHitV2 *hits;
} Art3m1sRfvpFrameV2;

/* Complete, tightly packed texture pixels. Generations are session-local.
 * A replay uses the textures of that frame, even if handles have been reused. */
typedef struct Art3m1sRfvpTextureV2 {
    uint32_t texture, format, width, height;
    uint64_t generation;
    Art3m1sRfvpBytesV2 pixels;
} Art3m1sRfvpTextureV2;

/* Calls are synchronous. Session calls from any thread run on the session's
 * native thread. Overlapping calls can return BUSY and can then be retried.
 * One replay session may be open per process. Close any Art3m1s RFVP v1
 * session before opening a v2 session. Use this table for all operations on
 * that session. Input arrays use sizeof(Art3m1sRfvpInputV2) stride and contain
 * at most 256 entries. A delta_ms of zero advances one millisecond.
 *
 * Each successful acquire grants a separate lease. The eight-lease limit is
 * process-wide. Views and texture bytes are immutable and may be read on any
 * thread, including after session close. Synchronize all readers before
 * releasing a lease. Release all leases and close the session before unload. */
typedef struct Art3m1sRfvpApiV2 {
    uint32_t struct_size, abi_major;
    uint64_t magic;
    int32_t (*open_directory)(const Art3m1sRfvpOpenV2 *, uint64_t *session);
    int32_t (*close)(uint64_t session);
    int32_t (*push_input)(uint64_t session, const Art3m1sRfvpInputV2 *, uint32_t count);
    int32_t (*step)(uint64_t session, uint32_t delta_ms, uint32_t *flags);
    int32_t (*acquire_frame)(uint64_t session, uint64_t *lease);
    int32_t (*frame_view)(uint64_t lease, Art3m1sRfvpFrameV2 *);
    int32_t (*frame_texture)(uint64_t lease, uint32_t index, Art3m1sRfvpTextureV2 *);
    int32_t (*release_frame)(uint64_t lease);
} Art3m1sRfvpApiV2;

/* Pass the requested ABI major and the size of the table prefix being used.
 * The returned table is immutable and valid for the library's lifetime. */
int32_t art3m1s_rfvp_get_api_v2(uint32_t major, uint32_t min_size,
                              const Art3m1sRfvpApiV2 **out);

#ifdef __cplusplus
}
#endif

#endif
