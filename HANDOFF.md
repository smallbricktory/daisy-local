# Handoff — smbr-daisy-app (2026-11-23 15:20 MST)

## Newest entry

dev-dyoo on xenonite, 2026-11-23 15:20 MST. Previous entry: `docs/handoffs/2026-11-23-0155.md`.

Owner decisions (form, 12:36): release 1.3.7; ui-tests `max-parallel: 2`; move direct chrono use to jiff ("make sure you don't break anything"); add the pre-tag cherry-mark step. Changelog text for 1.3.7: "- Bug fixes."

**Shipped (main)**
- `3f008354` RELEASING.md "Before tagging": `git log --cherry-mark --right-only --no-merges --oneline <commit>...origin/main`.
- `2a2b0b8e` ui-tests `max-parallel: 2` (Linux on beetle-ringo, Windows on astrophage).
- `2d20a3d4` version 1.3.7.
- `4b63098b` cookie 0.18.3 (RUSTSEC-2026-0594); tauri-app webview2-com 0.38→0.39 and tauri-plugin-wdio-webdriver 1.3.0→1.5.1 (Tauri 2.12.7's wry is on 0.39; the Windows build had two ICoreWebView2 types).
- `21a1928b` tauri-plugin-dialog 2.8.4, clipboard-manager 2.5.0 to match npm (mac release build refused the mismatch).
- `5fbfa399` tauri-app rfd 0.16→0.17, the dialog plugin's version (a lone 0.16 copy had no Linux backend).
- `52331ab1` build-release.sh stops early without `pasta`.
- Every CI job green on `5fbfa399` (run 45272370400), Windows UI with the BRIO mic.
- astrophage: Windows runner restarted 12:38; its BRIO mic was off USB until ~13:50 (Windows UI record spec fails with "No microphones detected" when it is). beetle-ringo: `passt` installed.

**In progress**
- Release v1.3.7, tag on `5fbfa399` (moved there twice; S3 was never written). Run 45276377660: deny, mirror, macOS green; Windows building; Linux failed (no pasta, fixed). When the run ends: `gh run rerun 45276377660 --failed` (Linux build + publish), then check `https://daisy.smbr.app/updates/<os>/latest.json` for 1.3.7 on each OS.
- chrono→jiff: branch `worktree-agent-a83d8ecd76c055a9d` (worktree `.claude/worktrees/agent-a83d8ecd76c055a9d`), `e19a7ea2` + `9b0c8520`, rebased on `5fbfa399`. 1456 tauri-app tests passed on the Mac before the rebase. A probe compared chrono and jiff output in 21 zones (all match): `~/dev/smbr-daisy-app/chrono-jiff-diff`. jiff 0.2.46 maps all 142 Windows zone ids (0.2.24 missed Alberta, British Columbia, Manitoba). Snooze-until-midnight in a zone whose clocks fall back across midnight now takes the later midnight on Windows too (chrono took the earlier there).
- Owner exporting `daisy-1792776720` with `~/dev/smbr-daisy-app/unseal-session --export` in Terminal (tool now renders notes) into the session scratchpad `export/`.

**Next, in order**
1. Finish the v1.3.7 release (rerun above), confirm latest.json, then add `smbr-daisy-web/content/changelog/1.3.7.md` (version, date 2026-11-23, "- Bug fixes.").
2. jiff branch: regenerate notices on beetle-ringo (jiff 0.2.46, jiff-core 0.1.8), rerun the tauri-app tests after the rebase, merge to main, watch CI. Then delete the worktree, the probe dir and the beetle-ringo scratch clone.
3. Carried over: Korean `daisy-1793963186` Summary/Chapters regen (owner); tester 4 recovery-key rotation.

**Still running**: release run 45276377660 and its Monitor (expires ~15:37; re-arm).

## Open items
- Summary and chapter output language: neither prompt is given `manifest.language` (chapters ask for "the meeting's language", the summary names none), so the model guesses; a garbled transcript produced Russian chapters. Measured in `docs/research/2026-11-03-nemotron-3.5/`: 0 of 10 non-English chapter sets and no non-English summaries came out in the meeting's language, with both Gemma 4 E4B and Bolt E4B. Fix = pass the recording language or the owner's language — owner's decision, first seen 2026-11-10.
- Notes-bug decision (iOS: Done doesn't close notes; the forms on :3333/:3337 are no longer served, checked 2026-11-10 17:20; not reproduced on 24 simulator variants; physical iPhone 15 Pro Max run waits on the owner's UI-automation passcode). — first seen 2026-11-07.
- `2026-10-sample-true-timeline-<machine>` never completes on a profile whose recordings were cleared (Settings › Storage keeps only `meeting.opus`): such sessions "wait" forever by design (`migrations.rs:1036-1038`, test `a_session_whose_audio_is_not_here_waits_for_it`), costing one WARN and a manifest scan per unlock; sealing is not blocked (measured: seal ran 18 ms after the failure). A fix needs an "audio cleared" mark distinguishing it from WAVs still syncing — design call for smbr-daisy-main — first seen 2026-11-07.
- Daisy window captures blank white in spectacle on xi (1.3.6-a and v1.3.5 alike; DOM renders); physical-screen glance pending — first seen 2026-10-28.
- Licence client has no certificate pinning (`post_json`, `crates/tauri-app/src/commands/license.rs`): out of scope by coordinator ruling D, 2026-11-03 — first seen 2026-11-03.
- VPN sync: the pair reply's `.local` host made a VPN-paired phone unable to sync. iOS `fe8ff5b6` now keeps the QR's reached IPv4 address; desktop unchanged. Owner chose LAN-only for now, 2026-11-06.
- xenonite Data volume ran full twice on 2026-11-05; deleting `target/debug/incremental` there waits on the owner (via smbr-daisy-main) — first seen 2026-11-05.
- #153 flaky `sync_seam_matrix::a_phone_killed_part_way_through_resumes_where_it_stopped` — first seen 2026-10-20.
- `feat/direct-wifi` parked (coordinator, 2026-10-28); AWDL protocol `v` 5→6 — owner's decision, first seen 2026-10-17.
- Automatic Windows sign-in on astrophage so the CI runner survives reboots — owner's decision, first seen 2026-10-18.

## Standing traps
- **dev-ai-agent cargo**: no `~/.cargo` and the sandbox blocks creating it. Run cargo with `CARGO_HOME=~/dev/smbr-daisy-app/cargo-home CARGO_HTTP_CAINFO="$SSL_CERT_FILE" CARGO_NET_GIT_FETCH_WITH_CLI=true` (the gateway re-signs TLS; libgit2 rejects it without these).
- **dev-ai-agent ssh**: no keys; xi, astrophage and xenonite are reachable only from dev-dyoo. LAN hosts outside the gateway (e.g. 192.168.0.127) are unreachable.
- `ci.yml` has `cancel-in-progress: true`: any push to main cancels the running main CI. Docs-only commits (handoffs) carry `[skip ci]`; otherwise push after the run ends.
- Pairing keys for phones go to `~/dev/smbr-daisy-app/pairing-xi/key.txt` (0600), never into another repo or a message; xi holds one key at a time, so queue a second request until the first window closes.
- Heavy work on astrophage beside a CI run flakes it (#155, Windows webview timeouts); check `gh run list` first.
- smbr-daisy-main coordinates merges, builds and releases; releases tag only on its or the owner's word.
- Never run 1.3.6-a against xenonite's production profile; a profile opened by 1.3.6-a is one-way for sessions (1.3.5 unlocks the vault but cannot read sealed sessions). Release builds ignore `DAISY_CONFIG_DIR` (ui-test only).
- Native UI runs on the Mac take window focus; ask before starting one while the owner is at the machine. WebKit throttles timers while the screen is locked, which invalidates spec 25 readings.
- astrophage has no ssh key for xi; relay files `ssh astrophage cat … | ssh xi 'cat > …'`. `playback_file` is Linux-only: its tests run on astrophage, not the Mac.
- profile-xi holds the tester's real tags and voiceprints; test with additive tags only.
- `win-host-ASTROPHAGE` goes offline after every reboot: `ssh astrophage '/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile -ExecutionPolicy Bypass -File "G:\actions-runner\start-runner.ps1"'`.
- A Windows prompt in front of astrophage's desktop fails the Windows UI job at "Runner desktop foreground".
- GitHub artifact storage quota is full: native UI artifacts stay on astrophage's runner disk.
- Notices must be generated on Linux (beetle-ringo has cargo-about 0.8.4; scratch clone `~/dev/smbr-daisy-app/notices-regen` there). `cargo update -p <crate>` re-resolves windows-sys in unrelated crates on Mac and Linux alike: keep only the target crates' lock blocks (or edit the manifest and run `cargo metadata --offline`, which changes only what the edit needs), then check `cargo metadata --locked` on both OSes.
- `pgrep -f`/`pkill -f` inside `ssh host '...'` match the ssh command itself.
- xenonite cannot reach xi's ssh directly (VPN path): use `ssh -J astrophage xi`.
- `cargo test -p tauri-app` stops at the first failing binary; pass `--no-fail-fast`, count with `grep -a`.
- e2e:browser under Node 26 hangs on a Chrome bump (wdio's unzip truncates Chrome-for-Testing): stage it by hand per memory `project_node26_extract_zip_truncation` and run under `perl -e 'alarm 600; exec @ARGV'`.
- e2e `callsOf(cmd)` records only commands the spec overrides (`mockInvoke`); a world-handled command leaves no call record.
- Tests that touch sealed sessions must hold the keyring test guard (`crate::test_keys`, `tests/keys/mod.rs`).
- Rust Tauri plugins must share major.minor with their npm `@tauri-apps/plugin-*` packages: `cargo tauri build` refuses a mismatch, and only the release pipeline runs it (CI does not). Bump both sides together.
- `gh run watch` on a re-run attempt exits at once with code 1; watch with Monitor polling `gh run view --json jobs`.
- `unseal-session` reads the passphrase from a tty: the owner runs it in Terminal, not through Claude Code's `!` (fails "Device not configured").
- astrophage's WSL disk (`ext4.vhdx`, 687 GB) lives on C: (127 GB free 2026-11-23); C: filling kills WSL. Runner and CI dirs are on G:.
