// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What this machine can run: ggml's device list and physical RAM.

use daisy_llm_protocol::{Device, Probe};

pub fn kind_of(t: llama_cpp_2::LlamaBackendDeviceType) -> &'static str {
    use llama_cpp_2::LlamaBackendDeviceType::*;
    match t {
        Gpu => "gpu",
        IntegratedGpu => "igpu",
        Cpu => "cpu",
        Accelerator => "accel",
        Unknown => "unknown",
    }
}

/// Initializes the backend, lists devices, reads RAM.
pub fn run() -> anyhow::Result<Probe> {
    let _backend = llama_cpp_2::llama_backend::LlamaBackend::init()?;
    let devices = llama_cpp_2::list_llama_ggml_backend_devices()
        .into_iter()
        .map(|d| Device {
            name: d.name,
            backend: d.backend,
            kind: kind_of(d.device_type).to_string(),
            memory_bytes: d.memory_total as u64,
        })
        .collect();
    let sys = sysinfo::System::new_all();
    Ok(Probe { devices, ram_bytes: sys.total_memory() })
}
