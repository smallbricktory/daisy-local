#!/usr/bin/env bash
# Fetches the Whisper ggml model used by the finalize transcription pass.
# Ships with the AppImage; no first-run download. License is MIT
# (ggerganov/whisper.cpp); safe to redistribute.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/whisper"
mkdir -p "$out"

# Defaults to base.en. A different default size requires updating the
# SHA256 pin alongside it.
size="${DAISY_WHISPER_SIZE:-base.en}"
name="ggml-${size}.bin"
dst="$out/$name"

# SHA256 pins for the shipped sizes (source: ggerganov/whisper.cpp on
# huggingface.co). Keep in lock-step with
# providers-local/src/download.rs::PINNED_SHA256.
case "$size" in
    base.en) want_sha="a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002" ;;
    *) echo "(error) no SHA256 pin for size=$size — update download-whisper.sh" >&2; exit 1 ;;
esac

verify_sha() {
    local file="$1" want="$2"
    local got
    got="$(sha256sum "$file" | awk '{print $1}')"
    if [ "$got" != "$want" ]; then
        echo "(error) SHA256 mismatch for $file" >&2
        echo "  expected: $want" >&2
        echo "  got:      $got"  >&2
        return 1
    fi
}

if [ -s "$dst" ]; then
    if verify_sha "$dst" "$want_sha"; then
        echo "exists: $name ($(stat -c%s "$dst") bytes, SHA OK)"
        exit 0
    fi
    echo "stale file at $dst — re-downloading"
    rm -f "$dst"
fi

base="https://huggingface.co/ggerganov/whisper.cpp/resolve/main"
echo "fetching $name -> $dst"
curl --fail --location --output "$dst.tmp" "$base/$name"
if ! verify_sha "$dst.tmp" "$want_sha"; then
    rm -f "$dst.tmp"
    exit 1
fi
mv "$dst.tmp" "$dst"
echo "Done. Whisper finalize model staged at $dst"
