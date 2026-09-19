// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The phone's half of the peripheral probe: scans for one service uuid,
// connects, reads the address characteristic, subscribes to the wake one and
// prints what it saw as a JSON line. Compiled and run by
// tests/macos_peripheral_probe.rs.
//
//   central <service-uuid> <wake-uuid> <address-uuid>

import CoreBluetooth
import Foundation

final class Central: NSObject, CBCentralManagerDelegate, CBPeripheralManagerDelegate, CBPeripheralDelegate {
    let service: CBUUID
    let wake: CBUUID
    let address: CBUUID
    var manager: CBCentralManager!
    var found: CBPeripheral?
    var sawService = false
    var addressValue: String?
    var wakeValue: String?

    init(service: String, wake: String, address: String) {
        self.service = CBUUID(string: service)
        self.wake = CBUUID(string: wake)
        self.address = CBUUID(string: address)
    }

    func run(seconds: TimeInterval) {
        manager = CBCentralManager(delegate: self, queue: nil)
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline, wakeValue == nil || addressValue == nil {
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        report()
    }

    private func report() {
        let payload: [String: Any] = [
            "found": sawService,
            "address": addressValue ?? "",
            "wake": wakeValue ?? "",
        ]
        let data = (try? JSONSerialization.data(withJSONObject: payload)) ?? Data()
        print(String(decoding: data, as: UTF8.self))
    }

    func centralManagerDidUpdateState(_ c: CBCentralManager) {
        FileHandle.standardError.write(Data("central state: \(c.state.rawValue)\n".utf8))
        guard c.state == .poweredOn else { return }
        c.scanForPeripherals(withServices: [service], options: nil)
        FileHandle.standardError.write(Data("scanning for \(service)\n".utf8))
    }

    func centralManager(_ c: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        FileHandle.standardError.write(Data("connect failed: \(String(describing: error))\n".utf8))
    }

    func centralManager(_ c: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        let advertised = (advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID]) ?? []
        let overflow = (advertisementData[CBAdvertisementDataOverflowServiceUUIDsKey] as? [CBUUID]) ?? []
        FileHandle.standardError.write(Data("saw peripheral, services \(advertised) overflow \(overflow)\n".utf8))
        guard advertised.contains(service) || overflow.contains(service) else { return }
        sawService = true
        found = peripheral
        peripheral.delegate = self
        c.stopScan()
        c.connect(peripheral, options: nil)
    }

    func centralManager(_ c: CBCentralManager, didConnect peripheral: CBPeripheral) {
        peripheral.discoverServices([service])
    }

    func peripheral(_ p: CBPeripheral, didDiscoverServices error: Error?) {
        guard let svc = p.services?.first(where: { $0.uuid == service }) else { return }
        p.discoverCharacteristics([wake, address], for: svc)
    }

    func peripheral(_ p: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        for characteristic in service.characteristics ?? [] {
            if characteristic.uuid == address { p.readValue(for: characteristic) }
            if characteristic.uuid == wake { p.setNotifyValue(true, for: characteristic) }
        }
    }

    func peripheral(_ p: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let value = characteristic.value else { return }
        let text = String(decoding: value, as: UTF8.self)
        if characteristic.uuid == address { addressValue = text }
        if characteristic.uuid == wake { wakeValue = text }
    }

    func peripheralManagerDidUpdateState(_ p: CBPeripheralManager) {}
}

let args = CommandLine.arguments
guard args.count == 4 else {
    FileHandle.standardError.write(Data("usage: central <service> <wake> <address>\n".utf8))
    exit(2)
}
Central(service: args[1], wake: args[2], address: args[3]).run(seconds: 25)
