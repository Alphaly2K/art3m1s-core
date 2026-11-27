#include "host_project.h"

#include <algorithm>
#include <exception>
#include <filesystem>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

#include "TVPMsg.h"
#include "TVPArchive.h"
#include "TVPStorage.h"
#include "tjs.h"

namespace
{
constexpr std::string_view kDomain = "project/";
constexpr std::string_view kSaveDirectory = "savedata";

class ProjectStream final : public tTJSBinaryStream
{
public:
    ProjectStream(Art3m1sKrkrProjectHostV1 host, std::string key, uint64_t size)
        : host_(host), key_(std::move(key)), size_(size) {}

    tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override
    {
        uint64_t base;
        switch (whence)
        {
            case TJS_BS_SEEK_SET: base = 0; break;
            case TJS_BS_SEEK_CUR: base = position_; break;
            case TJS_BS_SEEK_END: base = size_; break;
            default: return position_;
        }
        if (offset < 0)
        {
            const uint64_t magnitude = static_cast<uint64_t>(-(offset + 1)) + 1;
            if (magnitude <= base)
                position_ = base - magnitude;
        }
        else if (static_cast<uint64_t>(offset) <= (std::numeric_limits<uint64_t>::max)() - base)
            position_ = base + static_cast<uint64_t>(offset);
        return position_;
    }

    tjs_uint Read(void* buffer, tjs_uint read_size) override
    {
        if (position_ >= size_)
            return 0;
        const auto remaining = static_cast<size_t>(std::min<uint64_t>(size_ - position_, read_size));
        size_t total = 0;
        auto* output = static_cast<uint8_t*>(buffer);
        while (total < remaining)
        {
            size_t count = 0;
            if (host_.read_range(host_.user_data, key_.c_str(), position_, output + total,
                                 remaining - total, &count) != ART3M1S_KRKR_STATUS_OK ||
                count == 0 || count > remaining - total)
                TVPThrowExceptionMessage(TVPCannotOpenStorage, ttstr(key_.c_str()));
            position_ += count;
            total += count;
        }
        return static_cast<tjs_uint>(total);
    }

    tjs_uint Write(const void*, tjs_uint) override { return 0; }
    bool Flush() override { return true; }
    tjs_uint64 GetSize() override { return size_; }

private:
    Art3m1sKrkrProjectHostV1 host_;
    std::string key_;
    uint64_t size_;
    uint64_t position_ = 0;
};

class ProjectMedia final : public tTVPStorageMedia
{
public:
    ProjectMedia(Art3m1sKrkrProjectHostV1 host, ttstr save_root)
        : host_(host), save_root_(std::move(save_root)) {}

    void GetName(ttstr& name) override { name = TJS_N("arthost"); }
    void GetLocallyAccessibleName(ttstr& name) override
    {
        auto key = name.AsStdString();
        if (!key.empty() && key.back() == '/')
            key.pop_back();
        auto local = SavePath(Key(ttstr(key.c_str())));
        name = local.IsEmpty() ? ttstr() : TVPGetLocallyAccessibleName(local);
    }
    void GetListAt(const ttstr& name, iTVPStorageLister* lister) override
    {
        auto storage = name.AsStdString();
        if (storage.compare(0, kDomain.size(), kDomain) != 0)
            return;
        auto directory = storage.substr(kDomain.size());
        if (!directory.empty() && directory.back() == '/')
            directory.pop_back();
        if (!directory.empty() && !Art3m1sKrkrIsProjectKey(directory.c_str()))
            return;

        if (const auto saved = SavePath(directory); !saved.IsEmpty())
        {
            const auto local = std::filesystem::u8path(TVPGetLocallyAccessibleName(saved).AsStdString());
            std::error_code error;
            for (auto it = std::filesystem::directory_iterator(local, error);
                 !error && it != std::filesystem::directory_iterator(); it.increment(error))
            {
                if (it->is_regular_file(error))
                    lister->Add(ttstr(it->path().filename().u8string().c_str()));
            }
            return;
        }
        if (!host_.list_files)
            return;

        struct Listing
        {
            iTVPStorageLister* lister;
            std::exception_ptr error;
        } context{lister, nullptr};
        auto emit = [](void* pointer, const char* filename) {
            auto& state = *static_cast<Listing*>(pointer);
            if (state.error)
                return;
            try
            {
                if (!Art3m1sKrkrIsProjectKey(filename) || std::string_view(filename).find('/') != std::string_view::npos)
                    throw std::invalid_argument("invalid KRKR project listing entry");
                state.lister->Add(ttstr(filename));
            }
            catch (...)
            {
                state.error = std::current_exception();
            }
        };
        const auto status = host_.list_files(host_.user_data, directory.c_str(), &context, emit);
        if (context.error)
            std::rethrow_exception(context.error);
        if (status != ART3M1S_KRKR_STATUS_OK && status != ART3M1S_KRKR_STATUS_NO_COMMAND)
            TVPThrowExceptionMessage(TVPCannotOpenStorage, name);
    }

    bool CheckExistentStorage(const ttstr& name) override
    {
        uint64_t size = 0;
        const auto key = Key(name);
        if (const auto saved = SavePath(key); !saved.IsEmpty())
            return TVPIsExistentStorageNoSearch(saved);
        return !key.empty() && Query(key, name, size);
    }

    tTJSBinaryStream* Open(const ttstr& name, tjs_uint32 flags) override
    {
        uint64_t size = 0;
        auto key = Key(name);
        if (const auto saved = SavePath(key); !saved.IsEmpty())
            return TVPCreateStream(saved, flags);
        if (flags != TJS_BS_READ || key.empty() || !Query(key, name, size))
            TVPThrowExceptionMessage(TVPCannotOpenStorage, name);
        return new ProjectStream(host_, std::move(key), size);
    }

private:
    static std::string Key(const ttstr& name)
    {
        auto storage = name.AsStdString();
        if (storage.compare(0, kDomain.size(), kDomain) != 0)
            return {};
        auto key = storage.substr(kDomain.size());
        return Art3m1sKrkrIsProjectKey(key.c_str()) ? key : std::string{};
    }

    ttstr SavePath(std::string_view key) const
    {
        // KAG scripts can address savedata relative to the project storage.
        if (save_root_.IsEmpty() || key.substr(0, kSaveDirectory.size()) != kSaveDirectory)
            return {};
        key.remove_prefix(kSaveDirectory.size());
        if (key.empty())
            return save_root_;
        if (key.front() != '/')
            return {};
        return save_root_ + ttstr(std::string(key.substr(1)).c_str());
    }

    bool Query(const std::string& key, const ttstr& name, uint64_t& size) const
    {
        const auto status = host_.query_size(host_.user_data, key.c_str(), &size);
        if (status == ART3M1S_KRKR_STATUS_NO_COMMAND)
            return false;
        if (status != ART3M1S_KRKR_STATUS_OK)
            TVPThrowExceptionMessage(TVPCannotOpenStorage, name);
        return true;
    }

    Art3m1sKrkrProjectHostV1 host_;
    ttstr save_root_;
};

iTVPStorageMedia* g_project_media = nullptr;
}

bool Art3m1sKrkrIsProjectKey(const char* value)
{
    if (!value || !*value)
        return false;
    const std::string_view key(value);
    if (key.find_first_of("\\:>") != std::string_view::npos)
        return false;
    size_t start = 0;
    while (start <= key.size())
    {
        auto slash = key.find('/', start);
        auto part = key.substr(start, slash == key.npos ? key.npos : slash - start);
        if (part.empty() || part == "." || part == "..")
            return false;
        if (slash == key.npos)
            break;
        start = slash + 1;
    }
    return true;
}

void Art3m1sKrkrMountProject(const Art3m1sKrkrProjectHostV1& host, const char* archive_key)
{
    auto media = std::make_unique<ProjectMedia>(host, TVPDataPath);
    TVPRegisterStorageMedia(media.get());
    g_project_media = media.release();
    TVPNativeProjectDir.Clear();
    TVPNativeProjectData.Clear();
    TVPProjectDir = TJS_N("arthost://project/");
    TVPProjectData = TVPProjectDir + ttstr(archive_key ? archive_key : "");
}

void Art3m1sKrkrUnmountProject()
{
    if (!g_project_media)
        return;
    TVPClearArchiveCache();
    TVPFreeArchiveHandlePool();
    TVPSetCurrentDirectory(TJS_N("file://./"));
    auto* media = g_project_media;
    g_project_media = nullptr;
    TVPUnregisterStorageMedia(media);
    media->Release();
}
