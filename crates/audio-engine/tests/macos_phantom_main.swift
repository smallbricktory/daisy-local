// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Phantom-input bookkeeping tests for the macOS shim.
//
// Compiled together with shim.swift by scripts/test-swift-shim.sh and run on a
// Mac. Pure state only — no device is opened, so no microphone grant is needed
// and this runs fine over SSH.

import Foundation

var failures = 0

func check(_ condition: Bool, _ what: String) {
    if condition {
        print("  ok   \(what)")
    } else {
        print("  FAIL \(what)")
        failures += 1
    }
}

// A device that is input-capable in its own right — a USB headset with a mic —
// is never treated as a phantom, however often it is pinned as an output.
func testRealInputDeviceIsNeverPhantom() {
    print("real input device is never memoized as a phantom")
    let uid = "real-mic-out"
    check(memoInputCapable(uid: uid, live: true), "first observation reports input-capable")
    check(memoInputCapable(uid: uid, live: true), "stays input-capable on a later start")
}

// The memo answers from the first observation. Once one of our units has pinned
// the device, CoreAudio reports an input scope that the unit grafted, and that
// later `live` reading must not overwrite what the device really was.
func testMemoIgnoresContaminatedLaterReading() {
    print("memo ignores the input scope our own unit grafted")
    let uid = "speakers-memo"
    check(!memoInputCapable(uid: uid, live: false), "first observation: not input-capable")
    check(!memoInputCapable(uid: uid, live: true), "later contaminated reading is ignored")
}

// Two meter starts, each followed by the stopped unit being held undisposed.
// The held unit keeps the pinned output's input scope open, so the hold count
// must never reach zero across the handover.
func testHoldSurvivesTwoHandovers() {
    print("hold survives back-to-back held-unit handovers")
    let uid = "speakers-handover"

    // Start A. Nothing has pinned the device yet.
    let aCapable = memoInputCapable(uid: uid, live: false)
    check(!aCapable, "start A sees a plain output device")
    if !aCapable { retainPhantomInput(uid) }
    check(phantomHoldCount(uid) == 1, "start A holds the output")

    // Stop A: the unit goes into the hold slot and takes the phantom with it.
    var heldPhantom = uid
    check(phantomHoldCount(uid) == 1, "holding A keeps the output held")

    // Start B. Unit A is still undisposed, so CoreAudio now reports an input
    // scope on the device — the reading the memo has to override.
    let bCapable = memoInputCapable(uid: uid, live: true)
    check(!bCapable, "start B is not fooled by A's grafted input scope")
    if !bCapable { retainPhantomInput(uid) }
    check(phantomHoldCount(uid) == 2, "start B holds it alongside held A")

    // Stop B: B becomes the held unit, A is disposed and its hold released.
    let previousPhantom = heldPhantom
    heldPhantom = uid
    if !previousPhantom.isEmpty { releasePhantomInput(previousPhantom) }
    check(
        phantomHoldCount(uid) == 1,
        "output is still held after A is disposed (0 here = speakers appear as a mic)"
    )
    check(!heldPhantom.isEmpty, "the held unit carries a phantom to release later")
}

// A start that fails after pinning tears down and must give its hold back.
func testFailedStartReleasesItsHold() {
    print("a torn-down start releases its own hold")
    let uid = "speakers-teardown"
    if !memoInputCapable(uid: uid, live: false) { retainPhantomInput(uid) }
    check(phantomHoldCount(uid) == 1, "start holds the output")
    releasePhantomInput(uid)
    check(phantomHoldCount(uid) == 0, "teardown releases it")
}

// Releasing a device nothing holds must not underflow into a negative count.
func testReleaseWithoutHoldIsHarmless() {
    print("release without a hold is harmless")
    let uid = "never-held"
    releasePhantomInput(uid)
    check(phantomHoldCount(uid) == 0, "count stays at zero")
}

testRealInputDeviceIsNeverPhantom()
testMemoIgnoresContaminatedLaterReading()
testHoldSurvivesTwoHandovers()
testFailedStartReleasesItsHold()
testReleaseWithoutHoldIsHarmless()

if failures == 0 {
    print("swift shim tests: all passed")
    exit(0)
} else {
    print("swift shim tests: \(failures) FAILED")
    exit(1)
}
