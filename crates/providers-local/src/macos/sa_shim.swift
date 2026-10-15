// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// SpeechAnalyzer shim: a C ABI over the macOS 26 SpeechAnalyzer/
// SpeechTranscriber volatile→finalized result stream, consumed by
// `macos/mod.rs`. Compiled by build.rs (swiftc, static lib) on macOS only.
//
// ABI (all callbacks fire on an internal task; the Rust side forwards into
// channels and never blocks the callback). Word batches cross as parallel
// arrays:
//
//   typedef void (*sa_words_cb)(void *ctx, const char *const *texts,
//                               const double *start_s, const double *end_s,
//                               int32_t n, bool is_final);
//   bool  sa_is_available(void);
//   bool  sa_assets_installed(const char *locale_bcp47);  // "" = system locale
//   bool  sa_assets_install(const char *locale_bcp47, char err[256]);
//                              // blocking OS download; no byte progress exists
//   void *sa_create(const char *locale_bcp47, bool fast, sa_words_cb cb,
//                   void *ctx, char err[256]);  // NULL on failure, err filled;
//                              // fast: sooner results, some accuracy lost
//   int32_t sa_feed(void *h, const float *samples_16k_mono, int32_t n);
//   void  sa_flush(void *h);   // end input + finalize; final callbacks fire
//                              // before this returns
//   void  sa_destroy(void *h);
//
// Deployment target is macOS 15; every Speech API here is
// #available-guarded and `sa_is_available` reports availability at runtime.

import AVFAudio
import Darwin
import Foundation
import Speech

public typealias SaWordsCB = @convention(c) (
    UnsafeMutableRawPointer?,               // ctx
    UnsafePointer<UnsafePointer<CChar>?>?,  // texts[n]
    UnsafePointer<Double>?,                 // start_s[n]
    UnsafePointer<Double>?,                 // end_s[n]
    Int32,                                  // n
    Bool                                    // is_final
) -> Void

/// Block the (non-Swift-concurrency) calling thread on an async operation.
/// Callers are Rust worker threads, never the cooperative pool.
private func blockOn<T>(_ op: @escaping @Sendable () async -> T) -> T {
    let sem = DispatchSemaphore(value: 0)
    let box = UnsafeMutablePointer<T?>.allocate(capacity: 1)
    box.initialize(to: nil)
    defer { box.deinitialize(count: 1); box.deallocate() }
    Task.detached {
        box.pointee = await op()
        sem.signal()
    }
    sem.wait()
    return box.pointee!
}

private func fillErr(_ err: UnsafeMutablePointer<CChar>?, _ msg: String) {
    guard let err else { return }
    let bytes = Array(msg.utf8.prefix(255)) + [0]
    bytes.withUnsafeBufferPointer { src in
        err.update(from: src.baseAddress!.withMemoryRebound(to: CChar.self, capacity: bytes.count) { $0 },
                   count: bytes.count)
    }
}

@available(macOS 26.0, *)
private final class SaSessionImpl {
    private let transcriber: SpeechTranscriber
    private let analyzer: SpeechAnalyzer
    private let cont: AsyncStream<AnalyzerInput>.Continuation
    private let srcFmt: AVAudioFormat
    private let converter: AVAudioConverter?
    private let targetFmt: AVAudioFormat
    private let resultsDone = DispatchSemaphore(value: 0)
    private var flushed = false
    private var fedAny = false

    init(locale: String, fast: Bool, cb: @escaping SaWordsCB, ctx: UnsafeMutableRawPointer?) throws {
        let loc = locale.isEmpty ? Locale.current : Locale(identifier: locale)
        transcriber = SpeechTranscriber(
            locale: loc,
            transcriptionOptions: [],
            reportingOptions: fast ? [.volatileResults, .fastResults] : [.volatileResults],
            attributeOptions: [.audioTimeRange])
        analyzer = SpeechAnalyzer(modules: [transcriber])

        guard let src = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)
        else { throw SaError("make source format") }
        srcFmt = src
        let t = transcriber
        targetFmt = blockOn { await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [t]) } ?? src
        converter = srcFmt == targetFmt ? nil : AVAudioConverter(from: srcFmt, to: targetFmt)
        if srcFmt != targetFmt && converter == nil { throw SaError("make converter") }

        let (stream, c) = AsyncStream.makeStream(of: AnalyzerInput.self)
        cont = c

        // Results pump: every transcriber result → one callback batch. The
        // C strings live only for the duration of the callback.
        let done = resultsDone
        Task.detached { [transcriber] in
            do {
                for try await result in transcriber.results {
                    Self.emit(result, cb: cb, ctx: ctx)
                }
            } catch {
                fputs("sa_shim: results stream error: \(error)\n", stderr)
            }
            done.signal()
        }

        let a = analyzer
        let startErr: String? = blockOn {
            do {
                try await a.start(inputSequence: stream)
                return nil
            } catch {
                return "\(error)"
            }
        }
        if let startErr { throw SaError("analyzer start: \(startErr)") }
    }

    private static func emit(
        _ result: SpeechTranscriber.Result, cb: SaWordsCB, ctx: UnsafeMutableRawPointer?
    ) {
        var texts: [UnsafePointer<CChar>?] = []
        var starts: [Double] = []
        var ends: [Double] = []
        var storage: [UnsafeMutablePointer<CChar>] = []
        defer { storage.forEach { free($0) } }
        for run in result.text.runs {
            guard let tr = run.audioTimeRange else { continue }
            let text = String(result.text[run.range].characters)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if text.isEmpty { continue }
            guard let c = strdup(text) else { continue }
            storage.append(c)
            texts.append(UnsafePointer(c))
            starts.append(tr.start.seconds)
            ends.append(tr.end.seconds)
        }
        if texts.isEmpty { return }
        texts.withUnsafeBufferPointer { tp in
            starts.withUnsafeBufferPointer { sp in
                ends.withUnsafeBufferPointer { ep in
                    cb(ctx, tp.baseAddress, sp.baseAddress, ep.baseAddress,
                       Int32(tp.count), result.isFinal)
                }
            }
        }
    }

    func feed(_ samples: UnsafePointer<Float>, _ n: Int32) -> Int32 {
        guard n > 0, !flushed else { return 0 }
        let frames = AVAudioFrameCount(n)
        guard let src = AVAudioPCMBuffer(pcmFormat: srcFmt, frameCapacity: frames) else { return -1 }
        src.frameLength = frames
        src.floatChannelData![0].update(from: samples, count: Int(n))
        var out = src
        if let converter {
            let cap = AVAudioFrameCount(Double(n) * targetFmt.sampleRate / srcFmt.sampleRate) + 16
            guard let dst = AVAudioPCMBuffer(pcmFormat: targetFmt, frameCapacity: cap) else { return -1 }
            var fed = false
            var convErr: NSError?
            let st = converter.convert(to: dst, error: &convErr) { _, outStatus in
                if fed { outStatus.pointee = .noDataNow; return nil }
                fed = true
                outStatus.pointee = .haveData
                return src
            }
            if st == .error { return -1 }
            out = dst
        }
        cont.yield(AnalyzerInput(buffer: out))
        fedAny = true
        return n
    }

    /// End input and finalize; final-result callbacks have fired by return.
    /// A session that was never fed audio cancels instead.
    func flush() {
        if flushed { return }
        flushed = true
        cont.finish()
        if !fedAny {
            cancel()
            return
        }
        let a = analyzer
        _ = blockOn {
            do {
                try await a.finalizeAndFinishThroughEndOfInput()
            } catch {
                fputs("sa_shim: finalize: \(error)\n", stderr)
            }
            return true
        }
        awaitResults()
    }

    func destroy() {
        if !flushed {
            flushed = true
            cont.finish()
            cancel()
        }
    }

    private func cancel() {
        let a = analyzer
        _ = blockOn { await a.cancelAndFinishNow(); return true }
        awaitResults()
    }

    /// Wait for the results pump to drain, bounded.
    private func awaitResults() {
        if resultsDone.wait(timeout: .now() + RESULTS_DRAIN_SECONDS) == .timedOut {
            fputs("sa_shim: results pump did not drain in \(RESULTS_DRAIN_SECONDS)s\n", stderr)
        }
    }
}

/// Ceiling on draining the results pump after input ends.
private let RESULTS_DRAIN_SECONDS: Double = 5.0

private struct SaError: Error, CustomStringConvertible {
    let description: String
    init(_ d: String) { description = d }
}

/// Availability = the OS can transcribe on this machine (macOS 26+ API
/// present AND at least one supported locale). Cached after first call.
@_cdecl("sa_is_available")
public func sa_is_available() -> Bool {
    // The lock keeps the lazy probe single-flight.
    enum Cache {
        static let lock = NSLock()
        static var value: Bool?
    }
    Cache.lock.lock()
    defer { Cache.lock.unlock() }
    if let v = Cache.value { return v }
    var avail = false
    if #available(macOS 26.0, *) {
        avail = blockOn { !(await SpeechTranscriber.supportedLocales.isEmpty) }
    }
    Cache.value = avail
    return avail
}

@available(macOS 26.0, *)
private func resolveLocale(_ locale: UnsafePointer<CChar>?) -> Locale {
    let s = locale.map { String(cString: $0) } ?? ""
    return s.isEmpty ? Locale.current : Locale(identifier: s)
}

/// Speech assets for the locale are on disk (`""` = system locale). Distinct
/// from capability: a supported-but-not-downloaded locale returns false.
@_cdecl("sa_assets_installed")
public func sa_assets_installed(_ locale: UnsafePointer<CChar>?) -> Bool {
    guard #available(macOS 26.0, *) else { return false }
    let l = resolveLocale(locale)
    return blockOn {
        let installed = await SpeechTranscriber.installedLocales
        return installed.contains { $0.identifier(.bcp47) == l.identifier(.bcp47) }
    }
}

/// Blocking OS asset download for the locale. Requesting also reserves the
/// locale in the OS inventory.
@_cdecl("sa_assets_install")
public func sa_assets_install(
    _ locale: UnsafePointer<CChar>?, _ err: UnsafeMutablePointer<CChar>?
) -> Bool {
    guard #available(macOS 26.0, *) else {
        fillErr(err, "SpeechAnalyzer needs macOS 26+")
        return false
    }
    let l = resolveLocale(locale)
    let failure: String? = blockOn {
        do {
            let t = SpeechTranscriber(
                locale: l, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
            if let req = try await AssetInventory.assetInstallationRequest(supporting: [t]) {
                try await req.downloadAndInstall()
            }
            return nil
        } catch {
            return "\(error)"
        }
    }
    if let failure {
        fillErr(err, failure)
        return false
    }
    return true
}

@_cdecl("sa_create")
public func sa_create(
    _ locale: UnsafePointer<CChar>?,
    _ fast: Bool,
    _ cb: SaWordsCB?,
    _ ctx: UnsafeMutableRawPointer?,
    _ err: UnsafeMutablePointer<CChar>?
) -> UnsafeMutableRawPointer? {
    guard let cb else {
        fillErr(err, "null callback")
        return nil
    }
    guard #available(macOS 26.0, *) else {
        fillErr(err, "SpeechAnalyzer needs macOS 26+")
        return nil
    }
    let loc = locale.map { String(cString: $0) } ?? ""
    do {
        let s = try SaSessionImpl(locale: loc, fast: fast, cb: cb, ctx: ctx)
        return Unmanaged.passRetained(s).toOpaque()
    } catch {
        fillErr(err, "\(error)")
        return nil
    }
}

@_cdecl("sa_feed")
public func sa_feed(
    _ h: UnsafeMutableRawPointer?, _ samples: UnsafePointer<Float>?, _ n: Int32
) -> Int32 {
    guard let h, let samples, #available(macOS 26.0, *) else { return -1 }
    let s = Unmanaged<SaSessionImpl>.fromOpaque(h).takeUnretainedValue()
    return s.feed(samples, n)
}

@_cdecl("sa_flush")
public func sa_flush(_ h: UnsafeMutableRawPointer?) {
    guard let h, #available(macOS 26.0, *) else { return }
    Unmanaged<SaSessionImpl>.fromOpaque(h).takeUnretainedValue().flush()
}

@_cdecl("sa_destroy")
public func sa_destroy(_ h: UnsafeMutableRawPointer?) {
    guard let h, #available(macOS 26.0, *) else { return }
    let s = Unmanaged<SaSessionImpl>.fromOpaque(h).takeRetainedValue()
    s.destroy()
}
