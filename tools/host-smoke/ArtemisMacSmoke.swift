import CoreVideo
import Foundation

private enum SmokeError: Error {
    case check(String)
}

private func expect(_ passed: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !passed() { throw SmokeError.check(message) }
}

private let boot = Data("""
    *top
    [calllua function=load]
    [lyc id=1 width=4 height=4 color=ff00ff00]
    [takess]
    [caption data=\(String(repeating: "x", count: 5000))]
    [stop]
    [lua]
    function load(e)
        calls = (calls or 0) + 1
        local payload = assert(e:file('data' .. calls))
        local f = assert(io.open('save/slot', 'w'))
        f:write(payload)
        local ok, message = pcall(function() f:close() end)
        assert(not ok and string.find(message, 'denied'))
        assert(f:close())
        assert(calls == 1)
    end
    [/lua]
    """.utf8)

private func metadata(_ event: ArtemisMacEvent) throws -> [String: Any] {
    guard let object = try JSONSerialization.jsonObject(with: event.metadata) as? [String: Any] else {
        throw SmokeError.check("metadata is not an object")
    }
    return object
}

private func reply(_ event: ArtemisMacEvent, _ metadata: [String: Any], blob: Data = Data()) throws -> ArtemisMacMessage {
    try event.reply(metadata: JSONSerialization.data(withJSONObject: metadata), blob: blob)
}

private func service(_ event: ArtemisMacEvent, assets: URL, slot: URL, deny: Bool) throws -> ArtemisMacMessage {
    try expect(!Thread.isMainThread, "IO ran on the main thread")
    let args = try metadata(event)
    guard let key = args["key"] as? String else { throw SmokeError.check("missing resource key") }
    if event.kind == UInt32(ArtSwiftIoWrite) {
        try expect(args["namespace"] as? String == "save" && key == "slot", "unexpected write destination")
        if deny {
            try expect(Data(contentsOf: slot).isEmpty, "failed write changed the file")
            return try reply(event, ["status": "denied", "message": "denied"])
        }
        try event.blob.write(to: slot, options: .atomic)
        return try reply(event, ["status": "ok"])
    }
    let bytes: Data?
    switch key {
    case "boot.iet": bytes = boot
    case "data1": bytes = try Data(contentsOf: assets)
    default: bytes = nil
    }
    if event.kind == UInt32(ArtSwiftIoStat) {
        guard let bytes else { return try reply(event, ["status": "ok", "found": false]) }
        return try reply(event, ["status": "ok", "found": true, "size": String(bytes.count), "revision": "1"])
    }
    guard event.kind == UInt32(ArtSwiftIoRead), let bytes,
          let offset = (args["offset"] as? String).flatMap(Int.init),
          let length = (args["length"] as? String).flatMap(Int.init),
          offset >= 0, offset < bytes.count, length > 0 else { throw SmokeError.check("unexpected resource read") }
    let end = offset + min(length, bytes.count - offset, key == "data1" ? 2 : bytes.count)
    return try reply(event, ["status": "ok", "revision": "1", "eof": end == bytes.count],
                     blob: bytes.subdata(in: offset..<end))
}

private func makeSurface() throws -> CVPixelBuffer {
    let attrs: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:],
                                kCVPixelBufferMetalCompatibilityKey: true]
    var pixels: CVPixelBuffer?
    guard CVPixelBufferCreate(nil, 4, 4, kCVPixelFormatType_32BGRA, attrs as CFDictionary, &pixels) == kCVReturnSuccess,
          let pixels else { throw SmokeError.check("no shared pixel buffer") }
    return pixels
}

private func verifySurface(_ surface: CVPixelBuffer) throws {
    try expect(CVPixelBufferLockBaseAddress(surface, .readOnly) == kCVReturnSuccess, "cannot lock shared surface")
    defer { CVPixelBufferUnlockBaseAddress(surface, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddress(surface)?.assumingMemoryBound(to: UInt8.self) else {
        throw SmokeError.check("no shared pixels")
    }
    let stride = CVPixelBufferGetBytesPerRow(surface)
    for y in 0..<4 {
        for x in 0..<4 {
            let pixel = UnsafeBufferPointer(start: base.advanced(by: y * stride + x * 4), count: 4)
            try expect(pixel.elementsEqual([0, 255, 0, 255]), "unexpected shared pixel")
        }
    }
}

@main struct ArtemisMacSmoke {
    @MainActor static func main() async throws {
        let args = CommandLine.arguments
        try expect(args.count == 3, "expected Core and Metal dylib paths")
        let session = try ArtemisMacSession(corePath: args[1], metalPath: args[2], hostCaps: art_swift_io_cap(), screenshots: true)
        let other = try ArtemisMacSession(corePath: args[1], metalPath: args[2], hostCaps: art_swift_io_cap())
        defer { try? other.close(); try? session.close() }
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let assets = root.appendingPathComponent("data1")
        let slot = root.appendingPathComponent("slot")
        let payload = Data("confirm".utf8)
        try payload.write(to: assets, options: .atomic)
        let surface = try makeSurface()
        try session.setSurface(surface)
        let ini = Data("[WINDOWS]\nWIDTH=4\nHEIGHT=4\nBOOT=boot.iet\n".utf8)
        try session.openProject(ini, platform: "windows")
        try other.openProject(ini, platform: "windows")
        try expect(try session.present() == nil, "project displayed before stepping")

        var writeIDs: [UInt64] = []
        var lastSequence: UInt64 = 0
        var readOffsets: [String] = []
        var captured = false
        var receivedCaption = false
        var checkedSessions = false
        func step() throws -> ArtStepResult {
            try expect(Thread.isMainThread, "step left the main thread")
            return try session.step(frameID: 1, monotonicNS: 100, unixMS: 1, utcOffsetMinutes: 0).result
        }

        for _ in 0..<48 where !receivedCaption {
            _ = try step()
            var events = try session.pollEvents()
            while !events.isEmpty {
                for event in events {
                    try expect(event.sequence > lastSequence, "events reordered")
                    lastSequence = event.sequence
                    if event.domain == UInt16(ArtSwiftUI) && event.kind == UInt32(ArtSwiftCaption) {
                        try expect(try metadata(event)["text"] as? String == String(repeating: "x", count: 5000),
                                   "large caption was truncated")
                        receivedCaption = true
                    }
                    guard event.requiresReply else { continue }
                    let message: ArtemisMacMessage
                    if event.domain == UInt16(ArtSwiftIo) {
                        if event.kind == UInt32(ArtSwiftIoRead), try metadata(event)["key"] as? String == "data1" {
                            readOffsets.append(try metadata(event)["offset"] as? String ?? "missing")
                        }
                        if event.kind == UInt32(ArtSwiftIoWrite) { writeIDs.append(event.requestID) }
                        let deny = writeIDs.count == 2 && event.kind == UInt32(ArtSwiftIoWrite)
                        let waiting = try step()
                        try expect(waiting.flags & UInt32(ArtSwiftWaitIo) != 0, "Core advanced before the IO reply")
                        message = try await Task.detached {
                            try service(event, assets: assets, slot: slot, deny: deny)
                        }.value
                    } else {
                        let args = try metadata(event)
                        guard event.domain == UInt16(ArtSwiftRender), event.kind == UInt32(ArtSwiftCapture),
                              let id = (args["render_frame_id"] as? String).flatMap(UInt64.init) else {
                            throw SmokeError.check("unexpected request")
                        }
                        let frame = try session.capture(frameID: id)
                        try expect(frame.width == 4 && frame.height == 4 && frame.rgba == Data(Array(repeating: [UInt8(0), 255, 0, 255], count: 16).joined()),
                                   "capture did not read the logical frame")
                        captured = true
                        message = try reply(event, ["status": "ok"])
                    }
                    try expect(Thread.isMainThread, "reply left the main thread")
                    if !checkedSessions {
                        do {
                            _ = try other.submit([message])
                            throw SmokeError.check("another session accepted the reply")
                        } catch ArtemisMacError.wrongSession {}
                        checkedSessions = true
                    }
                    try expect(try session.submit([message]), "reply was not accepted")
                    do {
                        _ = try session.submit([message])
                        throw SmokeError.check("duplicate reply was accepted")
                    } catch ArtemisMacError.status(_, let status, _) where status == ArtSwiftStale {}
                }
                events = try session.pollEvents()
            }
        }
        try expect(receivedCaption && captured && checkedSessions, "session did not reach the final caption")
        try expect(readOffsets == ["0", "2", "4", "6"], "dynamic read restarted or skipped bytes")
        try expect(writeIDs.count == 3 && Set(writeIDs).count == 3, "writes replayed or lost the retry identity")
        try expect(try Data(contentsOf: slot) == payload, "the retried write did not persist the result")
        _ = try step()
        try expect(try session.present() != nil, "latest frame was not presented")
        try verifySurface(surface)
        try expect(try session.present() == nil, "unchanged frame was presented again")
        let replacement = try makeSurface()
        try session.setSurface(replacement)
        try expect(try session.present() != nil, "new surface did not get the latest frame")
        try verifySurface(replacement)
        try session.close()
        try other.close()
        print("Artemis Swift Host smoke passed")
    }
}
