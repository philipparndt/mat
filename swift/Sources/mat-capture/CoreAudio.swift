// Thin wrappers around the Core Audio HAL: property access, audio processes,
// process taps and the private aggregate device that reads them.

import AppKit
import CoreAudio
import Foundation

struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

func check(_ status: OSStatus, _ what: String) throws {
    guard status == noErr else { throw Failure("\(what) failed (OSStatus \(status))") }
}

func stderr(_ message: String) {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
}

// MARK: - Properties

let systemObject = AudioObjectID(kAudioObjectSystemObject)

func address(_ selector: AudioObjectPropertySelector,
             scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain)
}

func read<T: BitwiseCopyable>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector, initial: T) throws -> T {
    var addr = address(selector)
    var size = UInt32(MemoryLayout<T>.size)
    var value = initial
    try check(AudioObjectGetPropertyData(object, &addr, 0, nil, &size, &value), "reading property \(selector)")
    return value
}

func readString(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector) -> String {
    var addr = address(selector)
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    var value: Unmanaged<CFString>?
    guard AudioObjectGetPropertyData(object, &addr, 0, nil, &size, &value) == noErr, let value else { return "" }
    return value.takeRetainedValue() as String
}

func readObjectList(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector) throws -> [AudioObjectID] {
    var addr = address(selector)
    var size: UInt32 = 0
    try check(AudioObjectGetPropertyDataSize(object, &addr, 0, nil, &size), "reading list size \(selector)")
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    try check(AudioObjectGetPropertyData(object, &addr, 0, nil, &size, &ids), "reading list \(selector)")
    return Array(ids.prefix(Int(size) / MemoryLayout<AudioObjectID>.size))
}

// MARK: - Audio processes

/// The process that macOS holds responsible for `pid`: Safari for its WebKit
/// GPU process, Chrome for its helpers. Private libsystem SPI, looked up at
/// runtime so a missing symbol only loses the grouping.
private let responsiblePIDFor: (@convention(c) (pid_t) -> pid_t)? = {
    guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_get_pid_responsible_for_pid") else { return nil }
    return unsafeBitCast(symbol, to: (@convention(c) (pid_t) -> pid_t).self)
}()

/// A process that is connected to Core Audio.
struct AudioProcess {
    let object: AudioObjectID
    let pid: pid_t
    let name: String
    let bundleID: String
    /// The app the user sees: the responsible process, or the process itself.
    let appPID: pid_t
    let appName: String
    let appBundleID: String
    let isPlaying: Bool

    static func all() throws -> [AudioProcess] {
        try readObjectList(systemObject, kAudioHardwarePropertyProcessObjectList).compactMap { object in
            guard let pid = try? read(object, kAudioProcessPropertyPID, initial: pid_t(-1)), pid > 0, pid != getpid() else { return nil }
            let playing = (try? read(object, kAudioProcessPropertyIsRunningOutput, initial: UInt32(0))) ?? 0
            let appPID = responsiblePIDFor.map { $0(pid) }.flatMap { $0 > 0 ? $0 : nil } ?? pid
            let app = NSRunningApplication(processIdentifier: appPID)
            let bundleID = readString(object, kAudioProcessPropertyBundleID)
            // Other users' processes (system daemons) hide their names.
            let name = processName(pid) ?? (bundleID.isEmpty ? "pid \(pid)" : bundleID)
            return AudioProcess(
                object: object, pid: pid, name: name, bundleID: bundleID,
                appPID: appPID, appName: app?.localizedName ?? (appPID == pid ? name : processName(appPID) ?? name),
                appBundleID: app?.bundleIdentifier ?? "",
                isPlaying: playing != 0)
        }
    }

    /// `query` is a pid, an app or process name, or a bundle ID (which also
    /// matches the app's helpers, e.g. `com.google.Chrome` for
    /// `com.google.Chrome.helper`).
    func matches(_ query: String) -> Bool {
        if let pid = pid_t(query) { return pid == self.pid || pid == appPID }
        let q = query.lowercased()
        let names = [name, appName].map { $0.lowercased() }
        let bundles = [bundleID, appBundleID].map { $0.lowercased() }.filter { !$0.isEmpty }
        return names.contains(q) || bundles.contains { $0 == q || $0.hasPrefix(q + ".") }
    }

    var label: String {
        let own = appPID == pid ? "" : " via \(bundleID.isEmpty ? name : bundleID)"
        return "\(appName) (pid \(pid)\(own))"
    }
}

private func processName(_ pid: pid_t) -> String? {
    var buffer = [CChar](repeating: 0, count: 256)
    guard proc_name(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return String(cString: buffer)
}

// MARK: - Tap

/// A process tap read through a private aggregate device. The set of tapped
/// processes can change while it runs; nothing is torn down for that.
final class ProcessTap {
    let description: CATapDescription
    private(set) var tapID = AudioObjectID(kAudioObjectUnknown)
    private(set) var deviceID = AudioObjectID(kAudioObjectUnknown)
    private var ioProc: AudioDeviceIOProcID?
    let sampleRate: Double
    let channels: Int
    private let nonInterleaved: Bool

    /// `processes` are tapped, or with `system` everything except them.
    init(processes: [AudioObjectID], system: Bool, mute: Bool) throws {
        description = system
            ? CATapDescription(stereoGlobalTapButExcludeProcesses: processes)
            : CATapDescription(stereoMixdownOfProcesses: processes)
        description.uuid = UUID()
        description.name = "mat-capture"
        description.isPrivate = true
        description.muteBehavior = mute ? .muted : .unmuted

        var tap = AudioObjectID(kAudioObjectUnknown)
        try check(AudioHardwareCreateProcessTap(description, &tap), "creating the process tap")
        var format = AudioStreamBasicDescription()
        do {
            format = try read(tap, kAudioTapPropertyFormat, initial: format)
            guard format.mFormatID == kAudioFormatLinearPCM,
                  format.mFormatFlags & kAudioFormatFlagIsFloat != 0, format.mBitsPerChannel == 32 else {
                throw Failure("unexpected tap format (\(format.mBitsPerChannel)-bit, flags \(format.mFormatFlags))")
            }
        } catch {
            AudioHardwareDestroyProcessTap(tap)
            throw error
        }
        tapID = tap
        channels = Int(format.mChannelsPerFrame)
        nonInterleaved = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        sampleRate = format.mSampleRate

        // The tap is the aggregate's only member, so it is also its clock:
        // switching the output device mid-recording does not disturb it.
        let aggregate: [String: Any] = [
            kAudioAggregateDeviceNameKey: "mat-capture",
            kAudioAggregateDeviceUIDKey: "mat-capture-\(description.uuid.uuidString)",
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapUIDKey: description.uuid.uuidString,
                kAudioSubTapDriftCompensationKey: true,
            ]],
        ]
        try check(AudioHardwareCreateAggregateDevice(aggregate as CFDictionary, &deviceID), "creating the aggregate device")
    }

    /// Replaces the tapped (or, for a system tap, excluded) processes on the running tap.
    func setProcesses(_ processes: [AudioObjectID]) throws {
        description.processes = processes
        var addr = address(kAudioTapPropertyDescription)
        var ref = Unmanaged.passUnretained(description).toOpaque()
        try check(AudioObjectSetPropertyData(tapID, &addr, 0, nil, UInt32(MemoryLayout<UnsafeMutableRawPointer>.size), &ref),
                  "updating the tapped processes")
    }

    /// Starts IO. `onAudio` runs on the real-time IO thread with interleaved
    /// Float32 frames: it must not allocate, lock for long, or block.
    func start(_ onAudio: @escaping (UnsafePointer<Float32>, Int) -> Void) throws {
        let channels = channels
        let nonInterleaved = nonInterleaved
        let scratch = UnsafeMutablePointer<Float32>.allocate(capacity: 16_384 * channels)
        try check(AudioDeviceCreateIOProcIDWithBlock(&ioProc, deviceID, nil) { _, input, _, _, _ in
            let buffers = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: input))
            if nonInterleaved {
                guard buffers.count >= channels else { return }
                let frames = min(Int(buffers[0].mDataByteSize) / MemoryLayout<Float32>.size, 16_384)
                for c in 0..<channels {
                    guard let plane = buffers[c].mData?.assumingMemoryBound(to: Float32.self) else { return }
                    for f in 0..<frames { scratch[f * channels + c] = plane[f] }
                }
                onAudio(scratch, frames)
            } else {
                guard buffers.count >= 1, let data = buffers[0].mData else { return }
                onAudio(data.assumingMemoryBound(to: Float32.self),
                        Int(buffers[0].mDataByteSize) / MemoryLayout<Float32>.size / channels)
            }
        }, "creating the IO proc")
        try check(AudioDeviceStart(deviceID, ioProc), "starting the aggregate device")
    }

    func destroy() {
        if deviceID != kAudioObjectUnknown {
            if let ioProc {
                AudioDeviceStop(deviceID, ioProc)
                AudioDeviceDestroyIOProcID(deviceID, ioProc)
            }
            AudioHardwareDestroyAggregateDevice(deviceID)
            deviceID = AudioObjectID(kAudioObjectUnknown)
        }
        if tapID != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    deinit { destroy() }
}
