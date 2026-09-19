# Mobile Sync

Phones running Daisy pair with one desktop and sync their recordings to it. The
desktop is the mothership: every phone session arrives here, gets the pipeline,
and its artifacts go back. The wire contract both implementations follow is
smbr-daisy-ios `docs/sync/protocol.md`; the shared test vectors both sides
assert against are `crates/tauri-app/tests/fixtures/sync-vectors-v1.json`.

Code: `crates/tauri-app/src/sync/`, the `ble` crate, `crates/vault`.

## This machine's identity

`<profile>/sync.<machine>.vault.json` holds the Ed25519 signing key, the X25519
key phones wrap file keys to, the TLS key and self-signed certificate, the
device id and the paired phones. It is sealed with the vault passphrase,
re-sealed when the passphrase changes or the vault switches mode, removed by a
vault reset, and named per machine, so a profile shared between two computers
carries two identities and two pairing tables. It is cached in memory while the
vault is open, because opening it costs Argon2id and a phone gives its whole
exchange about ten seconds.

## Pairing

Settings → Mobile Sync → Show pairing code opens a 90-second window and shows a
QR carrying the device id, the signing public key, the TLS certificate
fingerprint the phone pins, a single-use 32-byte secret, and this machine's
`.local` name and port. An IP goes in the code only when the machine has no
name of its own: a phone that pinned an address is unreachable the day DHCP
moves it.

The phone posts its identity to `POST /pair` with an HMAC over the body under a
key derived from that secret. The desktop verifies the MAC, stores the phone,
signs its reply and closes the window, so one code pairs one phone. Unpairing
removes the phone and forgets the nonces it used; nothing else is remembered,
so the same phone can pair again from a fresh code.

A device id belongs to the phone that paired it. This desktop authorizes by
that id — whose tombstones it honours, whose staged files a delete reaches,
whose refusals a pairing lifts — and a pairing code is 90 seconds open to
whoever sees the screen, so `/pair` refuses a device id that is already paired
here under another Ed25519 key: 409 with `{"error":"device_id_taken"}` and the
sentence the phone shows. A phone's device id, signing key and client
certificate live in one Keychain service (`afterFirstUnlockThisDeviceOnly`), so
a phone that lost its key has a new device id with it: an unpair and re-pair,
an erase and a restore from backup all present either the same id with the same
key or a new id, and pair as they always did. The remedy when a phone does meet
the refusal is on this desktop: Settings → Mobile Sync → the phone's menu →
Unpair, then show a fresh code.

Settings → Mobile Sync → a phone's menu → Rename names the phone on this
desktop, which tells apart several phones that all announce "iPhone". The
rename lives in the pairing table keyed by the phone's device id: it names
the phone in the list, in import toasts and in refusal lines, is kept when the
same phone pairs again, and goes with an unpair. The name the phone paired
with stays beneath it. A blank name clears the rename.

## The LAN server

Port 7845 on every interface, rustls with this machine's own certificate. It
runs only while the vault is open and either a phone is paired or a code is
showing, so a laptop with no phone opens no port. A pairing code that could not
bind reports the port conflict instead of showing a QR nothing can reach.

`/pair` is the one route without a client certificate. Every other route needs
three things to agree: the certificate presented in the TLS handshake, the
device id in the header, and an Ed25519 signature over method, path, device id,
nonce and body hash. The nonce is the replay guard — iOS signs a background
upload when it queues it and sends it later, out of order — and accepted nonces
are kept per device in `sync.<machine>.nonces.json`.

Everything that can be checked from the handshake and the headers is checked
before the body is read: the presented certificate against the pairing table
and the device id it claims, then the body, then the signature over it (which
covers the body hash) and the nonce. A stranger that reaches the port allocates
nothing. `/pair` has to read a body to check its MAC, so it reads one only while
a code is on screen and caps it at 64 KiB, against the 256 MiB every other route
works to. The listener serves 16 connections at once and drops what arrives
past that.

| route | what it does |
|---|---|
| `POST /hello` | answers from disk: the inputs at a revision this desktop lacks, the chunks the manifest lists once the manifest is here and the audio is new, the artifacts it has built, and the tombstone of any session the phone still lists and this desktop has buried |
| `PUT /file/{hash}` | verifies the hash, caps the body at 256 MiB, refuses a tombstoned session, writes under a checked session id and relative path |
| `GET /artifact/{id}` | the bytes of an artifact, found by the hash the phone asks for |
| `POST /tombstone` | buries a session whose record verifies under the phone's key |

## Import and the pipeline

Uploads stage in `<profile>/sync.<machine>.inbox/`, beside the identity and
named for the machine: a half-arrived session is one desktop's business even
though the profile is synced. Every area inside it is keyed by the device that
sent the files — `sessions/<device>/<session>/`, `refused/<device>/`,
`imported/<device>/` and `profile/<device>/` — so two devices that minted the
same `daisy-<unix seconds>` stage side by side, one device's delete or refusal
reaches only its own files, and a session is imported under the device that
sent it whatever device's hello started the import run. `<device>` is the
first 16 hex of the SHA-256 of the device id, the same derivation the
tombstone markers use: a device id is whatever that device called itself at
pairing, so it reaches a path through its hash. `sessions/<device>/device`
holds the id itself, which is how a staged session finds its sender again. An
inbox written before this layout is moved at the next unlock
(`migrations.rs`, `2026-09-inbox-by-device-<machine>`): import records move
under the device the library manifest names, refusal records under the device
they name, and staged partials are dropped — the device uploads them again,
where losing an import record would make it re-upload its whole library. A
hello names up to four inputs per session:
`manifest.json`, `transcript.json`, `speakers.json` and `notes.json`, the page
typed on the phone and the annotations pinned to transcript lines
(smbr-daisy-ios `docs/notes.md`). When a session's manifest, transcript and every
chunk it names have arrived, the session is imported: every file is opened
(envelope v2 with this machine's X25519 key, or taken as it is when the phone
wrote it in the clear), the audio chunks included, so the day the phone starts
sealing `mic.m4a` the desktop reads it. Phone audio is 16 kHz mono AAC at
32 kbps in `.m4a`; the import decodes each chunk to the `mic.wav` beside it
that every desktop reader expects (transcribe, diarize, voice samples, the
mixdown), dropping the 2112 priming samples Apple's encoder writes ahead of
the audio (measured 2026-09-06: symphonia 0.5 decodes them and carries no
delay metadata to key on), so the chunk sits on the phone transcript's
timeline; the manifest names the WAV. A chunk that does not decode refuses
the session before anything is written. The manifest gains
`recording_kind: phone_room`, `origin_device` and `single_local_speaker: false`
(the phone's mic heard the whole room, so diarize clusters it whatever track
is asked for), and everything lands under `sessions/` as a session of this
desktop's own. Every device mints `daisy-<unix seconds>`, so two of them land
on one id: the session already here keeps it and the arriving one gets an id
this desktop mints from `daisy-900000000000` up (`MINTED_ID_FLOOR`, above
anything a clock reads), with the device's own id kept in the manifest as
`origin_session`. A device names sessions below that floor
(`inbox::is_wire_session_id`): an id at or above it is refused wherever it
arrives — staging, import, a hello, a tombstone — because it would take an id
the library is about to hand out. `origin_session` is authored here on every
import and stripped from whatever the phone sent: it has no field on
`SessionManifest`, so it would otherwise ride through with the keys an import
carries untouched, and a delete here buries the id it names. Wants, artifact
offers and tombstones travel under the id the device knows;
`import::local_id` resolves it to the library's. A library
directory whose manifest cannot be read counts as taken, so the arriving
session mints an id rather than writing over a recording this desktop cannot
identify.

Each import records the revisions it left the session at, per device, in
`imported/<device>/<session>.json`: each input's sealed hash (the hello's
`inputs`), its plaintext hash (`content.<input>`) and the session's `audio`
(protocol §5: the SHA-256 over each chunk's plaintext SHA-256, in manifest
order). An input is at the recorded revision when either hash matches, so the
reseal an unpair and a pairing put every file through asks for nothing, and
the sealed hashes a hello lists after a reseal become the recorded ones, which
a locked phone's hello (sealed hashes only) then matches. A session at a new
revision stages a claim (`claims/<device>/<session>.json`): the manifest
always travels, an input whose plaintext is the recorded one stays as the
library holds it, and audio equal to the recorded `audio` keeps the library's
WAVs, so a rename on the phone sends one manifest. A hello from a device id
this desktop holds nothing under, listing a session whose `audio` matches one
another device's import left under the same wire id, hands that session to it:
the manifest's `origin_device` becomes the new id and the import record is
copied, which is what a phone whose sync identity was reset looks like. A
phone that sends no `audio` is matched at import instead: a session under the
same wire id whose WAVs hold the same samples takes the arriving copy, and one
whose audio is gone is not matched. The pipeline then builds `meeting.opus` from the chunks the
way a desktop recording gets one, so playback, the storage counts and a
re-transcribe find the file they expect. Sessions imported before the decode
landed are converted at the next unlock (`migrations.rs`,
`2026-09-phone-chunks-to-wav`). The revisions it arrived at are recorded, so a
later hello naming the same revisions asks for nothing and one naming
different revisions (a rename, a trim or a note edit on the phone) brings the
session over again.

The tag catalog and the contacts merge (protocol §5a, `sync/profile.rs`). A
hello names the phone's `tags.json` and `contacts.json` by hash; a revision
this desktop has not merged is wanted, arrives under `X-Daisy-Session:
profile`, and is merged off the connection: a tag by id then name (ignoring
case), a contact by id then email then name, the later edit winning
(`updated_at_unix_seconds`, written by every tag and contact edit here) and,
where either side carries no timestamp, the fuller row. The desktop rewrites
its own references to the ids that lost (manifests, workflow conditions,
voiceprints in the vault), then offers the merged file back with the ids the
phone rewrites when it installs. `<inbox>/profile/<device>/` holds the staged
copy and the record of the revision last merged from that device.

`notes.json` is phone-owned: it lands beside `speakers.json`, Ask indexes one
chunk per note line and per annotation (`[m:ss] Note: …`, `[m:ss] Note on
Alex: …`), and Library search reports a hit in it as Notes. The desktop never
edits it and never sends one back. It sits outside every artifact's
`built_from`: a note typed after a summary was built changes nothing the
summary read, so the phone keeps holding that summary as current.

A session this desktop cannot open is refused at the revisions it arrived at:
a file sealed to a key this machine does not hold (`sealed`), or a manifest or
envelope schema newer than this build reads (`unsupported`). The refusal is
recorded under `refused/<device>/` in the inbox and the staged copies go; a
hello naming those revisions asks for nothing, one naming new revisions brings the session
over again. `sync_status` lists the refused sessions, Settings → Mobile Sync
shows them with the remedy (pair the phone again, or update Daisy), and a
`sync:refused` event raises a toast when one is recorded. Pairing a phone again
lifts its refusals, so what it offers next is tried afresh; unpairing it and a
delete on either side drop them. A session still missing a file stays staged
and is tried at the next hello.

Then `transcript.md`, the summary and the chapters, each best-effort. There is
nothing to transcribe and no system track or echo to cancel. All of it runs off
the connection: importing copies audio and summarizing takes minutes, and the
phone is waiting.

Every step of that reports itself. A `sync:import` event carries the phone, the
session and where it has got to: `receiving` as each file lands (with the chunks
here and the chunks the manifest names, once the manifest has arrived),
`importing`, then `audio`, `transcript`, `summary`, `chapters`, then `done` with
the meeting's title. From the moment the session is in the library the event
also carries `library_session`, the id it is held under here, which is the one
the toast opens. The frontend keeps one toast per session and replaces it in
place (`lib/syncImport.ts`); `done` is the one you can click, and it opens the
meeting. `done` fires whatever the pipeline managed, so a step that fails leaves
no toast spinning forever. Without it the minutes between a phone starting to
upload and a row appearing in the library are silent.

A delete on either device deletes on both. A tombstone from the phone removes
the imported session; deleting a phone session here writes a signed tombstone
that every hello offers while that device still lists the session. A tombstone
belongs to the device whose recording it buried: it is offered to that device
alone, refuses that device's uploads alone, and removes a library session only
when that device is its origin — another device's recording of the same id
stays. A tombstone with no device on record, which is what a marker written
before this desktop tracked origins reads back as, is offered to every device.

Who holds the id is read before anything is written. A library directory whose
manifest cannot be read settles nothing — a sync tool holds a live file open
for moments at a time — so `POST /tombstone` answers 503, no marker is written,
and the device sends the tombstone again at its next hello. Recording it first
would answer the delete while the recording stayed in the library and every
re-upload of it answered 409.

Known behaviour: a device that changes its id — a reinstall that pairs afresh
— is a device this desktop has never met, so the tombstones of its earlier id
do not answer it and a session deleted before the reinstall arrives again.
Deleting it once more buries it under the new id and it stays buried. The
alternative, burying a session for every device by id alone, deletes an
unrelated recording on a second device that minted the same
`daisy-<unix seconds>`.

## Bluetooth

macOS only. The desktop advertises one service per pairing — the derived uuid
and nothing else, no local name, no manufacturer data — with a notify the phone
wakes on and a readable address, both sealed under that pairing's key. The
address is republished whenever the listener state is applied, so a phone that
wakes after a network change reads the new one; a wake goes out when a
session's summary and chapters finish.

Elsewhere there is no peripheral role, and a phone syncs whenever its app is
open on the same network. Settings → Mobile Sync says which of those three
states this machine is in.

Two things about CoreBluetooth were measured rather than assumed: a Mac does
not see its own advertisements, so discovery needs a second device
(`scripts/ble-scan.sh`), and `updateValue` answers true with no central
subscribed, so the subscriber count is what says a phone is listening.

## Versions

The wire contract of record is smbr-daisy-ios `docs/sync/protocol.md`; §11
carries the rules. This desktop speaks `v` 2 and reads 1: a v1 phone gets wants
and tombstones and no offers; a phone newer than this build gets 426 and tells
its owner to update the desktop. `summary.json` is written as contract v2 by
every writer here and read through `SessionSummary::decode`, whose legacy arm
converts the `schema_version: 1` files this desktop wrote before; a legacy file
is written back as v2 the first time it is loaded, because this desktop authored
it. Phone notes (`notes.json`) are searchable here and never summarized here.

## Testing

| suite | what it covers |
|---|---|
| `cargo test -p tauri-app --test sync_vectors` | every derivation, MAC, signature and payload against the file the phone side asserts on too |
| `cargo test -p tauri-app --test sync_transport` | a synthetic phone over loopback TLS: pair, hello, upload, replay refused, import, artifact, delete |
| `cargo test -p tauri-app --test sync_commands` | the commands the settings screen calls; `DAISY_PHONE_SAMPLE=<dir>` adds a real phone recording |
| `cargo test -p ble -- --ignored` | the radio: powered on, service published, advertising |

Regenerating the vectors after any change to a derivation, a wire body or a
signing string:

```sh
cargo test -p tauri-app --test sync_vectors -- --ignored write_vectors
cp crates/tauri-app/tests/fixtures/sync-vectors-v1.json \
   ../smbr-daisy-ios/ios/DaisyKit/Tests/DaisyKitTests/Fixtures/
```

## Not built yet

- Nothing has run against a real phone. The two-device matrix — pair, unpair,
  re-pair, delete on either side, phone killed mid-chunk, desktop asleep, DHCP
  move — is the next measurement.
- Windows answers mDNS for its own hostname (measured 2026-09-05: a Mac
  resolved `astrophage.local` to the Windows LAN address in milliseconds;
  the Dnscache service listens on 5353 with `EnableMDNS` unset, which is on).
  The Windows firewall was measured 2026-09-06 on the same machine: the
  first `Show pairing code` binds 0.0.0.0:7845 and Windows raises its
  "allow public and private networks to access this app?" prompt, creating
  two inbound Block rules (TCP and UDP, Public profile) for that exact
  `daisy-app.exe` path at the moment the prompt appears. Allow needs an
  administrator and rewrites them to Allow; Cancel, or a prompt nobody
  answers, leaves them, and no phone reaches that binary until the user
  allows Daisy under Windows Security → Firewall & network protection →
  Allow an app through firewall. The per-user NSIS install runs without
  elevation, so it cannot pre-create the rule; the settings screen says
  what the prompt is and what Cancel costs. Detecting an existing Block
  rule from the app (INetFwPolicy2) is unbuilt.
