#include "host_audio.h"

#include <algorithm>
#include <cstring>
#include <deque>
#include <limits>
#include <memory>
#include <mutex>
#include <unordered_map>
#include <utility>
#include <vector>

#include "tjsCommHead.h"
#include "PlatformAudio.h"

#if defined(__APPLE__)
#include <SDL3/SDL_audio.h>
#include <SDL3/SDL_init.h>
#include <SDL3/SDL_log.h>
#endif

namespace art3m1s::krkr
{
namespace
{
struct PendingAudioCommand
{
    uint32_t kind = 0;
    uint32_t stream_id = 0;
    uint32_t sample_format = 0;
    uint32_t sample_rate = 0;
    uint32_t channels = 0;
    uint64_t sample_count = 0;
    float volume = 1.0f;
    float pan = 0.0f;
    std::vector<uint8_t> payload;
};

struct AudioStreamState
{
    AudioStreamState(uint32_t format, uint32_t rate, uint32_t channel_count,
                     uint32_t sample_size, uint32_t buffer_limit)
      : sample_format(format),
        sample_rate(rate),
        channels(channel_count),
        bytes_per_sample(sample_size),
        buffer_limit(std::max(1u, buffer_limit))
    {
    }

    uint32_t sample_format = 0;
    uint32_t sample_rate = 0;
    uint32_t channels = 0;
    uint32_t bytes_per_sample = 0;
    uint32_t buffer_limit = 1;
    uint64_t appended_frames = 0;
    uint64_t consumed_frames = 0;
    std::deque<std::pair<uint64_t, uint64_t>> buffered_ranges;

    uint64_t queued_frames() const
    {
        return appended_frames > consumed_frames ? appended_frames - consumed_frames : 0;
    }

    void PruneConsumed()
    {
        while (!buffered_ranges.empty() && buffered_ranges.front().second <= consumed_frames)
            buffered_ranges.pop_front();
    }

    void ResetCounters()
    {
        appended_frames = 0;
        consumed_frames = 0;
        buffered_ranges.clear();
    }
};

uint32_t ToProtocolSampleFormat(const tTVPWaveFormat& format)
{
    if (format.IsFloat)
        return format.BitsPerSample == 32 ? ART3M1S_KRKR_AUDIO_FORMAT_F32 : 0;

    switch (format.BitsPerSample)
    {
        case 8:
            return ART3M1S_KRKR_AUDIO_FORMAT_I8;
        case 16:
            return ART3M1S_KRKR_AUDIO_FORMAT_I16;
        case 24:
            return ART3M1S_KRKR_AUDIO_FORMAT_I24;
        case 32:
            return ART3M1S_KRKR_AUDIO_FORMAT_I32;
        default:
            return 0;
    }
}

class AudioHost
{
public:
    uint32_t CreateStream(const tTVPWaveFormat& format, int buffer_count)
    {
        const uint32_t sample_format = ToProtocolSampleFormat(format);
        if (!sample_format || !format.SamplesPerSec || !format.Channels ||
            !format.BytesPerSample)
            return 0;

        std::lock_guard<std::mutex> lock(mutex_);
        const uint32_t stream_id = next_stream_id_++;
        if (!stream_id)
            return 0;
        streams_.emplace(
            stream_id,
            AudioStreamState(sample_format, format.SamplesPerSec, format.Channels,
                             format.BytesPerSample,
                             static_cast<uint32_t>(std::max(buffer_count, 0))));

        PendingAudioCommand command;
        command.kind = ART3M1S_KRKR_AUDIO_CREATE_STREAM;
        command.stream_id = stream_id;
        command.sample_format = sample_format;
        command.sample_rate = format.SamplesPerSec;
        command.channels = format.Channels;
        EnqueueLocked(std::move(command));
        return stream_id;
    }

    void AppendPcm(uint32_t stream_id, const void* data, uint32_t size)
    {
        if (!data || !size)
            return;

        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        if (stream == streams_.end())
            return;

        const uint64_t frame_size =
            static_cast<uint64_t>(stream->second.channels) * stream->second.bytes_per_sample;
        const uint64_t frames = static_cast<uint64_t>(size) / frame_size;
        if (!frames)
            return;

        const uint64_t start = stream->second.appended_frames;
        stream->second.appended_frames += frames;
        stream->second.buffered_ranges.emplace_back(start, stream->second.appended_frames);

        PendingAudioCommand command;
        command.kind = ART3M1S_KRKR_AUDIO_SUBMIT_PCM;
        command.stream_id = stream_id;
        command.sample_format = stream->second.sample_format;
        command.sample_rate = stream->second.sample_rate;
        command.channels = stream->second.channels;
        command.sample_count = frames;
        command.payload.assign(static_cast<const uint8_t*>(data),
                               static_cast<const uint8_t*>(data) + size);
        EnqueueLocked(std::move(command));
    }

    void QueueControl(uint32_t kind, uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (streams_.find(stream_id) == streams_.end())
            return;
        PendingAudioCommand command;
        command.kind = kind;
        command.stream_id = stream_id;
        EnqueueLocked(std::move(command));
    }

    void QueueParams(uint32_t stream_id, float volume, float pan)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        if (streams_.find(stream_id) == streams_.end())
            return;
        PendingAudioCommand command;
        command.kind = ART3M1S_KRKR_AUDIO_SET_PARAMS;
        command.stream_id = stream_id;
        command.volume = volume;
        command.pan = pan;
        EnqueueLocked(std::move(command));
    }

    void ResetStream(uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        if (stream == streams_.end())
            return;
        stream->second.ResetCounters();
        PendingAudioCommand command;
        command.kind = ART3M1S_KRKR_AUDIO_STOP;
        command.stream_id = stream_id;
        EnqueueLocked(std::move(command));
    }

    void DestroyStream(uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        if (stream == streams_.end())
            return;

        PendingAudioCommand command;
        command.kind = ART3M1S_KRKR_AUDIO_DESTROY_STREAM;
        command.stream_id = stream_id;
        EnqueueLocked(std::move(command));
        streams_.erase(stream);
    }

    bool IsBufferValid(uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        if (stream == streams_.end())
            return false;
        stream->second.PruneConsumed();
        return stream->second.buffered_ranges.size() < stream->second.buffer_limit;
    }

    int GetRemainBuffers(uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        if (stream == streams_.end())
            return 0;
        stream->second.PruneConsumed();
        return static_cast<int>(stream->second.buffered_ranges.size());
    }

    uint64_t GetCurrentPlaySamples(uint32_t stream_id)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(stream_id);
        return stream == streams_.end() ? 0 : stream->second.queued_frames();
    }

    bool Poll(Art3m1sKrkrAudioCommandV1* out_command)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        borrowed_.reset();
        if (commands_.empty())
            return false;

        borrowed_ = std::make_unique<PendingAudioCommand>(std::move(commands_.front()));
        commands_.pop_front();

        const PendingAudioCommand& command = *borrowed_;
        std::memset(out_command, 0, sizeof(*out_command));
        out_command->struct_size = sizeof(Art3m1sKrkrAudioCommandV1);
        out_command->kind = command.kind;
        out_command->stream_id = command.stream_id;
        out_command->sample_format = command.sample_format;
        out_command->sample_rate = command.sample_rate;
        out_command->channels = command.channels;
        out_command->sample_count = command.sample_count;
        out_command->volume = command.volume;
        out_command->pan = command.pan;
        out_command->payload = command.payload.empty() ? nullptr : command.payload.data();
        out_command->payload_size = command.payload.size();
        return true;
    }

    int32_t SubmitConsumed(const Art3m1sKrkrAudioConsumedV1* consumed)
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto stream = streams_.find(consumed->stream_id);
        if (stream == streams_.end())
            return ART3M1S_KRKR_STATUS_INVALID_HANDLE;

        const uint64_t sample_count =
            std::min(consumed->consumed_samples, stream->second.appended_frames);
        stream->second.consumed_frames =
            std::max(stream->second.consumed_frames, sample_count);
        stream->second.PruneConsumed();
        return ART3M1S_KRKR_STATUS_OK;
    }

    void Reset()
    {
        std::lock_guard<std::mutex> lock(mutex_);
        commands_.clear();
        borrowed_.reset();
        streams_.clear();
    }

private:
    void EnqueueLocked(PendingAudioCommand command)
    {
        commands_.emplace_back(std::move(command));
    }

    std::mutex mutex_;
    std::deque<PendingAudioCommand> commands_;
    std::unique_ptr<PendingAudioCommand> borrowed_;
    std::unordered_map<uint32_t, AudioStreamState> streams_;
    uint32_t next_stream_id_ = 1;
};

AudioHost& Host()
{
    static AudioHost* host = new AudioHost;
    return *host;
}

class Art3m1sSoundBuffer final : public iTVPSoundBuffer
{
public:
    Art3m1sSoundBuffer(const tTVPWaveFormat& format, int buffer_count)
      : format_(format)
    {
        stream_id_ = Host().CreateStream(format, buffer_count);
    }

    ~Art3m1sSoundBuffer() override
    {
        if (stream_id_)
            Host().DestroyStream(stream_id_);
    }

    bool Init() override
    {
        if (!stream_id_)
        {
            delete this;
            return false;
        }
        return true;
    }

    void Release() override { delete this; }

    void Play() override
    {
        if (playing_)
            return;
        playing_ = true;
        Host().QueueControl(ART3M1S_KRKR_AUDIO_PLAY, stream_id_);
    }

    void Pause() override
    {
        if (!playing_)
            return;
        playing_ = false;
        Host().QueueControl(ART3M1S_KRKR_AUDIO_PAUSE, stream_id_);
    }

    void Stop() override
    {
        playing_ = false;
        Host().ResetStream(stream_id_);
    }

    void Reset() override
    {
        playing_ = false;
        Host().ResetStream(stream_id_);
    }

    bool IsPlaying() override { return playing_; }

    void SetVolume(float volume) override
    {
        volume_ = volume;
        Host().QueueParams(stream_id_, volume_, pan_);
    }

    float GetVolume() override { return volume_; }

    void SetPan(float pan) override
    {
        pan_ = pan;
        Host().QueueParams(stream_id_, volume_, pan_);
    }

    float GetPan() override { return pan_; }

    void AppendBuffer(const void* buffer, unsigned int length) override
    {
        Host().AppendPcm(stream_id_, buffer, length);
    }

    bool IsBufferValid() override { return Host().IsBufferValid(stream_id_); }

    bool IsValidFormat(tTVPWaveFormat& format) override
    {
        return format.SamplesPerSec == format_.SamplesPerSec &&
               format.Channels == format_.Channels &&
               format.BitsPerSample == format_.BitsPerSample &&
               format.IsFloat == format_.IsFloat;
    }

    tjs_uint GetCurrentPlaySamples() override
    {
        return static_cast<tjs_uint>(Host().GetCurrentPlaySamples(stream_id_));
    }

    tjs_uint GetLatencySamples() override { return 0; }
    float GetLatencySeconds() override { return 0.0f; }

    int GetRemainBuffers() override { return Host().GetRemainBuffers(stream_id_); }

    void SetPosition(float, float, float) override {}

private:
    tTVPWaveFormat format_{};
    uint32_t stream_id_ = 0;
    bool playing_ = false;
    float volume_ = 1.0f;
    float pan_ = 0.0f;
};

#if defined(__APPLE__)
// Apple hosts do not have a streaming PCM sink in Flutter or SwiftUI. Keep
// playback in the embedded SDL runtime so buffer consumption follows the
// actual audio device instead of a wall-clock estimate in either UI host.
class Art3m1sDarwinSoundBuffer final : public iTVPSoundBuffer
{
public:
    Art3m1sDarwinSoundBuffer(const tTVPWaveFormat& format, int buffer_count)
      : format_(format), buffer_limit_(std::max(buffer_count, 1))
    {
    }

    bool Init() override
    {
        if (!format_.SamplesPerSec || !format_.Channels || !format_.BytesPerSample)
            return false;
        SDL_AudioSpec spec{};
        spec.freq = static_cast<int>(format_.SamplesPerSec);
        spec.channels = static_cast<int>(format_.Channels);
        if (format_.IsFloat && format_.BitsPerSample == 32)
            spec.format = SDL_AUDIO_F32;
        else if (!format_.IsFloat)
        {
            switch (format_.BitsPerSample)
            {
                case 8: spec.format = SDL_AUDIO_S8; break;
                case 16: spec.format = SDL_AUDIO_S16; break;
                case 24: spec.format = SDL_AUDIO_S32; break;
                case 32: spec.format = SDL_AUDIO_S32; break;
                default: break;
            }
        }
        if (spec.format == SDL_AUDIO_UNKNOWN)
            return false;
        input_frame_size_ = format_.Channels * format_.BytesPerSample;
        queued_frame_size_ = format_.Channels * SDL_AUDIO_BYTESIZE(spec.format);
        if (!input_frame_size_ || !queued_frame_size_ ||
            !SDL_InitSubSystem(SDL_INIT_AUDIO))
        {
            SDL_LogError(SDL_LOG_CATEGORY_AUDIO, "KRKR audio init failed: %s", SDL_GetError());
            return false;
        }
        audio_initialized_ = true;
        device_ = SDL_OpenAudioDevice(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, nullptr);
        stream_ = SDL_CreateAudioStream(&spec, nullptr);
        if (!device_ || !stream_ || !SDL_BindAudioStream(device_, stream_) ||
            !SDL_PauseAudioStreamDevice(stream_))
        {
            SDL_LogError(SDL_LOG_CATEGORY_AUDIO, "KRKR audio stream failed: %s", SDL_GetError());
            return false;
        }
        SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "KRKR audio device ready: %s (%d Hz, %d ch)",
                    SDL_GetAudioDeviceName(device_), spec.freq, spec.channels);
        return true;
    }

    ~Art3m1sDarwinSoundBuffer() override
    {
        if (stream_)
        {
            SDL_UnbindAudioStream(stream_);
            SDL_DestroyAudioStream(stream_);
        }
        if (device_)
            SDL_CloseAudioDevice(device_);
        if (audio_initialized_)
            SDL_QuitSubSystem(SDL_INIT_AUDIO);
    }

    void Release() override { delete this; }
    void Play() override
    {
        if (stream_ && SDL_ResumeAudioStreamDevice(stream_))
        {
            if (!playing_)
                SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "KRKR audio playback started");
            playing_ = true;
        }
    }
    void Pause() override
    {
        if (stream_ && SDL_PauseAudioStreamDevice(stream_))
            playing_ = false;
    }
    void Stop() override
    {
        Pause();
        Reset();
    }
    void Reset() override
    {
        if (stream_)
            SDL_ClearAudioStream(stream_);
        appended_frames_ = 0;
        buffer_ends_.clear();
    }
    bool IsPlaying() override { return playing_; }
    void SetVolume(float volume) override
    {
        volume_ = std::max(0.0f, volume);
        if (stream_)
            SDL_SetAudioStreamGain(stream_, volume_);
    }
    float GetVolume() override { return volume_; }
    void SetPan(float pan) override { pan_ = pan; }
    float GetPan() override { return pan_; }

    void AppendBuffer(const void* data, unsigned int length) override
    {
        if (!stream_ || !data || !input_frame_size_)
            return;
        const uint64_t frames = length / input_frame_size_;
        if (!frames || frames >
                           static_cast<uint64_t>(std::numeric_limits<int>::max()) /
                               queued_frame_size_)
            return;
        const void* samples = data;
        int bytes = static_cast<int>(frames * input_frame_size_);
        std::vector<int32_t> converted;
        if (format_.BitsPerSample == 24)
        {
            const auto* source = static_cast<const uint8_t*>(data);
            converted.reserve(frames * format_.Channels);
            for (uint64_t index = 0; index < frames * format_.Channels; ++index)
            {
                const uint32_t value = static_cast<uint32_t>(source[index * 3]) |
                                       (static_cast<uint32_t>(source[index * 3 + 1]) << 8) |
                                       (static_cast<uint32_t>(source[index * 3 + 2]) << 16);
                converted.push_back(static_cast<int32_t>(value << 8));
            }
            samples = converted.data();
            bytes = static_cast<int>(converted.size() * sizeof(int32_t));
        }
        if (!SDL_PutAudioStreamData(stream_, samples, bytes))
        {
            SDL_LogError(SDL_LOG_CATEGORY_AUDIO, "KRKR audio enqueue failed: %s", SDL_GetError());
            return;
        }
        appended_frames_ += frames;
        buffer_ends_.push_back(appended_frames_);
        if (!reported_pcm_)
        {
            reported_pcm_ = true;
            SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "KRKR PCM queued: %llu frames",
                        static_cast<unsigned long long>(frames));
        }
    }

    bool IsBufferValid() override
    {
        PrunePlayedBuffers();
        return buffer_ends_.size() < static_cast<size_t>(buffer_limit_);
    }
    bool IsValidFormat(tTVPWaveFormat& format) override
    {
        return format.SamplesPerSec == format_.SamplesPerSec &&
               format.Channels == format_.Channels &&
               format.BitsPerSample == format_.BitsPerSample &&
               format.IsFloat == format_.IsFloat;
    }
    tjs_uint GetCurrentPlaySamples() override
    {
        const int queued = stream_ ? SDL_GetAudioStreamQueued(stream_) : 0;
        return queued > 0 && queued_frame_size_
                   ? static_cast<tjs_uint>(queued / queued_frame_size_)
                   : 0;
    }
    tjs_uint GetLatencySamples() override { return 0; }
    float GetLatencySeconds() override { return 0.0f; }
    int GetRemainBuffers() override
    {
        PrunePlayedBuffers();
        return static_cast<int>(buffer_ends_.size());
    }
    void SetPosition(float, float, float) override {}

private:
    void PrunePlayedBuffers()
    {
        const uint64_t queued = GetCurrentPlaySamples();
        const uint64_t consumed = appended_frames_ > queued ? appended_frames_ - queued : 0;
        if (consumed && !reported_consumption_)
        {
            reported_consumption_ = true;
            SDL_LogInfo(SDL_LOG_CATEGORY_APPLICATION, "KRKR PCM is being consumed by the audio device");
        }
        while (!buffer_ends_.empty() && buffer_ends_.front() <= consumed)
            buffer_ends_.pop_front();
    }

    tTVPWaveFormat format_{};
    int buffer_limit_ = 1;
    uint32_t input_frame_size_ = 0;
    uint32_t queued_frame_size_ = 0;
    SDL_AudioDeviceID device_ = 0;
    SDL_AudioStream* stream_ = nullptr;
    bool audio_initialized_ = false;
    bool playing_ = false;
    float volume_ = 1.0f;
    float pan_ = 0.0f;
    uint64_t appended_frames_ = 0;
    std::deque<uint64_t> buffer_ends_;
    bool reported_pcm_ = false;
    bool reported_consumption_ = false;
};
#endif
} // namespace

void ResetAudioHost()
{
    Host().Reset();
}

bool PollAudioCommand(Art3m1sKrkrAudioCommandV1* out_command)
{
    return out_command && Host().Poll(out_command);
}

int32_t SubmitAudioConsumed(const Art3m1sKrkrAudioConsumedV1* consumed)
{
    return consumed ? Host().SubmitConsumed(consumed)
                    : ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
}
} // namespace art3m1s::krkr

void TVPInitDirectSound(int)
{
}

void TVPUninitDirectSound()
{
}

iTVPSoundBuffer* TVPCreateSoundBuffer(tTVPWaveFormat& format, int buffer_count)
{
#if defined(__APPLE__)
    auto* sound_buffer = new art3m1s::krkr::Art3m1sDarwinSoundBuffer(format, buffer_count);
    if (sound_buffer->Init())
        return sound_buffer;
    delete sound_buffer;
#else
    auto* sound_buffer = new art3m1s::krkr::Art3m1sSoundBuffer(format, buffer_count);
    if (sound_buffer->Init())
        return sound_buffer;
#endif
    return nullptr;
}
