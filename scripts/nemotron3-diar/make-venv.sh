#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Creates the Python 3.12 environment for scripts/nemotron3-diar/export.py.
#
#   bash scripts/nemotron3-diar/make-venv.sh VENV_DIR
#
# Installs requirements.lock.txt when it exists (a committed, known-good
# environment), otherwise requirements.txt (direct pins; export.py records
# the resolved set as requirements.lock.txt beside its outputs). Installing
# touches only VENV_DIR; it downloads torch and NeMo (a few GB).
set -euo pipefail

venv="${1:?usage: make-venv.sh VENV_DIR}"
here="$(cd "$(dirname "$0")" && pwd)"

py=""
for cand in python3.12 /opt/homebrew/bin/python3.12 /usr/local/bin/python3.12; do
    if command -v "$cand" >/dev/null 2>&1; then
        py="$(command -v "$cand")"
        break
    fi
done
if [ -z "$py" ]; then
    echo "make-venv: python3.12 not found (macOS: brew install python@3.12)" >&2
    exit 1
fi

"$py" -m venv "$venv"
if [ -f "$here/requirements.lock.txt" ]; then
    reqs="$here/requirements.lock.txt"
else
    reqs="$here/requirements.txt"
fi
echo "make-venv: installing $reqs into $venv"
"$venv/bin/python" -m pip install -r "$reqs"
"$venv/bin/python" -c "import nemo, torch, onnx, onnxruntime; print('nemo', nemo.__version__, 'torch', torch.__version__, 'onnx', onnx.__version__, 'onnxruntime', onnxruntime.__version__)"
