import CoreVideo
import FlutterMacOS
import Foundation
import IOSurface

private struct ProbeError: Error, CustomStringConvertible {
    let description: String
    init(_ message: String) { description = message }
}

private func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ProbeError(message) }
}

private func readExactly(_ count: Int) throws -> Data {
    var bytes = Data()
    while bytes.count < count {
        guard let chunk = try FileHandle.standardInput.read(upToCount: count - bytes.count),
              !chunk.isEmpty else { throw ProbeError("Incomplete DXVK frame stream") }
        bytes.append(chunk)
    }
    return bytes
}

private struct Frame {
    let width: Int
    let height: Int
    let bgra: Data

    init(id: Int, width: Int, height: Int) throws {
        let header = try readExactly(12)
        let words = header.withUnsafeBytes { bytes in
            stride(from: 0, to: 12, by: 4).map {
                Int(UInt32(littleEndian: bytes.loadUnaligned(fromByteOffset: $0, as: UInt32.self)))
            }
        }
        try require(words == [id, width, height], "Unexpected DXVK frame header")
        self.width = width
        self.height = height
        bgra = try readExactly(width * height * 4)
    }

    func makePixelBuffer() throws -> CVPixelBuffer {
        let attributes: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:],
                                           kCVPixelBufferMetalCompatibilityKey: true]
        var output: CVPixelBuffer?
        let status = CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA,
                                         attributes as CFDictionary, &output)
        guard status == kCVReturnSuccess, let output, CVPixelBufferGetIOSurface(output) != nil else {
            throw ProbeError("Cannot allocate Flutter BGRA IOSurface")
        }
        try require(CVPixelBufferLockBaseAddress(output, []) == kCVReturnSuccess, "Cannot lock output for writing")
        defer { CVPixelBufferUnlockBaseAddress(output, []) }
        guard let base = CVPixelBufferGetBaseAddress(output) else { throw ProbeError("No output base address") }
        let pitch = CVPixelBufferGetBytesPerRow(output)
        bgra.withUnsafeBytes { (src: UnsafeRawBufferPointer) in
            for row in 0..<height {
                base.advanced(by: row * pitch).copyMemory(
                    from: src.baseAddress!.advanced(by: row * width * 4), byteCount: width * 4
                )
            }
        }
        return output
    }

    func verify(_ buffer: CVPixelBuffer) throws {
        try require(CVPixelBufferGetWidth(buffer) == width && CVPixelBufferGetHeight(buffer) == height &&
                    CVPixelBufferGetPixelFormatType(buffer) == kCVPixelFormatType_32BGRA,
                    "The Flutter texture returned a buffer with the wrong format or size")
        try require(CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess,
                    "Cannot lock the retained frame for reading")
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { throw ProbeError("No retained base address") }
        let pitch = CVPixelBufferGetBytesPerRow(buffer)
        for row in 0..<height {
            let copied = Data(bytes: base.advanced(by: row * pitch), count: width * 4)
            try require(copied == bgra[row * width * 4..<(row + 1) * width * 4], "Retained frame pixels changed")
        }
    }
}

private final class ReadbackTexture: NSObject, FlutterTexture {
    private let lock = NSLock()
    private var buffer: CVPixelBuffer?

    func publish(_ buffer: CVPixelBuffer?) {
        lock.withLock { self.buffer = buffer }
    }

    func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
        lock.withLock { buffer.map { .passRetained($0) } }
    }
}

@main struct ArtemisDXVKTextureSmoke {
    static func main() throws {
        let texture = ReadbackTexture()
        var retained: [(frame: Frame, buffer: CVPixelBuffer)] = []
        var surfaces: Set<IOSurfaceID> = []
        for (id, size) in [(4, 4), (4, 4), (6, 8)].enumerated() {
            let frame = try Frame(id: id + 1, width: size.0, height: size.1)
            texture.publish(try frame.makePixelBuffer())
            guard let buffer = texture.copyPixelBuffer()?.takeRetainedValue(),
                  let surface = CVPixelBufferGetIOSurface(buffer)?.takeUnretainedValue() else {
                throw ProbeError("The Flutter texture did not return an IOSurface")
            }
            try require(surfaces.insert(IOSurfaceGetID(surface)).inserted, "A retained IOSurface was reused")
            retained.append((frame, buffer))
            for picture in retained { try picture.frame.verify(picture.buffer) }
        }
        try require(retained[0].frame.bgra != retained[1].frame.bgra, "Successive DXVK frames did not change")
        try require(try FileHandle.standardInput.read(upToCount: 1)?.isEmpty != false, "Extra DXVK stream data")
        texture.publish(nil)
        try require(texture.copyPixelBuffer() == nil, "Detached texture still exposed a frame")
        for picture in retained { try picture.frame.verify(picture.buffer) }
        print("DXVK Flutter texture: three IOSurface frames passed update, resize and detach")
    }
}
