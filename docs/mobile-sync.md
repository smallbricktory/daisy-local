# Mobile Sync

Phones running Daisy pair with one desktop and sync their recordings to it. The
desktop is the mothership: every phone session arrives here, gets the pipeline,
and its artifacts go back. The wire contract both implementations follow is
the sync protocol document shared with the iOS app; the shared test vectors both sides
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

Settings → Mobile Sync → Pair a device mints an 80-bit key and shows it as a
QR (`daisy-pair:2:<key>@<host>:<port>`) and as 16 Crockford base32 characters
in four groups. The host is this machine's `.local` name, or a LAN IPv4 when it
has no name of its own: a phone that pinned an address is unreachable the day
DHCP moves it. The key lives until a phone pairs, Cancel, leaving Mobile Sync,
a vault lock, or ten minutes. While it lives the desktop advertises DNS-SD
`_daisy-pair._tcp` with its device id, host, port and a two-character routing
tag from the key's hash, so a phone given the typed key finds this machine.

The phone posts `POST /pair/start` with its identity, a fresh X25519 key and
nonce, and `cP`, an HMAC proving it holds the key. The desktop checks the TLS
client certificate against the body and `cP` before it keeps anything, then
answers with its identity, its own fresh X25519 key and nonce, and `cD`, an
HMAC under a key from the pairing key and the exchange. The phone checks `cD`
and the certificate it saw, then posts the v1 `/pair` body on a pinned
connection with `X-Daisy-Pair-Session` and a MAC under the session's secret S.
S is what protocol.md §2 derives the pairing key and BLE UUIDs from, so pairs
made by the v1 QR keep working unchanged. The desktop stores the phone, signs
its reply and ends the key, so one key pairs one phone. A wrong MAC ends that
session only. Unpairing removes the phone and forgets the nonces it used;
nothing else is remembered, so the same phone can pair again from a new key.
The shared vectors are `crates/tauri-app/tests/fixtures/sync-vectors-v2.json`.

A device id belongs to the phone that paired it. This desktop authorizes by
that id — whose tombstones it honours, whose staged files a delete reaches,
whose refusals a pairing lifts — and a pairing key is open to whoever sees
the screen, so `/pair` refuses a device id that is already paired
here under another Ed25519 key: 409 with `{"error":"device_id_taken"}` and the
sentence the phone shows. A phone's device id, signing key and client
certificate live in one Keychain service (`afterFirstUnlockThisDeviceOnly`), so
a phone that lost its key has a new device id with it: an unpair and re-pair,
an erase and a restore from backup all present either the same id with the same
key or a new id, and pair as they always did. The remedy when a phone does meet
the refusal is on this desktop: Settings → Mobile Sync → the phone's menu →
Unpair, then show a new key.

Settings → Mobile Sync → a phone's menu → Rename names the phone on this
desktop, which tells apart several phones that all announce "iPhone". The
rename lives in the pairing table keyed by the phone's device id: it names
the phone in the list, in import toasts and in refusal lines, is kept when the
same phone pairs again, and goes with an unpair. The name the phone paired
with stays beneath it. A blank name clears the rename.

## The LAN server

Port 7845 on every interface, rustls with this machine's own certificate. It
runs only while the vault is open and either a phone is paired or a key is
showing, so a laptop with no phone opens no port. A pairing key that could not
bind reports the port conflict instead of showing a QR nothing can reach.

`/pair/start` and `/pair` are the routes without a paired client certificate.
Every other route needs
three things to agree: the certificate presented in the TLS handshake, the
device id in the header, and an Ed25519 signature over method, path, device id,
nonce and body hash. The nonce is the replay guard — iOS signs a background
upload when it queues it and sends it later, out of order — and accepted nonces
are kept per device in `sync.<machine>.nonces.json`.

Everything that can be checked from the handshake and the headers is checked
before the body is read: the presented certificate against the pairing table
and the device id it claims, then the body, then the signature over it (which
covers the body hash) and the nonce. A stranger that reaches the port allocates
nothing. The pairing routes have to read a body to check it, so they read one
only while a key is live and caps it at 64 KiB, against the 256 MiB every other route
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
`imported/<device>/` and `records/<device>/` — so two devices that minted the
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
hello names up to five inputs per session:
`manifest.json`, `transcript.json`, `speakers.json`, `notes.json` (the page
typed on the phone and the annotations pinned to transcript lines,
the iOS app's notes format) and `live_transcript.jsonl`, the phone's live
captions while its own transcript is unwritten (protocol §5d). When a
session's manifest and every chunk it names have arrived, along with every
other input the latest hello offered and asked for, the session is imported.
The latest hello's offer is the claim (`claims/<device>/<session>.json`). A
claim expires seven days after the last hello that listed the session
(`import::CLAIM_LIFETIME`); the session then imports from its manifest and
audio, and a returning phone is asked for the rest. Live captions are never
waited on for a session's first import: a locked iPhone cannot seal them and
an Android phone may not list them while locked, so they are asked for at
every hello and fold in whenever they arrive, before or after the import.
A session the library holds at every other revision waits for them
(`Claim::live_only`), so a hello whose only news is the captions brings
them over once instead of importing the manifest again at every hello.
Captions are staged with the hash of their plaintext beside them
(`import::stage`), which is the revision a hello names them by. A session that
has sent only its `meta.json` holds nothing under `sessions/`: it is a staging
stub, outside the sessions waiting to import, until its manifest arrives.
Unpairing a device drops everything it has staged here — session files,
claims, staged records — and the recordings stay on the device. Every file
is opened
(envelope v2 with this machine's X25519 key, or taken as it is when the phone
wrote it in the clear), the audio chunks included, so the day the phone starts
sealing `mic.m4a` the desktop reads it. Phone audio is 16 kHz mono AAC at
32 kbps in `.m4a`; the import decodes each chunk to the `mic.wav` beside it
that every desktop reader expects (transcribe, diarize, voice samples, the
mixdown), dropping the 2112 priming samples Apple's encoder writes ahead of
the audio (symphonia 0.5 decodes them and carries no delay metadata), so the chunk sits on the phone transcript's
timeline; the manifest names the WAV. A `.m4a` or `.wav` chunk that does not
decode refuses the session (`undecodable`) before anything is written; a chunk
in any other container refuses it as `unsupported`. The manifest gains
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
`inputs`; for `live_transcript`, which the phone seals afresh on every send
and names by its plaintext, the plaintext hash), its plaintext hash
(`content.<input>`) and the session's `audio`
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

Four files are written on both devices and merge instead of travelling one
way: the tag catalog `tags.json` and the contacts `contacts.json` (protocol
§5a), the voices `voices.json` (§5c), and each session's `meta.json` (§5b).
One module holds the rules all four share (`sync/records.rs`): the later
second wins, a deleted row leaves a tombstone, and unknown keys are the union
of both sides'. `tags.json` and `contacts.json` add what makes two of their
rows the same row. `voices.json` adds the same, plus the samples taken back
(`removed`) and the merge of each voice's samples by where they came from.
`meta.json` has no rows to match: it merges value by value, and each run's
names cell by cell.

A hello names each file by the hash of its **plaintext**, which is also what
travels — `inputs` hashes the sealed envelope, this does not. The profile
files sit under `profile` (hello keys `tags`, `contacts`, `voices`, uploaded
under `X-Daisy-Session: profile`); a `meta.json` sits under its session's
`meta` hash and uploads under its session's id. A revision this desktop holds,
or holds staged, asks for nothing; one it has merged is answered with the
merged file under `merged_from`; any other is wanted (`profile_wants` names
files, `meta_wants` names bare session ids). A phone that names no file is
given this desktop's with `merged_from` null, which it installs whole, and a
desktop that holds no file offers none: `voices.json` behaves as the others
do, and a vault holding no voice, no tombstone and no unknown key holds no
file. A phone that sends no `profile` key at all — a locked phone — takes no
part in the profile files. `<inbox>/records/<device>/<session>/<file>`
holds each staged copy (`profile` as the session of the profile files) and,
beside it, `<file stem>.merged.json`, the revision last merged from that
device with the ids that device has to rewrite. The profile files merge off
the connection on the import worker, and `meta.json` merges on the hello, so
the reply offers the settled file. The offer's header carries `ids` for a
profile file and none for `meta.json`.

The rules, the same on either side of the seam:

- **Time.** Every row and cell carries the second it was written
  (`updated_at_unix_seconds`). A missing second reads as 0. The later second
  wins; **a tie keeps the desktop's**; between two rows from one side, the
  smaller id. Each device writes that comparison from its own seat and
  inverts it; spelled the same way on both sides, a tie would have each keep
  its own answer.
- **Tombstones.** `tags.json`, `contacts.json` and `voices.json` carry
  `deleted`: `{"at","id"}` for a row deleted, `{"at","id","into"}` for a tag
  or contact merged into another (a voice's collapse carries no `into`; its
  references follow the offer's `ids`), kept for good. Both sides' tombstones
  join by id, the later `at` standing with its `into`, a tie keeping the
  desktop's. A row whose latest moment (`updated_at_unix_seconds`, else
  `created_at_unix_seconds`; for a voice, also its newest sample) is no later
  than its tombstone's `at` is dropped before matching, so a delete wins a
  tie; a row edited after the delete survives and merges as usual. `into` is
  followed until it reaches a live row; a chain that ends at a delete, a
  missing id or a cycle is a delete.
- **Rows that are one row.** The same id, or — a tag — the trimmed names equal
  ignoring case; — a contact — an email in common ignoring case, or the
  trimmed names equal ignoring case; — a voice — one `contact_id`. Two rows of
  one side collapse the same way. The row with the later edit stands (the
  time rule above), keeps the earliest `created_at_unix_seconds` and the
  latest edit, and — a contact — every email the others knew; — a voice — the
  union of the galleries. Every id that lost gets a tombstone at the later of
  its row's and the winner's latest moment.
- **Unknown keys**, at every level — the top of the file, a row, a tombstone,
  a sample, a sample tombstone, a `meta.json` cell — are the union of both
  sides', the desktop's value on a clash.
- **Bytes.** Compact JSON, keys sorted at every depth, slashes bare, non-ASCII
  raw, no trailing newline (`records::canonical`), with every array in a
  defined order: rows and tombstones by id, comparing bytes. `tags.json` is
  written this way now too; a file written pretty before hashes differently,
  so the next round merges it and every write after is canonical.
  `JSONEncoder` asked to indent writes `"key" : value` with no way to stop
  it, so neither side indents; each side pins the same literal bytes in a
  test, and decode-then-re-encode lands on those bytes again.

What a merge retires, and every local delete, runs one cascade
(`records::apply_retirements`): a tag moved or deleted moves or drops its
cells in each session's `meta.json`; a contact moved or deleted moves or
drops the contact on each speaker name and takes its voices with it (each
voice tombstoned at the later of the contact's `at` and its own latest
moment, so every sample it holds goes too); a voice moved moves the
voiceprint id on manifest speaker rows. Workflow conditions follow tags and
contacts. The voice cascade runs on every contacts merge over every delete
tombstone, so a merge that failed partway catches up on the next; the rest
follows only the tombstones new here. Deleting a voice in Settings → Voices
deletes the person: the contact with a tombstone (no `into`), which takes
every voice under it. Renaming a voice renames its contact. A voice the
enrolment absorbs as a duplicate is a voice tombstone only; the contact
stays. The device owner is the contact named "Me", which the pinned picker entry
writes.

Each file is read, changed and written under its own lock — `tags.json` and
`contacts.json` in their command modules, every `meta.json` in
`recording::session_meta` — held from the read to the write, whether a person
here or a merge of the phone's copy is writing. The vault's voices are
written under the vault's own lock.

A tag's `use_count` is `0` in `tags.json` on both devices and is never merged.
Each device counts a tag's uses from its own library when it reads the
catalog; the two libraries hold different sessions, so a stored count would
differ on each side and the file would never hash the same on both. The key
stays in the file because every phone up to build 20 fails to decode a tag
that lacks it.

`voices.json` lives in the vault, so the desktop answers for it only while
unlocked. Each voice carries its name, its contact and a gallery: its
enrolment sample (the one whose four identity keys are all absent), kept
whatever its age, and its first 16 other samples in sample order (newest
first, then by source). A sample is keyed by where it came from: the session under the
phone's id, the diarization run and the speaker, or, for a vector held
before samples had a source, a `legacy` number the `voice-samples` migration
gave out once. A sample stores its model and its vector as base64 of
little-endian f32, carried as the bytes that arrived. One source twice keeps
the later sample, then the desktop's, then the smaller vector by bytes.
Detaching a speaker from a voice leaves a sample tombstone in `removed`. A
voice named again after a delete keeps only the samples taken after it.
Matching is each device's own: the desktop scores a cluster by its best
cosine against the gallery.

`meta.json` holds what a person wrote on a meeting: the title, the tags, the
dismissed suggestions and the speaker names. They moved off the manifest
because the manifest travels one way and is re-asked every round: the import
writes the phone's copy over this desktop's. Every value is a cell stamped
with its second and settles on its own; a cell only one side has stands; a
tag detached is a stamped `"on": false` and is kept. A cell for a tag the
catalog has tombstoned moves to the tag that stands, or goes when the tag
was deleted; an id the catalog has never seen stays, since the catalog that
names it may not have merged yet. Speaker names sit in `speaker_maps`, one
map per diarization run keyed by the run's `diarization_id`, each map keyed
by cluster number. A cluster number is a first-appearance ordinal over one
run's labels and names the same person only within that run, so maps from
different runs sit side by side, cells within one run settle cell by cell,
and no run's map is ever pruned. Clearing a name writes a stamped cell with
`display_name` `""` and no contact, and never removes the key: an absent key
loses to any present one, so the other side's older name would come back.
Readers take `""` as unnamed, as they take a cleared title `""`; removing a
speaker cluster clears its name the same way. A device's current run for a
session is the `diarization_id` of the speaker table it reads for that
session. `speakers.json` carries it at its top: the phone writes the run that
produced the table, so on a phone-diarized session both devices read and
write that run's map. A recording made here has no `speakers.json`, and its
run sits on the manifest's `diarization_id`, which never travels. Every
diarization here mints a fresh run and records it on the table it rewrote
(`speakers.json` when the session has one, else the manifest), and carries
the names of the clusters it kept into the new run's map, a step on this
machine and never a merge rule. A session diarized before runs were recorded
gets a run of this machine's own at the next unlock, with the names its
manifest shows (`migrations.rs`, `2026-09-diarized-sessions-get-a-run`). A
phone session whose transcript lands with clusters and no `speakers.json`
gets one at import, until the table lands and names its run. A participant
added by hand to a session no run has numbered starts a run of this
machine's own. A session with no run has no clusters, and naming one is an
error. A `speakers.json` that names no run (a phone up to build
20) has the run `"legacy"`, a constant both devices read the same way: its
names live at `speaker_maps["legacy"]`, a name typed here is written there at
once, and nothing is ever minted for a `speakers.json` table. A voice sample
taken from a cluster of such a table keeps `diarization_id` absent, so the
samples taken before still match. Each device reads
and edits only the map of its own current run. The cosine and the email never travel — a
score measured against one run's centroids means nothing under another
device's clustering — and the manifest row keeps them. `meta.json` sits
outside every artifact's `built_from`, which is `{}` and unchecked: a rename
or a tag toggle reaches nothing a summarizer read.

The manifest's `title`, `tag_ids`, `dismissed_suggested_tag_ids` and the
current run's rows of `speaker_map` are a projection of `meta.json`, written
after every change to `meta.json` and again after every import writes the
phone's manifest. The manifest stays on this desktop; of a session's files,
only `summary.json` and `notes.md` travel to the phone. This desktop's
screens, search and summaries read the projection alone:
`meta.json` is the only place the desktop reads or writes a speaker's name,
and `speakers.json` is read only for its `diarization_id` and rewritten only
to drop retired clusters or record a run started here — never for `name`,
`name_source` or `match_cosine`, which ride through a rewrite as bytes the
desktop does not look at. A session with no `meta.json` reads its manifest
stamped at `finalized_at_unix_seconds`, falling back to
`created_at_unix_seconds`, its names under the manifest's run; both devices
derive the same baseline from the same manifest without agreeing on
anything, and the first edit writes the file. `meta.json` files written with
one run's names at the top (`diarization_id`, `speaker_map`) are moved under
`speaker_maps` at the next unlock (`migrations.rs`,
`2026-09-meta-speaker-maps`), and the run they named is recorded on the session's speaker table.
A table naming a cluster `meta.json` says nothing about yet — the read having
moved off it before that name ever reached `meta.json` — is copied in once,
at the next unlock (`2026-09-table-names-to-meta`); a cell `meta.json`
already holds for that cluster, cleared or not, is the later decision and
stands.
The inbox's earlier `profile/` and `meta/` staging areas are dropped at the
next unlock (`2026-09-inbox-records-<machine>`), and the next hello asks for
what they held.

`tests/fixtures/sync-seam/` pins each file's two inputs and their merge:
`tags.*-v2.json`, `contacts.*-v2.json`, `voices.*-v1.json` and
`voices.*-v2.json` (unknown keys clashing at every level), and
`meta.*-v1.json` (two runs, a stamped removal, cells for a deleted and a
merged tag, unknown keys at every level; the merge followed through
`tags.merged-v2.json`'s tombstones), `speakers.phone-v2.json`, a phone's table naming its run, and `speakers.phone-legacy.json`, one naming none (its names are the `legacy` map in the meta fixtures). Every file re-encodes to itself, the
inputs merge to the pinned bytes, and merging the result with the phone's
input changes nothing. The phone asserts the same bytes.
`live_transcript.phone-v5.jsonl`, `hello.live.json` and
`transcript.phone-v5.json` pin protocol §5d: the recorder's live lines, which
promote onto `manifest.phone-v4.json`'s chunks; a hello naming them by their
plaintext; and a phone transcript carrying `edited_at_unix_seconds`.

`notes.json` is phone-owned: it lands beside `speakers.json`, Ask indexes one
chunk per note line and per annotation (`[m:ss] Note: …`, `[m:ss] Note on
Alex: …`), and Library search reports a hit in it as Notes. The desktop never
edits it and never sends one back. It sits outside every artifact's
`built_from`: a note typed after a summary was built changes nothing the
summary read, so the phone keeps holding that summary as current.

`notes.md` is desktop-owned: the notes typed here on a phone's session are
offered beside `summary.json` (§7), by the hash of their bytes, with
`built_from` `{}`, since nothing the phone holds is an input to them. Cleared
notes travel as an empty file. Only UTF-8 up to 256 KiB travels. The text is
the markdown the notes editor writes (tiptap-markdown: CommonMark with no raw
HTML, and a single newline is a line break).

A session this desktop cannot open is refused at the revisions it arrived at:
a file sealed to a key this machine does not hold (`sealed`), a manifest or
envelope schema newer than this build reads (`unsupported`), or an audio chunk
that does not decode, such as a recording that captured no sound
(`undecodable`). The refusal is
recorded under `refused/<device>/` in the inbox and the staged copies go; a
hello naming those revisions asks for nothing, one naming new revisions brings the session
over again. `sync_status` lists the refused sessions, Settings → Mobile Sync
shows them with the remedy (pair the phone again, update Daisy, or delete the
recording on the phone), and a
`sync:refused` event raises a toast when one is recorded, naming the recording
by its title (from its staged `meta.json`, else its manifest, without control
or bidi characters and cut to 80 characters) and the time its id was minted. Pairing a phone again
lifts its refusals, so what it offers next is tried afresh; unpairing it and a
delete on either side drop them. A session still missing a file stays staged
and is tried at the next hello.

Then `transcript.md`, the summary and the chapters, each best-effort. There is
nothing to transcribe and no system track or echo to cancel. All of it runs off
the connection: importing copies audio and summarizing takes minutes, and the
phone is waiting.

### The transcript

A phone session's transcript is, in order: the phone's `transcript.json`;
without one, its live captions promoted into a provisional transcript (final
mic lines only, onto the manifest's chunks, one mic track per chunk, marked
`"provisional": true`, a key that stays on this desktop); without either,
none. A session with no transcript lands as a recording made here does before
it is transcribed: the Transcript tab says there is none yet and Regen.
Transcript runs a pass over its audio. A snapshot's torn last line (no newline
after it) is dropped. Live captions are not asked for beside a phone
transcript, or once one is on record. Captions this desktop cannot open, or
over 16 MiB (`import::LIVE_MAX_BYTES`), are recorded at the revision offered
and not read; the session imports without them. At most 200 000 lines are
read, and a line that is not UTF-8 or does not decode is skipped.

A transcript arriving for a session already here replaces the library's
(`import::replaces`) when the library holds none, or holds the copy an earlier
import wrote (`written.transcript` in the import record is its stamp:
`transcribed_at`, `edited_at`, provisional), or a copy stamped as the arriving
one is; a phone's next transcript replaces its last one that way whatever
their stamps. Otherwise the library's copy was made here — Regen. Transcript,
Polish Transcript, or an edit of the provisional one — and the two rank by
`max(transcribed_at_unix_seconds, edited_at_unix_seconds)`, a provisional
transcript by its `edited_at` alone: the higher wins and a tie keeps the
library's. Polish Transcript stamps `edited_at_unix_seconds` on
`transcript.json`; naming speakers and diarization do not. Live captions
never replace a transcript that is not provisional. A replaced transcript
takes its `transcript.dedup.json` with it, and the pipeline renders, then
summarizes, the new one. A phone transcript that does not replace the
library's is recorded as declined (`rejected.transcript`,
`rejected.content.transcript`) and as the transcript revision on record, so
the summary's `built_from` names what the phone holds and no hello asks for
those bytes again. While the library keeps a transcript of its own — one it
declined the phone's for, or one whose stamps differ from what the last
import wrote — a phone `speakers.json` stays out, since it numbers the
clusters of a transcript the library does not hold; it is recorded as
declined (`rejected.speakers`), never as held, and is asked for again with
the next phone transcript fetched to replace the library's. While the
library's transcript is provisional, a hello asks for any phone transcript it
has not declined.

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
the imported session and whatever of it is still staged: its files, the
claim the import waits on, a refusal, and a staged `meta.json`. Burying a
session and writing an import into the library take one lock
(`import::library_write_lock`), and an import checks for a tombstone under it
before it writes anything, so a session deleted as it lands is not imported
and reports nothing. When files or a claim of the session were still waiting,
the `sync:withdrawn` event takes it out of the import toast, which acts only
while that session is still on its way in. Deleting a phone session here writes a signed tombstone
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
and nothing else, no local name, no manufacturer data — with three
characteristics (GATT profile 2, protocol §9a): a wake notify, a challenge the
phone writes, and an address it reads. Everything is sealed under that
pairing's key; the challenge uuid derives from that key, so existing pairings
carry it without pairing again.

- **Challenge.** A 16-byte write is kept against the writing central, and the
  address is sealed at once with `daisy-gatt-2/addr || challenge` as
  associated data. Any other length answers Invalid Attribute Value Length.
  Each service holds at most 8 centrals; the one whose last write is oldest
  goes first.
- **Address.** A read answers the bytes sealed at that central's last
  challenge; a central that wrote none gets Read Not Permitted. When the
  listener state is applied and the address has moved, every held reply is
  resealed under its own challenge.
- **Wake.** A notify sealed with `daisy-gatt-2/wake` carries a fresh `n`,
  which stays pending for 30 s on the monotonic clock and rides in every
  address sealed meanwhile as `w`. Reads never consume it. A wake goes out
  when a session's summary and chapters finish.

The per-central state lives in `sync::peripheral` (`Binding`); the Swift shim
routes each write and read to it by service and central identifier.
Unpairing or pairing a phone again restarts the radio, so only current
pairings are advertised.

Elsewhere there is no peripheral role, and a phone syncs whenever its app is
open on the same network. Settings → Mobile Sync says which of those three
states this machine is in.

Two CoreBluetooth behaviors: a Mac does
not see its own advertisements, so discovery needs a second device
(`scripts/ble-scan.sh`), and `updateValue` answers true with no central
subscribed, so the subscriber count is what says a phone is listening.

## Versions

The wire contract is the sync protocol document shared with the iOS app; §11
carries the versioning rules.

**Nothing load-bearing goes in a header.** `HelloReply` and the artifact
header are closed shapes on the phone: a key neither side declares is dropped
on the way in, so a new header field reaches an older peer as silence rather
than an error. `SessionManifest` and every record (`SessionMeta`, `Tag`,
`Contact`, `Voice` and each of their parts) keep what they do not recognise.
Both devices on one `v` know the same artifact and profile names, and **a
name in a reply that the phone does not know, or that is malformed, refuses
the whole reply** — the summary offer, the uploads and the tombstones with it.
A new name is a contract change and bumps `v`.

**The version gate.** This desktop speaks `v` 5 (`exchange::PROTOCOL_V`), and
`v` bumps on every change to the contract. A phone syncs only on the same `v`.
The hello route reads the body's `v` and `device_id` (`sync::gate::check`)
straight after the nonce, before the last-seen record, the staged import, the
tombstones or the profile hashes. On any other `v` it answers a signed 426
`{"v": 5}` and changes nothing but `sync.<machine>.gate.json`, which records the
side whose `v` is lower. That record sets `out_of_date` on the device's row in
Settings → Mobile Sync, which shows "Your desktop / iOS device / Android device
is out of date. Please update it to complete the sync. Sync has NOT been done."
in place of the last-seen line, and each change of side raises the same words
as a toast (`sync:gated`). A hello that passes, pairing again or unpairing
clears it. `/pair` is not gated. Every artifact header this desktop signs
(summary, notes, profile files, `voices.json`, `meta.json`) carries `v` 5;
record formats keep their own: the pairing code and exchange and the tombstone
record stay 1, `summary.json` stays 2.

`summary.json` is written as contract v2 by
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
| `cargo test -p tauri-app --lib sync::peripheral` | the challenge store: binding, eviction, pending wake, reseal |
| `cargo test -p ble` | the callback bridge from the shim to a responder |
| `scripts/test-swift-shim.sh` | the shim's ATT answers: Read Not Permitted before a challenge, lengths, offsets |
| `cargo test -p ble -- --ignored` | the radio: powered on, service published, advertising |

Regenerating the vectors after any change to a derivation, a wire body or a
signing string:

```sh
cargo test -p tauri-app --test sync_vectors -- --ignored write_vectors
```

Copy `crates/tauri-app/tests/fixtures/sync-vectors-v1.json` into the phone-side
test fixtures.

## Windows notes

- The first `Show pairing code` binds 0.0.0.0:7845 and Windows raises its
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
