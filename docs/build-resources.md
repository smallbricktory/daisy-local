# Linux release build: resource use

Measured 2026-11-12 on astrophage (WSL2, 16 vCPU, 20 GB RAM limit, 16 GB swap).
The build was `scripts/build-inside.sh` for v1.3.6 in the `daisy-build:24.04`
podman image, the same container the release pipeline uses. It started from a
fresh clone with four new, empty cache volumes (cargo registry, cargo git,
`target/`, pnpm store), so every crate compiled and every model downloaded.
The image was already built.

For about 3 of the 12 minutes, a CI native-UI job ran on the same host, so the
wall time may be slightly long.

## Results

| Measure | Value |
|---|---|
| Wall time, cold | 727 s (12.1 min) |
| CPU, mean over the run | 5.8 cores (582%) |
| CPU, highest 5 s sample | 9.3 cores (931%) |
| Memory, highest 5 s sample (`podman stats`) | 10.7 GB |
| Memory, cgroup `memory.peak` | 16.0 GB (includes reclaimable page cache) |
| Swap, cgroup `memory.swap.peak` | 12 KB |
| `target/` after the build | 11 GB |
| Cargo registry + git caches | 0.8 GB |
| Source tree with `node_modules` and models | 1.7 GB (models 1.1 GB) |
| Build image `daisy-build:24.04` | 3.3 GB |
| AppImage | 0.99 GB |

Stages, in seconds from the start:

| Stage | Start |
|---|---|
| Frontend dependencies and build | 0 |
| Model downloads (voiceprints, Nemotron, diarization, embeddings) | 13 |
| `daisy-llm` sidecar compile | 30 |
| `daisy-app` compile and link | 219 |
| Bundling (`.deb`, AppImage) | 654 |
| AppImage repack | 704 |
| Staging to `dist/` | 720 |

Memory per minute: below 3 GB for the first two minutes; 6.7 to 10.7 GB while
the C/C++ dependencies and the sidecar compile (minutes 2 to 6); 5 to 7.6 GB
through the `daisy-app` link and bundling.

## Sizing a VM for this build

- **CPU:** 8 vCPU. The build averaged 5.8 cores and never went past 9.3. The
  release profile's thin LTO with `codegen-units = 1` makes the final
  `daisy-app` link one long job. With 8 vCPU the cold build is likely around
  15 minutes; this is a hypothesis, not measured.
- **RAM:** 16 GB. The working set peaked near 11 GB. The 16 GB cgroup peak
  includes file cache the kernel can reclaim.
- **Disk:** 60 GB. One cold build uses about 20 GB: image, caches, `target/`,
  sources and installers. Incremental builds grow `target/`; astrophage's
  long-lived release cache reached 76 GB before anyone cleaned it.
- **Network:** about 2 GB per cold build (crates 0.8 GB, models 1.1 GB), plus
  the base image the first time.
