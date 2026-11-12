# Nemotron streaming model export

Recipe for the ONNX files Daisy ships as its Windows and Linux speech engine.

| Input | Pin |
|---|---|
| NVIDIA checkpoint | `nvidia/nemotron-speech-streaming-en-0.6b` @ `ac0580bb7d3d6e39c4361db6afe28db9211793e4` |
| Export script | sherpa-onnx `scripts/nemo/nemotron-speech-streaming-en-0.6b/export_onnx.py` @ `415b8c2aa261bcbe6c88772db9d80350955d1502` (Apache-2.0, Copyright 2026 Xiaomi Corp.), adapted as `export.py` |
| Python packages | `requirements.lock.txt` (NeMo 3.0.0, torch 2.14.1, onnx 1.23.1, onnxruntime 1.30.0) |

The weights are licensed by NVIDIA under the NVIDIA Open Model License. `export.py` keeps the Xiaomi copyright and Apache-2.0 notice and adds a "Modified by Apexis Solutions Inc." line.

## Quantization recipe

`quantize.py` turns the fp32 export (560 ms chunks) into the shipped files:

- Encoder: ONNX Runtime dynamic quantization, uint8 weights, per-channel, for MatMul, Gemm and Conv nodes. Nodes whose name contains `pre_encode` (the subsampling stack) stay fp32.
- Decoder and joiner: fp32, copied as `decoder.onnx` and `joiner.onnx`.
- `tokens.txt`: written by `export.py`.

Shipped files: `encoder.int8.onnx`, `encoder.int8.onnx.data` (external weights), `decoder.onnx`, `joiner.onnx`, `tokens.txt`. Their sha256 values are in `SHA256SUMS`, which `crates/providers-local/src/registry.rs` (`NEMOTRON_FILES`) and `scripts/download-nemotron.sh` mirror; `crates/providers-local/tests/packaging_pins.rs` asserts the three agree.

## Reproduce

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install -r scripts/nemotron-export/requirements.lock.txt
python scripts/nemotron-export/export.py work/fp32
python scripts/nemotron-export/quantize.py work/fp32 work/out
(cd work/out && shasum -a 256 -c ../../scripts/nemotron-export/SHA256SUMS)
```

The export downloads about 2.5 GB from Hugging Face and needs several GB of RAM.

## Publish

`scripts/publish-nemotron-model.sh <dir>` verifies the five files against `SHA256SUMS`, creates or completes release `asr-nemotron-streaming-en-0.6b-560ms-int8-1` on `smallbricktory/daisy-local`, then downloads the assets back and verifies them again.
