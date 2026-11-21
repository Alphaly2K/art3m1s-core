using System;
using System.Buffers.Binary;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Artemis.Host;
using static Artemis.Host.ArtemisNative;

internal static unsafe class Program
{
    private const int EventEnvelopeBytes = (int)ART_EVENT_ENVELOPE_BYTES;
    private const int EventHeaderBytes = EventEnvelopeBytes + (int)ART_EVENT_BODY_HEADER_BYTES;
    private static readonly byte[] Boot = Encoding.UTF8.GetBytes(
        "*top\n[lyc id=1 width=4 height=4 color=ff112233]\n[flip]\n[stop]\n");

    private static void Main(string[] args)
    {
        if (args.Length != 1) throw new ArgumentException("Pass the path to the standalone Core library.");
        if (sizeof(ArtApiHeader) != 24 || Marshal.OffsetOf<ArtMessage>(nameof(ArtMessage.session_epoch)) != (IntPtr)16)
            throw new InvalidOperationException("Unexpected C ABI layout.");
        string path = Path.GetFullPath(args[0]);
        NativeLibrary.SetDllImportResolver(typeof(ArtemisNative).Assembly,
            (_, _, _) => NativeLibrary.Load(path));
        ArtRootApiV2* root = null;
        Check(art3m1s_get_api_v2(ART_ABI_V2_MAJOR, (uint)sizeof(ArtRootApiV2), &root));
        if (root == null || root->header.magic != ART_API_MAGIC || root->header.abi_major != ART_ABI_V2_MAJOR)
            throw new InvalidOperationException("Unexpected root table.");
        void* table = null;
        Check(root->query_interface(ART_INTERFACE_RUNTIME, ART_ABI_V2_MAJOR, (uint)sizeof(ArtRuntimeApiV2), &table));
        var runtime = (ArtRuntimeApiV2*)table;
        Check(root->query_interface(ART_INTERFACE_FRAME, ART_ABI_V2_MAJOR, (uint)sizeof(ArtFrameApiV2), &table));
        var frames = (ArtFrameApiV2*)table;
        var create = new ArtCreateInfo
        {
            struct_size = (uint)sizeof(ArtCreateInfo),
            host_service_caps = ART_HOST_CAP_IO,
            renderer_caps = ART_RENDERER_CAP_SPRITE,
        };
        ulong handle = 0;
        Check(runtime->create(&create, &handle));
        try
        {
            Open(runtime, handle);
            Play(runtime, frames, handle);
        }
        finally
        {
            Check(runtime->destroy(handle));
        }
        Console.WriteLine("Artemis C# Host smoke passed");
    }

    private static void Open(ArtRuntimeApiV2* runtime, ulong handle)
    {
        byte[] ini = Encoding.UTF8.GetBytes("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n");
        byte[] platform = Encoding.UTF8.GetBytes("WINDOWS");
        fixed (byte* iniBytes = ini, platformBytes = platform)
        {
            var project = new ArtProjectInfo
            {
                struct_size = (uint)sizeof(ArtProjectInfo),
                ini_bytes = new ArtBytes { data = iniBytes, size = (ulong)ini.Length },
                platform_utf8 = new ArtBytes { data = platformBytes, size = (ulong)platform.Length },
            };
            Check(runtime->open_project(handle, &project));
        }
    }

    private static void Play(ArtRuntimeApiV2* runtime, ArtFrameApiV2* frames, ulong handle)
    {
        ulong frameId = 1;
        byte[] buffer = new byte[4096];
        for (int turn = 0; turn < 32; turn++)
        {
            var step = new ArtStepInfo
            {
                struct_size = (uint)sizeof(ArtStepInfo), host_frame_id = frameId,
                monotonic_ns = frameId * 1_000_000,
            };
            var result = new ArtStepResult { struct_size = (uint)sizeof(ArtStepResult) };
            Check(runtime->step(handle, &step, &result));
            var poll = new ArtPollResult { struct_size = (uint)sizeof(ArtPollResult) };
            fixed (byte* bytes = buffer)
            {
                int status = runtime->poll_events(handle, bytes, (ulong)buffer.Length, &poll);
                if (status != ART_NO_DATA) Check(status);
            }
            var records = buffer.AsSpan(0, checked((int)poll.written_bytes));
            while (!records.IsEmpty)
            {
                int size = checked((int)BinaryPrimitives.ReadUInt32LittleEndian(records));
                if (size < EventHeaderBytes || size > records.Length ||
                    BinaryPrimitives.ReadUInt16LittleEndian(records[4..]) != ART_EVENT_ENVELOPE_VERSION)
                    throw new InvalidOperationException("Invalid event record envelope.");
                ReplyToIo(runtime, handle, records[..size]);
                records = records[size..];
            }
            if (result.latest_render_frame_id != 0)
            {
                var state = new ArtRuntimeState { struct_size = (uint)sizeof(ArtRuntimeState) };
                Check(runtime->get_state(handle, &state));
                if (state.state != ART_RUNTIME_RUNNING) throw new InvalidOperationException("Project did not start.");
                CheckFrame(frames, handle);
                return;
            }
            if ((result.flags & ART_WAIT_IO) == 0) frameId++;
        }
        throw new InvalidOperationException("No frame was published.");
    }

    private static void ReplyToIo(ArtRuntimeApiV2* runtime, ulong handle, ReadOnlySpan<byte> record)
    {
        if (BinaryPrimitives.ReadUInt16LittleEndian(record[6..]) != ART_DOMAIN_IO) return;
        uint kind = BinaryPrimitives.ReadUInt32LittleEndian(record[8..]);
        int metadataSize = checked((int)BinaryPrimitives.ReadUInt32LittleEndian(record[EventEnvelopeBytes..]));
        using var metadata = JsonDocument.Parse(record.Slice(EventHeaderBytes, metadataSize).ToArray());
        bool hasAsset = metadata.RootElement.GetProperty("key").GetString() == "boot.iet";
        byte[] payload = Array.Empty<byte>();
        string response;
        if (kind == ART_IO_STAT)
        {
            response = hasAsset
                ? $"{{\"status\":\"ok\",\"found\":true,\"size\":\"{Boot.Length}\",\"revision\":\"1\"}}"
                : "{\"status\":\"ok\",\"found\":false}";
        }
        else if (kind == ART_IO_READ && hasAsset)
        {
            payload = Boot;
            response = "{\"status\":\"ok\",\"revision\":\"1\",\"eof\":true}";
        }
        else
        {
            throw new InvalidOperationException($"Unexpected IO request {kind}.");
        }
        byte[] json = Encoding.UTF8.GetBytes(response);
        fixed (byte* jsonBytes = json, blob = payload)
        {
            var reply = new ArtMessage
            {
                struct_size = (uint)sizeof(ArtMessage), domain = ART_DOMAIN_IO,
                schema_version = ART_SUBMIT_SCHEMA_VERSION, kind = kind,
                session_epoch = BinaryPrimitives.ReadUInt64LittleEndian(record[24..]),
                request_id = BinaryPrimitives.ReadUInt64LittleEndian(record[40..]),
                metadata_utf8 = new ArtBytes { data = jsonBytes, size = (ulong)json.Length },
                blob = new ArtBytes { data = blob, size = (ulong)payload.Length },
            };
            Check(runtime->submit(handle, &reply, 1));
        }
    }

    private static void CheckFrame(ArtFrameApiV2* frames, ulong handle)
    {
        var frame = new ArtFrameView { struct_size = (uint)sizeof(ArtFrameView) };
        Check(frames->acquire_latest(handle, 0, &frame));
        try
        {
            if (frame.schema_version != ART_FRAME_SCHEMA_VERSION || frame.packet.size < 4 ||
                Encoding.ASCII.GetString(frame.packet.data, 4) != "ARFP")
                throw new InvalidOperationException("Invalid frame packet.");
            ArtBytes pixels = default;
            Check(frames->texture_rgba(handle, frame.token, 0, &pixels));
            if (pixels.size < 4 || pixels.data[0] != 0x11 || pixels.data[1] != 0x22 ||
                pixels.data[2] != 0x33 || pixels.data[3] != 0xff)
                throw new InvalidOperationException("Unexpected RGBA pixels.");
        }
        finally
        {
            Check(frames->release(handle, frame.token));
        }
    }

    private static void Check(int status)
    {
        if (status != ART_OK) throw new InvalidOperationException($"Artemis returned status {status}.");
    }
}
