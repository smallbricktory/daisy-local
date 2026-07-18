#!/usr/bin/env bash
# Runs INSIDE the build container. Driven by scripts/build-release.sh.
# Don't invoke directly — it assumes /work mount and the daisy-build image env.
set -euo pipefail

export CI=true
# Newer CMake (>=4.0) rejects vendored CMakeLists with policy min <3.5
# (audiopus_sys/opus, aec/onnxruntime). Set the floor (harmless on older cmake).
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
export NVM_DIR=/root/.nvm
# shellcheck disable=SC1091
. "$NVM_DIR/nvm.sh"
nvm use default >/dev/null

echo "--- frontend deps ---"
# pnpm 11 refuses to silently run package install scripts; esbuild (its
# postinstall fetches the platform binary) is rebuilt after a metadata-only
# install.
(cd apps/frontend \
    && pnpm install --frozen-lockfile --ignore-scripts \
    && pnpm rebuild esbuild)

echo "--- frontend build ---"
(cd apps/frontend && npx tsc -b && npx vite build)

echo "--- staging voiceprints model for bundle ---"
bash /work/scripts/download-voiceprints.sh
bash /work/scripts/download-whisper.sh
# BGE embedding model for Q&A/RAG — the embeddings crate hard-fails
# (ModelMissing) without it. repack-appimage.sh bundles models/embeddings into
# usr/share/daisy + AppRun exports DAISY_EMBED_DIR; tauri.conf.json also lists it.
bash /work/scripts/download-embeddings.sh

# Clears stale bundles from prior versions. /work/target is a persistent
# named volume; every version's Daisy_<ver>_amd64.deb / .AppImage accumulates
# in bundle/ and the staging copy below globs them all. After this, only the
# current build's artifacts remain.
rm -rf /work/target/release/bundle/deb /work/target/release/bundle/appimage

echo "--- tauri bundle (daisy-app + .deb + .AppImage) ---"
# Frontend is already built above. tauri.conf.json leaves beforeBuildCommand
# empty; tauri-cli's cwd for beforeBuildCommand differs between Linux, the CI
# Windows runner, and a plain user checkout. The build scripts own the
# frontend build.
# linuxdeploy (run by the appimage bundler) scans daisy-app for NEEDED
# libs and refuses to proceed when one is unresolvable. The `ort`
# download-binaries crate drops libonnxruntime.so into /work/target/release/
# at build time; daisy-app links it (aec/voiceprints/embeddings). That dir is
# exposed on LD_LIBRARY_PATH; linuxdeploy finds it and copies the .so chain
# into the AppDir (repack-appimage.sh parks it in runtime-libs).
export LD_LIBRARY_PATH="/work/target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Whisper's Linux backends (Vulkan GPU offload + OpenBLAS CPU path) are
# target deps of providers-local. BLAS_INCLUDE_DIRS comes from
# .cargo/config.toml. Build needs glslc + libvulkan-dev + libopenblas-dev
# (in the image); the libvulkan loader is DT_NEEDED-linked and salvaged by
# repack-appimage.sh. GPU-less machines still load it (→ 0 devices → CPU
# fallback).
(cd crates/tauri-app && cargo tauri build --bundles deb,appimage --verbose)

echo "--- repacking AppImage (strip bundled GTK/glib so host supplies them) ---"
appimage_src=$(find /work/target/release/bundle/appimage -maxdepth 1 -name '*.AppImage' | head -1)
if [ -n "$appimage_src" ]; then
    /work/scripts/repack-appimage.sh "$appimage_src"
fi

echo "--- staging artifacts to /work/dist (bind-mounted) ---"
# /work/target lives in a named volume (cache reused across runs); binaries
# + bundles are copied out to /work/dist, which is bind-mounted from the
# host repo, where the outer script picks them up.
rm -rf /work/dist
mkdir -p /work/dist
cp -f /work/target/release/daisy-app /work/dist/
# daisy-cli package emits a binary called `daisy`.
cp -f /work/target/release/daisy /work/dist/daisy-cli
find /work/target/release/bundle -maxdepth 2 -name '*.deb' -o -name '*.AppImage' \
    | xargs -I{} cp -f {} /work/dist/
