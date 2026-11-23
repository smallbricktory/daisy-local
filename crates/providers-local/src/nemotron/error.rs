// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NemotronError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("array shape error: {0}")]
    Shape(#[from] ndarray::ShapeError),
    /// The files on disk do not match the model contract (metadata, graph
    /// output names, token table).
    #[error("model files mismatch: {0}")]
    Model(String),
    /// A model file is missing, unreadable or rejected by the runtime.
    #[error("cannot load {}: {reason}", path.display())]
    Load { path: std::path::PathBuf, reason: String },
    #[error("ort error: {0}")]
    Ort(String),
}

impl<R> From<ort::Error<R>> for NemotronError {
    fn from(e: ort::Error<R>) -> Self {
        NemotronError::Ort(format!("{e}"))
    }
}
