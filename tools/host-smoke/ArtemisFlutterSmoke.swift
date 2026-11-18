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

@main struct ArtemisFlutterSmoke {
    @MainActor static func main() throws {
        let paths = CommandLine.arguments.dropFirst()
        guard paths.count == 2, let core = paths.first, let metal = paths.last else {
            throw Failure(detail: "Expected Core and Metal dylib paths")
        }
        let messenger = Messenger()
        let registry = Registry()
        let host = ArtemisFlutterHost(registry: registry, corePath: core, metalPath: metal)
        host.register(messenger: messenger)
        defer { host.close() }

        let ini = Data("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n".utf8)
        let boot = Data("""
        *top
        [lyc id=1 width=4 height=4 color=ffff0000]
        [seplay id=fx file=tone.ogg]
        [caption data=ready]
        [stop]
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
        print("Artemis Flutter channel smoke passed")
    }
}
