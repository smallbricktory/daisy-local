// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// CoreBluetooth peripheral shim: a C ABI over CBPeripheralManager, consumed
// by `macos/mod.rs`. Compiled by build.rs (swiftc, static lib) on macOS only.
//
// The desktop advertises one service per paired phone and nothing else — no
// local name, no manufacturer data — with two characteristics: a notify the
// phone wakes on and a readable address. Both values are sealed by the
// caller; this shim moves bytes and never looks inside them.
//
//   void   *ble_start(char err[256]);           // NULL on failure
//   bool    ble_add_service(void *h, const char *service, const char *wake,
//                           const char *address, const uint8_t *value,
//                           int32_t len, char err[256]);
//   bool    ble_set_address(void *h, const char *service,
//                           const uint8_t *value, int32_t len);
//   bool    ble_notify(void *h, const char *service, const uint8_t *value,
//                      int32_t len);            // false when the queue is full
//   int32_t ble_subscribers(void *h, const char *service);
//   int32_t ble_state(void *h);                 // CBManagerState raw value
//   bool    ble_is_advertising(void *h);
//   void    ble_stop(void *h);

import CoreBluetooth
import Darwin
import Foundation

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

/// One paired phone's service: the two characteristics and the address value
/// a read returns.
private final class Entry {
    let service: CBMutableService
    let wake: CBMutableCharacteristic
    let address: CBMutableCharacteristic

    init(service: CBUUID, wake: CBUUID, address: CBUUID, value: Data) {
        self.wake = CBMutableCharacteristic(type: wake, properties: [.notify], value: nil,
                                            permissions: [.readable])
        self.address = CBMutableCharacteristic(type: address, properties: [.read], value: nil,
                                               permissions: [.readable])
        self.service = CBMutableService(type: service, primary: true)
        self.service.characteristics = [self.wake, self.address]
        self.addressValue = value
    }

    var addressValue: Data
    /// Centrals subscribed to the wake characteristic right now.
    var subscribers = 0
}

private final class Peripheral: NSObject, CBPeripheralManagerDelegate {
    private var manager: CBPeripheralManager!
    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    /// Services waiting for the radio to power on before they are published.
    private var pending: [Entry] = []
    private(set) var state: CBManagerState = .unknown

    var isAdvertising: Bool { manager.isAdvertising }

    override init() {
        super.init()
        manager = CBPeripheralManager(delegate: self, queue: DispatchQueue(label: "daisy.ble"))
    }

    func add(service: String, wake: String, address: String, value: Data) -> String? {
        guard let s = uuid(service), let w = uuid(wake), let a = uuid(address) else {
            return "not a UUID"
        }
        let entry = Entry(service: s, wake: w, address: a, value: value)
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

    func setAddress(service: String, value: Data) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let entry = entries[service.uppercased()] else { return false }
        entry.addressValue = value
        return true
    }

    /// Queues the wake payload for every subscribed central. False when the
    /// transmit queue is full; the caller retries at its own pace rather than
    /// blocking here. True says the radio took it, never that a phone heard.
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
        lock.unlock()
    }

    private func uuid(_ text: String) -> CBUUID? {
        UUID(uuidString: text).map { CBUUID(nsuuid: $0) }
    }

    /// Advertises the service UUIDs and nothing else: no local name, no
    /// manufacturer data, so only a phone that derived the UUID knows what it
    /// has found.
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

    func peripheralManager(_ p: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        lock.lock()
        let entry = entries.values.first { $0.address.uuid == request.characteristic.uuid }
        let value = entry?.addressValue
        lock.unlock()
        guard let value else {
            p.respond(to: request, withResult: .attributeNotFound)
            return
        }
        guard request.offset <= value.count else {
            p.respond(to: request, withResult: .invalidOffset)
            return
        }
        request.value = value.subdata(in: request.offset..<value.count)
        p.respond(to: request, withResult: .success)
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
public func ble_start(_ err: UnsafeMutablePointer<CChar>?) -> UnsafeMutableRawPointer? {
    let peripheral = Peripheral()
    fill(err, "")
    return Unmanaged.passRetained(peripheral).toOpaque()
}

@_cdecl("ble_add_service")
public func ble_add_service(_ handle: UnsafeMutableRawPointer?, _ service: UnsafePointer<CChar>?,
                            _ wake: UnsafePointer<CChar>?, _ address: UnsafePointer<CChar>?,
                            _ value: UnsafePointer<UInt8>?, _ len: Int32,
                            _ err: UnsafeMutablePointer<CChar>?) -> Bool {
    guard let peripheral = take(handle), let service, let wake, let address else {
        fill(err, "no peripheral")
        return false
    }
    if let problem = peripheral.add(service: String(cString: service), wake: String(cString: wake),
                                    address: String(cString: address), value: data(value, len)) {
        fill(err, problem)
        return false
    }
    return true
}

@_cdecl("ble_set_address")
public func ble_set_address(_ handle: UnsafeMutableRawPointer?, _ service: UnsafePointer<CChar>?,
                            _ value: UnsafePointer<UInt8>?, _ len: Int32) -> Bool {
    guard let peripheral = take(handle), let service else { return false }
    return peripheral.setAddress(service: String(cString: service), value: data(value, len))
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
