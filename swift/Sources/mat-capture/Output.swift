// Moving audio off the real-time thread, and writing it out.

import Foundation
import os

/// Single-producer, single-consumer FIFO of interleaved samples. The IO thread
/// writes, the writer thread reads; the lock only guards the two counters.
final class SampleRing {
    private let capacity: Int
    private let storage: UnsafeMutablePointer<Float32>
    private let lock = UnsafeMutablePointer<os_unfair_lock>.allocate(capacity: 1)
    private var written = 0
    private var consumed = 0
    /// Samples lost because the reader fell behind. Touched by the producer only.
    private(set) var dropped = 0

    init(capacity: Int) {
        self.capacity = capacity
        storage = .allocate(capacity: capacity)
        lock.initialize(to: os_unfair_lock())
    }

    deinit {
        storage.deallocate()
        lock.deallocate()
    }

    private func counters() -> (written: Int, consumed: Int) {
        os_unfair_lock_lock(lock)
        defer { os_unfair_lock_unlock(lock) }
        return (written, consumed)
    }

    func write(_ samples: UnsafePointer<Float32>, count: Int) {
        let (w, r) = counters()
        let n = min(count, capacity - (w - r))
        dropped += count - n
        for i in 0..<n { storage[(w + i) % capacity] = samples[i] }
        os_unfair_lock_lock(lock)
        written = w + n
        os_unfair_lock_unlock(lock)
    }

    func read(into destination: UnsafeMutablePointer<Float32>, max: Int) -> Int {
        let (w, r) = counters()
        let n = min(max, w - r)
        for i in 0..<n { destination[i] = storage[(r + i) % capacity] }
        os_unfair_lock_lock(lock)
        consumed = r + n
        os_unfair_lock_unlock(lock)
        return n
    }
}

protocol SampleSink {
    /// Returns false when the sink cannot take more (e.g. the reader of stdout went away).
    func write(_ samples: UnsafePointer<Float32>, count: Int) -> Bool
    func finish() throws
}

/// 32-bit float WAV. The header is rewritten every second, so an interrupted
/// recording is still a valid file up to the last second.
final class WavSink: SampleSink {
    private let fd: Int32
    private let sampleRate: Int
    private let channels: Int
    private var dataBytes = 0
    private var lastHeader = 0

    init(path: String, sampleRate: Int, channels: Int) throws {
        fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
        guard fd >= 0 else { throw Failure("cannot write \(path): \(String(cString: strerror(errno)))") }
        self.sampleRate = sampleRate
        self.channels = channels
        writeHeader()
    }

    private func writeHeader() {
        var h = Data()
        func u32(_ v: Int) { withUnsafeBytes(of: UInt32(v).littleEndian) { h.append(contentsOf: $0) } }
        func u16(_ v: Int) { withUnsafeBytes(of: UInt16(v).littleEndian) { h.append(contentsOf: $0) } }
        h.append(contentsOf: Array("RIFF".utf8)); u32(36 + dataBytes)
        h.append(contentsOf: Array("WAVE".utf8))
        h.append(contentsOf: Array("fmt ".utf8)); u32(16)
        u16(3) // WAVE_FORMAT_IEEE_FLOAT
        u16(channels); u32(sampleRate); u32(sampleRate * channels * 4); u16(channels * 4); u16(32)
        h.append(contentsOf: Array("data".utf8)); u32(dataBytes)
        h.withUnsafeBytes { _ = pwrite(fd, $0.baseAddress, h.count, 0) }
        lastHeader = dataBytes
    }

    func write(_ samples: UnsafePointer<Float32>, count: Int) -> Bool {
        let bytes = count * MemoryLayout<Float32>.size
        guard writeAll(fd, UnsafeRawPointer(samples), bytes, at: 44 + dataBytes) else { return false }
        dataBytes += bytes
        if dataBytes - lastHeader >= sampleRate * channels * 4 { writeHeader() }
        return true
    }

    func finish() throws {
        writeHeader()
        guard close(fd) == 0 else { throw Failure("closing the WAV file failed: \(String(cString: strerror(errno)))") }
    }
}

/// Raw interleaved little-endian Float32 on stdout.
final class StdoutSink: SampleSink {
    func write(_ samples: UnsafePointer<Float32>, count: Int) -> Bool {
        writeAll(STDOUT_FILENO, UnsafeRawPointer(samples), count * MemoryLayout<Float32>.size, at: nil)
    }

    func finish() throws {}
}

private func writeAll(_ fd: Int32, _ bytes: UnsafeRawPointer, _ count: Int, at offset: Int?) -> Bool {
    var done = 0
    while done < count {
        let n = offset.map { pwrite(fd, bytes + done, count - done, off_t($0 + done)) }
            ?? Darwin.write(fd, bytes + done, count - done)
        if n < 0 {
            if errno == EINTR { continue }
            return false
        }
        done += n
    }
    return true
}
