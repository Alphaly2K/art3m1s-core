#include "art3m1s_rfvp_v2.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct File {
    const char *key;
    uint8_t *data;
    uint64_t len;
    unsigned opens, reads, writes, removes, references;
};

enum { NUM_SAVES = 5 };

struct Host {
    struct File assets[3];
    struct File saves[NUM_SAVES];
    unsigned save_lists;
};

static Art3m1sRfvpBytesV2 name(const char *str) {
    return (Art3m1sRfvpBytesV2){ (const uint8_t *)str, strlen(str) };
}

static int equal(Art3m1sRfvpBytesV2 key, const char *name) {
    size_t len = strlen(name);
    return key.len == len && (len == 0 || memcmp(key.data, name, len) == 0);
}

static struct File *find(struct Host *host, uint32_t domain, Art3m1sRfvpBytesV2 key) {
    struct File *files = domain == ART3M1S_RFVP_V2_ASSETS ? host->assets : host->saves;
    unsigned count = domain == ART3M1S_RFVP_V2_ASSETS ? 3 : NUM_SAVES;
    for (unsigned i = 0; i < count; i++) {
        if (equal(key, files[i].key)) return &files[i];
    }
    return NULL;
}

static int32_t open_file(uintptr_t ctx, uint32_t domain, Art3m1sRfvpBytesV2 key,
                         uint64_t *file, uint64_t *len) {
    struct File *entry = find((struct Host *)ctx, domain, key);
    if (!entry || !entry->data) return ART3M1S_RFVP_V2_NOT_FOUND;
    entry->opens++;
    entry->references++;
    *len = entry->len;
    *file = (uintptr_t)entry;
    return 0;
}

static int32_t read_at(uintptr_t ctx, uint64_t handle, uint64_t offset,
                       uint8_t *out, uint64_t capacity, uint64_t *read) {
    (void)ctx;
    struct File *file = (struct File *)(uintptr_t)handle;
    assert(file->references && file->data);
    uint64_t remain = offset < file->len ? file->len - offset : 0;
    *read = remain < capacity ? remain : capacity;
    if (*read) memcpy(out, file->data + offset, (size_t)*read);
    file->reads++;
    return 0;
}

static void close_file(uintptr_t ctx, uint64_t handle) {
    (void)ctx;
    struct File *file = (struct File *)(uintptr_t)handle;
    assert(file->references);
    file->references--;
}

static int32_t list(uintptr_t ctx, uint32_t domain, Art3m1sRfvpBytesV2 root,
                    Art3m1sRfvpBytesV2 ext, Art3m1sRfvpVisitFileV2 visit, void *visitor_ctx) {
    struct Host *host = (struct Host *)ctx;
    assert(equal(root, "."));
    if (domain == ART3M1S_RFVP_V2_ASSETS) {
        struct File *file = equal(ext, "hcb") ? &host->assets[0] : &host->assets[1];
        assert(equal(ext, "hcb") || equal(ext, "bin"));
        return visit(visitor_ctx, name(file->key), file->len);
    }
    assert(domain == ART3M1S_RFVP_V2_SAVES && !ext.len);
    host->save_lists++;
    for (unsigned i = 0; i < NUM_SAVES; i++) {
        const struct File *file = &host->saves[i];
        if (file->data && visit(visitor_ctx, name(file->key), file->len)) {
            return ART3M1S_RFVP_V2_IO;
        }
    }
    return 0;
}

static int32_t write_save(uintptr_t ctx, Art3m1sRfvpBytesV2 key, Art3m1sRfvpBytesV2 data) {
    struct File *file = find((struct Host *)ctx, ART3M1S_RFVP_V2_SAVES, key);
    assert(file && !file->references && data.len);
    free(file->data);
    file->data = malloc((size_t)data.len);
    assert(file->data);
    memcpy(file->data, data.data, (size_t)data.len);
    file->len = data.len;
    file->writes++;
    return 0;
}

static int32_t remove_save(uintptr_t ctx, Art3m1sRfvpBytesV2 key) {
    struct File *file = find((struct Host *)ctx, ART3M1S_RFVP_V2_SAVES, key);
    if (!file || !file->data) return ART3M1S_RFVP_V2_NOT_FOUND;
    assert(!file->references);
    free(file->data);
    file->data = NULL;
    file->removes++;
    return 0;
}

static void load_fixture(struct File *entry, const char *path) {
    FILE *file = fopen(path, "rb");
    assert(file && fseek(file, 0, SEEK_END) == 0);
    long len = ftell(file);
    assert(len > 0 && fseek(file, 0, SEEK_SET) == 0);
    entry->data = malloc((size_t)len);
    assert(entry->data && fread(entry->data, 1, (size_t)len, file) == (size_t)len);
    entry->len = (uint64_t)len;
    assert(fclose(file) == 0);
}

int main(int argc, char **argv) {
    assert(argc == 4);
    struct Host host = {
        .assets = {{.key = "tile.hcb"}, {.key = "graph.bin"}, {.key = "loose.hzc"}},
        .saves = {{.key = "rfvp_s007.bin"}, {.key = "rfvp_s008.bin"},
                  {.key = "s009.bin"}, {.key = "rfvp_global.bin"}, {.key = "s8.bin"}},
    };
    for (unsigned i = 0; i < 3; i++) load_fixture(&host.assets[i], argv[i + 1]);

    const Art3m1sRfvpApiV2 *api = NULL;
    assert(art3m1s_rfvp_get_api_v2(2, sizeof(*api), &api) == 0);
    const Art3m1sRfvpIoV2 io = {
        .struct_size = sizeof(io), .nls = ART3M1S_RFVP_V2_UTF8, .context = (uintptr_t)&host,
        .open = open_file, .read_at = read_at, .close = close_file, .list = list,
        .write_save = write_save, .remove_save = remove_save,
    };
    uint64_t session = 0;
    assert(api->open_with_io(&io, &session) == 0 && session);
    uint32_t flags;
    assert(api->step(session, 16, &flags) == 0);
    uint64_t frame = 0;
    Art3m1sRfvpFrameV2 view;
    assert(api->acquire_frame(session, &frame) == 0);
    assert(api->frame_view(frame, &view) == 0 && view.texture_count == 3);
    const uint8_t colors[2][4] = {{255, 0, 0, 255}, {0, 255, 0, 255}};
    for (unsigned i = 0; i < 2; i++) {
        Art3m1sRfvpTextureV2 texture;
        assert(api->frame_texture(frame, i + 1, &texture) == 0);
        assert(texture.width == 4 && texture.height == 4 && texture.pixels.len == 64);
        assert(memcmp(texture.pixels.data, colors[i], 4) == 0);
    }
    assert(api->release_frame(frame) == 0);
    assert(host.saves[0].data);
    const struct File *slot = &host.saves[0];
    assert(write_save(io.context, name("s8.bin"), (Art3m1sRfvpBytesV2){slot->data, slot->len}) == 0);
    for (unsigned i = 0; i < 7; i++) assert(api->step(session, 16, &flags) == 0);
    assert(host.assets[0].reads && host.assets[1].reads > 1 && host.assets[2].reads);
    assert(host.save_lists && host.saves[0].writes && host.saves[0].reads >= 2);
    assert(host.saves[1].writes && host.saves[1].removes && !host.saves[1].data);
    assert(host.saves[4].removes && !host.saves[4].data);
    assert(api->close(session) == 0);
    assert(host.saves[3].writes && host.saves[3].len > 4);
    assert(memcmp(host.saves[3].data + host.saves[3].len - 4, "RFVG", 4) == 0);
    for (unsigned i = 0; i < 3; i++) assert(!host.assets[i].references);

    host.saves[2].data = host.saves[0].data;
    host.saves[2].len = host.saves[0].len;
    host.saves[0].data = NULL;
    assert(api->open_with_io(&io, &session) == 0 && session);
    assert(api->step(session, 16, &flags) == 0);
    assert(api->close(session) == 0);
    assert(host.saves[2].reads && host.saves[3].reads);
    for (unsigned i = 0; i < 3; i++) {
        assert(!host.assets[i].references);
        free(host.assets[i].data);
    }
    for (unsigned i = 0; i < NUM_SAVES; i++) {
        assert(!host.saves[i].references);
        free(host.saves[i].data);
    }
    puts("RFVP v2 Host IO smoke passed");
    return 0;
}
