import CoreAudio
import CoreVideo
import Foundation

@main struct ArtemisLegacyMediaSmoke {
    @MainActor static func main() async {
        do {
            try await run()
            print("Artemis legacy video scheduling smoke passed")
        } catch {
            FileHandle.standardError.write(Data("\(error)\n".utf8))
            exit(1)
        }
    }

    @MainActor private static func run() async throws {
        guard CommandLine.arguments.count == 3 else {
            throw Failure("Expected the media Host path and delayed WMV clip")
        }
        let verifyDeviceOutput = ProcessInfo.processInfo.environment["ARTEMIS_MEDIA_DEVICE_SMOKE"] == "1"
        if verifyDeviceOutput {
            guard #available(macOS 14.2, *) else {
                throw Failure("Device output verification requires macOS 14.2 or later")
            }
        }
        let library = try ArtemisLegacyMediaLibrary(path: CommandLine.arguments[1])
        let source = ArtemisLegacyVideoSource(library: library, path: CommandLine.arguments[2],
            loop: false, delayMarginMS: 50, suspended: false, volume: verifyDeviceOutput ? 0.1 : 0)
        defer { source.close() }

        let start = ProcessInfo.processInfo.systemUptime
        var delivered = 0
        var paused = false
        var activeBeforePause = false
        var activeAfterResume = false
        var stalled = false
        var previousPTS = -1.0
        var maxLag = 0.0
        while source.terminalState == nil && ProcessInfo.processInfo.systemUptime - start < 22 {
            if let frame = source.nextFrame() {
                let elapsed = ProcessInfo.processInfo.systemUptime - start
                guard delivered != 0 || elapsed < 2 else {
                    throw Failure("First frame was delayed by the file's absolute timestamp")
                }
                guard CVPixelBufferGetWidth(frame.buffer) == 32, CVPixelBufferGetHeight(frame.buffer) == 32 else {
                    throw Failure("Unexpected decoded frame extent")
                }
                let lag = source.playbackTime - frame.pts
                guard frame.pts > previousPTS, (-0.005...0.22).contains(lag) else {
                    throw Failure("Presented PTS \(frame.pts) after \(previousPTS); lag \(lag) s")
                }
                previousPTS = frame.pts
                maxLag = max(maxLag, lag)
                delivered += 1
                if verifyDeviceOutput, #available(macOS 14.2, *), audioOutputIsRunning() {
                    if paused {
                        activeAfterResume = true
                    } else {
                        activeBeforePause = true
                    }
                }
                if !stalled && frame.pts >= 4 {
                    try await Task.sleep(nanoseconds: 650_000_000)
                    stalled = true
                }
                if !paused && frame.pts >= 10 {
                    source.setSuspended(true)
                    let heldTime = source.playbackTime
                    try await Task.sleep(nanoseconds: 200_000_000)
                    guard source.nextFrame() == nil, source.terminalState == nil,
                          abs(source.playbackTime - heldTime) < 0.05 else {
                        throw Failure("Suspended video advanced or retired")
                    }
                    source.setSuspended(false)
                    paused = true
                }
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        let elapsed = ProcessInfo.processInfo.systemUptime - start
        guard source.terminalState == "ended", delivered >= 390, paused, stalled, elapsed >= 18,
              previousPTS >= 17.9 else {
            throw Failure("Expected playback to finish; got \(delivered) frames in \(elapsed) s, status \(source.terminalState ?? "none")")
        }
        if verifyDeviceOutput {
            guard activeBeforePause, activeAfterResume else {
                throw Failure("Active Core Audio output was not seen before pause and after resume")
            }
            print("Core Audio output active before pause and after resume")
        }
        print("Delivered \(delivered) frames in \(elapsed) s; maximum video lag \(maxLag * 1000) ms")
    }

    @available(macOS 14.2, *)
    private static func audioOutputIsRunning() -> Bool {
        var translate = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var pid = ProcessInfo.processInfo.processIdentifier
        var process = AudioObjectID(kAudioObjectUnknown)
        var bytes = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &translate,
            UInt32(MemoryLayout<pid_t>.size), &pid, &bytes, &process) == noErr,
            process != kAudioObjectUnknown else { return false }
        var output = AudioObjectPropertyAddress(mSelector: kAudioProcessPropertyIsRunningOutput,
            mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var running: UInt32 = 0
        bytes = UInt32(MemoryLayout<UInt32>.size)
        return AudioObjectGetPropertyData(process, &output, 0, nil, &bytes, &running) == noErr && running != 0
    }
}

private struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}
