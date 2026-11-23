#!/usr/bin/env python3
"""shrink-fp16.py <src.onnx> <dst.onnx>
Token-embedding Gather -> dynamic int8 (per-channel); every MatMul weight initializer
-> fp16 stored on disk with a Cast back to fp32 in the graph (constant-folded at session
load, so inference stays fp32 through the unquantized kernels)."""
import sys, os, onnx, numpy as np
from onnx import numpy_helper, helper, TensorProto
from onnxruntime.quantization import quantize_dynamic, QuantType
src, dst = sys.argv[1], sys.argv[2]
m = onnx.load(src, load_external_data=False)
inits = {i.name: i for i in m.graph.initializer}
gathers = [n.name for n in m.graph.node if n.op_type == 'Gather' and any(i in inits and int(np.prod(inits[i].dims)) > 1e6 for i in n.input)]
tmp = dst + '.gather.onnx'
quantize_dynamic(src, tmp, weight_type=QuantType.QInt8, per_channel=True, nodes_to_quantize=gathers, op_types_to_quantize=['Gather'])
m = onnx.load(tmp, load_external_data=False); os.remove(tmp)
inits = {i.name: i for i in m.graph.initializer}
consumers = {}
for n in m.graph.node:
    for i in n.input: consumers.setdefault(i, []).append(n)
converted = 0; saved = 0
new_nodes = []
for name, t in list(inits.items()):
    users = consumers.get(name, [])
    if not users or not all(u.op_type == 'MatMul' for u in users) or t.data_type != TensorProto.FLOAT: continue
    arr = numpy_helper.to_array(t)
    if arr.size < 1e5: continue
    half = numpy_helper.from_array(arr.astype(np.float16), name + '_fp16')
    m.graph.initializer.remove(t); m.graph.initializer.append(half)
    new_nodes.append(helper.make_node('Cast', [name + '_fp16'], [name], to=TensorProto.FLOAT, name=name + '_cast'))
    converted += 1; saved += arr.nbytes // 2
# Casts go first so every consumer sees its input defined earlier in topological order.
old = list(m.graph.node); del m.graph.node[:]; m.graph.node.extend(new_nodes + old)
onnx.checker.check_model(m)
onnx.save(m, dst)
print(f"matmul weights -> fp16: {converted} tensors, {saved/1e6:.0f} MB saved; file {os.path.getsize(dst)/1e6:.0f} MB")
