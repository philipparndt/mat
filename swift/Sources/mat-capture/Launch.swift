// Getting macOS to attribute audio capture to MatCapture.app instead of the terminal.

import Darwin
import Foundation

private let rtldDefault = UnsafeMutableRawPointer(bitPattern: -2)

/// Started from a shell, this process inherits the terminal as its
/// "responsible process", and macOS would ask the terminal for the audio
/// recording permission (and may want it relaunched afterwards). When the
/// executable lives in an app bundle, it runs itself again as its own
/// responsible process, so the permission belongs to MatCapture.app and
/// works immediately. Returns the child's exit status, or nil when it should
/// just carry on in this process.
func relaunchAsResponsibleProcess() -> Int32? {
    guard getenv("MAT_CAPTURE_RELAUNCHED") == nil else { return nil }
    guard let executable = executablePath(), isInAppBundle(executable) else {
        stderr("note: not running from MatCapture.app, so macOS asks your terminal for audio recording permission (build the app with swift/bundle-capture.sh)")
        return nil
    }
    typealias Disclaim = @convention(c) (UnsafeMutablePointer<posix_spawnattr_t?>, Int32) -> Int32
    guard let symbol = dlsym(rtldDefault, "responsibility_spawnattrs_setdisclaim") else { return nil }
    let disclaim = unsafeBitCast(symbol, to: Disclaim.self)

    var attributes: posix_spawnattr_t?
    posix_spawnattr_init(&attributes)
    defer { posix_spawnattr_destroy(&attributes) }
    _ = disclaim(&attributes, 1)
    var defaults = sigset_t()
    sigemptyset(&defaults)
    for s in [SIGINT, SIGTERM, SIGPIPE] { sigaddset(&defaults, s) }
    posix_spawnattr_setsigdefault(&attributes, &defaults)
    posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETSIGDEF))

    var environment = ProcessInfo.processInfo.environment
    environment["MAT_CAPTURE_RELAUNCHED"] = "1"
    let argv = CommandLine.arguments.map { strdup($0) } + [nil]
    let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
    defer { (argv + envp).forEach { free($0) } }

    // Ctrl-C reaches the child through the process group; SIGTERM is passed on.
    signal(SIGINT, SIG_IGN)
    signal(SIGTERM, SIG_IGN)
    var child: pid_t = 0
    let status = posix_spawn(&child, executable, nil, &attributes, argv, envp)
    guard status == 0 else {
        stderr("error: relaunching \(executable) failed: \(String(cString: strerror(status)))")
        return 1
    }
    let term = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
    term.setEventHandler { kill(child, SIGTERM) }
    term.resume()

    var result: Int32 = 0
    // The source must outlive the wait: Swift may otherwise free it right after resume().
    withExtendedLifetime(term) {
        while waitpid(child, &result, 0) < 0 && errno == EINTR {}
    }
    let signal = result & 0x7f
    return signal == 0 ? (result >> 8) & 0xff : 128 + signal
}

private func executablePath() -> String? {
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(getpid(), &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return String(cString: buffer)
}

private func isInAppBundle(_ executable: String) -> Bool {
    let parts = URL(fileURLWithPath: executable).pathComponents
    return parts.count >= 4 && parts[parts.count - 2] == "MacOS" && parts[parts.count - 3] == "Contents"
        && parts[parts.count - 4].hasSuffix(".app")
}

// MARK: - Permission

enum Permission {
    case granted, denied, undetermined, unknown
}

/// Audio capture permission through the private TCC framework, the only way
/// to ask before recording: without it a tap silently delivers zeros.
enum AudioCapturePermission {
    private static let tcc = dlopen("/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC", RTLD_NOW)
    private static let service = "kTCCServiceAudioCapture" as CFString

    static func status() -> Permission {
        typealias Preflight = @convention(c) (CFString, CFDictionary?) -> Int
        guard let tcc, let symbol = dlsym(tcc, "TCCAccessPreflight") else { return .unknown }
        switch unsafeBitCast(symbol, to: Preflight.self)(service, nil) {
        case 0: return .granted
        case 1: return .denied
        default: return .undetermined
        }
    }

    /// Shows the system prompt if the user has not decided yet.
    static func request() -> Permission {
        typealias Request = @convention(c) (CFString, CFDictionary?, @escaping @convention(block) (Bool) -> Void) -> Void
        guard status() == .undetermined, let tcc, let symbol = dlsym(tcc, "TCCAccessRequest") else { return status() }
        let answered = DispatchSemaphore(value: 0)
        var granted = false
        unsafeBitCast(symbol, to: Request.self)(service, nil) { granted = $0; answered.signal() }
        answered.wait()
        return granted ? .granted : .denied
    }

    static let settingsHint = "allow it in System Settings > Privacy & Security > Screen & System Audio Recording (System Audio Recording Only)"
}
