#!/usr/bin/env bash
# Container-build a Daisy release against the Ubuntu 24.04 baseline and drop
# the artifacts into the output directory (OUT_DIR).
#
# Builds:
#   - daisy-app             (Tauri desktop binary)
#   - daisy-cli             (finalize helper)
#   - daisy_*.AppImage      (self-contained, runs on 24.04+)
#   - daisy_*_amd64.deb     (Debian/Ubuntu package)
#
# Floor: Ubuntu 24.04 LTS. Forward-compatible to 25.04 / 25.10 / 26.04 etc.
# (On 26.04, `apt install libicu74` may be required.)

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out_dir="${OUT_DIR:-$HOME/Sync/daisy}"
image_tag="daisy-build:24.04"

if ! command -v podman >/dev/null 2>&1; then
    echo "podman not installed — run: sudo apt install -y podman" >&2
    exit 1
fi

mkdir -p "$out_dir"

# Build the image. Podman caches layers; subsequent runs reuse them.
echo "==> Building build-image $image_tag"
podman build -t "$image_tag" -f "$repo_root/Dockerfile.build" "$repo_root"

# Cargo + pnpm caches are persisted in named volumes.
echo "==> Running release build inside container"
DAISY_BUILD_SHA="${DAISY_BUILD_SHA:-$(git -C "$repo_root" rev-parse --short HEAD)}"
podman run --rm \
    --env DAISY_BUILD_SHA="$DAISY_BUILD_SHA" \
    --env DAISY_BUILD_TAGGED="${DAISY_BUILD_TAGGED:-}" \
    --volume "$repo_root:/work:Z" \
    --volume daisy-cargo-cache:/root/.cargo/registry \
    --volume daisy-cargo-git:/root/.cargo/git \
    --volume daisy-cargo-target:/work/target \
    --volume daisy-pnpm-store:/root/.local/share/pnpm \
    --workdir /work \
    "$image_tag" \
    bash /work/scripts/build-inside.sh

# The container stages artifacts to $repo_root/dist (bind-mounted
# /work/dist). The host's target/ is left untouched.
dist_dir="$repo_root/dist"

echo "==> Copying artifacts from $dist_dir to $out_dir"
if [ ! -d "$dist_dir" ]; then
    echo "(error) $dist_dir missing — container build did not stage artifacts" >&2
    exit 1
fi
# Only the user-installable artifacts ship; the raw daisy-app / daisy-cli
# binaries are already embedded in the AppImage and .deb. Stale copies from
# previous runs are dropped.
rm -f "$out_dir/daisy-app" "$out_dir/daisy-cli"
find "$dist_dir" -maxdepth 1 \( -name '*.AppImage' -o -name '*.deb' \) -exec cp -fv {} "$out_dir/" \;

# Marks check-laptop.sh executable when present.
if [ -f "$out_dir/check-laptop.sh" ]; then chmod +x "$out_dir/check-laptop.sh"; fi

# Ships the test-profile launchers alongside the build.
# Linux:
cp -f "$repo_root/scripts/run-test-profile.sh" "$out_dir/run-test-profile.sh"
chmod +x "$out_dir/run-test-profile.sh"
# Windows:
cp -f "$repo_root/scripts/run-test-profile.ps1" "$out_dir/run-test-profile.ps1"

# Stamps the build metadata.
{
    echo "built: $(date -Iseconds)"
    echo "host:  $(hostname)"
    echo "git:   $(cd "$repo_root" && git rev-parse HEAD)$( cd "$repo_root" && git diff --quiet || echo ' (dirty)')"
    echo "floor: Ubuntu 24.04 LTS (glibc 2.39)"
} > "$out_dir/BUILD-INFO.txt"

echo
echo "==> Done. Contents of $out_dir:"
ls -lh "$out_dir"
