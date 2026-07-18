# Test fixtures

Two committed fixture pairs anchor the cross-correlation invariant tests.
Tests skip cleanly when these are absent (CI safe).

## `same_source_30s.{a,b}.wav` — negative control

Two streams capturing **the same** PipeWire source. Demonstrates the bug
class we hit during the Python attempt (loopback fell back to the default
input, both channels captured the user's mic). The invariant test asserts
this pair has best correlation > 0.85 — i.e., the test infrastructure
recognizes when both streams are the same source.

How to record (assumes `cargo build` ran; sources visible via `daisy sources`):

```bash
DAISY=./target/debug/daisy
MIC_ID=$($DAISY sources | awk '/^[[:space:]]*\[mic / {gsub(/.*id=|[[:space:]].*/,""); print; exit}')
$DAISY capture --mic $MIC_ID --system $MIC_ID --duration-secs 30 \
    --out /tmp/same
mv /tmp/same/mic.wav crates/audio-engine/tests/fixtures/same_source_30s.a.wav
mv /tmp/same/system.wav crates/audio-engine/tests/fixtures/same_source_30s.b.wav
```

## `different_sources_30s.{mic,sys}.wav` — positive

Real mic + real Speaker-monitor, while audio plays through the system
speakers (e.g. a YouTube video). Demonstrates the typical no-headset
capture: mic has user voice + speaker bleed; system has clean playback.

How to record:

```bash
DAISY=./target/debug/daisy
MIC_ID=$($DAISY sources | awk '/^[[:space:]]*\[mic / {gsub(/.*id=|[[:space:]].*/,""); print; exit}')
SYS_ID=$($DAISY sources | awk '/Monitor of sof-hda-dsp Speaker/ {gsub(/.*id=|[[:space:]].*/,""); print; exit}')
# Start a podcast or speech clip playing through the system speakers, then:
$DAISY capture --mic $MIC_ID --system $SYS_ID --duration-secs 30 \
    --out /tmp/different
mv /tmp/different/mic.wav crates/audio-engine/tests/fixtures/different_sources_30s.mic.wav
mv /tmp/different/system.wav crates/audio-engine/tests/fixtures/different_sources_30s.sys.wav
```

## `meeting_2min.{mic,sys}.wav` — real-meeting integration fixture

A real (or simulated via two browser tabs playing different speech) 2-minute
conversation recorded with `--virtual-sink` mode. The system channel contains
only the audio routed into daisy-capture (the meeting / playing tab); the mic
channel contains the local mic with whatever bleed the room produced.

How to record:

~~~bash
DAISY=./target/debug/daisy
MIC_ID=$($DAISY sources | awk '/^[[:space:]]*\[mic / {gsub(/.*id=|[[:space:]].*/,""); print; exit}')

# Option A: with --auto-route (parses both legacy and PipeWire 1.4.7+/1.6.x
# `wpctl status` formats)
$DAISY capture --virtual-sink --auto-route --mic $MIC_ID --duration-secs 120 \
    --out /tmp/meeting2min

# Option B: manual route via pavucontrol — start audio playing, then in
# pavucontrol's Playback tab change the app's output sink to "Daisy_Capture",
# then run capture WITHOUT --auto-route:
$DAISY capture --virtual-sink --mic $MIC_ID --duration-secs 120 \
    --out /tmp/meeting2min

# Either way, move the result into fixtures/:
mv /tmp/meeting2min/mic.wav    crates/audio-engine/tests/fixtures/meeting_2min.mic.wav
mv /tmp/meeting2min/system.wav crates/audio-engine/tests/fixtures/meeting_2min.sys.wav
rm -rf /tmp/meeting2min
~~~

## Storage discipline

WAV files are committed (not LFS). Two pairs at 30 s mono 16 kHz totals
~3.6 MB. Don't add more without a clear reason; large fixtures push to
LFS or external hosting.
