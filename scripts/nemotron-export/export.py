#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright      2026  Xiaomi Corp.        (authors: Fangjun Kuang)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# Modified by Apexis Solutions Inc.: pins the NVIDIA checkpoint revision,
# exports the 560 ms configuration only, and writes the fp32 files into an
# output directory given on the command line.
#
# usage: export.py <out-dir>
# Writes <out-dir>/tokens.txt and <out-dir>/560/{encoder,decoder,joiner}.onnx
# (+ encoder.data), all fp32. quantize.py turns that directory into the
# shipped file set.
import os
import sys
from typing import Dict

import nemo.collections.asr as nemo_asr
import onnx
import torch

REVISION = "ac0580bb7d3d6e39c4361db6afe28db9211793e4"

"""
'_target_': 'nemo.collections.asr.modules.AudioToMelSpectrogramPreprocessor',
'sample_rate': 16000, 'normalize': 'NA', 'window_size': 0.025, 'window_stride': 0.01,
'window': 'hann', 'features': 128, 'n_fft': 512, 'log': True,
'frame_splicing': 1, 'dither': 1e-05, 'pad_to': 0, 'pad_value': 0.0}

# 80ms
streaming_cfg CacheAwareStreamingConfig(chunk_size=[1, 8], shift_size=[1, 8], cache_drop_size=0, last_channel_cache_size=70, valid_out_len=1, pre_encode_cache_size=[0, 9], drop_extra_pre_encoded=2, last_channel_num=0, last_time_num=0)
att_context_size [70, 0]
pre_encode_cache_size [0, 9]
"""


def add_meta_data(filename: str, meta_data: Dict[str, str]):
    """Add meta data to an ONNX model. It is changed in-place.

    Args:
      filename:
        Filename of the ONNX model to be changed.
      meta_data:
        Key-value pairs.
    """
    model = onnx.load(filename)

    while len(model.metadata_props):
        model.metadata_props.pop()

    for key, value in meta_data.items():
        meta = model.metadata_props.add()
        meta.key = key
        meta.value = str(value)

    external_filename = filename.split(".onnx")[0]
    onnx.save(
        model,
        filename,
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location=external_filename + ".data",
    )


@torch.no_grad()
def main():
    from huggingface_hub import hf_hub_download

    out_dir = os.path.abspath(sys.argv[1])
    os.makedirs(out_dir, exist_ok=True)
    os.chdir(out_dir)

    nemo_path = hf_hub_download(
        "nvidia/nemotron-speech-streaming-en-0.6b",
        "nemotron-speech-streaming-en-0.6b.nemo",
        revision=REVISION,
    )
    asr_model = nemo_asr.models.ASRModel.restore_from(nemo_path, map_location="cpu")

    with open("./tokens.txt", "w", encoding="utf-8") as f:
        for i, s in enumerate(asr_model.joint.vocabulary):
            f.write(f"{s} {i}\n")
        f.write(f"<blk> {i+1}\n")
        print("Saved to tokens.txt")

    asr_model.eval()

    assert asr_model.encoder.streaming_cfg is not None
    print("streaming_cfg", asr_model.encoder.streaming_cfg)

    chunk_size_ms_list = [560]
    for ms in chunk_size_ms_list:
        chunk_size = ms // 80 - 1
        print("chunk_size", chunk_size)
        asr_model.encoder.set_default_att_context_size([70, chunk_size])

        print("streaming_cfg", asr_model.encoder.streaming_cfg)
        print("att_context_size", asr_model.encoder.att_context_size)
        print(
            "pre_encode_cache_size",
            asr_model.encoder.streaming_cfg.pre_encode_cache_size,
        )

        if isinstance(asr_model.encoder.streaming_cfg.pre_encode_cache_size, list):
            pre_encode_cache_size = (
                asr_model.encoder.streaming_cfg.pre_encode_cache_size[1]
            )
        else:
            pre_encode_cache_size = (
                asr_model.encoder.streaming_cfg.pre_encode_cache_size
            )

        if isinstance(asr_model.encoder.streaming_cfg.chunk_size, list):
            chunk_size = asr_model.encoder.streaming_cfg.chunk_size[1]
        else:
            chunk_size = asr_model.encoder.streaming_cfg.chunk_size

        window_size = chunk_size + pre_encode_cache_size

        print("chunk_size", chunk_size)
        print("pre_encode_cache_size", pre_encode_cache_size)
        print("window_size", window_size)

        chunk_shift = chunk_size

        # cache_last_channel: (batch_size, dim1, dim2, dim3)
        cache_last_channel_dim1 = len(asr_model.encoder.layers)
        cache_last_channel_dim2 = (
            asr_model.encoder.streaming_cfg.last_channel_cache_size
        )
        cache_last_channel_dim3 = asr_model.encoder.d_model

        # cache_last_time: (batch_size, dim1, dim2, dim3)
        cache_last_time_dim1 = len(asr_model.encoder.layers)
        cache_last_time_dim2 = asr_model.encoder.d_model
        cache_last_time_dim3 = asr_model.encoder.conv_context_size[0]

        asr_model.set_export_config({"cache_support": True})

        asr_model.encoder.export("encoder.onnx")
        asr_model.decoder.export("decoder.onnx")
        asr_model.joint.export("joiner.onnx")

        normalize_type = asr_model.cfg.preprocessor.normalize
        if normalize_type == "NA":
            normalize_type = ""

        meta_data = {
            "vocab_size": asr_model.decoder.vocab_size,
            "window_size": window_size,
            "chunk_size_ms": ms,
            "chunk_shift": chunk_shift,
            "normalize_type": normalize_type,
            "cache_last_channel_dim1": cache_last_channel_dim1,
            "cache_last_channel_dim2": cache_last_channel_dim2,
            "cache_last_channel_dim3": cache_last_channel_dim3,
            "cache_last_time_dim1": cache_last_time_dim1,
            "cache_last_time_dim2": cache_last_time_dim2,
            "cache_last_time_dim3": cache_last_time_dim3,
            "pred_rnn_layers": asr_model.decoder.pred_rnn_layers,
            "pred_hidden": asr_model.decoder.pred_hidden,
            "subsampling_factor": 8,
            "feat_dim": 128,
            "model_type": "EncDecHybridRNNTCTCBPEModel",
            "version": "1",
            "model_author": "NeMo",
            "url": "https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b",
            "comment": "Only the transducer branch is exported",
        }
        print("meta_data", meta_data)
        add_meta_data("encoder.onnx", meta_data)

        print(meta_data)

        os.makedirs(str(ms), exist_ok=True)
        for name in os.listdir("."):
            if name.endswith((".onnx", ".data")):
                os.replace(name, os.path.join(str(ms), name))


if __name__ == "__main__":
    main()
