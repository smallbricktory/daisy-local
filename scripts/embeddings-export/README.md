# Embeddings model export

`models/embeddings/model.onnx` is Daisy's export of
[ibm-granite/granite-embedding-97m-multilingual-r2](https://huggingface.co/ibm-granite/granite-embedding-97m-multilingual-r2):
the token-embedding Gather stored int8, every MatMul weight stored fp16 with a
Cast back to fp32 that ONNX Runtime folds at session load. Inference runs the
fp32 kernels. Dynamic-int8 MatMul exports of this model drift to cosine 0.92
against fp32 on x86 CPUs, so they are not shipped.

The artifact and the unmodified `tokenizer.json` / `config.json` are release
assets on `smallbricktory/daisy-local`
(tag `embeddings-granite-g8m16-1`), pinned by sha256 in
`scripts/download-embeddings.sh`. Every build lane fetches from that release.

## Rebuild the artifact

```sh
python3 -m venv venv && . venv/bin/activate
pip install onnx onnxruntime numpy
curl -LO https://huggingface.co/ibm-granite/granite-embedding-97m-multilingual-r2/resolve/main/onnx/model.onnx
python3 scripts/embeddings-export/shrink-fp16.py model.onnx g8m16.onnx
```

## Verify before pinning

Run the example against the fp32 model and the candidate on an ARM and an x86
machine; same-chunk cosine against fp32 must stay at or above 0.999 on both.

```sh
DAISY_EMBED_DIR=<dir with fp32 model.onnx + tokenizer.json> \
  cargo run -p embeddings --release --example embed_eval -- index <sessions_root> runs/fp32.index.json
DAISY_EMBED_DIR=<dir with candidate model.onnx + tokenizer.json> \
  cargo run -p embeddings --release --example embed_eval -- index <sessions_root> runs/candidate.index.json
python3 scripts/embeddings-export/drift.py runs fp32 candidate
```

## Publish

1. Upload `model.onnx`, `tokenizer.json`, `config.json` to a new release tag on
   `smallbricktory/daisy-local` with `gh release create <tag> --latest=false`.
2. Put the new tag and the three sha256 values in `scripts/download-embeddings.sh`.
3. Update `scripts/lib/model-notices.txt` and `scripts/lib/model-sbom.json`,
   then run `scripts/gen-licenses.sh`.
4. Change `SessionIndex::MODEL_ID` when the vector space changes; every session
   index re-embeds on the next Ask.
