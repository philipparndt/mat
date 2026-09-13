// mat-au: renders the Audio Unit tracks of an exported timeline to dry stems.
//
//   mat-au list
//   mat-au render <timeline.json> --out <dir> [--sample-rate 48000]
//
// Each Audio Unit track is written to <dir>/<track-index>.wav (32-bit float,
// stereo). Mixing and effects stay in the Rust engine.

import AVFoundation
import Foundation

// MARK: - Timeline (subset of `mat export` JSON)

struct Timeline: Decodable {
    let end: Double
    let tracks: [Track]
}

struct Track: Decodable {
    let name: String
    let instrument: Instrument
    let notes: [Note]
}

struct Instrument: Decodable {
    let type: String
    let component: [String]?
    let load: String?
    let program: UInt8?
}

struct Note: Decodable {
    let start: Double
    let duration: Double
    let midi: Float
    let velocity: Float
}

struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

// MARK: - Helpers

func fourCC(_ s: String) -> OSType {
    s.utf8.reduce(0) { ($0 << 8) | OSType($1) }
}

func fourCCString(_ code: OSType) -> String {
    let bytes = [24, 16, 8, 0].map { UInt8((code >> $0) & 0xFF) }
    return String(bytes: bytes, encoding: .macOSRoman) ?? "????"
}

func stderr(_ message: String) {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
}

// MARK: - Commands

func listInstruments() {
    let any = AudioComponentDescription(
        componentType: kAudioUnitType_MusicDevice, componentSubType: 0,
        componentManufacturer: 0, componentFlags: 0, componentFlagsMask: 0)
    let components = AVAudioUnitComponentManager.shared().components(matching: any)
    for c in components.sorted(by: { $0.manufacturerName + $0.name < $1.manufacturerName + $1.name }) {
        let d = c.audioComponentDescription
        print("\(fourCCString(d.componentType)) \(fourCCString(d.componentSubType)) \(fourCCString(d.componentManufacturer))  \(c.manufacturerName): \(c.name)")
    }
}

func makeInstrument(_ inst: Instrument) async throws -> AVAudioUnit {
    let codes = inst.component ?? ["aumu", "samp", "appl"]
    guard codes.count == 3 else { throw Failure("component needs three codes") }
    if codes == ["aumu", "samp", "appl"] {
        return AVAudioUnitSampler()
    }
    let desc = AudioComponentDescription(
        componentType: fourCC(codes[0]), componentSubType: fourCC(codes[1]),
        componentManufacturer: fourCC(codes[2]), componentFlags: 0, componentFlagsMask: 0)
    return try await AVAudioUnit.instantiate(with: desc, options: [])
}

func loadContent(_ inst: Instrument, into unit: AVAudioUnit) throws {
    guard let path = inst.load else { return }
    let url = URL(fileURLWithPath: path)
    guard FileManager.default.fileExists(atPath: path) else { throw Failure("file not found: \(path)") }
    let ext = url.pathExtension.lowercased()

    if let sampler = unit as? AVAudioUnitSampler {
        if ext == "sf2" || ext == "dls" {
            try sampler.loadSoundBankInstrument(
                at: url, program: inst.program ?? 0,
                bankMSB: UInt8(kAUSampler_DefaultMelodicBankMSB), bankLSB: UInt8(kAUSampler_DefaultBankLSB))
        } else {
            try sampler.loadInstrument(at: url)
        }
    } else if ext == "aupreset" {
        let data = try Data(contentsOf: url)
        let state = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any]
        unit.auAudioUnit.fullState = state
    } else {
        throw Failure("don't know how to load .\(ext) into \(unit.name)")
    }
}

struct MidiEvent {
    let sample: Int64
    let bytes: [UInt8]
}

func renderStem(track: Track, end: Double, sampleRate: Double, to url: URL) async throws {
    let engine = AVAudioEngine()
    let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 2)!
    let unit = try await makeInstrument(track.instrument)
    engine.attach(unit)
    engine.connect(unit, to: engine.mainMixerNode, format: format)
    try loadContent(track.instrument, into: unit)

    let maxFrames: AVAudioFrameCount = 512
    try engine.enableManualRenderingMode(.offline, format: format, maximumFrameCount: maxFrames)
    try engine.start()

    if let program = track.instrument.program, !(unit is AVAudioUnitSampler) {
        (unit as? AVAudioUnitMIDIInstrument)?.sendProgramChange(program, onChannel: 0)
    }

    var events: [MidiEvent] = []
    for note in track.notes {
        let key = UInt8(max(0, min(127, note.midi.rounded())))
        let velocity = UInt8(max(1, min(127, (note.velocity * 127).rounded())))
        let on = Int64((note.start * sampleRate).rounded())
        let off = Int64(((note.start + note.duration) * sampleRate).rounded())
        events.append(MidiEvent(sample: on, bytes: [0x90, key, velocity]))
        events.append(MidiEvent(sample: max(on + 1, off), bytes: [0x80, key, 0]))
    }
    // Note-offs first when events coincide, so repeated notes retrigger.
    events.sort { $0.sample != $1.sample ? $0.sample < $1.sample : $0.bytes[0] < $1.bytes[0] }

    guard let schedule = unit.auAudioUnit.scheduleMIDIEventBlock else {
        throw Failure("\(unit.name) does not accept scheduled MIDI events")
    }

    let settings: [String: Any] = [
        AVFormatIDKey: kAudioFormatLinearPCM,
        AVSampleRateKey: sampleRate,
        AVNumberOfChannelsKey: 2,
        AVLinearPCMBitDepthKey: 32,
        AVLinearPCMIsFloatKey: true,
        AVLinearPCMIsNonInterleaved: false,
    ]
    let file = try AVAudioFile(forWriting: url, settings: settings, commonFormat: .pcmFormatFloat32, interleaved: false)
    let buffer = AVAudioPCMBuffer(pcmFormat: engine.manualRenderingFormat, frameCapacity: maxFrames)!

    let tail = 4.0
    let total = Int64((end + tail) * sampleRate)
    var position: Int64 = 0
    var next = 0
    while position < total {
        let frames = AVAudioFrameCount(min(Int64(maxFrames), total - position))
        let chunkEnd = position + Int64(frames)
        while next < events.count && events[next].sample < chunkEnd {
            let offset = max(0, events[next].sample - position)
            events[next].bytes.withUnsafeBufferPointer { bytes in
                schedule(AUEventSampleTimeImmediate + offset, 0, 3, bytes.baseAddress!)
            }
            next += 1
        }
        let status = try engine.renderOffline(frames, to: buffer)
        guard status == .success else { throw Failure("offline render failed (status \(status.rawValue))") }
        try file.write(from: buffer)
        position = chunkEnd
    }
    engine.stop()
}

func render(arguments: [String]) async throws {
    var input: String?
    var outDir: String?
    var sampleRate = 48_000.0
    var it = arguments.makeIterator()
    while let arg = it.next() {
        switch arg {
        case "--out": outDir = it.next()
        case "--sample-rate": sampleRate = Double(it.next() ?? "") ?? sampleRate
        default: input = arg
        }
    }
    guard let input, let outDir else { throw Failure("usage: mat-au render <timeline.json> --out <dir>") }

    let data = try Data(contentsOf: URL(fileURLWithPath: input))
    let timeline = try JSONDecoder().decode(Timeline.self, from: data)
    try FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)

    var failures = 0
    for (index, track) in timeline.tracks.enumerated() where track.instrument.type == "au" {
        let started = Date()
        let url = URL(fileURLWithPath: outDir).appendingPathComponent("\(index).wav")
        let name = track.instrument.load.map { URL(fileURLWithPath: $0).deletingPathExtension().lastPathComponent } ?? "Audio Unit"
        do {
            try await renderStem(track: track, end: timeline.end, sampleRate: sampleRate, to: url)
            print(String(format: "  %@ (%@): %d notes in %.2fs", track.name, name, track.notes.count, Date().timeIntervalSince(started)))
        } catch {
            try? FileManager.default.removeItem(at: url)
            stderr("  \(track.name) (\(name)): failed: \(error)")
            failures += 1
        }
    }
    if failures > 0 { exit(1) }
}

// MARK: - Entry point

let args = Array(CommandLine.arguments.dropFirst())
do {
    switch args.first {
    case "list": listInstruments()
    case "render": try await render(arguments: Array(args.dropFirst()))
    default:
        stderr("usage:\n  mat-au list\n  mat-au render <timeline.json> --out <dir> [--sample-rate 48000]")
        exit(2)
    }
} catch {
    stderr("error: \(error)")
    exit(1)
}
