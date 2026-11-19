#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include "artemis_shader_pass.h"

#include <fcntl.h>
#include <io.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { MAGIC = 0x31584441, COMPILE = 1, DRAW = 2, RELEASE = 3, TEXTURE_BINDING = 4, MAX_PROGRAMS = 64,
       MAX_BYTES = 64 * 1024 * 1024, MAX_NAME = 256 };

typedef struct Header { uint32_t magic, op, id, bytes; } Header;
typedef struct Program { uint32_t id; ArtD3D11Program *value; } Program;
typedef struct Bytes { const uint8_t *data; size_t size; } Bytes;

_Static_assert(sizeof(Header) == 16, "ADX1 header layout");
_Static_assert(sizeof(ArtD3D11Vertex) == 24, "ADX1 vertex layout");

static const void *take(Bytes *bytes, size_t count) {
    if (count > bytes->size) return NULL;
    const void *data = bytes->data;
    bytes->data += count;
    bytes->size -= count;
    return data;
}

static int extract(Bytes *bytes, void *value, size_t count) {
    const void *data = take(bytes, count);
    if (!data) return 0;
    memcpy(value, data, count);
    return 1;
}

static int reply(const Header *request, HRESULT status, const void *data, uint32_t bytes) {
    const Header response = { MAGIC, request->op, request->id, sizeof(status) + bytes };
    return fwrite(&response, sizeof(response), 1, stdout) == 1 &&
           fwrite(&status, sizeof(status), 1, stdout) == 1 &&
           (!bytes || fwrite(data, 1, bytes, stdout) == bytes) && fflush(stdout) == 0;
}

static Program *find_program(Program *programs, uint32_t id) {
    for (unsigned i = 0; i < MAX_PROGRAMS; ++i) {
        if (programs[i].id == id) return &programs[i];
    }
    return NULL;
}

static int compile(ArtD3D11Pass *pass, Program *programs, const Header *request, Bytes source) {
    Program *slot = find_program(programs, 0);
    if (!slot || find_program(programs, request->id)) return reply(request, E_INVALIDARG, NULL, 0);
    ID3DBlob *diagnostics = NULL;
    HRESULT status = art_d3d11_pass_compile(pass, source.data, source.size, &slot->value, &diagnostics);
    int written;
    if (SUCCEEDED(status)) {
        slot->id = request->id;
        written = reply(request, status, NULL, 0);
    } else if (diagnostics) {
        size_t length = ID3D10Blob_GetBufferSize(diagnostics);
        written = reply(request, status, ID3D10Blob_GetBufferPointer(diagnostics),
                        (uint32_t)(length < 65536 ? length : 65536));
    } else {
        written = reply(request, status, NULL, 0);
    }
    if (diagnostics) ID3D10Blob_Release(diagnostics);
    return written;
}

static int release(Program *programs, const Header *request, Bytes body) {
    uint32_t id;
    if (!extract(&body, &id, sizeof(id)) || !id || body.size) return reply(request, E_INVALIDARG, NULL, 0);
    Program *program = find_program(programs, id);
    if (!program) return reply(request, E_INVALIDARG, NULL, 0);
    art_d3d11_program_destroy(program->value);
    *program = (Program){0};
    return reply(request, S_OK, NULL, 0);
}

static int texture_binding(Program *programs, const Header *request, Bytes body) {
    uint32_t id, binding[2];
    char name[MAX_NAME + 1];
    if (!extract(&body, &id, sizeof(id)) || !id || !body.size || body.size > MAX_NAME ||
        memchr(body.data, 0, body.size)) return reply(request, E_INVALIDARG, NULL, 0);
    Program *program = find_program(programs, id);
    if (!program) return reply(request, E_INVALIDARG, NULL, 0);
    memcpy(name, body.data, body.size);
    name[body.size] = 0;
    HRESULT status = art_d3d11_texture_binding(program->value, name, &binding[0], &binding[1]);
    return FAILED(status) ? reply(request, status, NULL, 0) : reply(request, S_OK, binding, sizeof(binding));
}

static int draw(ArtD3D11Pass *pass, Program *programs, const Header *request, Bytes body) {
    ArtD3D11Draw draw = {0};
    uint32_t program_id, flags, count;
    uint32_t *pixels = NULL;
    void *constants = NULL;
    HRESULT status = E_INVALIDARG;
    uint64_t result_bytes = 0;
    int written;
    if (!extract(&body, &program_id, sizeof(program_id)) || !program_id ||
        !extract(&body, &draw.width, sizeof(draw.width)) ||
        !extract(&body, &draw.height, sizeof(draw.height)) ||
        !extract(&body, &flags, sizeof(flags)) || flags & ~3u ||
        !extract(&body, draw.clear_rgba, sizeof(draw.clear_rgba)) ||
        !extract(&body, draw.quad, sizeof(draw.quad)) ||
        !extract(&body, &count, sizeof(count)) || count > 256) goto done;

    Program *program = find_program(programs, program_id);
    if (!program || !draw.width || !draw.height || draw.width > 16384 || draw.height > 16384) goto done;
    result_bytes = (uint64_t)draw.width * draw.height * 4;
    if (result_bytes > MAX_BYTES) goto done;
    draw.alpha_blend = (flags & 1) != 0;
    draw.point_sampler = (flags & 2) != 0;
    draw.constant_bytes = art_d3d11_constant_bytes(program->value);
    constants = calloc(1, draw.constant_bytes ? draw.constant_bytes : 1);
    if (!constants) { status = E_OUTOFMEMORY; goto done; }
    draw.constants = constants;
    for (uint32_t i = 0; i < count; ++i) {
        char name[MAX_NAME + 1];
        float values[4];
        uint32_t length, elements;
        if (!extract(&body, &length, sizeof(length)) || !length || length > MAX_NAME ||
            !extract(&body, name, length) || memchr(name, 0, length) ||
            !extract(&body, &elements, sizeof(elements)) || !elements || elements > 4 ||
            !extract(&body, values, elements * sizeof(float))) goto done;
        name[length] = 0;
        HRESULT bound = art_d3d11_set_uniform(program->value, name, values, elements, constants, draw.constant_bytes);
        if (FAILED(bound)) { status = bound; goto done; }
    }
    for (unsigned i = 0; i < 4; ++i) {
        ArtD3D11Image *texture = &draw.textures[i];
        if (!extract(&body, &texture->width, sizeof(texture->width)) ||
            !extract(&body, &texture->height, sizeof(texture->height))) goto done;
        if (!texture->width && !texture->height) continue;
        if (!texture->width || !texture->height || texture->width > 16384 || texture->height > 16384) goto done;
        texture->row_bytes = texture->width * 4;
        texture->bytes = (size_t)texture->row_bytes * texture->height;
        texture->bgra = take(&body, texture->bytes);
        if (!texture->bgra) goto done;
    }
    if (body.size) goto done;
    pixels = malloc((size_t)result_bytes);
    if (!pixels) { status = E_OUTOFMEMORY; goto done; }
    status = art_d3d11_pass_draw(pass, program->value, &draw, pixels, (size_t)result_bytes);
done:
    written = reply(request, status, pixels, SUCCEEDED(status) ? (uint32_t)result_bytes : 0);
    free(pixels);
    free(constants);
    return written;
}

int main(void) {
    if (_setmode(_fileno(stdin), _O_BINARY) == -1 || _setmode(_fileno(stdout), _O_BINARY) == -1) return 1;
    ArtD3D11Pass *pass = NULL;
    Program programs[MAX_PROGRAMS] = {0};
    HRESULT status = art_d3d11_pass_create(&pass);
    if (!reply(&(Header){MAGIC, 0, 0, 0}, status, NULL, 0) || FAILED(status)) {
        art_d3d11_pass_destroy(pass);
        return 1;
    }
    int result = 0;
    Header request;
    size_t read_bytes;
    while ((read_bytes = fread(&request, 1, sizeof(request), stdin)) != 0) {
        if (read_bytes != sizeof(request) || request.magic != MAGIC || !request.id || request.bytes > MAX_BYTES) {
            result = 1;
            break;
        }
        void *payload = malloc(request.bytes ? request.bytes : 1);
        if (!payload || fread(payload, 1, request.bytes, stdin) != request.bytes) {
            free(payload);
            result = 1;
            break;
        }
        const Bytes body = {payload, request.bytes};
        int sent;
        switch (request.op) {
            case COMPILE: sent = compile(pass, programs, &request, body); break;
            case DRAW: sent = draw(pass, programs, &request, body); break;
            case RELEASE: sent = release(programs, &request, body); break;
            case TEXTURE_BINDING: sent = texture_binding(programs, &request, body); break;
            default: sent = reply(&request, E_NOTIMPL, NULL, 0); break;
        }
        free(payload);
        if (!sent) { result = 1; break; }
    }
    if (ferror(stdin)) result = 1;
    for (unsigned i = 0; i < MAX_PROGRAMS; ++i) art_d3d11_program_destroy(programs[i].value);
    art_d3d11_pass_destroy(pass);
    return result;
}
