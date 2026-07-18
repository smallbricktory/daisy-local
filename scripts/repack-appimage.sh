#!/usr/bin/env bash
# Post-processes a Tauri-built AppImage: strips the bundled system-GTK
# stack. linuxdeploy-plugin-gtk drags in libglib/libgtk/libwebkit copies
# from the build distro (24.04) and AppRun forces them via LD_LIBRARY_PATH;
# on a newer host (25.10+, 26.04) those bundled libs conflict with the
# host's gvfs / gstreamer / webkit modules
# ("g_variant_builder_init_static: undefined symbol",
# "GStreamer element appsink not found", webkit web-process crashes).
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

# Third-party open-source notices. The Linux AppImage bundles models
# manually (below) and does not use Tauri's `resources` config; this file —
# listed there for the Win/Mac bundlers — is copied in explicitly.
# MIT/Apache/BSD require the text to accompany the distributed binary.
licenses_src="$repo_root/THIRD-PARTY-LICENSES.txt"
if [ -f "$licenses_src" ]; then
    echo "bundling THIRD-PARTY-LICENSES.txt"
    mkdir -p "$appdir/usr/share/daisy"
    cp -f "$licenses_src" "$appdir/usr/share/daisy/THIRD-PARTY-LICENSES.txt"
fi

models_src="$repo_root/models/dtln-aec"
models_dst="$appdir/usr/share/daisy/models/dtln-aec"
if [ -d "$models_src" ]; then
    echo "bundling AEC models from $models_src"
    mkdir -p "$models_dst"
    cp -f "$models_src"/*.onnx "$models_dst/" 2>/dev/null || true
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
fi

# Pre-bundled Whisper ggml model for the finalize transcription pass; no
# first-run download. A single size ships (base.en); the path resolves via
# DAISY_WHISPER_MODEL_DIR below.
whisper_src="$repo_root/models/whisper"
whisper_dst="$appdir/usr/share/daisy/models/whisper"
if [ -d "$whisper_src" ]; then
    echo "bundling Whisper finalize model from $whisper_src"
    mkdir -p "$whisper_dst"
    cp -f "$whisper_src"/ggml-*.bin "$whisper_dst/" 2>/dev/null || true
fi

# Salvages the ONNX Runtime + OpenBLAS shared libs out of the bundled
# usr/lib tree before the wipe. whisper.cpp DT_NEEDED-links libopenblas;
# linuxdeploy drops it (+ libgfortran/quadmath) into usr/lib/. ORT is
# different: the `ort` download-binaries crate dlopens libonnxruntime at
# runtime (no DT_NEEDED entry); linuxdeploy never sees it and it lives in
# the build's target/release/ — staged explicitly. Everything lands in
# usr/share/daisy/runtime-libs, on LD_LIBRARY_PATH + ORT_DYLIB_PATH via
# AppRun. cp -P preserves SONAME symlink chains.
runtime_libs_dst="$appdir/usr/share/daisy/runtime-libs"
mkdir -p "$runtime_libs_dst"
if compgen -G "$appdir/usr/lib/libopenblas*.so*" >/dev/null 2>&1; then
    echo "salvaging openblas shared libs from usr/lib"
    find "$appdir/usr/lib" -maxdepth 4 \
        \( -name 'libopenblas*.so*' -o -name 'libgfortran*.so*' \
           -o -name 'libquadmath*.so*' \) \
        -exec cp -P {} "$runtime_libs_dst/" \;
fi
# The Vulkan loader (libvulkan.so.1) is DT_NEEDED-linked by the vulkan
# whisper backend; linuxdeploy drops it into usr/lib. It is kept: daisy-app
# fails to start on a machine with no system Vulkan loader. Salvaged into
# runtime-libs before the usr/lib wipe. The loader finds the user's GPU
# drivers (ICDs) at runtime; with none, it reports 0 devices → ggml CPU
# fallback.
if compgen -G "$appdir/usr/lib"/**/'libvulkan.so*' >/dev/null 2>&1 \
   || find "$appdir/usr/lib" -maxdepth 4 -name 'libvulkan.so*' | grep -q .; then
    echo "salvaging vulkan loader from usr/lib"
    find "$appdir/usr/lib" -maxdepth 4 -name 'libvulkan.so*' \
        -exec cp -P {} "$runtime_libs_dst/" \;
fi
# ort's libonnxruntime.so (dlopened — not bundled by linuxdeploy). Staged in
# runtime-libs (LD_LIBRARY_PATH + ORT_DYLIB_PATH) AND next to daisy-app in
# usr/bin; ort's most reliable search path is adjacent-to-executable.
if compgen -G "$repo_root/target/release/libonnxruntime.so*" >/dev/null 2>&1; then
    echo "staging libonnxruntime.so from target/release"
    cp -P "$repo_root"/target/release/libonnxruntime.so* "$runtime_libs_dst/"
    cp -P "$repo_root"/target/release/libonnxruntime.so* "$appdir/usr/bin/" 2>/dev/null || true
else
    echo "WARN: libonnxruntime.so not in target/release — AEC/diarize/Q&A will fail" >&2
fi

if [ -d "$lib_root" ]; then
    # Wipes everything under usr/lib/, including
    # $appdir/usr/lib/libwebkit2gtk-4.1.so.0 (one level above
    # x86_64-linux-gnu/) and the webkit2gtk-4.1 helper processes
    # WebKit{Network,Web}Process (no .so extension). Wiping all of usr/lib
    # forces daisy-app onto the host's GTK/glib/webkit stack via the
    # standard linker search path; the binary's rpath ($ORIGIN/../lib) finds
    # nothing bundled.
    echo "stripping bundled lib tree at $lib_root"
    rm -rf "$lib_root"
fi

# AppRun from linuxdeploy-plugin-gtk prepends $APPDIR/usr/lib/... to
# LD_LIBRARY_PATH, exports GIO_MODULE_DIR / GST_PLUGIN_*_DIR to bundled
# directories, etc. The replacement AppRun points those at the system or
# unsets them, and disables gvfs modules.
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
# AEC model dir — the compile-time CARGO_MANIFEST_DIR fallback resolves to
# the build container's /work path, absent on the user's machine. Points the
# runtime at the bundled copy.
export DAISY_MODEL_DIR="$HERE/usr/share/daisy/models/dtln-aec"
# Same for the BGE embedding model used by Q&A.
export DAISY_EMBED_DIR="$HERE/usr/share/daisy/models/embeddings"
# And the WeSpeaker voiceprint model used by cross-session speaker matching.
export DAISY_VOICEPRINT_DIR="$HERE/usr/share/daisy/models/voiceprints"
# Bundled Whisper ggml dir. profile.rs::whisper_model_path() probes this
# first, falling back to <profile>/models for users who side-loaded.
export DAISY_WHISPER_MODEL_DIR="$HERE/usr/share/daisy/models/whisper"
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
if ! find "$appdir" -path '*models/embeddings/model.onnx' -print -quit | grep -q .; then
    echo "(error) embeddings model.onnx missing from repacked AppImage" >&2
    exit 1
fi
chmod +x "$src"
echo "done: $(stat -c%s "$src") bytes"
