#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Runs INSIDE the build container. Driven by scripts/build-release.sh.
# Don't invoke directly — it assumes /work mount and the daisy-build image env.
set -euo pipefail

export CI=true
# Sets the CMake policy floor for vendored CMakeLists.
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
export NVM_DIR=/root/.nvm
# shellcheck disable=SC1091
. "$NVM_DIR/nvm.sh"
nvm use default >/dev/null

echo "--- frontend deps ---"
# Installs without scripts, then rebuilds esbuild.
(cd apps/frontend \
    && pnpm install --frozen-lockfile --ignore-scripts \
    && pnpm rebuild esbuild)

echo "--- frontend build ---"
# Builds the shipping dist.
unset VITE_UI_TEST
(cd apps/frontend && npx tsc -b && npx vite build)

echo "--- staging voiceprints model for bundle ---"
bash /work/scripts/download-voiceprints.sh
echo "--- staging nemotron model for bundle ---"
bash /work/scripts/download-nemotron.sh
echo "--- staging nemotron3 diarization model for bundle ---"
bash /work/scripts/download-nemotron3-diar.sh
[ -f /work/models/nemotron3-diar/nemotron3_diar_fast32.onnx ] \
    || { echo "(error) nemotron3 diarization model missing from staging" >&2; exit 1; }
# The linux bundle ships the nemotron floor row and no whisper model.
[ -f /work/models/nemotron/nemotron-speech-streaming-en-0.6b-560ms-int8/encoder.int8.onnx ] \
    || { echo "(error) nemotron model missing from staging" >&2; exit 1; }
echo "bundle models size: $(du -sh /work/models | cut -f1)"
# Embedding model for Q&A/RAG. repack-appimage.sh bundles models/embeddings into
# usr/share/daisy and AppRun exports DAISY_EMBED_DIR; tauri.conf.json also lists it.
bash /work/scripts/download-embeddings.sh

# Clears bundles from earlier versions.
rm -rf /work/target/release/bundle/deb /work/target/release/bundle/appimage

echo "--- tauri bundle (daisy-app + .deb + .AppImage) ---"
# The frontend is built above; tauri.conf.json leaves beforeBuildCommand empty.
echo "--- staging daisy-llm sidecar ---"
bash /work/scripts/stage-daisy-llm.sh
# The `ort` crate places libonnxruntime.so in /work/target/release/. That dir is on
# LD_LIBRARY_PATH so linuxdeploy copies the .so chain into the AppDir
# (repack-appimage.sh parks it in runtime-libs).
export LD_LIBRARY_PATH="/work/target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Whisper's Linux backends (Vulkan GPU offload, OpenBLAS CPU path) are target deps
# of providers-local. BLAS_INCLUDE_DIRS comes from .cargo/config.toml. The build
# needs glslc, libvulkan-dev and libopenblas-dev (in the image); repack-appimage.sh
# bundles the libvulkan loader.
(cd crates/tauri-app && cargo tauri build --bundles deb,appimage --verbose)
/work/scripts/check-no-license-test.sh /work/target/release/daisy-app

# The sidecar ships beside daisy-app in the .deb and the AppImage.
find /work/target/release/bundle -name daisy-llm -type f | grep -q . \
    || { echo "(error) daisy-llm missing from the bundle" >&2; exit 1; }

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
cp -f /work/target/release/daisy-llm /work/dist/
find /work/target/release/bundle -maxdepth 2 -name '*.deb' -o -name '*.AppImage' \
    | xargs -I{} cp -f {} /work/dist/
