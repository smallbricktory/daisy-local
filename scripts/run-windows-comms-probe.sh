#!/usr/bin/env bash
# Run the WASAPI communications-mode probe natively on Windows, from WSL.
#
#   scripts/run-windows-comms-probe.sh default|comms|comms-no-ns
#   scripts/run-windows-comms-probe.sh duck-default|duck-comms
#
# One run is one 23 s pass in one mode. Speech plays out the render endpoint
# while the mic records; beeps mark the windows (LONG = stay quiet, 3 SHORT =
# talk, 2 SHORT = stop). Prints the effect list and per-window mic RMS.
#
# Run `default` and `comms` back to back with the speaker volume untouched and
# the same speaking level: the result is the comparison between the two, so any
# change you make between runs lands in the numbers as a fake effect.
#
# Captured WAVs are copied back to diagnostics-out/comms-probe/.
set -uo pipefail

source "$(cd "$(dirname "$0")" && pwd)/lib/stage-windows.sh"
OUT_DIR="$REPO/diagnostics-out/comms-probe"

stage_windows || exit 1
mkdir -p "$OUT_DIR"

MODE="${1:-}"
case "$MODE" in
  default|comms|comms-no-ns|duck-default|duck-comms) ;;
  *) echo "usage: $0 default|comms|comms-no-ns|duck-default|duck-comms" >&2; exit 2 ;;
esac

# Build first, so the run itself starts the moment you press enter and the
# beeps are not preceded by a minute of silent compiling.
$PS -NoProfile -Command \
  "cd C:\\tmp\\daisy-build; cargo build -q -p audio-engine --example wasapi_comms_probe" \
  >/dev/null 2>&1

# No pipe on the run: a pipe re-buffers the child's stdout and the live cues
# arrive only after the probe has already finished.
$PS -NoProfile -Command \
  "cd C:\\tmp\\daisy-build; \$env:WASAPI_COMMS_PROBE_DIR='C:\\tmp\\daisy-build\\probe-out'; \
   \$env:WASAPI_COMMS_PROBE_MODE='$MODE'; \
   cargo run -q -p audio-engine --example wasapi_comms_probe; 'PROBE_RC='+\$LASTEXITCODE"
rc=$?

cp "$STAGE"/probe-out/*.wav "$STAGE"/probe-out/summary_*.txt "$OUT_DIR"/ 2>/dev/null &&
  echo "results → $OUT_DIR"

exit $rc
