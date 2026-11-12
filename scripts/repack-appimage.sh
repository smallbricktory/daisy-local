#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Post-processes a Tauri-built AppImage: strips the bundled system-GTK
# stack (libglib/libgtk/libwebkit copies added by linuxdeploy-plugin-gtk).
#
# The script extracts the AppImage, deletes every shared library the host
# system provides (anything matching the keep-list below stays), patches
# AppRun to leave LD_LIBRARY_PATH alone for those, and repacks with
# appimagetool.
#
# Input: $1 = path to AppImage produced by `cargo tauri build`.
# Output: same path, overwritten in place.

set -euo pipefail

src="${1:?usage: repack-appimage.sh path/to/foo.AppImage}"
[ -f "$src" ] || { echo "no such AppImage: $src" >&2; exit 1; }

if ! command -v appimagetool >/dev/null 2>&1; then
    echo "appimagetool not in PATH — install or build inside container" >&2
    exit 1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

cp -f "$src" "$workdir/in.AppImage"
chmod +x "$workdir/in.AppImage"
(cd "$workdir" && ./in.AppImage --appimage-extract >/dev/null)

appdir="$workdir/squashfs-root"
lib_root="$appdir/usr/lib"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

# Third-party open-source notices are copied into the AppImage.
licenses_src="$repo_root/THIRD-PARTY-LICENSES.txt"
if [ -f "$licenses_src" ]; then
    echo "bundling THIRD-PARTY-LICENSES.txt"
    mkdir -p "$appdir/usr/share/daisy"
    cp -f "$licenses_src" "$appdir/usr/share/daisy/THIRD-PARTY-LICENSES.txt"
fi

embed_src="$repo_root/models/embeddings"
embed_dst="$appdir/usr/share/daisy/models/embeddings"
if [ -d "$embed_src" ]; then
    echo "bundling embedding model from $embed_src"
    mkdir -p "$embed_dst"
    cp -f "$embed_src"/model.onnx "$embed_dst/" 2>/dev/null || true
    cp -f "$embed_src"/tokenizer.json "$embed_dst/" 2>/dev/null || true
    cp -f "$embed_src"/config.json "$embed_dst/" 2>/dev/null || true
fi

vp_src="$repo_root/models/voiceprints"
vp_dst="$appdir/usr/share/daisy/models/voiceprints"
if [ -d "$vp_src" ]; then
    echo "bundling voiceprint model from $vp_src"
    mkdir -p "$vp_dst"
    cp -f "$vp_src"/model.onnx "$vp_dst/" 2>/dev/null || true
    cp -f "$vp_src"/segmentation-3.0.onnx "$vp_dst/" 2>/dev/null || true
fi

# Pre-bundled nemotron model. The path resolves via DAISY_NEMOTRON_MODEL_DIR below.
nm_src="$repo_root/models/nemotron"
nm_dst="$appdir/usr/share/daisy/models/nemotron"
if [ -d "$nm_src" ]; then
    echo "bundling nemotron model from $nm_src"
    mkdir -p "$nm_dst"
    cp -rf "$nm_src"/. "$nm_dst/"
fi
# Nemotron 3 Diarization (live speaker names), every desktop artifact.
# Resolves via DAISY_NEMOTRON3_DIAR_DIR below.
nd_src="$repo_root/models/nemotron3-diar"
nd_dst="$appdir/usr/share/daisy/models/nemotron3-diar"
if [ -d "$nd_src" ]; then
    echo "bundling nemotron3 diarization model from $nd_src"
    mkdir -p "$nd_dst"
    cp -rf "$nd_src"/. "$nd_dst/"
fi

# Salvages the ONNX Runtime + OpenBLAS shared libs out of the bundled
# usr/lib tree before the wipe. libopenblas (+ libgfortran/quadmath) comes from
# usr/lib/; libonnxruntime is staged from the build's target/release/.
# Everything lands in usr/share/daisy/runtime-libs, on LD_LIBRARY_PATH +
# ORT_DYLIB_PATH via AppRun. cp -P preserves SONAME symlink chains.
runtime_libs_dst="$appdir/usr/share/daisy/runtime-libs"
mkdir -p "$runtime_libs_dst"
if compgen -G "$appdir/usr/lib/libopenblas*.so*" >/dev/null 2>&1; then
    echo "salvaging openblas shared libs from usr/lib"
    find "$appdir/usr/lib" -maxdepth 4 \
        \( -name 'libopenblas*.so*' -o -name 'libgfortran*.so*' \
           -o -name 'libquadmath*.so*' \) \
        -exec cp -P {} "$runtime_libs_dst/" \;
fi
# The Vulkan loader (libvulkan.so.1), used by the vulkan whisper backend, is
# salvaged into runtime-libs before the usr/lib wipe. With no GPU driver it
# reports 0 devices and ggml uses the CPU.
if compgen -G "$appdir/usr/lib"/**/'libvulkan.so*' >/dev/null 2>&1 \
   || find "$appdir/usr/lib" -maxdepth 4 -name 'libvulkan.so*' | grep -q .; then
    echo "salvaging vulkan loader from usr/lib"
    find "$appdir/usr/lib" -maxdepth 4 -name 'libvulkan.so*' \
        -exec cp -P {} "$runtime_libs_dst/" \;
fi
# ort's libonnxruntime.so is staged in runtime-libs (LD_LIBRARY_PATH +
# ORT_DYLIB_PATH) and next to daisy-app in usr/bin.
if compgen -G "$repo_root/target/release/libonnxruntime.so*" >/dev/null 2>&1; then
    echo "staging libonnxruntime.so from target/release"
    cp -P "$repo_root"/target/release/libonnxruntime.so* "$runtime_libs_dst/"
    cp -P "$repo_root"/target/release/libonnxruntime.so* "$appdir/usr/bin/" 2>/dev/null || true
else
    echo "WARN: libonnxruntime.so not in target/release — AEC/diarize/Q&A will fail" >&2
fi

find "$appdir/usr/bin" -name daisy-llm | grep -q . || { echo "daisy-llm missing from AppDir" >&2; exit 1; }

if [ -d "$lib_root" ]; then
    # Wipes everything under usr/lib/, including libwebkit2gtk-4.1.so.0 and the
    # webkit2gtk-4.1 helper processes WebKit{Network,Web}Process. daisy-app then
    # uses the host's GTK/glib/webkit stack.
    echo "stripping bundled lib tree at $lib_root"
    rm -rf "$lib_root"
fi

# Replaces AppRun: it points GIO/GST paths at the system or unsets them, and
# disables gvfs modules.
apprun="$appdir/AppRun"
if [ -f "$apprun" ]; then
    cat > "$apprun" <<'APPRUN'
#!/bin/bash
# Daisy AppRun — unbundled GTK; bundles only the ONNX Runtime + OpenBLAS
# shared libs the finalize pipeline (aec/voiceprints/embeddings/whisper) needs.
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:${PATH:-}"
# Bundled onnxruntime + openblas live here; GTK/webkit resolve from the
# system path.
export LD_LIBRARY_PATH="$HERE/usr/share/daisy/runtime-libs"
# `ort` (download-binaries) dlopens ONNX Runtime by this explicit path —
# used by aec (echo cancel), voiceprints (diarize), embeddings (Q&A).
export ORT_DYLIB_PATH="$HERE/usr/share/daisy/runtime-libs/libonnxruntime.so"
# Skips gvfs entirely — its modules break on version skew.
export GIO_USE_VFS=local
unset GIO_MODULE_DIR
# The system supplies gstreamer plugins.
unset GST_PLUGIN_PATH GST_PLUGIN_SYSTEM_PATH GST_PLUGIN_SCANNER GST_PLUGIN_SYSTEM_PATH_1_0 GST_PLUGIN_PATH_1_0
unset GDK_PIXBUF_MODULE_FILE GDK_PIXBUF_MODULEDIR
# Point WebKitGTK at the host's child-process helpers (WebKitNetworkProcess,
# WebKitWebProcess). Without this, libwebkit's $ORIGIN-relative lookup ends
# up as "././/lib/x86_64-linux-gnu/webkit2gtk-4.1/..." from $PWD and fails.
# /usr/lib/x86_64-linux-gnu/webkit2gtk-4.1 is the Debian/Ubuntu install path
# for libwebkit2gtk-4.1-0; matches 24.04 through 26.04.
export WEBKIT_EXEC_PATH=/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1
# GPU compositing: WebKit picks its best renderer (dmabuf → GBM → SW).
# `DAISY_DISABLE_GPU=1` re-enables the software path (handled in main.rs).
unset WEBKIT_DISABLE_DMABUF_RENDERER
if [ "${DAISY_DISABLE_GPU:-0}" = "1" ]; then
  export WEBKIT_DISABLE_DMABUF_RENDERER=1
  export WEBKIT_DISABLE_COMPOSITING_MODE=1
fi
# Model dirs — the compile-time CARGO_MANIFEST_DIR fallbacks resolve to
# the build container's /work path, absent on the user's machine. Points the
# runtime at the bundled copies. Embedding model used by Q&A:
export DAISY_EMBED_DIR="$HERE/usr/share/daisy/models/embeddings"
# And the WeSpeaker voiceprint model used by cross-session speaker matching.
export DAISY_VOICEPRINT_DIR="$HERE/usr/share/daisy/models/voiceprints"
# Bundled nemotron floor row. The resolver probes it when the profile has no
# copy; whisper models are user downloads.
export DAISY_NEMOTRON_MODEL_DIR="$HERE/usr/share/daisy/models/nemotron"
# Live speaker-name diarizer (Nemotron 3 Diarization fast32).
export DAISY_NEMOTRON3_DIAR_DIR="$HERE/usr/share/daisy/models/nemotron3-diar"
# Hand off to the actual binary (matches the .desktop Exec).
exec "$HERE/usr/bin/daisy-app" "$@"
APPRUN
    chmod +x "$apprun"
fi

# Repack. ARCH must be set for appimagetool when invoked headless.
echo "repacking $src"
ARCH=x86_64 appimagetool --no-appstream "$appdir" "$src.new" >/dev/null
mv -f "$src.new" "$src"
if ! find "$appdir" -path '*models/voiceprints/model.onnx' -print -quit | grep -q .; then
    echo "(error) voiceprints model.onnx missing from repacked AppImage" >&2
    exit 1
fi
if ! find "$appdir" -path '*models/voiceprints/segmentation-3.0.onnx' -print -quit | grep -q .; then
    echo "(error) voiceprints segmentation-3.0.onnx missing from repacked AppImage" >&2
    exit 1
fi
if ! find "$appdir" -path '*models/embeddings/model.onnx' -print -quit | grep -q .; then
    echo "(error) embeddings model.onnx missing from repacked AppImage" >&2
    exit 1
fi
if ! find "$appdir" -path '*models/nemotron/nemotron-speech-streaming-en-0.6b-560ms-int8/encoder.int8.onnx' -print -quit | grep -q .; then
    echo "(error) nemotron model missing from repacked AppImage" >&2
    exit 1
fi
for f in nemotron3_diar_fast32.onnx learnable_sil_emb.bin nemotron3_diar.json NOTICE LICENSE-OpenMDW-1.1.txt; do
    if ! find "$appdir" -path "*models/nemotron3-diar/$f" -print -quit | grep -q .; then
        echo "(error) nemotron3-diar/$f missing from repacked AppImage" >&2
        exit 1
    fi
done
if find "$appdir" -name 'ggml-*.bin' -print -quit | grep -q .; then
    echo "(error) a whisper ggml model is in the bundle — base.en is unbundled" >&2
    exit 1
fi
chmod +x "$src"
echo "done: $(stat -c%s "$src") bytes"
