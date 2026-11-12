// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The phone's half of the peripheral probe: scans for one service uuid,
// connects, subscribes to the wake characteristic, writes a random challenge,
// reads the address sealed to it and prints what it saw as a JSON line. Run
// by scripts/ble-scan.sh.
//
//   central <service-uuid> <wake-uuid> <address-uuid> <challenge-uuid>

import CoreBluetooth
import Foundation

final class Central: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    let service: CBUUID
    let wake: CBUUID
    let address: CBUUID
    let challengeUUID: CBUUID
    let challenge: Data
    var manager: CBCentralManager!
    var found: CBPeripheral?
    var sawService = false
    var addressValue: Data?
    var addressError: String?
    var wakeValue: Data?

    init(service: String, wake: String, address: String, challenge: String) {
        self.service = CBUUID(string: service)
        self.wake = CBUUID(string: wake)
        self.address = CBUUID(string: address)
        self.challengeUUID = CBUUID(string: challenge)
        var bytes = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        self.challenge = Data(bytes)
    }

    func run(seconds: TimeInterval) {
        manager = CBCentralManager(delegate: self, queue: nil)
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline, wakeValue == nil || (addressValue == nil && addressError == nil) {
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        report()
    }

    private func report() {
        let payload: [String: Any] = [
            "found": sawService,
            "challenge_b64": challenge.base64EncodedString(),
            "address_b64": addressValue?.base64EncodedString() ?? "",
            "address_error": addressError ?? "",
            "wake_b64": wakeValue?.base64EncodedString() ?? "",
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
        p.discoverCharacteristics([wake, address, challengeUUID], for: svc)
    }

    func peripheral(_ p: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        for characteristic in service.characteristics ?? [] {
            if characteristic.uuid == wake { p.setNotifyValue(true, for: characteristic) }
            if characteristic.uuid == challengeUUID { p.writeValue(challenge, for: characteristic, type: .withResponse) }
        }
    }

    func peripheral(_ p: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error {
            addressError = "challenge write: \(error.localizedDescription)"
            return
        }
        guard let svc = p.services?.first(where: { $0.uuid == service }),
              let addr = svc.characteristics?.first(where: { $0.uuid == address }) else { return }
        p.readValue(for: addr)
    }

    func peripheral(_ p: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        if characteristic.uuid == address, let error {
            addressError = "address read: \(error.localizedDescription)"
            return
        }
        guard let value = characteristic.value else { return }
        if characteristic.uuid == address { addressValue = value }
        if characteristic.uuid == wake { wakeValue = value }
    }
}

let args = CommandLine.arguments
guard args.count == 5 else {
    FileHandle.standardError.write(Data("usage: central <service> <wake> <address> <challenge>\n".utf8))
    exit(2)
}
Central(service: args[1], wake: args[2], address: args[3], challenge: args[4]).run(seconds: 25)
