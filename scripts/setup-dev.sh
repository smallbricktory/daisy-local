#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# One-shot dev-environment bootstrap for Daisy.
# For Ubuntu 22.04+ / 24.04+. Installs build-time system deps and rustup.
# Runtime deps (libpipewire-0.3 runtime, wireplumber, pulseaudio-utils) are
# typically already present on PipeWire-using desktops; if not, install them
# the same way.

set -euo pipefail

if [[ "$(id -u)" -eq 0 ]]; then
  echo "Run this script as your normal user (it will sudo where needed)." >&2
  exit 1
fi

echo "==> apt: build-time system deps"
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    pkgconf \
    meson \
    ninja-build \
    clang \
    libclang-dev \
    libssl-dev \
    libpipewire-0.3-dev \
    pulseaudio-utils \
    wireplumber \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libsoup-3.0-dev \
    libopenblas-dev \
    libvulkan-dev \
    glslc \
    spirv-headers \
    xvfb

# libssl-dev is needed by ort-sys's download-binaries feature, which fetches
# a prebuilt libonnxruntime over HTTPS at first build (then caches it).
#
# libwebkit2gtk-4.1-dev + libgtk-3-dev + libayatana-appindicator3-dev +
# librsvg2-dev + libsoup-3.0-dev are required by Tauri 2 on Linux for the
# desktop shell.
#
# libopenblas-dev + libvulkan-dev + glslc build whisper's Linux backends
# (OpenBLAS CPU path, Vulkan GPU offload). BLAS header paths come from
# .cargo/config.toml (BLAS_INCLUDE_DIRS).
#
# spirv-headers + libvulkan-dev + glslc build llama.cpp's Vulkan backend in
# the daisy-llm worker (scripts/stage-daisy-llm.sh).
#
# xvfb gives the native UI suite a display: `xvfb-run -a pnpm --dir
# apps/frontend run e2e:native` (docs/testing-ui.md). CI's ui-tests job runs
# it the same way.

if ! command -v rustup >/dev/null 2>&1; then
  echo "==> installing rustup (stable toolchain, minimal profile)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
  # shellcheck disable=SC1090
  source "$HOME/.cargo/env"
else
  echo "==> rustup already installed; ensuring stable toolchain is up to date"
  rustup update stable
fi

# cargo-tauri builds the bundles and the ui-test binary
# (`cargo tauri build`). Same version spec as release.yml, test-build.yml and
# ci.yml's ui-tests job, so a dev box and a runner resolve the same CLI.
command -v cargo-tauri >/dev/null || cargo install tauri-cli --version "^2" --locked

echo "==> verifying"
cargo --version
cargo tauri --version
rustc --version
pkg-config --modversion libpipewire-0.3
pkg-config --modversion openssl
clang --version | head -1

echo
echo "Dev environment ready. Run: cargo build"
echo "Note: first build will download a prebuilt libonnxruntime via ort-sys"
echo "      (~50 MB, cached after that)."
