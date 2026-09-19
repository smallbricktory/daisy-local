#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
#
# The seam, both ends real: this desktop's sync server against Daisy for iOS
# running on the Simulator. The desktop half is
# `cargo test -p tauri-app --test seam_live`, which shows a pairing code and
# waits; the phone half is DaisyUITests/DesktopSyncLiveUITests, which types
# that code in and asks for a sync.
#
#   scripts/test-seam-sim.sh
#
# Needs the iOS repo beside this one (DAISY_IOS_REPO to move it) and Daisy
# itself closed: the pairing reply names port 7845, so the phone will use
# that port and nothing else.
#
#   DAISY_SIM_DEVICE   simulator to use, default "iPhone 17 Pro"
#   DAISY_IOS_REPO     default ~/repos/smbr-daisy-ios
#   DAISY_LIVE_HOST    what goes in the code, default 127.0.0.1 (the
#                      Simulator reaches the host's loopback)
#   DAISY_SEAM_BOTH_WAYS=1  also require the desktop's tags to reach the
#                      phone, and the phone's profile to reach the desktop.
#                      Protocol 5a is written on both sides; this is the run
#                      that says whether the round trip works.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
IOS=${DAISY_IOS_REPO:-$HOME/repos/smbr-daisy-ios}
DEVICE=${DAISY_SIM_DEVICE:-iPhone 17 Pro}
BUNDLE=com.smallbricktory.daisy
DEST="platform=iOS Simulator,name=$DEVICE"
DERIVED=$IOS/.build/seam
WORK=$(mktemp -d "${TMPDIR:-/tmp}/daisy-seam.XXXXXX")
CODE_FILE=$WORK/pair-code.txt
DESKTOP_LOG=$WORK/desktop.log
PHONE_LOG=$WORK/phone.log
PHONE_DONE=$WORK/phone-done

[[ -d $IOS ]] || { echo "no iOS repo at $IOS — set DAISY_IOS_REPO" >&2; exit 2; }

DESKTOP_PID=
cleanup() {
  [[ -n $DESKTOP_PID ]] && kill "$DESKTOP_PID" 2>/dev/null || true
  # The pairing this run left on the phone would make the next one skip.
  TEST_RUNNER_DAISY_SEED_SYNC=clear xcodebuild test-without-building \
    -scheme Daisy -destination "$DEST" -derivedDataPath "$DERIVED" \
    -only-testing:DaisyTests/SyncFixtureSeedTests >/dev/null 2>&1 || true
  echo "== logs kept in $WORK =="
}
trap cleanup EXIT

if lsof -nP -iTCP:7845 -sTCP:LISTEN >/dev/null 2>&1; then
  echo "port 7845 is taken. Quit Daisy and run this again — the pairing reply" >&2
  echo "names that port, so the phone cannot be sent anywhere else." >&2
  exit 1
fi

echo "== building the phone =="
cd "$IOS"
xcodegen generate >/dev/null
xcrun simctl boot "$DEVICE" 2>/dev/null || true
xcrun simctl bootstatus "$DEVICE" -b >/dev/null
xcodebuild build-for-testing -scheme Daisy -destination "$DEST" \
  -derivedDataPath "$DERIVED" >"$WORK/build.log" 2>&1 ||
  { echo "the phone would not build:"; tail -30 "$WORK/build.log"; exit 1; }

echo "== seeding a session on the phone, and clearing any pairing =="
TEST_RUNNER_DAISY_SEED_SYNC=clear xcodebuild test-without-building \
  -scheme Daisy -destination "$DEST" -derivedDataPath "$DERIVED" \
  -only-testing:DaisyTests/SyncFixtureSeedTests >>"$WORK/seed.log" 2>&1 || true
# DAISY_SEED_SEAM is what puts a recording here that can actually cross. The
# rest of the fixtures are named `daisy-uitest-…` so a sweep on a real phone
# can find its own work; the desktop takes `daisy-` plus digits and skips
# everything else, so none of them travel. The seam fixture's id is numeric.
TEST_RUNNER_DAISY_SEED_SESSION=1 TEST_RUNNER_DAISY_SEED_SEAM=1 \
  xcodebuild test-without-building \
  -scheme Daisy -destination "$DEST" -derivedDataPath "$DERIVED" \
  -only-testing:DaisyTests/SummaryFixtureSeedTests >>"$WORK/seed.log" 2>&1 ||
  { echo "the session fixtures would not seed:"; tail -30 "$WORK/seed.log"; exit 1; }
grep -q "SEEDED SEAM" "$WORK/seed.log" ||
  { echo "the seam session did not seed — the phone would have nothing to send:";
    tail -30 "$WORK/seed.log"; exit 1; }

echo "== starting the desktop =="
cd "$REPO"
DAISY_LIVE_CODE_FILE=$CODE_FILE \
DAISY_LIVE_HOST=${DAISY_LIVE_HOST:-127.0.0.1} \
DAISY_LIVE_PAIR_TIMEOUT=${DAISY_LIVE_PAIR_TIMEOUT:-600} \
DAISY_LIVE_HELLO_TIMEOUT=${DAISY_LIVE_HELLO_TIMEOUT:-300} \
DAISY_LIVE_TRANSFER_TIMEOUT=${DAISY_LIVE_TRANSFER_TIMEOUT:-900} \
DAISY_LIVE_EXPECT_PROFILE=${DAISY_SEAM_BOTH_WAYS:-} \
DAISY_LIVE_PHONE_DONE_FILE=$PHONE_DONE \
  cargo test -p tauri-app --test seam_live -- --ignored --nocapture \
    a_real_phone_pairs_and_syncs_with_this_desktop >"$DESKTOP_LOG" 2>&1 &
DESKTOP_PID=$!

echo "== waiting for the pairing code =="
for _ in $(seq 1 120); do
  [[ -s $CODE_FILE ]] && break
  kill -0 "$DESKTOP_PID" 2>/dev/null ||
    { echo "the desktop exited before showing a code:"; tail -30 "$DESKTOP_LOG"; exit 1; }
  sleep 1
done
[[ -s $CODE_FILE ]] || { echo "no code after two minutes:"; tail -30 "$DESKTOP_LOG"; exit 1; }
CODE=$(cat "$CODE_FILE")

echo "== driving the phone =="
cd "$IOS"
PHONE_RC=0
TEST_RUNNER_DAISY_PAIR_CODE="$CODE" \
TEST_RUNNER_DAISY_EXPECT_DESKTOP_TAGS="${DAISY_SEAM_BOTH_WAYS:-0}" \
  xcodebuild test-without-building \
  -scheme Daisy -destination "$DEST" -derivedDataPath "$DERIVED" \
  -only-testing:DaisyUITests/DesktopSyncLiveUITests >"$PHONE_LOG" 2>&1 || PHONE_RC=$?
# The desktop holds the port until this appears: a phone driven through a UI
# is quiet between exchanges for longer than the desktop's quiet window, and
# one that ends early takes the listener with it.
: >"$PHONE_DONE"

echo "== waiting for the desktop to finish =="
DESKTOP_RC=0
wait "$DESKTOP_PID" || DESKTOP_RC=$?
DESKTOP_PID=

sed -n '/---- what crossed ----/,/-----------------------/p' "$DESKTOP_LOG" || true
if (( PHONE_RC != 0 )); then
  echo "== the phone failed =="
  grep -E "error:|XCTAssert|Test Case .* failed|Testing failed" "$PHONE_LOG" | head -20 || tail -30 "$PHONE_LOG"
fi
if (( DESKTOP_RC != 0 )); then
  echo "== the desktop failed =="
  grep -E "panicked|assertion|FAILED" "$DESKTOP_LOG" | head -20 || tail -30 "$DESKTOP_LOG"
fi
(( PHONE_RC == 0 && DESKTOP_RC == 0 )) || exit 1
echo "== both ends agree =="
