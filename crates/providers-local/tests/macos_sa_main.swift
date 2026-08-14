// Smoke test for the SpeechAnalyzer shim's C ABI. Compiled together with
// sa_shim.swift by scripts/test-swift-shim.sh (this file becomes main.swift).
// Feeds 2 s of silence through a real analyzer session and expects no crash
// and a clean flush. Silence may legitimately produce zero callbacks.

import Darwin
import Foundation

enum Counters {
    static var batches = 0
    static var finals = 0
}

let cb: SaWordsCB = { _, _, _, _, n, isFinal in
    Counters.batches += 1
    if isFinal { Counters.finals += 1 }
    precondition(n > 0, "callback must never fire with zero words")
}

if !sa_is_available() {
    print("SKIP sa_shim smoke: SpeechAnalyzer unavailable on this machine")
    exit(0)
}

// Exercise the asset FFI (informational — a machine without en-US assets is
// legitimate; install is NOT run here to keep the smoke test read-only).
print("assets en-US installed: \(sa_assets_installed("en-US"))")

var err = [CChar](repeating: 0, count: 256)
guard let h = sa_create("en-US", cb, nil, &err) else {
    print("FAIL sa_create: \(String(cString: err))")
    exit(1)
}

let half = [Float](repeating: 0, count: 8_000) // 0.5 s @ 16 kHz
for _ in 0..<4 {
    let fed = sa_feed(h, half, Int32(half.count))
    precondition(fed >= 0, "sa_feed rejected a silence buffer")
}
sa_flush(h)
sa_destroy(h)
print("PASS sa_shim: 2s silence fed, flushed clean (batches=\(Counters.batches), finals=\(Counters.finals))")
