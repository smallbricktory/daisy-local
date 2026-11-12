#!/usr/bin/env python3
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
"""ONNX export of NVIDIA Nemotron 3 Diarization for live speaker names.

Exports the streaming step graph and checks its fidelity against NeMo:

* downloads `Nemotron-3-Diarization.nemo` at the pinned revision and checks its
  size and sha256;
* builds one streaming-step graph per preset (`low`, `fast32`) with
  FluidAudio's tensor names, int32 lengths, fixed shapes, opset 17, fp32
  compute;
* stores the weights as bf16 initializers behind a Cast to fp32 (lossless:
  the checkpoint's weights are bf16), and keeps an all-fp32 copy for checks;
* checks every graph with onnx.checker, then runs it on onnxruntime against
  the NeMo forward on four state configurations per preset (max abs
  difference <= 1e-4), and the first step against NeMo's own
  `forward_streaming_step`;
* writes `learnable_sil_emb.bin`, `nemotron3_diar.json`, the step and mel
  fixtures, `requirements.lock.txt`, `NOTICE`, `MODEL_CARD.md` and
  `SHA256SUMS`.

The export loads every CPU core for minutes. It refuses to start without
`--i-know-recordings-are-done` and while the load average is high.

usage: export.py --i-know-recordings-are-done OUT_DIR
"""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

REPO = "nvidia/Nemotron-3-Diarization"
REVISION = "f667ed73aee57d40cc39428eb768b4fd87a0a29e"
NEMO_FILE = "Nemotron-3-Diarization.nemo"
NEMO_SHA256 = "867c53f552998f772e5b5e5c082962ae85ee7ca5669c2bc17d7f615133d4e96d"
NEMO_BYTES = 198_676_480
WEIGHTS_VERSION = "ga-2026-09-23"
NEMO_GIT = "1688cc3d6a9ade854f544987810c53f605dc86fc"
LICENSE_NAME = "OpenMDW-1.1"

OPSET = 17
TOLERANCE = 1e-4
N_MELS = 128
SUBSAMPLING = 8
EMB_DIM = 512
N_SPK = 8
SAMPLE_RATE = 16_000
HOP = 160

# Left context is 0 in every preset.
PRESETS = {
    "low": dict(chunk_len=9, chunk_right_context=4, fifo_len=264, spkcache_len=264, spkcache_update_period=222),
    "fast32": dict(chunk_len=32, chunk_right_context=4, fifo_len=40, spkcache_len=264, spkcache_update_period=40),
}
INPUTS = ["chunk", "chunk_lengths", "spkcache", "spkcache_lengths", "fifo", "fifo_lengths"]
OUTPUTS = ["speaker_preds", "speaker_preds_10ms", "chunk_pre_encode_embs"]

# Compression constants, read back from the model and compared.
COMPRESSION = {
    "pred_score_threshold": 0.25,
    "scores_boost_latest": 0.05,
    "strong_boost_rate": 0.75,
    "weak_boost_rate": 1.5,
    "min_pos_scores_rate": 0.5,
    "spkcache_sil_frames_per_spk": 1,
}

def shapes(p: dict) -> dict:
    chunk_enc = p["chunk_len"] + p["chunk_right_context"]
    packed = p["spkcache_len"] + p["fifo_len"] + chunk_enc
    return {
        "chunk": ("float32", [1, chunk_enc * SUBSAMPLING, N_MELS]),
        "chunk_lengths": ("int32", [1]),
        "spkcache": ("float32", [1, p["spkcache_len"], EMB_DIM]),
        "spkcache_lengths": ("int32", [1]),
        "fifo": ("float32", [1, p["fifo_len"], EMB_DIM]),
        "fifo_lengths": ("int32", [1]),
        "speaker_preds": ("float32", [1, packed, N_SPK]),
        "speaker_preds_10ms": ("float32", [1, packed * SUBSAMPLING, N_SPK]),
        "chunk_pre_encode_embs": ("float32", [1, chunk_enc, EMB_DIM]),
    }

def die(msg: str) -> None:
    print(f"export: {msg}", file=sys.stderr)
    sys.exit(1)

def guard(args: argparse.Namespace) -> None:
    if not args.i_know_recordings_are_done:
        die("refusing to start: pass --i-know-recordings-are-done once no test machine is recording.")
    if not hasattr(os, "getloadavg"):
        die("refusing to start: this platform has no load average; run the export on macOS or Linux.")
    cores = os.cpu_count() or 1
    limit = args.max_load if args.max_load is not None else max(1.0, 0.25 * cores)
    one, five, _ = os.getloadavg()
    if one > limit or five > limit:
        die(f"refusing to start: load average {one:.2f}/{five:.2f} (1/5 min) is above {limit:.2f}. "
            "Wait for the machine to idle, or pass --max-load.")
    print(f"export: load average {one:.2f}/{five:.2f}, limit {limit:.2f}, {cores} cores")

def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()

def fetch_nemo(cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / NEMO_FILE
    if not path.exists() or path.stat().st_size != NEMO_BYTES or sha256(path) != NEMO_SHA256:
        url = f"https://huggingface.co/{REPO}/resolve/{REVISION}/{NEMO_FILE}"
        print(f"export: downloading {url}")
        tmp = path.with_suffix(".part")
        with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
            shutil.copyfileobj(r, f, 1 << 20)
        tmp.replace(path)
    size, digest = path.stat().st_size, sha256(path)
    if size != NEMO_BYTES or digest != NEMO_SHA256:
        die(f"{path}: {size} bytes, sha256 {digest}; expected {NEMO_BYTES} bytes, sha256 {NEMO_SHA256}")
    print(f"export: {NEMO_FILE} verified ({size} bytes, sha256 {digest})")
    return path

def fetch_model_card(out: Path) -> None:
    url = f"https://huggingface.co/{REPO}/resolve/{REVISION}/README.md"
    with urllib.request.urlopen(url) as r:
        (out / "MODEL_CARD.md").write_bytes(r.read())

def synthetic_audio(seconds: float, seed: int):
    """Deterministic speech-like audio: four fictional synthetic voices
    (harmonic stacks at distinct pitches, syllable-rate envelopes) taking
    turns with short gaps. Content only shapes the fixtures; graph cost is
    fixed by the preset."""
    import numpy as np

    rng = np.random.default_rng(seed)
    n = int(seconds * SAMPLE_RATE)
    out = np.zeros(n, dtype=np.float64)
    pitches = [110.0, 155.0, 205.0, 260.0]
    t0 = 0
    voice = 0
    while t0 < n:
        dur = int(rng.uniform(1.5, 4.0) * SAMPLE_RATE)
        t = np.arange(min(dur, n - t0)) / SAMPLE_RATE
        f0 = pitches[voice] * (1.0 + 0.04 * np.sin(2 * np.pi * 0.7 * t))
        phase = 2 * np.pi * np.cumsum(f0) / SAMPLE_RATE
        sig = sum((0.6 / k) * np.sin(k * phase) for k in range(1, 9))
        env = 0.5 * (1 + np.sin(2 * np.pi * rng.uniform(3.0, 5.0) * t)) ** 2
        out[t0:t0 + len(t)] += 0.08 * sig * env
        t0 += len(t) + int(rng.uniform(0.15, 0.5) * SAMPLE_RATE)
        voice = (voice + int(rng.integers(1, 4))) % 4
    out += 0.002 * rng.standard_normal(n)
    return out.astype(np.float32)

def load_model(nemo_path: Path):
    import torch
    from nemo.collections.asr.models import SortformerEncLabelModel

    model = SortformerEncLabelModel.restore_from(str(nemo_path), map_location="cpu")
    model = model.float().eval()
    torch.set_grad_enabled(False)
    if not getattr(model, "high_resolution", False) or getattr(model, "upsample_factor", 0) != SUBSAMPLING:
        die("the checkpoint does not report 10 ms (high_resolution) output with upsample factor 8; "
            "the pinned NeMo commit does not support this checkpoint.")
    sm = model.sortformer_modules
    if not getattr(sm, "use_learnable_sil_emb", False):
        die("the checkpoint does not use a learnable silence embedding (spec §4.4 expects one).")
    if sm.fc_d_model != EMB_DIM or sm.n_spk != N_SPK or model.encoder.subsampling_factor != SUBSAMPLING:
        die(f"unexpected dims: fc_d_model={sm.fc_d_model} n_spk={sm.n_spk} "
            f"subsampling={model.encoder.subsampling_factor}")
    pre = model.preprocessor
    feat = getattr(pre, "featurizer", None)
    if feat is not None:
        if hasattr(feat, "dither"):
            feat.dither = 0.0
        if hasattr(feat, "pad_to"):
            feat.pad_to = 0
    for name, want in COMPRESSION.items():
        got = getattr(sm, name, None)
        if got is not None and abs(float(got) - float(want)) > 1e-9:
            die(f"compression constant {name} = {got}, spec §4.4 says {want}")
    return model

def prepare_for_export(model) -> list[str]:
    """NeMo's own export preparation, plus dense attention for tracing."""
    notes = []
    try:
        from nemo.utils.export_utils import replace_for_export

        try:
            replace_for_export(model)
        except TypeError:
            replace_for_export(model, False)
        notes.append("nemo.utils.export_utils.replace_for_export applied")
    except Exception as e:  # noqa: BLE001 - recorded, the parity check decides
        notes.append(f"replace_for_export unavailable: {e}")
    for name, mod in model.named_modules():
        for attr, dense in (("use_flex_attention", False), ("flex_attention", False),
                            ("use_flash_attention", False), ("attn_implementation", "eager")):
            if attr in vars(mod) and isinstance(vars(mod)[attr], (bool, str)) and vars(mod)[attr] != dense:
                notes.append(f"{name or 'model'}.{attr}: {vars(mod)[attr]!r} -> {dense!r}")
                setattr(mod, attr, dense)
    return notes

def set_preset(model, p: dict) -> None:
    sm = model.sortformer_modules
    sm.chunk_len = p["chunk_len"]
    sm.chunk_left_context = 0
    sm.chunk_right_context = p["chunk_right_context"]
    sm.fifo_len = p["fifo_len"]
    sm.spkcache_len = p["spkcache_len"]
    sm.spkcache_update_period = p["spkcache_update_period"]
    sm._check_streaming_parameters()
    model.async_streaming = True

def step_graph(model):
    import torch

    class StepGraph(torch.nn.Module):
        """NeMo `forward_for_export` with the 10 ms predictions kept as a
        second output, under FluidAudio's tensor names."""

        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, chunk, chunk_lengths, spkcache, spkcache_lengths, fifo, fifo_lengths):
            m = self.m
            embs, embs_lengths = m._call_pre_encode(chunk, chunk_lengths.to(torch.int64))
            embs_lengths = embs_lengths.to(torch.int64)
            packed, packed_lengths = m.sortformer_modules.concat_and_pad(
                [spkcache, fifo, embs],
                [spkcache_lengths.to(torch.int64), fifo_lengths.to(torch.int64), embs_lengths],
                output_length=spkcache.shape[1] + fifo.shape[1] + embs.shape[1],
            )
            enc, enc_lengths = m.frontend_encoder(
                processed_signal=packed, processed_signal_length=packed_lengths, bypass_pre_encode=True
            )
            preds_10ms = m.forward_infer(enc, enc_lengths)
            preds = m.sortformer_modules.downsample_preds(preds_10ms, m.upsample_factor)
            return preds, preds_10ms, embs

    return StepGraph(model).eval()

def mel(model, audio):
    import torch

    x = torch.from_numpy(audio).unsqueeze(0)
    feats, _ = model.preprocessor(input_signal=x, length=torch.tensor([x.shape[1]]))
    return feats[0].transpose(0, 1).contiguous().numpy()  # [T, 128]

def pre_encode_frames(model, mel_tc):
    """Pre-encode embeddings of a mel sequence, for realistic state contents."""
    import torch

    x = torch.from_numpy(mel_tc).unsqueeze(0)
    embs, _ = model._call_pre_encode(x, torch.tensor([x.shape[1]]))
    return embs[0].numpy()  # [T/8, 512]

def state_configs(p: dict) -> list[dict]:
    sc, ff = p["spkcache_len"], p["fifo_len"]
    core_mel = p["chunk_len"] * SUBSAMPLING
    full_mel = (p["chunk_len"] + p["chunk_right_context"]) * SUBSAMPLING
    return [
        {"name": "first", "spkcache": 0, "fifo": 0, "chunk_mel": full_mel},
        {"name": "half", "spkcache": sc // 2, "fifo": ff // 2, "chunk_mel": full_mel},
        {"name": "full", "spkcache": sc, "fifo": ff, "chunk_mel": full_mel},
        {"name": "tail", "spkcache": sc, "fifo": ff // 3, "chunk_mel": max(SUBSAMPLING, core_mel // 2)},
    ]

def build_inputs(p: dict, cfg: dict, mel_tc, emb_pool):
    import numpy as np

    sh = shapes(p)
    chunk = np.zeros(sh["chunk"][1], dtype=np.float32)
    chunk[0, : cfg["chunk_mel"], :] = mel_tc[: cfg["chunk_mel"]]
    spkcache = np.zeros(sh["spkcache"][1], dtype=np.float32)
    spkcache[0, : cfg["spkcache"]] = emb_pool[: cfg["spkcache"]]
    fifo = np.zeros(sh["fifo"][1], dtype=np.float32)
    fifo[0, : cfg["fifo"]] = emb_pool[cfg["spkcache"]: cfg["spkcache"] + cfg["fifo"]]
    i32 = lambda v: np.array([v], dtype=np.int32)  # noqa: E731
    return {
        "chunk": chunk,
        "chunk_lengths": i32(cfg["chunk_mel"]),
        "spkcache": spkcache,
        "spkcache_lengths": i32(cfg["spkcache"]),
        "fifo": fifo,
        "fifo_lengths": i32(cfg["fifo"]),
    }

def torch_run(graph, feeds: dict) -> dict:
    import torch

    args = [torch.from_numpy(feeds[k]) for k in INPUTS]
    out = graph(*args)
    return {k: v.numpy() for k, v in zip(OUTPUTS, out)}

def check_shapes(p: dict, outs: dict) -> None:
    sh = shapes(p)
    for k in OUTPUTS:
        if list(outs[k].shape) != sh[k][1]:
            die(f"torch output {k} has shape {list(outs[k].shape)}, spec §4.2 says {sh[k][1]}")

def maxdiff(a, b) -> float:
    import numpy as np

    return float(np.max(np.abs(a.astype(np.float64) - b.astype(np.float64)))) if a.size else 0.0

def nemo_export_reference(model, feeds: dict) -> dict:
    """NeMo's own forward_for_export: 80 ms predictions and chunk embeddings."""
    import torch

    args = [torch.from_numpy(feeds[k]) for k in INPUTS]
    args = [a.to(torch.int64) if a.dtype == torch.int32 else a for a in args]
    preds, embs, _ = model.forward_for_export(*args)
    return {"speaker_preds": preds.numpy(), "chunk_pre_encode_embs": embs.numpy()}

def nemo_streaming_first_step(model, p: dict, feeds: dict):
    """The 10 ms chunk predictions of NeMo's forward_streaming_step from an
    empty state, for the `first` configuration."""
    import torch

    state = model.sortformer_modules.init_streaming_state(batch_size=1, async_streaming=True,
                                                           device=torch.device("cpu"))
    total = torch.zeros((1, 0, N_SPK))
    valid = int(feeds["chunk_lengths"][0])
    chunk = torch.from_numpy(feeds["chunk"][:, :valid, :])
    core_mel = p["chunk_len"] * SUBSAMPLING
    out = model.forward_streaming_step(
        processed_signal=chunk,
        processed_signal_length=torch.tensor([valid]),
        streaming_state=state,
        total_preds=total,
        left_offset=0,
        right_offset=valid - core_mel,
    )
    return out[1][0].numpy()  # [chunk_len * 8, 8]

@contextlib.contextmanager
def dense_attention_for_tracing():
    """NeMo's transformer encoder builds a FlexAttention block mask with vmap,
    which torch.jit tracing cannot record. While tracing, swap in the same
    computation as plain tensor ops: the mask_mod evaluated on broadcast index
    tensors, and softmax attention with the relative-position bias."""
    import torch
    from nemo.collections.asr.modules import transformer_encoder as te

    def dense_block_mask(mask_mod, B, H, Q_LEN, KV_LEN, device=None):
        b = torch.arange(B, device=device).view(B, 1, 1, 1)
        h = torch.arange(H, device=device).view(1, H, 1, 1)
        q = torch.arange(Q_LEN, device=device).view(1, 1, Q_LEN, 1)
        k = torch.arange(KV_LEN, device=device).view(1, 1, 1, KV_LEN)
        return mask_mod(b, h, q, k)

    def dense_attention(q, k, v, block_mask=None, score_mod=None):
        scores = torch.matmul(q, k.transpose(-2, -1)) * (q.shape[-1] ** -0.5)
        if score_mod is not None:
            scores = scores + score_mod._relative_position_bias
        if block_mask is not None:
            scores = scores.masked_fill(~block_mask, torch.finfo(scores.dtype).min)
        return torch.matmul(torch.softmax(scores, dim=-1).to(v.dtype), v)

    utils = te._transformer_utils
    saved = (te.create_block_mask, utils._get_flex_attention)
    te.create_block_mask = dense_block_mask
    utils._get_flex_attention = lambda x: dense_attention
    try:
        yield
    finally:
        te.create_block_mask, utils._get_flex_attention = saved

def export_onnx(graph, p: dict, feeds: dict, path: Path) -> None:
    import torch

    args = tuple(torch.from_numpy(feeds[k]) for k in INPUTS)
    with dense_attention_for_tracing():
        torch.onnx.export(
            graph,
            args,
            str(path),
            input_names=INPUTS,
            output_names=OUTPUTS,
            opset_version=OPSET,
            do_constant_folding=True,
            dynamo=False,
        )
    fix_output_shapes(path, p)

def fix_output_shapes(path: Path, p: dict) -> None:
    """The traced graph computes its output lengths from data, so the exporter
    leaves the output dimensions symbolic. They are fixed by construction;
    declare them. `ort_run` asserts the runtime shapes match."""
    import onnx

    m = onnx.load(str(path))
    sh = shapes(p)
    for v in m.graph.output:
        dims = v.type.tensor_type.shape.dim
        want = sh[v.name][1]
        if len(dims) != len(want):
            die(f"{path.name}: output {v.name} has rank {len(dims)}, expected {len(want)}")
        for d, n in zip(dims, want):
            d.ClearField("dim_param")
            d.dim_value = n
    onnx.save(m, str(path))

def check_graph(path: Path, p: dict) -> None:
    import onnx

    onnx.checker.check_model(str(path), full_check=True)
    m = onnx.load(str(path), load_external_data=False)
    sh = shapes(p)
    dtypes = {onnx.TensorProto.FLOAT: "float32", onnx.TensorProto.INT32: "int32"}
    for vi, names in ((m.graph.input, INPUTS), (m.graph.output, OUTPUTS)):
        got = [v.name for v in vi]
        if got != names:
            die(f"{path.name}: tensor names {got}, expected {names}")
        for v in vi:
            t = v.type.tensor_type
            dims = [d.dim_value for d in t.shape.dim]
            want_dtype, want_dims = sh[v.name]
            if dtypes.get(t.elem_type) != want_dtype or dims != want_dims:
                die(f"{path.name}: {v.name} is {dtypes.get(t.elem_type)} {dims}, "
                    f"expected {want_dtype} {want_dims}")
    domains = {n.domain for n in m.graph.node if n.domain not in ("", "ai.onnx")}
    if domains:
        die(f"{path.name}: custom op domains {sorted(domains)}")
    opset = [o.version for o in m.opset_import if o.domain in ("", "ai.onnx")]
    if opset != [OPSET]:
        die(f"{path.name}: opset {opset}, expected {OPSET}")

def to_bf16_storage(src: Path, dst: Path) -> dict:
    """Store every large fp32 initializer whose values are exactly bf16 as a
    bf16 initializer feeding a Cast to fp32 under the original name."""
    import numpy as np
    import onnx
    from onnx import helper, numpy_helper

    m = onnx.load(str(src))
    g = m.graph
    kept, casts, moved_bytes, total_bytes = [], [], 0, 0
    for init in g.initializer:
        if init.data_type != onnx.TensorProto.FLOAT:
            kept.append(init)
            continue
        arr = numpy_helper.to_array(init).astype("<f4", copy=False)
        if arr.size >= 1024:
            total_bytes += arr.nbytes
        bits = arr.view("<u4")
        if arr.size < 1024 or np.any(bits & 0xFFFF):
            kept.append(init)
            continue
        t = onnx.TensorProto()
        t.name = init.name + "__bf16"
        t.data_type = onnx.TensorProto.BFLOAT16
        t.dims.extend(arr.shape)
        t.raw_data = (bits >> 16).astype("<u2").tobytes()
        kept.append(t)
        casts.append(helper.make_node("Cast", [t.name], [init.name], name=init.name + "__cast",
                                      to=onnx.TensorProto.FLOAT))
        moved_bytes += arr.nbytes
    if total_bytes == 0 or moved_bytes / total_bytes < 0.99:
        die(f"only {moved_bytes}/{total_bytes} bytes of weights are bf16-exact; spec §4.2's bf16 "
            "storage premise does not hold for this checkpoint. Re-run with --storage fp32 and report.")
    del g.initializer[:]
    g.initializer.extend(kept)
    nodes = list(g.node)
    del g.node[:]
    g.node.extend(casts + nodes)
    onnx.checker.check_model(m, full_check=True)
    onnx.save(m, str(dst))
    return {"bf16_initializers": len(casts), "bf16_bytes_fp32": moved_bytes, "weight_bytes_fp32": total_bytes}

def ort_run(path: Path, feeds: dict) -> dict:
    import onnxruntime as ort

    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    s = ort.InferenceSession(str(path), so, providers=["CPUExecutionProvider"])
    out = s.run(OUTPUTS, feeds)
    return dict(zip(OUTPUTS, out))

def assert_ort_shapes(p: dict, name: str, outs: dict) -> None:
    sh = shapes(p)
    for k in OUTPUTS:
        if list(outs[k].shape) != sh[k][1]:
            die(f"{name}: onnxruntime output {k} has shape {list(outs[k].shape)}, expected {sh[k][1]}")

def write_tensor(path: Path, arr) -> dict:
    import numpy as np

    dtype = {np.dtype("float32"): "<f4", np.dtype("int32"): "<i4"}[arr.dtype]
    path.write_bytes(arr.astype(dtype).tobytes())
    return {"file": path.name, "dtype": str(arr.dtype), "shape": list(arr.shape), "sha256": sha256(path)}

def write_step_fixture(fx: Path, preset: str, p: dict, runs: list) -> None:
    meta = {"preset": preset, "params": p,
            "source": {"repo": REPO, "revision": REVISION, "sha256": NEMO_SHA256},
            "reference": "NeMo fp32 forward (torch), CPU", "configs": []}
    for cfg, feeds, ref in runs:
        entry = {"name": cfg["name"], "tensors": {}}
        for k in INPUTS:
            entry["tensors"][k] = write_tensor(fx / f"step-v1.{preset}.{cfg['name']}.{k}.bin", feeds[k])
        for k in OUTPUTS:
            entry["tensors"][k] = write_tensor(fx / f"step-v1.{preset}.{cfg['name']}.{k}.bin", ref[k])
        meta["configs"].append(entry)
    (fx / f"step-v1.{preset}.json").write_text(json.dumps(meta, indent=1) + "\n")

def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out_dir", type=Path)
    ap.add_argument("--i-know-recordings-are-done", action="store_true")
    ap.add_argument("--max-load", type=float, default=None,
                    help="refuse when the 1- or 5-minute load average exceeds this (default max(1, cores/4))")
    ap.add_argument("--cache", type=Path, default=None, help="where the .nemo is kept (default OUT_DIR/../cache)")
    ap.add_argument("--storage", choices=["bf16", "fp32"], default="bf16")
    ap.add_argument("--presets", default="low,fast32")
    args = ap.parse_args()
    guard(args)

    import numpy as np
    import onnx
    import onnxruntime
    import torch

    out = args.out_dir.resolve()
    if out.exists() and any(out.iterdir()):
        die(f"{out} is not empty; give a fresh directory.")
    out.mkdir(parents=True, exist_ok=True)
    fx = out / "fixtures"
    fx.mkdir()
    work = out / "work"
    work.mkdir()
    cache = (args.cache or out.parent / "cache").resolve()

    torch.manual_seed(0)
    nemo_path = fetch_nemo(cache)
    fetch_model_card(out)
    model = load_model(nemo_path)
    notes = prepare_for_export(model)
    for n in notes:
        print(f"export: {n}")

    sil = model.sortformer_modules.learnable_sil_emb.detach().numpy().astype("<f4")
    if sil.shape != (EMB_DIM,):
        die(f"learnable_sil_emb has shape {sil.shape}")
    (out / "learnable_sil_emb.bin").write_bytes(sil.tobytes())

    # Mel golden: NeMo's preprocessor over 3 s of synthetic audio.
    audio = synthetic_audio(60.0, seed=20261001)
    mel_all = mel(model, audio)
    mel_meta = {"audio": write_tensor(fx / "mel-v1.audio.bin", audio[: 3 * SAMPLE_RATE]),
                "mel": write_tensor(fx / "mel-v1.mel.bin", mel(model, audio[: 3 * SAMPLE_RATE])),
                "layout": "audio f32 LE mono 16 kHz; mel f32 LE [frames, 128], NeMo preprocessor, dither 0"}
    (fx / "mel-v1.json").write_text(json.dumps(mel_meta, indent=1) + "\n")
    emb_pool = pre_encode_frames(model, mel_all)

    report = {"presets": {}, "export_notes": notes}
    names = [s.strip() for s in args.presets.split(",") if s.strip()]
    for preset in names:
        if preset not in PRESETS:
            die(f"unknown preset {preset}")
        p = PRESETS[preset]
        set_preset(model, p)
        graph = step_graph(model)
        cfgs = state_configs(p)
        start_mel = 4 * SAMPLE_RATE // HOP  # chunk audio from 4 s in
        runs = []
        for cfg in cfgs:
            feeds = build_inputs(p, cfg, mel_all[start_mel:], emb_pool)
            ref = torch_run(graph, feeds)
            check_shapes(p, ref)
            nemo = nemo_export_reference(model, feeds)
            for k in ("speaker_preds", "chunk_pre_encode_embs"):
                d = maxdiff(ref[k], nemo[k])
                if d > 1e-6:
                    die(f"{preset}/{cfg['name']}: wrapper {k} differs from NeMo forward_for_export by {d:.3e}")
            runs.append((cfg, feeds, ref))

        first_feeds, first_ref = runs[0][1], runs[0][2]
        stream = nemo_streaming_first_step(model, p, first_feeds)
        hi_first = first_ref["speaker_preds_10ms"][0, : stream.shape[0]]
        d_stream = maxdiff(hi_first, stream)
        if d_stream > TOLERANCE:
            die(f"{preset}: first-step 10 ms predictions differ from NeMo forward_streaming_step by {d_stream:.3e}")

        fp32_path = work / f"nemotron3_diar_{preset}.fp32.onnx"
        export_onnx(graph, p, runs[2][1], fp32_path)
        check_graph(fp32_path, p)
        ship = out / f"nemotron3_diar_{preset}.onnx"
        storage = {"storage": "fp32"}
        if args.storage == "bf16":
            storage = {"storage": "bf16", **to_bf16_storage(fp32_path, ship)}
            check_graph(ship, p)
        else:
            shutil.copyfile(fp32_path, ship)

        parity = {}
        for cfg, feeds, ref in runs:
            for label, path in (("fp32", fp32_path), ("shipped", ship)):
                got = ort_run(path, feeds)
                assert_ort_shapes(p, path.name, got)
                diffs = {k: maxdiff(got[k], ref[k]) for k in OUTPUTS}
                parity[f"{cfg['name']}/{label}"] = diffs
                worst = max(diffs.values())
                print(f"export: {preset} {cfg['name']:5s} {label:7s} max |ort - nemo| = {worst:.3e}")
                if worst > TOLERANCE:
                    die(f"{preset}/{cfg['name']}/{label}: onnxruntime differs from NeMo by {worst:.3e} "
                        f"(> {TOLERANCE})")
        write_step_fixture(fx, preset, p, runs)
        report["presets"][preset] = {
            "params": p,
            "file": ship.name,
            "sha256": sha256(ship),
            "bytes": ship.stat().st_size,
            "tensors": {k: {"dtype": d, "shape": s} for k, (d, s) in shapes(p).items()},
            "latency_s": round((p["chunk_len"] + p["chunk_right_context"]) * 0.08, 2),
            "audio_per_step_s": round(p["chunk_len"] * 0.08, 2),
            "parity_max_abs": parity,
            "nemo_streaming_first_step_max_abs": d_stream,
            **storage,
        }

    freeze = subprocess.run([sys.executable, "-m", "pip", "freeze"], capture_output=True, text=True, check=True)
    (out / "requirements.lock.txt").write_text(freeze.stdout)
    pre_cfg = {}
    try:
        from omegaconf import OmegaConf

        pre_cfg = OmegaConf.to_container(model.cfg.preprocessor, resolve=True)
    except Exception as e:  # noqa: BLE001
        pre_cfg = {"unavailable": str(e)}
    manifest = {
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "source": {"repo": REPO, "revision": REVISION, "file": NEMO_FILE, "sha256": NEMO_SHA256,
                   "bytes": NEMO_BYTES, "weights_version": WEIGHTS_VERSION, "license": LICENSE_NAME},
        "toolchain": {"python": platform.python_version(), "torch": torch.__version__, "onnx": onnx.__version__,
                      "onnxruntime": onnxruntime.__version__, "numpy": np.__version__, "nemo_git": NEMO_GIT,
                      "host": platform.node(), "machine": platform.machine(), "system": platform.system()},
        "opset": OPSET,
        "tolerance": TOLERANCE,
        "mel": {"sample_rate": SAMPLE_RATE, "n_mels": N_MELS, "n_fft": 512, "win_length": 400, "hop_length": HOP,
                "window": "hann (symmetric)", "preemph": 0.97, "log": "ln(x + 2^-24)", "normalize": "none",
                "dither": 0.0, "nemo_preprocessor_cfg": pre_cfg},
        "streaming": {"chunk_left_context": 0, **COMPRESSION, "silence_embedding": "learnable_sil_emb.bin"},
        "learnable_sil_emb": {"file": "learnable_sil_emb.bin", "dtype": "float32", "shape": [EMB_DIM],
                              "sha256": sha256(out / "learnable_sil_emb.bin")},
        **report,
    }
    (out / "nemotron3_diar.json").write_text(json.dumps(manifest, indent=1) + "\n")
    (out / "NOTICE").write_text(
        "NVIDIA Nemotron 3 Diarization\n"
        f"Source: https://huggingface.co/{REPO} at revision {REVISION}\n"
        f"Licensed by NVIDIA Corporation under the {LICENSE_NAME} license (OpenMDW License Agreement, "
        "version 1.1). The model card (MODEL_CARD.md) travels with these files; the release adds "
        "LICENSE-OpenMDW-1.1.\n"
        "Converted to ONNX by Apexis Solutions Inc. (dba Small Bricktory): one streaming step per preset, "
        "weights unchanged.\n"
    )
    shutil.rmtree(work)
    lines = []
    for f in sorted(out.rglob("*")):
        if f.is_file() and f.name != "SHA256SUMS":
            lines.append(f"{sha256(f)}  {f.relative_to(out).as_posix()}")
    (out / "SHA256SUMS").write_text("\n".join(lines) + "\n")
    print(f"export: done; {out}/SHA256SUMS lists {len(lines)} files")

if __name__ == "__main__":
    main()
