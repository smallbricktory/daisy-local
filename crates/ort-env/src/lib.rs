// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The process's one ONNX Runtime environment, and the session builder every
//! ONNX session starts from: on the CPU, in that environment.

use std::sync::OnceLock;

/// The environment, built on first use and kept for the life of the process.
pub fn environment() -> ort::Result<ort::environment::Environment> {
    static ENV: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().build().map_err(|e| e.to_string()))
        .clone()
        .map_err(ort::Error::new)
}

/// A session builder in [`environment`] that keeps the graph on the CPU.
pub fn session_builder() -> ort::Result<ort::session::builder::SessionBuilder> {
    Ok(ort::session::Session::builder(&environment()?)?
        .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_caller_gets_the_same_environment() {
        assert!(super::environment().is_ok());
        assert!(super::environment().is_ok(), "a second call reuses the first environment");
    }

    #[test]
    fn a_session_builder_comes_from_the_shared_environment() {
        assert!(super::session_builder().is_ok());
        assert!(super::session_builder().is_ok());
    }
}
