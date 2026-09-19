#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# The second half of the Bluetooth peripheral check, for a machine that is not
# the desktop under test: a Mac does not see its own advertisements, so
# discovery, the address read and a delivered wake can only be measured from
# another device.
#
#   ./scripts/ble-scan.sh <service-uuid> [wake-uuid] [address-uuid]
#
# The uuids are the pairing's, which the desktop logs when it publishes the
# service. Prints one JSON line: whether the service was found, the bytes the
# address characteristic returned, and the bytes of the first wake that
# arrived. macOS only; needs swiftc.
set -euo pipefail

SERVICE=${1:?usage: ble-scan.sh <service-uuid> [wake-uuid] [address-uuid]}
WAKE=${2:-0A23320A-898F-4334-8D10-93B7F079AEE2}
ADDRESS=${3:-C5057A9F-691F-4D19-8510-395B1430A39E}

here=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -O -o "$out/ble-scan" "$here/ble-scan.swift"
"$out/ble-scan" "$SERVICE" "$WAKE" "$ADDRESS"
