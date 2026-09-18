// mat-capture: records the audio of other apps with Core Audio process taps.
//
//   mat-capture list
//   mat-capture record <out.wav> --app <name|bundle-id|pid>... [--mute] [--seconds N]
//   mat-capture record <out.wav> --system [--exclude <app>...] [--seconds N]
//   mat-capture stream --app <app>...     raw interleaved f32le on stdout
//   mat-capture permission
//
// Nothing needs restarting: apps are tapped while they play, apps that start
// later are picked up as soon as they open an audio connection, and no driver
// is installed. Build it as an app bundle with swift/bundle-capture.sh so the
// recording permission belongs to MatCapture.app rather than the terminal.

import CoreAudio
import Foundation

let usage = """
    usage:
      mat-capture list                                   processes connected to Core Audio (* = playing)
      mat-capture record <out.wav> <source> [options]    record to a 32-bit float WAV
      mat-capture stream <source> [options]              raw interleaved f32le on stdout
      mat-capture permission                             ask for the audio recording permission

    source:
      --app <name|bundle-id|pid>    tap an app, repeatable; waits if it is not using audio yet
      --system [--exclude <app>]    tap everything, optionally except some apps

    options:
      --mute            silence the tapped audio on the speakers while capturing
      --seconds <n>     stop after n seconds (otherwise Ctrl-C)
      --help, -h        print this
    """

struct CaptureOptions {
    var apps: [String] = []
    var excludes: [String] = []
    var system = false
    var mute = false
    var seconds: Double?
    var output: String?

    init(_ arguments: [String], wantsOutput: Bool) throws {
        var it = arguments.makeIterator()
        func value(_ flag: String) throws -> String {
            guard let v = it.next() else { throw Failure("\(flag) needs a value") }
            return v
        }
        while let arg = it.next() {
            switch arg {
            case "--app": apps.append(try value(arg))
            case "--exclude": excludes.append(try value(arg))
            case "--system": system = true
            case "--mute": mute = true
            case "--seconds":
                guard let s = Double(try value(arg)), s > 0 else { throw Failure("--seconds needs a positive number") }
                seconds = s
            default:
                guard wantsOutput, output == nil, !arg.hasPrefix("--") else { throw Failure("unexpected argument: \(arg)") }
                output = arg
            }
        }
        if wantsOutput && output == nil { throw Failure("record needs an output file") }
        if system == !apps.isEmpty { throw Failure("give either --app or --system") }
        if !excludes.isEmpty && !system { throw Failure("--exclude only works with --system") }
    }
}

func list() throws {
    let processes = try AudioProcess.all().sorted { ($0.appName.lowercased(), $0.pid) < ($1.appName.lowercased(), $1.pid) }
    for p in processes {
        let detail = p.appPID == p.pid ? p.bundleID : "via \(p.bundleID.isEmpty ? p.name : p.bundleID)"
        print("\(p.isPlaying ? "*" : " ") \(String(p.pid).padding(toLength: 6, withPad: " ", startingAt: 0)) \(p.appName.padding(toLength: 28, withPad: " ", startingAt: 0)) \(detail)")
    }
}

func requirePermission() throws {
    switch AudioCapturePermission.request() {
    case .granted, .unknown: return
    case .denied, .undetermined: throw Failure("audio recording is not allowed: \(AudioCapturePermission.settingsHint)")
    }
}

final class Flag {
    private let lock = NSLock()
    private var value = false
    func set() { lock.lock(); value = true; lock.unlock() }
    var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
}

func capture(_ options: CaptureOptions, makeSink: (Int, Int) throws -> SampleSink) throws {
    try requirePermission()

    let queries = options.system ? options.excludes : options.apps
    func targets() throws -> [AudioProcess] {
        try AudioProcess.all().filter { p in queries.contains { p.matches($0) } }
    }
    let verb = options.system ? "excluding" : "tapping"
    var current = try targets()
    for p in current { stderr("\(verb) \(p.label)") }
    if !options.system && current.isEmpty {
        stderr("waiting for \(options.apps.joined(separator: ", ")) to use audio")
    }

    let tap = try ProcessTap(processes: current.map(\.object), system: options.system, mute: options.mute)
    defer { tap.destroy() }
    let sampleRate = Int(tap.sampleRate.rounded())
    let channels = tap.channels
    let sink = try makeSink(sampleRate, channels)

    // Follow the app: its helper processes come and go, and it may not have
    // started yet. The running tap is updated in place.
    let changes = DispatchQueue(label: "mat-capture.processes")
    var processList = address(kAudioHardwarePropertyProcessObjectList)
    let onChange: AudioObjectPropertyListenerBlock = { _, _ in
        guard let now = try? targets() else { return }
        let before = Set(current.map(\.object)), after = Set(now.map(\.object))
        guard before != after else { return }
        for p in now where !before.contains(p.object) { stderr("\(verb) \(p.label)") }
        for p in current where !after.contains(p.object) { stderr("stopped \(verb) \(p.label)") }
        do { try tap.setProcesses(now.map(\.object)) } catch { stderr("warning: \(error)") }
        current = now
    }
    AudioObjectAddPropertyListenerBlock(systemObject, &processList, changes, onChange)
    defer { AudioObjectRemovePropertyListenerBlock(systemObject, &processList, changes, onChange) }

    let ring = SampleRing(capacity: sampleRate * channels * 4)
    let stop = DispatchSemaphore(value: 0)
    let stopping = Flag()
    let finished = DispatchSemaphore(value: 0)
    var sinkFailed = false
    var samplesWritten = 0
    var peak: Float32 = 0

    signal(SIGINT, SIG_IGN)
    signal(SIGTERM, SIG_IGN)
    signal(SIGPIPE, SIG_IGN)
    let signals = [SIGINT, SIGTERM].map { DispatchSource.makeSignalSource(signal: $0, queue: .global()) }
    for source in signals {
        source.setEventHandler { stop.signal() }
        source.resume()
    }

    let limit = options.seconds.map { Int(($0 * Double(sampleRate)).rounded()) * channels }
    let showProgress = options.output != nil && isatty(STDERR_FILENO) != 0
    Thread {
        let chunk = 1 << 16
        let buffer = UnsafeMutablePointer<Float32>.allocate(capacity: chunk)
        defer { buffer.deallocate(); finished.signal() }
        var shownSecond = -1
        while true {
            let wanted = limit.map { min(chunk, $0 - samplesWritten) } ?? chunk
            let n = ring.read(into: buffer, max: wanted)
            if n == 0 {
                if stopping.isSet { break }
                usleep(5_000)
                continue
            }
            for i in 0..<n { peak = max(peak, abs(buffer[i])) }
            guard sink.write(buffer, count: n) else {
                sinkFailed = true
                stop.signal()
                break
            }
            samplesWritten += n
            if showProgress, samplesWritten / (sampleRate * channels) != shownSecond {
                shownSecond = samplesWritten / (sampleRate * channels)
                FileHandle.standardError.write(String(format: "\rrecording %d:%02d ", shownSecond / 60, shownSecond % 60).data(using: .utf8)!)
            }
            if let limit, samplesWritten >= limit {
                stop.signal()
                break
            }
        }
    }.start()

    try tap.start { samples, frames in ring.write(samples, count: frames * channels) }
    stderr("capturing at \(sampleRate) Hz, \(channels) channels\(options.mute ? ", muted" : "")\(options.seconds == nil ? " (Ctrl-C to stop)" : "")")

    withExtendedLifetime(signals) { stop.wait() }
    tap.destroy()
    stopping.set()
    finished.wait()
    try sink.finish()
    if showProgress { stderr("") }

    let seconds = Double(samplesWritten / channels) / Double(sampleRate)
    let peakDB = peak > 0 ? String(format: "%.1f dBFS", 20 * log10(Double(peak))) : "silence"
    stderr(String(format: "captured %.2f s, peak %@", seconds, peakDB) + (options.output.map { " -> \($0)" } ?? ""))
    if ring.dropped > 0 { stderr("warning: \(ring.dropped / channels) frames dropped because the output fell behind") }
    if peak == 0 && samplesWritten > 0 && AudioCapturePermission.status() != .granted {
        stderr("warning: only silence was captured; \(AudioCapturePermission.settingsHint)")
    }
    if sinkFailed && options.output != nil { throw Failure("writing \(options.output!) failed") }
}

// MARK: - Entry point

/// The options of `record` or `stream`; what is wrong with them is printed
/// with the usage, because that is what says how to write them.
func captureOptions(_ arguments: [String], wantsOutput: Bool) -> CaptureOptions {
    do {
        return try CaptureOptions(arguments, wantsOutput: wantsOutput)
    } catch {
        stderr("error: \(error)\n")
        stderr(usage)
        exit(2)
    }
}

let args = Array(CommandLine.arguments.dropFirst())
if args.contains(where: { ["--help", "-h", "help"].contains($0) }) {
    print(usage)
    exit(0)
}
if ["record", "stream", "permission"].contains(args.first), let status = relaunchAsResponsibleProcess() {
    exit(status)
}

do {
    switch args.first {
    case "list":
        try list()
    case "record":
        let options = captureOptions(Array(args.dropFirst()), wantsOutput: true)
        try capture(options) { rate, channels in try WavSink(path: options.output!, sampleRate: rate, channels: channels) }
    case "stream":
        let options = captureOptions(Array(args.dropFirst()), wantsOutput: false)
        try capture(options) { _, _ in StdoutSink() }
    case "permission":
        switch AudioCapturePermission.request() {
        case .granted: print("audio recording is allowed")
        case .unknown: print("could not check the permission; macOS will ask on the first recording")
        case .denied, .undetermined:
            print("audio recording is not allowed: \(AudioCapturePermission.settingsHint)")
            exit(1)
        }
    default:
        stderr(usage)
        exit(2)
    }
} catch {
    stderr("error: \(error)")
    exit(1)
}
