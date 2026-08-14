use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParakeetError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("NdArray error: {0}")]
    NdArrayError(#[from] ndarray::ShapeError),
    /// The files on disk don't match the expected model contract (graph
    /// output names, vocab size) — a wrong-version or hand-edited model dir.
    #[error("Model files mismatch: {0}")]
    Model(String),
    #[error("Ort error: {0}")]
    OrtError(String),
}

impl<R> From<ort::Error<R>> for ParakeetError {
    fn from(e: ort::Error<R>) -> Self {
        ParakeetError::OrtError(format!("{e}"))
    }
}
