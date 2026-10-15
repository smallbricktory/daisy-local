// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// ATT answers of the Bluetooth peripheral shim: how challenge writes and
// address reads are answered for each callback result.
//
// Compiled together with shim.swift by scripts/test-swift-shim.sh and run on a
// Mac. Pure state only; no radio is started.

import CoreBluetooth
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

/// What the fake callbacks keep, by central.
var held: [String: Data] = [:]

let fakeChallenge: ChallengeFn = { _, service, central, value, len in
    guard let service, let central else { return 3 }
    guard String(cString: service) == "SERVICE" else { return 3 }
    guard len == 16, let value else { return 1 }
    held[String(cString: central)] = Data(bytes: value, count: Int(len))
    return 0
}

let fakeAddress: AddressFn = { _, service, central, out, cap in
    guard let service, let central, let out else { return -3 }
    guard String(cString: service) == "SERVICE" else { return -2 }
    guard let value = held[String(cString: central)] else { return -1 }
    guard value.count <= Int(cap) else { return -3 }
    value.copyBytes(to: out, count: value.count)
    return Int32(value.count)
}

func testReadAnswers() {
    print("an address read is answered from the callback, never with an auth error")
    let bytes = Data((0..<40).map { UInt8($0) })
    let (noChallenge, none) = readReply(-1, Data(), offset: 0)
    check(noChallenge == .readNotPermitted, "no challenge held: Read Not Permitted")
    check(none == nil, "and no value")
    check(readReply(-2, Data(), offset: 0).0 == .attributeNotFound, "unknown service: Attribute Not Found")
    check(readReply(-3, Data(), offset: 0).0 == .unlikelyError, "failed: Unlikely Error")
    for answer: Int32 in [-1, -2, -3, -7] {
        let code = readReply(answer, Data(), offset: 0).0
        check(code != .insufficientAuthentication && code != .insufficientEncryption
              && code != .insufficientAuthorization && code != .insufficientEncryptionKeySize,
              "answer \(answer) never asks the central to bond")
    }
    let (ok, whole) = readReply(40, bytes, offset: 0)
    check(ok == .success && whole == bytes, "offset 0 reads the whole value")
    let (tail, rest) = readReply(40, bytes, offset: 22)
    check(tail == .success && rest == bytes.subdata(in: 22..<40), "a long read continues at its offset")
    let (end, empty) = readReply(40, bytes, offset: 40)
    check(end == .success && empty == Data(), "an offset at the end reads nothing")
    check(readReply(40, bytes, offset: 41).0 == .invalidOffset, "an offset past the end: Invalid Offset")
}

func testWriteAnswers() {
    print("a challenge write is answered from the callback")
    check(writeResult(0) == .success, "kept: success")
    check(writeResult(1) == .invalidAttributeValueLength, "wrong length: Invalid Attribute Value Length")
    check(writeResult(2) == .unlikelyError, "failed: Unlikely Error")
    check(writeResult(3) == .attributeNotFound, "unknown service: Attribute Not Found")
}

func testChallengeValue() {
    print("a challenge is one write at offset 0")
    let v = Data(repeating: 7, count: 16)
    check(challengeValue([(offset: 0, value: v)]) == v, "one request at offset 0")
    check(challengeValue([(offset: 0, value: nil)]) == Data(), "no value is empty")
    check(challengeValue([(offset: 3, value: v)]) == nil, "a nonzero offset")
    check(challengeValue([(offset: 0, value: v), (offset: 16, value: v)]) == nil, "a prepared long write")
    check(challengeValue([]) == nil, "no request")
}

func testBridgeRoundTrip() {
    print("the bridge hands service, central and bytes to the callbacks")
    held = [:]
    let bridge = Bridge(onChallenge: fakeChallenge, onAddress: fakeAddress, ctx: nil)
    let challenge = Data((0..<16).map { UInt8(200 + $0) })
    let (before, _) = bridge.address(service: "SERVICE", central: "A")
    check(before == -1, "a read before any write has no challenge")
    check(bridge.challenge(service: "SERVICE", central: "A", value: challenge) == 0, "sixteen bytes kept")
    check(bridge.challenge(service: "SERVICE", central: "A", value: Data(count: 15)) == 1, "fifteen refused")
    check(bridge.challenge(service: "OTHER", central: "A", value: challenge) == 3, "unknown service")
    let (n, value) = bridge.address(service: "SERVICE", central: "A")
    check(n == 16 && value == challenge, "central A reads what it wrote")
    let (again, same) = bridge.address(service: "SERVICE", central: "A")
    check(again == 16 && same == challenge, "a second read returns the same bytes")
    check(bridge.address(service: "SERVICE", central: "B").0 == -1, "central B holds nothing")
}

testReadAnswers()
testWriteAnswers()
testChallengeValue()
testBridgeRoundTrip()

if failures == 0 {
    print("swift shim tests: all passed")
    exit(0)
} else {
    print("swift shim tests: \(failures) FAILED")
    exit(1)
}
