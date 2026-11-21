import AVFoundation
import CoreVideo
import FlutterMacOS
import Foundation

private struct Failure: Error { let detail: String }

private func expect(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !condition() { throw Failure(detail: message) }
}

private final class Messenger: NSObject, FlutterBinaryMessenger {
    var handler: FlutterBinaryMessageHandler?
    var session: String?

    func send(onChannel channel: String, message: Data?) {}
    func send(onChannel channel: String, message: Data?, binaryReply callback: FlutterBinaryReply?) {}
    func setMessageHandlerOnChannel(_ channel: String, binaryMessageHandler handler: FlutterBinaryMessageHandler?)
        -> FlutterBinaryMessengerConnection {
        self.handler = handler
        return 1
    }
    func cleanUpConnection(_ connection: FlutterBinaryMessengerConnection) { handler = nil }

    func call(_ method: String, _ args: [String: Any] = [:], session: String? = nil) throws -> Any? {
        let codec = FlutterStandardMethodCodec.sharedInstance()
        var message = args
        message["session"] = session ?? self.session
        let call = FlutterMethodCall(methodName: method, arguments: message)
        var envelope: Data?
        handler?(codec.encode(call)) { envelope = $0 }
        guard let envelope else { throw Failure(detail: "No envelope for \(method)") }
        return codec.decodeEnvelope(envelope)
    }
}

private final class Registry: NSObject, FlutterTextureRegistry {
    private(set) var textures: [Int64: FlutterTexture] = [:]
    private(set) var notifications: [Int64] = []
    private(set) var removed: [Int64] = []
    private var nextID: Int64 = 0

    func register(_ texture: FlutterTexture) -> Int64 {
        let id = nextID
        nextID += 1
        textures[id] = texture
        return id
    }
    func textureFrameAvailable(_ textureId: Int64) { notifications.append(textureId) }
    func unregisterTexture(_ textureId: Int64) {
        textures.removeValue(forKey: textureId)
        removed.append(textureId)
    }
}

@MainActor private final class VideoFixture: ArtemisVideoSource {
    private let pixels: [CVPixelBuffer]
    private(set) var reads = 0
    var terminalState: String? { reads == pixels.count ? "ended" : nil }

    init(_ pixels: [CVPixelBuffer]) { self.pixels = pixels }

    func frame() -> CVPixelBuffer? {
        guard reads < pixels.count else { return nil }
        defer { reads += 1 }
        return pixels[reads]
    }

    func setVolume(_ volume: Float) {}
    func setSuspended(_ suspended: Bool) {}
    func close() {}
}

@main struct ArtemisFlutterSmoke {
    @MainActor static func main() throws {
        let paths = Array(CommandLine.arguments.dropFirst())
        guard (2...3).contains(paths.count) else {
            throw Failure(detail: "Expected Core and Metal dylib paths")
        }
        let messenger = Messenger()
        let registry = Registry()
        let angle = paths.count == 3 ? paths[2] : nil
        try verifyHeadless(corePath: paths[0], missingMetalPath: paths[1] + "/missing")
        try verifyUnavailableAngle(corePath: paths[0], metalPath: paths[1])
        let dxvk = ProcessInfo.processInfo.environment["ARTEMIS_DXVK_RUNTIME_DIR"]
        if let dxvk {
            try verifyUnavailableAngle(corePath: paths[0], metalPath: paths[1], dxvkDirectory: dxvk)
        }
        let host = ArtemisFlutterHost(registry: registry, corePath: paths[0], metalPath: paths[1],
                                      angleDirectory: angle, dxvkDirectory: dxvk)
        host.register(messenger: messenger)
        defer { host.close() }

        let ini = Data("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n".utf8)
        let boot = Data("""
        *top
        [autosave allow=1]
        [lyc id=1 width=4 height=4 color=ffff0000]
        [seplay id=fx file=tone.ogg]
        [caption data=ready]
        [wt0]
        """.utf8)
        let open: [String: Any] = ["ini": FlutterStandardTypedData(bytes: ini), "platform": "windows",
                                   "hostCaps": art_swift_io_cap() | UInt64(ArtSwiftMediaCapability)]
        guard let firstState = try messenger.call("open", open) as? [String: Any],
              let secondState = try messenger.call("open", open) as? [String: Any],
              let firstSession = firstState["session"] as? String,
              let secondSession = secondState["session"] as? String,
              firstSession != secondSession else { throw Failure(detail: "Could not open independent sessions") }
        messenger.session = firstSession
        try expect(messenger.call("surface", ["width": 4, "height": 4]) as? Int == 0, "Texture ID zero lost")
        try expect(messenger.call("present") == nil, "Presented without a frame")

        let tick: [String: Any] = ["frame": 1, "monotonicNS": 1, "unixMS": 0, "utcOffsetMinutes": 0]
        var ready = false
        var checkedIdentity = false
        var checkedPlayback = false
        for _ in 0..<24 where !ready {
            try expect(messenger.call("step", tick) is [String: Any], "Could not step")
            guard let events = try messenger.call("poll") as? [[String: Any]] else { throw Failure(detail: "Could not poll") }
            for event in events {
                let bytes = (event["metadata"] as? FlutterStandardTypedData)?.data ?? Data()
                let args = try JSONSerialization.jsonObject(with: bytes) as? [String: Any] ?? [:]
                if args["text"] as? String == "ready" { ready = true }
                if event["domain"] as? Int == Int(ArtSwiftMedia),
                   event["kind"] as? Int == Int(ArtSwiftAudioCommand),
                   let playback = args["playback_id"] as? String {
                    var feedback = event
                    feedback["kind"] = Int(ArtSwiftPlaybackStatus)
                    feedback["metadata"] = FlutterStandardTypedData(bytes: try JSONSerialization.data(withJSONObject: [
                        "playback_id": playback, "status": "ended",
                    ]))
                    feedback["owner"] = secondSession
                    let error = try messenger.call("submit", ["batch": [feedback]]) as? FlutterError
                    try expect(error?.code == "stale_reply", "Accepted playback feedback with another session's owner")
                    feedback["owner"] = firstSession
                    try expect(messenger.call("submit", ["batch": [feedback]]) as? Bool == true,
                               "Current playback feedback was rejected")
                    checkedPlayback = true
                }
                if event["request"] as? String == "0" { continue }
                var reply = event
                let metadata: [String: Any]
                if event["kind"] as? Int == Int(ArtSwiftIoStat) {
                    metadata = args["key"] as? String == "boot.iet"
                        ? ["status": "ok", "found": true, "size": String(boot.count), "revision": "1"]
                        : ["status": "ok", "found": false]
                } else {
                    try expect(event["kind"] as? Int == Int(ArtSwiftIoRead), "Unexpected request")
                    metadata = ["status": "ok", "revision": "1", "eof": true]
                    reply["blob"] = FlutterStandardTypedData(bytes: boot)
                }
                reply["metadata"] = FlutterStandardTypedData(bytes: try JSONSerialization.data(withJSONObject: metadata))
                if !checkedIdentity {
                    // The two runtimes have valid requests with identical local IDs.
                    _ = try messenger.call("step", tick, session: secondSession)
                    let other = try messenger.call("poll", session: secondSession) as? [[String: Any]]
                    try expect(other?.contains { $0["request"] as? String == event["request"] as? String } == true,
                               "Fixture needs coinciding request IDs")
                    let error = try messenger.call("submit", ["batch": [reply]], session: secondSession) as? FlutterError
                    try expect(error?.code == "stale_reply", "Accepted reply from another session")
                    checkedIdentity = true
                }
                try expect(messenger.call("submit", ["batch": [reply]]) as? Bool == true, "Reply not accepted")
                let error = try messenger.call("submit", ["batch": [reply]]) as? FlutterError
                try expect(error?.code == "stale_reply", "Accepted duplicate reply")
            }
        }
        try expect(ready && checkedIdentity && checkedPlayback, "Project did not complete the channel checks")
        try exerciseBackgroundAutosave(messenger, tick: tick)
        try expect(messenger.call("present") is String, "Frame not presented")
        try expect(messenger.call("present") == nil && registry.notifications == [0], "Notified twice for the same frame")
        guard let buffer = registry.textures[0]?.copyPixelBuffer()?.takeRetainedValue() else {
            throw Failure(detail: "Flutter texture has no pixels")
        }
        try expect(CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess, "Cannot lock pixel buffer")
        let pixel = CVPixelBufferGetBaseAddress(buffer).map { Array(UnsafeBufferPointer(start: $0.assumingMemoryBound(to: UInt8.self), count: 4)) }
        CVPixelBufferUnlockBaseAddress(buffer, .readOnly)
        try expect(pixel == [0, 0, 255, 255], "Unexpected BGRA pixel")
        try expect(messenger.call("surface", ["width": 8, "height": 8]) as? Int == 1, "Could not replace texture")
        try expect(messenger.call("present") is String && registry.notifications == [0, 1], "New texture did not redraw")
        _ = try messenger.call("close")
        try expect(registry.textures.isEmpty && registry.removed == [0, 1], "Did not unregister textures")
        if angle != nil {
            try exerciseAngle(messenger, registry: registry, ini: ini)
        }
        try exerciseLayerVideoDelivery(registry)
        if let clip = ProcessInfo.processInfo.environment["ARTEMIS_VIDEO_SMOKE_CLIP"] {
            try exerciseUnpolledVideoEnd(messenger, registry: registry, open: open, path: clip)
        }
        print("Artemis Flutter channel smoke passed")
    }

    @MainActor private static func verifyHeadless(corePath: String, missingMetalPath: String) throws {
        let messenger = Messenger()
        let registry = Registry()
        let host = ArtemisFlutterHost(registry: registry, corePath: corePath, metalPath: missingMetalPath)
        host.register(messenger: messenger)
        defer { host.close() }
        let ini = Data("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n".utf8)
        guard let state = try messenger.call("open", [
            "ini": FlutterStandardTypedData(bytes: ini), "platform": "windows",
            "hostCaps": art_swift_io_cap(), "renderer": "none",
        ]) as? [String: Any], let session = state["session"] as? String else {
            throw Failure(detail: "Headless session required the renderer library")
        }
        let rejected = try messenger.call("surface", ["width": 4, "height": 4], session: session)
        try expect(rejected is FlutterError && registry.textures.isEmpty, "Headless session registered a texture")
        _ = try messenger.call("close", session: session)
    }

    @MainActor private static func verifyUnavailableAngle(corePath: String, metalPath: String,
                                                          dxvkDirectory: String? = nil) throws {
        let missingDirectory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString).path
        let ini = FlutterStandardTypedData(bytes: Data("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n".utf8))
        for angleDirectory in [nil, missingDirectory] {
            let messenger = Messenger()
            let registry = Registry()
            let host = ArtemisFlutterHost(registry: registry, corePath: corePath, metalPath: metalPath,
                                          angleDirectory: angleDirectory, dxvkDirectory: dxvkDirectory)
            host.register(messenger: messenger)
            defer { host.close() }
            let hasDxvkRuntime: Bool
            if #available(macOS 15.0, *) {
                hasDxvkRuntime = dxvkDirectory != nil
            } else {
                hasDxvkRuntime = false
            }
            try expect(messenger.call("hasDxvkRuntime") as? Bool == hasDxvkRuntime,
                       "Unexpected DXVK runtime availability")
            var options: [String: Any] = [
                "ini": ini, "platform": "windows", "hostCaps": art_swift_io_cap(), "renderer": "angle",
            ]
            try expect(messenger.call("open", options) is FlutterError, "Explicit ANGLE accepted an unavailable runtime")
            options["mixedShaders"] = true
            options["hlslOnDemand"] = true
            guard let state = try messenger.call("open", options) as? [String: Any],
                  let session = state["session"] as? String else {
                throw Failure(detail: "Automatic selection did not fall back when ANGLE was unavailable")
            }
            try expect(state["dxvk"] as? Bool == hasDxvkRuntime, "Unexpected shader renderer after fallback")
            try expect(messenger.call("surface", ["width": 4, "height": 4], session: session) as? Int == 0,
                       "Automatic selection did not provide a usable Metal surface")
            _ = try messenger.call("close", session: session)
            try expect(registry.textures.isEmpty, "Fallback session leaked its surface")
        }
    }

    @MainActor private static func exerciseBackgroundAutosave(_ messenger: Messenger,
                                                               tick: [String: Any]) throws {
        var background = tick
        background["backgroundAutosave"] = true
        background["frame"] = 2
        var written: [String] = []
        for _ in 0..<8 {
            guard let state = try messenger.call("step", background) as? [String: Any],
                  let waits = state["waitFlags"] as? Int,
                  let events = try messenger.call("poll") as? [[String: Any]] else {
                throw Failure(detail: "Could not drive background autosave")
            }
            try expect(state["frame"] as? String == "1", "Background autosave advanced the Host frame")
            for event in events where event["request"] as? String != "0" {
                guard event["domain"] as? Int == Int(ArtSwiftIo),
                      event["kind"] as? Int == Int(ArtSwiftIoWrite),
                      let metadata = event["metadata"] as? FlutterStandardTypedData,
                      let args = try JSONSerialization.jsonObject(with: metadata.data) as? [String: Any],
                      let key = args["key"] as? String else {
                    throw Failure(detail: "Unexpected background autosave request")
                }
                written.append(key)
                var reply = event
                reply["metadata"] = FlutterStandardTypedData(bytes: Data("{\"status\":\"ok\"}".utf8))
                reply["blob"] = FlutterStandardTypedData(bytes: Data())
                try expect(messenger.call("submit", ["batch": [reply]]) as? Bool == true,
                           "Background autosave write acknowledgement was rejected")
            }
            if waits & Int(ArtSwiftWaitIo) == 0 {
                try expect(written == ["__autosave.dat", "saveg.dat", "system.dat"], "Incomplete background autosave")
                return
            }
        }
        throw Failure(detail: "Background autosave did not finish")
    }

    @MainActor private static func exerciseUnpolledVideoEnd(_ messenger: Messenger, registry: Registry,
                                                           open: [String: Any], path: String) throws {
        guard let state = try messenger.call("open", open) as? [String: Any],
              let session = state["session"] as? String, let epoch = state["epoch"] as? String else {
            throw Failure(detail: "Could not open short video session")
        }
        defer { _ = try? messenger.call("close", session: session) }
        var reachedEnd = false
        let center = NotificationCenter.default
        let observer = center.addObserver(forName: .AVPlayerItemDidPlayToEndTime, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { reachedEnd = true }
        }
        defer { center.removeObserver(observer) }
        let playback = "9223372036854775809"
        _ = try messenger.call("videoOpen", ["epoch": epoch, "playback": playback, "path": path,
                                             "fullscreen": true, "volume": 0.0], session: session)
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        while !reachedEnd && ProcessInfo.processInfo.systemUptime < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
        }
        try expect(reachedEnd, "AVPlayer did not reach the end of the short video")
        // Deliver both end observers before the first Host poll.
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.01))
        let events = try messenger.call("videoPoll", session: session) as? [[String: Any]]
        guard let events, events.count == 1, let event = events.first,
              event["playback"] as? String == playback, event["status"] as? String == "started",
              let texture = event["texture"] as? Int64,
              let buffer = registry.textures[texture]?.copyPixelBuffer()?.takeRetainedValue() else {
            throw Failure(detail: "Short video ended before delivery: \(String(describing: events))")
        }
        try expect(CVPixelBufferGetWidth(buffer) == 32 && CVPixelBufferGetHeight(buffer) == 32,
                   "Short video did not publish the expected texture")
        try expect(CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess,
                   "Could not read the short video texture")
        let pixel = CVPixelBufferGetBaseAddress(buffer)?.assumingMemoryBound(to: UInt8.self)
        let red = pixel.map { $0[0] < 16 && $0[1] < 16 && $0[2] > 220 && $0[3] == 255 }
        CVPixelBufferUnlockBaseAddress(buffer, .readOnly)
        try expect(red == true, "Short video published the wrong BGRA pixels")
        let ended = try messenger.call("videoPoll", session: session) as? [[String: String]]
        try expect(ended == [["playback": playback, "status": "ended"]] && registry.textures[texture] == nil,
                   "Short video's terminal event did not release its texture")
        try expect((messenger.call("videoPoll", session: session) as? [[String: Any]])?.isEmpty == true,
                   "Short video reported a second terminal event")
    }

    @MainActor private static func exerciseLayerVideoDelivery(_ registry: Registry) throws {
        func pixelBuffer(_ bgra: [UInt8]) throws -> CVPixelBuffer {
            var pixels: CVPixelBuffer?
            guard CVPixelBufferCreate(nil, 1, 1, kCVPixelFormatType_32BGRA, nil, &pixels) == kCVReturnSuccess,
                  let pixels,
                  CVPixelBufferLockBaseAddress(pixels, []) == kCVReturnSuccess else {
                throw Failure(detail: "Cannot allocate video fixture pixels")
            }
            defer { CVPixelBufferUnlockBaseAddress(pixels, []) }
            guard let bytes = CVPixelBufferGetBaseAddress(pixels) else {
                throw Failure(detail: "Video fixture has no writable pixels")
            }
            bgra.withUnsafeBytes { bytes.copyMemory(from: $0.baseAddress!, byteCount: $0.count) }
            return pixels
        }
        let source = try VideoFixture([pixelBuffer([3, 2, 1, 255]), pixelBuffer([6, 5, 4, 255])])
        let video = ArtemisFlutterVideo(epoch: 7, fullscreen: false, source: source)
        defer { video.close(registry) }
        func submit(hostFrame: UInt64, frame: String, pixel: [UInt8], accepts: Bool) throws {
            let delivered = try video.deliverLayerFrame(playbackID: 9, hostFrameID: hostFrame) { message in
                let args = try JSONSerialization.jsonObject(with: message.metadata) as? [String: Any]
                try expect(message.epoch == 7 && args?["playback_id"] as? String == "9" &&
                           args?["frame_id"] as? String == frame && message.blob == Data(pixel),
                           "Layer video retry changed its identity or pixels")
                return accepts
            }
            try expect(delivered == accepts, "Layer video delivery misreported the submit result")
        }
        try submit(hostFrame: 1, frame: "1", pixel: [1, 2, 3, 255], accepts: false)
        try submit(hostFrame: 2, frame: "1", pixel: [1, 2, 3, 255], accepts: true)
        try expect(source.reads == 1, "Retry consumed another decoder frame")
        try submit(hostFrame: 3, frame: "2", pixel: [4, 5, 6, 255], accepts: false)
        try expect(video.terminalState(after: 3) == nil, "End overtook a blocked layer frame")
        try submit(hostFrame: 4, frame: "2", pixel: [4, 5, 6, 255], accepts: true)
        try expect(video.terminalState(after: 4) == nil && video.terminalState(after: 5) == "ended",
                   "End did not wait until Core advanced past the submitted layer frame")
    }

    @MainActor private static func exerciseAngle(_ messenger: Messenger, registry: Registry, ini: Data) throws {
        messenger.session = nil
        let boot = Data("""
        *top
        [lyshader id=paint file=paint.frag]
        [lyc id=1 width=4 height=4 color=ffff0000]
        [lyprop id=1 shader=paint]
        [caption data=ready]
        [stop]
        """.utf8)
        let shader = Data("""
        precision mediump float;
        varying vec2 resultCoord;
        uniform sampler2D textureFore;
        void main() { gl_FragColor = texture2D(textureFore, resultCoord).bgra; }
        """.utf8)
        let assets = ["boot.iet": boot, "paint.frag": shader]
        let open: [String: Any] = ["ini": FlutterStandardTypedData(bytes: ini), "platform": "windows",
                                   "hostCaps": art_swift_io_cap(), "screenshots": true]
        func render(_ selection: [String: Any]) throws -> (session: String, frame: String, texture: Int64) {
            let args = open.merging(selection) { _, value in value }
            guard let state = try messenger.call("open", args) as? [String: Any],
                  let session = state["session"] as? String,
                  let texture = try messenger.call("surface", ["width": 4, "height": 4], session: session) as? Int else {
                throw Failure(detail: "Could not open an ANGLE session")
            }
            let tick: [String: Any] = ["frame": 1, "monotonicNS": 1, "unixMS": 0, "utcOffsetMinutes": 0]
            var loadedShader = false
            for _ in 0..<32 {
                _ = try messenger.call("step", tick, session: session)
                let events = try messenger.call("poll", session: session) as? [[String: Any]] ?? []
                for event in events {
                    guard event["request"] as? String != "0" else { continue }
                    let args = try JSONSerialization.jsonObject(with: (event["metadata"] as! FlutterStandardTypedData).data)
                        as? [String: Any] ?? [:]
                    let key = args["key"] as? String ?? ""
                    let asset = assets[key]
                    var reply = event
                    let metadata: [String: Any]
                    if event["kind"] as? Int == Int(ArtSwiftIoStat) {
                        metadata = asset.map { ["status": "ok", "found": true, "size": String($0.count), "revision": "1"] }
                            ?? ["status": "ok", "found": false]
                    } else {
                        try expect(event["kind"] as? Int == Int(ArtSwiftIoRead) && asset != nil, "Invalid ANGLE resource request")
                        metadata = ["status": "ok", "revision": "1", "eof": true]
                        reply["blob"] = FlutterStandardTypedData(bytes: asset!)
                        loadedShader = loadedShader || key == "paint.frag"
                    }
                    reply["metadata"] = FlutterStandardTypedData(bytes: try JSONSerialization.data(withJSONObject: metadata))
                    try expect(messenger.call("submit", ["batch": [reply]], session: session) as? Bool == true,
                               "Could not reply to ANGLE session")
                }
                if loadedShader, let frame = try messenger.call("present", session: session) as? String {
                    return (session, frame, Int64(texture))
                }
            }
            throw Failure(detail: "ANGLE scene never presented")
        }
        let first = try render(["renderer": "metal", "mixedShaders": true, "hlslOnDemand": true,
                                "glslSources": [FlutterStandardTypedData(bytes: shader)], "glslPrewarmMilliseconds": 0])
        let second = try render(["renderer": "angle"])
        _ = try messenger.call("close", session: second.session)
        guard let buffer = registry.textures[first.texture]?.copyPixelBuffer()?.takeRetainedValue() else {
            throw Failure(detail: "First ANGLE texture was released with the second session")
        }
        try expect(CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess, "Cannot lock ANGLE pixels")
        let pixel = CVPixelBufferGetBaseAddress(buffer).map { Array(UnsafeBufferPointer(start: $0.assumingMemoryBound(to: UInt8.self), count: 4)) }
        CVPixelBufferUnlockBaseAddress(buffer, .readOnly)
        try expect(pixel == [255, 0, 0, 255], "GLSL did not change red to blue")
        let capture = try messenger.call("capture", ["frame": first.frame], session: first.session) as? [String: Any]
        let rgba = (capture?["rgba"] as? FlutterStandardTypedData)?.data
        try expect(rgba == Data((0..<16).flatMap { _ in [UInt8(0), 0, 255, 255] }),
                   "ANGLE capture lost its framebuffer after closing the second session")
        _ = try messenger.call("surface", ["width": 8, "height": 8], session: first.session)
        try expect(messenger.call("present", session: first.session) as? String == first.frame, "ANGLE did not redraw onto the replacement surface")
        _ = try messenger.call("close", session: first.session)
        try expect(registry.textures.isEmpty, "ANGLE textures remained registered after close")
        print("Artemis Flutter ANGLE pixels, capture, and session isolation passed")
    }
}
