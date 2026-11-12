#!/usr/bin/env python3
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
"""Turn export.py's fp32 output into the shipped Nemotron file set.

usage: quantize.py <export-dir> <out-dir>

Encoder: per-channel uint8 dynamic quantization of MatMul, Gemm and Conv nodes,
leaving the subsampling stack (nodes named *pre_encode*) in fp32.
Decoder and joiner: copied as fp32 (decoder.onnx, joiner.onnx).
"""
import os
import shutil
import sys

import onnx
from onnxruntime.quantization import QuantType, quantize_dynamic

export_dir, out_dir = (os.path.abspath(a) for a in sys.argv[1:3])
src = os.path.join(export_dir, "560")
os.makedirs(out_dir, exist_ok=True)
shutil.copy(os.path.join(export_dir, "tokens.txt"), out_dir)

graph = onnx.load(os.path.join(src, "encoder.onnx"), load_external_data=False)
subsampling = [n.name for n in graph.graph.node if "pre_encode" in n.name]

quantize_dynamic(
    os.path.join(src, "encoder.onnx"),
    os.path.join(out_dir, "encoder.int8.onnx"),
    weight_type=QuantType.QUInt8,
    use_external_data_format=True,
    per_channel=True,
    op_types_to_quantize=["MatMul", "Gemm", "Conv"],
    nodes_to_exclude=subsampling,
)
for name in ("decoder", "joiner"):
    shutil.copy(os.path.join(src, f"{name}.onnx"), os.path.join(out_dir, f"{name}.onnx"))
