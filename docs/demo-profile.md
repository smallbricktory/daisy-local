# The demo profile

A Daisy profile holding one invented person's recordings, for screenshots,
walkthroughs and demos. Nothing in it is real: every person, company, client
and meeting is fiction.

## The persona

**Jordan Reyes**, 33, a product designer at Tidepool — an invented eleven-person
Gastown startup selling booking and payments software to small service
businesses. The corpus runs from mid-August to September 11 2026 and carries one
continuous story: the PM resigns, Jordan picks up the roadmap, research keeps
pointing at payments rather than the booking flow, and an accessibility audit
lands a month before the deadline.

The cast is stable across recordings, so speaker names, contacts and tags line
up wherever you open the app:

| Person | Role |
| --- | --- |
| Jordan Reyes | the recorder ("Me") |
| Priya Raman | product manager, leaving October 9 |
| Simone Okafor | engineering lead |
| Wes Tanaka | iOS engineer |
| Devon Clarke | content designer |
| Harriet Ng | CEO |
| Renee Beaulieu | design mentor |
| Bea Lindqvist | mentee |
| Hannah Boyd | recruiter |
| Owen Mbeki | senior designer candidate |
| Marisol Vega, Terrence Dube | research participants |

Twelve tags carry the taxonomy — Design System, User Research, Q4 Planning,
Hiring, Accessibility, 1:1, Vendor, Mentoring, Personal, Job Market, Trips,
Spain 2027 — most with a summary directive and a vocabulary list, so the
tag-prompt and tag-vocab features have something to show. Trips and Spain 2027
file only the phone's recordings.

A Google Calendar ICS feed is subscribed in the profile's vault under the name
"Jordan Reyes": 66 events from September 11 to October 17 2026, in
America/Vancouver, covering the same life the recordings do.

## Where it lives

| Path | What it is |
| --- | --- |
| `~/Sync/daisy/profile-test` | the working demo profile; launching Daisy on it writes to it |
| `~/Sync/daisy/profile-demo-golden` | the pristine copy, restored from |
| `~/Sync/daisy/run-demo-profile.sh` | launcher and restore |

## Running the app on it

```sh
~/Sync/daisy/run-demo-profile.sh            # launch on the demo profile
~/Sync/daisy/run-demo-profile.sh --restore  # put the working copy back first
~/Sync/daisy/run-demo-profile.sh --where    # print the paths
```

The launcher exports `DAISY_PROFILE_DIR`, so the platform-default profile
(`~/Library/Application Support/ai.daisy.Daisy`) is untouched.

## What each recording carries

Fourteen sessions, 2 to 5 minutes each, written the way a finalized desktop
recording leaves disk: `manifest.json`, `transcript.json`,
`transcript.dedup.json`, `transcript.md`, `summary.json`, `summary.md`,
`chapters.json`, `live_transcript.jsonl`, `finalize.status.json`, and
`meeting.opus`. Raw chunk wavs are absent, which is the state a profile is in
when raw audio was dropped after finalize.

`Design crit - Q4 design system` is the screenshot session: four speakers with
real synthesized speech, each voice distinct, the local speaker on the left
channel and everyone else on the right, exactly as the app records a call. Its
transcript timings were measured from that audio, so clicking a line seeks to
the words. Every other session carries a shaped low-level track: the waveform
follows the conversation, and there is nothing to hear.

## Rebuilding it

`scripts/demo-corpus/` holds the generator: the cast and tag taxonomy in
`cast.py`, the dialogue in `sessions_*.py`, and the writer in `build.py`.

```sh
# the whole corpus into a profile, hero audio included when it is on disk
python3 scripts/demo-corpus/run.py ~/Sync/daisy/profile-test \
    target/demo-corpus/hero_pcm.raw

# the hero session's speech, from the OpenAI key at ~/.secrets/.openai-dyoo
OPENAI_API_KEY=... python3 scripts/demo-corpus/hero_tts.py
```

`run.py` rewrites `tags.json` and `contacts.json` and writes into `sessions/`;
it leaves the rest of the profile — vault, settings, sync pairing — alone.
Audio goes through `cargo run -p recording --example pcm_to_opus`, so the demo
files are encoded by the same code the app uses.

Day to day, restore from `profile-demo-golden` rather than regenerating: the
hero session's speech costs an API call per turn, and the golden copy is the
one the screenshots were taken against.

## The phone half

Jordan Reyes records on a phone too, and that half lives in the
`smbr-daisy-ios` repo as the `jordan` demo library: sixteen recordings made
away from the desk — two shop visits, a walk back, seven calls, a physio
appointment, and a trip to Spain settled at a kitchen table — under six of the
tags above, spelled with the same names and colours so a tag synced between
the two devices is one tag.

It is seeded into a Simulator or a phone by the app's own seeder, because
sessions on the device are sealed at rest and cannot be written from
outside it:

```sh
TEST_RUNNER_DAISY_SEED_DEMO=device TEST_RUNNER_DAISY_DEMO_UNIVERSE=jordan \
  xcodebuild test -scheme Daisy -destination "platform=iOS,id=<udid>" \
  -derivedDataPath .build/dev -only-testing:DaisyTests/DemoSeedTests
```

`smbr-daisy-ios/docs/demo-library.md` carries the rest: how the transcripts are
generated, and the Simulator form of the command.
