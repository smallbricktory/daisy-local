# Sync transport findings

Measurements behind the choice of a fallback link for phone sync on Wi-Fi with
client isolation, where the access point drops traffic between clients and the
phone cannot reach port 7845 or see the desktop's mDNS advert.

## 2026-10-15: iPhone 15 Pro Max ↔ xenonite, one desk apart

Throwaway probe: a Mac app in the desktop's role (Bluetooth peripheral, or a
Network framework TCP listener advertised over Bonjour) and an iPhone app in the
phone's role, the same framing on every link: the phone uploads n bytes and the
Mac acknowledges with its own first-to-last-byte time, then the Mac sends n
bytes. Source and raw logs: `~/dev/smbr-daisy-app/ble-throughput/`.

| link | up (phone→Mac) | down (Mac→phone) | 1 h of phone audio (~14 MB, 32 kbps AAC) |
|---|---|---|---|
| Wi-Fi through the router (phone en0, Mac wired en8) | 41.6 MB/s (333 Mbps) | 39.9 MB/s (319 Mbps) | under 1 s |
| Apple peer-to-peer Wi-Fi, AWDL only (phone en0 and cellular prohibited) | 22.7 MB/s (182 Mbps) | 24.9 MB/s (199 Mbps) | under 1 s |
| Bluetooth LE GATT, write without response, 512-byte writes | 22.8 KB/s (182 kbps) | not measured | about 10.5 min |
| Bluetooth LE L2CAP channel | did not open | did not open | — |

- **AWDL** connected in 2.5 s and both ends reported `awdl0` as the path
  (`fe80::…%awdl0`). The phone stayed associated to its Wi-Fi (en0 listed
  throughout). Peer-to-peer must be on at both ends: `includePeerToPeer` on the
  phone's browser and connection and on the Mac's listener.
- **GATT**: 1,000,000 bytes in 1,954 writes, every byte received; phone and Mac
  timings agree (43.7 s and 44.1 s). One round.
- **L2CAP**: first attempt, after the GATT round on the same link,
  `openL2CAPChannel` hung 31 s, returned `CBErrorDomain Code=0 "Unknown error."`,
  and the Mac dropped the link (`Code=7`); the Mac's `didOpen` never fired. Two
  later L2CAP-only runs (unencrypted, then encrypted) found the peripheral and
  never completed the BLE connect within 10 min each. Cause unknown.

## 2026-10-16: phone disconnected from Wi-Fi

Phone Wi-Fi disconnected in Control Center (radio on, no network), cellular
prohibited, no cable; Mac on wired Ethernet with Wi-Fi unassociated. Two runs.

- **Discovery works over AWDL alone.** The browser found the Mac's Bonjour advert
  `via ["awdl0"]` both times.
- **The connection never used awdl0.** It stayed pending with the Mac seeing
  nothing, with and without `requiredInterfaceType = .wifi`. When the phone
  rejoined Wi-Fi the pending connection completed over the router, 101 s after
  it started (phone en0, Mac en8): 28.7 MB/s up, 46.3 MB/s down at the phone.
  Cause unknown.
- A USB cable to the Mac is itself a network link (phone `en2`/`anpi0`, Mac
  `en12`, about 370–585 MB/s) and wins discovery while plugged in; a run that
  needs AWDL has to start untethered.

## 2026-10-16: Bluetooth again, phone freshly launched

One round, 1 MB each, phone at RSSI −46. The link connected in 2 s and the L2CAP
channel opened at once (unencrypted, PSM 192), on the first attempt after the
GATT round.

| link | up (phone→Mac) | down (Mac→phone) | 1 h of phone audio (~14 MB) |
|---|---|---|---|
| GATT, write without response, 512-byte writes | 19.8 KB/s | not measured | about 12 min |
| L2CAP channel | 20.3 KB/s | 18.6 KB/s | about 12 min |

L2CAP moves data at the GATT rate on this pair: both are capped near 20 KB/s
(about 160 kbps), the iPhone-as-central rate other developers report. The
2026-10-15 L2CAP failures (an open that hung 31 s, then two runs whose BLE
connect never completed) did not recur; what made those runs fail is unknown.

## 2026-10-16 afternoon: advertising for AWDL, and AWDL connections

Mac side: the forwarder on branch `feat/direct-wifi` (Network framework
listener, peer-to-peer allowed, forwarding to a loopback server), in a probe
bundle holding a Local Network grant. Phone: BleProbe on Wi-Fi.

| Mac advert | phone, peer-to-peer off | phone, peer-to-peer on |
|---|---|---|
| listener's own Bonjour service | found via en0, connected, 73–90 MB/s through the forwarder | found via en0 only |
| `DNSServiceRegister` on awdl0's interface index | not found | found via awdl0 in 0.3–0.9 s |

- **An advert that reaches AWDL also reaches every network the Mac joins**
  unless it is registered on awdl0's own index. `kDNSServiceInterfaceIndexP2P`
  in `DNSServiceRegister` is mapped to "any interface plus P2P" (dns_sd.h). A
  registration on awdl0's index is found over AWDL only.
- **No AWDL connection completed** in seven runs this afternoon: phone with en0
  prohibited (both adverts), phone unrestricted against the awdl0-only advert,
  and the exact 2026-10-15 setup that connected over awdl0 in 2.5 s. The
  connection stays pending with nothing reaching the Mac. Cause unknown.
- A probe app without a Local Network grant gets `DNSServiceRegister START`
  in mDNSResponder and no result, with no error to the app. Rebuilt ad-hoc
  bundles and new bundle ids both stayed ungranted; no prompt was seen.

## Open

- **Why AWDL connections stopped completing**, when discovery over awdl0 works.
  AirDrop between the same iPhone and Mac would show whether AWDL data works
  on this pair at all.
- **A real isolated network.** On one the phone stays associated, which is the
  state where AWDL carried data (2026-10-15). Untested end to end: phone on a
  client-isolated network, Mac unreachable through it, discovery and data both
  over awdl0.
- **Why a phone with Wi-Fi disconnected discovers over AWDL but does not connect.**
- **Mac on Wi-Fi.** xenonite was on wired Ethernet, which left its Wi-Fi radio
  free for AWDL. A Mac whose only network is Wi-Fi shares one radio between the
  router and AWDL; throughput and the router link's latency during a transfer
  are unmeasured there.
- **L2CAP.** Why the 2026-10-15 runs failed to open or connect.
- **Android, Windows, Linux.** Not measured. Apple peer-to-peer Wi-Fi exists only
  between Apple devices.
