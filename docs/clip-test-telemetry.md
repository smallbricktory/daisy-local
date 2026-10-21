# Clip-test telemetry

Every desktop recording writes caption timing, engine cost and finalize
timing next to its audio, so a clip test (a known multi-voice clip played into
a recording) can be scored for live-caption lag, accuracy and cost per engine
on each OS.

## What a session holds

| file | written by | contents |
|---|---|---|
| `live_transcript.jsonl` | live pipeline | every interim and final caption line, with ms receive time and the engine that produced it |
| `telemetry.jsonl` | recorder, live pipeline, finalize | header, feed anchors, committed words with first-interim times, engine decode timing, process samples, finalize stages |

Both are on for every recording on macOS, Windows and Linux, in dev and
release builds alike. They stay on the machine: nothing in them is sent
anywhere. Session folders hold meeting content in plaintext
(`docs/SECURITY.md`, "Meeting content"), and these files contain words, so
they get the same care as the transcript: share a session folder only with
people who may read the meeting.

### Where the session folder is

`<profile>/sessions/<session-id>/`, where `<session-id>` is
`daisy-<unix seconds at record start>` (newest = largest number).

Default profile folder:

| OS | profile folder |
|---|---|
| macOS | `~/Library/Application Support/ai.daisy.Daisy/` |
| Windows | `%APPDATA%\daisy\Daisy\data\` |
| Linux | `~/.local/share/daisy/` (`$XDG_DATA_HOME/daisy/` when set) |

A profile picked in the first-run wizard, or the `DAISY_PROFILE_DIR`
environment variable, replaces the default. Settings → Profile shows the
folder in use.

## Getting the files off a machine

The analysis reads the session folder as it is on disk. Copy the whole
folder (`telemetry.jsonl` and `live_transcript.jsonl` are the two the report
needs).

From the app, on any OS:

1. Settings → Display → Advanced: turn on **Nerd Mode**.
2. Settings → Profile → **Open Profile Directory**. The OS file browser opens
   on the profile folder.
3. Open `sessions`, then the newest `daisy-…` folder.
4. Copy that folder (or zip it) to the machine that runs the analysis.

From a shell, copying the newest session into the current directory:

macOS (Terminal):

```bash
S=~/Library/Application\ Support/ai.daisy.Daisy/sessions
cp -R "$S/$(ls -t "$S" | head -1)" .
```

Linux (any shell):

```bash
S="${XDG_DATA_HOME:-$HOME/.local/share}/daisy/sessions"
cp -R "$S/$(ls -t "$S" | head -1)" .
```

Windows (PowerShell, not elevated):

```powershell
$s = Get-ChildItem "$env:APPDATA\daisy\Daisy\data\sessions" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Copy-Item $s.FullName -Destination . -Recurse
```

Replace the sessions path with the one Settings → Profile shows when the
profile lives elsewhere.

A session the desktop did not record (a phone recording, an imported file)
gets only the finalize `stage` lines in `telemetry.jsonl`. The report prints
whatever the folder holds: accuracy needs `live_transcript.jsonl`, the timing
sections need `telemetry.jsonl`.

## Running the analysis

Python 3, standard library only:

```bash
python3 scripts/clip-test-analyze.py <session-folder> --ref <reference> [--offset SEC | --window A:B] [--json]
```

- `--ref`: the clip's reference transcript, plain text or timed TSV lines
  `start_ms<TAB>end_ms<TAB>text` (the eval-corpus format).
- `--offset SEC`: with a timed reference, the session second at which the
  clip's zero played. The report then scores only the clip's span, padded by
  1 s before and 3 s after.
- `--window A:B`: score only session seconds A to B.
- `--json`: the same report as JSON, for comparing machines.

WER drops the fillers um, uh, umm, uhh, uhm, erm, hmm and mm from reference
and hypothesis, lowercases, and strips punctuation (word-internal apostrophes
stay). Hypothesis text is the live final lines per track, and both tracks
merged in time order.

Fictional example (a Linux desktop playing a clip through its speakers into
the system track):

```
session daisy-1790000000
  app 1.3.2-f (1a2b3c4) · linux Linux 24.04 Ubuntu · Example CPU 8000 (16 logical, 8 physical)
  live realtime: nemotron "Nemotron Streaming 0.6B" · mic "Desk Mic" · system "Speakers"
  window 9.0-1213.0 s session time

caption lag (s)                      words  in-interim  first-interim p50/p90  final p50/p90
  mic     nemotron                       41          38      0.44 / 0.81        2.61 / 4.90
  system  nemotron                     2894        2851      0.39 / 0.77        2.48 / 4.62

accuracy (fillers removed)            WER    sub   ins   del   ref   hyp
  live mic [nemotron]                0.986     3     0  2902  2943    41
  live system [nemotron]             0.141   236    61   118  2943  2886
  live both tracks [nemotron]        0.152   239    96   112  2943  2927

decode                               steps  cost p50/p90/max ms   RTF     backlog max ms
  mic     nemotron                     2160  48/71/312           0.0921  340
  system  nemotron                     2160  52/77/298           0.0987  320

process  samples 241 · cpu mean 41.3% max 88.0% (of one core) · rss peak 2210 MB · threads max 61
  daisy-live-pipe                34.9% mean
  tokio-runtime-w                 2.1% mean

finalize run 1: 38.20 s
  finalizing         0.01
  echo-cancelling    6.10
  transcribing       0.40
  deduping           9.80
  summarizing       17.30
  chaptering         3.90
  compressing        0.70
  done
```

The script's tests run on a fictional fixture:

```bash
python3 -m unittest discover -s scripts/tests
```

### Replaying a clip without a live recording

`clip_replay` feeds WAVs through the recording's live pipeline in real time
and leaves the same two files in an output folder:

```bash
cargo run --release -p tauri-app --example clip_replay -- <nemotron-model-dir | sa> <system.wav> <out-dir> [mic.wav]
python3 scripts/clip-test-analyze.py <out-dir> --ref <reference>
```

`sa` selects SpeechAnalyzer (macOS). The WAVs are 16 kHz mono; the mic track
carries silence when no mic WAV is given.

## How the numbers are defined

- **Anchors.** Each track records `anchor` points: audio up to `audio_ms` had
  reached the live engine at `wall_ms`. A word's audio end in wall time is the
  latest anchor at or before it, plus the remaining audio. Anchors are taken
  where audio enters the engine, after capture and echo cancellation; a
  paused recording or a stall anchors again on the next frame.
- **First-interim lag** (per committed word): receive time of the first
  interim that showed the word with its final text, minus its audio end.
  Words that went straight to a final count in `words` and are absent from
  `in-interim`.
- **Final lag** (per committed word): receive time of its final, minus its
  audio end.
- **Decode** (Nemotron, whisper): `cost_ms` is the wall time of each decode
  step (one per 560 ms chunk for Nemotron), RTF is total cost over the audio
  those steps advanced, backlog is queued audio a decode turn drained.
- **Results** (SpeechAnalyzer): arrival time of each volatile or final result
  batch minus the wall time of its audio end, and how much audio the
  recognizer had been fed beyond that end.
- **Process**: CPU is percent of one core (200 = two cores busy), sampled
  every 5 s for the recording's lifetime; RSS is resident memory. Per-thread
  CPU is grouped by OS thread name on Linux (names cut to 15 characters) and
  macOS; Windows reports the process only.
- **Finalize**: a stage runs from its `stage` line to the next one; `done`,
  `error` and `awaiting-labels` end a run.

Wall times come from the system clock; a clock step during a recording
(NTP correction, manual change) shifts the lags around it.

## Schema

### `telemetry.jsonl` (`v` = 1)

Every line: `v` (schema version), `kind`, `wall_ms` (Unix ms when the line
was produced). Audio times are session ms: audio since the session's first
mic frame. Tracks are `"mic"` or `"system"`.

| kind | fields |
|---|---|
| `header` | `app_version`, `build_sha`, `os`, `os_version`, `arch`, `cpu_model`, `logical_cores`, `physical_cores`, `session_id`, `session_start_wall_ms`, `live_mode` (`realtime`/`off`), `engine` `{engine, model}` or null, `fallbacks` `[{engine, model}]`, `mic_device`, `system_device`, `sample_rate` |
| `anchor` | `track`, `audio_ms` |
| `words` | `track`, `engine`, `words`: `[[text, start_ms, end_ms, first_seen_wall_ms or null], …]`; `wall_ms` is the final's receive time |
| `decode` | `track`, `engine`, `decoded_ms` (stream position decoded through), `step_ms` (audio this turn advanced), `cost_ms` `[ms per step]`, `backlog_ms` |
| `result` | `track`, `engine`, `audio_end_ms`, `fed_ms`, `final` (bool), `words` (count) |
| `proc` | `cpu_pct`, `rss_mb`, `threads` (null on Windows), `thread_cpu` `[[name, pct], …]` busiest first, up to 8 |
| `stage` | `stage` |
| `end` | `dropped`: lines lost to a full writer queue |

Telemetry lines go through a queue to one writer thread that writes each
line whole, so the audio and decode threads never wait on disk. A full queue
(8192 lines) drops lines and counts them in `end`.

### `live_transcript.jsonl`

| field | meaning |
|---|---|
| `track` | `mic` or `system` |
| `start_ms`, `end_ms` | session audio time of the line |
| `text` | caption text |
| `final` | true for committed finals |
| `kind` | `final` or `interim` |
| `received_at_ms` | Unix ms the line reached the pipeline |
| `received_at_unix` | the same in seconds |
| `engine`, `model` | live engine id and model label |
| `words` | finals only: `[{text, start_ms, end_ms}, …]` per word on the session clock; the texts joined by single spaces equal `text`. Absent on interims, markers and lines from older builds |

Interim lines repeat the open utterance as it grows; a later interim or final
replaces them. Finalize reads only `final` lines.
