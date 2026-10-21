// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// CoreBluetooth peripheral shim: a C ABI over CBPeripheralManager, consumed
// by `macos/mod.rs`. Compiled by build.rs (swiftc, static lib) on macOS only.
//
// The desktop advertises one service per paired phone and nothing else — no
// local name, no manufacturer data — with three characteristics: a notify the
// phone wakes on, a challenge the phone writes and an address it reads. The
// caller seals every value; this shim moves bytes and never looks inside them.
// Challenge writes and address reads go to the caller's callbacks, on this
// shim's queue, with the service and the central's identifier.
//
//   void   *ble_start(challenge_fn on_challenge, address_fn on_address,
//                     void *ctx, char err[256]);   // NULL on failure
//   bool    ble_add_service(void *h, const char *service, const char *wake,
//                           const char *address, const char *challenge,
//                           char err[256]);
//   bool    ble_notify(void *h, const char *service, const uint8_t *value,
//                      int32_t len);            // false when the queue is full
//   int32_t ble_subscribers(void *h, const char *service);
//   int32_t ble_state(void *h);                 // CBManagerState raw value
//   bool    ble_is_advertising(void *h);
//   void    ble_stop(void *h);                  // returns once no callback runs
//
//   int32_t challenge_fn(void *ctx, const char *service, const char *central,
//                        const uint8_t *value, int32_t len);
//       // 0 kept, 1 wrong length, 2 failed, 3 unknown service, 4 limited
//   int32_t address_fn(void *ctx, const char *service, const char *central,
//                      uint8_t *out, int32_t cap);
//       // bytes written to out, or -1 no challenge held, -2 unknown service,
//       // -3 failed

import CoreBluetooth
import Darwin
import Foundation

public typealias ChallengeFn = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?, UnsafePointer<CChar>?,
                                        UnsafePointer<UInt8>?, Int32) -> Int32
public typealias AddressFn = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?, UnsafePointer<CChar>?,
                                      UnsafeMutablePointer<UInt8>?, Int32) -> Int32

/// The largest attribute value ATT carries.
let maxAttributeValue = 512

private func fill(_ err: UnsafeMutablePointer<CChar>?, _ message: String) {
    guard let err else { return }
    let bytes = Array(message.utf8.prefix(255))
    bytes.withUnsafeBufferPointer { src in
        err.withMemoryRebound(to: UInt8.self, capacity: 256) { dst in
            dst.update(from: src.baseAddress!, count: bytes.count)
            dst[bytes.count] = 0
        }
    }
}

/// The ATT result for a challenge callback's answer.
func writeResult(_ answer: Int32) -> CBATTError.Code {
    switch answer {
    case 0: return .success
    case 1: return .invalidAttributeValueLength
    case 3: return .attributeNotFound
    default: return .unlikelyError
    }
}

/// The value of a challenge write: one request at offset 0, or `nil`, which
/// is answered Invalid Attribute Value Length.
func challengeValue(_ requests: [(offset: Int, value: Data?)]) -> Data? {
    guard requests.count == 1, requests[0].offset == 0 else { return nil }
    return requests[0].value ?? Data()
}

/// The ATT result and value for an address callback's answer and the bytes
/// it wrote, read from `offset`. Every offset reads the same stored bytes.
func readReply(_ answer: Int32, _ bytes: Data, offset: Int) -> (CBATTError.Code, Data?) {
    switch answer {
    case -1: return (.readNotPermitted, nil)
    case -2: return (.attributeNotFound, nil)
    case ..<0: return (.unlikelyError, nil)
    default:
        let value = Data(bytes.prefix(Int(answer)))
        guard offset <= value.count else { return (.invalidOffset, nil) }
        return (.success, value.subdata(in: offset..<value.count))
    }
}

/// The caller's callbacks and the context they are handed back.
struct Bridge {
    let onChallenge: ChallengeFn
    let onAddress: AddressFn
    let ctx: UnsafeMutableRawPointer?

    func challenge(service: String, central: String, value: Data) -> Int32 {
        service.withCString { s in
            central.withCString { c in
                value.withUnsafeBytes { raw in
                    onChallenge(ctx, s, c, raw.bindMemory(to: UInt8.self).baseAddress, Int32(value.count))
                }
            }
        }
    }

    func address(service: String, central: String) -> (Int32, Data) {
        var out = [UInt8](repeating: 0, count: maxAttributeValue)
        let answer = service.withCString { s in
            central.withCString { c in
                out.withUnsafeMutableBufferPointer { buf in
                    onAddress(ctx, s, c, buf.baseAddress, Int32(buf.count))
                }
            }
        }
        return (answer, answer > 0 ? Data(out.prefix(Int(answer))) : Data())
    }
}

/// One paired phone's service and its three characteristics.
private final class Entry {
    let service: CBMutableService
    let wake: CBMutableCharacteristic
    let address: CBMutableCharacteristic
    let challenge: CBMutableCharacteristic
    /// Centrals subscribed to the wake characteristic right now.
    var subscribers = 0

    init(service: CBUUID, wake: CBUUID, address: CBUUID, challenge: CBUUID) {
        self.wake = CBMutableCharacteristic(type: wake, properties: [.notify], value: nil,
                                            permissions: [.readable])
        self.address = CBMutableCharacteristic(type: address, properties: [.read], value: nil,
                                               permissions: [.readable])
        self.challenge = CBMutableCharacteristic(type: challenge, properties: [.write], value: nil,
                                                 permissions: [.writeable])
        self.service = CBMutableService(type: service, primary: true)
        self.service.characteristics = [self.wake, self.address, self.challenge]
    }
}

private final class Peripheral: NSObject, CBPeripheralManagerDelegate {
    private var manager: CBPeripheralManager!
    private let queue = DispatchQueue(label: "daisy.ble")
    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    /// Services waiting for the radio to power on before they are published.
    private var pending: [Entry] = []
    private var bridge: Bridge?
    private(set) var state: CBManagerState = .unknown

    var isAdvertising: Bool { manager.isAdvertising }

    init(bridge: Bridge) {
        self.bridge = bridge
        super.init()
        manager = CBPeripheralManager(delegate: self, queue: queue)
    }

    func add(service: String, wake: String, address: String, challenge: String) -> String? {
        guard let s = uuid(service), let w = uuid(wake), let a = uuid(address), let c = uuid(challenge) else {
            return "not a UUID"
        }
        let entry = Entry(service: s, wake: w, address: a, challenge: c)
        lock.lock()
        entries[service.uppercased()] = entry
        let powered = state == .poweredOn
        if !powered { pending.append(entry) }
        lock.unlock()
        if powered {
            manager.add(entry.service)
            advertise()
        }
        return nil
    }

    /// Queues the wake payload for every subscribed central. False when the
    /// transmit queue is full. True says the radio took it, never that a
    /// phone heard.
    func notify(service: String, value: Data) -> Bool {
        lock.lock()
        let entry = entries[service.uppercased()]
        lock.unlock()
        guard let entry else { return false }
        return manager.updateValue(value, for: entry.wake, onSubscribedCentrals: nil)
    }

    func subscribers(service: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return entries[service.uppercased()]?.subscribers ?? 0
    }

    func stop() {
        manager.stopAdvertising()
        manager.removeAllServices()
        lock.lock()
        entries.removeAll()
        pending.removeAll()
        bridge = nil
        lock.unlock()
        queue.sync {}
    }

    private func uuid(_ text: String) -> CBUUID? {
        UUID(uuidString: text).map { CBUUID(nsuuid: $0) }
    }

    /// The service key and the bridge for a request on the characteristic
    /// `uuid`, matched against each entry's `pick`.
    private func route(_ uuid: CBUUID, _ pick: (Entry) -> CBMutableCharacteristic) -> (service: String, bridge: Bridge)? {
        lock.lock()
        defer { lock.unlock() }
        guard let bridge, let key = entries.first(where: { pick($0.value).uuid == uuid })?.key else { return nil }
        return (key, bridge)
    }

    /// Advertises the service UUIDs and nothing else: no local name, no
    /// manufacturer data.
    private func advertise() {
        lock.lock()
        let uuids = entries.values.map { $0.service.uuid }
        lock.unlock()
        manager.stopAdvertising()
        guard !uuids.isEmpty else { return }
        manager.startAdvertising([CBAdvertisementDataServiceUUIDsKey: uuids])
    }

    func peripheralManagerDidUpdateState(_ p: CBPeripheralManager) {
        lock.lock()
        state = p.state
        let waiting = pending
        pending.removeAll()
        lock.unlock()
        guard p.state == .poweredOn else { return }
        for entry in waiting { p.add(entry.service) }
        advertise()
    }

    func peripheralManager(_ p: CBPeripheralManager, central: CBCentral,
                           didSubscribeTo characteristic: CBCharacteristic) {
        lock.lock()
        entries.values.first { $0.wake.uuid == characteristic.uuid }?.subscribers += 1
        lock.unlock()
    }

    func peripheralManager(_ p: CBPeripheralManager, central: CBCentral,
                           didUnsubscribeFrom characteristic: CBCharacteristic) {
        lock.lock()
        if let entry = entries.values.first(where: { $0.wake.uuid == characteristic.uuid }) {
            entry.subscribers = max(0, entry.subscribers - 1)
        }
        lock.unlock()
    }

    func peripheralManager(_ p: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        guard let first = requests.first else { return }
        guard let routed = route(first.characteristic.uuid, { $0.challenge }) else {
            p.respond(to: first, withResult: .attributeNotFound)
            return
        }
        guard let value = challengeValue(requests.map { (offset: $0.offset, value: $0.value) }) else {
            p.respond(to: first, withResult: .invalidAttributeValueLength)
            return
        }
        let answer = routed.bridge.challenge(service: routed.service,
                                             central: first.central.identifier.uuidString, value: value)
        p.respond(to: first, withResult: writeResult(answer))
    }

    func peripheralManager(_ p: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        guard let routed = route(request.characteristic.uuid, { $0.address }) else {
            p.respond(to: request, withResult: .attributeNotFound)
            return
        }
        let (answer, bytes) = routed.bridge.address(service: routed.service,
                                                    central: request.central.identifier.uuidString)
        let (result, value) = readReply(answer, bytes, offset: request.offset)
        if let value { request.value = value }
        p.respond(to: request, withResult: result)
    }
}

private func take(_ handle: UnsafeMutableRawPointer?) -> Peripheral? {
    handle.map { Unmanaged<Peripheral>.fromOpaque($0).takeUnretainedValue() }
}

private func data(_ bytes: UnsafePointer<UInt8>?, _ len: Int32) -> Data {
    guard let bytes, len > 0 else { return Data() }
    return Data(bytes: bytes, count: Int(len))
}

@_cdecl("ble_start")
public func ble_start(_ onChallenge: ChallengeFn, _ onAddress: AddressFn, _ ctx: UnsafeMutableRawPointer?,
                      _ err: UnsafeMutablePointer<CChar>?) -> UnsafeMutableRawPointer? {
    let peripheral = Peripheral(bridge: Bridge(onChallenge: onChallenge, onAddress: onAddress, ctx: ctx))
    fill(err, "")
    return Unmanaged.passRetained(peripheral).toOpaque()
}

@_cdecl("ble_add_service")
public func ble_add_service(_ handle: UnsafeMutableRawPointer?, _ service: UnsafePointer<CChar>?,
                            _ wake: UnsafePointer<CChar>?, _ address: UnsafePointer<CChar>?,
                            _ challenge: UnsafePointer<CChar>?,
                            _ err: UnsafeMutablePointer<CChar>?) -> Bool {
    guard let peripheral = take(handle), let service, let wake, let address, let challenge else {
        fill(err, "no peripheral")
        return false
    }
    if let problem = peripheral.add(service: String(cString: service), wake: String(cString: wake),
                                    address: String(cString: address), challenge: String(cString: challenge)) {
        fill(err, problem)
        return false
    }
    return true
}

@_cdecl("ble_notify")
public func ble_notify(_ handle: UnsafeMutableRawPointer?, _ service: UnsafePointer<CChar>?,
                       _ value: UnsafePointer<UInt8>?, _ len: Int32) -> Bool {
    guard let peripheral = take(handle), let service else { return false }
    return peripheral.notify(service: String(cString: service), value: data(value, len))
}

@_cdecl("ble_state")
public func ble_state(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let peripheral = take(handle) else { return 0 }
    return Int32(peripheral.state.rawValue)
}

@_cdecl("ble_subscribers")
public func ble_subscribers(_ handle: UnsafeMutableRawPointer?, _ service: UnsafePointer<CChar>?) -> Int32 {
    guard let peripheral = take(handle), let service else { return 0 }
    return Int32(peripheral.subscribers(service: String(cString: service)))
}

@_cdecl("ble_is_advertising")
public func ble_is_advertising(_ handle: UnsafeMutableRawPointer?) -> Bool {
    guard let peripheral = take(handle) else { return false }
    return peripheral.isAdvertising
}

@_cdecl("ble_stop")
public func ble_stop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    let peripheral = Unmanaged<Peripheral>.fromOpaque(handle).takeRetainedValue()
    peripheral.stop()
}
