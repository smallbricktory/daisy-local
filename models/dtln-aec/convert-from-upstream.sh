#!/usr/bin/env bash
# Build model_256_1.onnx / model_256_2.onnx from the upstream DTLN-aec
# TFLite weights (Westhausen et al., https://github.com/breizhn/DTLN-aec,
# MIT). Reproduce-it-yourself alternative to the pre-converted pair
# committed in this directory.
#
# Conversion runs tf2onnx 1.16.1 on tensorflow-cpu 2.15.1, which needs
# Python 3.9–3.11. When the system python3 is outside that range the
# script falls back to a python:3.11-slim container via podman or docker.
#
# Output is functionally identical to the release-hosted files (same graph,
# same node names; verified against the `aec` crate test suite) but not
# byte-identical, so no checksum comparison is done here.

set -euo pipefail

OUTDIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VARIANT="${1:-256}"
UPSTREAM="https://github.com/breizhn/DTLN-aec/raw/main/pretrained_models"
TF2ONNX_PIN="tf2onnx==1.16.1"
TF_PIN="tensorflow-cpu==2.15.1"

MODEL1="model_${VARIANT}_1.onnx"
MODEL2="model_${VARIANT}_2.onnx"

if [ -s "${OUTDIR}/${MODEL1}" ] && [ -s "${OUTDIR}/${MODEL2}" ]; then
  echo "DTLN-aec models already present; skipping conversion."
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

echo "Downloading upstream TFLite weights (variant=${VARIANT})..."
curl -fsSL -o "${WORK}/s1.tflite" "${UPSTREAM}/dtln_aec_${VARIANT}_1.tflite"
curl -fsSL -o "${WORK}/s2.tflite" "${UPSTREAM}/dtln_aec_${VARIANT}_2.tflite"

CONVERT_CMD='pip install --quiet '"${TF2ONNX_PIN} ${TF_PIN}"' \
  && python -m tf2onnx.convert --tflite s1.tflite --output out_1.onnx --opset 15 \
  && python -m tf2onnx.convert --tflite s2.tflite --output out_2.onnx --opset 15'

python_ok() {
  command -v python3 >/dev/null 2>&1 || return 1
  python3 -c 'import sys; sys.exit(0 if (3,9) <= sys.version_info[:2] <= (3,11) else 1)'
}

if python_ok; then
  echo "Converting with system python3 in a venv..."
  python3 -m venv "${WORK}/venv"
  # shellcheck disable=SC1091
  . "${WORK}/venv/bin/activate"
  (cd "${WORK}" && eval "${CONVERT_CMD}")
  deactivate
elif command -v podman >/dev/null 2>&1 || command -v docker >/dev/null 2>&1; then
  RUNNER=$(command -v podman || command -v docker)
  echo "System python3 is not 3.9–3.11; converting in a python:3.11-slim container..."
  "${RUNNER}" run --rm -v "${WORK}":/work -w /work python:3.11-slim \
    bash -c "${CONVERT_CMD}"
else
  echo "Error: need Python 3.9–3.11 (for ${TF_PIN}) or podman/docker." >&2
  exit 1
fi

mv "${WORK}/out_1.onnx" "${OUTDIR}/${MODEL1}"
mv "${WORK}/out_2.onnx" "${OUTDIR}/${MODEL2}"
echo "Wrote ${OUTDIR}/${MODEL1} and ${OUTDIR}/${MODEL2}."
echo "Verify with: cargo test -p aec"
