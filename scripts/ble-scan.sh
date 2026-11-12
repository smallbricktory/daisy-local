#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Bluetooth peripheral check, run from a second device: discovery, the challenge
# round and a delivered wake.
#
#   ./scripts/ble-scan.sh <service-uuid> [wake-uuid] [address-uuid] [challenge-uuid]
#
# The uuids are the pairing's. Prints one JSON line: whether the service was
# found, the random challenge written, the bytes the address read returned
# (or its ATT error), and the bytes of the first wake that arrived, all
# base64. The address opens under the pairing key with
# `daisy-gatt-2/addr || challenge` as associated data. macOS only; needs
# swiftc.
set -euo pipefail

SERVICE=${1:?usage: ble-scan.sh <service-uuid> [wake-uuid] [address-uuid] [challenge-uuid]}
WAKE=${2:-0A23320A-898F-4334-8D10-93B7F079AEE2}
ADDRESS=${3:-C5057A9F-691F-4D19-8510-395B1430A39E}
CHALLENGE=${4:-6F1C2B9E-3D4A-4B5C-9E8F-7A6B5C4D3E2F}

here=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -O -o "$out/ble-scan" "$here/ble-scan.swift"
"$out/ble-scan" "$SERVICE" "$WAKE" "$ADDRESS" "$CHALLENGE"
